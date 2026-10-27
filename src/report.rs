//! Human-readable views of the same records emitted by the command line.
//! Input strings are data, including filenames that resemble markup.
use serde_json::Value;
use std::fmt::Write as _;

fn payload(value: &Value) -> &Value {
    value.get("result").unwrap_or(value)
}

fn human_key(key: &str) -> String {
    let mut s = key.replace('_', " ");
    if let Some(first) = s.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    s
}

fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn scalar(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "Yes".into(),
        Value::Bool(false) => "No".into(),
        Value::Null => "Not established".into(),
        _ => value.to_string(),
    }
}

fn status(value: &Value) -> String {
    let body = payload(value);
    if value.get("error").is_some() || body.get("error").is_some() {
        return "Operation failed".into();
    }
    if let Some(s) = body.get("status").and_then(Value::as_str) {
        return s.replace('_', " ");
    }
    if let Some(verified) = body.get("verified").and_then(Value::as_bool) {
        return if verified {
            "Captured state restored and compared".into()
        } else {
            "Recovery verification did not pass".into()
        };
    }
    if body.get("comparisons").is_some() {
        return "Consolidation plan prepared".into();
    }
    if body.get("source_kind").is_some() && body.get("entries").is_some() {
        return "Checkpoint captured; verify recovery separately".into();
    }
    if body.get("worktrees").is_some() {
        return "Worktree inspection recorded".into();
    }
    "Recorded result; review its limits".into()
}

