// Copyright 2026 Kenneth A. Mendoza (MendozaLab). https://github.com/MendozaLab/Timber
// SPDX-License-Identifier: Apache-2.0
//! Source reads only; never ask Git to refresh an index or run content filters.
use crate::{
    fsx, git,
    model::{Entry, EntryKind, INSPECT_SCHEMA},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::os::unix::fs::MetadataExt;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

/// Deep scans are explicit; registration discovery always remains repository-wide.
#[derive(Debug, Clone, Default)]
pub struct InspectOptions {
    pub all_worktrees: bool,
    pub worktrees: Vec<PathBuf>,
    pub compare_folders: bool,
    /// Emit every ignored path instead of the bounded summary.
    pub list_ignored: bool,
}

/// Most top-level paths an ignored-file summary names.
pub const IGNORED_TOP_DIRS_LIMIT: usize = 20;

/// Bounded view of ignored-path rows: total entries, total recorded bytes, and the
/// top-level paths (first path component) contributing the most entries, ordered by
/// entry count descending, then path. Bytes are the `bytes` recorded per row; a
/// collapsed directory contributes its directory entry size, not its contents.
pub fn ignored_summary(rows: &[Value]) -> Value {
    let mut groups = BTreeMap::<&str, (u64, u64)>::new();
    let mut total = 0u64;
    for row in rows {
        let bytes = row["bytes"].as_u64().unwrap_or(0);
        total = total.saturating_add(bytes);
        let path = row["path"].as_str().unwrap_or_default();
        let top = path.split('/').next().unwrap_or(path);
        let group = groups.entry(top).or_default();
        group.0 += 1;
        group.1 = group.1.saturating_add(bytes);
    }
    let mut top = groups.into_iter().collect::<Vec<_>>();
    top.sort_by(|a, b| b.1.0.cmp(&a.1.0).then_with(|| a.0.cmp(b.0)));
    let top_dirs = top
        .into_iter()
        .take(IGNORED_TOP_DIRS_LIMIT)
        .map(|(path, (count, bytes))| json!({"path":path,"count":count,"bytes":bytes}))
        .collect::<Vec<_>>();
    json!({"count":rows.len(),"bytes":total,"top_dirs":top_dirs})
}

pub fn paths(repo: &Path, args: &[&str]) -> Result<Vec<String>> {
    git::run(repo, args)?
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| {
            let path = String::from_utf8(p.to_vec()).context("non-UTF-8 path unsupported")?;
            // ls-files may return a terminal slash for a collapsed directory
            // (including an ignored nested repository). Remove only that marker;
            // every remaining component still passes the normal strict validator.
            let path = path.strip_suffix('/').unwrap_or(&path).to_owned();
            fsx::validate_relative(&path)?;
            Ok(path)
        })
        .collect()
}

pub fn index(repo: &Path) -> Result<BTreeMap<String, (String, String)>> {
    records(&git::run(repo, &["ls-files", "--stage", "-z"])?, true)
}
/// Committed tree entries of HEAD, keyed by path, as (mode, object identity).
pub fn head_tree(repo: &Path) -> Result<BTreeMap<String, (String, String)>> {
    records(&git::run(repo, &["ls-tree", "-r", "-z", "HEAD"])?, false)
}
fn records(data: &[u8], index: bool) -> Result<BTreeMap<String, (String, String)>> {
    let mut entries = BTreeMap::new();
    for record in data.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let tab = record
            .iter()
            .position(|b| *b == b'\t')
            .context("invalid Git path record")?;
        let header = std::str::from_utf8(&record[..tab])?
            .split_whitespace()
            .collect::<Vec<_>>();
        ensure!(header.len() == 3, "invalid Git index/tree record");
        if index {
            ensure!(header[2] == "0", "unresolved index stages unsupported");
        }
        let path =
            String::from_utf8(record[tab + 1..].to_vec()).context("non-UTF-8 path unsupported")?;
        fsx::validate_relative(&path)?;
        let oid = if index { header[1] } else { header[2] };
        ensure!(
            entries
                .insert(path, (header[0].to_owned(), oid.to_owned()))
                .is_none(),
            "duplicate Git path record"
        );
    }
    Ok(entries)
}
fn file_info(root: &Path, path: &str) -> Value {
    match fs::symlink_metadata(root.join(path)) {
        Ok(m) => {
            json!({"path":path,"bytes":m.len(),"kind":if m.file_type().is_symlink(){"symlink"}else if m.is_dir(){"directory"}else if m.is_file(){"file"}else{"unsupported_special_file"},"cloud_placeholder":fsx::is_placeholder(&m)})
        }
        Err(e) => {
            json!({"path":path,"kind":"metadata_unavailable","status":"unknown","error":e.to_string()})
        }
    }
}

