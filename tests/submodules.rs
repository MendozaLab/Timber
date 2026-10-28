//! Submodules: checkpoint records each gitlink as a pointer and never captures the
//! submodule's files; restore recreates an empty path; comparison treats the
//! pointer as a scalar; an unreadable gitlink is still refused by path.
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use tempfile::TempDir;
use timber_worktrees::{
    capture, git, inspect,
    model::{EntryKind, Layer, Manifest, Selection, SubmoduleRecord, Verification},
    planning, preview, restore,
    store::Store,
};

const SUBMODULE: &str = "vendor/lib";

struct Fixture {
    dir: TempDir,
    app: PathBuf,
    store: Store,
}

fn identity(repo: &Path) {
    git::run(repo, &["config", "user.name", "Fixture Developer"]).unwrap();
    git::run(repo, &["config", "user.email", "fixture@example.invalid"]).unwrap();
}

fn commit(repo: &Path, message: &str) -> String {
    git::run(repo, &["commit", "-qm", message]).unwrap();
    git::text(repo, &["rev-parse", "HEAD"]).unwrap()
}

impl Fixture {
    /// Two synthetic repositories: `library` is added to `app` as the submodule
    /// `vendor/lib`. The app then carries a working change and an untracked note
    /// beside the submodule.
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("library");
        fs::create_dir(&library).unwrap();
        git::init(&library).unwrap();
        identity(&library);
        fs::write(library.join("lib.txt"), "invented library content\n").unwrap();
        git::run(&library, &["add", "."]).unwrap();
        commit(&library, "library base");
        let app = dir.path().join("app");
        fs::create_dir(&app).unwrap();
        git::init(&app).unwrap();
        identity(&app);
        fs::write(app.join("app.txt"), "invented app content\n").unwrap();
        git::run(&app, &["add", "."]).unwrap();
        commit(&app, "app base");
        git::run(
            &app,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                library.to_str().unwrap(),
                SUBMODULE,
            ],
        )
        .unwrap();
        commit(&app, "add submodule");
        fs::write(app.join("app.txt"), "working change beside the submodule\n").unwrap();
        fs::write(app.join("notes.txt"), "untracked note\n").unwrap();
        let store = Store::open(&dir.path().join("store")).unwrap();
        Self { dir, app, store }
    }
    fn submodule(&self) -> PathBuf {
        self.app.join(SUBMODULE)
    }
    fn gitlink(&self) -> String {
        let row = git::text(&self.app, &["ls-files", "--stage", SUBMODULE]).unwrap();
        let fields = row.split_whitespace().collect::<Vec<_>>();
        assert_eq!(fields[0], "160000", "{row}");
        fields[1].to_owned()
    }
    fn submodule_head(&self) -> String {
        git::text(&self.submodule(), &["rev-parse", "HEAD"]).unwrap()
    }
    /// Commits a new invented file inside the submodule without staging the pointer.
    fn commit_in_submodule(&self, name: &str) -> String {
        let sub = self.submodule();
        identity(&sub);
        fs::write(sub.join(name), format!("invented {name}\n")).unwrap();
        git::run(&sub, &["add", name]).unwrap();
        commit(&sub, name)
    }
    /// Commits inside the submodule and stages the new gitlink in the app.
    fn bump_submodule(&self, name: &str) -> String {
        let head = self.commit_in_submodule(name);
        git::run(&self.app, &["add", SUBMODULE]).unwrap();
        head
    }
}

fn selection(id: &str, layer: Layer) -> Selection {
    Selection {
        checkpoint: id.to_owned(),
        layer,
        extra_paths: Vec::new(),
    }
}

