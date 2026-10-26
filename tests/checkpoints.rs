use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
};
use tempfile::TempDir;
use timber_worktrees::{
    capture, fsx, git,
    model::{EntryKind, ImportMap, SCHEMA},
    restore,
    store::Store,
};

struct Fixture {
    _dir: TempDir,
    repo: PathBuf,
    store: Store,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir(&repo).unwrap();
        git::init(&repo).unwrap();
        fs::write(repo.join("tracked.txt"), "base\n").unwrap();
        fs::write(repo.join(".gitignore"), ".secret\ncache/\n").unwrap();
        git::run(&repo, &["add", "."]).unwrap();
        git::run(
            &repo,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-m",
                "base",
            ],
        )
        .unwrap();
        let store = Store::open(&dir.path().join("store")).unwrap();
        Self {
            _dir: dir,
            repo,
            store,
        }
    }
    fn root(&self) -> &Path {
        self._dir.path()
    }
}

#[test]
fn preserves_staged_working_untracked_modes_symlinks_without_source_writes() {
    let f = Fixture::new();
    fs::write(f.repo.join("tracked.txt"), "staged\n").unwrap();
    git::run(&f.repo, &["add", "tracked.txt"]).unwrap();
    fs::write(f.repo.join("tracked.txt"), "working\n").unwrap();
    fs::write(f.repo.join("untracked.txt"), "new\n").unwrap();
    fs::write(f.repo.join(".secret"), "private\n").unwrap();
    fs::write(f.repo.join("run.sh"), "echo fixture\n").unwrap();
    fs::set_permissions(f.repo.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    symlink("tracked.txt", f.repo.join("link")).unwrap();
    let before = fs::read(f.repo.join(".git/index")).unwrap();
    let m = capture::checkpoint(&f.store, &f.repo, &[], false).unwrap();
    assert_eq!(before, fs::read(f.repo.join(".git/index")).unwrap());
    assert_eq!(m.excluded.len(), 1);
    assert!(!m.entries.iter().any(|e| e.path == ".secret"));
    let stage = m
        .git
        .as_ref()
        .unwrap()
        .index
        .iter()
        .find(|e| e.path == "tracked.txt")
        .unwrap();
    assert_eq!(f.store.get(&stage.blob).unwrap(), b"staged\n");
    let working = m.entries.iter().find(|e| e.path == "tracked.txt").unwrap();
    assert_eq!(f.store.get(&working.sha256).unwrap(), b"working\n");
    let destination = f.root().join("restored");
    assert!(
        restore::restore(&f.store, &m.id, &destination)
            .unwrap()
            .verified
    );
    assert_eq!(
        fs::read(destination.join("tracked.txt")).unwrap(),
        b"working\n"
    );
    assert_eq!(
        fs::read_link(destination.join("link")).unwrap(),
        Path::new("tracked.txt")
    );
    assert_ne!(
        fs::metadata(destination.join("run.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0
    );
    assert_eq!(before, fs::read(f.repo.join(".git/index")).unwrap());
}

#[test]
fn verifies_after_original_repository_is_unavailable() {
    let f = Fixture::new();
    let m = capture::checkpoint(&f.store, &f.repo, &[], true).unwrap();
    fs::rename(&f.repo, f.root().join("unavailable")).unwrap();
    let v = restore::verify(&f.store, &m.id).unwrap();
    assert!(v.verified);
    assert_eq!(v.independent_backup, "unassessed");
    assert!(!m.capture.writer_exclusion_enforced);
}

#[test]
fn explicit_ignored_selection_and_corrupt_object_detection() {
    let f = Fixture::new();
    fs::write(f.repo.join(".secret"), b"selected local payload").unwrap();
    let m = capture::checkpoint(&f.store, &f.repo, &[".secret".into()], false).unwrap();
    assert!(m.excluded.is_empty());
    let entry = m.entries.iter().find(|e| e.path == ".secret").unwrap();
    fs::write(f.store.root.join("objects").join(&entry.sha256), b"corrupt").unwrap();
    assert!(restore::verify(&f.store, &m.id).is_err());
}

#[test]
fn folder_import_preserves_present_bytes_without_claiming_history() {
    let f = Fixture::new();
    let folder = f.root().join("old copy");
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("draft.txt"), b"payload").unwrap();
    fs::write(folder.join("receipt.json"), br#"{"verified":true}"#).unwrap();
    let map = ImportMap {
        schema: SCHEMA,
        files: vec![],
        git_dependencies: vec![],
        groups: Default::default(),
    };
    let m = capture::import_folder(&f.store, &folder, Some(map), false).unwrap();
    assert!(m.git.is_none());
    let v = restore::verify(&f.store, &m.id).unwrap();
    assert!(v.verified);
    assert_eq!(v.historical_completeness, "unknown");
}

#[test]
fn rejects_index_flags_and_restoring_over_an_existing_destination() {
    let f = Fixture::new();
    let m = capture::checkpoint(&f.store, &f.repo, &[], false).unwrap();
    assert!(restore::restore(&f.store, &m.id, &f.repo).is_err());
    assert!(restore::restore(&f.store, &m.id, f.root()).is_err());
    git::run(
        &f.repo,
        &["update-index", "--assume-unchanged", "tracked.txt"],
    )
    .unwrap();
    assert!(capture::checkpoint(&f.store, &f.repo, &[], false).is_err());
}

#[test]
fn rejects_manifest_escape_and_symlink_store_objects() {
    let f = Fixture::new();
    let mut m = capture::checkpoint(&f.store, &f.repo, &[], false).unwrap();
    m.entries[0].path = "../escape".into();
    m.id = timber_worktrees::store::manifest_id(&m).unwrap();
    assert!(f.store.publish(&m).is_err());
    let digest = fsx::digest(b"never read external");
    symlink(
        f.repo.join("tracked.txt"),
        f.store.root.join("objects").join(&digest),
    )
    .unwrap();
    assert!(f.store.get(&digest).is_err());
}

#[test]
fn deleted_tracked_files_and_empty_directories_roundtrip() {
    let f = Fixture::new();
    fs::remove_file(f.repo.join("tracked.txt")).unwrap();
    fs::create_dir(f.repo.join("empty")).unwrap();
    let m = capture::checkpoint(&f.store, &f.repo, &[], false).unwrap();
    assert!(!m.entries.iter().any(|e| e.path == "tracked.txt"));
    assert!(
        m.entries
            .iter()
            .any(|e| e.path == "empty" && e.kind == EntryKind::Directory)
    );
    assert!(restore::verify(&f.store, &m.id).unwrap().verified);
}

#[test]
fn failed_capture_retains_incomplete_marker_and_publishes_no_manifest() {
    let f = Fixture::new();
    let index = fs::read(f.repo.join(".git/index")).unwrap();
    let result = capture::checkpoint(&f.store, &f.repo, &["not-an-ignored-path".into()], false);
    assert!(result.is_err());
    assert!(f.store.list().unwrap().is_empty());
    let markers: Vec<_> = fs::read_dir(f.store.root.join("incomplete"))
        .unwrap()
        .collect();
    assert_eq!(markers.len(), 1);
    assert_eq!(index, fs::read(f.repo.join(".git/index")).unwrap());
}

#[test]
fn refuses_symlinked_content_store_directory() {
    let f = Fixture::new();
    let digest = f.store.put(b"payload").unwrap();
    let original = f.store.root.join("original-objects");
    fs::rename(f.store.root.join("objects"), &original).unwrap();
    symlink(&original, f.store.root.join("objects")).unwrap();
    assert!(f.store.get(&digest).is_err());
    assert!(f.store.put(b"other").is_err());
}

#[test]
fn refuses_unresolved_index_and_submodule_entries() {
    let f = Fixture::new();
    let index = capture::read_index(&f.repo).unwrap();
    let oid = &index.iter().find(|e| e.0 == "tracked.txt").unwrap().2;
    let info = format!(
        "0 {}\ttracked.txt\0\u{31}00644 {} 1\ttracked.txt\0",
        "0".repeat(40),
        oid
    );
    git::run_input(
        &f.repo,
        &["update-index", "-z", "--index-info"],
        info.as_bytes(),
    )
    .unwrap();
    assert!(capture::checkpoint(&f.store, &f.repo, &[], false).is_err());
    let g = Fixture::new();
    let head = git::text(&g.repo, &["rev-parse", "HEAD"]).unwrap();
    git::run(
        &g.repo,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{head},submodule"),
        ],
    )
    .unwrap();
    assert!(capture::checkpoint(&g.store, &g.repo, &[], false).is_err());
}

#[test]
fn refuses_sparse_shallow_partial_and_active_git_operations() {
    let f = Fixture::new();
    git::run(&f.repo, &["config", "core.sparseCheckout", "true"]).unwrap();
    assert!(capture::checkpoint(&f.store, &f.repo, &[], false).is_err());
    git::run(&f.repo, &["config", "--unset", "core.sparseCheckout"]).unwrap();
    let head = git::text(&f.repo, &["rev-parse", "HEAD"]).unwrap();
    fs::write(f.repo.join(".git/shallow"), format!("{head}\n")).unwrap();
    assert!(capture::checkpoint(&f.store, &f.repo, &[], false).is_err());
    fs::remove_file(f.repo.join(".git/shallow")).unwrap();
    git::run(&f.repo, &["config", "extensions.partialClone", "origin"]).unwrap();
    assert!(capture::checkpoint(&f.store, &f.repo, &[], false).is_err());
    git::run(&f.repo, &["config", "--unset", "extensions.partialClone"]).unwrap();
    fs::write(f.repo.join(".git/MERGE_HEAD"), format!("{head}\n")).unwrap();
    assert!(capture::checkpoint(&f.store, &f.repo, &[], false).is_err());
}

#[test]
fn materialization_cannot_follow_an_existing_parent_symlink() {
    let f = Fixture::new();
    let root = f.root().join("materialization");
    let outside = f.root().join("outside");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&outside).unwrap();
    symlink(&outside, root.join("parent")).unwrap();
    let bytes = b"must not escape";
    let entries = vec![timber_worktrees::model::Entry {
        path: "parent/payload".into(),
        kind: EntryKind::File,
        mode: 0o644,
        size: bytes.len() as u64,
        sha256: fsx::digest(bytes),
    }];
    assert!(fsx::materialize(&root, &entries, |_| Ok(bytes.to_vec())).is_err());
    assert!(!outside.join("payload").exists());
}

#[test]
fn existing_store_open_never_creates_a_missing_store() {
    let f = Fixture::new();
    let missing = f.repo.join("must-not-be-created");
    assert!(Store::existing(&missing).is_err());
    assert!(!missing.exists());
    assert_eq!(Store::existing(&f.store.root).unwrap().root, f.store.root);
}

#[test]
fn linked_worktree_capture_protects_shared_git_and_sibling_roots() {
    let f = Fixture::new();
    let linked = f.root().join("linked");
    git::run(
        &f.repo,
        &["worktree", "add", "-b", "agent", linked.to_str().unwrap()],
    )
    .unwrap();
    let m = capture::checkpoint(&f.store, &linked, &[], false).unwrap();
    let roots = &m.git.as_ref().unwrap().protected_roots;
    assert!(
        roots
            .iter()
            .any(|p| Path::new(p) == f.repo.canonicalize().unwrap())
    );
    assert!(
        roots
            .iter()
            .any(|p| Path::new(p) == f.repo.join(".git").canonicalize().unwrap())
    );
    let sibling_output = f.repo.join("must-not-be-created");
    assert!(restore::restore(&f.store, &m.id, &sibling_output).is_err());
    assert!(!sibling_output.exists());
    let metadata_output = f.repo.join(".git/must-not-be-created");
    assert!(restore::restore(&f.store, &m.id, &metadata_output).is_err());
    assert!(!metadata_output.exists());
    // The test deliberately creates an invalid store location before invoking the
    // core API; CLI preflight must refuse before this creation step.
    let bad_store = Store::open(&f.repo.join(".git/bad-store")).unwrap();
    assert!(capture::checkpoint(&bad_store, &linked, &[], false).is_err());
    assert!(bad_store.list().unwrap().is_empty());
    assert_eq!(
        fs::read_dir(bad_store.root.join("incomplete"))
            .unwrap()
            .count(),
        0
    );
    // Persisted roots remain protective even if their source directory disappears.
    fs::rename(&linked, f.root().join("unavailable-linked")).unwrap();
    assert!(restore::verify(&f.store, &m.id).unwrap().verified);
    assert!(restore::restore(&f.store, &m.id, &sibling_output).is_err());
}

#[test]
fn manifest_protected_roots_must_include_the_absolute_source() {
    let f = Fixture::new();
    let mut m = capture::checkpoint(&f.store, &f.repo, &[], false).unwrap();
    m.git.as_mut().unwrap().protected_roots = vec!["relative-root".into()];
    m.id = timber_worktrees::store::manifest_id(&m).unwrap();
    assert!(f.store.publish(&m).is_err());
    m.git.as_mut().unwrap().protected_roots = vec![f.store.root.to_str().unwrap().into()];
    m.id = timber_worktrees::store::manifest_id(&m).unwrap();
    assert!(f.store.publish(&m).is_err());
}
