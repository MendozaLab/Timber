//! Capture supported state without staging, committing or cleaning the source.
use crate::{
    fsx, git,
    model::*,
    store::{
        JOURNAL_FORMAT, Journal, Store, StoreLock, manifest_id, valid_oid, validate_import_map,
    },
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

/// A capture's incomplete marker in `store/incomplete/`. It stays until the
/// completed manifest is published; a refused run leaves it, with its journal of
/// the objects that run added, for `timber gc --dry-run` to report.
struct Marker {
    path: PathBuf,
    id: String,
    journal: Arc<Journal>,
    _lock: StoreLock,
}

impl Marker {
    /// Names the retained marker, the bytes it holds and the reclamation review
    /// command. Nothing is deleted here or anywhere else automatically.
    fn refused(&self, error: anyhow::Error) -> anyhow::Error {
        let (objects, bytes) = self.journal.totals();
        error.context(format!(
            "capture refused; incomplete marker {} is kept in the store and holds {bytes} bytes in {objects} objects this run added; review what can be reclaimed with `timber gc --dry-run` (Timber never deletes automatically)",
            self.id
        ))
    }

    fn finish(self, store: &Store) -> Result<()> {
        fs::remove_file(&self.path)?;
        File::open(store.root.join("incomplete"))?.sync_all()?;
        Ok(())
    }
}

fn started(store: &Store, source: &Path, kind: &str) -> Result<Marker> {
    let lock = store.capture_lock()?;
    let mut marker = tempfile::Builder::new()
        .prefix(crate::gc::MARKER_PREFIX)
        .suffix(crate::gc::MARKER_SUFFIX)
        .tempfile_in(store.root.join("incomplete"))?;
    // Held for the whole run: `gc --dry-run` reports a locked marker as in progress.
    ensure!(
        crate::store::try_flock(marker.as_file(), true)?,
        "new incomplete marker is already locked"
    );
    serde_json::to_writer_pretty(
        &mut marker,
        &serde_json::json!({"schema":SCHEMA,"operation":kind,"source":source,"complete":false,"started_unix":now()?,"journal":JOURNAL_FORMAT}),
    )?;
    marker.write_all(b"\n")?;
    marker.as_file().sync_all()?;
    let (file, path) = marker.keep()?;
    File::open(store.root.join("incomplete"))?.sync_all()?;
    let id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .context("non UTF-8 incomplete marker name")?
        .to_owned();
    Ok(Marker {
        path,
        id,
        journal: Arc::new(Journal::new(file)),
        _lock: lock,
    })
}

fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
fn nul_strings(bytes: &[u8]) -> Result<Vec<String>> {
    bytes
        .split(|b| *b == 0)
        .filter(|b| !b.is_empty())
        .map(|v| String::from_utf8(v.to_vec()).context("non UTF-8 paths are unsupported in v0.1"))
        .collect()
}

/// Index mode of a gitlink (a submodule pointer).
pub const GITLINK_MODE: &str = "160000";

/// Checks supported Git states and returns logical stage-zero index entries,
/// including gitlinks (mode 160000). Readability of each gitlink's submodule
/// directory is checked separately by [`submodule_records`].
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
            ["100644", "100755", "120000", GITLINK_MODE].contains(&fields[0]),
            "unsupported index mode: {}",
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

/// Records one gitlink without reading the submodule's files. The submodule path
/// must be a real directory that is either empty (not initialized; only the index
/// commit id is recorded) or holds a `.git` whose HEAD resolves inside that same
/// directory. Anything else is refused, naming the path: a missing path, a file or
/// symbolic link, a non-empty directory without `.git`, or an unreadable HEAD.
pub fn submodule_record(source: &Path, path: &str, gitlink: &str) -> Result<SubmoduleRecord> {
    fsx::validate_relative(path)?;
    ensure!(
        valid_oid(gitlink),
        "submodule gitlink commit id cannot be read: {path}"
    );
    let directory = source.join(path);
    let metadata = fs::symlink_metadata(&directory).with_context(|| {
        format!("submodule is not initialized or not readable (path missing): {path}")
    })?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink() && !fsx::is_placeholder(&metadata),
        "submodule path is not a real directory: {path}"
    );
    let dot_git = directory.join(".git");
    let initialized = match fs::symlink_metadata(&dot_git) {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => {
            return Err(e).with_context(|| format!("submodule metadata unreadable: {path}"));
        }
    };
    if !initialized {
        // Read at most one name: emptiness is all that is needed, and nothing is opened.
        let empty = fs::read_dir(&directory)
            .with_context(|| format!("submodule directory unreadable: {path}"))?
            .next()
            .is_none();
        ensure!(
            empty,
            "submodule is not initialized but its directory is not empty: {path}; its files cannot be recorded as a pointer"
        );
        return Ok(SubmoduleRecord {
            path: path.to_owned(),
            gitlink_commit: gitlink.to_owned(),
            checked_out_commit: None,
        });
    }
    let unreadable =
        |error: anyhow::Error| error.context(format!("submodule HEAD is not readable: {path}"));
    let toplevel = git::text(&directory, &["rev-parse", "--show-toplevel"]).map_err(unreadable)?;
    // An invalid `.git` directory lets Git walk up to the superproject; refuse that.
    ensure!(
        Path::new(&toplevel).canonicalize().ok() == directory.canonicalize().ok(),
        "submodule HEAD is not readable: {path}; Git resolved its repository elsewhere"
    );
    let head =
        git::text(&directory, &["rev-parse", "--verify", "HEAD^{commit}"]).map_err(unreadable)?;
    ensure!(
        valid_oid(&head),
        "submodule HEAD is not readable: {path}; unsupported commit identity"
    );
    Ok(SubmoduleRecord {
        path: path.to_owned(),
        gitlink_commit: gitlink.to_owned(),
        checked_out_commit: Some(head),
    })
}