/// Render a result without loading scripts, assets, URLs, or project code.
pub fn render(value: &Value) -> String {
    let body = payload(value);
    let mut html = String::from(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'\">\
         <title>Timber — consolidation review</title><style>\
         :root{color-scheme:light dark;--paper:#f8f6f1;--ink:#26392d;--muted:#57655a;--line:#d9dfd6;--panel:#fff;--accent:#315b3f}\
         *{box-sizing:border-box}body{margin:0;background:var(--paper);color:var(--ink);font:16px/1.6 system-ui,sans-serif}\
         main{max-width:1080px;margin:auto;padding:40px 24px 64px}header{border-bottom:2px solid var(--accent);padding-bottom:24px;margin-bottom:28px}\
         .brand{font-size:15px;letter-spacing:.12em;text-transform:uppercase;color:var(--muted)}h1{font-size:clamp(28px,5vw,42px);line-height:1.18;margin:10px 0 16px}\
         h2{font-size:23px;line-height:1.3;margin:0 0 12px}h3{font-size:17px;margin:18px 0 8px}p{max-width:76ch}\
         .status{display:inline-block;background:#e6efe4;border:1px solid #b6cdb4;border-radius:6px;padding:7px 12px;font-weight:650}.status.warn{background:#fff1d6;border-color:#cba362;color:#67420a}\
         .grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(280px,1fr));gap:20px}.panel,details{background:var(--panel);border:1px solid var(--line);border-radius:9px;padding:22px;margin-bottom:20px;min-width:0}\
         .quiet{color:var(--muted)}ul{padding-left:22px}li{margin:8px 0;overflow-wrap:anywhere}code,pre{font:13px/1.55 ui-monospace,monospace;overflow-wrap:anywhere}\
         pre{white-space:pre-wrap;background:var(--paper);padding:14px;border-radius:5px}dl{display:grid;grid-template-columns:minmax(100px,1fr) minmax(140px,3fr);gap:9px 16px;margin:0}\
         dt{font-weight:600}dd{margin:0;overflow-wrap:anywhere}summary{cursor:pointer;font-weight:650}details[open]>summary{margin-bottom:16px}\
         .source{border-top:1px solid var(--line);padding-top:8px}\
         table{border-collapse:collapse;width:100%;margin:8px 0 12px}th,td{text-align:left;padding:6px 10px;border-bottom:1px solid var(--line);overflow-wrap:anywhere}td.num,th.num{text-align:right}.label{font-weight:650}footer{color:var(--muted);font-size:14px}\
         @media(prefers-color-scheme:dark){:root{--paper:#18221b;--ink:#e9efe6;--muted:#bac8b8;--line:#405341;--panel:#202e23;--accent:#9dc19e}.status{background:#2c4630;border-color:#56785a}}\
         @media(max-width:540px){main{padding:24px 16px}dl{display:block}dd{margin-bottom:12px}}\
         </style></head><body><main><header><div class=\"brand\">Timber!</div>\
         <h1>Untangle dirty worktrees.<br>Keep the useful work.</h1>",
    );
    let result_status = status(value);
    let warning = matches!(
        body.get("status").and_then(Value::as_str),
        Some("partial" | "conflicted" | "blocked" | "failed" | "incomplete")
    ) || body.get("verified") == Some(&Value::Bool(false))
        || value.get("error").is_some();
    let _ = write!(
        html,
        "<p class=\"status{}\">{}</p>",
        if warning { " warn" } else { "" },
        escape(&result_status)
    );
    if let Some(command) = value.get("command").and_then(Value::as_str) {
        let _ = write!(
            html,
            "<p class=\"quiet\">Result from <code>{}</code>.</p>",
            escape(command)
        );
    }
    html.push_str("<p>This is a local review of recorded evidence. A clean merge shows that selected changes combine mechanically; application behavior needs separate testing.</p></header>");
    render_inspection_scope(body, &mut html);
    render_consolidation_summary(body, &mut html);
    render_inventory(body, &mut html);
    render_comparisons(body, &mut html);
    html.push_str("<section class=\"panel\"><h2>What needs review?</h2>");
    let mut has_review = false;
    for key in [
        "blockers",
        "conflict_records",
        "pending_sources",
        "excluded",
        "notes",
    ] {
        if let Some(items) = body.get(key).filter(|v| !is_empty(v)) {
            has_review = true;
            let _ = write!(html, "<h3>{}</h3>", escape(&human_key(key)));
            render_value(items, &mut html, 0);
        }
    }
    if !has_review {
        html.push_str("<p>No review items were supplied in this record. That does not establish complete recovery or permission to clean up.</p>");
    }
    html.push_str("</section><section class=\"panel\"><h2>Before cleanup</h2><ul>\
        <li>Check that every change you want to keep is selected or preserved as a recoverable alternative.</li>\
        <li>Verify checkpoints and recheck the original sources for changes since capture.</li>\
        <li>Resolve exclusions, missing dependencies, conflicts, and application testing separately.</li>\
        </ul><p><strong>Timber does not move, delete, prune, or apply changes to source worktrees.</strong></p></section>");
    html.push_str("<details><summary>Complete recorded evidence</summary>");
    render_value(body, &mut html, 0);
    html.push_str("</details><footer>Captured bytes, historical completeness, independent backup, successful consolidation, and human acceptance are separate conclusions. Reports may contain private paths and filenames; review them before sharing.</footer></main></body></html>");
    html
}

fn is_empty(value: &Value) -> bool {
    match value {
        Value::Array(v) => v.is_empty(),
        Value::Object(v) => v.is_empty(),
        Value::Null => true,
        Value::String(v) => v.is_empty(),
        _ => false,
    }
}

