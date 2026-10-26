//! Read-only analysis of saved files and explicitly declared Git recovery data.
//! Receipts describe producer claims. They never grant path access or cleanup authority.
use crate::{
    fsx, git,
    model::{Entry, EntryKind, ExpectedFile, GitDependency, ImportMap, SCHEMA},
    store::{valid_digest, valid_oid},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::MetadataExt,
    path::Path,
};

const ENTRY_LIMIT: usize = 100_000;
const RECEIPT_LIMIT: u64 = 8 * 1024 * 1024;

#[derive(Clone)]
struct Mapping {
    target: String,
    group: String,
    expected: Option<ExpectedFile>,
}
struct Description {
    format: &'static str,
    map: ImportMap,
    mappings: BTreeMap<String, Mapping>,
    heads: BTreeMap<String, String>,
    legacy_short_references: BTreeMap<String, String>,
    notes: Vec<String>,
}

fn receipt_reference(value: &str) -> Result<(String, Option<String>)> {
    let short = !value.starts_with("refs/");
    ensure!(
        !value.is_empty()
            && !value.starts_with('-')
            && !value.ends_with('.')
            && !value.contains("..")
            && !value.contains("@{")
            && !value
                .chars()
                .any(|ch| ch <= ' ' || ch == '\u{7f}' || "~^:?*[\\".contains(ch))
            && value
                .split('/')
                .all(|part| !part.is_empty() && !part.starts_with('.') && !part.ends_with(".lock")),
        "invalid legacy branch reference"
    );
    if short {
        ensure!(
            !(matches!(
                value,
                "HEAD" | "FETCH_HEAD" | "ORIG_HEAD" | "MERGE_HEAD" | "AUTO_MERGE"
            ) || ([40, 64].contains(&value.len())
                && value.bytes().all(|ch| ch.is_ascii_hexdigit()))),
            "legacy reference is not an unambiguous branch path"
        );
        Ok((format!("refs/heads/{value}"), Some(value.into())))
    } else {
        Ok((value.into(), None))
    }
}

fn read_metadata_json(folder: &Path, name: &str) -> Result<Value> {
    let meta = fs::symlink_metadata(folder.join(name))?;
    ensure!(
        meta.is_file() && !meta.file_type().is_symlink(),
        "recovery metadata must be a regular file"
    );
    ensure!(
        meta.len() <= RECEIPT_LIMIT,
        "recovery metadata exceeds 8 MiB limit"
    );
    let entry = Entry {
        path: name.into(),
        kind: EntryKind::File,
        mode: meta.mode() & 0o777,
        size: 0,
        sha256: String::new(),
    };
    serde_json::from_slice(&fsx::read_entry(folder, &entry)?).context("malformed recovery metadata")
}

fn strings(value: Option<&Value>) -> Result<Vec<String>> {
    match value {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| {
                let path = v
                    .as_str()
                    .context("receipt path list contains a non-string")?;
                fsx::validate_relative(path)?;
                Ok(path.to_owned())
            })
            .collect(),
        _ => anyhow::bail!("receipt path list is not an array"),
    }
}

