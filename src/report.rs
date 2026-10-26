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
         .status{display:inline-block;background:#e6efe4;border:1px solid #b6cdb4;border-radius:6px;padding:7px 12px;font-weight:650}\
         .grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(280px,1fr));gap:20px}.panel,details{background:var(--panel);border:1px solid var(--line);border-radius:9px;padding:22px;margin-bottom:20px;min-width:0}\
         .quiet{color:var(--muted)}ul{padding-left:22px}li{margin:8px 0;overflow-wrap:anywhere}code,pre{font:13px/1.55 ui-monospace,monospace;overflow-wrap:anywhere}\
         pre{white-space:pre-wrap;background:var(--paper);padding:14px;border-radius:5px}dl{display:grid;grid-template-columns:minmax(100px,1fr) minmax(140px,3fr);gap:9px 16px;margin:0}\
         dt{font-weight:600}dd{margin:0;overflow-wrap:anywhere}summary{cursor:pointer;font-weight:650}details[open]>summary{margin-bottom:16px}\
         .source{border-top:1px solid var(--line);padding-top:8px}.label{font-weight:650}footer{color:var(--muted);font-size:14px}\
         @media(prefers-color-scheme:dark){:root{--paper:#18221b;--ink:#e9efe6;--muted:#bac8b8;--line:#405341;--panel:#202e23;--accent:#9dc19e}.status{background:#2c4630;border-color:#56785a}}\
         @media(max-width:540px){main{padding:24px 16px}dl{display:block}dd{margin-bottom:12px}}\
         </style></head><body><main><header><div class=\"brand\">Timber!</div>\
         <h1>Untangle dirty worktrees.<br>Keep the useful work.</h1>",
    );
    let _ = write!(html, "<p class=\"status\">{}</p>", escape(&status(value)));
    if let Some(command) = value.get("command").and_then(Value::as_str) {
        let _ = write!(
            html,
            "<p class=\"quiet\">Result from <code>{}</code>.</p>",
            escape(command)
        );
    }
    html.push_str("<p>This is a local review of recorded evidence. A clean merge shows that selected changes combine mechanically; application behavior needs separate testing.</p></header>");
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
                }
            }
            for (field, label) in [
                ("entries", "Recorded entries"),
                ("historical_completeness", "Historical completeness"),
                ("receipt_claims", "Receipt claims"),
            ] {
                if let Some(value) = item.get(field) {
                    let _ = write!(html, "<dt>{label}</dt><dd>{}</dd>", escape(&scalar(value)));
                }
            }
            html.push_str("</dl>");
            if let Some(blockers) = item.get("blockers").filter(|v| !is_empty(v)) {
                html.push_str("<p class=\"label\">Blockers and limits</p>");
                render_value(blockers, html, 0);
            }
        }
        html.push_str("</section>");
    }
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
    terminal_value(body, &mut out, 0);
    out.push_str(
        "\nTimber does not apply changes to source worktrees. Cleanup requires separate human review.\n",
    );
    out
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