/// Records every gitlink in the given index, refusing an unreadable one by path.
pub fn submodule_records(
    source: &Path,
    index: &[(String, String, String)],
) -> Result<Vec<SubmoduleRecord>> {
    index
        .iter()
        .filter(|(_, mode, _)| mode == GITLINK_MODE)
        .map(|(path, _, oid)| submodule_record(source, path, oid))
        .collect()
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
    let index = read_index(path)?;
    submodule_records(path, &index)?;
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

/// How an exclusion was declared: its manifest reason and the name used in messages.
fn exclusion_label(reason: &str) -> &'static str {
    if reason == EXCLUDED_BY_CONVENTION_FILE {
        "timber.toml exclude"
    } else {
        "--exclude"
    }
}

/// Validates every declared exclusion (command-line `--exclude` and the repository's
/// `timber.toml` `exclude` list) before the store is touched. Both are held to the
/// same rules: an exclusion must name an existing path, reached without following a
/// symbolic link, that holds no tracked path. Tracked paths are refused even when
/// clean: their history and index are captured anyway, and a missing working copy
/// would read as a deletion in previews. Each refusal names the path and its origin.
fn declared_exclusions(
    source: &Path,
    requests: &[(String, &'static str)],
    include_ignored: &[String],
) -> Result<Vec<(String, &'static str)>> {
    let mut selectors: Vec<(String, &'static str)> = Vec::new();
    for (raw, reason) in requests {
        let label = exclusion_label(reason);
        let selector = raw.trim_end_matches('/').to_owned();
        fsx::validate_relative(&selector)
            .with_context(|| format!("invalid {label} path: {raw:?}"))?;
        for (earlier, earlier_reason) in &selectors {
            let overlap = included(&selector, std::slice::from_ref(earlier))
                || included(earlier, std::slice::from_ref(&selector));
            if earlier_reason == reason {
                ensure!(
                    !overlap,
                    "{label} paths repeat or overlap: {earlier} and {selector}"
                );
            } else {
                ensure!(
                    !overlap,
                    "exclusions repeat or overlap: {earlier} ({}) and {selector} ({label})",
                    exclusion_label(earlier_reason)
                );
            }
        }
        for chosen in include_ignored {
            let chosen = chosen.trim_end_matches('/').to_owned();
            ensure!(
                !included(&chosen, std::slice::from_ref(&selector))
                    && !included(&selector, std::slice::from_ref(&chosen)),
                "path is both excluded and selected with --include-ignored: {selector} and {chosen}"
            );
        }
        selectors.push((selector, *reason));
    }
    if selectors.is_empty() {
        return Ok(selectors);
    }
    let index = crate::inspect::index(source)?;
    let head = crate::inspect::head_tree(source)?;
    for (selector, reason) in &selectors {
        let label = exclusion_label(reason);
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
                    "{label} refused for tracked path with staged or working changes: {path} (named by {label} {selector})"
                ),
                None => bail!(
                    "{label} refused for tracked path: {first} (named by {label} {selector}); exclusion applies only to untracked or ignored paths"
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
                    return Err(e).with_context(|| format!("{label} path unavailable: {selector}"));
                }
            };
            ensure!(
                exists,
                "{label} matches no path in the worktree: {selector}"
            );
        }
    }
    Ok(selectors)
}

