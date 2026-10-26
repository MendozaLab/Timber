use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use timber_worktrees::{capture, fsx, git, model::*, planning, preview, store::Store};

fn g(path: &Path, args: &[&str]) -> String {
    git::text(path, args).unwrap()
}
fn init(path: &Path) {
    fs::create_dir(path).unwrap();
    git::init(path).unwrap();
    g(path, &["config", "user.name", "Fixture Developer"]);
    g(path, &["config", "user.email", "fixture@example.invalid"]);
    fs::write(path.join("base.txt"), "base\n").unwrap();
    g(path, &["add", "base.txt"]);
    g(path, &["commit", "-qm", "base"]);
}
fn worktree(repo: &Path, to: &Path, branch: &str) {
    g(
        repo,
        &["worktree", "add", "-q", "-b", branch, to.to_str().unwrap()],
    );
}
fn working(cp: &Manifest) -> Selection {
    Selection {
        checkpoint: cp.id.clone(),
        layer: Layer::Working,
        extra_paths: Vec::new(),
    }
}
fn checkpoint(store: &Store, path: &Path) -> Manifest {
    capture::checkpoint(store, path, &[], true).unwrap()
}
fn setup() -> (tempfile::TempDir, PathBuf, Store) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    init(&repo);
    let store = Store::open(&dir.path().join("store")).unwrap();
    (dir, repo, store)
}
#[test]
fn dirty_target_and_two_sources_integrate_without_source_changes() {
    let (dir, repo, store) = setup();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    worktree(&repo, &a, "agent-a");
    worktree(&repo, &b, "agent-b");
    fs::write(repo.join("base.txt"), "dirty target\n").unwrap();
    fs::write(a.join("a.txt"), "agent a\n").unwrap();
    g(&a, &["add", "a.txt"]);
    fs::write(b.join("b.txt"), "agent b\n").unwrap();
    g(&b, &["add", "b.txt"]);
    let before = [
        fsx::inventory(&repo).unwrap(),
        fsx::inventory(&a).unwrap(),
        fsx::inventory(&b).unwrap(),
    ];
    let target = checkpoint(&store, &repo);
    let ca = checkpoint(&store, &a);
    let cb = checkpoint(&store, &b);
    let plan =
        planning::create(&store, working(&target), vec![working(&ca), working(&cb)]).unwrap();
    let result = preview::build(&store, &plan, &dir.path().join("preview")).unwrap();
    assert_eq!(result.status, "complete");
    assert_eq!(result.completed_sources.len(), 2);
    let p = dir.path().join("preview");
    assert_eq!(
        fs::read_to_string(p.join("base.txt")).unwrap(),
        "dirty target\n"
    );
    assert_eq!(fs::read_to_string(p.join("a.txt")).unwrap(), "agent a\n");
    assert_eq!(fs::read_to_string(p.join("b.txt")).unwrap(), "agent b\n");
    assert_eq!(
        before,
        [
            fsx::inventory(&repo).unwrap(),
            fsx::inventory(&a).unwrap(),
            fsx::inventory(&b).unwrap()
        ]
    );
    assert!(!p.join(".git/objects/info/alternates").exists());
    assert!(g(&p, &["remote"]).is_empty());
}
#[test]
fn selected_working_layer_retains_staged_alternate() {
    let (dir, repo, store) = setup();
    let a = dir.path().join("a");
    worktree(&repo, &a, "agent");
    let target = checkpoint(&store, &repo);
    fs::write(a.join("base.txt"), "staged\n").unwrap();
    g(&a, &["add", "base.txt"]);
    fs::write(a.join("base.txt"), "working\n").unwrap();
    let source = checkpoint(&store, &a);
    let plan = planning::create(&store, working(&target), vec![working(&source)]).unwrap();
    assert_eq!(plan.comparisons[0].staged_alternates, vec!["base.txt"]);
    preview::build(&store, &plan, &dir.path().join("working-preview")).unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join("working-preview/base.txt")).unwrap(),
        "working\n"
    );
    let mut index_plan = plan.clone();
    index_plan.sources[0].layer = Layer::Index;
    preview::build(&store, &index_plan, &dir.path().join("index-preview")).unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join("index-preview/base.txt")).unwrap(),
        "staged\n"
    );
}
#[test]
fn conflict_preserves_last_clean_target_and_reports_remaining_sources() {
    let (dir, repo, store) = setup();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    worktree(&repo, &a, "agent-a");
    worktree(&repo, &b, "agent-b");
    fs::write(repo.join("base.txt"), "target change\n").unwrap();
    fs::write(a.join("base.txt"), "source change\n").unwrap();
    let target = checkpoint(&store, &repo);
    let source = checkpoint(&store, &a);
    let pending = checkpoint(&store, &b);
    let plan = planning::create(
        &store,
        working(&target),
        vec![working(&source), working(&pending)],
    )
    .unwrap();
    let result = preview::build(&store, &plan, &dir.path().join("preview")).unwrap();
    assert_eq!(result.status, "conflicted");
    assert_eq!(result.pending_sources.len(), 2);
    assert!(result.completed_sources.is_empty());
    assert!(result.conflict_tree.is_some());
    assert!(!result.conflict_records.is_empty());
    assert_eq!(
        fs::read_to_string(dir.path().join("preview/base.txt")).unwrap(),
        "target change\n"
    );
}
#[test]
fn binary_conflict_is_not_mistaken_for_a_clean_merge() {
    let (dir, repo, store) = setup();
    fs::write(repo.join("binary"), b"\0base").unwrap();
    g(&repo, &["add", "binary"]);
    g(&repo, &["commit", "-qm", "binary"]);
    let a = dir.path().join("a");
    worktree(&repo, &a, "agent");
    fs::write(repo.join("binary"), b"\0target").unwrap();
    fs::write(a.join("binary"), b"\0source").unwrap();
    let target = checkpoint(&store, &repo);
    let source = checkpoint(&store, &a);
    let plan = planning::create(&store, working(&target), vec![working(&source)]).unwrap();
    let result = preview::build(&store, &plan, &dir.path().join("preview")).unwrap();
    assert_eq!(result.status, "conflicted");
    assert!(
        result
            .conflict_records
            .iter()
            .any(|r| r.to_string().contains("binary"))
    );
}
#[test]
fn original_repositories_can_be_unavailable() {
    let (dir, repo, store) = setup();
    let a = dir.path().join("a");
    worktree(&repo, &a, "agent");
    let target = checkpoint(&store, &repo);
    fs::write(a.join("base.txt"), "unfinished\n").unwrap();
    let source = checkpoint(&store, &a);
    let plan = planning::create(&store, working(&target), vec![working(&source)]).unwrap();
    fs::rename(&repo, dir.path().join("unavailable")).unwrap();
    fs::rename(&a, dir.path().join("unavailable-a")).unwrap();
    let result = preview::build(&store, &plan, &dir.path().join("preview")).unwrap();
    assert_eq!(result.status, "complete");
    assert_eq!(
        fs::read_to_string(dir.path().join("preview/base.txt")).unwrap(),
        "unfinished\n"
    );
}
#[test]
fn source_history_is_exact_ancestry_and_current_drift_is_separate() {
    let (dir, repo, store) = setup();
    let target = checkpoint(&store, &repo);
    let source = checkpoint(&store, &repo);
    let plan = planning::create(&store, working(&target), vec![working(&source)]).unwrap();
    assert!(plan.comparisons[0].already_in_target_history);
    fs::write(repo.join("base.txt"), "later\n").unwrap();
    let assessment = planning::assess(&store, &plan).unwrap();
    assert!(
        assessment
            .to_string()
            .contains("source_contents_changed_since_checkpoint")
    );
    preview::build(&store, &plan, &dir.path().join("preview")).unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join("preview/base.txt")).unwrap(),
        "base\n"
    );
}
#[test]
fn folder_mapping_adds_new_files_but_never_overwrites_differing_target() {
    let (dir, repo, store) = setup();
    let target = checkpoint(&store, &repo);
    let folder = dir.path().join("folder");
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("saved"), "saved content\n").unwrap();
    let map = ImportMap {
        schema: SCHEMA,
        files: vec![ImportFile {
            stored_path: "saved".into(),
            target_path: "new.txt".into(),
            base_sha256: None,
        }],
        git_dependencies: Vec::new(),
        groups: BTreeMap::new(),
    };
    let source = capture::import_folder(&store, &folder, Some(map.clone()), true).unwrap();
    let plan = planning::create(&store, working(&target), vec![working(&source)]).unwrap();
    let result = preview::build(&store, &plan, &dir.path().join("preview")).unwrap();
    assert_eq!(result.status, "complete");
    assert_eq!(
        fs::read_to_string(dir.path().join("preview/new.txt")).unwrap(),
        "saved content\n"
    );
    let mut collision_map = map;
    collision_map.files[0].target_path = "base.txt".into();
    let collision = capture::import_folder(&store, &folder, Some(collision_map), true).unwrap();
    let plan = planning::create(&store, working(&target), vec![working(&collision)]).unwrap();
    let result = preview::build(&store, &plan, &dir.path().join("collision")).unwrap();
    assert_eq!(result.status, "conflicted");
    assert_eq!(
        fs::read_to_string(dir.path().join("collision/base.txt")).unwrap(),
        "base\n"
    );
}
#[test]
fn explicit_extra_file_selection_does_not_silently_include_other_files() {
    let (dir, repo, store) = setup();
    let target = checkpoint(&store, &repo);
    fs::write(repo.join("wanted"), "selected").unwrap();
    fs::write(repo.join("other"), "retained only").unwrap();
    let source = checkpoint(&store, &repo);
    let mut selected = working(&source);
    selected.extra_paths.push("wanted".into());
    let plan = planning::create(&store, working(&target), vec![selected]).unwrap();
    preview::build(&store, &plan, &dir.path().join("preview")).unwrap();
    assert!(dir.path().join("preview/wanted").exists());
    assert!(!dir.path().join("preview/other").exists());
}
#[test]
fn union_attributes_block_preview_without_running_helpers() {
    let (dir, repo, store) = setup();
    let target = checkpoint(&store, &repo);
    fs::write(repo.join(".gitattributes"), "base.txt merge=union\n").unwrap();
    g(&repo, &["add", ".gitattributes"]);
    let source = checkpoint(&store, &repo);
    let plan = planning::create(&store, working(&target), vec![working(&source)]).unwrap();
    let error = preview::build(&store, &plan, &dir.path().join("preview")).unwrap_err();
    assert!(error.to_string().contains("union"));
}

