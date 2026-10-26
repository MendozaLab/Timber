//! Consolidation is performed only in a newly created independent repository.
use crate::{fsx, git, model::*, restore, store::Store};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    path::Path,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TreeFile {
    pub mode: String,
    pub oid: String,
}

pub(crate) fn import_manifest_objects(
    store: &Store,
    manifest: &Manifest,
    repo: &Path,
) -> Result<()> {
    let state = manifest
        .git
        .as_ref()
        .context("folder checkpoint has no Git history")?;
    ensure!(
        state.object_format == "sha1",
        "unsupported Git object format"
    );
    let mut bundle = tempfile::NamedTempFile::new_in(repo.join(".git"))?;
    bundle.write_all(&store.get(&state.bundle)?)?;
    bundle.as_file().sync_all()?;
    let path = bundle.path().to_str().context("bundle path is not UTF-8")?;
    git::run(repo, &["bundle", "verify", path]).context("bundle is not self-contained")?;
    git::run(repo, &["bundle", "unbundle", path])?;
    for entry in &state.index {
        let bytes = store.get(&entry.blob)?;
        let actual = String::from_utf8(git::run_input(
            repo,
            &["hash-object", "--stdin", "-w", "--no-filters"],
            &bytes,
        )?)?;
        ensure!(
            actual.trim() == entry.oid,
            "staged object identity mismatch for {:?}",
            entry.path
        );
    }
    Ok(())
}

pub(crate) fn tree_files(repo: &Path, revision: &str) -> Result<BTreeMap<String, TreeFile>> {
    let bytes = git::run(repo, &["ls-tree", "-r", "-z", "--full-tree", revision])?;
    let mut files = BTreeMap::new();
    for record in bytes.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let tab = record
            .iter()
            .position(|b| *b == b'\t')
            .context("malformed ls-tree record")?;
        let fields: Vec<_> = std::str::from_utf8(&record[..tab])?
            .split_whitespace()
            .collect();
        ensure!(
            fields.len() == 3 && fields[1] == "blob",
            "unsupported tree object"
        );
        let path = std::str::from_utf8(&record[tab + 1..])?.to_owned();
        fsx::validate_relative(&path)?;
        ensure!(
            matches!(fields[0], "100644" | "100755" | "120000"),
            "unsupported tree mode"
        );
        ensure!(
            files
                .insert(
                    path,
                    TreeFile {
                        mode: fields[0].into(),
                        oid: fields[2].into()
                    }
                )
                .is_none(),
            "duplicate tree path"
        );
    }
    validate_tree_paths(&files)?;
    Ok(files)
}

fn validate_tree_paths(files: &BTreeMap<String, TreeFile>) -> Result<()> {
    let entries: Vec<_> = files
        .iter()
        .map(|(path, file)| Entry {
            path: path.clone(),
            kind: if file.mode == "120000" {
                EntryKind::Symlink
            } else {
                EntryKind::File
            },
            mode: if file.mode == "100755" { 0o755 } else { 0o644 },
            size: 0,
            sha256: "0".repeat(64),
        })
        .collect();
    fsx::validate_entries(&entries)
}

pub(crate) fn validate_selection(manifest: &Manifest, selection: &Selection) -> Result<()> {
    ensure!(
        manifest.id == selection.checkpoint,
        "checkpoint selection mismatch"
    );
    ensure!(
        selection.layer == Layer::Working || selection.extra_paths.is_empty(),
        "only working layer supports explicitly selected extra files"
    );
    let mut seen = BTreeSet::new();
    for path in &selection.extra_paths {
        fsx::validate_relative(path)?;
        ensure!(seen.insert(path), "duplicate extra path {:?}", path);
        ensure!(
            manifest
                .entries
                .iter()
                .any(|e| e.path == *path || e.path.starts_with(&format!("{path}/"))),
            "extra path {:?} was not captured",
            path
        );
    }
    if manifest.git.is_none() {
        ensure!(
            selection.layer == Layer::Working,
            "folder imports only support the working layer"
        );
    }
    Ok(())
}