/// Deep inspection of one worktree. `ignored` is always the complete per-path list
/// here; `inspect_with_options` bounds it for output unless `list_ignored` is set.
pub fn worktree(path: &Path) -> Result<Value> {
    let started = Instant::now();
    let head = git::text(path, &["rev-parse", "--verify", "HEAD"])?;
    let index = index(path)?;
    let base = head_tree(path)?;
    let mut staged = Vec::new();
    for name in index.keys().chain(base.keys()).collect::<BTreeSet<_>>() {
        if index.get(name) != base.get(name) {
            staged.push(name.to_owned());
        }
    }
    let mut working = Vec::new();
    let mut unknown = Vec::new();
    let mut blockers: Vec<Value> = Vec::new();
    if let Err(error) = crate::capture::validate_worktree(path) {
        blockers.push(format!("Checkpoint unavailable: {error:#}").into());
    }
    let requested: Vec<_> = index
        .values()
        .filter(|(mode, _)| mode != "160000")
        .map(|(_, oid)| oid.clone())
        .collect();
    let unique_count = requested.iter().collect::<BTreeSet<_>>().len();
    // One batch request replaces one process per indexed path. Object identities,
    // not filenames, are sent to Git, so embedded tabs/newlines remain harmless.
    let fingerprints = git::batch_blob_fingerprints(path, &requested)?;
    let mut compared = 0usize;
    let mut submodules = Vec::new();
    for (name, (mode, oid)) in &index {
        if mode == crate::capture::GITLINK_MODE {
            // A submodule is recorded by checkpoint as a pointer; its files are never
            // read. Only an unreadable gitlink remains a blocker.
            match crate::capture::submodule_record(path, name, oid) {
                Ok(record) => {
                    if record
                        .checked_out_commit
                        .as_ref()
                        .is_some_and(|head| head != oid)
                    {
                        working.push(name.clone());
                    }
                    submodules.push(record);
                }
                Err(error) => {
                    blockers.push(format!("submodule unreadable: {name}: {error:#}").into());
                    unknown.push(name.clone());
                }
            }
            continue;
        }
        let metadata = match fs::symlink_metadata(path.join(name)) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                working.push(name.clone());
                continue;
            }
            Err(e) => {
                unknown.push(name.clone());
                blockers.push(format!("working file metadata unavailable: {name:?}: {e}").into());
                continue;
            }
        };
        if fsx::is_placeholder(&metadata) {
            blockers.push(format!("cloud placeholder: {name}").into());
            unknown.push(name.clone());
            continue;
        }
        if !metadata.is_file() && !metadata.file_type().is_symlink() {
            working.push(name.clone());
            continue;
        }
        let entry = Entry {
            path: name.clone(),
            kind: if metadata.file_type().is_symlink() {
                EntryKind::Symlink
            } else {
                EntryKind::File
            },
            mode: metadata.mode() & 0o777,
            size: 0,
            sha256: String::new(),
        };
        let observed = match fsx::fingerprint(path, &entry) {
            Ok(observed) => observed,
            Err(error) => {
                unknown.push(name.clone());
                blockers.push(format!("working file read unavailable: {name:?}: {error:#}").into());
                continue;
            }
        };
        let expected = fingerprints
            .get(oid)
            .context("batch omitted an indexed blob")?;
        let executable = metadata.mode() & 0o111 != 0;
        if observed.0 != expected.0
            || observed.1 != expected.1
            || (mode == "120000") != metadata.file_type().is_symlink()
            || (mode == "100755" && !executable)
            || (mode == "100644" && executable)
        {
            working.push(name.clone());
        }
        compared += 1;
    }
    let untracked = paths(path, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    let ignored = paths(
        path,
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "-z",
        ],
    )?;
    let untracked = untracked
        .iter()
        .map(|p| file_info(path, p))
        .collect::<Vec<_>>();
    let ignored = ignored
        .iter()
        .map(|p| file_info(path, p))
        .collect::<Vec<_>>();
    let unknown_metadata = untracked
        .iter()
        .chain(&ignored)
        .filter(|row| row["kind"] == "metadata_unavailable")
        .map(|row| row["path"].clone())
        .collect::<Vec<_>>();
    if !unknown_metadata.is_empty() {
        blockers.push("Some untracked/ignored metadata became unavailable during inspection; those entries remain unknown.".into());
    }
    let gitlinks: BTreeSet<_> = index
        .iter()
        .filter(|(_, (mode, _))| mode == crate::capture::GITLINK_MODE)
        .map(|(p, _)| p.as_str())
        .collect();
    let mut linked_worktrees_inside = Vec::new();
    let nested_scan_complete = match nested_repository_blocker(path, &index, &base, &gitlinks) {
        Ok((blocker, linked)) => {
            blockers.extend(blocker);
            linked_worktrees_inside = linked;
            true
        }
        Err(error) => {
            blockers.push(
                format!("Nested repository scan unavailable; checkpoint may refuse this worktree: {error:#}")
                    .into(),
            );
            false
        }
    };
    let head_after = git::text(path, &["rev-parse", "--verify", "HEAD"])?;
    let index_after = self::index(path)?;
    let git_identity_stable = head == head_after && index == index_after;
    if !git_identity_stable {
        blockers.push(
            "HEAD or logical index changed during inspection; repeat after writers pause".into(),
        );
    }
    Ok(json!({
        "path":path,"kind":"registered_worktree","inspection_status":if unknown.is_empty() && unknown_metadata.is_empty() && git_identity_stable && nested_scan_complete {"scanned"}else{"partial"},
        "head":head,"staged_paths":staged,"working_paths":working,"unknown_working_paths":unknown,
        "untracked":untracked,"ignored":ignored,"unknown_metadata_paths":unknown_metadata,
        "blockers":blockers,"linked_worktrees_inside":linked_worktrees_inside,"submodules_recorded_not_captured":submodules,"git_identity_stable":git_identity_stable,"writer_exclusion_enforced":false,
        "statistics":{"index_paths":index.len(),"git_blob_objects_requested":requested.len(),"unique_index_blob_objects":unique_count,"cat_file_batch_processes":usize::from(!requested.is_empty()),"working_files_compared":compared,"elapsed_seconds":started.elapsed().as_secs_f64()},
        "note":"Working differences compare raw bytes without clean filters or index refresh. Per-file stable reads and unchanged HEAD/index do not establish a frozen snapshot or permission to clean up."
    }))
}

