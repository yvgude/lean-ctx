use std::path::PathBuf;

fn mcp_server_quiet_mode() -> bool {
    std::env::var_os("LEAN_CTX_MCP_SERVER").is_some()
}

/// Silently refresh all hook scripts for agents that are already configured.
/// Called after updates and on MCP server start to ensure hooks match the current binary version.
pub fn refresh_installed_hooks() {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return,
    };

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

    if home.join(".codex/hooks/lean-ctx-rewrite-codex.sh").exists() {
        install_codex_hook_scripts(&home);
    }
}

fn resolve_binary_path() -> String {
    if is_lean_ctx_in_path() {
        return "lean-ctx".to_string();
    }
    std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "lean-ctx".to_string())
}

fn is_lean_ctx_in_path() -> bool {
    let which_cmd = if cfg!(windows) { "where" } else { "which" };
    std::process::Command::new(which_cmd)
        .arg("lean-ctx")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn resolve_binary_path_for_bash() -> String {
    let path = resolve_binary_path();
    to_bash_compatible_path(&path)
}

pub fn to_bash_compatible_path(path: &str) -> String {
    let path = path.replace('\\', "/");
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
    let mut p = path.to_string();

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
  git\ *|gh\ *|cargo\ *|npm\ *|pnpm\ *|yarn\ *|docker\ *|kubectl\ *|pip\ *|pip3\ *|ruff\ *|go\ *|curl\ *|grep\ *|rg\ *|find\ *|cat\ *|head\ *|tail\ *|ls\ *|ls|eslint*|prettier*|tsc*|pytest*|mypy*|aws\ *|helm\ *)
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
    format!(
        r#"#!/usr/bin/env bash
# lean-ctx hook — rewrites shell commands
set -euo pipefail
LEAN_CTX_BIN="{binary}"
INPUT=$(cat)
CMD=$(echo "$INPUT" | grep -oE '"command":"([^"\\]|\\.)*"' | head -1 | sed 's/^"command":"//;s/"$//' | sed 's/\\"/"/g;s/\\\\/\\/g' 2>/dev/null || echo "")
if [ -z "$CMD" ] || echo "$CMD" | grep -qE "^(lean-ctx |$LEAN_CTX_BIN )"; then exit 0; fi
case "$CMD" in
  git\ *|gh\ *|cargo\ *|npm\ *|pnpm\ *|docker\ *|kubectl\ *|pip\ *|ruff\ *|go\ *|curl\ *|grep\ *|rg\ *|find\ *|ls\ *|ls|cat\ *|aws\ *|helm\ *)
    SHELL_ESC=$(printf '%s' "$CMD" | sed 's/\\/\\\\/g;s/"/\\"/g')
    REWRITE="$LEAN_CTX_BIN -c \"$SHELL_ESC\""
    JSON_CMD=$(printf '%s' "$REWRITE" | sed 's/\\/\\\\/g;s/"/\\"/g')
    printf '{{"hookSpecificOutput":{{"hookEventName":"PreToolUse","permissionDecision":"allow","updatedInput":{{"command":"%s"}}}}}}' "$JSON_CMD" ;;
  *) exit 0 ;;
esac
"#
    )
}

const REDIRECT_SCRIPT_CLAUDE: &str = r#"#!/usr/bin/env bash
# lean-ctx PreToolUse hook — all native tools pass through
# Read/Grep/ListFiles are allowed so Edit (which requires native Read) works.
# The MCP instructions guide the AI to prefer ctx_read/ctx_search/ctx_tree.
exit 0
"#;

const REDIRECT_SCRIPT_GENERIC: &str = r#"#!/usr/bin/env bash
# lean-ctx hook — all native tools pass through
exit 0
"#;