fn expected(value: Option<&Value>) -> Result<Option<ExpectedFile>> {
    let Some(value) = value else { return Ok(None) };
    let sha = value
        .get("sha256")
        .and_then(Value::as_str)
        .context("missing expected content digest")?;
    let bytes = value
        .get("bytes")
        .and_then(Value::as_u64)
        .context("missing expected byte length")?;
    let mode = value
        .get("mode")
        .and_then(Value::as_u64)
        .context("missing expected mode")?;
    let kind = value
        .get("type")
        .or_else(|| value.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or("file");
    // The structural receipt format uses "regular" for ordinary files.
    // Normalize this one documented spelling; canonical maps keep their strict enum.
    let kind = if kind == "regular" { "file" } else { kind };
    ensure!(
        valid_digest(sha) && mode <= 0o777,
        "invalid expected file fingerprint"
    );
    ensure!(
        ["file", "symlink"].contains(&kind),
        "unsupported expected file kind"
    );
    Ok(Some(ExpectedFile {
        sha256: sha.into(),
        bytes,
        mode: mode as u32,
        kind: kind.into(),
    }))
}

fn add_mapping(
    description: &mut Description,
    stored: String,
    target: String,
    group: String,
    expected: Option<ExpectedFile>,
) -> Result<()> {
    fsx::validate_relative(&stored)?;
    fsx::validate_relative(&target)?;
    ensure!(
        !description.mappings.contains_key(&stored),
        "duplicate stored path in recovery metadata"
    );
    description.map.files.push(crate::model::ImportFile {
        stored_path: stored.clone(),
        target_path: target.clone(),
        base_sha256: None,
    });
    description.mappings.insert(
        stored,
        Mapping {
            target,
            group,
            expected,
        },
    );
    Ok(())
}

fn empty_description(format: &'static str) -> Description {
    Description {
        format,
        map: ImportMap {
            schema: SCHEMA,
            files: Vec::new(),
            git_dependencies: Vec::new(),
            groups: BTreeMap::new(),
        },
        mappings: BTreeMap::new(),
        heads: BTreeMap::new(),
        legacy_short_references: BTreeMap::new(),
        notes: Vec::new(),
    }
}

fn describe(folder: &Path) -> Result<Option<Description>> {
    if fs::symlink_metadata(folder.join("import-map.json")).is_ok() {
        let value = read_metadata_json(folder, "import-map.json")?;
        let map: ImportMap =
            serde_json::from_value(value).context("invalid canonical import map")?;
        crate::store::validate_import_map(&map)?;
        let mut result = empty_description("import_map");
        for item in &map.files {
            result.mappings.insert(
                item.stored_path.clone(),
                Mapping {
                    target: item.target_path.clone(),
                    group: "declared".into(),
                    expected: None,
                },
            );
        }
        for dep in &map.git_dependencies {
            if let Some(head) = &dep.source_head {
                result.heads.insert(dep.group.clone(), head.clone());
            }
        }
        result.map = map;
        return Ok(Some(result));
    }
    if fs::symlink_metadata(folder.join("receipt.json")).is_err() {
        return Ok(None);
    }
    let receipt = read_metadata_json(folder, "receipt.json")?;
    if let Some(groups) = receipt.get("results").and_then(Value::as_array) {
        let mut result = empty_description("grouped_receipt");
        let mut labels = BTreeSet::new();
        for group in groups {
            let label = group
                .get("label")
                .and_then(Value::as_str)
                .context("receipt group lacks a label")?;
            fsx::validate_relative(label)?;
            ensure!(
                labels.insert(label.to_owned()),
                "duplicate recovery group label"
            );
            let head = group.get("head").and_then(Value::as_str).map(str::to_owned);
            if let Some(head) = &head {
                ensure!(valid_oid(head), "invalid declared source commit");
                result.heads.insert(label.to_owned(), head.clone());
                result
                    .map
                    .groups
                    .insert(label.to_owned(), format!("declared source commit {head}"));
            }
            let before = group.get("before").and_then(Value::as_object);
            for (key, directory) in [
                ("untracked", "untracked"),
                ("ignored_source_media", "ignored-source-media"),
            ] {
                for path in strings(group.get(key))? {
                    let fingerprint = expected(before.and_then(|m| m.get(&path)))?;
                    add_mapping(
                        &mut result,
                        format!("{label}/{directory}/{path}"),
                        path,
                        label.into(),
                        fingerprint,
                    )?;
                }
            }
            let tracked = strings(group.get("tracked"))?;
            if !tracked.is_empty() {
                let oid = group
                    .get("stash_sha")
                    .and_then(Value::as_str)
                    .context("tracked recovery paths lack a saved Git commit identity")?;
                ensure!(valid_oid(oid), "invalid saved Git commit identity");
                let mut expected_files = BTreeMap::new();
                for path in &tracked {
                    if let Some(value) = expected(before.and_then(|m| m.get(path)))? {
                        expected_files.insert(path.clone(), value);
                    }
                }
                let reference = group
                    .get("ref")
                    .and_then(Value::as_str)
                    .map(receipt_reference)
                    .transpose()?;
                if let Some((_, Some(short))) = &reference {
                    result
                        .legacy_short_references
                        .insert(label.into(), short.clone());
                }
                result.map.git_dependencies.push(GitDependency {
                    group: label.into(),
                    oid: oid.into(),
                    reference: reference.map(|(reference, _)| reference),
                    kind: "stash".into(),
                    source_head: head,
                    tracked_paths: tracked,
                    expected_files,
                });
            }
        }
        result.notes.push("Grouped receipt mappings use each validated label and the documented untracked/ignored-source-media subdirectories; absolute producer output paths are not followed.".into());
        crate::store::validate_import_map(&result.map)?;
        return Ok(Some(result));
    }
    if let Some(files) = receipt.get("source_before").and_then(Value::as_object) {
        let mut result = empty_description("file_manifest_receipt");
        if let Some(head) = receipt.get("source_head").and_then(Value::as_str) {
            ensure!(valid_oid(head), "invalid declared source commit");
            result.heads.insert("preserved".into(), head.into());
            result
                .map
                .groups
                .insert("preserved".into(), format!("declared source commit {head}"));
        }
        for (path, value) in files {
            add_mapping(
                &mut result,
                path.clone(),
                path.clone(),
                "preserved".into(),
                expected(Some(value))?,
            )?;
        }
        if let Some(copied) = receipt.get("copied").and_then(Value::as_object) {
            if copied != files {
                result.notes.push("The producer's source and copied manifests differ; current bytes are checked independently against the declared source fingerprints.".into());
            }
        }
        crate::store::validate_import_map(&result.map)?;
        return Ok(Some(result));
    }
    Ok(None)
}

/// Translate supported receipt claims into the public map contract. No Git or payload reads.
pub fn map_from_receipt(folder: &Path) -> Result<Option<ImportMap>> {
    Ok(describe(folder)?.map(|description| description.map))
}

type MetadataInventory = (Vec<(String, fs::Metadata)>, usize, Vec<String>);

fn leaf_metadata(folder: &Path) -> Result<MetadataInventory> {
    let mut leaves = Vec::new();
    let mut directories = 0;
    let mut blockers = Vec::new();
    let mut seen = 0;
    let root = fs::symlink_metadata(folder)?;
    ensure!(
        root.is_dir() && !root.file_type().is_symlink(),
        "folder root must be a real directory"
    );
    ensure!(
        !fsx::is_placeholder(&root),
        "folder root is a cloud placeholder"
    );
    let mut pending = vec![folder.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for item in fs::read_dir(dir)? {
            seen += 1;
            ensure!(
                seen <= ENTRY_LIMIT,
                "folder metadata inventory exceeds 100000 entries"
            );
            let item = item?;
            let path = item.path();
            let relative = path
                .strip_prefix(folder)?
                .to_str()
                .context("non-UTF-8 recovery path unsupported")?
                .to_owned();
            fsx::validate_relative(&relative)?;
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                directories += 1;
                if fsx::is_placeholder(&metadata) {
                    blockers.push(format!(
                        "cloud-placeholder directory not traversed: {relative}"
                    ));
                } else {
                    pending.push(path);
                }
            } else if relative != "receipt.json" && relative != "import-map.json" {
                leaves.push((relative, metadata));
            }
        }
    }
    leaves.sort_by(|a, b| a.0.cmp(&b.0));
    Ok((leaves, directories, blockers))
}