/// Message carried by the nested-repository blocker in JSON, terminal and HTML views.
pub const NESTED_REPOSITORIES_MESSAGE: &str =
    "checkpoint will refuse this worktree until nested repositories are handled";

/// Lists every nested repository below the worktree (a directory holding its own
/// `.git` directory or file) other than a submodule at a gitlink path, without
/// reading its contents, and classifies each path as tracked, ignored or untracked.
/// Classification spends one Git process for the common directory and one for
/// ignore rules, and only when a nested repository exists.
///
/// Returns the blocker for foreign nested repositories (none when there are none) and
/// the linked worktrees of this repository found inside the worktree. A linked
/// worktree is reported separately and is not a blocker, because checkpoint skips it
/// and records it as `linked_worktree_captured_separately`. A linked worktree whose
/// folder holds tracked paths of this worktree cannot be skipped and stays a blocker.
fn nested_repository_blocker(
    root: &Path,
    index: &BTreeMap<String, (String, String)>,
    base: &BTreeMap<String, (String, String)>,
    gitlinks: &BTreeSet<&str>,
) -> Result<(Option<Value>, Vec<Value>)> {
    // A repository at a gitlink path is a submodule, recorded as a pointer by
    // checkpoint; it is reported under `submodules_recorded_not_captured` instead.
    if fsx::nested_repositories(root)?
        .iter()
        .all(|r| gitlinks.contains(r.as_str()))
    {
        return Ok((None, Vec::new()));
    }
    let common = git::common_dir(root)?;
    let found = fsx::classify_nested_repositories(root, &common, |_| false)?
        .into_iter()
        .filter(|r| !gitlinks.contains(r.path.as_str()))
        .collect::<Vec<_>>();
    if found.is_empty() {
        return Ok((None, Vec::new()));
    }
    let covers = |repository: &str, path: &str| {
        path == repository
            || path
                .strip_prefix(repository)
                .is_some_and(|rest| rest.starts_with('/'))
    };
    let tracked = found
        .iter()
        .map(|r| r.path.as_str())
        .filter(|r| index.keys().chain(base.keys()).any(|p| covers(r, p)))
        .collect::<BTreeSet<_>>();
    let mut input = Vec::new();
    for repository in found.iter().filter(|r| !tracked.contains(r.path.as_str())) {
        input.extend_from_slice(repository.path.as_bytes());
        input.push(0);
    }
    let mut ignored = BTreeSet::new();
    if !input.is_empty() {
        // check-ignore exits 1 when no supplied path is ignored.
        let output = git::output(root, &["check-ignore", "-z", "--stdin"], Some(&input))?;
        ensure!(
            matches!(output.status.code(), Some(0 | 1)),
            "Git check-ignore failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
        for record in output.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
            ignored
                .insert(String::from_utf8(record.to_vec()).context("non-UTF-8 path unsupported")?);
        }
    }
    let mut repositories = Vec::new();
    let mut linked = Vec::new();
    for repository in &found {
        let path = repository.path.as_str();
        let tracking = if tracked.contains(path) {
            "tracked"
        } else if ignored.contains(path) {
            "ignored"
        } else {
            "untracked"
        };
        match &repository.linked_worktree {
            Some(name) if tracking != "tracked" => {
                linked.push(json!({"path":path,"registration":name,"tracking":tracking}));
            }
            Some(name) => {
                repositories.push(json!({"path":path,"tracking":tracking,"linked_worktree":name}))
            }
            None => repositories.push(json!({"path":path,"tracking":tracking})),
        }
    }
    let blocker = (!repositories.is_empty()).then(|| {
        json!({
            "kind":"nested_repositories","message":NESTED_REPOSITORIES_MESSAGE,"count":repositories.len(),"repositories":repositories,
            "remedy":"Handle each nested repository separately, or name a disposable untracked or ignored folder with checkpoint --exclude for that run."
        })
    });
    Ok((blocker, linked))
}

