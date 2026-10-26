use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};
use timber_worktrees::{fsx, git, recovery};

fn g(repo: &Path, args: &[&str]) -> String {
    git::text(repo, args).unwrap()
}
fn repo(path: &Path) {
    fs::create_dir(path).unwrap();
    git::init(path).unwrap();
    g(path, &["config", "user.name", "Fixture Developer"]);
    g(path, &["config", "user.email", "fixture@example.invalid"]);
    fs::write(path.join("notes.txt"), "base\n").unwrap();
    g(path, &["add", "notes.txt"]);
    g(path, &["commit", "-qm", "base"]);
}
fn write(root: &Path, path: &str, bytes: &[u8]) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}
fn metadata(bytes: &[u8]) -> Value {
    json!({"type":"file","mode":420,"bytes":bytes.len(),"sha256":fsx::digest(bytes)})
}
fn receipt(root: &Path, value: &Value) {
    fs::write(
        root.join("receipt.json"),
        serde_json::to_vec_pretty(value).unwrap(),
    )
    .unwrap();
}

#[test]
fn unmanaged_folder_separates_identical_different_and_absent_paths_without_mutation() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path().join("repo");
    repo(&r);
    let f = d.path().join("saved");
    fs::create_dir(&f).unwrap();
    write(&r, "same.txt", b"same");
    write(&r, "different.txt", b"current");
    write(&f, "same.txt", b"same");
    write(&f, "different.txt", b"saved");
    write(&f, "absent.txt", b"new");
    let source_before = fsx::inventory(&r).unwrap();
    let folder_before = fsx::inventory(&f).unwrap();
    let index = fs::read(r.join(".git/index")).unwrap();
    let result = recovery::analyze_folder(&r, &f, true).unwrap();
    assert_eq!(result["summary"]["same_path_identical"], 1);
    assert_eq!(result["summary"]["same_path_different"], 1);
    assert_eq!(result["summary"]["absent_at_proposed_path"], 1);
    assert_eq!(result["historical_completeness"], "unknown");
    assert_eq!(result["cleanup_candidates"]["may_delete"], false);
    assert_eq!(source_before, fsx::inventory(&r).unwrap());
    assert_eq!(folder_before, fsx::inventory(&f).unwrap());
    assert_eq!(index, fs::read(r.join(".git/index")).unwrap());
}

#[test]
fn metadata_only_mode_does_not_read_unreadable_payload() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("saved");
    fs::create_dir(&f).unwrap();
    write(&f, "unreadable", b"private fixture");
    fs::set_permissions(f.join("unreadable"), fs::Permissions::from_mode(0o000)).unwrap();
    let result = recovery::analyze_folder(d.path(), &f, false).unwrap();
    assert_eq!(result["summary"]["payload_files"], 1);
    assert_eq!(result["summary"]["not_requested"], 1);
    assert!(result["payload_rows"][0].get("sha256").is_none());
    fs::set_permissions(f.join("unreadable"), fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn file_manifest_checks_saved_hashes_and_executable_bits() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path().join("repo");
    repo(&r);
    let f = d.path().join("saved");
    fs::create_dir(&f).unwrap();
    write(&f, "same.txt", b"same");
    write(&r, "same.txt", b"same");
    write(&f, "mode.txt", b"mode");
    write(&r, "mode.txt", b"mode");
    fs::set_permissions(f.join("mode.txt"), fs::Permissions::from_mode(0o755)).unwrap();
    let mut mode = metadata(b"mode");
    mode["mode"] = json!(493);
    let manifest = json!({"same.txt":metadata(b"same"),"mode.txt":mode});
    receipt(
        &f,
        &json!({"source_before":manifest,"copied":manifest,"source_head":g(&r,&["rev-parse","HEAD"]),"copy_verified":true}),
    );
    let result = recovery::analyze_folder(&r, &f, true).unwrap();
    assert_eq!(result["summary"]["payload_files"], 2);
    assert_eq!(result["summary"]["same_path_identical"], 1);
    assert_eq!(result["summary"]["same_path_different"], 1);
    assert!(
        result["payload_rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["receipt_match"] == "matched")
    );
    assert_eq!(result["source_heads"][0]["available_as_commit"], true);
    assert_eq!(
        recovery::map_from_receipt(&f).unwrap().unwrap().files.len(),
        2
    );
}

fn stash_fixture() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    String,
) {
    let d = tempfile::tempdir().unwrap();
    let r = d.path().join("repo");
    repo(&r);
    let f = d.path().join("saved");
    fs::create_dir(&f).unwrap();
    let head = g(&r, &["rev-parse", "HEAD"]);
    write(&r, "notes.txt", b"staged version\n");
    g(&r, &["add", "notes.txt"]);
    write(&r, "notes.txt", b"saved working version\n");
    let oid = g(&r, &["stash", "create"]);
    g(&r, &["update-ref", "refs/heads/saved/worker-a", &oid]);
    write(&f, "worker-a/untracked/extra.txt", b"extra\n");
    receipt(
        &f,
        &json!({"results":[{"label":"worker-a","head":head,"tracked":["notes.txt"],"untracked":["extra.txt"],"ignored_source_media":[],"before":{"notes.txt":metadata(b"saved working version\n"),"extra.txt":metadata(b"extra\n")},"stash_sha":oid,"ref":"refs/heads/saved/worker-a","output_dir":"/untrusted/producer/output"}]}),
    );
    write(&r, "notes.txt", b"current main\n");
    (d, r, f, oid)
}