pub fn install_project_rules() {
    let cwd = std::env::current_dir().unwrap_or_default();

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
| `ctx_read(path)` | `Read` | Cached, 8 compression modes |
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
| `mcp_lean_ctx_ctx_read` | `readFile`, `readCode` | Cached reads, 8 compression modes, re-reads cost ~13 tokens |
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
        "jetbrains" => install_mcp_json_agent(
            "JetBrains IDEs",
            "~/.jb-mcp.json",
            &dirs::home_dir().unwrap_or_default().join(".jb-mcp.json"),
        ),
        "kiro" => install_kiro_hook(),
        "verdent" => install_mcp_json_agent(
            "Verdent",
            "~/.verdent/mcp.json",
            &dirs::home_dir()
                .unwrap_or_default()
                .join(".verdent/mcp.json"),
        ),
        "opencode" => install_mcp_json_agent(
            "OpenCode",
            "~/.opencode/mcp.json",
            &dirs::home_dir()
                .unwrap_or_default()
                .join(".opencode/mcp.json"),
        ),
        "aider" => install_mcp_json_agent(
            "Aider",
            "~/.aider/mcp.json",
            &dirs::home_dir().unwrap_or_default().join(".aider/mcp.json"),
        ),
        "amp" => install_mcp_json_agent(
            "Amp",
            "~/.amp/mcp.json",
            &dirs::home_dir().unwrap_or_default().join(".amp/mcp.json"),
        ),
        "crush" => install_crush_hook(),
        _ => {
            eprintln!("Unknown agent: {agent}");
            eprintln!("  Supported: claude, cursor, gemini, codex, windsurf, cline, roo, copilot, pi, qwen, trae, amazonq, jetbrains, kiro, verdent, opencode, aider, amp, crush, antigravity");
            std::process::exit(1);
        }
    }
}

fn install_claude_hook(global: bool) {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => {
            eprintln!("Cannot resolve home directory");
            return;
        }
    };

    install_claude_hook_scripts(&home);
    install_claude_hook_config(&home);
    install_claude_rules_file(&home);
    install_claude_skill(&home);

    let _ = global;
}

fn install_claude_skill(home: &std::path::Path) {
    let skill_dir = home.join(".claude/skills/lean-ctx");
    let _ = std::fs::create_dir_all(skill_dir.join("scripts"));

    let skill_md = include_str!("../skills/lean-ctx/SKILL.md");
    let install_sh = include_str!("../skills/lean-ctx/scripts/install.sh");

    let skill_path = skill_dir.join("SKILL.md");
    let script_path = skill_dir.join("scripts/install.sh");

    write_file(&skill_path, skill_md);
    write_file(&script_path, install_sh);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(mut perms) = std::fs::metadata(&script_path).map(|m| m.permissions()) {
            perms.set_mode(0o755);
            let _ = std::fs::set_permissions(&script_path, perms);
        }
    }
}

fn install_claude_rules_file(home: &std::path::Path) {
    let rules_dir = crate::core::editor_registry::claude_rules_dir(home);
    let _ = std::fs::create_dir_all(&rules_dir);
    let rules_path = rules_dir.join("lean-ctx.md");

    let desired = crate::rules_inject::rules_dedicated_markdown();
    let existing = std::fs::read_to_string(&rules_path).unwrap_or_default();

    if existing.is_empty() {
        write_file(&rules_path, desired);
        return;
    }
    if existing.contains(crate::rules_inject::RULES_VERSION_STR) {
        return;
    }
    if existing.contains("<!-- lean-ctx-rules-") {
        write_file(&rules_path, desired);
    }
}

fn install_claude_hook_scripts(home: &std::path::Path) {
    let hooks_dir = crate::core::editor_registry::claude_state_dir(home).join("hooks");
    let _ = std::fs::create_dir_all(&hooks_dir);

    let binary = resolve_binary_path();

    let rewrite_path = hooks_dir.join("lean-ctx-rewrite.sh");
    let rewrite_script = generate_rewrite_script(&resolve_binary_path_for_bash());
    write_file(&rewrite_path, &rewrite_script);
    make_executable(&rewrite_path);

    let redirect_path = hooks_dir.join("lean-ctx-redirect.sh");
    write_file(&redirect_path, REDIRECT_SCRIPT_CLAUDE);
    make_executable(&redirect_path);

    let wrapper = |subcommand: &str| -> String {
        if cfg!(windows) {
            format!("{binary} hook {subcommand}")
        } else {
            format!("{} hook {subcommand}", resolve_binary_path_for_bash())
        }
    };

    let rewrite_native = hooks_dir.join("lean-ctx-rewrite-native");
    write_file(
        &rewrite_native,
        &format!(
            "#!/bin/sh\nexec {} hook rewrite\n",
            resolve_binary_path_for_bash()
        ),
    );
    make_executable(&rewrite_native);

    let redirect_native = hooks_dir.join("lean-ctx-redirect-native");
    write_file(
        &redirect_native,
        &format!(
            "#!/bin/sh\nexec {} hook redirect\n",
            resolve_binary_path_for_bash()
        ),
    );
    make_executable(&redirect_native);

    let _ = wrapper; // suppress unused warning on unix
}

