// Copyright 2026 Kenneth A. Mendoza (MendozaLab). https://github.com/MendozaLab/Timber
// SPDX-License-Identifier: Apache-2.0
//! Independent reconstruction; no checkout, hooks, filters or original object store.
use crate::{
    capture::{GITLINK_MODE, read_index},
    fsx, git,
    model::{
        EXCLUDED_BY_CONVENTION_FILE, EXCLUDED_BY_OPERATOR, EntryKind,
        LINKED_WORKTREE_CAPTURED_SEPARATELY, Manifest, SCHEMA, Verification,
    },
    store::Store,
};
use anyhow::{Context, Result, ensure};
use std::{collections::BTreeSet, fs, path::Path};

pub fn restore(store: &Store, id: &str, to: &Path) -> Result<Verification> {
    let manifest = store.load(id)?;
    fsx::ensure_outside(to, Path::new(&manifest.source_path))?;
    if let Some(state) = &manifest.git {
        for root in &state.protected_roots {
            fsx::ensure_outside(to, Path::new(root))?;
        }
    }
    for dependency in &manifest.captured_dependencies {
        for root in &dependency.protected_roots {
            fsx::ensure_outside(to, Path::new(root))?;
        }
    }
    fsx::ensure_outside(to, &store.root)?;
    // Validate all bytes and every path before creating a destination.
    for entry in &manifest.entries {
        if matches!(entry.kind, EntryKind::File | EntryKind::Symlink) {
            let content = store.get(&entry.sha256)?;
            ensure!(
                content.len() as u64 == entry.size,
                "stored entry size mismatch"
            );
        }
    }
    if let Some(state) = &manifest.git {
        store.get(&state.bundle)?;
        for entry in &state.index {
            store.get(&entry.blob)?;
        }
    }
    for dependency in &manifest.captured_dependencies {
        crate::dependencies::verify(store, dependency)?;
    }
    fsx::fresh_dir(to)?;
    reconstruct(store, &manifest, to)?;
    check_restored(store, &manifest, to)
}

pub fn verify(store: &Store, id: &str) -> Result<Verification> {
    // Verification scratch is deliberately independent of both source and store.
    let scratch = tempfile::Builder::new()
        .prefix("timber-verify-")
        .tempdir()?;
    restore(store, id, &scratch.path().join("restored"))
}

fn reconstruct(store: &Store, manifest: &Manifest, to: &Path) -> Result<()> {
    if let Some(state) = &manifest.git {
        git::init(to)?;
        ensure!(
            git::text(to, &["rev-parse", "--show-object-format"])? == state.object_format,
            "Git object format differs"
        );
        let staging = tempfile::Builder::new()
            .prefix("timber-history-")
            .tempdir()?;
        let bundle_path = staging.path().join("history.bundle");
        fs::write(&bundle_path, store.get(&state.bundle)?)?;
        let bundle_str = bundle_path
            .to_str()
            .context("non UTF-8 verification scratch path")?;
        git::run(to, &["bundle", "verify", bundle_str])?;
        git::run(to, &["bundle", "unbundle", bundle_str])?;
        ensure!(
            git::text(to, &["cat-file", "-t", &state.head])? == "commit",
            "HEAD is not a commit"
        );
        // Write only the new repository; --no-deref detaches rather than updating
        // an implicitly chosen branch.
        git::run(to, &["update-ref", "--no-deref", "HEAD", &state.head])?;
        let object_ids = state
            .index
            .iter()
            .map(|entry| entry.oid.clone())
            .collect::<BTreeSet<_>>();
        let mut ensured_objects = existing_blobs(to, &object_ids)?;
        let mut index_info = Vec::new();
        for entry in &state.index {
            // Most index blobs are already in the self-contained history bundle.
            // Only newly staged objects need insertion; duplicate paths share it.
            if ensured_objects.insert(entry.oid.clone()) {
                let bytes = store.get(&entry.blob)?;
                let actual = String::from_utf8(git::run_input(
                    to,
                    &["hash-object", "-t", "blob", "--no-filters", "-w", "--stdin"],
                    &bytes,
                )?)?;
                ensure!(
                    actual.trim() == entry.oid,
                    "staged blob Git object identity mismatch: {}",
                    entry.path
                );
            }
            index_info.extend_from_slice(
                format!("{} {} 0\t{}\0", entry.mode, entry.oid, entry.path).as_bytes(),
            );
        }
        // A gitlink needs no object: the pointer is restored, never the submodule.
        for submodule in &state.submodules {
            index_info.extend_from_slice(
                format!(
                    "{GITLINK_MODE} {} 0\t{}\0",
                    submodule.gitlink_commit, submodule.path
                )
                .as_bytes(),
            );
        }
        git::run_input(to, &["update-index", "-z", "--index-info"], &index_info)?;
        git::run(to, &["fsck", "--full", "--no-reflogs"])?;
    }
    fsx::materialize(to, &manifest.entries, |digest| store.get(digest))?;
    Ok(())
}

