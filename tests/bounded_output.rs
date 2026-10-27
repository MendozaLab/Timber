//! Bounded inspect output: ignored paths are summarized by default, listed in full
//! with `--list-ignored`, and the recovery evidence lists stay complete either way.
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use timber_worktrees::{
    capture, git,
    inspect::{self, IGNORED_TOP_DIRS_LIMIT, InspectOptions},
    model::{INSPECT_SCHEMA, SCHEMA},
    report,
    store::Store,
};

const CACHE_SHARDS: usize = 24;
const CACHE_PER_SHARD: usize = 100;
const LOGS: usize = 600;
const TEMPS: usize = 30;
const CACHE_BYTES: &[u8] = b"cache-bytes\n";
const LOG_BYTES: &[u8] = b"log\n";
const TEMP_BYTES: &[u8] = b"tmp-data\n";
const IGNORED_COUNT: usize = CACHE_SHARDS * CACHE_PER_SHARD + LOGS + TEMPS;

fn total_bytes() -> u64 {
    (CACHE_SHARDS * CACHE_PER_SHARD * CACHE_BYTES.len()
        + LOGS * LOG_BYTES.len()
        + TEMPS * TEMP_BYTES.len()) as u64
}

/// A synthetic worktree with staged, working and untracked changes and a few
/// thousand ignored files spread over two ignored folders and 30 top-level files.
fn fixture(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    fs::create_dir(&repo).unwrap();
    git::init(&repo).unwrap();
    git::run(&repo, &["config", "user.name", "Fixture Developer"]).unwrap();
    git::run(&repo, &["config", "user.email", "fixture@example.invalid"]).unwrap();
    fs::create_dir(repo.join("src")).unwrap();
    fs::write(repo.join("base.txt"), "base\n").unwrap();
    fs::write(repo.join("src/lib.txt"), "library\n").unwrap();
    fs::write(repo.join(".gitignore"), "build-cache/\nlogs/\n*.tmp\n").unwrap();
    git::run(&repo, &["add", "."]).unwrap();
    git::run(&repo, &["commit", "-qm", "base"]).unwrap();
    fs::write(repo.join("base.txt"), "staged change\n").unwrap();
    git::run(&repo, &["add", "base.txt"]).unwrap();
    fs::write(repo.join("src/lib.txt"), "working change\n").unwrap();
    fs::create_dir(repo.join("notes")).unwrap();
    fs::write(repo.join("notes/draft.md"), "untracked draft\n").unwrap();
    fs::write(repo.join("untracked-a.txt"), "untracked\n").unwrap();
    for shard in 0..CACHE_SHARDS {
        let dir = repo.join(format!("build-cache/shard-{shard:02}"));
        fs::create_dir_all(&dir).unwrap();
        for item in 0..CACHE_PER_SHARD {
            fs::write(dir.join(format!("item-{item:04}.bin")), CACHE_BYTES).unwrap();
        }
    }
    fs::create_dir(repo.join("logs")).unwrap();
    for i in 0..LOGS {
        fs::write(repo.join(format!("logs/run-{i:04}.log")), LOG_BYTES).unwrap();
    }
    for i in 0..TEMPS {
        fs::write(repo.join(format!("scratch-{i:02}.tmp")), TEMP_BYTES).unwrap();
    }
    repo
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_timber"))
        .args(args)
        .output()
        .unwrap()
}

fn inspect_json(repo: &Path, extra: &[&str]) -> (Vec<u8>, Value) {
    let mut args = vec!["--json", "inspect", "--repo", repo.to_str().unwrap()];
    args.extend(extra);
    let out = run(&args);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    (out.stdout, value)
}