fn install_claude_hook_config(home: &std::path::Path) {
    let hooks_dir = crate::core::editor_registry::claude_state_dir(home).join("hooks");
    let binary = resolve_binary_path();

    let rewrite_cmd = format!("{binary} hook rewrite");
    let redirect_cmd = format!("{binary} hook redirect");

    let settings_path = crate::core::editor_registry::claude_state_dir(home).join("settings.json");
    let settings_content = if settings_path.exists() {
        std::fs::read_to_string(&settings_path).unwrap_or_default()
    } else {
        String::new()
    };

    let needs_update =
        !settings_content.contains("hook rewrite") || !settings_content.contains("hook redirect");
    let has_old_hooks = settings_content.contains("lean-ctx-rewrite.sh")
        || settings_content.contains("lean-ctx-redirect.sh");

    if !needs_update && !has_old_hooks {
        return;
    }

    let hook_entry = serde_json::json!({
        "hooks": {
            "PreToolUse": [
                {
                    "matcher": "Bash|bash",
                    "hooks": [{
                        "type": "command",
                        "command": rewrite_cmd
                    }]
                },
                {
                    "matcher": "Read|read|ReadFile|read_file|View|view|Grep|grep|Search|search|ListFiles|list_files|ListDirectory|list_directory",
                    "hooks": [{
                        "type": "command",
                        "command": redirect_cmd
                    }]
                }
            ]
        }
    });

    if settings_content.is_empty() {
        write_file(
            &settings_path,
            &serde_json::to_string_pretty(&hook_entry).unwrap(),
        );
    } else if let Ok(mut existing) = serde_json::from_str::<serde_json::Value>(&settings_content) {
        if let Some(obj) = existing.as_object_mut() {
            obj.insert("hooks".to_string(), hook_entry["hooks"].clone());
            write_file(
                &settings_path,
                &serde_json::to_string_pretty(&existing).unwrap(),
            );
        }
    }
    if !mcp_server_quiet_mode() {
        println!("Installed Claude Code hooks at {}", hooks_dir.display());
    }
}

fn install_cursor_hook(global: bool) {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => {
            eprintln!("Cannot resolve home directory");
            return;
        }
    };

    install_cursor_hook_scripts(&home);
    install_cursor_hook_config(&home);

    if !global {
        let rules_dir = PathBuf::from(".cursor").join("rules");
        let _ = std::fs::create_dir_all(&rules_dir);
        let rule_path = rules_dir.join("lean-ctx.mdc");
        if !rule_path.exists() {
            let rule_content = include_str!("templates/lean-ctx.mdc");
            write_file(&rule_path, rule_content);
            println!("Created .cursor/rules/lean-ctx.mdc in current project.");
        } else {
            println!("Cursor rule already exists.");
        }
    } else {
        println!("Global mode: skipping project-local .cursor/rules/ (use without --global in a project).");
    }

    println!("Restart Cursor to activate.");
}