fn registrations(data: &[u8]) -> Result<Vec<BTreeMap<String, String>>> {
    let mut rows = Vec::new();
    let mut current = BTreeMap::new();
    for item in data.split(|b| *b == 0) {
        if item.is_empty() {
            if !current.is_empty() {
                ensure!(
                    current.contains_key("worktree"),
                    "registration lacks a worktree path"
                );
                rows.push(std::mem::take(&mut current));
            }
            continue;
        }
        let line = std::str::from_utf8(item).context("non-UTF-8 worktree path unsupported")?;
        let (key, value) = line.split_once(' ').unwrap_or((line, "true"));
        ensure!(
            current.insert(key.to_owned(), value.to_owned()).is_none(),
            "duplicate worktree registration field"
        );
    }
    if !current.is_empty() {
        ensure!(
            current.contains_key("worktree"),
            "registration lacks a worktree path"
        );
        rows.push(current);
    }
    Ok(rows)
}

fn normalized(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return Ok(fs::canonicalize(path)?);
    }
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    ensure!(
        !path
            .components()
            .any(|p| matches!(p, std::path::Component::ParentDir)),
        "missing worktree selection must have a normalized path"
    );
    Ok(path)
}

pub fn inspect(repo: &Path, folders: &[PathBuf]) -> Result<Value> {
    inspect_with_options(repo, folders, &InspectOptions::default())
}