#[test]
fn default_inspect_summarizes_ignored_paths_with_bounded_top_dirs() {
    let temp = tempfile::tempdir().unwrap();
    let repo = fixture(temp.path());
    let result = inspect::inspect(&repo, &[]).unwrap();
    assert_eq!(result["schema"], INSPECT_SCHEMA);
    assert_eq!(result["scope"]["list_ignored"], false);
    let summary = &result["worktrees"][0]["ignored"];
    let keys = summary
        .as_object()
        .expect("ignored is a summary object by default")
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    assert_eq!(keys, ["bytes", "count", "top_dirs"]);
    assert_eq!(summary["count"], IGNORED_COUNT);
    assert_eq!(summary["bytes"], total_bytes());
    let top = summary["top_dirs"].as_array().unwrap();
    assert_eq!(top.len(), IGNORED_TOP_DIRS_LIMIT);
    assert_eq!(
        top[0],
        json!({"path":"build-cache","count":CACHE_SHARDS * CACHE_PER_SHARD,"bytes":CACHE_SHARDS * CACHE_PER_SHARD * CACHE_BYTES.len()})
    );
    assert_eq!(
        top[1],
        json!({"path":"logs","count":LOGS,"bytes":LOGS * LOG_BYTES.len()})
    );
    // Equal counts fall back to path order; the bound drops the remaining files.
    assert_eq!(
        top[2],
        json!({"path":"scratch-00.tmp","count":1,"bytes":TEMP_BYTES.len()})
    );
    assert_eq!(top[IGNORED_TOP_DIRS_LIMIT - 1]["path"], "scratch-17.tmp");
    assert!(!top.iter().any(|row| row["path"] == "scratch-18.tmp"));
    let row = &result["worktrees"][0];
    assert_eq!(row["inspection_status"], "scanned");
    assert_eq!(row["staged_paths"], json!(["base.txt"]));
    assert_eq!(row["working_paths"], json!(["src/lib.txt"]));
    assert_eq!(row["untracked"].as_array().unwrap().len(), 2);

    // Capture is unchanged: it still enumerates ignored paths itself, captures
    // only the selection, and records every omission.
    let store = Store::open(&temp.path().join("store")).unwrap();
    let manifest = capture::checkpoint(&store, &repo, &["logs".into()], false).unwrap();
    let omitted = manifest
        .excluded
        .iter()
        .filter(|e| e.reason == "ignored file not explicitly selected")
        .count();
    assert_eq!(omitted, CACHE_SHARDS * CACHE_PER_SHARD + TEMPS);
    assert!(
        manifest
            .entries
            .iter()
            .any(|e| e.path == "logs/run-0599.log")
    );
    assert_eq!(manifest.schema, SCHEMA);
}

