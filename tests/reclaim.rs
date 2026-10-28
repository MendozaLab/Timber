//! Reclamation of incomplete captures: journals, `gc --dry-run` and `gc`.
//! Synthetic fixtures only. Reachability from completed manifests governs deletion.
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use tempfile::TempDir;
use timber_worktrees::{
    capture, fsx,
    gc::{self, JournalState, Reclamation},
    git,
    model::*,
    restore,
    store::{Store, referenced_objects},
};

struct Fixture {
    _dir: TempDir,
    repo: PathBuf,
    folder: PathBuf,
    store: Store,
    dependency: GitDependency,
}

impl Fixture {
    /// A repository with staged and working changes plus a pinned stash reference,
    /// and a recovery folder that can declare that stash as a Git dependency.
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
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
        let folder = dir.path().join("preserved");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("loose.txt"), "loose\n").unwrap();
        let dependency = GitDependency {
            group: "agent-a".into(),
            oid,
            reference: Some("refs/heads/saved-agent".into()),
            kind: "stash".into(),
            source_head: Some(head),
            tracked_paths: vec!["draft.txt".into()],
            expected_files: BTreeMap::from([(
                "draft.txt".into(),
                ExpectedFile {
                    sha256: fsx::digest(b"working\n"),
                    bytes: 8,
                    mode: 0o644,
                    kind: "file".into(),
                },
            )]),
        };
        let store = Store::open(&dir.path().join("store")).unwrap();
        Self {
            _dir: dir,
            repo,
            folder,
            store,
            dependency,
        }
    }

    /// An import that stores the dependency bundle, then is refused because a mapped
    /// file is missing: an aborted run that really added objects to the store.
    fn refused_import(&self) -> anyhow::Error {
        let map = ImportMap {
            schema: SCHEMA,
            files: vec![ImportFile {
                stored_path: "missing.txt".into(),
                target_path: "missing.txt".into(),
                base_sha256: None,
            }],
            git_dependencies: vec![self.dependency.clone()],
            groups: BTreeMap::new(),
        };
        capture::import_folder_with_dependencies(
            &self.store,
            &self.folder,
            Some(map),
            false,
            Some(&self.repo),
        )
        .unwrap_err()
    }

    fn objects(&self) -> BTreeMap<String, u64> {
        fs::read_dir(self.store.root.join("objects"))
            .unwrap()
            .map(|e| e.unwrap())
            .map(|e| {
                (
                    e.file_name().into_string().unwrap(),
                    e.metadata().unwrap().len(),
                )
            })
            .collect()
    }

    fn markers(&self) -> Vec<PathBuf> {
        let mut found = fs::read_dir(self.store.root.join("incomplete"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("capture-"))
            })
            .collect::<Vec<_>>();
        found.sort();
        found
    }

    fn referenced(&self) -> BTreeSet<String> {
        self.store
            .list()
            .unwrap()
            .iter()
            .flat_map(referenced_objects)
            .collect()
    }
}

