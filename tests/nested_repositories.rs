//! Nested repositories: inspect names the blocker, checkpoint refuses it unless the
//! operator explicitly excludes a disposable untracked or ignored path for that run.
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
    process::{Command, Output},
};
use tempfile::TempDir;
use timber_worktrees::{
    capture, git, inspect,
    model::{
        EXCLUDED_BY_OPERATOR, LINKED_WORKTREE_CAPTURED_SEPARATELY, Layer, Selection, Verification,
    },
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

// Linked worktrees of the same repository placed inside its own checkout.

/// Adds a registered linked worktree at `relative` (checked out on a new branch) and
/// writes a working change inside it that the outer checkpoint must never read.
fn add_linked_worktree(repo: &std::path::Path, relative: &str) {
    git::run(repo, &["worktree", "add", "-q", relative]).unwrap();
    fs::write(repo.join(relative).join("inner-only.txt"), "inner work\n").unwrap();
}

fn linked_inside(result: &Value) -> Value {
    result["worktrees"][0]["linked_worktrees_inside"].clone()
}

fn reasons(m: &timber_worktrees::model::Manifest, reason: &str) -> Vec<String> {
    m.excluded
        .iter()
        .filter(|x| x.reason == reason)
        .map(|x| x.path.clone())
        .collect()
}

#[test]
fn inspect_reports_linked_worktree_separately_from_foreign_nested_repository() {
    let f = Fixture::new();
    add_linked_worktree(&f.repo, "cache/wt-one");
    let result = inspect::inspect(&f.repo, &[]).unwrap();
    let blocker = nested_blocker(&result);
    assert_eq!(blocker["message"], MESSAGE);
    assert_eq!(blocker["count"], 1);
    assert_eq!(
        blocker["repositories"],
        json!([{"path":"artifacts/run-1","tracking":"untracked"}])
    );
    assert_eq!(
        linked_inside(&result),
        json!([{"path":"cache/wt-one","registration":"wt-one","tracking":"ignored"}])
    );
    assert_eq!(result["worktrees"][0]["inspection_status"], "scanned");
    let html = report::render(&json!({"schema":1,"command":"inspect","result":result}));
    assert!(html.contains("Linked worktrees inside"), "{html}");

    // Without any foreign nested repository the linked worktree is not a blocker.
    fs::remove_dir_all(f.repo.join("artifacts")).unwrap();
    let clean = inspect::inspect(&f.repo, &[]).unwrap();
    assert!(nested_blocker(&clean).is_null());
    assert_eq!(linked_inside(&clean).as_array().unwrap().len(), 1);
}

#[test]
fn checkpoint_skips_linked_worktrees_and_still_refuses_foreign_nested_repository() {
    let f = Fixture::new();
    add_linked_worktree(&f.repo, "cache/wt-one");
    // A second linked worktree below an untracked, not ignored, folder.
    add_linked_worktree(&f.repo, "agent-trees/wt-two");
    let error = capture::checkpoint(&f.store, &f.repo, &[], false).unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("nested repository refused"), "{message}");
    assert!(message.contains("artifacts/run-1/.git"), "{message}");
    assert!(
        !message.contains("wt-one") && !message.contains("wt-two"),
        "{message}"
    );
    assert!(f.store.list().unwrap().is_empty());

    let m = capture::checkpoint_with_exclusions(
        &f.store,
        &f.repo,
        &[],
        &exclude(&["artifacts"]),
        false,
    )
    .unwrap();
    assert_eq!(reasons(&m, EXCLUDED_BY_OPERATOR), ["artifacts"]);
    assert_eq!(
        reasons(&m, LINKED_WORKTREE_CAPTURED_SEPARATELY),
        ["agent-trees/wt-two", "cache/wt-one"]
    );
    assert!(
        !m.entries.iter().any(
            |e| e.path.starts_with("cache/wt-one") || e.path.starts_with("agent-trees/wt-two/")
        ),
        "linked worktree contents were captured"
    );
    for path in ["keep.txt", "staged.txt", "tracked.txt", "agent-trees"] {
        assert!(m.entries.iter().any(|e| e.path == path), "{path} missing");
    }

    let verification = restore::verify(&f.store, &m.id).unwrap();
    assert!(verification.verified);
    assert_eq!(verification.excluded_by_operator, ["artifacts"]);
    assert_eq!(
        verification.linked_worktrees_captured_separately,
        ["agent-trees/wt-two", "cache/wt-one"]
    );
    assert!(
        verification
            .notes
            .iter()
            .any(|n| n.contains("2 linked worktrees"))
    );
    let destination = f.dir.path().join("restored");
    let restored = restore::restore(&f.store, &m.id, &destination).unwrap();
    assert_eq!(restored.linked_worktrees_captured_separately.len(), 2);
    assert!(!destination.join("cache/wt-one").exists());
    assert!(!destination.join("agent-trees/wt-two").exists());
    assert!(destination.join("keep.txt").exists());

    // Assessment prunes linked worktrees like operator exclusions.
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

    // The linked worktree is untouched and still checkpoints on its own.
    let inner = f.repo.join("cache/wt-one");
    let own = capture::checkpoint(&f.store, &inner, &[], false).unwrap();
    assert!(own.entries.iter().any(|e| e.path == "inner-only.txt"));
}

