//! Capture supported state without staging, committing or cleaning the source.
use crate::{
    fsx, git,
    model::*,
    store::{Store, manifest_id, valid_oid, validate_import_map},
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

fn started(store: &Store, source: &Path, kind: &str) -> Result<PathBuf> {
    let mut marker = tempfile::Builder::new()
        .prefix("capture-")
        .suffix(".json")
        .tempfile_in(store.root.join("incomplete"))?;
    serde_json::to_writer_pretty(
        &mut marker,
        &serde_json::json!({"schema":SCHEMA,"operation":kind,"source":source,"complete":false,"started_unix":now()?}),
    )?;
    marker.write_all(b"\n")?;
    marker.as_file().sync_all()?;
    let (_, path) = marker.keep()?;
    File::open(store.root.join("incomplete"))?.sync_all()?;
    Ok(path)
}

fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
fn finish(store: &Store, marker: &Path, manifest: &Manifest) -> Result<()> {
    store.publish(manifest)?;
    fs::remove_file(marker)?;
    File::open(store.root.join("incomplete"))?.sync_all()?;
    Ok(())
}
fn nul_strings(bytes: &[u8]) -> Result<Vec<String>> {
    bytes
        .split(|b| *b == 0)
        .filter(|b| !b.is_empty())
        .map(|v| String::from_utf8(v.to_vec()).context("non UTF-8 paths are unsupported in v0.1"))
        .collect()
}

/// Checks supported Git states and returns logical stage-zero index entries.
pub fn read_index(path: &Path) -> Result<Vec<(String, String, String)>> {
    let raw = git::run(path, &["ls-files", "--stage", "-z"])?;
    let mut entries = Vec::new();
    for record in raw.split(|b| *b == 0).filter(|b| !b.is_empty()) {
        let tab = record
            .iter()
            .position(|b| *b == b'\t')
            .context("malformed index entry")?;
        let header = std::str::from_utf8(&record[..tab])?;
        let fields: Vec<_> = header.split(' ').collect();
        ensure!(
            fields.len() == 3 && fields[2] == "0",
            "unresolved index stages are unsupported"
        );
        ensure!(
            ["100644", "100755", "120000"].contains(&fields[0]),
            "unsupported index mode (including submodules): {}",
            fields[0]
        );
        ensure!(
            valid_oid(fields[1]),
            "unsupported or missing Git object identity"
        );
        let name = String::from_utf8(record[tab + 1..].to_vec())
            .context("non UTF-8 index paths are unsupported")?;
        fsx::validate_relative(&name)?;
        entries.push((name, fields[0].to_owned(), fields[1].to_owned()));
    }
    // Extended flags cannot be reproduced by the v1 logical-index contract.
    let debug = git::run(path, &["ls-files", "--debug", "-z"])?;
    // Parse only the metadata after each NUL-delimited filename. A malicious filename
    // containing 'flags:' cannot masquerade as metadata.
    let mut rest = debug.as_slice();
    while !rest.is_empty() {
        let end = rest
            .iter()
            .position(|b| *b == 0)
            .context("malformed index debug record")?;
        rest = &rest[end + 1..];
        let needle = b"  size: ";
        let start = rest
            .windows(needle.len())
            .position(|w| w == needle)
            .context("missing index size metadata")?;
        let finish = rest[start..]
            .iter()
            .position(|b| *b == b'\n')
            .context("truncated index metadata")?
            + start;
        let line = std::str::from_utf8(&rest[start..finish])?;
        let flags = line
            .split("flags: ")
            .nth(1)
            .context("missing index flags")?
            .trim();
        ensure!(
            flags == "0",
            "unsupported index flags: {flags}; assume-unchanged, skip-worktree and intent-to-add must be resolved first"
        );
        rest = &rest[finish + 1..];
    }
    entries.sort();
    Ok(entries)
}

pub fn validate_worktree(path: &Path) -> Result<()> {
    let root = PathBuf::from(git::text(path, &["rev-parse", "--show-toplevel"])?);
    ensure!(
        root.canonicalize()? == path.canonicalize()?,
        "checkpoint must select the worktree root"
    );
    ensure!(
        git::text(path, &["rev-parse", "--is-bare-repository"])? == "false",
        "bare repositories are unsupported"
    );
    ensure!(
        git::text(path, &["rev-parse", "--show-object-format"])? == "sha1",
        "v0.1 supports SHA-1 Git repositories only"
    );
    ensure!(
        git::text(path, &["rev-parse", "--is-shallow-repository"])? == "false",
        "shallow history is unsupported"
    );
    ensure!(
        git::text(path, &["config", "--bool", "--get", "core.sparseCheckout"]).unwrap_or_default()
            != "true",
        "sparse checkouts are unsupported"
    );
    ensure!(
        git::text(path, &["config", "--get", "extensions.partialClone"])
            .unwrap_or_default()
            .is_empty(),
        "partial clones are unsupported"
    );
    ensure!(
        git::text(path, &["config", "--get-regexp", "remote\\..*\\.promisor"])
            .unwrap_or_default()
            .is_empty(),
        "promisor repositories are unsupported"
    );
    for name in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-merge",
        "rebase-apply",
        "BISECT_LOG",
        "sequencer",
        "index.lock",
    ] {
        let value = PathBuf::from(git::text(path, &["rev-parse", "--git-path", name])?);
        let state = if value.is_absolute() {
            value
        } else {
            path.join(value)
        };
        ensure!(
            !state.try_exists()?,
            "unsupported or active Git operation: {name}"
        );
    }
    read_index(path)?;
    Ok(())
}