#[test]
fn grouped_receipt_identifies_external_stash_layers_and_compares_saved_work() {
    let (_d, r, f, oid) = stash_fixture();
    let before = fs::read(r.join(".git/index")).unwrap();
    let result = recovery::analyze_folder(&r, &f, true).unwrap();
    let dep = &result["git_dependencies"][0];
    assert_eq!(dep["oid"], oid);
    assert_eq!(dep["status"], "matched");
    assert_eq!(dep["receipt_hashes_verified"], true);
    assert_eq!(dep["self_contained_capture"], false);
    assert_eq!(dep["tracked_rows"][0]["comparison"], "same_path_different");
    assert_eq!(dep["tracked_rows"][0]["index_matches_base"], false);
    assert_eq!(
        dep["tracked_rows"][0]["layers"]["index"]["sha256"],
        fsx::digest(b"staged version\n")
    );
    assert_eq!(
        dep["tracked_rows"][0]["layers"]["working"]["sha256"],
        fsx::digest(b"saved working version\n")
    );
    assert_eq!(result["summary"]["saved_git_versions"], 1);
    assert_eq!(result["summary"]["absent_at_proposed_path"], 1);
    let map = recovery::map_from_receipt(&f).unwrap().unwrap();
    assert_eq!(map.git_dependencies[0].tracked_paths, vec!["notes.txt"]);
    assert_eq!(map.files[0].stored_path, "worker-a/untracked/extra.txt");
    assert_eq!(before, fs::read(r.join(".git/index")).unwrap());
}

#[test]
fn moved_reference_and_receipt_hash_mismatch_are_not_reported_as_verified() {
    let (_d, r, f, _) = stash_fixture();
    let head = g(&r, &["rev-parse", "HEAD"]);
    g(&r, &["update-ref", "refs/heads/saved/worker-a", &head]);
    let result = recovery::analyze_folder(&r, &f, true).unwrap();
    assert_eq!(result["git_dependencies"][0]["status"], "ref_mismatch");
    let mut value: Value =
        serde_json::from_slice(&fs::read(f.join("receipt.json")).unwrap()).unwrap();
    value["results"][0]["ref"] = Value::Null;
    value["results"][0]["before"]["notes.txt"]["sha256"] = json!("0".repeat(64));
    receipt(&f, &value);
    let result = recovery::analyze_folder(&r, &f, true).unwrap();
    assert_eq!(result["git_dependencies"][0]["status"], "hash_mismatch");
    assert_eq!(
        result["git_dependencies"][0]["receipt_hashes_verified"],
        false
    );
}

#[test]
fn missing_dependency_is_visible_while_present_payload_stays_useful() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path().join("repo");
    repo(&r);
    let f = d.path().join("saved");
    fs::create_dir(&f).unwrap();
    write(&f, "saved.txt", b"saved");
    fs::write(f.join("import-map.json"),serde_json::to_vec(&json!({"schema":1,"files":[{"stored_path":"saved.txt","target_path":"new.txt"}],"git_dependencies":[{"group":"worker","oid":"0".repeat(40),"kind":"stash"}]})).unwrap()).unwrap();
    let result = recovery::analyze_folder(&r, &f, true).unwrap();
    assert_eq!(result["git_dependencies"][0]["status"], "missing");
    assert_eq!(result["summary"]["absent_at_proposed_path"], 1);
    assert_eq!(
        result["cleanup_candidates"]["unresolved_git_dependencies"],
        1
    );
}

#[test]
fn receipt_path_traversal_is_rejected_without_reading_outside_folder() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path().join("repo");
    repo(&r);
    let f = d.path().join("saved");
    fs::create_dir(&f).unwrap();
    write(&f, "kept", b"safe");
    receipt(
        &f,
        &json!({"source_before":{"../outside":metadata(b"outside")},"copy_verified":true}),
    );
    let result = recovery::analyze_folder(&r, &f, true).unwrap();
    assert!(!result["blockers"].as_array().unwrap().is_empty());
    assert_eq!(result["payload_rows"][0]["comparison"], "unmapped");
    assert!(recovery::map_from_receipt(&f).is_err());
}

#[test]
fn mapped_missing_files_and_extra_unmapped_files_remain_visible() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path().join("repo");
    repo(&r);
    let f = d.path().join("saved");
    fs::create_dir(&f).unwrap();
    write(&f, "extra", b"extra");
    receipt(
        &f,
        &json!({"source_before":{"missing":metadata(b"missing")}}),
    );
    let result = recovery::analyze_folder(&r, &f, true).unwrap();
    assert_eq!(result["summary"]["unmapped"], 1);
    assert_eq!(result["summary"]["saved_file_missing"], 1);
    assert_eq!(result["cleanup_candidates"]["may_delete"], false);
}

