use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const SCHEMA: u32 = 1;
/// Schema of `inspect` results. Separate from [`SCHEMA`], which governs stored
/// manifests, plans and receipts and is unchanged by output-shape revisions.
/// Version 2: each scanned worktree's `ignored` is a bounded summary object
/// (`count`, `bytes`, `top_dirs`) unless `scope.list_ignored` is true, in which
/// case it is the complete per-path list that version 1 always carried.
pub const INSPECT_SCHEMA: u32 = 2;
/// `Exclusion::reason` for a path the operator named with `checkpoint --exclude`.
/// Declared per run; never remembered or inferred.
pub const EXCLUDED_BY_OPERATOR: &str = "excluded_by_operator";
/// `Exclusion::reason` for a path listed in the repository's `timber.toml`
/// `exclude` key and applied as if passed with `checkpoint --exclude`. Kept distinct
/// from [`EXCLUDED_BY_OPERATOR`] so a reviewer can tell team convention from a
/// per-run operator choice.
pub const EXCLUDED_BY_CONVENTION_FILE: &str = "excluded_by_convention_file";
/// `Exclusion::reason` for a folder below the worktree that Git registered as a linked
/// worktree of this same repository. Checkpoint skips it automatically; it is
/// captured by checkpointing that worktree on its own.
pub const LINKED_WORKTREE_CAPTURED_SEPARATELY: &str = "linked_worktree_captured_separately";
/// True for exclusion reasons that name a declared folder skipped as a whole (operator
/// exclusions, convention-file exclusions and linked worktrees), as opposed to
/// ignored-file omissions.
pub fn is_skipped_folder(reason: &str) -> bool {
    reason == EXCLUDED_BY_OPERATOR
        || reason == EXCLUDED_BY_CONVENTION_FILE
        || reason == LINKED_WORKTREE_CAPTURED_SEPARATELY
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Symlink,
    Directory,
    /// A gitlink (index mode 160000). The path is recorded with empty `sha256`, zero
    /// `mode` and zero `size`; its commit ids live in [`GitState::submodules`]. The
    /// submodule's files are never captured, and restore recreates an empty directory.
    Submodule,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub kind: EntryKind,
    pub mode: u32,
    pub size: u64,
    pub sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexEntry {
    pub path: String,
    pub mode: String,
    pub oid: String,
    pub blob: String,
}
/// One recorded submodule pointer. Contents are never captured.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubmoduleRecord {
    pub path: String,
    /// Commit id of the gitlink in the superproject index.
    pub gitlink_commit: String,
    /// Commit id the submodule's HEAD resolved to at capture, or `None` when the
    /// submodule working directory was an empty, uninitialized directory.
    pub checked_out_commit: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitState {
    pub head: String,
    pub branch: Option<String>,
    pub object_format: String,
    /// Logical stage-zero index for regular files and symbolic links. Gitlinks are
    /// recorded in `submodules` instead, because they have no captured blob.
    pub index: Vec<IndexEntry>,
    pub bundle: String,
    pub protected_roots: Vec<String>,
    /// Gitlinks recorded as pointers, one per `EntryKind::Submodule` entry. Defaults to
    /// empty and is omitted when empty, so manifests without submodules keep the same
    /// bytes and identity as before this field existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub submodules: Vec<SubmoduleRecord>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Exclusion {
    pub path: String,
    pub reason: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureConditions {
    pub scans_matched: bool,
    pub writers_paused_declared: bool,
    pub writer_exclusion_enforced: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportFile {
    pub stored_path: String,
    pub target_path: String,
    #[serde(default)]
    pub base_sha256: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitDependency {
    pub group: String,
    pub oid: String,
    #[serde(default)]
    pub reference: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_head: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracked_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub expected_files: BTreeMap<String, ExpectedFile>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExpectedFile {
    pub sha256: String,
    pub bytes: u64,
    pub mode: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapturedDependency {
    pub dependency: GitDependency,
    pub bundle: String,
    pub protected_roots: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportMap {
    pub schema: u32,
    #[serde(default)]
    pub files: Vec<ImportFile>,
    #[serde(default)]
    pub git_dependencies: Vec<GitDependency>,
    #[serde(default)]
    pub groups: BTreeMap<String, String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub id: String,
    pub source_kind: String,
    pub source_path: String,
    pub created_unix: u64,
    pub entries: Vec<Entry>,
    pub git: Option<GitState>,
    pub excluded: Vec<Exclusion>,
    pub capture: CaptureConditions,
    pub import_map: Option<ImportMap>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub captured_dependencies: Vec<CapturedDependency>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    Committed,
    Index,
    #[default]
    Working,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Selection {
    pub checkpoint: String,
    #[serde(default)]
    pub layer: Layer,
    #[serde(default)]
    pub extra_paths: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comparison {
    pub checkpoint: String,
    pub already_in_target_history: bool,
    pub unique_commits: Vec<String>,
    pub identical_paths: Vec<String>,
    pub differing_paths: Vec<String>,
    pub staged_alternates: Vec<String>,
    pub notes: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub schema: u32,
    pub target: Selection,
    pub sources: Vec<Selection>,
    pub comparisons: Vec<Comparison>,
    pub notes: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreviewResult {
    pub schema: u32,
    pub status: String,
    pub destination: String,
    pub plan_sha256: String,
    pub completed_sources: Vec<String>,
    pub pending_sources: Vec<String>,
    pub head: String,
    pub conflict_tree: Option<String>,
    pub conflict_records: Vec<serde_json::Value>,
    pub notes: Vec<String>,
    pub protected_roots: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verification {
    pub schema: u32,
    pub checkpoint: String,
    pub manifest_sha256: String,
    pub verifier_version: String,
    pub verified: bool,
    pub historical_completeness: String,
    pub independent_backup: String,
    /// Paths the operator excluded at capture; absent from the checkpoint and any restore.
    /// Defaults to empty so verification records written before this field still load.
    #[serde(default)]
    pub excluded_by_operator: Vec<String>,
    /// Paths excluded at capture because the repository's `timber.toml` listed them.
    /// Absent from the checkpoint and any restore. Omitted when empty, so records
    /// without convention exclusions keep their earlier shape.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_by_convention_file: Vec<String>,
    /// Linked worktrees of this repository found below the captured worktree and skipped
    /// automatically; each is captured by checkpointing it on its own. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub linked_worktrees_captured_separately: Vec<String>,
    /// Submodule pointers recorded by the checkpoint. Their contents were not captured;
    /// restore recreates each path as an empty directory. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub submodules_recorded_not_captured: Vec<SubmoduleRecord>,
    pub notes: Vec<String>,
}