pub fn inspect_with_options(
    repo: &Path,
    folders: &[PathBuf],
    options: &InspectOptions,
) -> Result<Value> {
    let started = Instant::now();
    let repo = fs::canonicalize(repo)?;
    let data = git::run(&repo, &["worktree", "list", "--porcelain", "-z"])?;
    let registrations = registrations(&data)?;
    let registered: BTreeSet<_> = registrations
        .iter()
        .map(|r| normalized(Path::new(&r["worktree"])))
        .collect::<Result<_>>()?;
    let requested_root = registered
        .iter()
        .filter(|root| repo.starts_with(root))
        .max_by_key(|root| root.components().count())
        .context("--repo must select a registered worktree or a directory inside it")?
        .clone();
    let target_root = requested_root.clone();
    let mut selected = BTreeSet::from([requested_root]);
    for path in &options.worktrees {
        let selected_path = normalized(path)?;
        ensure!(
            registered.contains(&selected_path),
            "explicit worktree is not registered in this repository: {}",
            path.display()
        );
        selected.insert(selected_path);
    }
    let mut worktrees = Vec::new();
    let mut scanned = 0usize;
    let mut missing = 0usize;
    let mut failed = 0usize;
    let mut skipped = 0usize;
    for registration in registrations {
        let path = &registration["worktree"];
        let root = Path::new(path);
        let exists = root.try_exists();
        let selected = options.all_worktrees || selected.contains(&normalized(root)?);
        let mut value = if let Err(error) = &exists {
            failed += 1;
            json!({"path":path,"kind":"inspection_unavailable","inspection_status":"failed","blockers":[format!("worktree metadata unavailable: {error}")]})
        } else if !exists.unwrap_or(false) {
            missing += 1;
            json!({"path":path,"kind":"missing_worktree_registration","inspection_status":"missing","metadata_prune_candidate":registration.contains_key("prunable") && !registration.contains_key("locked"),"cleanup_authorized":false,"blockers":["Worktree directory is absent. Any prune candidate concerns registration metadata only; retained work, branch references, locks and recovery dependencies require separate review."]})
        } else if !selected {
            skipped += 1;
            json!({"path":path,"kind":"registered_worktree","inspection_status":"not_scanned","notes":["Registration listed only. Select --worktree or --all-worktrees for a deep inspection."]})
        } else {
            eprintln!(
                "Timber: inspecting selected worktree {}...",
                scanned + failed + 1
            );
            match worktree(root) {
                Ok(mut v) => {
                    scanned += 1;
                    if !options.list_ignored {
                        if let Some(rows) = v["ignored"].as_array() {
                            v["ignored"] = ignored_summary(rows);
                        }
                    }
                    if v["inspection_status"] == "partial" {
                        failed += 1;
                    }
                    v
                }
                Err(e) => {
                    failed += 1;
                    json!({"path":path,"kind":"inspection_unavailable","inspection_status":"failed","blockers":[format!("{e:#}")]})
                }
            }
        };
        value["registration"] = json!(registration);
        worktrees.push(value);
    }
    let mut preserved = Vec::new();
    let mut seen_folders = BTreeSet::new();
    for folder in folders {
        eprintln!(
            "Timber: inspecting recovery folder {}...",
            preserved.len() + 1
        );
        let folder = match fs::canonicalize(folder) {
            Ok(folder) => folder,
            Err(error) => {
                failed += 1;
                preserved.push(json!({"path":folder,"kind":"folder_inspection_unavailable","inspection_status":"failed","blockers":[format!("folder metadata unavailable: {error}")]}));
                continue;
            }
        };
        if !seen_folders.insert(folder.clone()) {
            continue;
        }
        match crate::recovery::analyze_folder(&target_root, &folder, options.compare_folders) {
            Ok(mut value) => {
                let partial = folder_partial(&value);
                value["inspection_status"] = json!(if partial { "partial" } else { "scanned" });
                if partial {
                    failed += 1;
                }
                preserved.push(value);
            }
            Err(error) => {
                failed += 1;
                preserved.push(json!({"path":folder,"kind":"folder_inspection_unavailable","inspection_status":"failed","blockers":[format!("{error:#}")]}));
            }
        }
    }
    let registration_stable = data == git::run(&repo, &["worktree", "list", "--porcelain", "-z"])?;
    if !registration_stable {
        failed += 1;
    }
    let prune_candidates = worktrees
        .iter()
        .filter(|row| row["metadata_prune_candidate"] == true)
        .count();
    let consolidation_summary = consolidation_summary(&preserved);
    eprintln!(
        "Timber: inspection finished; {scanned} worktrees scanned, {skipped} listed only, {missing} missing registrations."
    );
    let ignored_note = if options.list_ignored {
        "Ignored files are listed from metadata (--list-ignored)."
    } else {
        "Ignored files are summarized from metadata: count, bytes and the top-level paths with the most entries. Use --list-ignored for every path."
    };
    Ok(json!({
        "schema":INSPECT_SCHEMA,"repo":repo,"target_worktree":target_root,"status":if failed==0 {"complete"}else{"partial"},"worktrees":worktrees,"folders":preserved,
        "scope":{"mode":if options.all_worktrees {"all_worktrees"}else{"selected_worktrees"},"registered_worktrees":worktrees.len(),"deep_scanned_worktrees":scanned,"listed_only_worktrees":skipped,"missing_registrations":missing,"metadata_prune_candidates":prune_candidates,"failed_or_partial_scans":failed,"compare_folder_contents":options.compare_folders,"list_ignored":options.list_ignored},
        "registration_stable":registration_stable,"elapsed_seconds":started.elapsed().as_secs_f64(),"consolidation_summary":consolidation_summary,
        "notes":["Inspection never prunes, moves, stages, commits, or deletes source work.","Default scope deeply inspects the requested worktree only; other registrations are listed without scanning their files.",ignored_note,"Recovery-folder payload comparison requires --compare-folders.","Changed registrations or partial scans require review; no result authorizes cleanup."]
    }))
}

