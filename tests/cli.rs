use std::{
    fs,
    path::Path,
    process::{Command, Output},
};
use timber_worktrees::{fsx, git};
fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_timber"))
        .args(args)
        .output()
        .unwrap()
}
fn repo(path: &Path) {
    fs::create_dir(path).unwrap();
    git::init(path).unwrap();
    git::run(path, &["config", "user.name", "Fixture Developer"]).unwrap();
    git::run(path, &["config", "user.email", "fixture@example.invalid"]).unwrap();
    fs::write(path.join("base.txt"), "base\n").unwrap();
    git::run(path, &["add", "base.txt"]).unwrap();
    git::run(path, &["commit", "-qm", "base"]).unwrap();
}
#[test]
fn inspect_is_read_only_and_never_runs_clean_filters() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path().join("repo");
    repo(&r);
    let sentinel = d.path().join("executed");
    git::run(
        &r,
        &[
            "config",
            "filter.hostile.clean",
            &format!("touch '{}'; cat", sentinel.display()),
        ],
    )
    .unwrap();
    fs::write(r.join(".gitattributes"), "base.txt filter=hostile\n").unwrap();
    fs::write(r.join("base.txt"), "dirty\n").unwrap();
    let index = fs::read(r.join(".git/index")).unwrap();
    let before = fsx::inventory(&r).unwrap();
    let out = run(&["inspect", "--repo", r.to_str().unwrap(), "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let data: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        data["result"]["worktrees"][0]["working_paths"][0],
        "base.txt"
    );
    assert!(!sentinel.exists());
    assert_eq!(index, fs::read(r.join(".git/index")).unwrap());
    assert_eq!(before, fsx::inventory(&r).unwrap());
}
#[test]
fn source_nested_store_is_refused_before_creation() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path().join("repo");
    repo(&r);
    let store = r.join("should-not-exist");
    let out = run(&[
        "--store",
        store.to_str().unwrap(),
        "checkpoint",
        r.to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    assert!(!store.exists());
}
#[test]
fn stale_registration_is_reported_without_pruning() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path().join("repo");
    repo(&r);
    let w = d.path().join("agent");
    git::run(
        &r,
        &["worktree", "add", "-q", "-b", "agent", w.to_str().unwrap()],
    )
    .unwrap();
    fs::rename(&w, d.path().join("parked")).unwrap();
    let before = git::run(&r, &["worktree", "list", "--porcelain", "-z"]).unwrap();
    let out = run(&["inspect", "--repo", r.to_str().unwrap(), "--json"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("missing_worktree_registration"));
    assert_eq!(
        before,
        git::run(&r, &["worktree", "list", "--porcelain", "-z"]).unwrap()
    );
}
#[test]
fn json_checkpoint_restore_and_report_roundtrip() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path().join("repo");
    repo(&r);
    let s = d.path().join("store");
    let output = d.path().join("checkpoint.json");
    fs::write(r.join("base.txt"), "staged\n").unwrap();
    git::run(&r, &["add", "base.txt"]).unwrap();
    fs::write(r.join("base.txt"), "working\n").unwrap();
    let out = run(&[
        "--store",
        s.to_str().unwrap(),
        "--json",
        "checkpoint",
        r.to_str().unwrap(),
        "--out",
        output.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let id = v["result"]["checkpoint"]["id"].as_str().unwrap();
    let restored = d.path().join("restored");
    let out = run(&[
        "--store",
        s.to_str().unwrap(),
        "restore",
        id,
        "--to",
        restored.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::read_to_string(restored.join("base.txt")).unwrap(),
        "working\n"
    );
    let html = d.path().join("report.html");
    let out = run(&[
        "--store",
        s.to_str().unwrap(),
        "report",
        output.to_str().unwrap(),
        "--to",
        html.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    assert!(fs::read_to_string(html).unwrap().contains("Timber"));
}

#[test]
fn every_output_and_noncreating_lookup_stays_outside_sources() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path().join("repo");
    repo(&r);
    let s = d.path().join("store");
    let out = run(&[
        "--store",
        s.to_str().unwrap(),
        "--json",
        "checkpoint",
        r.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let id = value["result"]["checkpoint"]["id"].as_str().unwrap();
    let source_output = r.join("must-not-exist.json");
    for args in [vec!["checkpoints", "show", id], vec!["checkpoints", "list"]] {
        let mut cmd = vec![
            "--store",
            s.to_str().unwrap(),
            "--out",
            source_output.to_str().unwrap(),
        ];
        cmd.extend(args);
        assert!(!run(&cmd).status.success());
        assert!(!source_output.exists());
    }
    let saved = d.path().join("saved.json");
    fs::write(&saved, serde_json::to_vec(&value).unwrap()).unwrap();
    let html = d.path().join("must-not-exist.html");
    let out = run(&[
        "--store",
        s.to_str().unwrap(),
        "--out",
        source_output.to_str().unwrap(),
        "report",
        saved.to_str().unwrap(),
        "--to",
        html.to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    assert!(!source_output.exists());
    assert!(!html.exists());
    let missing = r.join("missing-store");
    let out = run(&["--store", missing.to_str().unwrap(), "checkpoints", "list"]);
    assert!(!out.status.success());
    assert!(!missing.exists());
}

#[test]
fn linked_worktree_cannot_write_into_shared_repository() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path().join("repo");
    repo(&r);
    let w = d.path().join("agent");
    git::run(
        &r,
        &["worktree", "add", "-q", "-b", "agent", w.to_str().unwrap()],
    )
    .unwrap();
    for store in [r.join("new-store"), r.join(".git/new-store")] {
        let out = run(&[
            "--store",
            store.to_str().unwrap(),
            "checkpoint",
            w.to_str().unwrap(),
        ]);
        assert!(!out.status.success());
        assert!(!store.exists());
    }
    let s = d.path().join("store");
    let source_output = r.join("new-report.json");
    let out = run(&[
        "--store",
        s.to_str().unwrap(),
        "--out",
        source_output.to_str().unwrap(),
        "checkpoint",
        w.to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    assert!(!source_output.exists());
    assert!(!s.exists());
}
#[test]
fn path_aliases_and_symlink_parent_reads_are_refused() {
    for p in [
        "a/./b",
        "a//b",
        "../b",
        "a/../b",
        "/tmp/file",
        ".git/config",
        ".GiT/config",
    ] {
        assert!(fsx::validate_relative(p).is_err(), "{p}");
    }
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("root");
    let outside = d.path().join("outside");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret"), "not readable through alias").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
    let e = timber_worktrees::model::Entry {
        path: "link/secret".into(),
        kind: timber_worktrees::model::EntryKind::File,
        mode: 0o600,
        size: 0,
        sha256: String::new(),
    };
    assert!(fsx::read_entry(&root, &e).is_err());
}