fn record(manifest: &Manifest) -> &SubmoduleRecord {
    let records = &manifest.git.as_ref().unwrap().submodules;
    assert_eq!(records.len(), 1, "{records:?}");
    &records[0]
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_timber"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn checkpoint_records_gitlink_ids_and_omits_submodule_files() {
    let f = Fixture::new();
    let gitlink = f.gitlink();
    let head = f.submodule_head();
    let index_before = fs::read(f.app.join(".git/index")).unwrap();
    let m = capture::checkpoint(&f.store, &f.app, &[], false).unwrap();
    assert_eq!(index_before, fs::read(f.app.join(".git/index")).unwrap());
    let entry = m.entries.iter().find(|e| e.path == SUBMODULE).unwrap();
    assert_eq!(entry.kind, EntryKind::Submodule);
    assert!(entry.sha256.is_empty());
    assert!(
        !m.entries
            .iter()
            .any(|e| e.path.starts_with(&format!("{SUBMODULE}/"))),
        "submodule files must never be captured"
    );
    let r = record(&m);
    assert_eq!(r.path, SUBMODULE);
    assert_eq!(r.gitlink_commit, gitlink);
    assert_eq!(r.checked_out_commit.as_deref(), Some(head.as_str()));
    let state = m.git.as_ref().unwrap();
    assert!(!state.index.iter().any(|i| i.path == SUBMODULE));
    assert!(state.index.iter().any(|i| i.path == ".gitmodules"));
    let working = m.entries.iter().find(|e| e.path == "app.txt").unwrap();
    assert_eq!(
        f.store.get(&working.sha256).unwrap(),
        b"working change beside the submodule\n"
    );
    assert!(m.entries.iter().any(|e| e.path == "notes.txt"));
    assert_eq!(f.store.load(&m.id).unwrap().id, m.id);
}

#[test]
fn restore_recreates_empty_submodule_path_and_reports_ids() {
    let f = Fixture::new();
    let gitlink = f.gitlink();
    let head = f.submodule_head();
    let m = capture::checkpoint(&f.store, &f.app, &[], false).unwrap();
    let destination = f.dir.path().join("restored");
    let v = restore::restore(&f.store, &m.id, &destination).unwrap();
    assert!(v.verified);
    let restored = destination.join(SUBMODULE);
    assert!(restored.is_dir());
    assert_eq!(fs::read_dir(&restored).unwrap().count(), 0);
    let staged = git::text(&destination, &["ls-files", "--stage", SUBMODULE]).unwrap();
    assert!(
        staged.starts_with(&format!("160000 {gitlink} 0")),
        "{staged}"
    );
    assert_eq!(v.submodules_recorded_not_captured.len(), 1);
    let r = &v.submodules_recorded_not_captured[0];
    assert_eq!(r.gitlink_commit, gitlink);
    assert_eq!(r.checked_out_commit.as_deref(), Some(head.as_str()));
    assert!(
        v.notes
            .iter()
            .any(|n| n.contains(SUBMODULE) && n.contains("contents not captured")),
        "{:?}",
        v.notes
    );
    // Restoration never fetched or initialized the submodule.
    assert!(!destination.join(".git/modules").exists());
}

