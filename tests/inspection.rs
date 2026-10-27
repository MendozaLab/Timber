use std::{fs, path::Path};
use timber_worktrees::{
    git,
    inspect::{self, InspectOptions},
};

fn init(path: &Path) {
    fs::create_dir(path).unwrap();
    git::init(path).unwrap();
    git::run(path, &["config", "user.name", "Fixture Developer"]).unwrap();
    git::run(path, &["config", "user.email", "fixture@example.invalid"]).unwrap();
    fs::write(path.join("base.txt"), "base\n").unwrap();
    git::run(path, &["add", "base.txt"]).unwrap();
    git::run(path, &["commit", "-qm", "base"]).unwrap();
}
fn linked(repo: &Path, path: &Path, name: &str) {
    git::run(
        repo,
        &["worktree", "add", "-q", "-b", name, path.to_str().unwrap()],
    )
    .unwrap();
}

#[test]
fn default_scope_only_scans_requested_worktree_and_lists_other_registrations() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    init(&repo);
    let agent = temp.path().join("agent");
    linked(&repo, &agent, "agent");
    fs::write(agent.join("base.txt"), "agent unfinished\n").unwrap();
    let result = inspect::inspect(&repo, &[]).unwrap();
    assert_eq!(result["scope"]["registered_worktrees"], 2);
    assert_eq!(result["scope"]["deep_scanned_worktrees"], 1);
    assert_eq!(result["scope"]["listed_only_worktrees"], 1);
    assert_eq!(result["scope"]["mode"], "selected_worktrees");
    assert_eq!(result["worktrees"][1]["inspection_status"], "not_scanned");
    assert!(result["worktrees"][1].get("working_paths").is_none());
    let all = inspect::inspect_with_options(
        &repo,
        &[],
        &InspectOptions {
            all_worktrees: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(all["scope"]["deep_scanned_worktrees"], 2);
    assert_eq!(all["scope"]["listed_only_worktrees"], 0);
    assert_eq!(all["worktrees"][1]["working_paths"][0], "base.txt");
}

#[test]
fn explicit_worktree_selection_is_registered_and_does_not_hide_missing_rows() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    init(&repo);
    let agent = temp.path().join("agent");
    linked(&repo, &agent, "agent");
    let missing = temp.path().join("missing");
    linked(&repo, &missing, "missing");
    fs::rename(&missing, temp.path().join("held")).unwrap();
    let before = git::run(&repo, &["worktree", "list", "--porcelain", "-z"]).unwrap();
    let result = inspect::inspect_with_options(
        &repo,
        &[],
        &InspectOptions {
            worktrees: vec![agent],
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(result["scope"]["deep_scanned_worktrees"], 2);
    assert_eq!(result["scope"]["missing_registrations"], 1);
    assert_eq!(
        result["worktrees"][2]["kind"],
        "missing_worktree_registration"
    );
    assert_eq!(result["worktrees"][2]["cleanup_authorized"], false);
    assert_eq!(
        before,
        git::run(&repo, &["worktree", "list", "--porcelain", "-z"]).unwrap()
    );
    let ordinary = temp.path().join("ordinary");
    fs::create_dir(&ordinary).unwrap();
    assert!(
        inspect::inspect_with_options(
            &repo,
            &[],
            &InspectOptions {
                worktrees: vec![ordinary],
                ..Default::default()
            }
        )
        .is_err()
    );
}

#[test]
fn batch_comparison_preserves_unusual_names_and_executable_changes() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    init(&repo);
    let unusual = "line\nbreak\tname.txt";
    fs::write(repo.join(unusual), b"original\0bytes\n").unwrap();
    git::run(&repo, &["add", "--", unusual]).unwrap();
    git::run(&repo, &["commit", "-qm", "unusual path"]).unwrap();
    let raw_index = fs::read(repo.join(".git/index")).unwrap();
    let refs = git::run(&repo, &["show-ref"]).unwrap();
    fs::write(repo.join(unusual), b"changed\0bytes\n").unwrap();
    fs::set_permissions(repo.join("base.txt"), fs::Permissions::from_mode(0o755)).unwrap();
    let result = inspect::worktree(&repo).unwrap();
    assert_eq!(
        result["working_paths"],
        serde_json::json!(["base.txt", unusual])
    );
    assert_eq!(result["statistics"]["cat_file_batch_processes"], 1);
    assert_eq!(result["git_identity_stable"], true);
    assert_eq!(fs::read(repo.join(".git/index")).unwrap(), raw_index);
    assert_eq!(git::run(&repo, &["show-ref"]).unwrap(), refs);
}

#[test]
fn thousand_unique_blobs_use_one_batch_process() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    init(&repo);
    for i in 0..1000 {
        fs::write(
            repo.join(format!("fixture-{i:04}.txt")),
            format!("unique synthetic contents {i}\n"),
        )
        .unwrap();
    }
    git::run(&repo, &["add", "--all"]).unwrap();
    git::run(&repo, &["commit", "-qm", "large synthetic fixture"]).unwrap();
    let result = inspect::worktree(&repo).unwrap();
    assert_eq!(result["statistics"]["index_paths"], 1001);
    assert_eq!(result["statistics"]["unique_index_blob_objects"], 1001);
    assert_eq!(result["statistics"]["cat_file_batch_processes"], 1);
    assert_eq!(result["statistics"]["working_files_compared"], 1001);
    assert_eq!(result["working_paths"], serde_json::json!([]));
}

