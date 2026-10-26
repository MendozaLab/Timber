use std::{collections::BTreeMap, fs, path::PathBuf};
use timber_worktrees::{capture, dependencies, fsx, git, model::*, restore, store::Store};

struct Fixture {
    root: tempfile::TempDir,
    repo: PathBuf,
    folder: PathBuf,
    store: Store,
    dependency: GitDependency,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("source");
        fs::create_dir(&repo).unwrap();
        git::init(&repo).unwrap();
        git::run(&repo, &["config", "user.name", "Synthetic Developer"]).unwrap();
        git::run(&repo, &["config", "user.email", "fixture@example.invalid"]).unwrap();
        fs::write(repo.join("draft.txt"), "base\n").unwrap();
        git::run(&repo, &["add", "draft.txt"]).unwrap();
        git::run(&repo, &["commit", "-qm", "base"]).unwrap();
        let head = git::text(&repo, &["rev-parse", "HEAD"]).unwrap();
        fs::write(repo.join("draft.txt"), "staged\n").unwrap();
        git::run(&repo, &["add", "draft.txt"]).unwrap();
        fs::write(repo.join("draft.txt"), "working\n").unwrap();
        let oid = git::text(&repo, &["stash", "create", "synthetic saved state"]).unwrap();
        git::run(&repo, &["update-ref", "refs/heads/saved-agent", &oid]).unwrap();
        let folder = root.path().join("preserved");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("loose.txt"), "loose\n").unwrap();
        let expected = ExpectedFile {
            sha256: fsx::digest(b"working\n"),
            bytes: 8,
            mode: 0o644,
            kind: "file".into(),
        };
        let dependency = GitDependency {
            group: "agent-a".into(),
            oid,
            reference: Some("refs/heads/saved-agent".into()),
            kind: "stash".into(),
            source_head: Some(head),
            tracked_paths: vec!["draft.txt".into()],
            expected_files: BTreeMap::from([("draft.txt".into(), expected)]),
        };
        let store = Store::open(&root.path().join("store")).unwrap();
        Self {
            root,
            repo,
            folder,
            store,
            dependency,
        }
    }
    fn map(&self) -> ImportMap {
        ImportMap {
            schema: SCHEMA,
            files: vec![],
            git_dependencies: vec![self.dependency.clone()],
            groups: BTreeMap::new(),
        }
    }
    fn capture(&self) -> Manifest {
        capture::import_folder_with_dependencies(
            &self.store,
            &self.folder,
            Some(self.map()),
            false,
            Some(&self.repo),
        )
        .unwrap()
    }
}

#[test]
fn pinned_stash_captures_and_restores_independent_layers_without_source_writes() {
    let f = Fixture::new();
    let before = fsx::inventory(&f.repo).unwrap();
    let index = fs::read(f.repo.join(".git/index")).unwrap();
    let refs = git::run(&f.repo, &["show-ref"]).unwrap();
    let config = fs::read(f.repo.join(".git/config")).unwrap();
    let manifest = f.capture();
    assert_eq!(manifest.captured_dependencies.len(), 1);
    assert_eq!(before, fsx::inventory(&f.repo).unwrap());
    assert_eq!(index, fs::read(f.repo.join(".git/index")).unwrap());
    assert_eq!(refs, git::run(&f.repo, &["show-ref"]).unwrap());
    assert_eq!(config, fs::read(f.repo.join(".git/config")).unwrap());
    let receipt = restore::verify(&f.store, &manifest.id).unwrap();
    assert!(receipt.verified);
    assert_eq!(receipt.historical_completeness, "unknown");
    assert!(
        !receipt
            .notes
            .iter()
            .any(|n| n.contains("remain unresolved"))
    );
    let to = f.root.path().join("recovered");
    assert!(
        restore::restore_dependency(&f.store, &manifest.id, "agent-a", &to)
            .unwrap()
            .verified
    );
    assert_eq!(fs::read(to.join("draft.txt")).unwrap(), b"working\n");
    assert_eq!(git::run(&to, &["show", ":draft.txt"]).unwrap(), b"staged\n");
    assert_eq!(
        git::run(&to, &["show", "HEAD:draft.txt"]).unwrap(),
        b"base\n"
    );
    assert!(!to.join(".git/objects/info/alternates").exists());
    assert!(git::text(&to, &["remote"]).unwrap().is_empty());
}