#[test]
fn assessment_never_reads_omitted_or_new_ignored_content() {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, repo, store) = setup();
    fs::write(repo.join(".gitignore"), "*.secret\n").unwrap();
    g(&repo, &["add", ".gitignore"]);
    fs::write(repo.join("old.secret"), "omitted bytes").unwrap();
    let target = checkpoint(&store, &repo);
    let plan = planning::create(&store, working(&target), Vec::new()).unwrap();
    fs::set_permissions(repo.join("old.secret"), fs::Permissions::from_mode(0o0)).unwrap();
    // If assess opens ignored bytes it fails for ordinary unprivileged users.
    let assessment = planning::assess(&store, &plan).unwrap();
    assert_eq!(
        assessment["sources"][0]["source_matches_captured_files"],
        true
    );
    fs::write(repo.join("new.secret"), vec![0u8; 1024 * 1024]).unwrap();
    fs::set_permissions(repo.join("new.secret"), fs::Permissions::from_mode(0o0)).unwrap();
    let assessment = planning::assess(&store, &plan).unwrap();
    assert_eq!(
        assessment["sources"][0]["source_matches_captured_files"],
        true
    );
    assert!(
        assessment
            .to_string()
            .contains("ignored_path_coverage_changed_since_checkpoint")
    );
    fs::set_permissions(repo.join("old.secret"), fs::Permissions::from_mode(0o600)).unwrap();
    fs::set_permissions(repo.join("new.secret"), fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn preview_refuses_other_registered_worktrees_and_shared_git_metadata() {
    let (dir, repo, store) = setup();
    let other = dir.path().join("other");
    worktree(&repo, &other, "other");
    let target = checkpoint(&store, &repo);
    let plan = planning::create(&store, working(&target), Vec::new()).unwrap();
    for destination in [
        other.join("nested-preview"),
        repo.join(".git/nested-preview"),
    ] {
        assert!(preview::build(&store, &plan, &destination).is_err());
        assert!(!destination.exists());
    }
}