fn render_inspection_scope(body: &Value, html: &mut String) {
    let Some(scope) = body.get("scope") else {
        return;
    };
    html.push_str("<section class=\"panel\"><h2>Inspection scope and timing</h2><dl>");
    for (key, label) in [
        ("mode", "Selected scope"),
        ("registered_worktrees", "Registered worktrees listed"),
        ("deep_scanned_worktrees", "Worktrees inspected deeply"),
        ("listed_only_worktrees", "Other worktrees listed only"),
        ("missing_registrations", "Missing worktree locations"),
        (
            "metadata_prune_candidates",
            "Registration-metadata review candidates",
        ),
        ("failed_or_partial_scans", "Failed or partial scans"),
    ] {
        if let Some(value) = scope.get(key) {
            let _ = write!(html, "<dt>{label}</dt><dd>{}</dd>", escape(&scalar(value)));
        }
    }
    if let Some(seconds) = body.get("elapsed_seconds") {
        let _ = write!(
            html,
            "<dt>Elapsed seconds</dt><dd>{}</dd>",
            escape(&scalar(seconds))
        );
    }
    if let Some(stable) = body.get("registration_stable") {
        let _ = write!(
            html,
            "<dt>Registrations unchanged during scan</dt><dd>{}</dd>",
            escape(&scalar(stable))
        );
    }
    html.push_str("</dl><p class=\"quiet\">Listed-only worktrees have not had their files inspected. Registration metadata candidates do not establish that branch history or saved work can be discarded.</p></section>");
}

fn dependency_counts(item: &Value) -> (usize, usize, usize, usize) {
    let Some(dependencies) = item.get("git_dependencies").and_then(Value::as_array) else {
        return (0, 0, 0, 0);
    };
    let available = dependencies
        .iter()
        .filter(|d| d["object_available"] == true)
        .count();
    let hashes = dependencies
        .iter()
        .filter(|d| d["status"] == "hash_mismatch")
        .count();
    let unresolved = dependencies
        .iter()
        .filter(|d| {
            !matches!(
                d["status"].as_str(),
                Some("matched" | "available_content_checked" | "available_unverified_content")
            )
        })
        .count();
    (dependencies.len(), available, hashes, unresolved)
}

fn render_consolidation_summary(body: &Value, html: &mut String) {
    let Some(summary) = body.get("consolidation_summary") else {
        return;
    };
    if summary.get("folders_analyzed") == Some(&Value::from(0)) {
        return;
    }
    if summary.get("content_comparisons_requested") == Some(&Value::Bool(false)) {
        html.push_str("<section class=\"panel\"><h2>Saved-folder comparison not requested</h2><p>Use <code>--compare-folders</code> to assess exact matches and consolidation candidates in explicitly supplied recovery folders.</p></section>");
        return;
    }
    html.push_str(
        "<section class=\"panel\"><h2>Consolidation candidates across saved folders</h2><dl>",
    );
    for (key, label) in [
        ("payload_files", "Saved payload copies"),
        ("same_path_identical", "Copies matching the current target"),
        ("same_path_different", "Competing current versions"),
        (
            "absent_at_proposed_path",
            "Addition candidates at proposed paths",
        ),
    ] {
        let value = summary["payload"][key].as_u64().unwrap_or(0);
        let _ = write!(html, "<dt>{label}</dt><dd>{value}</dd>");
    }
    for (key, label) in [
        (
            "tracked_git_versions",
            "Git-backed saved versions inspected",
        ),
        (
            "unique_proposed_target_paths",
            "Distinct proposed target paths",
        ),
        (
            "exact_duplicate_saved_group_count",
            "Exact saved-copy duplicate groups",
        ),
        ("duplicate_extra_copies", "Extra copies within those groups"),
    ] {
        if let Some(value) = summary.get(key) {
            let _ = write!(html, "<dt>{label}</dt><dd>{}</dd>", escape(&scalar(value)));
        }
    }
    html.push_str("</dl><p>These categories overlap: a saved duplicate can also match the current target. They are review candidates, not an approved deletion list.</p>");
    if let Some(values) = summary
        .get("tracked_git_comparisons")
        .filter(|v| !is_empty(v))
    {
        html.push_str("<h3>Saved Git version comparison</h3>");
        render_value(values, html, 0);
    }
    html.push_str("</section>");
}