fn ignored_paths(path: &Path) -> Result<BTreeSet<String>> {
    // Git collapses an ignored nested repository into one `dir/` record. Remove only
    // that terminal marker so the recorded omission is a canonical relative path.
    Ok(nul_strings(&git::run(
        path,
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "-z",
        ],
    )?)?
    .into_iter()
    .map(|p| p.strip_suffix('/').map(str::to_owned).unwrap_or(p))
    .collect())
}
fn included(path: &str, selectors: &[String]) -> bool {
    selectors.iter().any(|selector| {
        path == selector
            || path
                .strip_prefix(selector)
                .is_some_and(|suffix| suffix.starts_with('/'))
    })
}
/// True when a tracked path's current working bytes, type or executable bit differ
/// from its index entry, or cannot be read to prove otherwise.
fn working_differs(
    source: &Path,
    path: &str,
    mode: &str,
    expected: Option<&(String, u64)>,
) -> bool {
    let Ok(metadata) = fs::symlink_metadata(source.join(path)) else {
        return true;
    };
    let symlink = metadata.file_type().is_symlink();
    if !(symlink || metadata.is_file()) || fsx::is_placeholder(&metadata) {
        return true;
    }
    let executable = std::os::unix::fs::PermissionsExt::mode(&metadata.permissions()) & 0o111 != 0;
    if (mode == "120000") != symlink
        || (mode == "100755" && !executable)
        || (mode == "100644" && executable)
    {
        return true;
    }
    let entry = Entry {
        path: path.to_owned(),
        kind: if symlink {
            EntryKind::Symlink
        } else {
            EntryKind::File
        },
        mode: 0,
        size: 0,
        sha256: String::new(),
    };
    fsx::fingerprint(source, &entry)
        .map(|observed| Some(&observed) != expected)
        .unwrap_or(true)
}

