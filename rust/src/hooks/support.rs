use std::path::Path;

pub(super) fn install_named_json_server(
    name: &str,
    display_path: &str,
    config_path: &std::path::Path,
    root_key: &str,
    entry: serde_json::Value,
) {
    if let Some(parent) = config_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    if config_path.exists() {
        let content = std::fs::read_to_string(config_path).unwrap_or_default();
        if content.contains("lean-ctx") {
            println!("{name} MCP already configured at {display_path}");
            return;
        }
        if update_named_json_server(config_path, &content, root_key, &entry) {
            print_named_json_server_success(name, display_path);
            return;
        }
    }

    if write_named_json_server_root(config_path, root_key, entry) {
        print_named_json_server_success(name, display_path);
    } else {
        tracing::error!("Failed to configure {name}");
    }
}

fn update_named_json_server(
    config_path: &std::path::Path,
    content: &str,
    root_key: &str,
    entry: &serde_json::Value,
) -> bool {
    let Ok(mut json) = crate::core::jsonc::parse_jsonc(content) else {
        return false;
    };
    let Some(obj) = json.as_object_mut() else {
        return false;
    };
    let servers = obj
        .entry(root_key.to_string())
        .or_insert_with(|| serde_json::json!({}));
    let Some(servers_obj) = servers.as_object_mut() else {
        return false;
    };
    servers_obj.insert("lean-ctx".to_string(), entry.clone());
    write_json_config(config_path, &json)
}

fn write_named_json_server_root(
    config_path: &std::path::Path,
    root_key: &str,
    entry: serde_json::Value,
) -> bool {
    let mut servers = serde_json::Map::new();
    servers.insert("lean-ctx".to_string(), entry);

    let mut root = serde_json::Map::new();
    root.insert(root_key.to_string(), serde_json::Value::Object(servers));

    write_json_config(config_path, &serde_json::Value::Object(root))
}

fn write_json_config(config_path: &std::path::Path, value: &serde_json::Value) -> bool {
    let Ok(json_str) = serde_json::to_string_pretty(value) else {
        return false;
    };
    std::fs::write(config_path, json_str).is_ok()
}

fn print_named_json_server_success(name: &str, display_path: &str) {
    println!("  \x1b[32m✓\x1b[0m {name} MCP configured at {display_path}");
}

pub(super) fn install_codex_instruction_docs(codex_dir: &Path) -> bool {
    let agents_path = codex_dir.join("AGENTS.md");
    let agents_content = "# Global Agent Instructions\n\n@LEAN-CTX.md\n";
    let lean_ctx_md = codex_dir.join("LEAN-CTX.md");
    let lean_ctx_content = codex_instruction_doc_content();

    match std::fs::read_to_string(&agents_path) {
        Ok(content) if content.contains("lean-ctx") || content.contains("LEAN-CTX") => {
            if lean_ctx_md.exists() {
                false
            } else {
                super::write_file(&lean_ctx_md, &lean_ctx_content);
                true
            }
        }
        _ => {
            super::write_file(&agents_path, agents_content);
            super::write_file(&lean_ctx_md, &lean_ctx_content);
            true
        }
    }
}

fn codex_instruction_doc_content() -> String {
    let binary = super::resolve_binary_path();
    format!(
        r"# lean-ctx — Token Optimization

Prefix all shell commands with `{binary} -c` for compressed output:

```bash
{binary} -c git status    # instead of: git status
{binary} -c cargo test    # instead of: cargo test
{binary} -c ls src/       # instead of: ls src/
```

This saves 60-90% tokens per command. Works with: git, cargo, npm, pnpm, docker, kubectl, pip, ruff, go, curl, grep, find, ls, aws, helm, and 90+ more commands.
Use `{binary} -c --raw <cmd>` to skip compression and get full output.
"
    )
}