#[test]
fn list_ignored_restores_full_list_and_keeps_recovery_lists_byte_identical() {
    let temp = tempfile::tempdir().unwrap();
    let repo = fixture(temp.path());
    let (summary_bytes, summary) = inspect_json(&repo, &[]);
    let (list_bytes, listed) = inspect_json(&repo, &["--list-ignored"]);
    let (summary, listed) = (&summary["result"], &listed["result"]);
    assert_eq!(summary["schema"], INSPECT_SCHEMA);
    assert_eq!(listed["schema"], INSPECT_SCHEMA);
    assert_eq!(listed["scope"]["list_ignored"], true);
    let rows = listed["worktrees"][0]["ignored"].as_array().unwrap();
    assert_eq!(rows.len(), IGNORED_COUNT);
    let paths = rows
        .iter()
        .map(|r| r["path"].as_str().unwrap().to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(paths.len(), IGNORED_COUNT);
    for path in [
        "build-cache/shard-00/item-0000.bin",
        "build-cache/shard-23/item-0099.bin",
        "logs/run-0599.log",
        "scratch-29.tmp",
    ] {
        assert!(paths.contains(path), "missing {path}");
    }
    // The default summary is exactly the summary of the full list.
    assert_eq!(
        summary["worktrees"][0]["ignored"],
        inspect::ignored_summary(rows)
    );
    for key in [
        "head",
        "staged_paths",
        "working_paths",
        "unknown_working_paths",
        "untracked",
        "unknown_metadata_paths",
        "blockers",
    ] {
        assert_eq!(
            serde_json::to_vec(&summary["worktrees"][0][key]).unwrap(),
            serde_json::to_vec(&listed["worktrees"][0][key]).unwrap(),
            "{key} differs between modes"
        );
    }
    assert!(
        summary_bytes.len() * 10 < list_bytes.len(),
        "summary {} bytes, list {} bytes",
        summary_bytes.len(),
        list_bytes.len()
    );
}

#[test]
fn report_renders_summary_table_or_full_list_following_the_json() {
    let temp = tempfile::tempdir().unwrap();
    let repo = fixture(temp.path());
    let store = temp.path().join("store");
    let mut html = Vec::new();
    for (name, extra) in [("summary", None), ("list", Some("--list-ignored"))] {
        let saved = temp.path().join(format!("{name}.json"));
        let mut args = vec![
            "--store",
            store.to_str().unwrap(),
            "--json",
            "--out",
            saved.to_str().unwrap(),
            "inspect",
            "--repo",
            repo.to_str().unwrap(),
        ];
        args.extend(extra);
        let out = run(&args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let page = temp.path().join(format!("{name}.html"));
        let out = run(&[
            "--store",
            store.to_str().unwrap(),
            "report",
            saved.to_str().unwrap(),
            "--to",
            page.to_str().unwrap(),
        ]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        html.push(fs::read_to_string(page).unwrap());
    }
    let (summary, list) = (&html[0], &html[1]);
    for page in [summary, list] {
        assert!(!page.contains("<script"));
        assert!(!page.contains("<link"));
        assert!(page.contains("Content-Security-Policy"));
        assert!(page.contains(&format!("<dt>Ignored files</dt><dd>{IGNORED_COUNT}</dd>")));
    }
    let before_evidence = summary.split("Complete recorded evidence").next().unwrap();
    assert!(before_evidence.contains("Ignored files summary"));
    assert!(before_evidence.contains("<table>"));
    assert!(before_evidence.contains("<code>build-cache</code>"));
    assert!(before_evidence.contains(&total_bytes().to_string()));
    assert!(!summary.contains("build-cache/shard-00/item-0000.bin"));
    assert!(!summary.contains("logs/run-0000.log"));
    assert!(!list.contains("Ignored files summary"));
    assert!(list.contains("build-cache/shard-00/item-0000.bin"));
    assert!(list.contains("logs/run-0599.log"));
    assert!(summary.len() * 10 < list.len());
}

#[test]
fn inspect_schema_bump_marks_the_shape_and_leaves_stored_schema_alone() {
    // Stored manifests, plans and receipts keep their own schema.
    assert_eq!(SCHEMA, 1);
    assert_eq!(INSPECT_SCHEMA, 2);
    let temp = tempfile::tempdir().unwrap();
    let repo = fixture(temp.path());
    let options = InspectOptions {
        list_ignored: true,
        ..Default::default()
    };
    // Consumers detect the shape by the schema number: below 2 the list is the only
    // shape; from 2 the summary is the default and the list requires the flag.
    let shape = |result: &Value| {
        let ignored = &result["worktrees"][0]["ignored"];
        match result["schema"].as_u64() {
            Some(1) => ignored.as_array().map(|_| "list"),
            Some(2) if result["scope"]["list_ignored"] == true => {
                ignored.as_array().map(|_| "list")
            }
            Some(2) => ignored.as_object().map(|_| "summary"),
            _ => None,
        }
    };
    let summary = inspect::inspect(&repo, &[]).unwrap();
    let listed = inspect::inspect_with_options(&repo, &[], &options).unwrap();
    assert_eq!(shape(&summary), Some("summary"));
    assert_eq!(shape(&listed), Some("list"));
    let legacy = json!({"schema":1,"worktrees":[{"path":"agent-a","kind":"registered_worktree","ignored":[{"path":"cache/legacy.bin","bytes":7}]}]});
    assert_eq!(shape(&legacy), Some("list"));
    // A legacy list still renders as a list, never as a summary table.
    let html = report::render(&json!({"schema":1,"command":"inspect","result":legacy}));
    assert!(html.contains("cache/legacy.bin"));
    assert!(!html.contains("Ignored files summary"));
    assert!(html.contains("<dt>Ignored files</dt><dd>1</dd>"));
}