fn entry(path: &str, meta: &fs::Metadata) -> Result<Entry> {
    ensure!(!fsx::is_placeholder(meta), "cloud placeholder not read");
    let kind = if meta.file_type().is_symlink() {
        EntryKind::Symlink
    } else {
        ensure!(meta.is_file(), "special filesystem object unsupported");
        EntryKind::File
    };
    Ok(Entry {
        path: path.into(),
        kind,
        mode: if meta.file_type().is_symlink() {
            0o777
        } else {
            meta.mode() & 0o777
        },
        size: 0,
        sha256: String::new(),
    })
}

fn current_fingerprint(root: &Path, path: &str) -> Result<Option<(String, u64, bool, EntryKind)>> {
    fsx::validate_relative(path)?;
    let metadata = match fs::symlink_metadata(root.join(path)) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let entry = entry(path, &metadata)?;
    let (sha, size) = fsx::fingerprint(root, &entry)?;
    let after = fs::symlink_metadata(root.join(path))?;
    ensure!(
        metadata.dev() == after.dev()
            && metadata.ino() == after.ino()
            && metadata.mode() == after.mode()
            && metadata.mtime() == after.mtime()
            && metadata.mtime_nsec() == after.mtime_nsec()
            && metadata.len() == after.len(),
        "source changed during read"
    );
    Ok(Some((
        sha,
        size,
        entry.kind != EntryKind::Symlink && metadata.mode() & 0o111 != 0,
        entry.kind,
    )))
}

