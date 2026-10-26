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