fn install_cursor_hook_scripts(home: &std::path::Path) {
    let hooks_dir = home.join(".cursor").join("hooks");
    let _ = std::fs::create_dir_all(&hooks_dir);

    let binary = resolve_binary_path_for_bash();

    let rewrite_path = hooks_dir.join("lean-ctx-rewrite.sh");
    let rewrite_script = generate_compact_rewrite_script(&binary);
    write_file(&rewrite_path, &rewrite_script);
    make_executable(&rewrite_path);

    let redirect_path = hooks_dir.join("lean-ctx-redirect.sh");
    write_file(&redirect_path, REDIRECT_SCRIPT_GENERIC);
    make_executable(&redirect_path);

    let native_binary = resolve_binary_path();
    let rewrite_native = hooks_dir.join("lean-ctx-rewrite-native");
    write_file(
        &rewrite_native,
        &format!("#!/bin/sh\nexec {} hook rewrite\n", native_binary),
    );
    make_executable(&rewrite_native);

    let redirect_native = hooks_dir.join("lean-ctx-redirect-native");
    write_file(
        &redirect_native,
        &format!("#!/bin/sh\nexec {} hook redirect\n", native_binary),
    );
    make_executable(&redirect_native);
}

fn install_cursor_hook_config(home: &std::path::Path) {
    let binary = resolve_binary_path();
    let rewrite_cmd = format!("{binary} hook rewrite");
    let redirect_cmd = format!("{binary} hook redirect");

    let hooks_json = home.join(".cursor").join("hooks.json");

    let hook_config = serde_json::json!({
        "version": 1,
        "hooks": {
            "preToolUse": [
                {
                    "matcher": "terminal_command",
                    "command": rewrite_cmd
                },
                {
                    "matcher": "read_file|grep|search|list_files|list_directory",
                    "command": redirect_cmd
                }
            ]
        }
    });

    let content = if hooks_json.exists() {
        std::fs::read_to_string(&hooks_json).unwrap_or_default()
    } else {
        String::new()
    };

    let has_correct_format = content.contains("\"version\"") && content.contains("\"preToolUse\"");
    if has_correct_format && content.contains("hook rewrite") && content.contains("hook redirect") {
        return;
    }

    if content.is_empty() || !content.contains("\"version\"") {
        write_file(
            &hooks_json,
            &serde_json::to_string_pretty(&hook_config).unwrap(),
        );
    } else if let Ok(mut existing) = serde_json::from_str::<serde_json::Value>(&content) {
        if let Some(obj) = existing.as_object_mut() {
            obj.insert("version".to_string(), serde_json::json!(1));
            obj.insert("hooks".to_string(), hook_config["hooks"].clone());
            write_file(
                &hooks_json,
                &serde_json::to_string_pretty(&existing).unwrap(),
            );
        }
    } else {
        write_file(
            &hooks_json,
            &serde_json::to_string_pretty(&hook_config).unwrap(),
        );
    }

    if !mcp_server_quiet_mode() {
        println!("Installed Cursor hooks at {}", hooks_json.display());
    }
}

fn install_gemini_hook() {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => {
            eprintln!("Cannot resolve home directory");
            return;
        }
    };

    install_gemini_hook_scripts(&home);
    install_gemini_hook_config(&home);
}

fn install_gemini_hook_scripts(home: &std::path::Path) {
    let hooks_dir = home.join(".gemini").join("hooks");
    let _ = std::fs::create_dir_all(&hooks_dir);

    let binary = resolve_binary_path_for_bash();

    let rewrite_path = hooks_dir.join("lean-ctx-rewrite-gemini.sh");
    let rewrite_script = generate_compact_rewrite_script(&binary);
    write_file(&rewrite_path, &rewrite_script);
    make_executable(&rewrite_path);

    let redirect_path = hooks_dir.join("lean-ctx-redirect-gemini.sh");
    write_file(&redirect_path, REDIRECT_SCRIPT_GENERIC);
    make_executable(&redirect_path);
}