pub(super) fn ensure_codex_hooks_enabled(config_content: &str) -> Option<String> {
    let newline = config_newline(config_content);
    let mut lines = config_lines(config_content);
    let layout = inspect_codex_hooks_layout(&lines);

    let mut changed =
        rewrite_existing_codex_hooks_assignment(&mut lines, layout.features_codex_index);
    changed |= clear_stray_codex_hooks_assignments(&mut lines, &layout.stray_codex_indices);

    if layout.features_codex_index.is_none() {
        insert_codex_hooks_assignment(&mut lines, layout.features_insert_index);
        changed = true;
    }

    changed.then(|| render_codex_hook_config(lines, newline))
}

struct CodexHooksLayout {
    features_insert_index: Option<usize>,
    features_codex_index: Option<usize>,
    stray_codex_indices: Vec<usize>,
}

fn config_newline(config_content: &str) -> &'static str {
    if config_content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

fn config_lines(config_content: &str) -> Vec<String> {
    config_content
        .lines()
        .map(std::string::ToString::to_string)
        .collect()
}

fn inspect_codex_hooks_layout(lines: &[String]) -> CodexHooksLayout {
    let mut features_codex_index = None;
    let mut features_insert_index = None;
    let mut stray_codex_indices = Vec::new();
    let mut current_section: Option<&str> = None;

    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if let Some(section) = parse_toml_section_name(trimmed) {
            current_section = Some(section);
            if section == "features" {
                features_insert_index = Some(idx + 1);
            }
            continue;
        }

        if current_section == Some("features") {
            features_insert_index = Some(idx + 1);
        }

        if !is_codex_hooks_assignment(trimmed) {
            continue;
        }

        if current_section == Some("features") && features_codex_index.is_none() {
            features_codex_index = Some(idx);
        } else {
            stray_codex_indices.push(idx);
        }
    }

    CodexHooksLayout {
        features_insert_index,
        features_codex_index,
        stray_codex_indices,
    }
}

fn rewrite_existing_codex_hooks_assignment(lines: &mut [String], index: Option<usize>) -> bool {
    let Some(index) = index else {
        return false;
    };

    let replacement = rewrite_codex_hooks_line(&lines[index]);
    if lines[index] == replacement {
        return false;
    }

    lines[index] = replacement;
    true
}

fn clear_stray_codex_hooks_assignments(lines: &mut [String], indices: &[usize]) -> bool {
    let mut changed = false;
    for &idx in indices {
        if !lines[idx].is_empty() {
            lines[idx].clear();
            changed = true;
        }
    }
    changed
}

fn insert_codex_hooks_assignment(lines: &mut Vec<String>, insert_index: Option<usize>) {
    if let Some(insert_index) = insert_index {
        let insert_at = trim_blank_lines_before(lines, insert_index);
        lines.insert(insert_at, "codex_hooks = true".to_string());
        return;
    }

    if !lines.is_empty() && !lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.push(String::new());
    }
    lines.push("[features]".to_string());
    lines.push("codex_hooks = true".to_string());
}

fn trim_blank_lines_before(lines: &[String], mut index: usize) -> usize {
    while index > 0 && lines[index - 1].trim().is_empty() {
        index -= 1;
    }
    index
}

fn render_codex_hook_config(lines: Vec<String>, newline: &str) -> String {
    let mut output = compact_blank_lines(lines).join(newline);
    output.push_str(newline);
    output
}

fn compact_blank_lines(lines: Vec<String>) -> Vec<String> {
    let mut compacted = Vec::with_capacity(lines.len());
    let mut previous_blank = false;
    for line in lines {
        let is_blank = line.trim().is_empty();
        if is_blank && previous_blank {
            continue;
        }
        previous_blank = is_blank;
        compacted.push(line);
    }
    while compacted.last().is_some_and(|line| line.trim().is_empty()) {
        compacted.pop();
    }
    compacted
}

fn parse_toml_section_name(trimmed_line: &str) -> Option<&str> {
    if trimmed_line.starts_with('[') && trimmed_line.ends_with(']') {
        Some(
            trimmed_line
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim(),
        )
    } else {
        None
    }
}

