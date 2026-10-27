//! Nested repositories: inspect names the blocker, checkpoint refuses it unless the
//! operator explicitly excludes a disposable untracked or ignored path for that run.
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::symlink,
    path::PathBuf,
    process::{Command, Output},
};
use tempfile::TempDir;
use timber_worktrees::{
    capture, git, inspect,
    model::{EXCLUDED_BY_OPERATOR, Layer, Selection, Verification},
    planning, report, restore,
    store::Store,
};

const MESSAGE: &str = "checkpoint will refuse this worktree until nested repositories are handled";

struct Fixture {
    dir: TempDir,
    repo: PathBuf,
    store: Store,
}
impl Fixture {
    /// A worktree with a modified tracked file, a staged file, an untracked keeper,
    /// and a disposable nested repository below the untracked `artifacts/` folder.
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir(&repo).unwrap();
        git::init(&repo).unwrap();
        git::run(&repo, &["config", "user.name", "Fixture Developer"]).unwrap();
        git::run(&repo, &["config", "user.email", "fixture@example.invalid"]).unwrap();
        fs::create_dir(repo.join("docs")).unwrap();
        fs::write(repo.join("docs/guide.md"), "base guide\n").unwrap();
        fs::write(repo.join("tracked.txt"), "base\n").unwrap();
        fs::write(repo.join(".gitignore"), "cache/\n").unwrap();
        git::run(&repo, &["add", "."]).unwrap();
        git::run(&repo, &["commit", "-qm", "base"]).unwrap();
        fs::write(repo.join("tracked.txt"), "working change\n").unwrap();
        fs::write(repo.join("staged.txt"), "staged addition\n").unwrap();
        git::run(&repo, &["add", "staged.txt"]).unwrap();
        fs::write(repo.join("keep.txt"), "untracked keeper\n").unwrap();
        let nested = repo.join("artifacts/run-1");
        fs::create_dir_all(&nested).unwrap();
        git::init(&nested).unwrap();
        fs::write(nested.join("output.log"), "disposable output\n").unwrap();
        fs::write(repo.join("artifacts/summary.txt"), "disposable summary\n").unwrap();
        let store = Store::open(&dir.path().join("store")).unwrap();
        Self { dir, repo, store }
    }
    fn incomplete_markers(&self) -> usize {
        fs::read_dir(self.store.root.join("incomplete"))
            .unwrap()
            .count()
    }
}

fn exclude(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|p| (*p).to_owned()).collect()
}

fn nested_blocker(result: &Value) -> Value {
    result["worktrees"][0]["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["kind"] == "nested_repositories")
        .cloned()
        .unwrap_or(Value::Null)
}

fn working(id: &str) -> Selection {
    Selection {
        checkpoint: id.to_owned(),
        layer: Layer::Working,
        extra_paths: Vec::new(),
    }
}

#[test]
fn nested_repository_under_untracked_folder_refuses_checkpoint_without_exclude() {
    let f = Fixture::new();
    let index = fs::read(f.repo.join(".git/index")).unwrap();
    let error = capture::checkpoint(&f.store, &f.repo, &[], false).unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("nested repository refused"), "{message}");
    assert!(message.contains("artifacts/run-1/.git"), "{message}");
    assert!(f.store.list().unwrap().is_empty());
    // An empty exclusion list is exactly the original behavior.
    assert!(capture::checkpoint_with_exclusions(&f.store, &f.repo, &[], &[], false).is_err());
    assert!(f.store.list().unwrap().is_empty());
    assert_eq!(index, fs::read(f.repo.join(".git/index")).unwrap());
}