fn existing_blobs(repo: &Path, object_ids: &BTreeSet<String>) -> Result<BTreeSet<String>> {
    if object_ids.is_empty() {
        return Ok(BTreeSet::new());
    }
    let mut input = String::new();
    for oid in object_ids {
        input.push_str(oid);
        input.push('\n');
    }
    let output = git::run_input(repo, &["cat-file", "--batch-check"], input.as_bytes())?;
    let text = std::str::from_utf8(&output)?;
    let rows = text.lines().collect::<Vec<_>>();
    ensure!(
        rows.len() == object_ids.len(),
        "Git object-presence response count differs"
    );
    let mut present = BTreeSet::new();
    for (expected, row) in object_ids.iter().zip(rows) {
        let fields = row.split_whitespace().collect::<Vec<_>>();
        ensure!(
            fields.first().copied() == Some(expected.as_str()),
            "Git object-presence identity differs"
        );
        if fields.len() == 2 && fields[1] == "missing" {
            continue;
        }
        ensure!(
            fields.len() == 3 && fields[1] == "blob" && fields[2].parse::<u64>().is_ok(),
            "unsupported staged object-presence response"
        );
        present.insert(expected.clone());
    }
    Ok(present)
}

fn check_restored(store: &Store, manifest: &Manifest, to: &Path) -> Result<Verification> {
    let mut actual = fsx::inventory(to)?;
    // Each recorded submodule must come back as an empty directory; any file below it
    // would appear as an extra entry and fail the comparison.
    for entry in &mut actual {
        if let Some(recorded) = manifest
            .entries
            .iter()
            .find(|e| e.kind == EntryKind::Submodule && e.path == entry.path)
        {
            ensure!(
                entry.kind == EntryKind::Directory,
                "restored submodule path is not a directory: {}",
                entry.path
            );
            *entry = recorded.clone();
        }
    }
    ensure!(
        actual == manifest.entries,
        "restored paths, bytes, symlinks or modes differ from checkpoint"
    );
    if let Some(state) = &manifest.git {
        ensure!(
            git::text(to, &["rev-parse", "--verify", "HEAD"])? == state.head,
            "restored HEAD differs"
        );
        let mut wanted: Vec<_> = state
            .index
            .iter()
            .map(|e| (e.path.clone(), e.mode.clone(), e.oid.clone()))
            .chain(state.submodules.iter().map(|s| {
                (
                    s.path.clone(),
                    GITLINK_MODE.to_owned(),
                    s.gitlink_commit.clone(),
                )
            }))
            .collect();
        wanted.sort();
        ensure!(read_index(to)? == wanted, "restored logical index differs");
        let object_ids = state
            .index
            .iter()
            .map(|entry| entry.oid.clone())
            .collect::<Vec<_>>();
        let fingerprints = git::batch_blob_fingerprints(to, &object_ids)?;
        for entry in &state.index {
            let stored = store.get(&entry.blob)?;
            ensure!(
                fingerprints.get(&entry.oid) == Some(&(entry.blob.clone(), stored.len() as u64)),
                "restored staged content differs"
            );
        }
    }
    let mut notes = vec![
        "Verified only the supported state captured in this checkpoint; this does not establish application correctness or authorize cleanup.".into(),
        "Matching scans and a declared pause do not prove enforced exclusion of concurrent writers.".into(),
        "Timestamps, ownership, extended attributes, hard-link topology and external runtime state are outside the metadata guarantee.".into(),
    ];
    if manifest.git.is_some() {
        notes.push("Restored Git HEAD is detached; the original branch name remains recorded in the checkpoint.".into());
    }
    if !manifest.excluded.is_empty() {
        notes.push(format!(
            "{} excluded paths remain outside this recovery claim.",
            manifest.excluded.len()
        ));
    }
    let submodules = manifest
        .git
        .as_ref()
        .map(|state| state.submodules.clone())
        .unwrap_or_default();
    if !submodules.is_empty() {
        notes.push(format!(
            "{} submodules are recorded as pointers only; their contents were not captured. Each path is restored as an empty directory with its gitlink in the index; nothing was fetched or initialized. See submodules_recorded_not_captured.",
            submodules.len()
        ));
        for submodule in &submodules {
            notes.push(format!(
                "Submodule {}: gitlink commit {}; checked-out commit {}; contents not captured.",
                submodule.path,
                submodule.gitlink_commit,
                submodule
                    .checked_out_commit
                    .as_deref()
                    .unwrap_or("none (not initialized at capture)")
            ));
        }
    }
    let excluded_by_operator = manifest
        .excluded
        .iter()
        .filter(|x| x.reason == EXCLUDED_BY_OPERATOR)
        .map(|x| x.path.clone())
        .collect::<Vec<_>>();
    if !excluded_by_operator.is_empty() {
        notes.push(format!(
            "Partial capture: the operator excluded {} paths at checkpoint time; they and everything below them are absent from this checkpoint and from any restore. See excluded_by_operator.",
            excluded_by_operator.len()
        ));
    }
    let excluded_by_convention_file = manifest
        .excluded
        .iter()
        .filter(|x| x.reason == EXCLUDED_BY_CONVENTION_FILE)
        .map(|x| x.path.clone())
        .collect::<Vec<_>>();
    if !excluded_by_convention_file.is_empty() {
        notes.push(format!(
            "Partial capture: the repository's timber.toml excluded {} paths at checkpoint time; they and everything below them are absent from this checkpoint and from any restore. See excluded_by_convention_file.",
            excluded_by_convention_file.len()
        ));
    }
    let linked_worktrees_captured_separately = manifest
        .excluded
        .iter()
        .filter(|x| x.reason == LINKED_WORKTREE_CAPTURED_SEPARATELY)
        .map(|x| x.path.clone())
        .collect::<Vec<_>>();
    if !linked_worktrees_captured_separately.is_empty() {
        notes.push(format!(
            "{} linked worktrees of this repository sat inside the worktree and were skipped at checkpoint time; their folders are absent from this checkpoint and from any restore. Checkpoint each linked worktree on its own. See linked_worktrees_captured_separately.",
            linked_worktrees_captured_separately.len()
        ));
    }
    if manifest.source_kind == "folder" {
        notes.push("Folder import verifies files present at import time; historical receipts remain producer claims, not proof of former worktree completeness.".into());
    }
    if let Some(map) = &manifest.import_map {
        let unresolved = map.git_dependencies.len() - manifest.captured_dependencies.len();
        if unresolved != 0 {
            notes.push(format!("{unresolved} declared external Git dependencies remain unresolved and are not captured by this folder import."));
        }
        if !manifest.captured_dependencies.is_empty() {
            notes.push(format!("{} pinned Git dependencies were independently reconstructed and verified from captured bundles; restore-git can recover their staged and working layers without the original repository.", manifest.captured_dependencies.len()));
        }
    }
    Ok(Verification {
        schema: SCHEMA,
        checkpoint: manifest.id.clone(),
        manifest_sha256: fsx::digest(&serde_json::to_vec(manifest)?),
        verifier_version: env!("CARGO_PKG_VERSION").into(),
        verified: true,
        historical_completeness: if manifest.source_kind == "folder" {
            "unknown"
        } else {
            "captured_supported_state_only"
        }
        .into(),
        independent_backup: "unassessed".into(),
        excluded_by_operator,
        excluded_by_convention_file,
        linked_worktrees_captured_separately,
        submodules_recorded_not_captured: submodules,
        notes,
    })
}

pub fn restore_dependency(store: &Store, id: &str, group: &str, to: &Path) -> Result<Verification> {
    crate::dependencies::restore_dependency(store, id, group, to)
}