fn compare_fingerprint(
    saved: &(String, u64, bool, EntryKind),
    current: Option<&(String, u64, bool, EntryKind)>,
) -> &'static str {
    match current {
        None => "absent_at_proposed_path",
        Some(current) if saved == current => "same_path_identical",
        Some(_) => "same_path_different",
    }
}
fn expected_matches(actual: &(String, u64, bool, EntryKind), expected: &ExpectedFile) -> bool {
    actual.0 == expected.sha256
        && actual.1 == expected.bytes
        && actual.2 == (expected.kind != "symlink" && expected.mode & 0o111 != 0)
        && (actual.3 == EntryKind::Symlink) == (expected.kind == "symlink")
}
fn error_category(error: &anyhow::Error) -> &'static str {
    if format!("{error:#}").contains("changed") {
        "changed_during_read"
    } else {
        "unsupported"
    }
}

#[derive(Clone)]
struct TreeEntry {
    mode: String,
    oid: String,
}
fn tree_entries(repo: &Path, commit: &str) -> Result<BTreeMap<String, TreeEntry>> {
    ensure!(valid_oid(commit), "invalid tree commit identity");
    let bytes = git::run(repo, &["ls-tree", "-r", "-z", "--full-tree", commit])?;
    let mut result = BTreeMap::new();
    for record in bytes.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let tab = record
            .iter()
            .position(|b| *b == b'\t')
            .context("malformed Git tree record")?;
        let head = std::str::from_utf8(&record[..tab])?
            .split_whitespace()
            .collect::<Vec<_>>();
        ensure!(
            head.len() == 3
                && head[1] == "blob"
                && ["100644", "100755", "120000"].contains(&head[0]),
            "unsupported saved Git tree entry"
        );
        let path = std::str::from_utf8(&record[tab + 1..])?.to_owned();
        fsx::validate_relative(&path)?;
        result.insert(
            path,
            TreeEntry {
                mode: head[0].into(),
                oid: head[2].into(),
            },
        );
    }
    Ok(result)
}
fn commit_present(repo: &Path, oid: &str) -> bool {
    valid_oid(oid) && git::text(repo, &["cat-file", "-t", oid]).is_ok_and(|kind| kind == "commit")
}