fn render_folder_summary(item: &Value, html: &mut String) {
    let Some(summary) = item.get("summary") else {
        return;
    };
    html.push_str("<h3>Saved-file comparison</h3><dl>");
    if let Some(count) = summary.get("payload_files") {
        let _ = write!(
            html,
            "<dt>Saved payload files</dt><dd>{}</dd>",
            escape(&scalar(count))
        );
    }
    if item.get("comparison_requested") == Some(&Value::Bool(true)) {
        for (key, label) in [
            (
                "same_path_identical",
                "Same path, bytes and executable state",
            ),
            ("same_path_different", "Competing current versions"),
            ("absent_at_proposed_path", "Absent at proposed target path"),
            ("receipt_hash_mismatch", "Receipt fingerprint mismatches"),
            ("saved_file_missing", "Missing saved files"),
            ("unmapped", "Unmapped saved files"),
            ("unsupported", "Unsupported comparisons"),
            ("changed_during_read", "Files changed during comparison"),
            ("saved_git_versions", "Git-backed saved versions inspected"),
        ] {
            let count = summary.get(key).and_then(Value::as_u64).unwrap_or(0);
            let _ = write!(html, "<dt>{label}</dt><dd>{count}</dd>");
        }
    }
    html.push_str("</dl>");
    if item.get("comparison_requested") != Some(&Value::Bool(true)) {
        html.push_str("<p>Payload content comparison was not requested. Use <code>--compare-folders</code> to compare the explicitly supplied folders with the selected target.</p>");
    } else {
        html.push_str("<p>Same bytes are consolidation candidates for review. Differing versions need comparison; an absent target path is an addition candidate, not proof that the work is missing everywhere.</p>");
    }
    let (declared, available, hashes, unresolved) = dependency_counts(item);
    html.push_str("<h3>Saved Git dependencies</h3><dl>");
    for (label, count) in [
        ("Declared dependencies", declared),
        ("Saved commit objects available", available),
        ("Saved-content fingerprint mismatches", hashes),
        ("Unresolved dependencies", unresolved),
    ] {
        let _ = write!(html, "<dt>{label}</dt><dd>{count}</dd>");
    }
    html.push_str("</dl>");
    if let Some(dependencies) = item.get("git_dependencies").and_then(Value::as_array) {
        for dependency in dependencies {
            let group = dependency
                .get("group")
                .map(scalar)
                .unwrap_or_else(|| "Unnamed group".into());
            let status = dependency
                .get("status")
                .map(scalar)
                .unwrap_or_else(|| "unknown".into());
            let _ = write!(
                html,
                "<p><code>{}</code>: <strong>{}</strong></p>",
                escape(&group),
                escape(&human_key(&status))
            );
        }
    }
    html.push_str("<p class=\"quiet\">Object availability and matched receipt hashes do not establish a self-contained checkpoint or historical completeness. Use explicit dependency capture and restoration before relying on an independent recovery copy.</p>");
}

fn render_inventory(body: &Value, html: &mut String) {
    for (key, title) in [
        ("worktrees", "Worktrees to review"),
        ("folders", "Preserved folders to review"),
    ] {
        let Some(items) = body.get(key).and_then(Value::as_array) else {
            continue;
        };
        let _ = write!(html, "<section class=\"panel\"><h2>{title}</h2>");
        if items.is_empty() {
            html.push_str("<p>None supplied or recorded.</p>");
        }
        for item in items {
            let path = item
                .get("path")
                .map(scalar)
                .unwrap_or_else(|| "Unknown location".into());
            let _ = write!(
                html,
                "<h3 class=\"source\"><code>{}</code></h3>",
                escape(&path)
            );
            if let Some(kind) = item.get("kind").and_then(Value::as_str) {
                let _ = write!(html, "<p>{}</p>", escape(&human_key(kind)));
            }
            html.push_str("<dl>");
            for (field, label) in [
                ("staged_paths", "Staged changes"),
                ("working_paths", "Working-file changes"),
                ("untracked", "Untracked files"),
                ("ignored", "Ignored files"),
            ] {
                if let Some(paths) = item.get(field).and_then(Value::as_array) {
                    let _ = write!(html, "<dt>{label}</dt><dd>{}</dd>", paths.len());
                } else if let Some(count) = item.get(field).and_then(|v| v.get("count")) {
                    let _ = write!(html, "<dt>{label}</dt><dd>{}</dd>", escape(&scalar(count)));
                }
            }
            for (field, label) in [
                ("inspection_status", "Inspection status"),
                ("entries", "Recorded entries"),
                ("historical_completeness", "Historical completeness"),
                ("receipt_claims", "Receipt claims"),
            ] {
                if let Some(value) = item.get(field) {
                    let _ = write!(html, "<dt>{label}</dt><dd>{}</dd>", escape(&scalar(value)));
                }
            }
            html.push_str("</dl>");
            render_ignored_summary(item, html);
            render_folder_summary(item, html);
            if let Some(blockers) = item.get("blockers").filter(|v| !is_empty(v)) {
                html.push_str("<p class=\"label\">Blockers and limits</p>");
                render_value(blockers, html, 0);
            }
        }
        html.push_str("</section>");
    }
}