/// Validates every `--exclude` before the store is touched. An exclusion must name an
/// existing path, reached without following a symbolic link, that holds no tracked
/// path. Tracked paths are refused even when clean: their history and index are
/// captured anyway, and a missing working copy would read as a deletion in previews.
fn operator_exclusions(
    source: &Path,
    exclude: &[String],
    include_ignored: &[String],
) -> Result<Vec<String>> {
    let mut selectors: Vec<String> = Vec::new();
    for raw in exclude {
        let selector = raw.trim_end_matches('/').to_owned();
        fsx::validate_relative(&selector)
            .with_context(|| format!("invalid --exclude path: {raw:?}"))?;
        for earlier in &selectors {
            ensure!(
                !included(&selector, std::slice::from_ref(earlier))
                    && !included(earlier, std::slice::from_ref(&selector)),
                "--exclude paths repeat or overlap: {earlier} and {selector}"
            );
        }
        for chosen in include_ignored {
            let chosen = chosen.trim_end_matches('/').to_owned();
            ensure!(
                !included(&chosen, std::slice::from_ref(&selector))
                    && !included(&selector, std::slice::from_ref(&chosen)),
                "path is both excluded and selected with --include-ignored: {selector} and {chosen}"
            );
        }
        selectors.push(selector);
    }
    if selectors.is_empty() {
        return Ok(selectors);
    }
    let index = crate::inspect::index(source)?;
    let head = crate::inspect::head_tree(source)?;
    for selector in &selectors {
        let scope = std::slice::from_ref(selector);
        let tracked = index
            .keys()
            .chain(head.keys())
            .filter(|p| included(p, scope))
            .collect::<BTreeSet<_>>();
        if let Some(first) = tracked.first() {
            let oids = tracked
                .iter()
                .filter_map(|p| index.get(*p))
                .filter(|(mode, _)| mode != "160000")
                .map(|(_, oid)| oid.clone())
                .collect::<Vec<_>>();
            let fingerprints = git::batch_blob_fingerprints(source, &oids)?;
            let changed = tracked.iter().find(|p| match index.get(**p) {
                None => true,
                Some(entry) if head.get(**p) != Some(entry) => true,
                Some((mode, oid)) => working_differs(source, p, mode, fingerprints.get(oid)),
            });
            match changed {
                Some(path) => bail!(
                    "--exclude refused for tracked path with staged or working changes: {path} (named by --exclude {selector})"
                ),
                None => bail!(
                    "--exclude refused for tracked path: {first} (named by --exclude {selector}); exclusion applies only to untracked or ignored paths"
                ),
            }
        }
        let mut current = source.to_path_buf();
        let parts = selector.split('/').collect::<Vec<_>>();
        for (position, part) in parts.iter().enumerate() {
            current.push(part);
            let exists = match fs::symlink_metadata(&current) {
                Ok(m) => position + 1 == parts.len() || (m.is_dir() && !m.file_type().is_symlink()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("--exclude path unavailable: {selector}"));
                }
            };
            ensure!(
                exists,
                "--exclude matches no path in the worktree: {selector}"
            );
        }
    }
    Ok(selectors)
}

fn persist_entries(store: &Store, source: &Path, entries: &[Entry]) -> Result<()> {
    for entry in entries {
        if entry.kind != EntryKind::Directory {
            let bytes = fsx::read_entry(source, entry)?;
            ensure!(
                bytes.len() as u64 == entry.size && fsx::digest(&bytes) == entry.sha256,
                "source changed while reading {}",
                entry.path
            );
            ensure!(
                store.put(&bytes)? == entry.sha256,
                "content identity mismatch"
            );
        }
    }
    Ok(())
}

pub fn checkpoint(
    store: &Store,
    path: &Path,
    include_ignored: &[String],
    writers_paused: bool,
) -> Result<Manifest> {
    checkpoint_with_exclusions(store, path, include_ignored, &[], writers_paused)
}