fn is_codex_hooks_assignment(trimmed_line: &str) -> bool {
    let without_comment = trimmed_line.split('#').next().unwrap_or("").trim();
    without_comment
        .strip_prefix("codex_hooks")
        .is_some_and(|rest| rest.trim_start().starts_with('='))
}

fn rewrite_codex_hooks_line(line: &str) -> String {
    let indent_len = line.chars().take_while(|c| c.is_whitespace()).count();
    let indent = &line[..indent_len];
    let comment = line
        .find('#')
        .map(|index| line[index..].trim_end())
        .filter(|comment| !comment.is_empty());

    match comment {
        Some(comment) => format!("{indent}codex_hooks = true  {comment}"),
        None => format!("{indent}codex_hooks = true"),
    }
}

pub(super) fn upsert_lean_ctx_codex_hook_entries(
    root: &mut serde_json::Value,
    session_start_cmd: &str,
    pre_tool_use_cmd: &str,
) -> bool {
    let original = root.clone();
    if !root.is_object() {
        *root = serde_json::json!({});
    }
    let root_obj = root.as_object_mut().expect("root should be object");
    let hooks_value = root_obj
        .entry("hooks".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !hooks_value.is_object() {
        *hooks_value = serde_json::json!({});
    }
    let hooks_obj = hooks_value
        .as_object_mut()
        .expect("hooks should be object after normalization");

    remove_lean_ctx_codex_managed_entries(hooks_obj, "PreToolUse");
    remove_lean_ctx_codex_managed_entries(hooks_obj, "SessionStart");

    push_codex_hook_entry(hooks_obj, "PreToolUse", "Bash", pre_tool_use_cmd);
    push_codex_hook_entry(
        hooks_obj,
        "SessionStart",
        "startup|resume|clear",
        session_start_cmd,
    );

    *root != original
}

fn push_codex_hook_entry(
    hooks_obj: &mut serde_json::Map<String, serde_json::Value>,
    event_name: &str,
    matcher: &str,
    command: &str,
) {
    codex_hook_entries_mut(hooks_obj, event_name).push(serde_json::json!({
        "matcher": matcher,
        "hooks": [{
            "type": "command",
            "command": command,
            "timeout": 15
        }]
    }));
}

fn codex_hook_entries_mut<'a>(
    hooks_obj: &'a mut serde_json::Map<String, serde_json::Value>,
    event_name: &str,
) -> &'a mut Vec<serde_json::Value> {
    let value = hooks_obj
        .entry(event_name.to_string())
        .or_insert_with(|| serde_json::json!([]));
    if !value.is_array() {
        *value = serde_json::json!([]);
    }
    value
        .as_array_mut()
        .unwrap_or_else(|| panic!("{event_name} should be an array"))
}

pub(super) fn remove_lean_ctx_codex_managed_entries(
    hooks_obj: &mut serde_json::Map<String, serde_json::Value>,
    event_name: &str,
) {
    let Some(entries) = hooks_obj
        .get_mut(event_name)
        .and_then(|value| value.as_array_mut())
    else {
        return;
    };
    entries.retain(|entry| !is_lean_ctx_codex_managed_entry(event_name, entry));
    if entries.is_empty() {
        hooks_obj.remove(event_name);
    }
}

pub(super) fn is_lean_ctx_codex_managed_entry(event_name: &str, entry: &serde_json::Value) -> bool {
    let Some(entry_obj) = entry.as_object() else {
        return false;
    };

    let Some(hooks) = entry_obj.get("hooks").and_then(|value| value.as_array()) else {
        return false;
    };

    hooks.iter().any(|hook| {
        let Some(command) = hook.get("command").and_then(|value| value.as_str()) else {
            return false;
        };
        match event_name {
            "PreToolUse" => {
                entry_obj.get("matcher").and_then(|value| value.as_str()) == Some("Bash")
                    && command.contains("lean-ctx")
                    && (command.contains("hook rewrite")
                        || command.contains("hook codex-pretooluse"))
            }
            "SessionStart" => {
                command.contains("lean-ctx") && command.contains("hook codex-session-start")
            }
            _ => false,
        }
    })
}
