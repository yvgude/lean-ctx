use std::path::PathBuf;

pub mod agents;
mod support;
use agents::{
    install_amp_hook, install_claude_hook, install_claude_hook_config, install_claude_hook_scripts,
    install_claude_project_hooks, install_cline_rules, install_codex_hook, install_copilot_hook,
    install_crush_hook, install_cursor_hook, install_cursor_hook_config,
    install_cursor_hook_scripts, install_gemini_hook, install_gemini_hook_config,
    install_gemini_hook_scripts, install_hermes_hook, install_jetbrains_hook, install_kiro_hook,
    install_opencode_hook, install_pi_hook, install_windsurf_rules,
};
use support::{
    ensure_codex_hooks_enabled, install_codex_instruction_docs, install_named_json_server,
    upsert_lean_ctx_codex_hook_entries,
};

fn mcp_server_quiet_mode() -> bool {
    std::env::var_os("LEAN_CTX_MCP_SERVER").is_some()
        || matches!(std::env::var("LEAN_CTX_QUIET"), Ok(value) if value.trim() == "1")
}

/// Silently refresh all hook scripts for agents that are already configured.
/// Called after updates and on MCP server start to ensure hooks match the current binary version.
pub fn refresh_installed_hooks() {
    let Some(home) = dirs::home_dir() else { return };

    let claude_dir = crate::setup::claude_config_dir(&home);
    let claude_hooks = claude_dir.join("hooks/lean-ctx-rewrite.sh").exists()
        || claude_dir.join("settings.json").exists()
            && std::fs::read_to_string(claude_dir.join("settings.json"))
                .unwrap_or_default()
                .contains("lean-ctx");

    if claude_hooks {
        install_claude_hook_scripts(&home);
        install_claude_hook_config(&home);
    }

    let cursor_hooks = home.join(".cursor/hooks/lean-ctx-rewrite.sh").exists()
        || home.join(".cursor/hooks.json").exists()
            && std::fs::read_to_string(home.join(".cursor/hooks.json"))
                .unwrap_or_default()
                .contains("lean-ctx");

    if cursor_hooks {
        install_cursor_hook_scripts(&home);
        install_cursor_hook_config(&home);
    }

    let gemini_rewrite = home.join(".gemini/hooks/lean-ctx-rewrite-gemini.sh");
    let gemini_legacy = home.join(".gemini/hooks/lean-ctx-hook-gemini.sh");
    if gemini_rewrite.exists() || gemini_legacy.exists() {
        install_gemini_hook_scripts(&home);
        install_gemini_hook_config(&home);
    }

    let codex_hooks = home.join(".codex/hooks/lean-ctx-rewrite-codex.sh").exists()
        || home.join(".codex/hooks.json").exists()
            && std::fs::read_to_string(home.join(".codex/hooks.json"))
                .unwrap_or_default()
                .contains("lean-ctx");

    if codex_hooks {
        install_codex_hook();
    }
}

fn resolve_binary_path() -> String {
    if is_lean_ctx_in_path() {
        return "lean-ctx".to_string();
    }
    crate::core::portable_binary::resolve_portable_binary()
}