/// Every path and content digest below the store, to prove a run wrote nothing.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, String>) {
        for item in fs::read_dir(dir).unwrap() {
            let path = item.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            let key = path.strip_prefix(root).unwrap().to_path_buf();
            if meta.is_dir() {
                out.insert(key, "directory".into());
                walk(root, &path, out);
            } else {
                out.insert(key, fsx::digest(&fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// Header and journaled object ids of one incomplete marker.
fn read_marker(path: &Path) -> (Value, Vec<String>) {
    let bytes = fs::read(path).unwrap();
    let mut values = serde_json::Deserializer::from_slice(&bytes).into_iter::<Value>();
    let header = values.next().unwrap().unwrap();
    let objects = values
        .map(|v| v.unwrap()["object"].as_str().unwrap().to_owned())
        .collect();
    (header, objects)
}

fn marker_id(path: &Path) -> String {
    path.file_stem().unwrap().to_str().unwrap().to_owned()
}

fn nested_repository(repo: &Path) {
    let nested = repo.join("scratch/clone");
    fs::create_dir_all(&nested).unwrap();
    git::init(&nested).unwrap();
    fs::write(nested.join("output.log"), "disposable output\n").unwrap();
}

#[test]
fn refused_checkpoint_keeps_journaled_marker_and_names_gc_dry_run() {
    let f = Fixture::new();
    nested_repository(&f.repo);
    let before = f.objects();
    let message = format!(
        "{:#}",
        capture::checkpoint(&f.store, &f.repo, &[], false).unwrap_err()
    );
    assert!(message.contains("nested repository refused"), "{message}");
    let markers = f.markers();
    assert_eq!(markers.len(), 1);
    let id = marker_id(&markers[0]);
    assert!(
        message.contains(&format!("incomplete marker {id}")),
        "{message}"
    );
    assert!(message.contains("holds 0 bytes in 0 objects"), "{message}");
    assert!(message.contains("`timber gc --dry-run`"), "{message}");
    assert!(message.contains("never deletes automatically"), "{message}");
    let (header, journal) = read_marker(&markers[0]);
    assert_eq!(header["journal"], 1);
    assert_eq!(header["complete"], false);
    assert!(journal.is_empty());
    assert_eq!(before, f.objects());
    assert!(f.store.list().unwrap().is_empty());

    let report = gc::dry_run(&f.store).unwrap();
    assert_eq!(report.incomplete_markers.len(), 1);
    let marker = &report.incomplete_markers[0];
    assert_eq!(marker.id, id);
    assert_eq!(marker.journal, JournalState::Present);
    assert_eq!(marker.journaled_objects, 0);
    assert!(!marker.in_progress);
    assert!(marker.age_seconds.is_some());
    // Timber never deletes on its own: the marker is still there.
    assert_eq!(f.markers().len(), 1);
}

#[test]
fn aborted_run_journals_exactly_the_objects_it_added() {
    let f = Fixture::new();
    // A completed checkpoint first, so some objects already exist in the store.
    capture::checkpoint(&f.store, &f.repo, &[], false).unwrap();
    for _ in 0..2 {
        let before = f.objects();
        let previous = f.markers();
        let message = format!("{:#}", f.refused_import());
        assert!(
            message.contains("mapped source file is missing"),
            "{message}"
        );
        let after = f.objects();
        let marker = f
            .markers()
            .into_iter()
            .find(|m| !previous.contains(m))
            .unwrap();
        let (header, journal) = read_marker(&marker);
        assert_eq!(header["operation"], "folder_import");
        let added = after
            .keys()
            .filter(|k| !before.contains_key(*k))
            .cloned()
            .collect::<BTreeSet<_>>();
        assert_eq!(journal.iter().cloned().collect::<BTreeSet<_>>(), added);
        assert!(journal.iter().all(|id| !before.contains_key(id)));
        let bytes: u64 = added.iter().map(|id| after[id]).sum();
        assert!(
            message.contains(&format!(
                "incomplete marker {} is kept in the store and holds {bytes} bytes in {} objects",
                marker_id(&marker),
                added.len()
            )),
            "{message}"
        );
        assert!(message.contains("`timber gc --dry-run`"), "{message}");
    }
    // The first refused run added at least the dependency bundle.
    let journals = f
        .markers()
        .iter()
        .map(|m| read_marker(m).1.len())
        .collect::<Vec<_>>();
    assert!(journals[0] + journals[1] >= 1, "{journals:?}");
}

/// A completed checkpoint, a real aborted run, a synthetic journaled marker naming
/// one shared and one orphan object, and one unattributed orphan.
fn mixed_store(f: &Fixture) -> (Manifest, String, String, String) {
    let completed = capture::checkpoint(&f.store, &f.repo, &[], false).unwrap();
    f.refused_import();
    let shared = completed
        .entries
        .iter()
        .find(|e| e.kind == EntryKind::File)
        .unwrap()
        .sha256
        .clone();
    let orphan = f
        .store
        .put(b"synthetic orphan from an aborted run\n")
        .unwrap();
    let unattributed = f.store.put(b"synthetic object no journal names\n").unwrap();
    fs::write(
        f.store.root.join("incomplete/capture-synthetic.json"),
        format!(
            "{}\n{{\"object\":\"{shared}\",\"bytes\":8}}\n{{\"object\":\"{orphan}\",\"bytes\":37}}\n",
            serde_json::json!({"schema":1,"operation":"checkpoint","source":"/synthetic/source","complete":false,"started_unix":1,"journal":1})
        ),
    )
    .unwrap();
    (completed, shared, orphan, unattributed)
}

#[test]
fn dry_run_reports_unreferenced_bytes_and_never_lists_referenced_objects() {
    let f = Fixture::new();
    let (completed, shared, orphan, unattributed) = mixed_store(&f);
    let referenced = f.referenced();
    assert!(referenced.contains(&shared));
    let before = snapshot(&f.store.root);
    let report = gc::dry_run(&f.store).unwrap();
    assert_eq!(
        before,
        snapshot(&f.store.root),
        "dry run wrote to the store"
    );
    assert_eq!(report.status, "dry_run");
    assert!(report.removed.is_none());
    assert_eq!(report.completed_checkpoints, vec![completed.id.clone()]);
    assert!(report.missing_references.is_empty());

    let objects = f.objects();
    let expected = objects
        .iter()
        .filter(|(id, _)| !referenced.contains(*id))
        .map(|(id, bytes)| (id.clone(), *bytes))
        .collect::<BTreeMap<_, _>>();
    let listed = report
        .unreferenced_objects
        .iter()
        .map(|o| (o.id.clone(), o.bytes))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(listed, expected);
    assert!(listed.keys().all(|id| !referenced.contains(id)));
    assert!(listed.contains_key(&orphan) && listed.contains_key(&unattributed));
    assert_eq!(
        report.unreferenced_object_bytes,
        expected.values().sum::<u64>()
    );
    let marker_bytes: u64 = f
        .markers()
        .iter()
        .map(|m| fs::metadata(m).unwrap().len())
        .sum();
    assert_eq!(
        report.reclaimable_bytes,
        expected.values().sum::<u64>() + marker_bytes
    );
    assert_eq!(report.kept.completed_checkpoints, 1);
    assert_eq!(report.kept.referenced_objects, referenced.len() as u64);
    assert_eq!(
        report.kept.referenced_bytes,
        referenced.iter().map(|id| objects[id]).sum::<u64>()
    );

    let synthetic = report
        .incomplete_markers
        .iter()
        .find(|m| m.id == "capture-synthetic")
        .unwrap();
    assert_eq!(synthetic.journal, JournalState::Present);
    assert_eq!(synthetic.journaled_objects, 2);
    assert_eq!(synthetic.journaled_kept_referenced, 1);
    assert_eq!(synthetic.journaled_reclaimable_objects, 1);
    assert_eq!(synthetic.journaled_reclaimable_bytes, objects[&orphan]);
    assert_eq!(
        synthetic.journaled_bytes,
        objects[&orphan] + objects[&shared]
    );
    let record = |id: &str| {
        report
            .unreferenced_objects
            .iter()
            .find(|o| o.id == id)
            .unwrap()
            .clone()
    };
    assert_eq!(record(&orphan).journaled_by, ["capture-synthetic"]);
    assert!(record(&unattributed).journaled_by.is_empty());
    assert_eq!(report.incomplete_markers.len(), 2);
}

#[test]
fn gc_removes_exactly_unreferenced_objects_and_completed_checkpoints_still_verify() {
    let f = Fixture::new();
    let (completed, ..) = mixed_store(&f);
    let imported = capture::import_folder(&f.store, &f.folder, None, false).unwrap();
    // Leftovers of interrupted writes, and an item Timber does not recognize.
    fs::write(f.store.root.join("objects/.tmpLEFTOVER"), b"partial").unwrap();
    fs::create_dir(f.store.root.join("incomplete/bundle-LEFTOVER")).unwrap();
    fs::write(
        f.store
            .root
            .join("incomplete/bundle-LEFTOVER/history.bundle"),
        b"partial bundle",
    )
    .unwrap();
    fs::write(
        f.store.root.join("incomplete/operator-note.txt"),
        b"keep me",
    )
    .unwrap();
    let referenced = f.referenced();
    let preview = gc::dry_run(&f.store).unwrap();
    assert_eq!(preview.stray_items.len(), 2);
    assert_eq!(preview.kept.unrecognized, ["incomplete/operator-note.txt"]);
    let checkpoints_before = snapshot(&f.store.root.join("checkpoints"));

    let report = gc::reclaim(&f.store).unwrap();
    assert_eq!(report.status, "reclaimed");
    let removed = report.removed.clone().unwrap();
    assert_eq!(removed.total_bytes, preview.reclaimable_bytes);
    assert_eq!(removed.objects, preview.unreferenced_objects.len() as u64);
    assert_eq!(removed.object_bytes, preview.unreferenced_object_bytes);
    assert_eq!(removed.markers.len(), 2);
    assert_eq!(removed.stray_items, 2);

    // Exactly the referenced objects remain; no marker or stray item remains.
    assert_eq!(f.objects().into_keys().collect::<BTreeSet<_>>(), referenced);
    assert!(f.markers().is_empty());
    assert!(!f.store.root.join("incomplete/bundle-LEFTOVER").exists());
    assert!(f.store.root.join("incomplete/operator-note.txt").exists());
    assert_eq!(
        checkpoints_before,
        snapshot(&f.store.root.join("checkpoints"))
    );
    for id in [&completed.id, &imported.id] {
        assert!(restore::verify(&f.store, id).unwrap().verified, "{id}");
    }
    // A second run has nothing left to reclaim but the unrecognized item it keeps.
    let again = gc::reclaim(&f.store).unwrap();
    assert_eq!(again.removed.unwrap().total_bytes, 0);
    assert_eq!(again.kept.completed_checkpoints, 2);
}

#[test]
fn gc_refuses_before_deleting_when_a_completed_manifest_lacks_an_object() {
    let f = Fixture::new();
    let (completed, shared, ..) = mixed_store(&f);
    fs::remove_file(f.store.root.join("objects").join(&shared)).unwrap();
    let before = snapshot(&f.store.root);

    let preview = gc::dry_run(&f.store).unwrap();
    assert_eq!(preview.status, "dry_run_would_refuse");
    assert_eq!(preview.missing_references.len(), 1);
    assert_eq!(preview.missing_references[0].checkpoint, completed.id);
    assert_eq!(preview.missing_references[0].object, shared);

    let message = format!("{:#}", gc::reclaim(&f.store).unwrap_err());
    assert!(message.contains("before removing anything"), "{message}");
    assert!(
        message.contains(&format!("completed checkpoint {}", completed.id)),
        "{message}"
    );
    assert!(message.contains(&shared), "{message}");
    assert_eq!(
        before,
        snapshot(&f.store.root),
        "refused gc removed something"
    );
}

#[test]
fn marker_without_journal_is_reported_absent_and_reachability_still_governs() {
    let f = Fixture::new();
    let completed = capture::checkpoint(&f.store, &f.repo, &[], false).unwrap();
    // An older Timber wrote this marker: pretty JSON header, no journal key.
    fs::write(
        f.store.root.join("incomplete/capture-legacy.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&serde_json::json!({"schema":1,"operation":"checkpoint","source":"/synthetic/legacy","complete":false,"started_unix":1}))
                .unwrap()
        ),
    )
    .unwrap();
    // Its run left one object a completed checkpoint also references, and one orphan.
    let shared = f.store.put(b"working\n").unwrap();
    let orphan = f.store.put(b"legacy orphan\n").unwrap();
    assert!(f.referenced().contains(&shared));

    let report = gc::dry_run(&f.store).unwrap();
    let legacy = &report.incomplete_markers[0];
    assert_eq!(legacy.id, "capture-legacy");
    assert_eq!(legacy.journal, JournalState::Absent);
    assert_eq!(legacy.journaled_objects, 0);
    assert!(report.notes.iter().any(|n| n.contains("journal absent")));
    let listed = report
        .unreferenced_objects
        .iter()
        .map(|o| o.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(listed, std::slice::from_ref(&orphan));
    assert_eq!(report.unattributed_objects, 1);

    gc::reclaim(&f.store).unwrap();
    assert!(f.store.root.join("objects").join(&shared).exists());
    assert!(!f.store.root.join("objects").join(&orphan).exists());
    assert!(f.markers().is_empty());
    assert!(restore::verify(&f.store, &completed.id).unwrap().verified);
}

#[test]
fn gc_and_capture_exclude_each_other() {
    let f = Fixture::new();
    f.refused_import();
    let before = snapshot(&f.store.root);
    {
        let _capture = f.store.capture_lock().unwrap();
        let message = format!("{:#}", gc::reclaim(&f.store).unwrap_err());
        assert!(
            message.contains("capture or another gc is in progress"),
            "{message}"
        );
        assert_eq!(before, snapshot(&f.store.root));
        // A dry run still reports while a capture runs.
        gc::dry_run(&f.store).unwrap();
    }
    {
        let _reclaiming = f.store.reclaim_lock().unwrap();
        let message = format!(
            "{:#}",
            capture::checkpoint(&f.store, &f.repo, &[], false).unwrap_err()
        );
        assert!(
            message.contains("being reclaimed by `timber gc`"),
            "{message}"
        );
        assert_eq!(
            before,
            snapshot(&f.store.root),
            "refused capture wrote a marker"
        );
    }
}

#[test]
fn malformed_journal_tail_is_reported_truncated() {
    let f = Fixture::new();
    let orphan = f.store.put(b"journaled before the crash\n").unwrap();
    fs::write(
        f.store.root.join("incomplete/capture-crashed.json"),
        format!(
            "{}\n{{\"object\":\"{orphan}\",\"bytes\":27}}\n{{\"obj",
            serde_json::json!({"schema":1,"operation":"checkpoint","source":"/synthetic/crashed","complete":false,"started_unix":1,"journal":1})
        ),
    )
    .unwrap();
    let report = gc::dry_run(&f.store).unwrap();
    let marker = &report.incomplete_markers[0];
    assert_eq!(marker.journal, JournalState::Truncated);
    assert_eq!(marker.journaled_objects, 1);
    assert_eq!(marker.journaled_reclaimable_objects, 1);
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_timber"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn cli_gc_takes_no_source_path_and_reports_then_reclaims() {
    let f = Fixture::new();
    let store = f.store.root.to_str().unwrap();
    nested_repository(&f.repo);
    let refused = run(&["--store", store, "checkpoint", f.repo.to_str().unwrap()]);
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("timber gc --dry-run"), "{stderr}");
    assert!(stderr.contains("incomplete marker capture-"), "{stderr}");
    fs::remove_dir_all(f.repo.join("scratch")).unwrap();
    let completed = capture::checkpoint(&f.store, &f.repo, &[], false).unwrap();
    f.refused_import();

    // No source path is accepted, so gc cannot be pointed at a worktree.
    let before = snapshot(&f.store.root);
    let repo_before = fsx::inventory(&f.repo).unwrap();
    let pointed = run(&["--store", store, "gc", f.repo.to_str().unwrap()]);
    assert!(!pointed.status.success());
    assert_eq!(before, snapshot(&f.store.root));

    let dry = run(&["--store", store, "--json", "gc", "--dry-run"]);
    assert!(
        dry.status.success(),
        "{}",
        String::from_utf8_lossy(&dry.stderr)
    );
    let envelope: Value = serde_json::from_slice(&dry.stdout).unwrap();
    assert_eq!(envelope["command"], "gc_dry_run");
    let preview: Reclamation = serde_json::from_value(envelope["result"].clone()).unwrap();
    assert_eq!(preview.incomplete_markers.len(), 2);
    assert_eq!(before, snapshot(&f.store.root));
    let text = run(&["--store", store, "gc", "--dry-run"]);
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(
        text.contains(&format!(
            "Would reclaim: {} bytes",
            preview.reclaimable_bytes
        )),
        "{text}"
    );

    let reclaimed = run(&["--store", store, "gc"]);
    assert!(
        reclaimed.status.success(),
        "{}",
        String::from_utf8_lossy(&reclaimed.stderr)
    );
    let text = String::from_utf8_lossy(&reclaimed.stdout);
    assert!(
        text.contains(&format!("Removed: {} bytes", preview.reclaimable_bytes)),
        "{text}"
    );
    assert!(text.contains("Kept: 1 completed checkpoints"), "{text}");
    assert!(f.markers().is_empty());
    assert_eq!(repo_before, fsx::inventory(&f.repo).unwrap());
    let verify = run(&["--store", store, "verify", &completed.id]);
    assert!(verify.status.success());
}