fn write_blob(store: &Store, repo: &Path, entry: &Entry) -> Result<TreeFile> {
    ensure!(
        entry.kind != EntryKind::Directory,
        "cannot add directory as a file"
    );
    let bytes = store.get(&entry.sha256)?;
    let oid = String::from_utf8(git::run_input(
        repo,
        &["hash-object", "--stdin", "-w", "--no-filters"],
        &bytes,
    )?)?
    .trim()
    .to_owned();
    let mode = if entry.kind == EntryKind::Symlink {
        "120000"
    } else if entry.mode & 0o111 != 0 {
        "100755"
    } else {
        "100644"
    };
    Ok(TreeFile {
        mode: mode.into(),
        oid,
    })
}

pub(crate) fn selected_files(
    store: &Store,
    repo: &Path,
    manifest: &Manifest,
    selection: &Selection,
) -> Result<BTreeMap<String, TreeFile>> {
    validate_selection(manifest, selection)?;
    let state = manifest
        .git
        .as_ref()
        .context("folder imports use explicit mappings instead of a Git tree")?;
    if selection.layer == Layer::Committed {
        ensure!(
            selection.extra_paths.is_empty(),
            "committed layer cannot include extra working files"
        );
        return tree_files(repo, &state.head);
    }
    let mut files = BTreeMap::new();
    if selection.layer == Layer::Index {
        ensure!(
            selection.extra_paths.is_empty(),
            "index layer cannot include extra working files"
        );
        for entry in &state.index {
            files.insert(
                entry.path.clone(),
                TreeFile {
                    mode: entry.mode.clone(),
                    oid: entry.oid.clone(),
                },
            );
        }
    } else {
        let tracked: BTreeSet<_> = state.index.iter().map(|e| e.path.as_str()).collect();
        for entry in &manifest.entries {
            if entry.kind == EntryKind::Directory {
                continue;
            }
            let selected_extra = selection
                .extra_paths
                .iter()
                .any(|p| entry.path == *p || entry.path.starts_with(&format!("{p}/")));
            if tracked.contains(entry.path.as_str()) || selected_extra {
                files.insert(entry.path.clone(), write_blob(store, repo, entry)?);
            }
        }
    }
    Ok(files)
}

fn tree_from_files(repo: &Path, files: &BTreeMap<String, TreeFile>) -> Result<String> {
    validate_tree_paths(files)?;
    git::run(repo, &["read-tree", "--empty"])?;
    let mut data = Vec::new();
    for (path, file) in files {
        fsx::validate_relative(path)?;
        data.extend_from_slice(format!("{} {}\t{}", file.mode, file.oid, path).as_bytes());
        data.push(0);
    }
    git::run_input(repo, &["update-index", "-z", "--index-info"], &data)?;
    git::text(repo, &["write-tree"])
}

fn commit(repo: &Path, tree: &str, parents: &[&str], message: &str) -> Result<String> {
    let mut args = vec![
        "-c",
        "user.name=Timber Preview",
        "-c",
        "user.email=preview@timber.invalid",
        "commit-tree",
        "--no-gpg-sign",
        tree,
    ];
    for p in parents {
        args.extend(["-p", p]);
    }
    Ok(
        String::from_utf8(git::run_input(repo, &args, message.as_bytes())?)?
            .trim()
            .into(),
    )
}

pub(crate) fn selected_commit(
    store: &Store,
    repo: &Path,
    manifest: &Manifest,
    selection: &Selection,
) -> Result<String> {
    let state = manifest
        .git
        .as_ref()
        .context("target must be a Git checkpoint")?;
    reject_unsafe_attributes(repo, &state.head)?;
    if selection.layer == Layer::Committed {
        return Ok(state.head.clone());
    }
    let files = selected_files(store, repo, manifest, selection)?;
    let tree = tree_from_files(repo, &files)?;
    reject_unsafe_attributes(repo, &tree)?;
    commit(
        repo,
        &tree,
        &[&state.head],
        &format!(
            "Timber preview snapshot\n\nCheckpoint: {}\nLayer: {:?}\n",
            manifest.id, selection.layer
        ),
    )
}