fn analyze_dependency(
    repo: &Path,
    dependency: &GitDependency,
    compare: bool,
    legacy_short: Option<&str>,
) -> Value {
    let mut result = json!({"group":dependency.group,"oid":dependency.oid,"reference":dependency.reference,"kind":if dependency.kind.is_empty(){"commit"}else{&dependency.kind},"status":"unverified","object_available":false,"identity_matches":false,"receipt_hashes_verified":false,"self_contained_capture":false,"historical_completeness":"unknown","tracked_rows":[],"notes":["A present Git object is an external recovery dependency, not an independent backup or completed restoration."]});
    let analyze = || -> Result<Value> {
        ensure!(valid_oid(&dependency.oid), "invalid saved commit identity");
        ensure!(
            dependency.kind.is_empty() || ["commit", "stash"].contains(&dependency.kind.as_str()),
            "unsupported dependency kind"
        );
        if !commit_present(repo, &dependency.oid) {
            result["status"] = json!("missing");
            return Ok(result);
        }
        result["object_available"] = json!(true);
        if let Some(short) = legacy_short {
            let normalized = dependency
                .reference
                .as_deref()
                .context("normalized branch reference missing")?;
            let present =
                git::output(repo, &["show-ref", "--verify", "--quiet", normalized], None)?;
            if present.status.code() == Some(1) {
                result["status"] = json!("reference_missing");
                return Ok(result);
            }
            ensure!(
                present.status.success(),
                "could not inspect normalized branch reference"
            );
            let resolved = git::output(
                repo,
                &[
                    "rev-parse",
                    "--symbolic-full-name",
                    "--verify",
                    "--end-of-options",
                    short,
                ],
                None,
            )?;
            if !resolved.status.success() {
                result["status"] = json!("reference_missing");
                return Ok(result);
            }
            let observed = std::str::from_utf8(&resolved.stdout)?.trim();
            if !resolved.stderr.is_empty() || Some(observed) != dependency.reference.as_deref() {
                result["status"] = json!("ambiguous_reference");
                result["notes"].as_array_mut().unwrap().push(json!("Legacy short reference did not resolve uniquely to its declared branch; no namespace is guessed."));
                return Ok(result);
            }
        }
        if let Some(reference) = &dependency.reference {
            ensure!(
                reference.starts_with("refs/"),
                "dependency reference must be an explicit ref"
            );
            git::run(repo, &["check-ref-format", reference])?;
            match git::text(repo, &["rev-parse", "--verify", reference]) {
                Ok(actual) if actual == dependency.oid => {}
                Ok(actual) => {
                    result["status"] = json!("ref_mismatch");
                    result["observed_reference_oid"] = json!(actual);
                    return Ok(result);
                }
                Err(_) => {
                    result["status"] = json!("reference_missing");
                    return Ok(result);
                }
            }
        }
        result["identity_matches"] = json!(true);
        if !compare {
            result["status"] = json!("available_unverified_content");
            return Ok(result);
        }
        let parents = git::text(repo, &["rev-list", "--parents", "-n", "1", &dependency.oid])?
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let stash = dependency.kind == "stash";
        ensure!(
            !parents.is_empty() && (!stash || (parents.len() >= 3 && parents.len() <= 4)),
            "unsupported saved stash topology"
        );
        if let Some(head) = &dependency.source_head {
            ensure!(
                valid_oid(head) && commit_present(repo, head),
                "declared source commit is unavailable"
            );
            if stash {
                ensure!(
                    parents.get(1) == Some(head),
                    "saved stash first parent differs from declared source commit"
                );
            }
        }
        let mut trees = BTreeMap::new();
        trees.insert("working", tree_entries(repo, &dependency.oid)?);
        if stash {
            trees.insert("base", tree_entries(repo, &parents[1])?);
            trees.insert("index", tree_entries(repo, &parents[2])?);
        }
        let mut oids = BTreeSet::new();
        for path in &dependency.tracked_paths {
            fsx::validate_relative(path)?;
            for tree in trees.values() {
                if let Some(entry) = tree.get(path) {
                    oids.insert(entry.oid.clone());
                }
            }
        }
        let fingerprints =
            git::batch_blob_fingerprints(repo, &oids.into_iter().collect::<Vec<_>>())?;
        let mut rows = Vec::new();
        let mut all_hashes = !dependency.tracked_paths.is_empty();
        let mut mismatches = 0;
        for path in &dependency.tracked_paths {
            let mut row = json!({"path":path,"layers":{},"comparison":"unverified","receipt_match":"not_supplied"});
            for (layer, tree) in &trees {
                if let Some(saved) = tree.get(path) {
                    let (sha, size) = fingerprints
                        .get(&saved.oid)
                        .context("missing saved blob fingerprint")?;
                    row["layers"][layer] =
                        json!({"oid":saved.oid,"sha256":sha,"bytes":size,"mode":saved.mode});
                } else {
                    row["layers"][layer] = Value::Null;
                }
            }
            if let Some(saved) = trees["working"].get(path) {
                let (sha, size) = fingerprints
                    .get(&saved.oid)
                    .context("missing saved working fingerprint")?;
                let value = (
                    sha.clone(),
                    *size,
                    saved.mode == "100755",
                    if saved.mode == "120000" {
                        EntryKind::Symlink
                    } else {
                        EntryKind::File
                    },
                );
                match current_fingerprint(repo, path) {
                    Ok(current) => {
                        row["comparison"] = json!(compare_fingerprint(&value, current.as_ref()))
                    }
                    Err(error) => {
                        row["comparison"] = json!(error_category(&error));
                        row["error"] = json!(format!("{error:#}"));
                    }
                }
                if let Some(expected) = dependency.expected_files.get(path) {
                    let matched = expected_matches(&value, expected);
                    row["receipt_match"] = json!(if matched { "matched" } else { "hash_mismatch" });
                    if !matched {
                        mismatches += 1;
                        all_hashes = false;
                    }
                } else {
                    all_hashes = false;
                }
            } else {
                row["comparison"] = json!("saved_path_missing");
                row["receipt_match"] = json!("saved_path_missing");
                all_hashes = false;
                mismatches += 1;
            }
            row["index_matches_base"] =
                json!(stash && row["layers"]["base"] == row["layers"]["index"]);
            rows.push(row);
        }
        result["tracked_rows"] = json!(rows);
        result["receipt_hashes_verified"] = json!(all_hashes);
        result["status"] = json!(if mismatches > 0 {
            "hash_mismatch"
        } else if all_hashes {
            "matched"
        } else {
            "available_content_checked"
        });
        Ok(result)
    };
    match analyze() {
        Ok(value) => value,
        Err(error) => {
            json!({"group":dependency.group,"oid":dependency.oid,"reference":dependency.reference,"status":"unsupported","error":format!("{error:#}"),"self_contained_capture":false,"historical_completeness":"unknown","tracked_rows":[]})
        }
    }
}

