use serde_json::json;
use timber_worktrees::report;

#[test]
fn html_is_self_contained_and_escapes_untrusted_labels() {
    let value = json!({"schema":1,"command":"inspect <img src=x onerror=alert(1)>","result":{"status":"blocked <script>alert(1)</script>","notes":["<img src=x onerror=alert(1)>","'quoted' & \"double\""],"source_path":"../<svg onload=alert(1)>"}});
    let html = report::render(&value);
    assert!(html.starts_with("<!doctype html>"));
    assert!(html.contains("Content-Security-Policy"));
    assert!(html.contains("&lt;script&gt;"));
    assert!(html.contains("&lt;img src=x onerror=alert(1)&gt;"));
    assert!(!html.contains("<script"));
    assert!(!html.contains("<img"));
    assert!(!html.contains("<svg"));
    assert!(!html.contains("<link"));
    assert!(!html.contains("href="));
}

#[test]
fn comparison_keeps_sources_and_alternates_visible() {
    let html = report::render(
        &json!({"schema":1,"target":{"checkpoint":"target"},"sources":[{"checkpoint":"source-a"}],"comparisons":[{"checkpoint":"source-a","already_in_target_history":true,"unique_commits":["1234"],"identical_paths":["same.rs"],"differing_paths":["changed.rs"],"staged_alternates":["alternate.rs"],"notes":["Review source order"]}]}),
    );
    for text in [
        "What is unique or different?",
        "What is already represented?",
        "source-a",
        "changed.rs",
        "alternate.rs",
        "Before cleanup",
    ] {
        assert!(html.contains(text), "missing {text}");
    }
    assert!(html.contains("already in the target"));
    assert!(html.contains("retained separately"));
}

#[test]
fn failed_verification_never_looks_successful() {
    let value = json!({"verified":false,"checkpoint":"broken","notes":["object digest mismatch"]});
    assert!(report::render(&value).contains("Recovery verification did not pass"));
    assert!(report::terminal(&value).contains("Recovery verification did not pass"));
}

#[test]
fn terminal_escapes_control_sequences() {
    let result = report::terminal(&json!({"notes":["\u{1b}[31mred\nnext"]}));
    assert!(!result.contains('\u{1b}'));
    assert!(result.contains("\\u{1b}[31mred\\nnext"));
}

#[test]
fn partial_preview_keeps_pending_sources_and_conflicts_visible() {
    let html = report::render(
        &json!({"status":"conflicted","completed_sources":["a"],"pending_sources":["b","c"],"conflict_records":["CONFLICT (binary)"]}),
    );
    assert!(html.contains("conflicted"));
    assert!(html.contains("Pending sources"));
    assert!(html.contains("CONFLICT (binary)"));
}

#[test]
fn arbitrary_json_values_are_renderable() {
    for value in [
        json!(null),
        json!([]),
        json!({}),
        json!("<unsafe>"),
        json!([1, true]),
    ] {
        assert!(report::render(&value).contains("Complete recorded evidence"));
        assert!(!report::terminal(&value).is_empty());
    }
}

#[test]
fn inspection_exposes_counts_and_blockers_before_raw_evidence() {
    let html = report::render(
        &json!({"worktrees":[{"path":"agent-a","kind":"registered_worktree","staged_paths":["a.rs"],"working_paths":["b.rs"],"untracked":[],"ignored":[{"path":"cache.bin"}],"blockers":["source is changing"]}],"folders":[{"path":"saved-a","kind":"unmanaged_folder","entries":3,"historical_completeness":"unknown"}]}),
    );
    assert!(html.contains("Worktrees to review"));
    assert!(html.contains("Staged changes"));
    assert!(html.contains("source is changing"));
    assert!(html.contains("Preserved folders to review"));
    assert!(html.find("agent-a").unwrap() < html.find("Complete recorded evidence").unwrap());
}

#[test]
fn inspection_summary_exposes_scope_payload_and_dependency_status_without_raw_expansion() {
    let value = json!({"status":"partial","elapsed_seconds":0.75,"registration_stable":true,"scope":{"mode":"selected_worktrees","registered_worktrees":4,"deep_scanned_worktrees":1,"listed_only_worktrees":2,"missing_registrations":1,"metadata_prune_candidates":1,"failed_or_partial_scans":1},"folders":[{"path":"saved-fixture","kind":"preservation_folder_with_receipt","inspection_status":"partial","comparison_requested":true,"summary":{"payload_files":6,"same_path_identical":2,"same_path_different":1,"absent_at_proposed_path":3,"saved_git_versions":2},"git_dependencies":[{"group":"saved-a","status":"matched","object_available":true},{"group":"saved-b","status":"hash_mismatch","object_available":true},{"group":"saved-c","status":"missing","object_available":false}]}],"consolidation_summary":{"payload":{"payload_files":6,"same_path_identical":2,"same_path_different":1,"absent_at_proposed_path":3},"tracked_git_versions":2,"tracked_git_comparisons":{"same_path_different":2},"unique_proposed_target_paths":5,"exact_duplicate_saved_group_count":1,"duplicate_extra_copies":1}});
    let html = report::render(&value);
    let before = html.split("Complete recorded evidence").next().unwrap();
    for text in [
        "Inspection scope and timing",
        "Worktrees inspected deeply",
        "Other worktrees listed only",
        "Elapsed seconds",
        "Consolidation candidates across saved folders",
        "Same path, bytes and executable state",
        "Competing current versions",
        "Absent at proposed target path",
        "Saved-content fingerprint mismatches",
        "Unresolved dependencies",
        "saved-b",
        "Hash mismatch",
        "not an approved deletion list",
    ] {
        assert!(before.contains(text), "missing {text}");
    }
    assert!(html.contains("class=\"status warn\""));
    let terminal = report::terminal(&value);
    assert!(terminal.contains("1 deeply inspected; 2 listed only; 1 missing"));
    assert!(
        terminal.contains("Matching copies: 2; competing versions: 1; absent at proposed path: 3")
    );
    assert!(terminal.contains("3 declared; 2 available; 1 content mismatches; 2 unresolved"));
}

#[test]
fn metadata_only_folder_report_does_not_present_zero_matches_as_a_completed_comparison() {
    let html = report::render(
        &json!({"folders":[{"path":"saved-fixture","summary":{"payload_files":5},"comparison_requested":false,"git_dependencies":[]}]}),
    );
    let before = html.split("Complete recorded evidence").next().unwrap();
    assert!(before.contains("Payload content comparison was not requested"));
    assert!(!before.contains("Same path, bytes and executable state"));
}

#[test]
fn html_names_the_generating_timber_version() {
    let html =
        report::render(&json!({"schema":1,"command":"inspect","result":{"status":"complete"}}));
    assert!(html.contains(&format!(
        "Generated by Timber {}",
        env!("CARGO_PKG_VERSION")
    )));
    assert!(html.contains("https://github.com/MendozaLab/Timber"));
}
