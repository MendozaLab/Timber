//! Team convention file (`timber.toml`) and `timber init`: synthetic fixtures only.
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use tempfile::TempDir;
use timber_worktrees::{
    capture, convention, git,
    model::{
        EXCLUDED_BY_CONVENTION_FILE, EXCLUDED_BY_OPERATOR, LINKED_WORKTREE_CAPTURED_SEPARATELY,
        Layer, Selection,
    },
    planning, restore,
    store::Store,
};

struct Fixture {
    dir: TempDir,
    repo: PathBuf,
}
impl Fixture {
    /// A worktree with a tracked change, an untracked keeper, an untracked scratch
    /// folder, and a disposable nested repository below the untracked `artifacts/`.
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
        git::run(&repo, &["add", "."]).unwrap();
        git::run(&repo, &["commit", "-qm", "base"]).unwrap();
        fs::write(repo.join("tracked.txt"), "working change\n").unwrap();
        fs::write(repo.join("keep.txt"), "untracked keeper\n").unwrap();
        fs::create_dir(repo.join("scratch")).unwrap();
        fs::write(repo.join("scratch/notes.txt"), "scratch\n").unwrap();
        let nested = repo.join("artifacts/run-1");
        fs::create_dir_all(&nested).unwrap();
        git::init(&nested).unwrap();
        fs::write(nested.join("output.log"), "disposable output\n").unwrap();
        Self { dir, repo }
    }
    /// Writes and commits `timber.toml`, as a team would.
    fn convention(&self, text: &str) {
        fs::write(self.repo.join("timber.toml"), text).unwrap();
        git::run(&self.repo, &["add", "timber.toml"]).unwrap();
        git::run(&self.repo, &["commit", "-qm", "convention"]).unwrap();
    }
    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
    /// Runs Timber with TIMBER_STORE pointed at a decoy inside the fixture, so no
    /// test can reach a real per-user store.
    fn run(&self, args: &[&str]) -> Output {
        self.run_env(args, &[])
    }
    fn run_env(&self, args: &[&str], env: &[(&str, &Path)]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_timber"));
        command
            .args(args)
            .env("TIMBER_STORE", self.path("decoy-store"));
        for (key, value) in env {
            command.env(key, value);
        }
        command.output().unwrap()
    }
}

fn s(path: &Path) -> &str {
    path.to_str().unwrap()
}
fn json(out: &Output) -> Value {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}
fn reasons(result: &Value, reason: &str) -> Vec<String> {
    result["checkpoint"]["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x["reason"] == reason)
        .map(|x| x["path"].as_str().unwrap().to_owned())
        .collect()
}
fn working(id: &str) -> Selection {
    Selection {
        checkpoint: id.to_owned(),
        layer: Layer::Working,
        extra_paths: Vec::new(),
    }
}

#[test]
fn init_writes_commented_template_and_refuses_to_overwrite() {
    let f = Fixture::new();
    let file = f.repo.join("timber.toml");
    let out = f.run(&["--json", "init", s(&f.repo)]);
    let result = json(&out);
    assert_eq!(result["command"], "init");
    assert_eq!(result["result"]["status"], "written");
    assert_eq!(fs::read_to_string(&file).unwrap(), convention::TEMPLATE);
    // The template is all comments: reading it changes nothing.
    assert_eq!(
        convention::load(&f.repo).unwrap().values,
        convention::Values::default()
    );

    fs::write(&file, "schedule = \"edited by the team\"\n").unwrap();
    let again = f.run(&["init", s(&f.repo)]);
    assert_eq!(again.status.code(), Some(3));
    assert!(
        stderr(&again).contains("already exists; init never overwrites it"),
        "{}",
        stderr(&again)
    );
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "schedule = \"edited by the team\"\n"
    );

    // A symbolic link at the name is refused too, and its target is untouched.
    let g = Fixture::new();
    let target = g.path("elsewhere.toml");
    fs::write(&target, "outside\n").unwrap();
    std::os::unix::fs::symlink(&target, g.repo.join("timber.toml")).unwrap();
    assert!(!g.run(&["init", s(&g.repo)]).status.success());
    assert_eq!(fs::read_to_string(&target).unwrap(), "outside\n");

    // Init writes only at the worktree root.
    let h = Fixture::new();
    let below = h.run(&["init", s(&h.repo.join("docs"))]);
    assert!(!below.status.success());
    assert!(stderr(&below).contains("init must select the worktree root"));
    assert!(!h.repo.join("docs/timber.toml").exists());
    assert!(!h.repo.join("timber.toml").exists());
}

