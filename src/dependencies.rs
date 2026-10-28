// Copyright 2026 Kenneth A. Mendoza (MendozaLab). https://github.com/MendozaLab/Timber
// SPDX-License-Identifier: Apache-2.0
//! Portable recovery of explicitly declared Git history; source repositories stay read-only.
use crate::{
    fsx, git,
    model::*,
    store::{Store, valid_digest, valid_oid},
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

#[derive(Debug, Clone)]
pub struct DependencyLayers {
    pub base: String,
    pub index: String,
    pub working: String,
    pub untracked: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct TreeEntry {
    mode: String,
    oid: String,
}

pub fn validate_declaration(value: &GitDependency) -> Result<()> {
    ensure!(!value.group.is_empty(), "empty dependency group");
    ensure!(valid_oid(&value.oid), "invalid declared Git object");
    ensure!(
        matches!(value.kind.as_str(), "" | "commit" | "stash"),
        "unsupported dependency kind"
    );
    if let Some(reference) = &value.reference {
        ensure!(
            reference.starts_with("refs/")
                && !reference.chars().any(char::is_control)
                && !reference.contains("..")
                && !reference.contains('@'),
            "invalid pinned Git reference"
        );
    }
    if let Some(head) = &value.source_head {
        ensure!(valid_oid(head), "invalid dependency source head");
    }
    let mut paths = BTreeSet::new();
    for path in &value.tracked_paths {
        fsx::validate_relative(path)?;
        ensure!(paths.insert(path), "duplicate dependency path");
    }
    for (path, expected) in &value.expected_files {
        fsx::validate_relative(path)?;
        ensure!(
            value.tracked_paths.is_empty() || paths.contains(path),
            "expected dependency path is not declared tracked"
        );
        ensure!(
            matches!(expected.kind.as_str(), "" | "file" | "symlink" | "absent"),
            "unsupported expected file kind"
        );
        ensure!(
            expected.mode <= 0o777,
            "unsupported expected file permissions"
        );
        if expected.kind == "absent" {
            ensure!(
                expected.sha256.is_empty() && expected.bytes == 0,
                "absent expectation must not claim content"
            );
        } else {
            ensure!(
                valid_digest(&expected.sha256),
                "invalid expected dependency digest"
            );
        }
    }
    Ok(())
}

fn resolve_reference(repo: &Path, value: &GitDependency) -> Result<()> {
    if let Some(reference) = &value.reference {
        let actual = git::text(
            repo,
            &[
                "rev-parse",
                "--verify",
                "--end-of-options",
                &format!("{reference}^{{commit}}"),
            ],
        )?;
        ensure!(
            actual == value.oid,
            "dependency reference no longer matches pinned object: {}",
            value.group
        );
    }
    Ok(())
}

pub fn layers(repo: &Path, value: &GitDependency) -> Result<DependencyLayers> {
    validate_declaration(value)?;
    ensure!(
        git::text(repo, &["cat-file", "-t", &value.oid])? == "commit",
        "dependency is not a commit"
    );
    let raw = git::text(repo, &["rev-list", "--parents", "-n", "1", &value.oid])?;
    let fields: Vec<_> = raw.split_whitespace().collect();
    ensure!(
        fields.first() == Some(&value.oid.as_str()) && fields.iter().all(|oid| valid_oid(oid)),
        "invalid dependency ancestry"
    );
    if let Some(head) = &value.source_head {
        ensure!(
            fields.get(1).copied() == Some(head.as_str()),
            "dependency source-head mismatch"
        );
    }
    if value.kind == "stash" {
        ensure!(
            matches!(fields.len(), 3 | 4),
            "stash dependency must have two or three parents"
        );
        let index_parent = git::text(repo, &["rev-list", "--parents", "-n", "1", fields[2]])?;
        let index_fields: Vec<_> = index_parent.split_whitespace().collect();
        ensure!(
            index_fields.len() == 2 && index_fields[1] == fields[1],
            "stash index parent does not match base"
        );
        Ok(DependencyLayers {
            base: fields[1].into(),
            index: fields[2].into(),
            working: value.oid.clone(),
            untracked: fields.get(3).map(|oid| (*oid).into()),
        })
    } else {
        Ok(DependencyLayers {
            base: value.oid.clone(),
            index: value.oid.clone(),
            working: value.oid.clone(),
            untracked: None,
        })
    }
}

fn tree(repo: &Path, revision: &str) -> Result<BTreeMap<String, TreeEntry>> {
    let bytes = git::run(repo, &["ls-tree", "-r", "-z", "--full-tree", revision])?;
    let mut result = BTreeMap::new();
    for row in bytes.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let tab = row
            .iter()
            .position(|b| *b == b'\t')
            .context("invalid dependency tree record")?;
        let fields: Vec<_> = std::str::from_utf8(&row[..tab])?
            .split_whitespace()
            .collect();
        ensure!(
            fields.len() == 3
                && fields[1] == "blob"
                && matches!(fields[0], "100644" | "100755" | "120000")
                && valid_oid(fields[2]),
            "unsupported dependency tree entry"
        );
        let path = std::str::from_utf8(&row[tab + 1..])?.to_owned();
        fsx::validate_relative(&path)?;
        ensure!(
            result
                .insert(
                    path,
                    TreeEntry {
                        mode: fields[0].into(),
                        oid: fields[2].into()
                    }
                )
                .is_none(),
            "duplicate dependency path"
        );
    }
    let entries = result
        .iter()
        .map(|(path, entry)| Entry {
            path: path.clone(),
            kind: if entry.mode == "120000" {
                EntryKind::Symlink
            } else {
                EntryKind::File
            },
            mode: 0o644,
            size: 0,
            sha256: "0".repeat(64),
        })
        .collect::<Vec<_>>();
    fsx::validate_entries(&entries)?;
    Ok(result)
}

fn check_expected(repo: &Path, value: &GitDependency, state: &DependencyLayers) -> Result<()> {
    let files = tree(repo, &state.working)?;
    let oids = value
        .expected_files
        .iter()
        .filter(|(_, e)| e.kind != "absent")
        .map(|(path, _)| {
            files
                .get(path)
                .map(|e| e.oid.clone())
                .with_context(|| format!("expected dependency path missing: {path}"))
        })
        .collect::<Result<Vec<_>>>()?;
    let blobs = git::batch_blobs(repo, &oids)?;
    for (path, expected) in &value.expected_files {
        if expected.kind == "absent" {
            ensure!(
                !files.contains_key(path),
                "expected dependency deletion is present: {path}"
            );
            continue;
        }
        let entry = files.get(path).context("expected path missing")?;
        let bytes = blobs
            .get(&entry.oid)
            .context("expected dependency blob unavailable")?;
        ensure!(
            bytes.len() as u64 == expected.bytes && fsx::digest(bytes) == expected.sha256,
            "dependency content does not match expected fingerprint: {path}"
        );
        let symlink = expected.kind == "symlink";
        ensure!(
            symlink == (entry.mode == "120000"),
            "dependency file kind differs: {path}"
        );
        if !symlink {
            ensure!(
                (expected.mode & 0o111 != 0) == (entry.mode == "100755"),
                "dependency executable flag differs: {path}"
            );
        }
    }
    Ok(())
}

pub fn capture(
    store: &Store,
    repo: &Path,
    declarations: &[GitDependency],
) -> Result<Vec<CapturedDependency>> {
    ensure!(
        git::text(repo, &["rev-parse", "--show-object-format"])? == "sha1",
        "dependency export supports SHA-1 repositories only"
    );
    ensure!(
        git::text(repo, &["rev-parse", "--is-shallow-repository"])? == "false",
        "shallow dependency history unsupported"
    );
    let roots = git::protected_roots(repo)?;
    for root in &roots {
        fsx::ensure_outside(&store.root, root)?;
    }
    let mut protected_roots = roots
        .iter()
        .map(|root| {
            let root = root.canonicalize().unwrap_or_else(|_| root.clone());
            root.to_str()
                .context("non UTF-8 dependency protected root")
                .map(str::to_owned)
        })
        .collect::<Result<Vec<_>>>()?;
    protected_roots.sort();
    protected_roots.dedup();
    let mut captured = Vec::new();
    for declaration in declarations {
        validate_declaration(declaration)?;
        resolve_reference(repo, declaration)?;
        let state = layers(repo, declaration)?;
        check_expected(repo, declaration, &state)?;
        // Export immutable object identity and complete reachable history without
        // ever creating a reference, lock or index in the source repository.
        let pack = git::run_input(
            repo,
            &["pack-objects", "--stdout", "--revs"],
            format!("{}\n", declaration.oid).as_bytes(),
        )?;
        let scratch = tempfile::Builder::new()
            .prefix("timber-dependency-export-")
            .tempdir()?;
        git::init(scratch.path())?;
        git::run_input(scratch.path(), &["index-pack", "--stdin"], &pack)?;
        git::run(
            scratch.path(),
            &[
                "update-ref",
                "refs/heads/timber-dependency",
                &declaration.oid,
            ],
        )?;
        let bundle = scratch.path().join("dependency.bundle");
        git::run(
            scratch.path(),
            &[
                "bundle",
                "create",
                bundle.to_str().context("non UTF-8 scratch")?,
                "refs/heads/timber-dependency",
            ],
        )?;
        let record = CapturedDependency {
            dependency: declaration.clone(),
            bundle: store.put(&fs::read(&bundle)?)?,
            protected_roots: protected_roots.clone(),
        };
        verify(store, &record)?;
        resolve_reference(repo, declaration)?;
        captured.push(record);
    }
    ensure!(
        roots == git::protected_roots(repo)?,
        "dependency repository registration changed during capture"
    );
    Ok(captured)
}

fn import_objects(
    store: &Store,
    captured: &CapturedDependency,
    to: &Path,
) -> Result<DependencyLayers> {
    git::init(to)?;
    let bundle = tempfile::Builder::new()
        .prefix("timber-dependency-bundle-")
        .tempdir()?;
    let path = bundle.path().join("dependency.bundle");
    fs::write(&path, store.get(&captured.bundle)?)?;
    let name = path.to_str().context("non UTF-8 scratch")?;
    git::run(to, &["bundle", "verify", name])?;
    git::run(to, &["bundle", "unbundle", name])?;
    let state = layers(to, &captured.dependency)?;
    check_expected(to, &captured.dependency, &state)?;
    Ok(state)
}

fn reconstruct(store: &Store, captured: &CapturedDependency, to: &Path) -> Result<()> {
    let state = import_objects(store, captured, to)?;
    let indexed = tree(to, &state.index)?;
    let mut working = tree(to, &state.working)?;
    if let Some(untracked) = &state.untracked {
        for (path, entry) in tree(to, untracked)? {
            ensure!(
                !working.contains_key(&path),
                "stash untracked path collides with tracked path"
            );
            working.insert(path, entry);
        }
    }
    let oids = working
        .values()
        .chain(indexed.values())
        .map(|e| e.oid.clone())
        .collect::<Vec<_>>();
    let blobs = git::batch_blobs(to, &oids)?;
    let mut contents = BTreeMap::new();
    let mut entries = Vec::new();
    for (path, entry) in &working {
        let bytes = blobs
            .get(&entry.oid)
            .context("restored dependency blob missing")?;
        let digest = fsx::digest(bytes);
        contents.insert(digest.clone(), bytes.clone());
        entries.push(Entry {
            path: path.clone(),
            kind: if entry.mode == "120000" {
                EntryKind::Symlink
            } else {
                EntryKind::File
            },
            mode: match entry.mode.as_str() {
                "100755" => 0o755,
                "120000" => 0o777,
                _ => 0o644,
            },
            size: bytes.len() as u64,
            sha256: digest,
        });
    }
    fsx::validate_entries(&entries)?;
    git::run(to, &["update-ref", "--no-deref", "HEAD", &state.base])?;
    let mut index_info = Vec::new();
    for (path, entry) in &indexed {
        index_info
            .extend_from_slice(format!("{} {} 0\t{}\0", entry.mode, entry.oid, path).as_bytes());
    }
    git::run_input(to, &["update-index", "-z", "--index-info"], &index_info)?;
    fsx::materialize(to, &entries, |digest| {
        contents
            .get(digest)
            .cloned()
            .context("missing materialized dependency content")
    })?;
    // Compare independently enumerated files and index to the expected Git layers.
    let actual = fsx::inventory(to)?;
    let actual_files = actual
        .into_iter()
        .filter(|e| e.kind != EntryKind::Directory)
        .collect::<Vec<_>>();
    ensure!(
        actual_files == entries,
        "recovered dependency working files differ"
    );
    let wanted = indexed
        .iter()
        .map(|(path, e)| (path.clone(), e.mode.clone(), e.oid.clone()))
        .collect::<Vec<_>>();
    ensure!(
        crate::capture::read_index(to)? == wanted,
        "recovered dependency staged layer differs"
    );
    ensure!(
        git::text(to, &["rev-parse", "HEAD"])? == state.base,
        "recovered dependency base differs"
    );
    git::run(to, &["fsck", "--full", "--no-reflogs"])?;
    Ok(())
}

pub fn verify(store: &Store, captured: &CapturedDependency) -> Result<()> {
    let scratch = tempfile::Builder::new()
        .prefix("timber-dependency-verify-")
        .tempdir()?;
    let destination = scratch.path().join("restored");
    fsx::fresh_dir(&destination)?;
    reconstruct(store, captured, &destination)
}

pub fn restore_dependency(store: &Store, id: &str, group: &str, to: &Path) -> Result<Verification> {
    let manifest = store.load(id)?;
    fsx::ensure_outside(to, Path::new(&manifest.source_path))?;
    fsx::ensure_outside(to, &store.root)?;
    for dependency in &manifest.captured_dependencies {
        for root in &dependency.protected_roots {
            fsx::ensure_outside(to, Path::new(root))?;
        }
    }
    let captured = manifest
        .captured_dependencies
        .iter()
        .find(|d| d.dependency.group == group)
        .context("captured dependency group not found; import with --dependency-repo first")?;
    // Validate bytes and expected identity before creating the user destination.
    verify(store, captured)?;
    fsx::fresh_dir(to)?;
    reconstruct(store, captured, to)?;
    Ok(Verification {schema:SCHEMA,checkpoint:id.into(),manifest_sha256:fsx::digest(&serde_json::to_vec(&manifest)?),verifier_version:env!("CARGO_PKG_VERSION").into(),verified:true,historical_completeness:"captured_git_dependency_layers_only".into(),independent_backup:"unassessed".into(),excluded_by_operator:Vec::new(),excluded_by_convention_file:Vec::new(),linked_worktrees_captured_separately:Vec::new(),submodules_recorded_not_captured:Vec::new(),notes:vec![format!("Recovered pinned Git dependency group {group:?} with independent base, staged and working layers."),"This does not establish former worktree completeness, application correctness, independent backup or cleanup authority.".into()]})
}