/// Linked worktrees of this repository below `source`, by relative path, that
/// checkpoint skips automatically. The walk does not enter declared exclusions
/// (operator or convention file) or gitlink paths. A linked worktree whose folder
/// holds a tracked path of this worktree is refused, naming it, because a missing
/// tracked working copy would read as a deletion; a declared exclusion strictly
/// inside a linked worktree is refused because that folder is skipped as a whole. Foreign nested repositories are not
/// returned; the capture walk refuses them.
fn linked_worktrees_inside(
    source: &Path,
    declared: &[(String, &'static str)],
    gitlinks: &BTreeSet<String>,
) -> Result<BTreeMap<String, String>> {
    let skipped = |p: &str| gitlinks.contains(p) || declared.iter().any(|(x, _)| x == p);
    let candidates = fsx::nested_repositories(source)?;
    if candidates.iter().all(|p| skipped(p)) {
        return Ok(BTreeMap::new());
    }
    let common = git::common_dir(source)?;
    let linked = fsx::classify_nested_repositories(source, &common, skipped)?
        .into_iter()
        .filter(|r| !gitlinks.contains(&r.path))
        .filter_map(|r| r.linked_worktree.map(|name| (r.path, name)))
        .collect::<BTreeMap<_, _>>();
    if linked.is_empty() {
        return Ok(linked);
    }
    let index = crate::inspect::index(source)?;
    let head = crate::inspect::head_tree(source)?;
    for (path, name) in &linked {
        let scope = std::slice::from_ref(path);
        if let Some(tracked) = index.keys().chain(head.keys()).find(|p| included(p, scope)) {
            bail!(
                "linked worktree {path} (registration {name}) holds tracked path {tracked}; checkpoint cannot skip it"
            );
        }
        if let Some((inner, reason)) = declared
            .iter()
            .find(|(x, _)| x.as_str() != path && included(x, scope))
        {
            bail!(
                "{} {inner} lies inside linked worktree {path} (registration {name}), which checkpoint skips as a whole; name the folder or nothing",
                exclusion_label(reason)
            );
        }
    }
    Ok(linked)
}

fn persist_entries(store: &Store, source: &Path, entries: &[Entry]) -> Result<()> {
    for entry in entries {
        if matches!(entry.kind, EntryKind::File | EntryKind::Symlink) {
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
/// excluded path is skipped entirely and recorded as `excluded_by_operator`. A linked
/// worktree of this same repository below the worktree is skipped automatically and
/// recorded as `linked_worktree_captured_separately`. Without exclusions this is
/// exactly [`checkpoint`]: a foreign nested repository still refuses.
pub fn checkpoint_with_exclusions(
    store: &Store,
    path: &Path,
    include_ignored: &[String],
    exclude: &[String],
    writers_paused: bool,
) -> Result<Manifest> {
    checkpoint_with_convention(store, path, include_ignored, exclude, &[], writers_paused)
}

/// Capture with operator exclusions and exclusions from the repository's convention
/// file. Convention exclusions are applied exactly as if passed with `--exclude`,
/// under the same refusals, and recorded as `excluded_by_convention_file`. Callers
/// resolve command-line overrides first (see `convention::merge_exclusions`); any
/// remaining overlap between the two lists is refused, naming both origins. Linked
/// worktrees are skipped and recorded as in [`checkpoint_with_exclusions`].
pub fn checkpoint_with_convention(
    store: &Store,
    path: &Path,
    include_ignored: &[String],
    exclude: &[String],
    convention_exclude: &[String],
    writers_paused: bool,
) -> Result<Manifest> {
    let source = path.canonicalize()?;
    fsx::ensure_outside(&store.root, &source)?;
    validate_worktree(&source)?;
    let requests = exclude
        .iter()
        .map(|p| (p.clone(), EXCLUDED_BY_OPERATOR))
        .chain(
            convention_exclude
                .iter()
                .map(|p| (p.clone(), EXCLUDED_BY_CONVENTION_FILE)),
        )
        .collect::<Vec<_>>();
    let declared = declared_exclusions(&source, &requests, include_ignored)?;
    // Linked worktrees of this repository are skipped as whole folders and recorded;
    // foreign nested repositories still refuse during the capture walk.
    let gitlinks_now = read_index(&source)?
        .into_iter()
        .filter(|(_, mode, _)| mode == GITLINK_MODE)
        .map(|(path, _, _)| path)
        .collect::<BTreeSet<_>>();
    let linked = linked_worktrees_inside(&source, &declared, &gitlinks_now)?;
    let pruned = std::cell::RefCell::new(BTreeSet::new());
    let prune = |p: &str| {
        let hit = declared.iter().find(|(x, _)| x.as_str() == p);
        if let Some((x, _)) = hit {
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
    let journaled = store.journaled(marker.journal.clone());
    let store = &journaled;
    let run = || -> Result<Manifest> {
        // Gitlinks are recorded as pointers: their directories are pruned from the file
        // walk so a submodule's `.git` and files are never read.
        let index_before = read_index(&source)?;
        let submodules = submodule_records(&source, &index_before)?;
        let gitlinks = submodules
            .iter()
            .map(|s| s.path.clone())
            .collect::<BTreeSet<_>>();
        let scan = |include: &dyn Fn(&str) -> bool| -> Result<Vec<Entry>> {
            let mut entries = fsx::inventory_pruned(
                &source,
                |p| gitlinks.contains(p) || linked.contains_key(p) || prune(p),
                include,
            )?;
            entries.extend(submodules.iter().map(|s| Entry {
                path: s.path.clone(),
                kind: EntryKind::Submodule,
                mode: 0,
                size: 0,
                sha256: String::new(),
            }));
            entries.sort_by(|a, b| a.path.cmp(&b.path));
            fsx::validate_entries(&entries)?;
            Ok(entries)
        };
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
        let entries = scan(&|p| !exclude(p))?;
        for (selector, reason) in &declared {
            ensure!(
                pruned.borrow().contains(selector),
                "{} matched nothing during capture: {selector}",
                exclusion_label(reason)
            );
        }
        let head = git::text(&source, &["rev-parse", "--verify", "HEAD"])?;
        let branch = git::text(&source, &["symbolic-ref", "-q", "HEAD"]).ok();
        let object_ids = index_before
            .iter()
            .filter(|(_, mode, _)| mode != GITLINK_MODE)
            .map(|(_, _, oid)| oid.clone())
            .collect::<Vec<_>>();
        let mut staged_objects = BTreeMap::new();
        git::for_each_blob(&source, &object_ids, |oid, bytes| {
            staged_objects.insert(oid.to_owned(), store.put(&bytes)?);
            Ok(())
        })?;
        let mut index = Vec::new();
        for (path, mode, oid) in index_before.iter().filter(|(_, m, _)| m != GITLINK_MODE) {
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
            entries == scan(&|p| !exclude(p))?,
            "working files changed during capture; checkpoint remains incomplete"
        );
        ensure!(
            index_before == read_index(&source)?,
            "index changed during capture"
        );
        let submodules_after = submodule_records(&source, &index_before)?;
        if let Some(changed) = submodules
            .iter()
            .zip(&submodules_after)
            .find(|(a, b)| a != b)
        {
            bail!(
                "submodule checkout changed during capture: {}",
                changed.0.path
            );
        }
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
            linked == linked_worktrees_inside(&source, &declared, &gitlinks)?,
            "linked worktrees inside the worktree changed during capture"
        );
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
        // Operator exclusions come first, then convention-file exclusions, then linked
        // worktrees skipped automatically. Ignored omissions keep their own records, even
        // below a skipped folder, so later assessment compares like with like.
        let excluded = declared
            .iter()
            .filter(|(_, reason)| *reason == EXCLUDED_BY_OPERATOR)
            .chain(
                declared
                    .iter()
                    .filter(|(_, reason)| *reason == EXCLUDED_BY_CONVENTION_FILE),
            )
            .map(|(path, reason)| Exclusion {
                path: path.clone(),
                reason: (*reason).into(),
            })
            .chain(linked.keys().map(|path| Exclusion {
                path: path.clone(),
                reason: LINKED_WORKTREE_CAPTURED_SEPARATELY.into(),
            }))
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
                submodules,
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
        store.publish(&manifest)?;
        Ok(manifest)
    };
    let manifest = run().map_err(|error| marker.refused(error))?;
    marker.finish(store)?;
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
    let journaled = store.journaled(marker.journal.clone());
    let store = &journaled;
    let run = || -> Result<Manifest> {
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
        store.publish(&manifest)?;
        Ok(manifest)
    };
    let manifest = run().map_err(|error| marker.refused(error))?;
    marker.finish(store)?;
    Ok(manifest)
}