fn is_lean_ctx_in_path() -> bool {
    let which_cmd = if cfg!(windows) { "where" } else { "which" };
    std::process::Command::new(which_cmd)
        .arg("lean-ctx")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn resolve_binary_path_for_bash() -> String {
    let path = resolve_binary_path();
    to_bash_compatible_path(&path)
}

pub fn to_bash_compatible_path(path: &str) -> String {
    let path = match crate::core::pathutil::strip_verbatim_str(path) {
        Some(stripped) => stripped,
        None => path.replace('\\', "/"),
    };
    if path.len() >= 2 && path.as_bytes()[1] == b':' {
        let drive = (path.as_bytes()[0] as char).to_ascii_lowercase();
        format!("/{drive}{}", &path[2..])
    } else {
        path
    }
}

/// Normalize paths from any client format to a consistent OS-native form.
/// Handles MSYS2/Git Bash (`/c/Users/...` -> `C:/Users/...`), mixed separators,
/// double slashes, and trailing slashes. Always uses forward slashes for consistency.
pub fn normalize_tool_path(path: &str) -> String {
    let mut p = match crate::core::pathutil::strip_verbatim_str(path) {
        Some(stripped) => stripped,
        None => path.to_string(),
    };

    // MSYS2/Git Bash: /c/Users/... -> C:/Users/...
    if p.len() >= 3
        && p.starts_with('/')
        && p.as_bytes()[1].is_ascii_alphabetic()
        && p.as_bytes()[2] == b'/'
    {
        let drive = p.as_bytes()[1].to_ascii_uppercase() as char;
        p = format!("{drive}:{}", &p[2..]);
    }

    p = p.replace('\\', "/");

    // Collapse double slashes (preserve UNC paths starting with //)
    while p.contains("//") && !p.starts_with("//") {
        p = p.replace("//", "/");
    }

    // Remove trailing slash (unless root like "/" or "C:/")
    if p.len() > 1 && p.ends_with('/') && !p.ends_with(":/") {
        p.pop();
    }

    p
}

pub fn generate_rewrite_script(binary: &str) -> String {
    let case_pattern = crate::rewrite_registry::bash_case_pattern();
    format!(
        r#"#!/usr/bin/env bash
# lean-ctx PreToolUse hook — rewrites bash commands to lean-ctx equivalents
set -euo pipefail

LEAN_CTX_BIN="{binary}"

INPUT=$(cat)
TOOL=$(echo "$INPUT" | grep -oE '"tool_name":"([^"\\]|\\.)*"' | head -1 | sed 's/^"tool_name":"//;s/"$//' | sed 's/\\"/"/g;s/\\\\/\\/g')

if [ "$TOOL" != "Bash" ] && [ "$TOOL" != "bash" ]; then
  exit 0
fi

CMD=$(echo "$INPUT" | grep -oE '"command":"([^"\\]|\\.)*"' | head -1 | sed 's/^"command":"//;s/"$//' | sed 's/\\"/"/g;s/\\\\/\\/g')

if [ -z "$CMD" ] || echo "$CMD" | grep -qE "^(lean-ctx |$LEAN_CTX_BIN )"; then
  exit 0
fi

case "$CMD" in
  {case_pattern})
    # Shell-escape then JSON-escape (two passes)
    SHELL_ESC=$(printf '%s' "$CMD" | sed 's/\\/\\\\/g;s/"/\\"/g')
    REWRITE="$LEAN_CTX_BIN -c \"$SHELL_ESC\""
    JSON_CMD=$(printf '%s' "$REWRITE" | sed 's/\\/\\\\/g;s/"/\\"/g')
    printf '{{"hookSpecificOutput":{{"hookEventName":"PreToolUse","permissionDecision":"allow","updatedInput":{{"command":"%s"}}}}}}' "$JSON_CMD"
    ;;
  *) exit 0 ;;
esac
"#
    )
}

pub fn generate_compact_rewrite_script(binary: &str) -> String {
    let case_pattern = crate::rewrite_registry::bash_case_pattern();
    format!(
        r#"#!/usr/bin/env bash
# lean-ctx hook — rewrites shell commands
set -euo pipefail
LEAN_CTX_BIN="{binary}"
INPUT=$(cat)
CMD=$(echo "$INPUT" | grep -oE '"command":"([^"\\]|\\.)*"' | head -1 | sed 's/^"command":"//;s/"$//' | sed 's/\\"/"/g;s/\\\\/\\/g' 2>/dev/null || echo "")
if [ -z "$CMD" ] || echo "$CMD" | grep -qE "^(lean-ctx |$LEAN_CTX_BIN )"; then exit 0; fi
case "$CMD" in
  {case_pattern})
    SHELL_ESC=$(printf '%s' "$CMD" | sed 's/\\/\\\\/g;s/"/\\"/g')
    REWRITE="$LEAN_CTX_BIN -c \"$SHELL_ESC\""
    JSON_CMD=$(printf '%s' "$REWRITE" | sed 's/\\/\\\\/g;s/"/\\"/g')
    printf '{{"hookSpecificOutput":{{"hookEventName":"PreToolUse","permissionDecision":"allow","updatedInput":{{"command":"%s"}}}}}}' "$JSON_CMD" ;;
  *) exit 0 ;;