fn install_gemini_hook_config(home: &std::path::Path) {
    let binary = resolve_binary_path();
    let rewrite_cmd = format!("{binary} hook rewrite");
    let redirect_cmd = format!("{binary} hook redirect");

    let settings_path = home.join(".gemini").join("settings.json");
    let settings_content = if settings_path.exists() {
        std::fs::read_to_string(&settings_path).unwrap_or_default()
    } else {
        String::new()
    };

    let has_new_format = settings_content.contains("hook rewrite")
        && settings_content.contains("hook redirect")
        && settings_content.contains("\"type\"");
    let has_old_hooks = settings_content.contains("lean-ctx-rewrite")
        || settings_content.contains("lean-ctx-redirect")
        || (settings_content.contains("hook rewrite") && !settings_content.contains("\"type\""));

    if has_new_format && !has_old_hooks {
        return;
    }

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

    if settings_content.is_empty() {
        write_file(
            &settings_path,
            &serde_json::to_string_pretty(&hook_config).unwrap(),
        );
    } else if let Ok(mut existing) = serde_json::from_str::<serde_json::Value>(&settings_content) {
        if let Some(obj) = existing.as_object_mut() {
            obj.insert("hooks".to_string(), hook_config["hooks"].clone());
            write_file(
                &settings_path,
                &serde_json::to_string_pretty(&existing).unwrap(),
            );
        }
    }
    if !mcp_server_quiet_mode() {
        println!(
            "Installed Gemini CLI hooks at {}",
            settings_path.parent().unwrap_or(&settings_path).display()
        );
    }
}

fn install_codex_hook() {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => {
            eprintln!("Cannot resolve home directory");
            return;
        }
    };

    let codex_dir = home.join(".codex");
    let _ = std::fs::create_dir_all(&codex_dir);

    install_codex_hook_scripts(&home);

    let agents_path = codex_dir.join("AGENTS.md");
    let agents_content = "# Global Agent Instructions\n\n@LEAN-CTX.md\n";

    let lean_ctx_md = codex_dir.join("LEAN-CTX.md");
    let binary = resolve_binary_path();
    let lean_ctx_content = format!(
        r#"# lean-ctx — Token Optimization

Prefix all shell commands with `{binary} -c` for compressed output:

```bash
{binary} -c git status    # instead of: git status
{binary} -c cargo test    # instead of: cargo test
{binary} -c ls src/       # instead of: ls src/
```

This saves 60-90% tokens per command. Works with: git, cargo, npm, pnpm, docker, kubectl, pip, ruff, go, curl, grep, find, ls, aws, helm, and 90+ more commands.
Use `{binary} -c --raw <cmd>` to skip compression and get full output.
"#
    );

    if agents_path.exists() {
        let content = std::fs::read_to_string(&agents_path).unwrap_or_default();
        if content.contains("lean-ctx") || content.contains("LEAN-CTX") {
            println!("Codex AGENTS.md already configured.");
            return;
        }
    }

    write_file(&agents_path, agents_content);
    write_file(&lean_ctx_md, &lean_ctx_content);
    println!("Installed Codex instructions at {}", codex_dir.display());
}

fn install_codex_hook_scripts(home: &std::path::Path) {
    let hooks_dir = home.join(".codex").join("hooks");
    let _ = std::fs::create_dir_all(&hooks_dir);

    let binary = resolve_binary_path_for_bash();
    let rewrite_path = hooks_dir.join("lean-ctx-rewrite-codex.sh");
    let rewrite_script = generate_compact_rewrite_script(&binary);
    write_file(&rewrite_path, &rewrite_script);
    make_executable(&rewrite_path);
    if !mcp_server_quiet_mode() {
        println!(
            "  \x1b[32m✓\x1b[0m Installed Codex hook scripts at {}",
            hooks_dir.display()
        );
    }
}

fn install_windsurf_rules(global: bool) {
    if global {
        println!("Global mode: skipping project-local .windsurfrules (use without --global in a project).");
        return;
    }

    let rules_path = PathBuf::from(".windsurfrules");
    if rules_path.exists() {
        let content = std::fs::read_to_string(&rules_path).unwrap_or_default();
        if content.contains("lean-ctx") {
            println!(".windsurfrules already configured.");
            return;
        }
    }

    let rules = include_str!("templates/windsurfrules.txt");
    write_file(&rules_path, rules);
    println!("Installed .windsurfrules in current project.");
}

