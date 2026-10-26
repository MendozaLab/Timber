//! Capture supported state without staging, committing or cleaning the source.
use crate::{
    fsx, git,
    model::*,
    store::{Store, manifest_id, valid_oid, validate_import_map},
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeSet,
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
    let source = path.canonicalize()?;
    fsx::ensure_outside(&store.root, &source)?;
    validate_worktree(&source)?;
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
    let entries = fsx::inventory_filtered(&source, |p| !exclude(p))?;
    let head = git::text(&source, &["rev-parse", "--verify", "HEAD"])?;
    let branch = git::text(&source, &["symbolic-ref", "-q", "HEAD"]).ok();
    let index_before = read_index(&source)?;
    let mut index = Vec::new();
    for (path, mode, oid) in &index_before {
        let bytes = git::run(&source, &["cat-file", "blob", oid])?;
        index.push(IndexEntry {
            path: path.clone(),
            mode: mode.clone(),
            oid: oid.clone(),
            blob: store.put(&bytes)?,
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
        entries == fsx::inventory_filtered(&source, |p| !exclude(p))?,
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
    let excluded = ignored
        .into_iter()
        .filter(|p| !included(p, &selectors))
        .map(|path| Exclusion {
            path,
            reason: "ignored file not explicitly selected".into(),
        })
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
    let source = path.canonicalize()?;
    fsx::ensure_outside(&store.root, &source)?;
    ensure!(
        !source.join(".git").try_exists()?,
        "folder is a Git repository; use checkpoint instead"
    );
    if let Some(map) = &map {
        validate_import_map(map)?;
    }
    let marker = started(store, &source, "folder_import")?;
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
    };
    manifest.id = manifest_id(&manifest)?;
    finish(store, &marker, &manifest)?;
    Ok(manifest)
}