esac
"#
    )
}

const REDIRECT_SCRIPT_CLAUDE: &str = r"#!/usr/bin/env bash
# lean-ctx PreToolUse hook — all native tools pass through
# Read/Grep/ListFiles are allowed so Edit (which requires native Read) works.
# The MCP instructions guide the AI to prefer ctx_read/ctx_search/ctx_tree.
exit 0
";

const REDIRECT_SCRIPT_GENERIC: &str = r"#!/usr/bin/env bash
# lean-ctx hook — all native tools pass through
exit 0
";

pub fn install_project_rules() {
    if crate::core::config::Config::load().rules_scope_effective()
        == crate::core::config::RulesScope::Global
    {
        return;
    }

    let cwd = std::env::current_dir().unwrap_or_default();

    if !is_inside_git_repo(&cwd) {
        eprintln!(
            "  Skipping project files: not inside a git repository.\n  \
             Run this command from your project root to create CLAUDE.md / AGENTS.md."
        );
        return;
    }

    let home = dirs::home_dir().unwrap_or_default();
    if cwd == home {
        eprintln!(
            "  Skipping project files: current directory is your home folder.\n  \
             Run this command from a project directory instead."
        );
        return;
    }

    ensure_project_agents_integration(&cwd);

    let cursorrules = cwd.join(".cursorrules");
    if !cursorrules.exists()
        || !std::fs::read_to_string(&cursorrules)
            .unwrap_or_default()
            .contains("lean-ctx")
    {
        let content = CURSORRULES_TEMPLATE;
        if cursorrules.exists() {
            let mut existing = std::fs::read_to_string(&cursorrules).unwrap_or_default();
            if !existing.ends_with('\n') {
                existing.push('\n');
            }
            existing.push('\n');
            existing.push_str(content);
            write_file(&cursorrules, &existing);
        } else {
            write_file(&cursorrules, content);
        }
        println!("Created/updated .cursorrules in project root.");
    }

    let claude_rules_dir = cwd.join(".claude").join("rules");
    let claude_rules_file = claude_rules_dir.join("lean-ctx.md");
    if !claude_rules_file.exists()
        || !std::fs::read_to_string(&claude_rules_file)
            .unwrap_or_default()
            .contains(crate::rules_inject::RULES_VERSION_STR)
    {
        let _ = std::fs::create_dir_all(&claude_rules_dir);
        write_file(
            &claude_rules_file,
            crate::rules_inject::rules_dedicated_markdown(),
        );
        println!("Created .claude/rules/lean-ctx.md (Claude Code project rules).");
    }

    install_claude_project_hooks(&cwd);

    let kiro_dir = cwd.join(".kiro");
    if kiro_dir.exists() {
        let steering_dir = kiro_dir.join("steering");
        let steering_file = steering_dir.join("lean-ctx.md");
        if !steering_file.exists()
            || !std::fs::read_to_string(&steering_file)
                .unwrap_or_default()
                .contains("lean-ctx")
        {
            let _ = std::fs::create_dir_all(&steering_dir);
            write_file(&steering_file, KIRO_STEERING_TEMPLATE);
            println!("Created .kiro/steering/lean-ctx.md (Kiro steering).");
        }
    }
}

const PROJECT_LEAN_CTX_MD_MARKER: &str = "<!-- lean-ctx-owned: PROJECT-LEAN-CTX.md v1 -->";
const PROJECT_LEAN_CTX_MD: &str = "LEAN-CTX.md";
const PROJECT_AGENTS_MD: &str = "AGENTS.md";
const AGENTS_BLOCK_START: &str = "<!-- lean-ctx -->";
const AGENTS_BLOCK_END: &str = "<!-- /lean-ctx -->";

