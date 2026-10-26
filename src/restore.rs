//! Independent reconstruction; no checkout, hooks, filters or original object store.
use crate::{
    capture::read_index,
    fsx, git,
    model::{Manifest, SCHEMA, Verification},
    store::Store,
};
use anyhow::{Context, Result, ensure};
use std::{fs, path::Path};

pub fn restore(store: &Store, id: &str, to: &Path) -> Result<Verification> {
    let manifest = store.load(id)?;
    fsx::ensure_outside(to, Path::new(&manifest.source_path))?;
    if let Some(state) = &manifest.git {
        for root in &state.protected_roots {
            fsx::ensure_outside(to, Path::new(root))?;
        }
    }
    fsx::ensure_outside(to, &store.root)?;
    // Validate all bytes and every path before creating a destination.
    for entry in &manifest.entries {
        if entry.kind != crate::model::EntryKind::Directory {
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
        let mut index_info = Vec::new();
        for entry in &state.index {
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
            index_info.extend_from_slice(
                format!("{} {} 0\t{}\0", entry.mode, entry.oid, entry.path).as_bytes(),
            );
        }
        git::run_input(to, &["update-index", "-z", "--index-info"], &index_info)?;
        git::run(to, &["fsck", "--full", "--no-reflogs"])?;
    }
    fsx::materialize(to, &manifest.entries, |digest| store.get(digest))?;
    Ok(())
}

fn check_restored(store: &Store, manifest: &Manifest, to: &Path) -> Result<Verification> {
    let actual = fsx::inventory(to)?;
    ensure!(
        actual == manifest.entries,
        "restored paths, bytes, symlinks or modes differ from checkpoint"
    );
    if let Some(state) = &manifest.git {
        ensure!(
            git::text(to, &["rev-parse", "--verify", "HEAD"])? == state.head,
            "restored HEAD differs"
        );
        let wanted: Vec<_> = state
            .index
            .iter()
            .map(|e| (e.path.clone(), e.mode.clone(), e.oid.clone()))
            .collect();
        ensure!(read_index(to)? == wanted, "restored logical index differs");
        for entry in &state.index {
            ensure!(
                git::run(to, &["cat-file", "blob", &entry.oid])? == store.get(&entry.blob)?,
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
    if manifest.source_kind == "folder" {
        notes.push("Folder import verifies files present at import time; historical receipts remain producer claims, not proof of former worktree completeness.".into());
    }
    if let Some(map) = &manifest.import_map {
        if !map.git_dependencies.is_empty() {
            notes.push(format!("{} declared external Git dependencies remain unresolved and are not captured by this folder import.", map.git_dependencies.len()));
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
        notes,
    })
}