fn install_cline_rules(global: bool) {
    if global {
        println!(
            "Global mode: skipping project-local .clinerules (use without --global in a project)."
        );
        return;
    }

    let rules_path = PathBuf::from(".clinerules");
    if rules_path.exists() {
        let content = std::fs::read_to_string(&rules_path).unwrap_or_default();
        if content.contains("lean-ctx") {
            println!(".clinerules already configured.");
            return;
        }
    }

    let binary = resolve_binary_path();
    let rules = format!(
        r#"# lean-ctx Shell Optimization
# Prefix all shell commands with `{binary} -c` for 60-90% token savings.

When running shell commands, always prefix them with `{binary} -c` to compress output. For example:
- `{binary} -c git status` instead of `git status`
- `{binary} -c cargo test` instead of `cargo test`
- `{binary} -c ls src/` instead of `ls src/`

Supported commands: git, cargo, npm, pnpm, docker, kubectl, pip, ruff, go, curl, grep, find, ls, aws, helm, and 90+ more.
"#
    );

    write_file(&rules_path, &rules);
    println!("Installed .clinerules in current project.");
}

fn install_pi_hook(global: bool) {
    let has_pi = std::process::Command::new("pi")
        .arg("--version")
        .output()
        .is_ok();

    if !has_pi {
        println!("Pi Coding Agent not found in PATH.");
        println!("Install Pi first: npm install -g @mariozechner/pi-coding-agent");
        println!();
    }

    println!("Installing pi-lean-ctx Pi Package...");
    println!();

    let install_result = std::process::Command::new("pi")
        .args(["install", "npm:pi-lean-ctx"])
        .status();

    match install_result {
        Ok(status) if status.success() => {
            println!("Installed pi-lean-ctx Pi Package.");
        }
        _ => {
            println!("Could not auto-install pi-lean-ctx. Install manually:");
            println!("  pi install npm:pi-lean-ctx");
            println!();
        }
    }

    write_pi_mcp_config();

    if !global {
        let agents_md = PathBuf::from("AGENTS.md");
        if !agents_md.exists()
            || !std::fs::read_to_string(&agents_md)
                .unwrap_or_default()
                .contains("lean-ctx")
        {
            let content = include_str!("templates/PI_AGENTS.md");
            write_file(&agents_md, content);
            println!("Created AGENTS.md in current project directory.");
        } else {
            println!("AGENTS.md already contains lean-ctx configuration.");
        }
    } else {
        println!(
            "Global mode: skipping project-local AGENTS.md (use without --global in a project)."
        );
    }

    println!();
    println!("Setup complete. All Pi tools (bash, read, grep, find, ls) route through lean-ctx.");
    println!("MCP tools (ctx_session, ctx_knowledge, ctx_semantic_search, ...) also available.");
    println!("Use /lean-ctx in Pi to verify the binary path and MCP status.");
}

fn write_pi_mcp_config() {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return,
    };

    let mcp_config_path = home.join(".pi/agent/mcp.json");

    if !home.join(".pi/agent").exists() {
        println!("  \x1b[2m○ ~/.pi/agent/ not found — skipping MCP config\x1b[0m");
        return;
    }

    if mcp_config_path.exists() {
        let content = match std::fs::read_to_string(&mcp_config_path) {
            Ok(c) => c,
            Err(_) => return,
        };
        if content.contains("lean-ctx") {
            println!("  \x1b[32m✓\x1b[0m Pi MCP config already contains lean-ctx");
            return;
        }

        if let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(obj) = json.as_object_mut() {
                let servers = obj
                    .entry("mcpServers")
                    .or_insert_with(|| serde_json::json!({}));
                if let Some(servers_obj) = servers.as_object_mut() {
                    servers_obj.insert("lean-ctx".to_string(), pi_mcp_server_entry());
                }
                if let Ok(formatted) = serde_json::to_string_pretty(&json) {
                    let _ = std::fs::write(&mcp_config_path, formatted);
                    println!(
                        "  \x1b[32m✓\x1b[0m Added lean-ctx to Pi MCP config (~/.pi/agent/mcp.json)"
                    );
                }
            }
        }
        return;
    }

    let content = serde_json::json!({
        "mcpServers": {
            "lean-ctx": pi_mcp_server_entry()
        }
    });
    if let Ok(formatted) = serde_json::to_string_pretty(&content) {
        let _ = std::fs::write(&mcp_config_path, formatted);
        println!("  \x1b[32m✓\x1b[0m Created Pi MCP config (~/.pi/agent/mcp.json)");
    }
}