fn ensure_project_agents_integration(cwd: &std::path::Path) {
    let lean_ctx_md = cwd.join(PROJECT_LEAN_CTX_MD);
    let desired = format!(
        "{PROJECT_LEAN_CTX_MD_MARKER}\n{}\n",
        crate::rules_inject::rules_dedicated_markdown()
    );

    if !lean_ctx_md.exists() {
        write_file(&lean_ctx_md, &desired);
    } else if std::fs::read_to_string(&lean_ctx_md)
        .unwrap_or_default()
        .contains(PROJECT_LEAN_CTX_MD_MARKER)
    {
        let current = std::fs::read_to_string(&lean_ctx_md).unwrap_or_default();
        if !current.contains(crate::rules_inject::RULES_VERSION_STR) {
            write_file(&lean_ctx_md, &desired);
        }
    }

    let block = format!(
        "{AGENTS_BLOCK_START}\n\
## lean-ctx\n\n\
Prefer lean-ctx MCP tools over native equivalents for token savings.\n\
Full rules: @{PROJECT_LEAN_CTX_MD}\n\
{AGENTS_BLOCK_END}\n"
    );

    let agents_md = cwd.join(PROJECT_AGENTS_MD);
    if !agents_md.exists() {
        let content = format!("# Agent Instructions\n\n{block}");
        write_file(&agents_md, &content);
        println!("Created AGENTS.md in project root (lean-ctx reference only).");
        return;
    }

    let existing = std::fs::read_to_string(&agents_md).unwrap_or_default();
    if existing.contains(AGENTS_BLOCK_START) {
        let updated = replace_marked_block(&existing, AGENTS_BLOCK_START, AGENTS_BLOCK_END, &block);
        if updated != existing {
            write_file(&agents_md, &updated);
        }
        return;
    }

    if existing.contains("lean-ctx") && existing.contains(PROJECT_LEAN_CTX_MD) {
        return;
    }

    let mut out = existing;
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    out.push_str(&block);
    write_file(&agents_md, &out);
    println!("Updated AGENTS.md (added lean-ctx reference block).");
}

fn replace_marked_block(content: &str, start: &str, end: &str, replacement: &str) -> String {
    let s = content.find(start);
    let e = content.find(end);
    match (s, e) {
        (Some(si), Some(ei)) if ei >= si => {
            let after_end = ei + end.len();
            let before = &content[..si];
            let after = &content[after_end..];
            let mut out = String::new();
            out.push_str(before.trim_end_matches('\n'));
            out.push('\n');
            out.push('\n');
            out.push_str(replacement.trim_end_matches('\n'));
            out.push('\n');
            out.push_str(after.trim_start_matches('\n'));
            out
        }
        _ => content.to_string(),
    }
}

const CURSORRULES_TEMPLATE: &str = "\
# lean-ctx — Context Engineering Layer

PREFER lean-ctx MCP tools over native equivalents for token savings:

| PREFER | OVER | Why |
|--------|------|-----|
| `ctx_read(path)` | `Read` | Cached, 10 compression modes |
| `ctx_shell(command)` | `Shell` | Pattern compression |
| `ctx_search(pattern, path)` | `Grep` | Compact results |
| `ctx_tree(path, depth)` | `ls` / `find` | Directory maps |
| `ctx_edit(path, old_string, new_string)` | `Edit` (when Read unavailable) | Search-and-replace without native Read |

Edit files: use native Edit/StrReplace if available. If Edit requires Read and Read is unavailable, use ctx_edit.
Write, Delete, Glob — use normally. NEVER loop on Edit failures — switch to ctx_edit immediately.
";

pub const KIRO_STEERING_TEMPLATE: &str = "\
---
inclusion: always
---

# lean-ctx — Context Engineering Layer

The workspace has the `lean-ctx` MCP server installed. You MUST prefer lean-ctx tools over native equivalents for token efficiency and caching.

## Mandatory Tool Preferences

| Use this | Instead of | Why |
|----------|-----------|-----|
| `mcp_lean_ctx_ctx_read` | `readFile`, `readCode` | Cached reads, 10 compression modes, re-reads cost ~13 tokens |
| `mcp_lean_ctx_ctx_multi_read` | `readMultipleFiles` | Batch cached reads in one call |
| `mcp_lean_ctx_ctx_shell` | `executeBash` | Pattern compression for git/npm/test output |
| `mcp_lean_ctx_ctx_search` | `grepSearch` | Compact, .gitignore-aware results |
| `mcp_lean_ctx_ctx_tree` | `listDirectory` | Compact directory maps with file counts |

## When to use native Kiro tools instead