/// Capture with explicit per-run operator exclusions (`checkpoint --exclude`). Each
/// excluded path is skipped entirely and recorded as `excluded_by_operator`. Without
/// exclusions this is exactly [`checkpoint`]: a nested repository still refuses.
pub fn checkpoint_with_exclusions(
    store: &Store,
    path: &Path,
    include_ignored: &[String],
    exclude: &[String],
    writers_paused: bool,
) -> Result<Manifest> {
    let source = path.canonicalize()?;
    fsx::ensure_outside(&store.root, &source)?;
    validate_worktree(&source)?;
    let operator = operator_exclusions(&source, exclude, include_ignored)?;
    let pruned = std::cell::RefCell::new(BTreeSet::new());
    let prune = |p: &str| {
        let hit = operator.iter().find(|x| x.as_str() == p);
        if let Some(x) = hit {
            pruned.borrow_mut().insert(x.clone());
        }
        hit.is_some()
    };
    let protected_roots = git::protected_roots(&source)?;
    for root in &protected_roots {
        fsx::ensure_outside(&store.root, root)?;
    }
    let mut selectors = Vec::new();
    for selector in include_ignored {
        let selector = selector.trim_end_matches('/').to_owned();
        fsx::validate_relative(&selector)?;
        selectors.push(selector);
    }
    let marker = started(store, &source, "checkpoint")?;
    let ignored = ignored_paths(&source)?;
    for selector in &selectors {
        ensure!(
            ignored
                .iter()
                .any(|p| included(p, std::slice::from_ref(selector))),
            "ignored selector matches no ignored files: {selector}"
        );
    }
    let exclude = |p: &str| ignored.contains(p) && !included(p, &selectors);
    let entries = fsx::inventory_pruned(&source, prune, |p| !exclude(p))?;
    for selector in &operator {
        ensure!(
            pruned.borrow().contains(selector),
            "--exclude matched nothing during capture: {selector}"
        );
    }
    let head = git::text(&source, &["rev-parse", "--verify", "HEAD"])?;
    let branch = git::text(&source, &["symbolic-ref", "-q", "HEAD"]).ok();
    let index_before = read_index(&source)?;
    let object_ids = index_before
        .iter()
        .map(|(_, _, oid)| oid.clone())
        .collect::<Vec<_>>();
    let mut staged_objects = BTreeMap::new();
    git::for_each_blob(&source, &object_ids, |oid, bytes| {
        staged_objects.insert(oid.to_owned(), store.put(&bytes)?);
        Ok(())
    })?;
    let mut index = Vec::new();
    for (path, mode, oid) in &index_before {
        index.push(IndexEntry {
            path: path.clone(),
            mode: mode.clone(),
            oid: oid.clone(),
            blob: staged_objects
                .get(oid)
                .context("staged Git blob was not captured")?
                .clone(),
        });
    }
    persist_entries(store, &source, &entries)?;
    let staging = tempfile::Builder::new()
        .prefix("bundle-")
        .tempdir_in(store.root.join("incomplete"))?;
    let bundle_path = staging.path().join("history.bundle");
    let bundle_str = bundle_path.to_str().context("bundle path must be UTF-8")?;
    git::run(&source, &["bundle", "create", bundle_str, "HEAD"])?;
    let bundle = store.put(&fs::read(&bundle_path)?)?;
    ensure!(
        entries == fsx::inventory_pruned(&source, prune, |p| !exclude(p))?,
        "working files changed during capture; checkpoint remains incomplete"
    );
    ensure!(
        index_before == read_index(&source)?,
        "index changed during capture"
    );
    ensure!(
        head == git::text(&source, &["rev-parse", "--verify", "HEAD"])?
            && branch == git::text(&source, &["symbolic-ref", "-q", "HEAD"]).ok(),
        "HEAD identity changed during capture"
    );
    ensure!(
        ignored == ignored_paths(&source)?,
        "ignored path coverage changed during capture"
    );
    validate_worktree(&source)?;
    ensure!(
        protected_roots == git::protected_roots(&source)?,
        "registered worktree or shared Git roots changed during capture"
    );
    // Persist canonical existing roots so an unavailable original does not require
    // filesystem resolution merely to validate the checkpoint later.
    let mut protected_roots = protected_roots
        .iter()
        .map(|path| {
            let path = path.canonicalize().unwrap_or_else(|_| path.clone());
            path.to_str()
                .context("non UTF-8 protected root")
                .map(str::to_owned)
        })
        .collect::<Result<Vec<_>>>()?;
    protected_roots.sort();
    protected_roots.dedup();
    // Operator exclusions come first. Ignored omissions keep their own records, even
    // below an excluded path, so later assessment compares like with like.
    let excluded = operator
        .iter()
        .map(|path| Exclusion {
            path: path.clone(),
            reason: EXCLUDED_BY_OPERATOR.into(),
        })
        .chain(
            ignored
                .into_iter()
                .filter(|p| !included(p, &selectors))
                .map(|path| Exclusion {
                    path,
                    reason: "ignored file not explicitly selected".into(),
                }),
        )
        .collect();
    let mut manifest = Manifest {
        schema: SCHEMA,
        id: String::new(),
        source_kind: "worktree".into(),
        source_path: source.to_str().context("non UTF-8 source root")?.into(),
        created_unix: now()?,
        entries,
        git: Some(GitState {
            head,
            branch,
            object_format: "sha1".into(),
            index,
            bundle,
            protected_roots,
        }),
        excluded,
        capture: CaptureConditions {
            scans_matched: true,
            writers_paused_declared: writers_paused,
            writer_exclusion_enforced: false,
        },
        import_map: None,
        captured_dependencies: Vec::new(),
    };
    manifest.id = manifest_id(&manifest)?;
    finish(store, &marker, &manifest)?;
    Ok(manifest)
}