#[test]
fn verify_prints_each_submodule_with_both_ids() {
    let f = Fixture::new();
    let staged = f.bump_submodule("second.txt");
    let checked_out = f.commit_in_submodule("third.txt");
    assert_eq!(f.gitlink(), staged);
    assert_ne!(staged, checked_out);
    let store = f.dir.path().join("cli-store");
    let s = store.to_str().unwrap();
    let out = run(&[
        "--store",
        s,
        "--json",
        "checkpoint",
        f.app.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let id = v["result"]["checkpoint"]["id"].as_str().unwrap();
    let text = run(&["--store", s, "verify", id]);
    assert!(
        text.status.success(),
        "{}",
        String::from_utf8_lossy(&text.stderr)
    );
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("Submodules recorded not captured"), "{text}");
    assert!(text.contains(SUBMODULE), "{text}");
    assert!(text.contains(&staged), "{text}");
    assert!(text.contains(&checked_out), "{text}");
    assert!(text.contains("contents were not captured"), "{text}");
    let json = run(&["--store", s, "--json", "verify", id]);
    let json: Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(
        json["result"]["submodules_recorded_not_captured"],
        json!([{"path":SUBMODULE,"gitlink_commit":staged,"checked_out_commit":checked_out}])
    );
}

#[test]
fn plan_compares_gitlinks_as_scalars_and_preview_stops_on_conflicting_pointers() {
    let f = Fixture::new();
    let base = capture::checkpoint(&f.store, &f.app, &[], false).unwrap();
    let first = f.bump_submodule("first.txt");
    let a = capture::checkpoint(&f.store, &f.app, &[], false).unwrap();
    let second = f.bump_submodule("second.txt");
    let b = capture::checkpoint(&f.store, &f.app, &[], false).unwrap();
    assert_ne!(first, second);

    // Different gitlink ids at the same path are a competing version, by pointer.
    let plan = planning::create(
        &f.store,
        selection(&a.id, Layer::Index),
        vec![selection(&b.id, Layer::Index)],
    )
    .unwrap();
    let row = &plan.comparisons[0];
    assert!(
        row.differing_paths.iter().any(|p| p == SUBMODULE),
        "{row:?}"
    );
    assert!(
        row.notes.iter().any(|n| n.contains(SUBMODULE)
            && n.contains("different at the same path")
            && n.contains(&first)
            && n.contains(&second)),
        "{:?}",
        row.notes
    );
    assert!(row.identical_paths.iter().any(|p| p == "app.txt"));

    // Assessment rechecks the current pointer and names the uncaptured contents.
    let assessment = planning::assess(&f.store, &plan).unwrap();
    let current = &assessment["sources"][1]["blockers"];
    assert!(
        current
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b == "submodule_contents_not_captured"),
        "{current}"
    );
    for drift in [
        "source_contents_changed_since_checkpoint",
        "source_index_changed_or_unavailable",
        "submodule_checkout_changed_or_unavailable",
    ] {
        assert!(
            !current.as_array().unwrap().iter().any(|b| b == drift),
            "{current}"
        );
    }
    let earlier = &assessment["sources"][0]["blockers"];
    assert!(
        earlier
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b == "source_index_changed_or_unavailable"),
        "{earlier}"
    );

    // A one-sided pointer change merges without touching submodule contents.
    let one_sided = planning::create(
        &f.store,
        selection(&base.id, Layer::Index),
        vec![selection(&a.id, Layer::Index)],
    )
    .unwrap();
    let clean_to = f.dir.path().join("preview-clean");
    let clean = preview::build(&f.store, &one_sided, &clean_to).unwrap();
    assert_eq!(clean.status, "complete");
    let tree = git::text(&clean_to, &["ls-tree", "HEAD", SUBMODULE]).unwrap();
    assert!(
        tree.starts_with(&format!("160000 commit {first}")),
        "{tree}"
    );
    assert_eq!(fs::read_dir(clean_to.join(SUBMODULE)).unwrap().count(), 0);

    // Both sides moved the pointer to different commits: a conflict naming the path.
    let to = f.dir.path().join("preview-conflict");
    let conflicted = preview::build(&f.store, &plan, &to).unwrap();
    assert_eq!(conflicted.status, "conflicted");
    let record = &conflicted.conflict_records[0];
    assert_eq!(record["kind"], "submodule_gitlink");
    assert_eq!(record["path"], SUBMODULE);
    assert_eq!(record["preview_commit"], first.as_str());
    assert_eq!(record["source_commit"], second.as_str());
    assert!(
        record["message"]
            .as_str()
            .unwrap()
            .contains("different at the same path")
    );
    assert!(!to.join(".git/modules").exists());
}