#[test]
fn operator_exclusion_captures_around_nested_repository_and_is_reported() {
    let f = Fixture::new();
    let index = fs::read(f.repo.join(".git/index")).unwrap();
    let m = capture::checkpoint_with_exclusions(
        &f.store,
        &f.repo,
        &[],
        &exclude(&["artifacts/"]),
        false,
    )
    .unwrap();
    assert_eq!(index, fs::read(f.repo.join(".git/index")).unwrap());
    let operator = m
        .excluded
        .iter()
        .filter(|x| x.reason == EXCLUDED_BY_OPERATOR)
        .map(|x| x.path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(operator, ["artifacts"]);
    assert!(!m.entries.iter().any(|e| e.path.starts_with("artifacts")));
    for path in ["keep.txt", "staged.txt", "tracked.txt", "docs/guide.md"] {
        assert!(m.entries.iter().any(|e| e.path == path), "{path} missing");
    }
    let working_file = m.entries.iter().find(|e| e.path == "tracked.txt").unwrap();
    assert_eq!(
        f.store.get(&working_file.sha256).unwrap(),
        b"working change\n"
    );
    // The stored manifest reloads and still carries the exclusion.
    let reloaded = f.store.load(&m.id).unwrap();
    assert!(
        reloaded
            .excluded
            .iter()
            .any(|x| x.path == "artifacts" && x.reason == EXCLUDED_BY_OPERATOR)
    );

    let verification = restore::verify(&f.store, &m.id).unwrap();
    assert!(verification.verified);
    assert_eq!(verification.excluded_by_operator, ["artifacts"]);
    assert!(
        verification
            .notes
            .iter()
            .any(|n| n.contains("operator excluded 1 paths"))
    );
    let destination = f.dir.path().join("restored");
    let restored = restore::restore(&f.store, &m.id, &destination).unwrap();
    assert_eq!(restored.excluded_by_operator, ["artifacts"]);
    assert!(!destination.join("artifacts").exists());
    assert_eq!(
        fs::read_to_string(destination.join("keep.txt")).unwrap(),
        "untracked keeper\n"
    );
    assert_eq!(
        fs::read_to_string(destination.join("tracked.txt")).unwrap(),
        "working change\n"
    );

    // Assessment rescans the source with the same exclusion instead of refusing the
    // nested repository, and still marks the capture as partial.
    let other = capture::checkpoint_with_exclusions(
        &f.store,
        &f.repo,
        &[],
        &exclude(&["artifacts"]),
        false,
    )
    .unwrap();
    let plan = planning::create(&f.store, working(&m.id), vec![working(&other.id)]).unwrap();
    let assessment = planning::assess(&f.store, &plan).unwrap().to_string();
    assert!(assessment.contains("excluded_paths_are_not_preserved"));
    assert!(!assessment.contains("source_unavailable_or_unsupported"));
    assert!(!assessment.contains("source_contents_changed_since_checkpoint"));
    assert!(!assessment.contains("ignored_path_coverage_changed_since_checkpoint"));
}

#[test]
fn ignored_nested_repository_can_be_excluded_with_canonical_records() {
    let f = Fixture::new();
    fs::remove_dir_all(f.repo.join("artifacts")).unwrap();
    let nested = f.repo.join("cache/inner");
    fs::create_dir_all(&nested).unwrap();
    git::init(&nested).unwrap();
    assert!(capture::checkpoint(&f.store, &f.repo, &[], false).is_err());
    let m =
        capture::checkpoint_with_exclusions(&f.store, &f.repo, &[], &exclude(&["cache"]), false)
            .unwrap();
    let reloaded = f.store.load(&m.id).unwrap();
    assert!(
        reloaded
            .excluded
            .iter()
            .any(|x| x.path == "cache" && x.reason == EXCLUDED_BY_OPERATOR)
    );
    // Git collapses the ignored nested repository to `cache/inner/`; the record is canonical.
    assert!(reloaded.excluded.iter().any(|x| x.path == "cache/inner"));
    assert!(!m.entries.iter().any(|e| e.path.starts_with("cache")));
}

#[test]
fn exclusion_of_tracked_path_with_staged_or_working_changes_is_refused() {
    let f = Fixture::new();
    let cases: [(&[&str], &str); 3] = [
        (&["tracked.txt"], "tracked.txt"),
        (&["staged.txt"], "staged.txt"),
        (&["artifacts", "tracked.txt"], "tracked.txt"),
    ];
    for (paths, named) in cases {
        let error =
            capture::checkpoint_with_exclusions(&f.store, &f.repo, &[], &exclude(paths), false)
                .unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("staged or working changes") && message.contains(named),
            "{message}"
        );
    }
    fs::write(f.repo.join("docs/guide.md"), "edited guide\n").unwrap();
    let error =
        capture::checkpoint_with_exclusions(&f.store, &f.repo, &[], &exclude(&["docs"]), false)
            .unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("staged or working changes: docs/guide.md"),
        "{message}"
    );
    // A clean tracked path is refused too: its history and index are captured anyway.
    fs::write(f.repo.join("docs/guide.md"), "base guide\n").unwrap();
    let error =
        capture::checkpoint_with_exclusions(&f.store, &f.repo, &[], &exclude(&["docs"]), false)
            .unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("tracked path: docs/guide.md"), "{message}");
    assert!(!message.contains("staged or working changes"), "{message}");
    assert!(f.store.list().unwrap().is_empty());
    assert_eq!(f.incomplete_markers(), 0);
}