/// Metadata is always read; saved/current content is hashed only when comparison is requested.
pub fn analyze_folder(repo: &Path, folder: &Path, compare: bool) -> Result<Value> {
    let folder = fs::canonicalize(folder)?;
    let (leaves, directories, mut blockers) = leaf_metadata(&folder)?;
    let receipt_present = fs::symlink_metadata(folder.join("receipt.json")).is_ok();
    let (description, metadata_rejected) = match describe(&folder) {
        Ok(value) => (value, false),
        Err(error) => {
            blockers.push(format!("Recovery metadata rejected: {error:#}"));
            (None, true)
        }
    };
    let mut rows = Vec::new();
    let mut summary = [
        "same_path_identical",
        "same_path_different",
        "absent_at_proposed_path",
        "unmapped",
        "not_requested",
        "unsupported",
        "changed_during_read",
        "saved_file_missing",
        "receipt_hash_mismatch",
        "saved_git_versions",
        "saved_git_same_path_identical",
        "saved_git_same_path_different",
        "saved_git_absent_at_proposed_path",
    ]
    .into_iter()
    .map(|key| (key.to_owned(), 0u64))
    .collect::<BTreeMap<_, _>>();
    let mut metadata_bytes = 0u64;
    summary.insert("payload_files".into(), leaves.len() as u64);
    let mut present = BTreeSet::new();
    for (path, metadata) in &leaves {
        present.insert(path.as_str());
        metadata_bytes = metadata_bytes.saturating_add(metadata.len());
        let mapping = description.as_ref().and_then(|d| d.mappings.get(path));
        let proposed = if let Some(mapping) = mapping {
            Some(mapping.target.clone())
        } else if description.is_none() && !metadata_rejected {
            Some(path.clone())
        } else {
            None
        };
        let mut row = json!({"stored_path":path,"target_path":proposed,"group":mapping.map(|m|m.group.as_str()),"metadata_bytes":metadata.len(),"comparison":"not_requested","receipt_match":"not_supplied"});
        if proposed.is_none() {
            row["comparison"] = json!("unmapped");
        } else if compare {
            let compared = || -> Result<Value> {
                let saved = current_fingerprint(&folder, path)?
                    .context("saved file disappeared during inspection")?;
                let target = proposed.as_ref().context("no proposed target")?;
                let current = current_fingerprint(repo, target)?;
                row["comparison"] = json!(compare_fingerprint(&saved, current.as_ref()));
                row["sha256"] = json!(saved.0);
                row["bytes"] = json!(saved.1);
                row["executable"] = json!(saved.2);
                row["kind"] = json!(saved.3);
                if let Some(expected) = mapping.and_then(|m| m.expected.as_ref()) {
                    row["receipt_match"] = json!(if expected_matches(&saved, expected) {
                        "matched"
                    } else {
                        "hash_mismatch"
                    });
                }
                Ok(row)
            };
            row = match compared() {
                Ok(value) => value,
                Err(error) => {
                    json!({"stored_path":path,"target_path":proposed,"comparison":error_category(&error),"error":format!("{error:#}")})
                }
            };
        }
        let category = row["comparison"]
            .as_str()
            .unwrap_or("unsupported")
            .to_owned();
        *summary.entry(category).or_default() += 1;
        if row["receipt_match"] == "hash_mismatch" {
            *summary.entry("receipt_hash_mismatch".into()).or_default() += 1;
        }
        rows.push(row);
    }
    if let Some(description) = &description {
        for (stored, mapping) in &description.mappings {
            if !present.contains(stored.as_str()) {
                rows.push(json!({"stored_path":stored,"target_path":mapping.target,"group":mapping.group,"comparison":"saved_file_missing","receipt_match":"saved_file_missing"}));
                *summary.entry("saved_file_missing".into()).or_default() += 1;
            }
        }
    }
    let mut heads = Vec::new();
    let mut dependencies = Vec::new();
    let mut notes=vec!["Historical receipts are producer claims; this inspection independently checks only the selected repository and files present now.".to_owned(),"Exact same-path matches are candidates for review, not permission to discard a copy. Different versions do not establish that a fix is missing.".into()];
    if let Some(description) = &description {
        notes.extend(description.notes.clone());
        for (group, head) in &description.heads {
            heads.push(
                json!({"group":group,"oid":head,"available_as_commit":commit_present(repo,head)}),
            );
        }
        for dependency in &description.map.git_dependencies {
            dependencies.push(analyze_dependency(
                repo,
                dependency,
                compare,
                description
                    .legacy_short_references
                    .get(&dependency.group)
                    .map(String::as_str),
            ));
        }
    } else if !receipt_present {
        notes.push("No supported receipt was supplied. Relative saved paths are proposed target mappings only; original source lineage is unknown.".into());
    } else {
        notes.push("Receipt format was unrecognized or rejected. No historical completeness or trusted mapping is inferred from its presence.".into());
    }
    let unresolved = dependencies
        .iter()
        .filter(|d| {
            !matches!(
                d["status"].as_str(),
                Some("matched" | "available_content_checked" | "available_unverified_content")
            )
        })
        .count();
    let tracked_versions = dependencies
        .iter()
        .filter_map(|d| d["tracked_rows"].as_array())
        .map(Vec::len)
        .sum::<usize>();
    summary.insert("saved_git_versions".into(), tracked_versions as u64);
    for dependency in &dependencies {
        if let Some(rows) = dependency["tracked_rows"].as_array() {
            for row in rows {
                let comparison = row["comparison"].as_str().unwrap_or("unsupported");
                *summary
                    .entry(format!("saved_git_{comparison}"))
                    .or_default() += 1;
            }
        }
    }
    let cleanup = json!({"matching_payload_copies":summary.get("same_path_identical").copied().unwrap_or(0),"different_payload_versions":summary.get("same_path_different").copied().unwrap_or(0),"target_addition_candidates":summary.get("absent_at_proposed_path").copied().unwrap_or(0),"unresolved_git_dependencies":unresolved,"may_delete":false,"notes":["Resolve missing or differing versions, external dependencies, capture coverage, backup and ownership before any cleanup.","This report does not consolidate files, prune registrations, or delete work."]});
    Ok(
        json!({"path":folder,"kind":if receipt_present{"preservation_folder_with_receipt"}else{"unmanaged_folder"},"entries":leaves.len()+directories,"metadata_bytes":metadata_bytes,"comparison_requested":compare,"receipt_format":description.as_ref().map(|d|d.format),"receipt_claims":"not trusted as verification","historical_completeness":"unknown","independent_backup":"unassessed","self_contained_capture":false,"summary":summary,"payload_rows":rows,"source_heads":heads,"git_dependencies":dependencies,"derived_import_map":description.as_ref().map(|d|&d.map),"cleanup_candidates":cleanup,"blockers":blockers,"notes":notes}),
    )
}