pub fn import_folder(
    store: &Store,
    path: &Path,
    map: Option<ImportMap>,
    writers_paused: bool,
) -> Result<Manifest> {
    import_folder_with_dependencies(store, path, map, writers_paused, None)
}

pub fn import_folder_with_dependencies(
    store: &Store,
    path: &Path,
    map: Option<ImportMap>,
    writers_paused: bool,
    dependency_repo: Option<&Path>,
) -> Result<Manifest> {
    let source = path.canonicalize()?;
    fsx::ensure_outside(&store.root, &source)?;
    ensure!(
        !source.join(".git").try_exists()?,
        "folder is a Git repository; use checkpoint instead"
    );
    if let Some(map) = &map {
        validate_import_map(map)?;
    }
    // Preflight every dependency source before writing even an incomplete marker.
    if let Some(repo) = dependency_repo {
        for root in git::protected_roots(repo)? {
            fsx::ensure_outside(&store.root, &root)?;
        }
    }
    let marker = started(store, &source, "folder_import")?;
    let captured_dependencies = if let Some(repo) = dependency_repo {
        let map = map
            .as_ref()
            .context("dependency capture requires an explicit import map")?;
        ensure!(
            !map.git_dependencies.is_empty(),
            "import map has no Git dependencies to capture"
        );
        crate::dependencies::capture(store, repo, &map.git_dependencies)?
    } else {
        Vec::new()
    };
    let entries = fsx::inventory(&source)?;
    if let Some(map) = &map {
        for file in &map.files {
            ensure!(
                entries
                    .iter()
                    .any(|e| e.path == file.stored_path && e.kind != EntryKind::Directory),
                "mapped source file is missing: {}",
                file.stored_path
            );
        }
    }
    persist_entries(store, &source, &entries)?;
    ensure!(
        entries == fsx::inventory(&source)?,
        "folder changed during import"
    );
    let mut manifest = Manifest {
        schema: SCHEMA,
        id: String::new(),
        source_kind: "folder".into(),
        source_path: source.to_str().context("non UTF-8 source root")?.into(),
        created_unix: now()?,
        entries,
        git: None,
        excluded: Vec::new(),
        capture: CaptureConditions {
            scans_matched: true,
            writers_paused_declared: writers_paused,
            writer_exclusion_enforced: false,
        },
        import_map: map,
        captured_dependencies,
    };
    manifest.id = manifest_id(&manifest)?;
    finish(store, &marker, &manifest)?;
    Ok(manifest)
}