#[test]
fn dependency_verification_works_without_original_repository_or_reference() {
    let f = Fixture::new();
    let manifest = f.capture();
    fs::rename(&f.repo, f.root.path().join("offline-source")).unwrap();
    assert!(restore::verify(&f.store, &manifest.id).unwrap().verified);
    let to = f.root.path().join("restored");
    assert!(
        restore::restore_dependency(&f.store, &manifest.id, "agent-a", &to)
            .unwrap()
            .verified
    );
    assert_eq!(fs::read(to.join("draft.txt")).unwrap(), b"working\n");
}

#[test]
fn moved_reference_and_expected_content_mismatch_refuse_completed_import() {
    let f = Fixture::new();
    let head = git::text(&f.repo, &["rev-parse", "HEAD"]).unwrap();
    git::run(&f.repo, &["update-ref", "refs/heads/saved-agent", &head]).unwrap();
    assert!(
        capture::import_folder_with_dependencies(
            &f.store,
            &f.folder,
            Some(f.map()),
            false,
            Some(&f.repo)
        )
        .is_err()
    );
    assert!(f.store.list().unwrap().is_empty());
    assert!(
        fs::read_dir(f.store.root.join("incomplete"))
            .unwrap()
            .count()
            > 0
    );
    git::run(
        &f.repo,
        &["update-ref", "refs/heads/saved-agent", &f.dependency.oid],
    )
    .unwrap();
    let mut map = f.map();
    map.git_dependencies[0]
        .expected_files
        .get_mut("draft.txt")
        .unwrap()
        .sha256 = fsx::digest(b"wrong");
    assert!(
        capture::import_folder_with_dependencies(
            &f.store,
            &f.folder,
            Some(map),
            false,
            Some(&f.repo)
        )
        .is_err()
    );
}

#[test]
fn missing_object_wrong_source_head_and_corrupt_bundle_fail() {
    let f = Fixture::new();
    let mut map = f.map();
    map.git_dependencies[0].oid = "f".repeat(40);
    map.git_dependencies[0].reference = None;
    assert!(
        capture::import_folder_with_dependencies(
            &f.store,
            &f.folder,
            Some(map),
            false,
            Some(&f.repo)
        )
        .is_err()
    );
    let mut map = f.map();
    map.git_dependencies[0].source_head = Some(f.dependency.oid.clone());
    assert!(
        capture::import_folder_with_dependencies(
            &f.store,
            &f.folder,
            Some(map),
            false,
            Some(&f.repo)
        )
        .is_err()
    );
    let manifest = f.capture();
    let bundle = &manifest.captured_dependencies[0].bundle;
    fs::write(f.store.root.join("objects").join(bundle), b"broken").unwrap();
    assert!(restore::verify(&f.store, &manifest.id).is_err());
    let dest = f.root.path().join("not-created");
    assert!(restore::restore_dependency(&f.store, &manifest.id, "agent-a", &dest).is_err());
    assert!(!dest.exists());
}

#[test]
fn implicit_folder_import_keeps_dependencies_unresolved_and_schema_compatible() {
    let f = Fixture::new();
    let original: GitDependency = serde_json::from_value(
        serde_json::json!({"group":"old","oid":f.dependency.oid,"reference":null}),
    )
    .unwrap();
    let encoded = serde_json::to_value(&original).unwrap();
    assert_eq!(encoded.as_object().unwrap().len(), 3);
    let manifest = capture::import_folder(&f.store, &f.folder, Some(f.map()), false).unwrap();
    assert!(manifest.captured_dependencies.is_empty());
    let value = serde_json::to_value(&manifest).unwrap();
    assert!(value.get("captured_dependencies").is_none());
    let reloaded: Manifest = serde_json::from_value(value).unwrap();
    assert_eq!(
        timber_worktrees::store::manifest_id(&manifest).unwrap(),
        timber_worktrees::store::manifest_id(&reloaded).unwrap()
    );
    assert!(
        restore::verify(&f.store, &manifest.id)
            .unwrap()
            .notes
            .iter()
            .any(|n| n.contains("remain unresolved"))
    );
    assert!(
        restore::restore_dependency(
            &f.store,
            &manifest.id,
            "agent-a",
            &f.root.path().join("missing")
        )
        .is_err()
    );
}