#[test]
fn exclusion_matching_nothing_or_ambiguous_is_refused_before_any_store_write() {
    let f = Fixture::new();
    let elsewhere = f.dir.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    fs::write(elsewhere.join("file.txt"), "outside\n").unwrap();
    symlink(&elsewhere, f.repo.join("linked")).unwrap();
    let missing: [&str; 3] = [
        "no-such-folder",
        "artifacts/no-such-file",
        // A symbolic-link parent is never followed, so nothing inside it can match.
        "linked/file.txt",
    ];
    for named in missing {
        let error =
            capture::checkpoint_with_exclusions(&f.store, &f.repo, &[], &exclude(&[named]), false)
                .unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains(&format!(
                "--exclude matches no path in the worktree: {named}"
            )),
            "{message}"
        );
    }
    let overlapping: [&[&str]; 2] = [
        &["artifacts", "artifacts"],
        &["artifacts", "artifacts/run-1"],
    ];
    for paths in overlapping {
        let error =
            capture::checkpoint_with_exclusions(&f.store, &f.repo, &[], &exclude(paths), false)
                .unwrap_err();
        assert!(
            format!("{error:#}").contains("repeat or overlap"),
            "{error:#}"
        );
    }
    let error = capture::checkpoint_with_exclusions(
        &f.store,
        &f.repo,
        &exclude(&["cache"]),
        &exclude(&["cache"]),
        false,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("both excluded and selected"));
    for invalid in ["../outside", "/absolute", "artifacts/run-1/.git", ""] {
        assert!(
            capture::checkpoint_with_exclusions(
                &f.store,
                &f.repo,
                &[],
                &exclude(&[invalid]),
                false
            )
            .is_err(),
            "{invalid:?} accepted"
        );
    }
    assert!(f.store.list().unwrap().is_empty());
    assert_eq!(f.incomplete_markers(), 0);
    // Excluding a path inside the nested repository does not hide the nested repository.
    let error = capture::checkpoint_with_exclusions(
        &f.store,
        &f.repo,
        &[],
        &exclude(&["artifacts/run-1/output.log"]),
        false,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("nested repository refused"));
    assert!(f.store.list().unwrap().is_empty());
}

#[test]
fn inspect_reports_nested_repositories_as_blocker_before_and_after_exclusion() {
    let f = Fixture::new();
    let ignored = f.repo.join("cache/inner");
    fs::create_dir_all(&ignored).unwrap();
    git::init(&ignored).unwrap();
    // A `.git` file (as in a linked worktree) also marks a nested repository.
    fs::create_dir(f.repo.join("docs/pointer")).unwrap();
    fs::write(f.repo.join("docs/pointer/.git"), "gitdir: missing\n").unwrap();
    let index = fs::read(f.repo.join(".git/index")).unwrap();
    let before = inspect::inspect(&f.repo, &[]).unwrap();
    assert_eq!(index, fs::read(f.repo.join(".git/index")).unwrap());
    let blocker = nested_blocker(&before);
    assert_eq!(blocker["message"], MESSAGE);
    assert_eq!(blocker["count"], 3);
    assert_eq!(
        blocker["repositories"],
        json!([
            {"path":"artifacts/run-1","tracking":"untracked"},
            {"path":"cache/inner","tracking":"ignored"},
            {"path":"docs/pointer","tracking":"untracked"}
        ])
    );
    assert_eq!(before["worktrees"][0]["inspection_status"], "scanned");

    fs::remove_dir_all(f.repo.join("docs/pointer")).unwrap();
    fs::remove_dir_all(f.repo.join("cache")).unwrap();
    capture::checkpoint_with_exclusions(&f.store, &f.repo, &[], &exclude(&["artifacts"]), false)
        .unwrap();
    // Inspection takes no exclusion: the nested repository is still reported.
    let after = inspect::inspect(&f.repo, &[]).unwrap();
    let blocker = nested_blocker(&after);
    assert_eq!(blocker["message"], MESSAGE);
    assert_eq!(blocker["count"], 1);
    assert_eq!(
        blocker["repositories"],
        json!([{"path":"artifacts/run-1","tracking":"untracked"}])
    );

    let html = report::render(&json!({"schema":1,"command":"inspect","result":after}));
    assert!(html.contains(MESSAGE));
    assert!(html.contains("artifacts/run-1"));
}