#[test]
fn selected_linked_repo_is_the_default_deep_scan_instead_of_the_main_checkout() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    init(&repo);
    let agent = temp.path().join("agent");
    linked(&repo, &agent, "agent");
    let result = inspect::inspect(&agent, &[]).unwrap();
    assert_eq!(result["worktrees"][0]["inspection_status"], "not_scanned");
    assert_eq!(result["worktrees"][1]["inspection_status"], "scanned");
    assert_eq!(result["scope"]["deep_scanned_worktrees"], 1);
}

#[test]
fn saved_folder_overlap_counts_actual_copies_once_and_keeps_categories_separate() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    init(&repo);
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    fs::write(first.join("base.txt"), "base\n").unwrap();
    fs::write(second.join("base.txt"), "base\n").unwrap();
    fs::write(first.join("new.txt"), "new candidate\n").unwrap();
    fs::write(second.join("new.txt"), "competing candidate\n").unwrap();
    let result = inspect::inspect_with_options(
        &repo,
        &[first.clone(), second, first],
        &InspectOptions {
            compare_folders: true,
            ..Default::default()
        },
    )
    .unwrap();
    let summary = &result["consolidation_summary"];
    assert_eq!(result["folders"].as_array().unwrap().len(), 2);
    assert_eq!(summary["payload"]["payload_files"], 4);
    assert_eq!(summary["payload"]["same_path_identical"], 2);
    assert_eq!(summary["payload"]["absent_at_proposed_path"], 2);
    assert_eq!(summary["unique_proposed_target_paths"], 2);
    assert_eq!(summary["exact_duplicate_saved_group_count"], 1);
    assert_eq!(summary["duplicate_extra_copies"], 1);
    assert_eq!(
        summary["exact_duplicate_saved_groups"][0]["target_path"],
        "base.txt"
    );
    assert_eq!(
        summary["exact_duplicate_saved_groups"][0]["members"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(summary["cleanup_authorized"], false);
}

#[test]
fn repository_subdirectory_keeps_folder_targets_relative_to_worktree_root() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    init(&repo);
    fs::create_dir(repo.join("subdir")).unwrap();
    let folder = temp.path().join("saved");
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("base.txt"), "base\n").unwrap();
    let result = inspect::inspect_with_options(
        &repo.join("subdir"),
        &[folder],
        &InspectOptions {
            compare_folders: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(result["folders"][0]["summary"]["same_path_identical"], 1);
    assert_eq!(
        result["target_worktree"],
        serde_json::to_value(repo.canonicalize().unwrap()).unwrap()
    );
}

#[test]
fn ignored_nested_git_directory_markers_are_listed_without_traversal_or_capture() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    init(&repo);
    fs::write(repo.join(".gitignore"), "nested-cache/\n").unwrap();
    git::run(&repo, &["add", ".gitignore"]).unwrap();
    git::run(&repo, &["commit", "-qm", "ignore local cache"]).unwrap();
    let nested = repo.join("nested-cache");
    fs::create_dir(&nested).unwrap();
    git::init(&nested).unwrap();
    fs::write(nested.join("payload.txt"), "contents not inspected\n").unwrap();
    let raw = git::run(
        &repo,
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "-z",
        ],
    )
    .unwrap();
    assert!(
        raw.split(|b| *b == 0)
            .any(|record| record == b"nested-cache/")
    );
    let before = fs::read(repo.join(".git/index")).unwrap();
    let nested_config = fs::read(nested.join(".git/config")).unwrap();
    let summary = inspect::inspect(&repo, &[]).unwrap();
    assert_eq!(
        summary["worktrees"][0]["ignored"]["top_dirs"][0]["path"],
        "nested-cache"
    );
    assert_eq!(summary["worktrees"][0]["ignored"]["count"], 1);
    let result = inspect::inspect_with_options(
        &repo,
        &[],
        &InspectOptions {
            list_ignored: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(result["status"], "complete");
    assert_eq!(result["worktrees"][0]["inspection_status"], "scanned");
    let ignored = result["worktrees"][0]["ignored"].as_array().unwrap();
    let directory = ignored
        .iter()
        .find(|row| row["path"] == "nested-cache")
        .expect("directory marker remains visible");
    assert_eq!(directory["kind"], "directory");
    assert!(
        !ignored
            .iter()
            .any(|row| row["path"] == "nested-cache/payload.txt")
    );
    assert!(directory.get("sha256").is_none());
    assert_eq!(before, fs::read(repo.join(".git/index")).unwrap());
    assert_eq!(nested_config, fs::read(nested.join(".git/config")).unwrap());
}