#[test]
fn unreadable_gitlink_is_still_refused_naming_the_path() {
    let f = Fixture::new();
    // A `.git` that points nowhere: the submodule HEAD cannot be read.
    fs::write(f.submodule().join(".git"), "gitdir: ../../missing-module\n").unwrap();
    let error = format!(
        "{:#}",
        capture::checkpoint(&f.store, &f.app, &[], false).unwrap_err()
    );
    assert!(error.contains(SUBMODULE), "{error}");
    assert!(f.store.list().unwrap().is_empty());

    // Files without any `.git` at a gitlink path cannot be recorded as a pointer.
    fs::remove_file(f.submodule().join(".git")).unwrap();
    let error = format!(
        "{:#}",
        capture::checkpoint(&f.store, &f.app, &[], false).unwrap_err()
    );
    assert!(error.contains(SUBMODULE), "{error}");
    assert!(error.contains("not empty"), "{error}");

    // A gitlink whose path is missing remains refused.
    fs::remove_dir_all(f.submodule()).unwrap();
    let error = format!(
        "{:#}",
        capture::checkpoint(&f.store, &f.app, &[], false).unwrap_err()
    );
    assert!(error.contains(SUBMODULE), "{error}");
    assert!(f.store.list().unwrap().is_empty());
}

#[test]
fn uninitialized_empty_submodule_records_only_the_gitlink() {
    let f = Fixture::new();
    let gitlink = f.gitlink();
    fs::remove_dir_all(f.submodule()).unwrap();
    fs::create_dir(f.submodule()).unwrap();
    let m = capture::checkpoint(&f.store, &f.app, &[], false).unwrap();
    let r = record(&m);
    assert_eq!(r.gitlink_commit, gitlink);
    assert_eq!(r.checked_out_commit, None);
    let v = restore::verify(&f.store, &m.id).unwrap();
    assert!(v.verified);
    assert!(
        v.notes
            .iter()
            .any(|n| n.contains("not initialized at capture")),
        "{:?}",
        v.notes
    );
}

#[test]
fn inspect_reports_submodule_as_recorded_not_captured() {
    let f = Fixture::new();
    // An untracked nested repository elsewhere keeps its own blocker.
    let other = f.app.join("scratch/other");
    fs::create_dir_all(&other).unwrap();
    git::init(&other).unwrap();
    let result = inspect::inspect(&f.app, &[]).unwrap();
    let row = &result["worktrees"][0];
    assert_eq!(
        row["submodules_recorded_not_captured"],
        json!([{"path":SUBMODULE,"gitlink_commit":f.gitlink(),"checked_out_commit":f.submodule_head()}])
    );
    let blockers = row["blockers"].as_array().unwrap();
    assert!(
        !blockers
            .iter()
            .any(|b| b.as_str().is_some_and(|s| s.contains("submodule"))),
        "{blockers:?}"
    );
    let nested = blockers
        .iter()
        .find(|b| b["kind"] == "nested_repositories")
        .unwrap();
    assert_eq!(
        nested["repositories"],
        json!([{"path":"scratch/other","tracking":"untracked"}])
    );
    assert!(
        !row["unknown_working_paths"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p == SUBMODULE)
    );

    // Without the nested repository, the worktree scans cleanly.
    fs::remove_dir_all(f.app.join("scratch")).unwrap();
    let clean = inspect::inspect(&f.app, &[]).unwrap();
    assert_eq!(clean["worktrees"][0]["inspection_status"], "scanned");
    assert_eq!(clean["status"], "complete");

    // An unreadable gitlink remains a blocker and leaves the scan partial.
    fs::write(f.submodule().join(".git"), "gitdir: ../../missing-module\n").unwrap();
    let broken = inspect::inspect(&f.app, &[]).unwrap();
    let row = &broken["worktrees"][0];
    assert_eq!(row["inspection_status"], "partial");
    assert!(
        row["blockers"].as_array().unwrap().iter().any(|b| b
            .as_str()
            .is_some_and(|s| s.starts_with(&format!("submodule unreadable: {SUBMODULE}")))),
        "{:?}",
        row["blockers"]
    );
}

