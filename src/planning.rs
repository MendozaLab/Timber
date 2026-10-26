//! Plans bind source order and selected layers to immutable checkpoints.
use crate::{fsx, git, model::*, preview, restore, store::Store};
use anyhow::{Context, Result, ensure};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

fn alternates(manifest: &Manifest) -> Vec<String> {
    let Some(state) = &manifest.git else {
        return Vec::new();
    };
    let working: BTreeMap<_, _> = manifest
        .entries
        .iter()
        .map(|e| (e.path.as_str(), e))
        .collect();
    state
        .index
        .iter()
        .filter(|i| {
            working.get(i.path.as_str()).is_none_or(|e| {
                e.sha256 != i.blob
                    || (e.kind == EntryKind::Symlink) != (i.mode == "120000")
                    || (e.kind != EntryKind::Symlink && e.mode & 0o111 != 0) != (i.mode == "100755")
            })
        })
        .map(|i| i.path.clone())
        .collect()
}

pub fn create(store: &Store, target: Selection, sources: Vec<Selection>) -> Result<Plan> {
    let target_manifest = store.load(&target.checkpoint)?;
    ensure!(
        target_manifest.git.is_some(),
        "target must be a Git worktree checkpoint"
    );
    preview::validate_selection(&target_manifest, &target)?;
    ensure!(
        restore::verify(store, &target.checkpoint)?.verified,
        "target checkpoint did not verify"
    );
    let scratch = tempfile::tempdir()?;
    git::init(scratch.path())?;
    preview::import_manifest_objects(store, &target_manifest, scratch.path())?;
    let target_head = &target_manifest.git.as_ref().unwrap().head;
    let target_files = preview::selected_files(store, scratch.path(), &target_manifest, &target)?;
    let mut comparisons = Vec::new();
    let mut selected_ids = BTreeSet::new();
    for selection in &sources {
        ensure!(
            selected_ids.insert(&selection.checkpoint),
            "source checkpoint selected more than once"
        );
        let manifest = store.load(&selection.checkpoint)?;
        preview::validate_selection(&manifest, selection)?;
        ensure!(
            restore::verify(store, &manifest.id)?.verified,
            "source checkpoint did not verify"
        );
        let mut row = Comparison {
            checkpoint: manifest.id.clone(),
            already_in_target_history: false,
            unique_commits: Vec::new(),
            identical_paths: Vec::new(),
            differing_paths: Vec::new(),
            staged_alternates: alternates(&manifest),
            notes: Vec::new(),
        };
        if let Some(state) = &manifest.git {
            preview::import_manifest_objects(store, &manifest, scratch.path())?;
            let ancestry = git::output(
                scratch.path(),
                &["merge-base", "--is-ancestor", &state.head, target_head],
                None,
            )?;
            ensure!(
                matches!(ancestry.status.code(), Some(0) | Some(1)),
                "could not determine ancestry"
            );
            row.already_in_target_history = ancestry.status.success();
            row.unique_commits = git::text(
                scratch.path(),
                &["rev-list", &format!("{target_head}..{}", state.head)],
            )?
            .lines()
            .map(str::to_owned)
            .collect();
            let files = preview::selected_files(store, scratch.path(), &manifest, selection)?;
            let paths: BTreeSet<_> = files.keys().chain(target_files.keys()).collect();
            for path in paths {
                if files.get(path) == target_files.get(path) {
                    row.identical_paths.push(path.clone());
                } else {
                    row.differing_paths.push(path.clone());
                }
            }
            row.notes.push(format!("Selected {:?} layer. Matching file objects prove exact bytes and Git mode, not equivalent application behavior.", selection.layer));
            if !row.staged_alternates.is_empty() {
                row.notes.push("Staged and working versions differ; only the selected layer enters the preview. Other versions remain in the checkpoint.".into());
            }
        } else {
            row.notes.push("Imported folder: historical worktree completeness and Git ancestry are unknown. Only explicitly mapped or selected files enter a preview.".into());
            if let Some(map) = &manifest.import_map {
                for mapping in &map.files {
                    let entry = manifest
                        .entries
                        .iter()
                        .find(|e| e.path == mapping.stored_path)
                        .context("mapped entry missing")?;
                    if let Some(existing) = target_files.get(&mapping.target_path) {
                        let bytes = git::run(scratch.path(), &["cat-file", "blob", &existing.oid])?;
                        let same_mode = (entry.kind == EntryKind::Symlink)
                            == (existing.mode == "120000")
                            && (entry.kind != EntryKind::Symlink && entry.mode & 0o111 != 0)
                                == (existing.mode == "100755");
                        if fsx::digest(&bytes) == entry.sha256 && same_mode {
                            row.identical_paths.push(mapping.target_path.clone());
                        } else {
                            row.differing_paths.push(mapping.target_path.clone());
                        }
                    } else {
                        row.differing_paths.push(mapping.target_path.clone());
                    }
                }
                for dependency in &map.git_dependencies {
                    let captured = manifest.captured_dependencies.iter().any(|c| {
                        c.dependency.group == dependency.group && c.dependency.oid == dependency.oid
                    });
                    row.notes.push(if captured {
                        format!("Git dependency {} for group {:?} is preserved and recovery-verified separately; restore-git then checkpoint it to select those changes for a preview.",dependency.oid,dependency.group)
                    }else{
                        format!("Declared Git dependency {} for group {:?} has not been captured or independently reconstructed.",dependency.oid,dependency.group)
                    });
                }
            }
        }
        if let Some(state) = &manifest.git {
            let index_paths: BTreeSet<_> = state.index.iter().map(|e| e.path.as_str()).collect();
            let omitted_extras = manifest
                .entries
                .iter()
                .filter(|e| {
                    e.kind != EntryKind::Directory
                        && !index_paths.contains(e.path.as_str())
                        && !selection
                            .extra_paths
                            .iter()
                            .any(|p| e.path == *p || e.path.starts_with(&format!("{p}/")))
                })
                .count();
            if omitted_extras > 0 {
                row.notes.push(format!("{omitted_extras} captured untracked or ignored files are not selected for integration and remain only in the checkpoint."));
            }
        }
        let empty_dirs = manifest
            .entries
            .iter()
            .filter(|e| {
                e.kind == EntryKind::Directory
                    && !manifest
                        .entries
                        .iter()
                        .any(|child| child.path.starts_with(&format!("{}/", e.path)))
            })
            .count();
        if empty_dirs > 0 {
            row.notes.push(format!("{empty_dirs} empty directories are preserved by the checkpoint but are not versioned in Git previews."));
        }
        if !manifest.excluded.is_empty() {
            row.notes.push(format!(
                "{} excluded paths are outside captured recovery coverage.",
                manifest.excluded.len()
            ));
        }
        comparisons.push(row);
    }
    let mut notes = vec![
        "Source order is explicit. Review layers, extra_paths, and folder mappings before previewing; no original worktree will be edited.".into(),
        "Exact duplicate evidence does not authorize deletion. A mechanically clean preview does not establish application correctness.".into(),
    ];
    let target_alternates = alternates(&target_manifest);
    if !target_alternates.is_empty() {
        notes.push(format!("Target staged and working versions differ at {:?}; the {:?} layer is selected, and alternatives remain recoverable in its checkpoint.", target_alternates, target.layer));
    }
    Ok(Plan {
        schema: SCHEMA,
        target,
        sources,
        comparisons,
        notes,
    })
}