fn pi_mcp_server_entry() -> serde_json::Value {
    let binary = resolve_binary_path();
    serde_json::json!({
        "command": binary,
        "lifecycle": "lazy",
        "directTools": true
    })
}

fn install_copilot_hook(global: bool) {
    let binary = resolve_binary_path();

    if global {
        let mcp_path = copilot_global_mcp_path();
        if mcp_path.as_os_str() == "/nonexistent" {
            println!("  \x1b[2mVS Code not found — skipping global Copilot config\x1b[0m");
            return;
        }
        write_vscode_mcp_file(&mcp_path, &binary, "global VS Code User MCP");
    } else {
        let vscode_dir = PathBuf::from(".vscode");
        let _ = std::fs::create_dir_all(&vscode_dir);
        let mcp_path = vscode_dir.join("mcp.json");
        write_vscode_mcp_file(&mcp_path, &binary, ".vscode/mcp.json");
    }
}

fn copilot_global_mcp_path() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        #[cfg(target_os = "macos")]
        {
            return home.join("Library/Application Support/Code/User/mcp.json");
        }
        #[cfg(target_os = "linux")]
        {
            return home.join(".config/Code/User/mcp.json");
        }
        #[cfg(target_os = "windows")]
        {
            if let Ok(appdata) = std::env::var("APPDATA") {
                return PathBuf::from(appdata).join("Code/User/mcp.json");
            }
        }
        #[allow(unreachable_code)]
        home.join(".config/Code/User/mcp.json")
    } else {
        PathBuf::from("/nonexistent")
    }
}

fn write_vscode_mcp_file(mcp_path: &PathBuf, binary: &str, label: &str) {
    let desired = serde_json::json!({ "command": binary, "args": [] });
    if mcp_path.exists() {
        let content = std::fs::read_to_string(mcp_path).unwrap_or_default();
        match serde_json::from_str::<serde_json::Value>(&content) {
            Ok(mut json) => {
                if let Some(obj) = json.as_object_mut() {
                    let servers = obj
                        .entry("servers")
                        .or_insert_with(|| serde_json::json!({}));
                    if let Some(servers_obj) = servers.as_object_mut() {
                        if servers_obj.get("lean-ctx") == Some(&desired) {
                            println!("  \x1b[32m✓\x1b[0m Copilot already configured in {label}");
                            return;
                        }
                        servers_obj.insert("lean-ctx".to_string(), desired);
                    }
                    write_file(
                        mcp_path,
                        &serde_json::to_string_pretty(&json).unwrap_or_default(),
                    );
                    println!("  \x1b[32m✓\x1b[0m Added lean-ctx to {label}");
                    return;
                }
            }
            Err(e) => {
                eprintln!(
                    "Could not parse VS Code MCP config at {}: {e}\nAdd to \"servers\": \"lean-ctx\": {{ \"command\": \"{}\", \"args\": [] }}",
                    mcp_path.display(),
                    binary
                );
                return;
            }
        };
    }

    if let Some(parent) = mcp_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let config = serde_json::json!({
        "servers": {
            "lean-ctx": {
                "command": binary,
                "args": []
            }
        }
    });

    write_file(
        mcp_path,
        &serde_json::to_string_pretty(&config).unwrap_or_default(),
    );
    println!("  \x1b[32m✓\x1b[0m Created {label} with lean-ctx MCP server");
}

fn write_file(path: &std::path::Path, content: &str) {
    if let Err(e) = crate::config_io::write_atomic_with_backup(path, content) {
        eprintln!("Error writing {}: {e}", path.display());
    }
}

#[cfg(unix)]
fn make_executable(path: &PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
}

#[cfg(not(unix))]
fn make_executable(_path: &PathBuf) {}