#[test]
fn exclude_naming_a_linked_worktree_is_recorded_as_operator_exclusion() {
    let f = Fixture::new();
    add_linked_worktree(&f.repo, "cache/wt-one");
    let m = capture::checkpoint_with_exclusions(
        &f.store,
        &f.repo,
        &[],
        &exclude(&["artifacts", "cache/wt-one"]),
        false,
    )
    .unwrap();
    assert_eq!(
        reasons(&m, EXCLUDED_BY_OPERATOR),
        ["artifacts", "cache/wt-one"]
    );
    assert!(reasons(&m, LINKED_WORKTREE_CAPTURED_SEPARATELY).is_empty());
    // An exclusion strictly inside a linked worktree is refused before any store write.
    let error = capture::checkpoint_with_exclusions(
        &f.store,
        &f.repo,
        &[],
        &exclude(&["artifacts", "cache/wt-one/inner-only.txt"]),
        false,
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("inside linked worktree cache/wt-one"),
        "{error:#}"
    );
    assert_eq!(f.store.list().unwrap().len(), 1);
    assert_eq!(f.incomplete_markers(), 0);
}

/// Each `.git` file below that is not provably this repository's own registration is
/// classified as a foreign nested repository and keeps the refusal.
#[test]
fn git_files_not_registered_in_this_repository_are_foreign() {
    let f = Fixture::new();
    fs::remove_dir_all(f.repo.join("artifacts")).unwrap();
    add_linked_worktree(&f.repo, "cache/wt-one");
    let admin = fs::canonicalize(f.repo.join(".git/worktrees/wt-one")).unwrap();

    // Another repository's registered linked worktree admin directory.
    let other = f.dir.path().join("other");
    fs::create_dir(&other).unwrap();
    git::init(&other).unwrap();
    git::run(&other, &["config", "user.name", "Fixture Developer"]).unwrap();
    git::run(&other, &["config", "user.email", "fixture@example.invalid"]).unwrap();
    fs::write(other.join("a.txt"), "a\n").unwrap();
    git::run(&other, &["add", "."]).unwrap();
    git::run(&other, &["commit", "-qm", "other"]).unwrap();
    git::run(&other, &["worktree", "add", "-q", "../other-wt"]).unwrap();
    let other_admin = fs::canonicalize(other.join(".git/worktrees/other-wt")).unwrap();

    let pointer = |folder: &str, content: String| {
        fs::create_dir_all(f.repo.join(folder)).unwrap();
        fs::write(f.repo.join(folder).join(".git"), content).unwrap();
    };
    pointer(
        "scratch/other-repo",
        format!("gitdir: {}\n", other_admin.display()),
    );
    // A copy of this repository's own pointer: Git's registration names another folder.
    pointer("scratch/copied", format!("gitdir: {}\n", admin.display()));
    // This repository's common directory itself, not a registration below `worktrees`.
    pointer(
        "scratch/common",
        format!(
            "gitdir: {}\n",
            admin.parent().unwrap().parent().unwrap().display()
        ),
    );
    pointer("scratch/empty", String::new());
    pointer("scratch/no-path", "gitdir: \n".into());
    pointer("scratch/not-pointer", "worktree pointer\n".into());

    let result = inspect::inspect(&f.repo, &[]).unwrap();
    let blocker = nested_blocker(&result);
    assert_eq!(
        blocker["repositories"],
        json!([
            {"path":"scratch/common","tracking":"untracked"},
            {"path":"scratch/copied","tracking":"untracked"},
            {"path":"scratch/empty","tracking":"untracked"},
            {"path":"scratch/no-path","tracking":"untracked"},
            {"path":"scratch/not-pointer","tracking":"untracked"},
            {"path":"scratch/other-repo","tracking":"untracked"}
        ])
    );
    assert_eq!(
        linked_inside(&result),
        json!([{"path":"cache/wt-one","registration":"wt-one","tracking":"ignored"}])
    );
    let error = capture::checkpoint(&f.store, &f.repo, &[], false).unwrap_err();
    assert!(format!("{error:#}").contains("nested repository refused: \"scratch/"));
    let m =
        capture::checkpoint_with_exclusions(&f.store, &f.repo, &[], &exclude(&["scratch"]), false)
            .unwrap();
    assert_eq!(
        reasons(&m, LINKED_WORKTREE_CAPTURED_SEPARATELY),
        ["cache/wt-one"]
    );
}

