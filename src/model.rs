use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const SCHEMA: u32 = 1;
/// `Exclusion::reason` for a path the operator named with `checkpoint --exclude`.
/// Declared per run; never remembered or inferred.
pub const EXCLUDED_BY_OPERATOR: &str = "excluded_by_operator";
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Symlink,
    Directory,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitState {
    pub head: String,
    pub branch: Option<String>,
    pub object_format: String,
    pub index: Vec<IndexEntry>,
    pub bundle: String,
    pub protected_roots: Vec<String>,
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
    pub notes: Vec<String>,
}