#[test]
fn recovery_refuses_all_source_and_shared_git_destinations() {
    let f = Fixture::new();
    let manifest = f.capture();
    for destination in [
        f.repo.join("bad"),
        f.repo.join(".git/bad"),
        f.folder.join("bad"),
        f.store.root.join("bad"),
    ] {
        assert!(
            restore::restore_dependency(&f.store, &manifest.id, "agent-a", &destination).is_err()
        );
        assert!(!destination.exists());
    }
}

#[test]
fn third_stash_parent_restores_untracked_files() {
    let mut f = Fixture::new();
    fs::write(f.repo.join("extra.txt"), b"untracked\n").unwrap();
    git::run(&f.repo, &["stash", "push", "-u", "-m", "three layers"]).unwrap();
    f.dependency.oid = git::text(&f.repo, &["rev-parse", "refs/stash"]).unwrap();
    f.dependency.reference = Some("refs/stash".into());
    let manifest = f.capture();
    let to = f.root.path().join("three");
    restore::restore_dependency(&f.store, &manifest.id, "agent-a", &to).unwrap();
    assert_eq!(fs::read(to.join("extra.txt")).unwrap(), b"untracked\n");
    assert!(!git::text(&to, &["ls-files"]).unwrap().contains("extra.txt"));
}

#[test]
fn declaration_rejects_path_escape_and_duplicate_groups() {
    let f = Fixture::new();
    let mut bad = f.dependency.clone();
    bad.tracked_paths = vec!["../escape".into()];
    assert!(dependencies::validate_declaration(&bad).is_err());
    let mut map = f.map();
    map.git_dependencies.push(f.dependency.clone());
    assert!(timber_worktrees::store::validate_import_map(&map).is_err());
}

#[test]
fn unreferenced_pinned_object_exports_without_creating_a_source_reference() {
    let mut f = Fixture::new();
    git::run(&f.repo, &["update-ref", "-d", "refs/heads/saved-agent"]).unwrap();
    f.dependency.reference = None;
    let refs = git::run(&f.repo, &["show-ref"]).unwrap();
    let m = f.capture();
    assert_eq!(refs, git::run(&f.repo, &["show-ref"]).unwrap());
    assert!(restore::verify(&f.store, &m.id).unwrap().verified);
}

#[test]
fn source_nested_dependency_store_is_refused_before_incomplete_marker() {
    let f = Fixture::new();
    let nested = Store::open(&f.repo.join("local-store")).unwrap();
    let before = fsx::inventory(&f.repo).unwrap();
    assert!(
        capture::import_folder_with_dependencies(
            &nested,
            &f.folder,
            Some(f.map()),
            false,
            Some(&f.repo)
        )
        .is_err()
    );
    assert_eq!(before, fsx::inventory(&f.repo).unwrap());
    assert!(
        fs::read_dir(nested.root.join("incomplete"))
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn many_committed_index_objects_and_new_staged_blob_roundtrip() {
    let f = Fixture::new();
    fs::create_dir(f.repo.join("many")).unwrap();
    for n in 0..256 {
        fs::write(
            f.repo.join(format!("many/{n:03}.txt")),
            format!("committed object {n}\n"),
        )
        .unwrap();
    }
    git::run(&f.repo, &["add", "many"]).unwrap();
    git::run(&f.repo, &["commit", "-qm", "many synthetic objects"]).unwrap();
    fs::write(f.repo.join("many/000.txt"), b"new staged object\n").unwrap();
    git::run(&f.repo, &["add", "many/000.txt"]).unwrap();
    fs::write(f.repo.join("many/000.txt"), b"separate working state\n").unwrap();
    let before = fs::read(f.repo.join(".git/index")).unwrap();
    let manifest = capture::checkpoint(&f.store, &f.repo, &[], false).unwrap();
    assert!(manifest.git.as_ref().unwrap().index.len() >= 257);
    let to = f.root.path().join("many-restored");
    assert!(
        restore::restore(&f.store, &manifest.id, &to)
            .unwrap()
            .verified
    );
    assert_eq!(
        git::run(&to, &["show", ":many/000.txt"]).unwrap(),
        b"new staged object\n"
    );
    assert_eq!(
        fs::read(to.join("many/000.txt")).unwrap(),
        b"separate working state\n"
    );
    for n in 1..256 {
        assert_eq!(
            fs::read_to_string(to.join(format!("many/{n:03}.txt"))).unwrap(),
            format!("committed object {n}\n")
        );
    }
    assert_eq!(before, fs::read(f.repo.join(".git/index")).unwrap());
}