fn reject_unsafe_attributes(repo: &Path, tree: &str) -> Result<()> {
    for (path, file) in tree_files(repo, tree)? {
        if path == ".gitattributes" || path.ends_with("/.gitattributes") {
            let bytes = git::run(repo, &["cat-file", "blob", &file.oid])?;
            let text =
                std::str::from_utf8(&bytes).context("non-UTF-8 attributes are unsupported")?;
            for line in text.lines().filter(|l| !l.trim_start().starts_with('#')) {
                for field in line.split_whitespace() {
                    if let Some(driver) = field.strip_prefix("merge=") {
                        ensure!(
                            matches!(driver, "text" | "binary"),
                            "unsupported custom or union merge driver in {:?}",
                            path
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

fn materialize_head(repo: &Path, revision: &str) -> Result<()> {
    let files = tree_files(repo, revision)?;
    let mut entries = Vec::new();
    let mut contents = BTreeMap::new();
    let mut dirs = BTreeSet::new();
    for (path, file) in files {
        let bytes = git::run(repo, &["cat-file", "blob", &file.oid])?;
        let digest = fsx::digest(&bytes);
        let mut parent = Path::new(&path).parent();
        while let Some(p) = parent {
            if p.as_os_str().is_empty() {
                break;
            }
            dirs.insert(p.to_str().context("invalid parent path")?.to_owned());
            parent = p.parent();
        }
        entries.push(Entry {
            path,
            kind: if file.mode == "120000" {
                EntryKind::Symlink
            } else {
                EntryKind::File
            },
            mode: if file.mode == "100755" {
                0o755
            } else if file.mode == "120000" {
                0o777
            } else {
                0o644
            },
            size: bytes.len() as u64,
            sha256: digest.clone(),
        });
        contents.insert(digest, bytes);
    }
    entries.extend(dirs.into_iter().map(|path| Entry {
        path,
        kind: EntryKind::Directory,
        mode: 0o755,
        size: 0,
        sha256: String::new(),
    }));
    fsx::validate_entries(&entries)?;
    fsx::materialize(repo, &entries, |sha| {
        contents
            .get(sha)
            .cloned()
            .context("materialization content missing")
    })?;
    git::run(repo, &["read-tree", revision])?;
    Ok(())
}

fn folder_files(
    manifest: &Manifest,
    selection: &Selection,
) -> Result<Vec<(Entry, String, Option<String>)>> {
    validate_selection(manifest, selection)?;
    let mut files = Vec::new();
    if let Some(map) = &manifest.import_map {
        ensure!(map.schema == SCHEMA, "unsupported import map schema");
        let mut destinations = BTreeSet::new();
        for mapping in &map.files {
            fsx::validate_relative(&mapping.stored_path)?;
            fsx::validate_relative(&mapping.target_path)?;
            ensure!(
                destinations.insert(&mapping.target_path),
                "duplicate mapped destination"
            );
            let entry = manifest
                .entries
                .iter()
                .find(|e| e.path == mapping.stored_path && e.kind != EntryKind::Directory)
                .context("mapped source file not captured")?;
            files.push((
                entry.clone(),
                mapping.target_path.clone(),
                mapping.base_sha256.clone(),
            ));
        }
    }
    for entry in &manifest.entries {
        if entry.kind != EntryKind::Directory
            && selection
                .extra_paths
                .iter()
                .any(|p| entry.path == *p || entry.path.starts_with(&format!("{p}/")))
        {
            ensure!(
                !files.iter().any(|(_, p, _)| *p == entry.path),
                "duplicate extra mapped destination"
            );
            files.push((entry.clone(), entry.path.clone(), None));
        }
    }
    ensure!(
        !files.is_empty(),
        "folder import has no selected files: supply an import map or explicit extra_paths in the plan"
    );
    Ok(files)
}

fn parse_conflict_records(bytes: &[u8]) -> Result<Vec<Value>> {
    let tokens: Vec<_> = bytes.split(|b| *b == 0).collect();
    let mut cursor = 1;
    let mut records = Vec::new();
    while cursor < tokens.len() && !tokens[cursor].is_empty() {
        let raw = tokens[cursor];
        let tab = raw
            .iter()
            .position(|b| *b == b'\t')
            .context("malformed Git conflict entry")?;
        let fields: Vec<_> = std::str::from_utf8(&raw[..tab])?
            .split_whitespace()
            .collect();
        ensure!(fields.len() == 3, "malformed Git conflict fields");
        records.push(json!({"kind":"index_stage", "mode":fields[0], "oid":fields[1], "stage":fields[2], "path":std::str::from_utf8(&raw[tab+1..])?}));
        cursor += 1;
    }
    cursor += 1; // blank record separates higher-order stages from messages.
    while cursor < tokens.len() && !tokens[cursor].is_empty() {
        let count: usize = std::str::from_utf8(tokens[cursor])?
            .parse()
            .context("malformed Git conflict message path count")?;
        cursor += 1;
        ensure!(
            cursor + count + 2 <= tokens.len(),
            "truncated Git conflict message"
        );
        let paths: Vec<_> = tokens[cursor..cursor + count]
            .iter()
            .map(|p| std::str::from_utf8(p).map(str::to_owned))
            .collect::<std::result::Result<_, _>>()?;
        cursor += count;
        let kind = std::str::from_utf8(tokens[cursor])?;
        let message = std::str::from_utf8(tokens[cursor + 1])?;
        records.push(
            json!({"kind":"message", "conflict_type":kind, "paths":paths, "message":message}),
        );
        cursor += 2;
    }
    Ok(records)
}

fn persist(repo: &Path, result: &PreviewResult) -> Result<()> {
    std::fs::write(
        repo.join(".git/timber-preview.json"),
        serde_json::to_vec_pretty(result)?,
    )?;
    Ok(())
}

pub fn build(store: &Store, plan: &Plan, to: &Path) -> Result<PreviewResult> {
    ensure!(plan.schema == SCHEMA, "unsupported plan schema");
    let target = store.load(&plan.target.checkpoint)?;
    ensure!(
        target.git.is_some(),
        "preview target must be a Git checkpoint"
    );
    let mut manifests = vec![target.clone()];
    for selection in &plan.sources {
        manifests.push(store.load(&selection.checkpoint)?);
    }
    for (manifest, selection) in manifests
        .iter()
        .zip(std::iter::once(&plan.target).chain(&plan.sources))
    {
        validate_selection(manifest, selection)?;
        fsx::ensure_outside(to, Path::new(&manifest.source_path))?;
        if let Some(state) = &manifest.git {
            for root in &state.protected_roots {
                fsx::ensure_outside(to, Path::new(root))?;
            }
        }
        let receipt = restore::verify(store, &manifest.id)?;
        ensure!(
            receipt.verified,
            "checkpoint {} did not verify",
            manifest.id
        );
    }
    fsx::ensure_outside(to, &store.root)?;
    fsx::fresh_dir(to)?;
    git::init(to)?;
    for manifest in &manifests {
        if manifest.git.is_some() {
            import_manifest_objects(store, manifest, to)?;
        }
    }
    let mut current = selected_commit(store, to, &target, &plan.target)?;
    let mut result = PreviewResult { schema: SCHEMA, status: "incomplete".into(), destination: std::fs::canonicalize(to)?.display().to_string(), plan_sha256: fsx::digest(&serde_json::to_vec(plan)?), completed_sources: Vec::new(), pending_sources: plan.sources.iter().map(|s| s.checkpoint.clone()).collect(), head: current.clone(), conflict_tree: None, conflict_records: Vec::new(), protected_roots: Vec::new(), notes: vec!["Mechanical integration only; no project code or tests were executed. Original repositories are untouched.".into(), "Commit timestamps may vary between runs; selected contents and the recorded source order determine the preview tree.".into()] };
    std::fs::write(
        to.join(".git/timber-plan.json"),
        serde_json::to_vec_pretty(plan)?,
    )?;
    result.protected_roots = manifests
        .iter()
        .flat_map(|manifest| {
            let mut roots = vec![manifest.source_path.clone()];
            if let Some(state) = &manifest.git {
                roots.extend(state.protected_roots.clone());
            }
            roots
        })
        .collect();
    result.protected_roots.sort();
    result.protected_roots.dedup();
    persist(to, &result)?;
    for (selection, manifest) in plan.sources.iter().zip(manifests.iter().skip(1)) {
        let candidate = if manifest.git.is_some() {
            selected_commit(store, to, manifest, selection)?
        } else {
            result.notes.push(format!("Folder checkpoint {} contributes only explicitly selected present files. Historical completeness is unknown; declared external Git dependencies are not reconstructed.", manifest.id));
            let mut files = tree_files(to, &current)?;
            let mut conflicts = Vec::new();
            for (entry, target_path, base) in folder_files(manifest, selection)? {
                let incoming = write_blob(store, to, &entry)?;
                if let Some(existing) = files.get(&target_path) {
                    if existing != &incoming {
                        conflicts.push(format!("Folder path {:?} differs from target; declared base {:?} is not sufficient for automatic overwrite in v0.1", target_path, base));
                        continue;
                    }
                } else if files.keys().any(|p| {
                    p.starts_with(&format!("{target_path}/"))
                        || target_path.starts_with(&format!("{p}/"))
                }) {
                    conflicts.push(format!(
                        "Folder path {:?} collides with a target file or directory",
                        target_path
                    ));
                    continue;
                }
                files.insert(target_path, incoming);
            }
            if !conflicts.is_empty() {
                result.status = "conflicted".into();
                result.conflict_records = conflicts
                    .into_iter()
                    .map(|message| json!({"kind":"folder_collision","message":message}))
                    .collect();
                result.notes.push("Folder mappings never establish historical completeness; differing existing files require manual review.".into());
                break;
            }
            let tree = tree_from_files(to, &files)?;
            reject_unsafe_attributes(to, &tree)?;
            commit(
                to,
                &tree,
                &[&current],
                &format!(
                    "Timber mapped folder additions\n\nCheckpoint: {}\n",
                    manifest.id
                ),
            )?
        };
        reject_unsafe_attributes(to, &current)?;
        let output = git::output(
            to,
            &[
                "merge-tree",
                "--write-tree",
                "-z",
                "--messages",
                &current,
                &candidate,
            ],
            None,
        )?;
        match output.status.code() {
            Some(0) => {
                let tree = std::str::from_utf8(
                    output
                        .stdout
                        .split(|b| *b == 0 || *b == b'\n')
                        .next()
                        .unwrap_or_default(),
                )?
                .trim()
                .to_owned();
                ensure!(
                    tree.len() == 40 && tree.bytes().all(|b| b.is_ascii_hexdigit()),
                    "invalid merge-tree result"
                );
                reject_unsafe_attributes(to, &tree)?;
                current = commit(
                    to,
                    &tree,
                    &[&current, &candidate],
                    &format!(
                        "Timber consolidation preview\n\nCheckpoint: {}\n",
                        manifest.id
                    ),
                )?;
                result.completed_sources.push(manifest.id.clone());
                result.pending_sources.remove(0);
                result.head = current.clone();
                persist(to, &result)?;
            }
            Some(1) => {
                result.status = "conflicted".into();
                let records: Vec<_> = output
                    .stdout
                    .split(|b| *b == 0)
                    .map(|r| String::from_utf8_lossy(r).into_owned())
                    .collect();
                result.conflict_tree = records.first().filter(|r| r.len() == 40).cloned();
                result.conflict_records = parse_conflict_records(&output.stdout)?;
                result.notes.push("Git reported a conflict. The working files show the last clean step; the conflict tree and raw Git records are retained under .git.".into());
                std::fs::write(to.join(".git/timber-conflict-output.bin"), &output.stdout)?;
                break;
            }
            _ => bail!(
                "preview merge failed; incomplete operation retained at {}: {}",
                to.display(),
                String::from_utf8_lossy(&output.stderr)
            ),
        }
    }
    if result.pending_sources.is_empty() {
        result.status = "complete".into();
    }
    git::run(to, &["update-ref", "refs/heads/timber-preview", &current])?;
    git::run(to, &["symbolic-ref", "HEAD", "refs/heads/timber-preview"])?;
    materialize_head(to, &current)?;
    persist(to, &result)?;
    Ok(result)
}