/// Inspect schema 2 carries ignored paths as a bounded summary object unless
/// `--list-ignored` was used; a list (schema 1, or schema 2 with that flag) is
/// rendered with the complete recorded evidence instead.
fn render_ignored_summary(item: &Value, html: &mut String) {
    let Some(summary) = item.get("ignored").filter(|v| v.is_object()) else {
        return;
    };
    let field = |key: &str| summary.get(key).map(scalar).unwrap_or_default();
    let _ = write!(
        html,
        "<h3>Ignored files summary</h3><table><thead><tr><th class=\"num\">Ignored entries</th><th class=\"num\">Bytes</th></tr></thead>\
         <tbody><tr><td class=\"num\">{}</td><td class=\"num\">{}</td></tr></tbody></table>",
        escape(&field("count")),
        escape(&field("bytes"))
    );
    if let Some(rows) = summary
        .get("top_dirs")
        .and_then(Value::as_array)
        .filter(|rows| !rows.is_empty())
    {
        html.push_str("<table><thead><tr><th>Top-level path</th><th class=\"num\">Entries</th><th class=\"num\">Bytes</th></tr></thead><tbody>");
        for row in rows {
            let cell = |key: &str| escape(&row.get(key).map(scalar).unwrap_or_default());
            let _ = write!(
                html,
                "<tr><td><code>{}</code></td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
                cell("path"),
                cell("count"),
                cell("bytes")
            );
        }
        html.push_str("</tbody></table>");
    }
    html.push_str("<p class=\"quiet\">Bounded summary: the top-level paths with the most ignored entries. Run <code>inspect --list-ignored</code> for every path. Ignored files are not captured unless selected.</p>");
}

fn render_comparisons(body: &Value, html: &mut String) {
    let Some(comparisons) = body.get("comparisons").and_then(Value::as_array) else {
        return;
    };
    html.push_str(
        "<div class=\"grid\"><section class=\"panel\"><h2>What is unique or different?</h2>",
    );
    if comparisons.is_empty() {
        html.push_str("<p>No source comparison was recorded.</p>");
    }
    for item in comparisons {
        source_heading(item, html);
        for (key, label) in [
            ("unique_commits", "Commits outside target history"),
            ("differing_paths", "Paths with different contents"),
            (
                "staged_alternates",
                "Staged alternatives retained separately",
            ),
        ] {
            let _ = write!(html, "<p class=\"label\">{label}</p>");
            render_value(item.get(key).unwrap_or(&Value::Null), html, 0);
        }
    }
    html.push_str("</section><section class=\"panel\"><h2>What is already represented?</h2>");
    for item in comparisons {
        source_heading(item, html);
        let ancestry = item
            .get("already_in_target_history")
            .and_then(Value::as_bool);
        html.push_str(match ancestry {
            Some(true) => "<p>The source commit is already in the target's recorded history.</p>",
            Some(false) => "<p>The source commit is not recorded as part of target history.</p>",
            None => "<p>Commit ancestry has not been established.</p>",
        });
        html.push_str("<p class=\"label\">Identical paths</p>");
        render_value(item.get("identical_paths").unwrap_or(&Value::Null), html, 0);
        html.push_str("<p class=\"quiet\">Identical paths do not establish that every change from this source has been consolidated.</p>");
        if let Some(notes) = item.get("notes").filter(|v| !is_empty(v)) {
            render_value(notes, html, 0);
        }
    }
    html.push_str("</section></div>");
}