#[test]
fn invalid_canonical_map_does_not_fall_back_to_guessed_targets() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path().join("repo");
    repo(&r);
    let f = d.path().join("saved");
    fs::create_dir(&f).unwrap();
    write(&f, "saved.txt", b"saved");
    fs::write(
        f.join("import-map.json"),
        serde_json::to_vec(
            &json!({"schema":1,"files":[{"stored_path":"saved.txt","target_path":"../outside"}]}),
        )
        .unwrap(),
    )
    .unwrap();
    let result = recovery::analyze_folder(&r, &f, true).unwrap();
    assert!(!result["blockers"].as_array().unwrap().is_empty());
    assert_eq!(result["payload_rows"][0]["comparison"], "unmapped");
    assert!(result["payload_rows"][0].get("sha256").is_none());
}

#[test]
fn target_symlink_ancestor_is_not_followed_for_comparison() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path().join("repo");
    repo(&r);
    let f = d.path().join("saved");
    fs::create_dir(&f).unwrap();
    let outside = d.path().join("outside");
    fs::create_dir(&outside).unwrap();
    write(&f, "link/secret", b"saved");
    write(&outside, "secret", b"external fixture contents");
    std::os::unix::fs::symlink(&outside, r.join("link")).unwrap();
    let result = recovery::analyze_folder(&r, &f, true).unwrap();
    assert_eq!(result["summary"]["unsupported"], 1);
    assert!(
        !result
            .to_string()
            .contains(&fsx::digest(b"external fixture contents"))
    );
    assert!(!result.to_string().contains("external fixture contents"));
}

#[test]
fn structural_receipts_normalize_only_regular_file_spelling() {
    let (_d, r, f, _) = stash_fixture();
    let mut value: Value =
        serde_json::from_slice(&fs::read(f.join("receipt.json")).unwrap()).unwrap();
    for fingerprint in value["results"][0]["before"]
        .as_object_mut()
        .unwrap()
        .values_mut()
    {
        fingerprint["type"] = json!("regular");
    }
    receipt(&f, &value);
    let result = recovery::analyze_folder(&r, &f, true).unwrap();
    assert!(result["blockers"].as_array().unwrap().is_empty());
    assert_eq!(result["summary"]["absent_at_proposed_path"], 1);
    assert_eq!(result["git_dependencies"][0]["status"], "matched");
    let map = recovery::map_from_receipt(&f).unwrap().unwrap();
    assert_eq!(
        map.git_dependencies[0].expected_files["notes.txt"].kind,
        "file"
    );

    let single = f.join("single");
    fs::create_dir(&single).unwrap();
    write(&single, "notes.txt", b"current main\n");
    let mut fingerprint = metadata(b"current main\n");
    fingerprint["type"] = json!("regular");
    receipt(&single, &json!({"source_before":{"notes.txt":fingerprint}}));
    let result = recovery::analyze_folder(&r, &single, true).unwrap();
    assert_eq!(result["summary"]["same_path_identical"], 1);
    assert_eq!(result["payload_rows"][0]["receipt_match"], "matched");
}

#[test]
fn regular_alias_does_not_accept_unknown_kinds_or_unsafe_paths() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path().join("repo");
    repo(&r);
    let f = d.path().join("saved");
    fs::create_dir(&f).unwrap();
    write(&f, "notes.txt", b"saved");
    for unknown in ["socket", "REGULAR", "Regular", "regular-file"] {
        let mut fingerprint = metadata(b"saved");
        fingerprint["type"] = json!(unknown);
        receipt(&f, &json!({"source_before":{"notes.txt":fingerprint}}));
        assert!(recovery::map_from_receipt(&f).is_err());
        let result = recovery::analyze_folder(&r, &f, true).unwrap();
        assert!(!result["blockers"].as_array().unwrap().is_empty());
        assert_eq!(result["summary"]["unmapped"], 1);
    }
    let mut fingerprint = metadata(b"saved");
    fingerprint["type"] = json!("regular");
    receipt(&f, &json!({"source_before":{"../outside":fingerprint}}));
    assert!(recovery::map_from_receipt(&f).is_err());
}

#[test]
fn symlink_receipt_kind_still_compares_link_data_without_following_target() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path().join("repo");
    repo(&r);
    let f = d.path().join("saved");
    fs::create_dir(&f).unwrap();
    std::os::unix::fs::symlink("absent-target", f.join("link")).unwrap();
    std::os::unix::fs::symlink("absent-target", r.join("link")).unwrap();
    receipt(
        &f,
        &json!({"source_before":{"link":{"type":"symlink","mode":511,"bytes":13,"sha256":fsx::digest(b"absent-target")}}}),
    );
    let result = recovery::analyze_folder(&r, &f, true).unwrap();
    assert_eq!(result["summary"]["same_path_identical"], 1);
    assert_eq!(result["payload_rows"][0]["receipt_match"], "matched");
    assert!(!f.join("absent-target").exists());
    assert!(!r.join("absent-target").exists());
}