fn current_index(path: &Path) -> Result<Vec<(String, String, String)>> {
    let raw = git::run(path, &["ls-files", "--stage", "-z"])?;
    let mut values = Vec::new();
    for record in raw.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let tab = record
            .iter()
            .position(|b| *b == b'\t')
            .context("malformed current index")?;
        let fields: Vec<_> = std::str::from_utf8(&record[..tab])?
            .split_whitespace()
            .collect();
        ensure!(
            fields.len() == 3 && fields[2] == "0",
            "current index has unsupported stages"
        );
        values.push((
            std::str::from_utf8(&record[tab + 1..])?.into(),
            fields[0].into(),
            fields[1].into(),
        ));
    }
    values.sort();
    Ok(values)
}

pub fn assess(store: &Store, plan: &Plan) -> Result<serde_json::Value> {
    ensure!(plan.schema == SCHEMA, "unsupported plan schema");
    let mut rows = Vec::new();
    for selection in std::iter::once(&plan.target).chain(&plan.sources) {
        let manifest = store.load(&selection.checkpoint)?;
        preview::validate_selection(&manifest, selection)?;
        let mut blockers = Vec::new();
        let verify = restore::verify(store, &manifest.id);
        match &verify {
            Ok(receipt) if receipt.verified => {}
            Ok(_) => blockers.push("checkpoint_verification_failed".to_string()),
            Err(error) => blockers.push(format!("checkpoint_verification_failed: {error:#}")),
        }
        let source = Path::new(&manifest.source_path);
        // Discover ignored names without opening their contents. New ignored files
        // are coverage drift, not permission to read files the operator never selected.
        let ignored = if manifest.git.is_some() {
            git::run(
                source,
                &[
                    "ls-files",
                    "--others",
                    "--ignored",
                    "--exclude-standard",
                    "-z",
                ],
            )
            .and_then(|bytes| {
                bytes
                    .split(|b| *b == 0)
                    .filter(|b| !b.is_empty())
                    .map(|p| Ok(std::str::from_utf8(p)?.to_owned()))
                    .collect::<Result<BTreeSet<_>>>()
            })
        } else {
            Ok(BTreeSet::new())
        };
        let captured: BTreeSet<_> = manifest
            .entries
            .iter()
            .filter(|e| e.kind != EntryKind::Directory)
            .map(|e| e.path.as_str())
            .collect();
        let expected_omissions: BTreeSet<_> =
            manifest.excluded.iter().map(|e| e.path.as_str()).collect();
        if let Ok(names) = &ignored {
            let omitted_now: BTreeSet<_> = names
                .iter()
                .map(String::as_str)
                .filter(|p| !captured.contains(p))
                .collect();
            if manifest.git.is_some() && omitted_now != expected_omissions {
                blockers.push("ignored_path_coverage_changed_since_checkpoint".into());
            }
        }
        let observed = match &ignored {
            Ok(names) => fsx::inventory_filtered(source, |path| {
                !manifest
                    .excluded
                    .iter()
                    .any(|x| path == x.path || path.starts_with(&format!("{}/", x.path)))
                    && (!names.contains(path) || captured.contains(path))
            }),
            Err(error) => Err(anyhow::anyhow!(
                "cannot resolve current ignored-file coverage: {error:#}"
            )),
        };
        let matches_files = observed.as_ref().is_ok_and(|entries| {
            let mut filtered: Vec<_> = entries
                .iter()
                .filter(|e| {
                    !manifest
                        .excluded
                        .iter()
                        .any(|x| e.path == x.path || e.path.starts_with(&format!("{}/", x.path)))
                })
                .cloned()
                .collect();
            let mut expected = manifest.entries.clone();
            filtered.sort_by(|a, b| a.path.cmp(&b.path));
            expected.sort_by(|a, b| a.path.cmp(&b.path));
            filtered == expected
        });
        if !matches_files {
            blockers.push(match observed {
                Err(error) => format!("source_unavailable_or_unsupported: {error:#}"),
                Ok(_) => "source_contents_changed_since_checkpoint".into(),
            });
        }
        if let Some(state) = &manifest.git {
            if let Err(error) = crate::capture::validate_worktree(source) {
                blockers.push(format!(
                    "source_state_unsupported_or_unavailable: {error:#}"
                ));
            }
            if git::text(source, &["symbolic-ref", "-q", "HEAD"]).ok() != state.branch {
                blockers.push("source_branch_binding_changed_or_unavailable".into());
            }
            if git::text(source, &["rev-parse", "HEAD"]).as_ref().ok() != Some(&state.head) {
                blockers.push("source_HEAD_changed_or_unavailable".into());
            }
            let mut expected: Vec<_> = state
                .index
                .iter()
                .map(|i| (i.path.clone(), i.mode.clone(), i.oid.clone()))
                .collect();
            expected.sort();
            if current_index(source).as_ref().ok() != Some(&expected) {
                blockers.push("source_index_changed_or_unavailable".into());
            }
        } else {
            blockers.push("historical_worktree_completeness_unknown".into());
            if source.join(".git").exists() {
                blockers.push("folder_source_now_contains_git_metadata".into());
            }
        }
        if !manifest.excluded.is_empty() {
            blockers.push("excluded_paths_are_not_preserved".into());
        }
        if !alternates(&manifest).is_empty() {
            blockers.push("staged_and_working_alternates_require_review".into());
        }
        if manifest.import_map.as_ref().is_some_and(|m| {
            m.git_dependencies.iter().any(|d| {
                !manifest
                    .captured_dependencies
                    .iter()
                    .any(|c| c.dependency.group == d.group && c.dependency.oid == d.oid)
            })
        }) {
            blockers.push("declared_git_dependencies_not_reconstructed".into());
        }
        blockers.push("human_review_required_before_cleanup".into());
        rows.push(json!({ "checkpoint": manifest.id, "source_path": manifest.source_path, "source_matches_captured_files": matches_files, "verification": verify.as_ref().ok(), "blockers": blockers, "writer_exclusion_enforced": false, "independent_backup": "unassessed" }));
    }
    Ok(
        json!({ "schema": SCHEMA, "kind": "assessment", "status": "review_required", "sources": rows, "notes": ["This assessment does not apply changes or authorize cleanup.", "Current source identity and captured content are checked separately from the historical preview. No independent backup or exclusive writer control is established."] }),
    )
}