#[test]
fn convention_exclusions_apply_to_checkpoint_and_record_their_origin() {
    let f = Fixture::new();
    f.convention(
        "# team convention\nexclude = [\n  \"artifacts/\",\n]\nschedule = \"before each merge\"\n",
    );
    let store = f.path("store");
    // Without the convention the nested repository would refuse the checkpoint; with
    // it, no command-line flag is needed.
    let out = f.run(&["--store", s(&store), "--json", "checkpoint", s(&f.repo)]);
    let result = json(&out)["result"].clone();
    assert!(
        stderr(&out).contains("read convention file"),
        "{}",
        stderr(&out)
    );
    assert_eq!(reasons(&result, EXCLUDED_BY_CONVENTION_FILE), ["artifacts"]);
    assert!(reasons(&result, EXCLUDED_BY_OPERATOR).is_empty());
    assert_eq!(result["convention"]["status"], "read");
    assert_eq!(
        result["convention"]["file"],
        s(&f.repo.canonicalize().unwrap().join("timber.toml"))
    );
    assert_eq!(result["convention"]["exclusions_applied"][0], "artifacts");
    assert_eq!(
        result["convention"]["values"]["schedule"],
        "before each merge"
    );
    assert!(
        result["convention"]["notes"][0]
            .as_str()
            .unwrap()
            .contains("never schedules anything")
    );
    assert_eq!(
        result["verification"]["excluded_by_convention_file"][0],
        "artifacts"
    );
    assert_eq!(
        result["verification"]["excluded_by_operator"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    // A command-line exclusion adds to the file and keeps its own origin.
    let both = f.run(&[
        "--store",
        s(&store),
        "--json",
        "checkpoint",
        s(&f.repo),
        "--exclude",
        "scratch",
    ]);
    let both = json(&both)["result"].clone();
    assert_eq!(reasons(&both, EXCLUDED_BY_OPERATOR), ["scratch"]);
    assert_eq!(reasons(&both, EXCLUDED_BY_CONVENTION_FILE), ["artifacts"]);

    // Verify, restore and assessment treat convention exclusions as declared paths.
    let store = Store::existing(&store).unwrap();
    let id = result["checkpoint"]["id"].as_str().unwrap();
    let verification = restore::verify(&store, id).unwrap();
    assert_eq!(verification.excluded_by_convention_file, ["artifacts"]);
    assert!(
        verification
            .notes
            .iter()
            .any(|n| n.contains("timber.toml excluded 1 paths"))
    );
    let destination = f.path("restored");
    restore::restore(&store, id, &destination).unwrap();
    assert!(!destination.join("artifacts").exists());
    assert!(destination.join("scratch/notes.txt").exists());
    assert_eq!(
        fs::read_to_string(destination.join("timber.toml")).unwrap(),
        fs::read_to_string(f.repo.join("timber.toml")).unwrap()
    );
    let again = capture::checkpoint_with_convention(
        &store,
        &f.repo,
        &[],
        &[],
        &["artifacts".to_owned()],
        false,
    )
    .unwrap();
    let plan = planning::create(&store, working(id), vec![working(&again.id)]).unwrap();
    let assessment = planning::assess(&store, &plan).unwrap().to_string();
    assert!(assessment.contains("excluded_paths_are_not_preserved"));
    assert!(!assessment.contains("source_contents_changed_since_checkpoint"));
    assert!(!assessment.contains("ignored_path_coverage_changed_since_checkpoint"));
}

#[test]
fn convention_exclusions_keep_every_exclude_refusal() {
    let f = Fixture::new();
    let store = Store::open(&f.path("store")).unwrap();
    for (paths, expected) in [
        (
            vec!["docs"],
            "timber.toml exclude refused for tracked path: docs/guide.md (named by timber.toml exclude docs)",
        ),
        (
            vec!["missing"],
            "timber.toml exclude matches no path in the worktree: missing",
        ),
    ] {
        let paths = paths.into_iter().map(str::to_owned).collect::<Vec<_>>();
        let error = capture::checkpoint_with_convention(&store, &f.repo, &[], &[], &paths, false)
            .unwrap_err();
        assert!(format!("{error:#}").contains(expected), "{error:#}");
    }
    // Overlap between a command-line and a convention exclusion that was not
    // resolved as an override is refused, naming both origins.
    let error = capture::checkpoint_with_convention(
        &store,
        &f.repo,
        &[],
        &["artifacts/run-1".to_owned()],
        &["artifacts".to_owned()],
        false,
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains(
            "exclusions repeat or overlap: artifacts/run-1 (--exclude) and artifacts (timber.toml exclude)"
        ),
        "{error:#}"
    );
    assert!(store.list().unwrap().is_empty());

    // Through the command line, a tracked path in the file refuses the run.
    f.convention("exclude = [\"docs\"]\n");
    let out = f.run(&["--store", s(&f.path("cli-store")), "checkpoint", s(&f.repo)]);
    assert_eq!(out.status.code(), Some(3));
    assert!(stderr(&out).contains("timber.toml exclude refused for tracked path"));
    // As with --exclude, the store may exist but holds no checkpoint or marker.
    let cli_store = Store::existing(&f.path("cli-store")).unwrap();
    assert!(cli_store.list().unwrap().is_empty());
    assert_eq!(
        fs::read_dir(cli_store.root.join("incomplete"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn command_line_flags_override_the_convention_file() {
    let f = Fixture::new();
    fs::write(f.repo.join(".gitignore"), "cache/\n").unwrap();
    git::run(&f.repo, &["add", ".gitignore"]).unwrap();
    fs::create_dir(f.repo.join("cache")).unwrap();
    fs::write(f.repo.join("cache/model.bin"), "valuable ignored bytes\n").unwrap();
    fs::write(f.repo.join("cache/tmp.bin"), "disposable\n").unwrap();
    f.convention("exclude = [\"artifacts\", \"scratch\", \"cache\"]\ngit_timeout_seconds = 300\n");
    let store = f.path("store");

    let plain = json(&f.run(&["--store", s(&store), "--json", "checkpoint", s(&f.repo)]));
    let plain = &plain["result"]["convention"];
    assert_eq!(plain["git_timeout_seconds"]["value"], 300);
    assert_eq!(plain["git_timeout_seconds"]["from"], "convention_file");

    // The same path on the command line wins: recorded as the operator's choice.
    let out = f.run(&[
        "--store",
        s(&store),
        "--git-timeout",
        "45",
        "--json",
        "checkpoint",
        s(&f.repo),
        "--exclude",
        "artifacts",
    ]);
    let result = json(&out)["result"].clone();
    assert_eq!(reasons(&result, EXCLUDED_BY_OPERATOR), ["artifacts"]);
    assert_eq!(
        reasons(&result, EXCLUDED_BY_CONVENTION_FILE),
        ["scratch", "cache"]
    );
    let report = &result["convention"];
    assert_eq!(report["git_timeout_seconds"]["value"], 45);
    assert_eq!(report["git_timeout_seconds"]["from"], "command_line");
    assert_eq!(report["exclusions_overridden"][0]["path"], "artifacts");
    assert_eq!(
        report["exclusions_overridden"][0]["by"],
        "--exclude artifacts"
    );

    // An explicit --include-ignored selection overrides an overlapping convention
    // exclusion for that run: the selected ignored file is captured, the rest of the
    // folder is an ordinary ignored omission, and the override is reported.
    let selected = f.run(&[
        "--store",
        s(&store),
        "--json",
        "checkpoint",
        s(&f.repo),
        "--include-ignored",
        "cache/model.bin",
    ]);
    let selected = json(&selected)["result"].clone();
    assert_eq!(
        reasons(&selected, EXCLUDED_BY_CONVENTION_FILE),
        ["artifacts", "scratch"]
    );
    assert_eq!(
        reasons(&selected, "ignored file not explicitly selected"),
        ["cache/tmp.bin"]
    );
    assert!(
        selected["checkpoint"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["path"] == "cache/model.bin")
    );
    assert_eq!(
        selected["convention"]["exclusions_overridden"][0]["by"],
        "--include-ignored cache/model.bin"
    );

    // A command-line exclusion strictly inside a convention exclusion is refused.
    let inside = f.run(&[
        "--store",
        s(&store),
        "checkpoint",
        s(&f.repo),
        "--exclude",
        "artifacts/run-1",
    ]);
    assert_eq!(inside.status.code(), Some(3));
    assert!(
        stderr(&inside)
            .contains("--exclude artifacts/run-1 lies inside the timber.toml exclusion artifacts"),
        "{}",
        stderr(&inside)
    );
}

#[test]
fn unknown_key_is_refused_naming_the_key() {
    let f = Fixture::new();
    f.convention("store = \"/tmp/never-used\"\nretention_days = 30\n");
    let store = f.path("store");
    for args in [
        vec!["--store", s(&store), "checkpoint", s(&f.repo)],
        vec!["inspect", "--repo", s(&f.repo)],
    ] {
        let out = f.run(&args);
        assert_eq!(out.status.code(), Some(3), "{args:?}");
        let message = stderr(&out);
        assert!(
            message.contains("unknown key in timber.toml: retention_days (line 2)"),
            "{message}"
        );
        assert!(message.contains("timber.toml"), "{message}");
    }
    assert!(!store.exists());
    assert!(!f.path("decoy-store").exists());
}

#[test]
fn store_from_file_is_used_without_store_flag_and_store_flag_wins() {
    let f = Fixture::new();
    let team_store = f.path("team-store");
    f.convention(&format!(
        "store = \"{}\"\nexclude = [\"artifacts\"]\n",
        s(&team_store)
    ));
    // The file's store beats TIMBER_STORE (a decoy here) when --store is absent.
    let out = f.run(&["--json", "checkpoint", s(&f.repo)]);
    let result = json(&out)["result"].clone();
    assert_eq!(result["convention"]["store"]["from"], "convention_file");
    assert_eq!(result["convention"]["store"]["path"], s(&team_store));
    assert_eq!(
        Store::existing(&team_store).unwrap().list().unwrap().len(),
        1
    );
    assert!(!f.path("decoy-store").exists());

    // --store wins when present; the file's store is not touched.
    let flag_store = f.path("flag-store");
    let out = f.run(&[
        "--store",
        s(&flag_store),
        "--json",
        "checkpoint",
        s(&f.repo),
    ]);
    let result = json(&out)["result"].clone();
    assert_eq!(result["convention"]["store"]["from"], "command_line");
    assert_eq!(
        Store::existing(&flag_store).unwrap().list().unwrap().len(),
        1
    );
    assert_eq!(
        Store::existing(&team_store).unwrap().list().unwrap().len(),
        1
    );

    // An environment-variable reference resolves from the caller's environment.
    let g = Fixture::new();
    g.convention("store = \"${TIMBER_FIXTURE_TEAM_STORE}/shared\"\nexclude = [\"artifacts\"]\n");
    let base = g.path("env-base");
    let out = g.run_env(
        &["--json", "checkpoint", s(&g.repo)],
        &[("TIMBER_FIXTURE_TEAM_STORE", &base)],
    );
    let result = json(&out)["result"].clone();
    assert_eq!(
        result["convention"]["store"]["path"],
        s(&base.join("shared"))
    );
    assert!(base.join("shared").exists());
    // Unset, it is refused by name before any store is created.
    let unset = g.run(&["checkpoint", s(&g.repo)]);
    assert_eq!(unset.status.code(), Some(3));
    assert!(stderr(&unset).contains("environment variable TIMBER_FIXTURE_TEAM_STORE"));
    assert!(!g.path("decoy-store").exists());

    // A store inside the source is still refused, wherever it came from.
    let h = Fixture::new();
    h.convention(&format!(
        "store = \"{}\"\nexclude = [\"artifacts\"]\n",
        s(&h.repo.join("inside-store"))
    ));
    assert!(!h.run(&["checkpoint", s(&h.repo)]).status.success());
    assert!(!h.repo.join("inside-store").exists());
}

#[test]
fn inspect_and_checkpoint_say_when_no_convention_file_exists() {
    let f = Fixture::new();
    let inspect = f.run(&["--json", "inspect", "--repo", s(&f.repo)]);
    assert!(
        stderr(&inspect).contains("no convention file found at"),
        "{}",
        stderr(&inspect)
    );
    let inspect = json(&inspect);
    assert_eq!(inspect["result"]["convention"]["status"], "not_found");
    assert_eq!(
        inspect["result"]["convention"]["git_timeout_seconds"]["from"],
        "default"
    );
    let text = f.run(&["inspect", "--repo", s(&f.repo)]);
    assert!(String::from_utf8_lossy(&text.stdout).contains("Status: not_found"));

    let out = f.run(&[
        "--store",
        s(&f.path("store")),
        "--json",
        "checkpoint",
        s(&f.repo),
        "--exclude",
        "artifacts",
    ]);
    assert!(stderr(&out).contains("no convention file found at"));
    let result = json(&out)["result"].clone();
    assert_eq!(result["convention"]["status"], "not_found");
    assert_eq!(reasons(&result, EXCLUDED_BY_OPERATOR), ["artifacts"]);
    assert!(reasons(&result, EXCLUDED_BY_CONVENTION_FILE).is_empty());
    assert!(
        result["verification"]
            .get("excluded_by_convention_file")
            .is_none()
    );
}

#[test]
fn convention_exclusions_coexist_with_linked_worktrees_inside() {
    let f = Fixture::new();
    f.convention("exclude = [\"artifacts\"]\n");
    let linked = f.repo.join("agents/one");
    fs::create_dir(f.repo.join("agents")).unwrap();
    git::run(
        &f.repo,
        &["worktree", "add", "-q", "-b", "agent-one", s(&linked)],
    )
    .unwrap();
    let store = f.path("store");
    let out = f.run(&["--store", s(&store), "--json", "checkpoint", s(&f.repo)]);
    let result = json(&out)["result"].clone();
    assert_eq!(reasons(&result, EXCLUDED_BY_CONVENTION_FILE), ["artifacts"]);
    assert_eq!(
        reasons(&result, LINKED_WORKTREE_CAPTURED_SEPARATELY),
        ["agents/one"]
    );
    assert!(reasons(&result, EXCLUDED_BY_OPERATOR).is_empty());
    assert_eq!(
        result["verification"]["excluded_by_convention_file"][0],
        "artifacts"
    );
    assert_eq!(
        result["verification"]["linked_worktrees_captured_separately"][0],
        "agents/one"
    );

    // Assessment prunes all three declared reasons alike.
    let store = Store::existing(&store).unwrap();
    let id = result["checkpoint"]["id"].as_str().unwrap();
    let again = capture::checkpoint_with_convention(
        &store,
        &f.repo,
        &[],
        &[],
        &["artifacts".to_owned()],
        false,
    )
    .unwrap();
    let plan = planning::create(&store, working(id), vec![working(&again.id)]).unwrap();
    let assessment = planning::assess(&store, &plan).unwrap().to_string();
    assert!(!assessment.contains("source_contents_changed_since_checkpoint"));
    assert!(!assessment.contains("ignored_path_coverage_changed_since_checkpoint"));

    // A convention entry strictly inside a linked worktree is refused, naming its origin.
    let error = capture::checkpoint_with_convention(
        &store,
        &f.repo,
        &[],
        &[],
        &["artifacts".to_owned(), "agents/one/docs".to_owned()],
        false,
    )
    .unwrap_err();
    assert!(
        format!("{error:#}")
            .contains("timber.toml exclude agents/one/docs lies inside linked worktree agents/one"),
        "{error:#}"
    );
}

#[test]
fn convention_capture_refused_by_foreign_nested_repository_keeps_journaled_marker() {
    let f = Fixture::new();
    // The convention excludes only `scratch/`; the foreign nested repository below
    // `artifacts/` is not covered, so the capture walk refuses it after the marker.
    f.convention("exclude = [\"scratch/\"]\n");
    let store = f.path("store");
    let out = f.run(&["--store", s(&store), "checkpoint", s(&f.repo)]);
    assert!(!out.status.success());
    let message = stderr(&out);
    assert!(message.contains("nested repository refused"), "{message}");
    assert!(message.contains("artifacts/run-1"), "{message}");
    let markers = fs::read_dir(store.join("incomplete"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("capture-"))
        })
        .collect::<Vec<_>>();
    assert_eq!(markers.len(), 1);
    let id = markers[0].file_stem().unwrap().to_str().unwrap().to_owned();
    assert!(
        message.contains(&format!("incomplete marker {id}")),
        "{message}"
    );
    assert!(message.contains("`timber gc --dry-run`"), "{message}");
    let bytes = fs::read(&markers[0]).unwrap();
    let header = serde_json::Deserializer::from_slice(&bytes)
        .into_iter::<Value>()
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(header["journal"], 1);
    let opened = Store::existing(&store).unwrap();
    assert!(opened.list().unwrap().is_empty());
    let report = timber_worktrees::gc::dry_run(&opened).unwrap();
    assert_eq!(report.incomplete_markers.len(), 1);
    assert_eq!(report.incomplete_markers[0].id, id);
    assert_eq!(
        report.incomplete_markers[0].journal,
        timber_worktrees::gc::JournalState::Present
    );
}
