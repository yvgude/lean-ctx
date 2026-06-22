use std::path::Path;

pub(crate) fn install_named_json_server(
    name: &str,
    display_path: &str,
    config_path: &std::path::Path,
    root_key: &str,
    entry: serde_json::Value,
) {
    // #281: honor `[setup] auto_update_mcp = false`. This is the shared writer
    // for every JSON-config agent (Aider, Continue, Qwen, Zed, Amazon Q, …), so
    // gating it here keeps locked-down installs free of MCP server entries while
    // their hooks/rules still install. The editor-target path is gated at its
    // call sites; this closes the hooks-layer path missed by the first fix.
    // The skip stays silent: init/setup/onboard/doctor already print one
    // per-agent skip line, so re-announcing here would just double the noise.
    if !crate::core::config::Config::load()
        .setup
        .should_update_mcp()
    {
        return;
    }

    if let Some(parent) = config_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    if config_path.exists() {
        let content = std::fs::read_to_string(config_path).unwrap_or_default();
        if content.contains("lean-ctx") {
            if !super::mcp_server_quiet_mode() {
                eprintln!("{name} MCP already configured at {display_path}");
            }
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
    if !super::mcp_server_quiet_mode() {
        eprintln!("  \x1b[32m✓\x1b[0m {name} MCP configured at {display_path}");
    }
}

const CODEX_AGENTS_BLOCK_START: &str = "<!-- lean-ctx -->";
const CODEX_AGENTS_BLOCK_END: &str = crate::core::rules_canonical::END_MARK;

pub(super) fn install_codex_instruction_docs(codex_dir: &Path) -> bool {
    let agents_path = codex_dir.join("AGENTS.md");
    let lean_ctx_md = codex_dir.join("LEAN-CTX.md");
    let lean_ctx_content = codex_instruction_doc_content();

    let mut changed = false;

    // LEAN-CTX.md (full rules) is lean-ctx-owned and fully removable — written in
    // both modes, never the user's AGENTS.md.
    let existing_lean_ctx = std::fs::read_to_string(&lean_ctx_md).unwrap_or_default();
    if existing_lean_ctx != lean_ctx_content {
        super::write_file(&lean_ctx_md, &lean_ctx_content);
        changed = true;
    }

    // Dedicated mode (#343): never touch AGENTS.md. The Codex SessionStart hook
    // injects the compact summary; strip any block a prior shared install left.
    if crate::core::config::Config::load().rules_injection_effective()
        == crate::core::config::RulesInjection::Dedicated
    {
        if agents_path.exists()
            && std::fs::read_to_string(&agents_path)
                .is_ok_and(|c| c.contains(CODEX_AGENTS_BLOCK_START))
        {
            crate::marked_block::remove_from_file(
                &agents_path,
                CODEX_AGENTS_BLOCK_START,
                CODEX_AGENTS_BLOCK_END,
                true,
                "Codex AGENTS.md lean-ctx block",
            );
            changed = true;
        }
        return changed;
    }

    let rules_path = codex_dir.join("LEAN-CTX.md");
    let block = format!(
        "{CODEX_AGENTS_BLOCK_START}\n## lean-ctx\n\n\
         Prefer lean-ctx MCP tools over native equivalents for token savings.\n\n\
         For compression you can rely on regardless of your Codex surface (CLI, Desktop, \
         or Cloud) or Codex version, route shell commands through `ctx_shell` \
         (or `{binary} -c \"<cmd>\"`), file reads through `ctx_read`, and code search through \
         `ctx_search`. Hook-driven auto-compression may also be active, but the MCP/CLI tools \
         are the path that works everywhere — otherwise large outputs (builds, `tsc`, tests, \
         logs) can reach the model uncompressed.\n\n\
         Full rules: `{rules}`\n{CODEX_AGENTS_BLOCK_END}\n",
        binary = super::resolve_binary_path(),
        rules = rules_path.display()
    );

    if !agents_path.exists() {
        let content = format!("# Global Agent Instructions\n\n{block}");
        super::write_file(&agents_path, &content);
        return true;
    }

    let existing = std::fs::read_to_string(&agents_path).unwrap_or_default();

    if existing.contains(CODEX_AGENTS_BLOCK_START) {
        let updated = crate::marked_block::replace_marked_block(
            &existing,
            CODEX_AGENTS_BLOCK_START,
            CODEX_AGENTS_BLOCK_END,
            &block,
        );
        if updated != existing {
            super::write_file(&agents_path, &updated);
            return true;
        }
        return changed;
    }

    if existing.contains("lean-ctx") || existing.contains("LEAN-CTX") {
        return changed;
    }

    let mut out = existing;
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    out.push_str(&block);
    super::write_file(&agents_path, &out);
    true
}

fn codex_instruction_doc_content() -> String {
    let binary = super::resolve_binary_path();
    let marker = crate::core::rules_canonical::START_MARK;
    let bullets = crate::core::rules_canonical::BULLETS;
    let never = crate::core::rules_canonical::NEVER;
    format!(
        r#"{marker} (Hybrid Mode)

lean-ctx is available via **both** MCP tools and CLI commands.

## Reliable compression on every Codex surface

lean-ctx can compress automatically through Codex lifecycle hooks, but whether
those fire depends on your Codex version and surface (CLI / Desktop / Cloud) and
on the hooks being trusted (run `/hooks` to review). The agent usually cannot tell
which environment it is in — so for compression that works **everywhere**, route
work through lean-ctx explicitly:

- Shell commands → call the `ctx_shell` MCP tool (or `{binary} -c "<cmd>"`).
- File reads → call the `ctx_read` MCP tool (instead of `cat`/`head`/`tail`).
- Code search → call the `ctx_search` MCP tool (instead of `grep`/`rg`).

Running `tsc`, builds, tests, `git`, or log-heavy commands directly sends the full
uncompressed output to the model. Routing them through `ctx_shell` saves 60-90% of
those tokens.

## MCP tools

{bullets}

{never}

## CLI

Prefix shell commands with `{binary} -c` for compressed output:

```bash
{binary} -c "tsc"          # instead of: tsc
{binary} -c "cargo test"   # instead of: cargo test
{binary} -c "git status"   # instead of: git status
```

Works with git, cargo, npm, pnpm, docker, kubectl, pip, ruff, go, tsc, and 95+ more.
Use `{binary} -c --raw <cmd>` to skip compression and get full output.

## Hooks across Codex surfaces

- **Hook-driven auto-compression** fires once hooks are trusted via `/hooks`. Whether
  hooks run at all depends on your Codex version and surface, and behaviour has
  changed across releases — so do not assume it is always on.
- **MCP tools / CLI** (`ctx_shell`, `ctx_read`, `ctx_search`, or `{binary} -c`) compress
  on **every** surface (CLI, Desktop, Cloud) regardless of hook status — use them as
  the reliable path described above.
"#
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
        lines.insert(insert_at, "hooks = true".to_string());
        return;
    }

    if !lines.is_empty() && !lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.push(String::new());
    }
    lines.push("[features]".to_string());
    lines.push("hooks = true".to_string());
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
    if without_comment
        .strip_prefix("codex_hooks")
        .is_some_and(|rest| rest.trim_start().starts_with('='))
    {
        return true;
    }
    without_comment
        .strip_prefix("hooks")
        .is_some_and(|rest| rest.trim_start().starts_with('=') && !rest.starts_with('_'))
}

fn rewrite_codex_hooks_line(line: &str) -> String {
    let indent_len = line.chars().take_while(|c| c.is_whitespace()).count();
    let indent = &line[..indent_len];
    let comment = line
        .find('#')
        .map(|index| line[index..].trim_end())
        .filter(|comment| !comment.is_empty());

    match comment {
        Some(comment) => format!("{indent}hooks = true  {comment}"),
        None => format!("{indent}hooks = true"),
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