/// The real registered worktree becomes foreign when its own `.git` file is
/// malformed, unreadable, or reaches the registration through a symbolic link.
#[test]
fn malformed_unreadable_or_symlinked_git_file_is_never_guessed_as_linked() {
    let f = Fixture::new();
    fs::remove_dir_all(f.repo.join("artifacts")).unwrap();
    add_linked_worktree(&f.repo, "cache/wt-one");
    let dot_git = f.repo.join("cache/wt-one/.git");
    let original = fs::read_to_string(&dot_git).unwrap();
    let admin = fs::canonicalize(f.repo.join(".git/worktrees/wt-one")).unwrap();
    let common = admin.parent().unwrap().parent().unwrap().to_path_buf();
    let alias = f.dir.path().join("common-alias");
    symlink(&common, &alias).unwrap();

    let foreign = |label: &str| {
        let result = inspect::inspect(&f.repo, &[]).unwrap();
        assert_eq!(
            nested_blocker(&result)["repositories"],
            json!([{"path":"cache/wt-one","tracking":"ignored"}]),
            "{label}"
        );
        assert_eq!(linked_inside(&result), json!([]), "{label}");
        let error = capture::checkpoint(&f.store, &f.repo, &[], false).unwrap_err();
        assert!(
            format!("{error:#}").contains("nested repository refused: \"cache/wt-one/.git\""),
            "{label}: {error:#}"
        );
    };
    let variants = [
        ("two lines", format!("{}extra line\n", original)),
        ("trailing space", original.replace('\n', " \n")),
        ("prefix only", "gitdir:\n".to_owned()),
        (
            "through a symbolic link",
            format!("gitdir: {}\n", alias.join("worktrees/wt-one").display()),
        ),
        ("oversized", format!("{original}{}", " ".repeat(5000))),
    ];
    for (label, content) in variants {
        fs::write(&dot_git, content).unwrap();
        foreign(label);
    }
    fs::write(&dot_git, &original).unwrap();
    fs::set_permissions(&dot_git, fs::Permissions::from_mode(0o000)).unwrap();
    // A process that can still read a mode-000 file (root) cannot exercise this case.
    if fs::read(&dot_git).is_err() {
        foreign("unreadable");
    }
    fs::set_permissions(&dot_git, fs::Permissions::from_mode(0o644)).unwrap();
    // Restored, it is a linked worktree again.
    let result = inspect::inspect(&f.repo, &[]).unwrap();
    assert!(nested_blocker(&result).is_null());
    assert_eq!(linked_inside(&result).as_array().unwrap().len(), 1);
    assert!(f.store.list().unwrap().is_empty());
}

#[test]
fn linked_worktree_holding_tracked_paths_stays_a_blocker_and_is_refused() {
    let f = Fixture::new();
    fs::remove_dir_all(f.repo.join("artifacts")).unwrap();
    fs::create_dir(f.repo.join("old")).unwrap();
    fs::write(f.repo.join("old/file.txt"), "tracked once\n").unwrap();
    git::run(&f.repo, &["add", "old/file.txt"]).unwrap();
    git::run(&f.repo, &["commit", "-qm", "old"]).unwrap();
    fs::remove_dir_all(f.repo.join("old")).unwrap();
    add_linked_worktree(&f.repo, "old");
    let result = inspect::inspect(&f.repo, &[]).unwrap();
    assert_eq!(
        nested_blocker(&result)["repositories"],
        json!([{"path":"old","tracking":"tracked","linked_worktree":"old"}])
    );
    assert_eq!(linked_inside(&result), json!([]));
    let error = capture::checkpoint(&f.store, &f.repo, &[], false).unwrap_err();
    assert!(
        format!("{error:#}")
            .contains("linked worktree old (registration old) holds tracked path old/file.txt"),
        "{error:#}"
    );
    assert!(f.store.list().unwrap().is_empty());
    assert_eq!(f.incomplete_markers(), 0);
}

#[test]
fn cli_checkpoint_records_and_verify_prints_linked_worktrees() {
    let f = Fixture::new();
    add_linked_worktree(&f.repo, "cache/wt-one");
    let store = f.dir.path().join("cli-store");
    let s = store.to_str().unwrap();
    let r = f.repo.to_str().unwrap();
    let v = success(&run(&[
        "--store",
        s,
        "--json",
        "checkpoint",
        r,
        "--exclude",
        "artifacts",
    ]));
    assert!(
        v["result"]["checkpoint"]["excluded"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["path"] == "cache/wt-one"
                && x["reason"] == LINKED_WORKTREE_CAPTURED_SEPARATELY)
    );
    let id = v["result"]["checkpoint"]["id"].as_str().unwrap();
    let text = run(&["--store", s, "verify", id]);
    assert!(text.status.success());
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(
        text.contains("Linked worktrees captured separately"),
        "{text}"
    );
    assert!(text.contains("- cache/wt-one"), "{text}");
    let inspected = success(&run(&["--json", "inspect", "--repo", r]));
    assert_eq!(
        linked_inside(&inspected["result"]),
        json!([{"path":"cache/wt-one","registration":"wt-one","tracking":"ignored"}])
    );
}