- `fsWrite` / `fsAppend` — always use native (lean-ctx doesn't write files)
- `strReplace` — always use native (precise string replacement)
- `semanticRename` / `smartRelocate` — always use native (IDE integration)
- `getDiagnostics` — always use native (language server diagnostics)
- `deleteFile` — always use native

## Session management

- At the start of a long task, call `mcp_lean_ctx_ctx_preload` with a task description to warm the cache
- Use `mcp_lean_ctx_ctx_compress` periodically in long conversations to checkpoint context
- Use `mcp_lean_ctx_ctx_knowledge` to persist important discoveries across sessions

## Rules

- NEVER loop on edit failures — switch to `mcp_lean_ctx_ctx_edit` immediately
- For large files, use `mcp_lean_ctx_ctx_read` with `mode: \"signatures\"` or `mode: \"map\"` first
- For re-reading a file you already read, just call `mcp_lean_ctx_ctx_read` again (cache hit = ~13 tokens)
- When running tests or build commands, use `mcp_lean_ctx_ctx_shell` for compressed output
";

pub fn install_agent_hook(agent: &str, global: bool) {
    match agent {
        "claude" | "claude-code" => install_claude_hook(global),
        "cursor" => install_cursor_hook(global),
        "gemini" | "antigravity" => install_gemini_hook(),
        "codex" => install_codex_hook(),
        "windsurf" => install_windsurf_rules(global),
        "cline" | "roo" => install_cline_rules(global),
        "copilot" => install_copilot_hook(global),
        "pi" => install_pi_hook(global),
        "qwen" => install_mcp_json_agent(
            "Qwen Code",
            "~/.qwen/mcp.json",
            &dirs::home_dir().unwrap_or_default().join(".qwen/mcp.json"),
        ),
        "trae" => install_mcp_json_agent(
            "Trae",
            "~/.trae/mcp.json",
            &dirs::home_dir().unwrap_or_default().join(".trae/mcp.json"),
        ),
        "amazonq" => install_mcp_json_agent(
            "Amazon Q Developer",
            "~/.aws/amazonq/mcp.json",
            &dirs::home_dir()
                .unwrap_or_default()
                .join(".aws/amazonq/mcp.json"),
        ),
        "jetbrains" => install_jetbrains_hook(),
        "kiro" => install_kiro_hook(),
        "verdent" => install_mcp_json_agent(
            "Verdent",
            "~/.verdent/mcp.json",
            &dirs::home_dir()
                .unwrap_or_default()
                .join(".verdent/mcp.json"),
        ),
        "opencode" => install_opencode_hook(),
        "aider" => install_mcp_json_agent(
            "Aider",
            "~/.aider/mcp.json",
            &dirs::home_dir().unwrap_or_default().join(".aider/mcp.json"),
        ),
        "amp" => install_amp_hook(),
        "crush" => install_crush_hook(),
        "hermes" => install_hermes_hook(global),
        _ => {
            eprintln!("Unknown agent: {agent}");
            eprintln!("  Supported: claude, cursor, gemini, codex, windsurf, cline, roo, copilot, pi, qwen, trae, amazonq, jetbrains, kiro, verdent, opencode, aider, amp, crush, antigravity, hermes");
            std::process::exit(1);
        }
    }
}

fn write_file(path: &std::path::Path, content: &str) {
    if let Err(e) = crate::config_io::write_atomic_with_backup(path, content) {
        tracing::error!("Error writing {}: {e}", path.display());
    }
}

fn is_inside_git_repo(path: &std::path::Path) -> bool {
    let mut p = path;
    loop {
        if p.join(".git").exists() {
            return true;
        }
        match p.parent() {
            Some(parent) => p = parent,
            None => return false,
        }
    }
}

#[cfg(unix)]
fn make_executable(path: &PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
}

#[cfg(not(unix))]
fn make_executable(_path: &PathBuf) {}

fn full_server_entry(binary: &str) -> serde_json::Value {
    let data_dir = crate::core::data_dir::lean_ctx_data_dir()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_default();
    let auto_approve = crate::core::editor_registry::auto_approve_tools();
    serde_json::json!({
        "command": binary,
        "env": { "LEAN_CTX_DATA_DIR": data_dir },
        "autoApprove": auto_approve
    })
}

fn install_mcp_json_agent(name: &str, display_path: &str, config_path: &std::path::Path) {
    let binary = resolve_binary_path();
    let entry = full_server_entry(&binary);
    install_named_json_server(name, display_path, config_path, "mcpServers", entry);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_path_unix_unchanged() {
        assert_eq!(
            to_bash_compatible_path("/usr/local/bin/lean-ctx"),
            "/usr/local/bin/lean-ctx"
        );
    }

    #[test]
    fn bash_path_home_unchanged() {
        assert_eq!(
            to_bash_compatible_path("/home/user/.cargo/bin/lean-ctx"),
            "/home/user/.cargo/bin/lean-ctx"
        );
    }

    #[test]
    fn bash_path_windows_drive_converted() {
        assert_eq!(
            to_bash_compatible_path("C:\\Users\\Fraser\\bin\\lean-ctx.exe"),
            "/c/Users/Fraser/bin/lean-ctx.exe"
        );
    }

    #[test]
    fn bash_path_windows_lowercase_drive() {
        assert_eq!(
            to_bash_compatible_path("D:\\tools\\lean-ctx.exe"),
            "/d/tools/lean-ctx.exe"
        );
    }

    #[test]
    fn bash_path_windows_forward_slashes() {
        assert_eq!(
            to_bash_compatible_path("C:/Users/Fraser/bin/lean-ctx.exe"),
            "/c/Users/Fraser/bin/lean-ctx.exe"
        );
    }

    #[test]
    fn bash_path_bare_name_unchanged() {
        assert_eq!(to_bash_compatible_path("lean-ctx"), "lean-ctx");
    }

    #[test]
    fn normalize_msys2_path() {
        assert_eq!(
            normalize_tool_path("/c/Users/game/Downloads/project"),
            "C:/Users/game/Downloads/project"
        );
    }

    #[test]
    fn normalize_msys2_drive_d() {
        assert_eq!(
            normalize_tool_path("/d/Projects/app/src"),
            "D:/Projects/app/src"
        );
    }

    #[test]
    fn normalize_backslashes() {
        assert_eq!(
            normalize_tool_path("C:\\Users\\game\\project\\src"),
            "C:/Users/game/project/src"
        );
    }

    #[test]
    fn normalize_mixed_separators() {
        assert_eq!(
            normalize_tool_path("C:\\Users/game\\project/src"),
            "C:/Users/game/project/src"
        );
    }

    #[test]
    fn normalize_double_slashes() {
        assert_eq!(
            normalize_tool_path("/home/user//project///src"),
            "/home/user/project/src"
        );
    }

    #[test]
    fn normalize_trailing_slash() {
        assert_eq!(
            normalize_tool_path("/home/user/project/"),
            "/home/user/project"
        );
    }

    #[test]
    fn normalize_root_preserved() {
        assert_eq!(normalize_tool_path("/"), "/");
    }

    #[test]
    fn normalize_windows_root_preserved() {
        assert_eq!(normalize_tool_path("C:/"), "C:/");
    }

    #[test]
    fn normalize_unix_path_unchanged() {
        assert_eq!(
            normalize_tool_path("/home/user/project/src/main.rs"),
            "/home/user/project/src/main.rs"
        );
    }

    #[test]
    fn normalize_relative_path_unchanged() {
        assert_eq!(normalize_tool_path("src/main.rs"), "src/main.rs");
    }

    #[test]
    fn normalize_dot_unchanged() {
        assert_eq!(normalize_tool_path("."), ".");
    }

    #[test]
    fn normalize_unc_path_preserved() {
        assert_eq!(
            normalize_tool_path("//server/share/file"),
            "//server/share/file"
        );
    }

    #[test]
    fn cursor_hook_config_has_version_and_object_hooks() {
        let config = serde_json::json!({
            "version": 1,
            "hooks": {
                "preToolUse": [
                    {
                        "matcher": "terminal_command",
                        "command": "lean-ctx hook rewrite"
                    },
                    {
                        "matcher": "read_file|grep|search|list_files|list_directory",
                        "command": "lean-ctx hook redirect"
                    }
                ]
            }
        });

        let json_str = serde_json::to_string_pretty(&config).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(parsed["version"], 1);
        assert!(parsed["hooks"].is_object());
        assert!(parsed["hooks"]["preToolUse"].is_array());
        assert_eq!(parsed["hooks"]["preToolUse"].as_array().unwrap().len(), 2);
        assert_eq!(
            parsed["hooks"]["preToolUse"][0]["matcher"],
            "terminal_command"
        );
    }

    #[test]
    fn cursor_hook_detects_old_format_needs_migration() {
        let old_format = r#"{"hooks":[{"event":"preToolUse","command":"lean-ctx hook rewrite"}]}"#;
        let has_correct =
            old_format.contains("\"version\"") && old_format.contains("\"preToolUse\"");
        assert!(
            !has_correct,
            "Old format should be detected as needing migration"
        );
    }

    #[test]
    fn gemini_hook_config_has_type_command() {
        let binary = "lean-ctx";
        let rewrite_cmd = format!("{binary} hook rewrite");
        let redirect_cmd = format!("{binary} hook redirect");

        let hook_config = serde_json::json!({
            "hooks": {
                "BeforeTool": [
                    {
                        "hooks": [{
                            "type": "command",
                            "command": rewrite_cmd
                        }]
                    },
                    {
                        "hooks": [{
                            "type": "command",
                            "command": redirect_cmd
                        }]
                    }
                ]
            }
        });

        let parsed = hook_config;
        let before_tool = parsed["hooks"]["BeforeTool"].as_array().unwrap();
        assert_eq!(before_tool.len(), 2);

        let first_hook = &before_tool[0]["hooks"][0];
        assert_eq!(first_hook["type"], "command");
        assert_eq!(first_hook["command"], "lean-ctx hook rewrite");

        let second_hook = &before_tool[1]["hooks"][0];
        assert_eq!(second_hook["type"], "command");
        assert_eq!(second_hook["command"], "lean-ctx hook redirect");
    }

    #[test]
    fn gemini_hook_old_format_detected() {
        let old_format = r#"{"hooks":{"BeforeTool":[{"command":"lean-ctx hook rewrite"}]}}"#;
        let has_new = old_format.contains("hook rewrite")
            && old_format.contains("hook redirect")
            && old_format.contains("\"type\"");
        assert!(!has_new, "Missing 'type' field should trigger migration");
    }

    #[test]
    fn rewrite_script_uses_registry_pattern() {
        let script = generate_rewrite_script("/usr/bin/lean-ctx");
        assert!(script.contains(r"git\ *"), "script missing git pattern");
        assert!(script.contains(r"cargo\ *"), "script missing cargo pattern");
        assert!(script.contains(r"npm\ *"), "script missing npm pattern");
        assert!(
            !script.contains(r"rg\ *"),
            "script should not contain rg pattern"
        );
        assert!(
            script.contains("LEAN_CTX_BIN=\"/usr/bin/lean-ctx\""),
            "script missing binary path"
        );
    }

    #[test]
    fn compact_rewrite_script_uses_registry_pattern() {
        let script = generate_compact_rewrite_script("/usr/bin/lean-ctx");
        assert!(script.contains(r"git\ *"), "compact script missing git");
        assert!(script.contains(r"cargo\ *"), "compact script missing cargo");
        assert!(
            !script.contains(r"rg\ *"),
            "compact script should not contain rg"
        );
    }

    #[test]
    fn rewrite_scripts_contain_all_registry_commands() {
        let script = generate_rewrite_script("lean-ctx");
        let compact = generate_compact_rewrite_script("lean-ctx");
        for entry in crate::rewrite_registry::REWRITE_COMMANDS {
            if entry.category == crate::rewrite_registry::Category::Search {
                continue;
            }
            let pattern = if entry.command.contains('-') {
                format!("{}*", entry.command.replace('-', r"\-"))
            } else {
                format!(r"{}\ *", entry.command)
            };
            assert!(
                script.contains(&pattern),
                "rewrite_script missing '{}' (pattern: {})",
                entry.command,
                pattern
            );
            assert!(
                compact.contains(&pattern),
                "compact_rewrite_script missing '{}' (pattern: {})",
                entry.command,
                pattern
            );
        }
    }
}