fn install_crush_hook() {
    let binary = resolve_binary_path();
    let home = dirs::home_dir().unwrap_or_default();
    let config_path = home.join(".config/crush/crush.json");
    let display_path = "~/.config/crush/crush.json";

    if let Some(parent) = config_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    if config_path.exists() {
        let content = std::fs::read_to_string(&config_path).unwrap_or_default();
        if content.contains("lean-ctx") {
            println!("Crush MCP already configured at {display_path}");
            return;
        }

        if let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(obj) = json.as_object_mut() {
                let servers = obj.entry("mcp").or_insert_with(|| serde_json::json!({}));
                if let Some(servers_obj) = servers.as_object_mut() {
                    servers_obj.insert(
                        "lean-ctx".to_string(),
                        serde_json::json!({ "type": "stdio", "command": binary }),
                    );
                }
                if let Ok(formatted) = serde_json::to_string_pretty(&json) {
                    let _ = std::fs::write(&config_path, formatted);
                    println!("  \x1b[32m✓\x1b[0m Crush MCP configured at {display_path}");
                    return;
                }
            }
        }
    }

    let content = serde_json::to_string_pretty(&serde_json::json!({
        "mcp": {
            "lean-ctx": {
                "type": "stdio",
                "command": binary
            }
        }
    }));

    if let Ok(json_str) = content {
        let _ = std::fs::write(&config_path, json_str);
        println!("  \x1b[32m✓\x1b[0m Crush MCP configured at {display_path}");
    } else {
        eprintln!("  \x1b[31m✗\x1b[0m Failed to configure Crush");
    }
}

fn install_kiro_hook() {
    let home = dirs::home_dir().unwrap_or_default();

    install_mcp_json_agent(
        "AWS Kiro",
        "~/.kiro/settings/mcp.json",
        &home.join(".kiro/settings/mcp.json"),
    );

    let cwd = std::env::current_dir().unwrap_or_default();
    let steering_dir = cwd.join(".kiro").join("steering");
    let steering_file = steering_dir.join("lean-ctx.md");

    if steering_file.exists()
        && std::fs::read_to_string(&steering_file)
            .unwrap_or_default()
            .contains("lean-ctx")
    {
        println!("  Kiro steering file already exists at .kiro/steering/lean-ctx.md");
    } else {
        let _ = std::fs::create_dir_all(&steering_dir);
        write_file(&steering_file, KIRO_STEERING_TEMPLATE);
        println!("  \x1b[32m✓\x1b[0m Created .kiro/steering/lean-ctx.md (Kiro will now prefer lean-ctx tools)");
    }
}

fn install_mcp_json_agent(name: &str, display_path: &str, config_path: &std::path::Path) {
    let binary = resolve_binary_path();

    if let Some(parent) = config_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    if config_path.exists() {
        let content = std::fs::read_to_string(config_path).unwrap_or_default();
        if content.contains("lean-ctx") {
            println!("{name} MCP already configured at {display_path}");
            return;
        }

        if let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(obj) = json.as_object_mut() {
                let servers = obj
                    .entry("mcpServers")
                    .or_insert_with(|| serde_json::json!({}));
                if let Some(servers_obj) = servers.as_object_mut() {
                    servers_obj.insert(
                        "lean-ctx".to_string(),
                        serde_json::json!({ "command": binary }),
                    );
                }
                if let Ok(formatted) = serde_json::to_string_pretty(&json) {
                    let _ = std::fs::write(config_path, formatted);
                    println!("  \x1b[32m✓\x1b[0m {name} MCP configured at {display_path}");
                    return;
                }
            }
        }
    }

    let content = serde_json::to_string_pretty(&serde_json::json!({
        "mcpServers": {
            "lean-ctx": {
                "command": binary
            }
        }
    }));

    if let Ok(json_str) = content {
        let _ = std::fs::write(config_path, json_str);
        println!("  \x1b[32m✓\x1b[0m {name} MCP configured at {display_path}");
    } else {
        eprintln!("  \x1b[31m✗\x1b[0m Failed to configure {name}");
    }
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
}