fn folder_partial(folder: &Value) -> bool {
    folder
        .get("blockers")
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty())
        || [
            "unsupported",
            "changed_during_read",
            "receipt_hash_mismatch",
            "saved_file_missing",
        ]
        .iter()
        .any(|key| folder["summary"][key].as_u64().unwrap_or(0) > 0)
        || folder
            .get("git_dependencies")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|d| {
                    matches!(
                        d["status"].as_str(),
                        Some(
                            "unsupported"
                                | "hash_mismatch"
                                | "ref_mismatch"
                                | "reference_missing"
                                | "missing"
                        )
                    )
                })
            })
}

/// Exact observed relationships only; counts can overlap and never grant cleanup authority.
pub fn consolidation_summary(folders: &[Value]) -> Value {
    let mut totals = BTreeMap::<String, u64>::new();
    let mut targets = BTreeSet::new();
    let mut duplicates = BTreeMap::<(String, String, u64, String, bool), Vec<Value>>::new();
    let mut tracked = BTreeMap::<String, u64>::new();
    for folder in folders {
        for key in [
            "payload_files",
            "same_path_identical",
            "same_path_different",
            "absent_at_proposed_path",
            "receipt_hash_mismatch",
            "saved_file_missing",
            "unmapped",
            "unsupported",
            "changed_during_read",
        ] {
            *totals.entry(key.into()).or_default() += folder["summary"][key].as_u64().unwrap_or(0);
        }
        if let Some(rows) = folder.get("payload_rows").and_then(Value::as_array) {
            for row in rows {
                let Some(target) = row["target_path"].as_str() else {
                    continue;
                };
                targets.insert(target.to_owned());
                let (Some(sha), Some(bytes), Some(kind), Some(executable)) = (
                    row["sha256"].as_str(),
                    row["bytes"].as_u64(),
                    row["kind"].as_str(),
                    row["executable"].as_bool(),
                ) else {
                    continue;
                };
                duplicates
                    .entry((target.into(), sha.into(), bytes, kind.into(), executable))
                    .or_default()
                    .push(json!({"folder":folder["path"],"stored_path":row["stored_path"]}));
            }
        }
        if let Some(dependencies) = folder.get("git_dependencies").and_then(Value::as_array) {
            for dependency in dependencies {
                if let Some(rows) = dependency.get("tracked_rows").and_then(Value::as_array) {
                    for row in rows {
                        if let Some(path) = row["path"].as_str() {
                            targets.insert(path.into());
                        }
                        *tracked
                            .entry(row["comparison"].as_str().unwrap_or("unverified").into())
                            .or_default() += 1;
                    }
                }
            }
        }
    }
    let mut groups = Vec::new();
    let mut extra = 0usize;
    for ((target, sha, bytes, kind, executable), members) in duplicates {
        if members.len() > 1 {
            extra += members.len() - 1;
            groups.push(json!({"target_path":target,"sha256":sha,"bytes":bytes,"kind":kind,"executable":executable,"members":members}));
        }
    }
    json!({"folders_analyzed":folders.len(),"content_comparisons_requested":folders.iter().any(|f|f["comparison_requested"]==true),"payload":totals,"tracked_git_versions":tracked.values().sum::<u64>(),"tracked_git_comparisons":tracked,"unique_proposed_target_paths":targets.len(),"exact_duplicate_saved_group_count":groups.len(),"exact_duplicate_saved_groups":groups,"duplicate_extra_copies":extra,"cleanup_authorized":false,"notes":["Matching the current target and matching another saved copy can describe the same file; do not add those counts as independent cleanup candidates.","Duplicate groups require identical proposed target path, content digest, byte length, type and executable state. They do not establish equivalent behavior or permission to delete.","Absent means absent at the proposed path only; renamed or otherwise represented work may exist elsewhere."]})
}