#[test]
fn inspect_classifies_a_tracked_nested_repository_and_adds_nothing_when_absent() {
    let f = Fixture::new();
    fs::remove_dir_all(f.repo.join("artifacts")).unwrap();
    let clean = inspect::inspect(&f.repo, &[]).unwrap();
    assert!(nested_blocker(&clean).is_null());
    assert!(
        clean["worktrees"][0]["blockers"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // A repository created inside a tracked directory after its files were committed.
    git::init(&f.repo.join("docs")).unwrap();
    let tracked = inspect::inspect(&f.repo, &[]).unwrap();
    assert_eq!(
        nested_blocker(&tracked)["repositories"],
        json!([{"path":"docs","tracking":"tracked"}])
    );
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_timber"))
        .args(args)
        .output()
        .unwrap()
}
fn success(out: &Output) -> Value {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn cli_exclude_flag_is_recorded_and_printed_by_verify_and_restore() {
    let f = Fixture::new();
    let store = f.dir.path().join("cli-store");
    let s = store.to_str().unwrap();
    let r = f.repo.to_str().unwrap();
    let refused = run(&["--store", s, "checkpoint", r]);
    assert_eq!(refused.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("nested repository refused"));
    let missing = run(&["--store", s, "checkpoint", r, "--exclude", "absent-folder"]);
    assert_eq!(missing.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("absent-folder"));

    let v = success(&run(&[
        "--store",
        s,
        "--json",
        "checkpoint",
        r,
        "--exclude",
        "artifacts",
    ]));
    let checkpoint = &v["result"]["checkpoint"];
    assert!(
        checkpoint["excluded"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["path"] == "artifacts" && x["reason"] == EXCLUDED_BY_OPERATOR)
    );
    assert_eq!(
        v["result"]["verification"]["excluded_by_operator"],
        json!(["artifacts"])
    );
    let id = checkpoint["id"].as_str().unwrap();
    let verified = success(&run(&["--store", s, "--json", "verify", id]));
    assert_eq!(
        verified["result"]["excluded_by_operator"],
        json!(["artifacts"])
    );
    let text = run(&["--store", s, "verify", id]);
    assert!(text.status.success());
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("Excluded by operator"), "{text}");
    assert!(text.contains("- artifacts"), "{text}");
    let destination = f.dir.path().join("cli-restored");
    let restored = run(&[
        "--store",
        s,
        "restore",
        id,
        "--to",
        destination.to_str().unwrap(),
    ]);
    assert!(restored.status.success());
    assert!(String::from_utf8_lossy(&restored.stdout).contains("- artifacts"));
    assert!(!destination.join("artifacts").exists());
    assert!(destination.join("keep.txt").exists());

    let inspected = success(&run(&["--json", "inspect", "--repo", r]));
    assert_eq!(nested_blocker(&inspected["result"])["message"], MESSAGE);
}

#[test]
fn verification_records_without_operator_exclusions_still_load() {
    let older = json!({
        "schema":1,"checkpoint":"0".repeat(64),"manifest_sha256":"0".repeat(64),
        "verifier_version":"0.1.0","verified":true,
        "historical_completeness":"captured_supported_state_only",
        "independent_backup":"unassessed","notes":[]
    });
    let parsed: Verification = serde_json::from_value(older).unwrap();
    assert!(parsed.excluded_by_operator.is_empty());
}