fn source_heading(item: &Value, html: &mut String) {
    let source = item
        .get("checkpoint")
        .map(scalar)
        .unwrap_or_else(|| "Unidentified source".into());
    let _ = write!(
        html,
        "<h3 class=\"source\">Source <code>{}</code></h3>",
        escape(&source)
    );
}

fn render_value(value: &Value, html: &mut String, depth: usize) {
    if depth > 12 {
        let _ = write!(html, "<pre>{}</pre>", escape(&value.to_string()));
        return;
    }
    match value {
        Value::Object(fields) if !fields.is_empty() => {
            html.push_str("<dl>");
            for (key, value) in fields {
                let _ = write!(html, "<dt>{}</dt><dd>", escape(&human_key(key)));
                render_value(value, html, depth + 1);
                html.push_str("</dd>");
            }
            html.push_str("</dl>");
        }
        Value::Array(items) if !items.is_empty() => {
            html.push_str("<ul>");
            for item in items {
                html.push_str("<li>");
                render_value(item, html, depth + 1);
                html.push_str("</li>");
            }
            html.push_str("</ul>");
        }
        Value::Array(_) | Value::Object(_) => {
            html.push_str("<span class=\"quiet\">None recorded</span>")
        }
        _ => {
            let _ = write!(html, "<span>{}</span>", escape(&scalar(value)));
        }
    }
}

/// Plain text view. Control characters are escaped so hostile filenames cannot
/// inject terminal escape sequences; line breaks are rendered as literal `\\n`.
pub fn terminal(value: &Value) -> String {
    let body = payload(value);
    let mut out = format!("Timber — {}\n", clean_terminal(&status(value)));
    if let Some(command) = value.get("command").and_then(Value::as_str) {
        let _ = writeln!(out, "Command: {}", clean_terminal(command));
    }
    terminal_inspection_summary(body, &mut out);
    terminal_value(body, &mut out, 0);
    out.push_str(
        "\nTimber does not apply changes to source worktrees. Cleanup requires separate human review.\n",
    );
    out
}

fn terminal_inspection_summary(body: &Value, out: &mut String) {
    if let Some(scope) = body.get("scope") {
        let _ = writeln!(
            out,
            "Scope: {} deeply inspected; {} listed only; {} missing locations; {} failed/partial scans.",
            scope["deep_scanned_worktrees"],
            scope["listed_only_worktrees"],
            scope["missing_registrations"],
            scope["failed_or_partial_scans"]
        );
        if let Some(elapsed) = body.get("elapsed_seconds") {
            let _ = writeln!(out, "Elapsed seconds: {}", clean_terminal(&scalar(elapsed)));
        }
    }
    if let Some(summary) = body.get("consolidation_summary").filter(|s| {
        s.get("folders_analyzed") != Some(&Value::from(0))
            && s.get("content_comparisons_requested") != Some(&Value::Bool(false))
    }) {
        let n = |key: &str| summary["payload"][key].as_u64().unwrap_or(0);
        let _ = writeln!(
            out,
            "Saved copies: {} matching current target; {} competing versions; {} absent at proposed paths.",
            n("same_path_identical"),
            n("same_path_different"),
            n("absent_at_proposed_path")
        );
        let _ = writeln!(
            out,
            "Saved-copy overlap: {} exact groups, {} extra copies (overlaps the target-match counts).",
            summary["exact_duplicate_saved_group_count"], summary["duplicate_extra_copies"]
        );
    }
    if let Some(folders) = body.get("folders").and_then(Value::as_array) {
        for folder in folders {
            let Some(summary) = folder.get("summary") else {
                continue;
            };
            let name = folder
                .get("path")
                .map(scalar)
                .unwrap_or_else(|| "Unknown folder".into());
            let _ = writeln!(out, "\nSaved folder: {}", clean_terminal(&name));
            if folder["comparison_requested"] == true {
                let count = |key: &str| summary.get(key).and_then(Value::as_u64).unwrap_or(0);
                let _ = writeln!(
                    out,
                    "  Matching copies: {}; competing versions: {}; absent at proposed path: {}; receipt mismatches: {}.",
                    count("same_path_identical"),
                    count("same_path_different"),
                    count("absent_at_proposed_path"),
                    count("receipt_hash_mismatch")
                );
            } else {
                out.push_str("  Payload bytes were not compared; use --compare-folders.\n");
            }
            let (declared, available, hashes, unresolved) = dependency_counts(folder);
            let _ = writeln!(
                out,
                "  Git dependencies: {declared} declared; {available} available; {hashes} content mismatches; {unresolved} unresolved."
            );
            out.push_str("  These are review candidates, not cleanup authorization.\n");
        }
    }
}