#[test]
fn manifests_and_verifications_without_submodules_keep_their_shape() {
    let f = Fixture::new();
    let plain = f.dir.path().join("plain");
    fs::create_dir(&plain).unwrap();
    git::init(&plain).unwrap();
    identity(&plain);
    fs::write(plain.join("only.txt"), "invented\n").unwrap();
    git::run(&plain, &["add", "."]).unwrap();
    commit(&plain, "plain");
    let m = capture::checkpoint(&f.store, &plain, &[], false).unwrap();
    let stored = fs::read_to_string(
        f.store
            .root
            .join("checkpoints")
            .join(format!("{}.json", m.id)),
    )
    .unwrap();
    assert!(!stored.contains("submodule"), "{stored}");
    let v = restore::verify(&f.store, &m.id).unwrap();
    assert!(
        !serde_json::to_string(&v)
            .unwrap()
            .contains("submodules_recorded_not_captured")
    );
    let older = json!({
        "schema":1,"checkpoint":"0".repeat(64),"manifest_sha256":"0".repeat(64),
        "verifier_version":"0.1.0","verified":true,
        "historical_completeness":"captured_supported_state_only",
        "independent_backup":"unassessed","notes":[]
    });
    let parsed: Verification = serde_json::from_value(older).unwrap();
    assert!(parsed.submodules_recorded_not_captured.is_empty());
}

/// A `git` on PATH that sleeps before listing other files, then runs the real Git.
fn slow_git(dir: &Path) -> String {
    let original = std::env::var("PATH").unwrap();
    let real = original
        .split(':')
        .map(|d| Path::new(d).join("git"))
        .find(|p| p.is_file())
        .unwrap();
    let bin = dir.join("slow-bin");
    fs::create_dir(&bin).unwrap();
    let script = bin.join("git");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nfor arg in \"$@\"; do\n  if [ \"$arg\" = \"--others\" ]; then sleep 5; break; fi\ndone\nexec '{}' \"$@\"\n",
            real.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    format!("{}:{original}", bin.display())
}

#[test]
fn git_timeout_flag_names_the_operation_and_leaves_the_result_partial() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    fs::create_dir(&repo).unwrap();
    git::init(&repo).unwrap();
    identity(&repo);
    fs::write(repo.join("tracked.txt"), "invented\n").unwrap();
    git::run(&repo, &["add", "."]).unwrap();
    commit(&repo, "base");
    fs::write(repo.join("tracked.txt"), "working change\n").unwrap();
    let index = fs::read(repo.join(".git/index")).unwrap();
    let path = slow_git(dir.path());
    let r = repo.to_str().unwrap();
    let started = std::time::Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_timber"))
        .env("PATH", &path)
        .args(["--git-timeout", "1", "--json", "inspect", "--repo", r])
        .output()
        .unwrap();
    assert!(started.elapsed().as_secs() < 5, "{:?}", started.elapsed());
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["result"]["status"], "partial");
    let row = &v["result"]["worktrees"][0];
    assert_eq!(row["inspection_status"], "failed");
    let blocker = row["blockers"][0].as_str().unwrap();
    assert!(blocker.contains("ls-files --others"), "{blocker}");
    assert!(blocker.contains("1-second limit"), "{blocker}");
    assert!(blocker.contains("--git-timeout 1"), "{blocker}");
    assert!(blocker.contains("source was not changed"), "{blocker}");
    assert_eq!(index, fs::read(repo.join(".git/index")).unwrap());
    // The default limit lets the same slow operations finish.
    let out = Command::new(env!("CARGO_BIN_EXE_timber"))
        .env("PATH", &path)
        .args(["--json", "inspect", "--repo", r])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["result"]["status"], "complete");
    // A zero limit is refused by the argument parser.
    let out = run(&["--git-timeout", "0", "inspect", "--repo", r]);
    assert!(!out.status.success());
}