fn clean_terminal(text: &str) -> String {
    text.chars()
        .flat_map(|ch| {
            if ch.is_control() {
                ch.escape_default().collect::<Vec<_>>()
            } else {
                vec![ch]
            }
        })
        .collect()
}

fn terminal_value(value: &Value, out: &mut String, depth: usize) {
    let indent = "  ".repeat(depth.min(12));
    match value {
        Value::Object(fields) if depth < 12 => {
            for (key, value) in fields {
                if matches!(key.as_str(), "payload_rows" | "tracked_rows") && value.is_array() {
                    let _ = writeln!(
                        out,
                        "{indent}{}: {} rows; use --json for full per-file evidence",
                        clean_terminal(&human_key(key)),
                        value.as_array().expect("array checked").len()
                    );
                    continue;
                }
                if key == "entries" && value.is_array() {
                    let items = value.as_array().expect("array checked");
                    let bytes: u64 = items
                        .iter()
                        .filter_map(|e| e.get("size").and_then(Value::as_u64))
                        .fold(0u64, u64::saturating_add);
                    let _ = writeln!(
                        out,
                        "{indent}Captured entries: {} ({bytes} content bytes; --json shows the full inventory)",
                        items.len()
                    );
                    continue;
                }
                if key == "index" && value.is_array() {
                    let _ = writeln!(
                        out,
                        "{indent}Staged index entries: {}",
                        value.as_array().expect("array checked").len()
                    );
                    continue;
                }
                if value.is_array() || value.is_object() {
                    let _ = writeln!(out, "{indent}{}:", clean_terminal(&human_key(key)));
                    terminal_value(value, out, depth + 1);
                } else {
                    let _ = writeln!(
                        out,
                        "{indent}{}: {}",
                        clean_terminal(&human_key(key)),
                        clean_terminal(&scalar(value))
                    );
                }
            }
        }
        Value::Array(items) if depth < 12 => {
            if items.is_empty() {
                let _ = writeln!(out, "{indent}(none recorded)");
            }
            for item in items.iter().take(12) {
                if item.is_object() || item.is_array() {
                    let _ = writeln!(out, "{indent}-");
                    terminal_value(item, out, depth + 1);
                } else {
                    let _ = writeln!(out, "{indent}- {}", clean_terminal(&scalar(item)));
                }
            }
            if items.len() > 12 {
                let _ = writeln!(
                    out,
                    "{indent}… {} more; use --json for complete evidence",
                    items.len() - 12
                );
            }
        }
        _ => {
            let _ = writeln!(out, "{indent}{}", clean_terminal(&scalar(value)));
        }
    }
}
