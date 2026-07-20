use std::path::PathBuf;

pub mod agents;
mod support;

/// Controls how hooks instruct agents to access lean-ctx functionality.
///
/// * `Mcp` — MCP server only (extension/plugin-based agents without reliable shell).
/// * `Hybrid` — MCP server + shell hooks for command compression (best of both).
/// * `Replace` — Native Read/Grep/Glob/Shell are **denied**; lean-ctx MCP tools are
///   the only path. Eliminates tool drift entirely — no agent compliance needed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookMode {
    #[default]
    Mcp,
    Hybrid,
    Replace,
}

impl std::fmt::Display for HookMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Mcp => write!(f, "MCP"),
            Self::Hybrid => write!(f, "Hybrid"),
            Self::Replace => write!(f, "Replace"),
        }
    }
}

impl HookMode {
    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s.to_lowercase().replace('-', "").as_str() {
            "mcp" => Some(Self::Mcp),
            "hybrid" => Some(Self::Hybrid),
            "replace" => Some(Self::Replace),
            _ => None,
        }
    }

    /// Downgrade Replace → Hybrid; leave Hybrid/Mcp unchanged.
    pub fn cap_at_hybrid(self) -> Self {
        if matches!(self, Self::Replace) {
            Self::Hybrid
        } else {
            self
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::Mcp => "MCP server only (extension/plugin-based agents without reliable shell)",
            Self::Hybrid => "MCP server + shell hooks for command compression (best of both)",
            Self::Replace => {
                "Native tools denied — lean-ctx MCP is the only path (zero tool drift)"
            }
        }
    }
}

/// Agents with reliable shell + hook infrastructure that support Replace mode
/// (native tools denied, lean-ctx MCP is the only path). These agents have
/// either `permissions.deny` support or PreToolUse deny-hook capability.
pub const REPLACE_AGENTS: &[&str] = &[
    "cursor",
    "claude",
    "claude-code",
    "codebuddy",
    "codex",
    "windsurf",
    "opencode",
    "gemini",
];

/// Agents that get Hybrid mode (MCP + shell hooks) because they lack reliable
/// deny infrastructure but do have shell hooks for command compression.
pub const HYBRID_AGENTS: &[&str] = &[
    "cursor",
    "gemini",
    "codex",
    "claude",
    "claude-code",
    "crush",
    "hermes",
    "opencode",
    "openclaw",
    "pi",
    "qoder",
    "windsurf",
    "amp",
    "cline",
    "roo",
    "copilot",
    "kiro",
    "qwen",
    "trae",
    "antigravity",
    "antigravity-cli",
    "amazonq",
    "verdent",
];

/// Auto-detect the best hook mode for a given agent key.
///
/// Priority: disabled/shadow_mode cap > config override > Replace > Hybrid > Mcp
/// - Replace: native tools denied, MCP-only path (zero tool drift)
/// - Hybrid: MCP + shell hooks (fallback for agents without deny support)
/// - Mcp: MCP server only (no shell hooks available)
///
/// `LEAN_CTX_DISABLED=1` or `shadow_mode = false` cap the mode at Hybrid so
/// the deny-list is never injected when the user opted out (#1037).
pub fn recommend_hook_mode(agent_key: &str) -> HookMode {
    let deny_suppressed = is_deny_suppressed();
    if let Some(override_mode) = crate::core::config::Config::load().hook_mode_override() {
        return if deny_suppressed {
            override_mode.cap_at_hybrid()
        } else {
            override_mode
        };
    }
    if deny_suppressed {
        if REPLACE_AGENTS.contains(&agent_key) || HYBRID_AGENTS.contains(&agent_key) {
            return HookMode::Hybrid;
        }
        return HookMode::Mcp;
    }
    if REPLACE_AGENTS.contains(&agent_key) {
        HookMode::Replace
    } else if HYBRID_AGENTS.contains(&agent_key) {
        HookMode::Hybrid
    } else {
        HookMode::Mcp
    }
}

/// True when the user explicitly opted out of deny-list injection.
fn is_deny_suppressed() -> bool {
    if std::env::var("LEAN_CTX_DISABLED").is_ok() {
        return true;
    }
    if matches!(std::env::var("LEAN_CTX_SHADOW_MODE"), Ok(v) if v.trim() == "false" || v.trim() == "0")
    {
        return true;
    }
    if matches!(std::env::var("LEAN_CTX_HEAL"), Ok(v) if v.trim().eq_ignore_ascii_case("off") || v.trim() == "0")
    {
        return true;
    }
    let cfg = crate::core::config::Config::load();
    !cfg.shadow_mode
}
use agents::{
    install_amp_hook, install_antigravity_cli_hook, install_antigravity_hook,
    install_claude_hook_config, install_claude_hook_scripts, install_claude_hook_with_mode,
    install_claude_permissions_deny_replace, install_claude_project_hooks, install_cline_rules,
    install_codebuddy_hook_config, install_codebuddy_hook_scripts,
    install_codebuddy_hook_with_mode, install_codebuddy_permissions_deny_replace,
    install_codebuddy_project_hooks, install_codex_hook, install_copilot_hook,
    install_crush_hook_with_mode, install_cursor_deny_hook, install_cursor_hook_config,
    install_cursor_hook_scripts, install_cursor_hook_with_mode, install_gemini_deny_hook,
    install_gemini_hook, install_gemini_hook_config, install_gemini_hook_scripts, install_grok_mcp,
    install_hermes_hook_with_mode, install_jetbrains_hook, install_kiro_hook,
    install_openclaw_hook, install_opencode_hook_with_mode, install_pi_hook_with_mode,
    install_qoder_hook, install_qoder_hook_with_mode, install_vibe_hook, install_windsurf_hooks,
    install_windsurf_hooks_replace, install_windsurf_rules,
};
use support::{
    ensure_codex_hooks_enabled, install_codex_instruction_docs, install_named_json_server,
    upsert_lean_ctx_codex_hook_entries,
};

fn mcp_server_quiet_mode() -> bool {
    std::env::var_os("LEAN_CTX_MCP_SERVER").is_some()
        || matches!(std::env::var("LEAN_CTX_QUIET"), Ok(value) if value.trim() == "1")
}

/// Agents whose global shell-hook artifacts embed the binary path / command
/// and therefore must be re-rendered after an update or on MCP server start so
/// they always point at the current binary. Each entry is gated on a detection
/// marker (see `hooks_installed_for`) so we never install hooks for an agent
/// the user never configured. The `refresh_covers_every_hybrid_agent` test
/// proves this list plus `REFRESH_EXEMPT_HYBRID_AGENTS` accounts for every
/// Hybrid agent, so a newly added agent can never silently regress.
const REFRESHABLE_HOOK_AGENTS: &[&str] = &[
    "claude", "cursor", "gemini", "codex", "windsurf", "copilot", "qoder",
];

/// Hybrid agents intentionally NOT auto-refreshed, with the reason each is safe
/// to skip. Refresh runs silently (including on every MCP server start), so it
/// must never spawn subprocesses or write project/cwd-relative files. Used by
/// the coverage test to prove every Hybrid agent has an explicit decision.
#[cfg(test)]
const REFRESH_EXEMPT_HYBRID_AGENTS: &[&str] = &[
    // Alias of `claude` — same global files, already refreshed via "claude".
    "claude-code",
    // Installer shells out to `pi install` (subprocess) — unsafe on every start.
    "pi",
    // Write project/cwd-relative rules (.clinerules, .kiro/steering) — a silent
    // server-start refresh must not create files in the user's working dir.
    "cline",
    "roo",
    "kiro",
    // MCP-config / rules wiring only (no global binary-embedding shell-hook
    // script to keep current); refreshed by `setup --fix`, not on start.
    "antigravity",
    "antigravity-cli",
    "amp",
    "crush",
    "hermes",
    "opencode",
    "openclaw",
    "qwen",
    "trae",
    "amazonq",
    "verdent",
];

/// Silently refresh all hook scripts for agents that are already configured.
/// Called after updates and on MCP server start to ensure hooks match the
/// current binary version. Registry-driven: every Hybrid agent with a global
/// shell hook is covered (the rest are explicitly exempted, enforced by test).
pub fn refresh_installed_hooks() {
    let Some(home) = crate::core::home::resolve_home_dir() else {
        return;
    };
    for agent in REFRESHABLE_HOOK_AGENTS {
        if hooks_installed_for(agent, &home) {
            refresh_agent_hooks(agent, &home);
        }
    }
}

/// True when `agent` already has lean-ctx hook artifacts on disk (global only).
fn hooks_installed_for(agent: &str, home: &std::path::Path) -> bool {
    match agent {
        "claude" => {
            let dir = crate::setup::claude_config_dir(home);
            dir.join("hooks/lean-ctx-rewrite.sh").exists()
                || file_contains_lean_ctx(&dir.join("settings.json"))
        }
        "codebuddy" => {
            let dir = crate::core::editor_registry::codebuddy_state_dir(home);
            dir.join("hooks/lean-ctx-rewrite.sh").exists()
                || file_contains_lean_ctx(&dir.join("settings.json"))
        }
        "cursor" => {
            home.join(".cursor/hooks/lean-ctx-rewrite.sh").exists()
                || file_contains_lean_ctx(&home.join(".cursor/hooks.json"))
        }
        "gemini" => {
            home.join(".gemini/hooks/lean-ctx-rewrite-gemini.sh")
                .exists()
                || home.join(".gemini/hooks/lean-ctx-hook-gemini.sh").exists()
        }
        "codex" => {
            let dir = crate::core::home::resolve_codex_dir().unwrap_or_else(|| home.join(".codex"));
            dir.join("hooks/lean-ctx-rewrite-codex.sh").exists()
                || file_contains_lean_ctx(&dir.join("hooks.json"))
        }
        "windsurf" => file_contains_lean_ctx(&home.join(".codeium/windsurf/hooks.json")),
        "copilot" => {
            // User-level Copilot hooks live under ~/.copilot/hooks (#381);
            // ~/.github/hooks is the pre-#381 legacy location.
            file_contains_lean_ctx(&home.join(".copilot/hooks/hooks.json"))
                || file_contains_lean_ctx(&home.join(".github/hooks/hooks.json"))
        }
        "qoder" => file_contains_lean_ctx(&home.join(".qoder/settings.json")),
        _ => false,
    }
}

/// Re-render the hook artifacts for an already-configured agent. Only calls
/// narrow, subprocess-free, global installers (never the full agent setup).
/// Mode-aware: preserves Replace-mode deny artifacts (permissions.deny, deny
/// hooks) so an MCP server restart never downgrades Replace → Hybrid.
fn refresh_agent_hooks(agent: &str, home: &std::path::Path) {
    let mode = recommend_hook_mode(agent);
    match agent {
        "claude" => {
            install_claude_hook_scripts(home);
            install_claude_hook_config(home);
            if mode == HookMode::Replace {
                install_claude_permissions_deny_replace(home);
            }
        }
        "codebuddy" => {
            install_codebuddy_hook_scripts(home);
            install_codebuddy_hook_config(home);
            if mode == HookMode::Replace {
                install_codebuddy_permissions_deny_replace(home);
            }
        }
        "cursor" => {
            install_cursor_hook_scripts(home);
            install_cursor_hook_config(home);
            if mode == HookMode::Replace {
                install_cursor_deny_hook(true);
            }
        }
        "gemini" => {
            install_gemini_hook_scripts(home);
            install_gemini_hook_config(home);
            if mode == HookMode::Replace {
                install_gemini_deny_hook(home);
            }
        }
        "codex" => install_codex_hook(),
        "windsurf" => {
            if mode == HookMode::Replace {
                install_windsurf_hooks_replace(home);
            } else {
                install_windsurf_hooks(home);
            }
        }
        "copilot" => install_copilot_hook(true),
        "qoder" => install_qoder_hook(),
        _ => {}
    }
}

fn file_contains_lean_ctx(path: &std::path::Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|c| c.contains("lean-ctx"))
}

/// Resolve the lean-ctx binary to an **absolute** path for generated hook
/// commands and MCP server entries.
///
/// Agent hooks (Codex, Cursor, Claude, Gemini, Antigravity, …) are executed by
/// the host under a plain non-login shell (`sh -c …`) whose `PATH` is not
/// guaranteed to contain the install dir (e.g. `/usr/local/bin`). A bare
/// `lean-ctx` therefore fails with exit code 127 (#367). Always emitting the
/// resolved absolute path makes hook execution deterministic and matches what
/// MCP setup (`setup/mcp.rs`) and `doctor` already do. Existing configs with a
/// bare command are rewritten on the next `lean-ctx init` / `doctor` run.
///
/// Kept strictly absolute — also used for MCP server `command` fields, which
/// hosts spawn **directly** (no shell), so `$HOME/...` forms would break
/// there. Shell-executed hook commands go through
/// [`resolve_hook_command_binary`], which honors the portable override (#708).
fn resolve_binary_path() -> String {
    crate::core::portable_binary::resolve_portable_binary()
}

/// Binary token for **shell-executed** hook commands (`<binary> hook rewrite`
/// in Claude/Cursor/Gemini/… hook configs and generated `#!/bin/sh` scripts).
///
/// Portable override (#708): `LEAN_CTX_HOOK_BINARY` env, then config
/// `hook_binary`, is emitted **verbatim** — for settings files synced across
/// machines with different usernames (`$HOME/.local/bin/lean-ctx`). Hook
/// hosts run these commands through a shell, so the variable expands at
/// execution time; `doctor` accepts the override as current, so
/// `init`/`--fix`/`update` stop rewriting synced files. MCP registrations
/// and autostart units keep the absolute path (no shell there).
fn resolve_hook_command_binary() -> String {
    if let Some(portable) = crate::core::portable_binary::hook_binary_override() {
        return portable;
    }
    resolve_binary_path()
}

fn resolve_binary_path_for_bash() -> String {
    if let Some(portable) = crate::core::portable_binary::hook_binary_override() {
        return portable;
    }
    to_bash_compatible_path(&resolve_binary_path())
}

/// Shell-quotes a binary token for generated `#!/bin/sh` wrappers and
/// `LEAN_CTX_BIN=` assignments (#719). Double quotes keep `$HOME`-style
/// portable overrides (#708) expanding at execution time while paths with
/// spaces survive word splitting (npm installs under
/// `C:\Users\First Last\AppData\…`). `"` and `` ` `` are escaped; `$` stays
/// active on purpose — portable forms rely on it.
pub(crate) fn shell_quoted_binary(binary: &str) -> String {
    let escaped = binary.replace('"', "\\\"").replace('`', "\\`");
    format!("\"{escaped}\"")
}

/// #719: true when an existing generated wrapper references a portable binary
/// form (`$HOME/…`, `${HOME}/…`, `%USERPROFILE%\…`) that resolves to an
/// existing binary on THIS machine. Such a wrapper is healthy and must not be
/// re-stamped with a machine-absolute path: on multi-machine synced setups
/// (Dropbox'd `~/.claude`, different usernames) a heal on the machine WITHOUT
/// the portable override would otherwise bake its absolute path into the
/// wrapper, and every new session on the peer machine dies mid tool call with
/// no surfaced error.
fn wrapper_is_portable_and_working(path: &std::path::Path, home: &std::path::Path) -> bool {
    std::fs::read_to_string(path)
        .is_ok_and(|content| wrapper_content_is_portable_and_working(&content, home))
}

pub(crate) fn wrapper_content_is_portable_and_working(
    content: &str,
    home: &std::path::Path,
) -> bool {
    let Some(token) = wrapper_binary_token(content) else {
        return false;
    };
    if !(token.contains("$HOME") || token.contains("${HOME}") || token.contains("%USERPROFILE%")) {
        return false;
    }
    let home_s = home.to_string_lossy();
    let expanded = token
        .replace("${HOME}", &home_s)
        .replace("$HOME", &home_s)
        .replace("%USERPROFILE%", &home_s);
    std::path::Path::new(&from_bash_to_native_path(&expanded)).exists()
}

/// Extracts the binary token from a generated wrapper script: the
/// `LEAN_CTX_BIN=` assignment (rewrite scripts) or the `exec <binary> hook …`
/// line (native wrappers) — quoted or bare.
pub(crate) fn wrapper_binary_token(content: &str) -> Option<String> {
    for line in content.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("LEAN_CTX_BIN=") {
            let rest = rest.trim();
            let tok = rest
                .strip_prefix('"')
                .map_or(rest, |r| r.split('"').next().unwrap_or_default());
            if !tok.is_empty() {
                return Some(tok.to_string());
            }
        }
        if let Some(rest) = t.strip_prefix("exec ") {
            let rest = rest.trim();
            let tok = match rest.strip_prefix('"') {
                Some(r) => r.split('"').next().unwrap_or_default().to_string(),
                None => rest
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_string(),
            };
            if !tok.is_empty() {
                return Some(tok);
            }
        }
    }
    None
}

/// Writes a generated hook wrapper unless the portable override is unset AND
/// the existing file already carries a working portable reference (#719) —
/// healing must never replace a synced portable wrapper with a
/// machine-absolute path.
fn write_wrapper_file(path: &std::path::Path, content: &str, home: &std::path::Path) {
    if crate::core::portable_binary::hook_binary_override().is_none()
        && wrapper_is_portable_and_working(path, home)
    {
        return;
    }
    write_file(path, content);
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

/// Convert a Unix/MSYS-style path (`/c/Users/...`) back to native Windows
/// format (`C:/Users/...`). No-op for paths that don't match the pattern.
pub fn from_bash_to_native_path(path: &str) -> String {
    crate::core::pathutil::normalize_tool_path(path)
}

/// Normalize paths from any client format to a consistent OS-native form.
/// Delegates to `core::pathutil` so `core` crates do not depend on `hooks`.
pub fn normalize_tool_path(path: &str) -> String {
    crate::core::pathutil::normalize_tool_path(path)
}

pub fn generate_rewrite_script(binary: &str) -> String {
    let case_pattern = crate::rewrite_registry::bash_case_pattern();
    // #719: assignment + rewritten command carry the binary quoted, so
    // portable `$HOME/…` overrides expand at exec time and paths with spaces
    // survive word splitting.
    let quoted_binary = shell_quoted_binary(binary);
    format!(
        r#"#!/usr/bin/env bash
# lean-ctx PreToolUse hook — rewrites bash commands to lean-ctx equivalents
set -euo pipefail

LEAN_CTX_BIN={quoted_binary}

INPUT=$(cat)
TOOL=$(echo "$INPUT" | grep -oE '"tool_name":"([^"\\]|\\.)*"' | head -1 | sed 's/^"tool_name":"//;s/"$//' | sed 's/\\"/"/g;s/\\\\/\\/g')

case "$TOOL" in
  Bash|bash|PowerShell|powershell) ;;
  *) exit 0 ;;
esac

CMD=$(echo "$INPUT" | grep -oE '"command":"([^"\\]|\\.)*"' | head -1 | sed 's/^"command":"//;s/"$//' | sed 's/\\"/"/g;s/\\\\/\\/g')

if [ -z "$CMD" ] || echo "$CMD" | grep -qE "^(lean-ctx |\"?$LEAN_CTX_BIN\"? )"; then
  exit 0
fi

# Skip multi-line commands: the grep/sed extraction above does not decode
# JSON \n into real newlines, so lean-ctx -c would receive fused lines (#787).
if printf '%s' "$CMD" | grep -qF '\n'; then exit 0; fi

case "$CMD" in
  {case_pattern})
    # Shell-escape then JSON-escape (two passes)
    SHELL_ESC=$(printf '%s' "$CMD" | sed 's/\\/\\\\/g;s/"/\\"/g')
    REWRITE="\"$LEAN_CTX_BIN\" -c \"$SHELL_ESC\""
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
    let quoted_binary = shell_quoted_binary(binary);
    format!(
        r#"#!/usr/bin/env bash
# lean-ctx hook — rewrites shell commands
set -euo pipefail
LEAN_CTX_BIN={quoted_binary}
INPUT=$(cat)
CMD=$(echo "$INPUT" | grep -oE '"command":"([^"\\]|\\.)*"' | head -1 | sed 's/^"command":"//;s/"$//' | sed 's/\\"/"/g;s/\\\\/\\/g' 2>/dev/null || echo "")
if [ -z "$CMD" ] || echo "$CMD" | grep -qE "^(lean-ctx |\"?$LEAN_CTX_BIN\"? )"; then exit 0; fi
if printf '%s' "$CMD" | grep -qF '\n'; then exit 0; fi
case "$CMD" in
  {case_pattern})
    SHELL_ESC=$(printf '%s' "$CMD" | sed 's/\\/\\\\/g;s/"/\\"/g')
    REWRITE="\"$LEAN_CTX_BIN\" -c \"$SHELL_ESC\""
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

pub fn hybrid_rules_content() -> String {
    use crate::core::rules_canonical;
    format!(
        "{start}\n<!-- version: {version} -->\n\n\
# lean-ctx \u{2014} Hybrid Mode (MCP reads + CLI commands)\n\n\
{bullets}\n\n\
{never}\n\n\
{end}",
        start = rules_canonical::START_MARK,
        version = rules_canonical::RULES_VERSION,
        bullets = rules_canonical::BULLETS,
        never = rules_canonical::NEVER,
        end = rules_canonical::END_MARK,
    )
}

pub fn replace_rules_content() -> String {
    use crate::core::rules_canonical;
    format!(
        "{start}\n<!-- version: {version} -->\n\n\
# lean-ctx \u{2014} Replace Mode (native tools denied)\n\n\
Native Read/Grep/Glob/Bash are denied by policy. Use ONLY ctx_* MCP tools:\n\
- ctx_read for ALL file reads (cached, 10 modes, re-reads ~13 tokens)\n\
- ctx_shell for ALL shell commands (95+ compression patterns)\n\
- ctx_search instead of Grep/rg (compact results)\n\
- ctx_tree instead of ls/find (compact directory maps)\n\
- ctx_glob instead of Glob (file pattern matching)\n\n\
Do NOT attempt native Read, Grep, Glob, or Bash \u{2014} they will be denied.\n\n\
{end}",
        start = rules_canonical::START_MARK,
        version = rules_canonical::RULES_VERSION,
        end = rules_canonical::END_MARK,
    )
}

pub fn install_project_rules() {
    install_project_rules_for_agents(&[]);
}

/// Install project rules, optionally scoped to specific agents.
/// If `agents` is empty, installs for all agents (legacy behavior).
pub fn install_project_rules_for_agents(agents: &[&str]) {
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

    let home = crate::core::home::resolve_home_dir().unwrap_or_default();
    if cwd == home {
        eprintln!(
            "  Skipping project files: current directory is your home folder.\n  \
             Run this command from a project directory instead."
        );
        return;
    }

    let all = agents.is_empty();
    let wants = |name: &str| all || agents.iter().any(|a| a.eq_ignore_ascii_case(name));

    ensure_project_agents_integration(&cwd);

    if wants("cursor") || wants("windsurf") {
        let cursorrules = cwd.join(".cursorrules");
        if !cursorrules.exists()
            || !std::fs::read_to_string(&cursorrules)
                .unwrap_or_default()
                .contains("lean-ctx")
        {
            let content = cursorrules_content();
            if cursorrules.exists() {
                let mut existing = std::fs::read_to_string(&cursorrules).unwrap_or_default();
                if !existing.ends_with('\n') {
                    existing.push('\n');
                }
                existing.push('\n');
                existing.push_str(&content);
                write_file(&cursorrules, &existing);
            } else {
                write_file(&cursorrules, &content);
            }
            if !mcp_server_quiet_mode() {
                eprintln!("Created/updated .cursorrules in project root.");
            }
        }
    }

    if wants("claude") {
        // GL #555: project rules files without `paths:` frontmatter load
        // unconditionally every session and stacked on top of the global
        // CLAUDE.md block (12k+ token memory footprints in the field). The
        // AGENTS.md block + on-demand skill carry the same guidance, so the
        // lean-ctx-owned copy is removed instead of refreshed.
        let claude_rules_file = cwd.join(".claude").join("rules").join("lean-ctx.md");
        if let Ok(existing) = std::fs::read_to_string(&claude_rules_file)
            && existing.contains(crate::core::rules_canonical::RULES_MARKER_PREFIX)
            && std::fs::remove_file(&claude_rules_file).is_ok()
            && !mcp_server_quiet_mode()
        {
            eprintln!(
                "Removed .claude/rules/lean-ctx.md (always-loaded duplicate; AGENTS.md block + skill replace it)."
            );
        }

        install_claude_project_hooks(&cwd);
    }

    if wants("codebuddy") {
        let codebuddy_rules_file = cwd.join(".codebuddy").join("rules").join("lean-ctx.md");
        if let Ok(existing) = std::fs::read_to_string(&codebuddy_rules_file)
            && existing.contains(crate::core::rules_canonical::RULES_MARKER_PREFIX)
            && std::fs::remove_file(&codebuddy_rules_file).is_ok()
            && !mcp_server_quiet_mode()
        {
            eprintln!(
                "Removed .codebuddy/rules/lean-ctx.md (always-loaded duplicate; CODEBUDDY.md block + skill replace it)."
            );
        }

        install_codebuddy_project_hooks(&cwd);
    }

    if wants("kiro") {
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
                write_file(&steering_file, &kiro_steering_content());
                if !mcp_server_quiet_mode() {
                    eprintln!("Created .kiro/steering/lean-ctx.md (Kiro steering).");
                }
            }
        }
    }

    if wants("copilot") || wants("vscode") {
        ensure_copilot_instructions(&cwd);
        ensure_vscode_instruction_files_setting(&cwd);
    }
}

const PROJECT_LEAN_CTX_MD_MARKER: &str =
    crate::core::rules_canonical::PROJECT_LEAN_CTX_OWNED_MARKER;
const PROJECT_LEAN_CTX_MD: &str = "LEAN-CTX.md";
const PROJECT_AGENTS_MD: &str = "AGENTS.md";
// The AGENTS.md pointer block keeps its own marker pair, independent of the
// dedicated rules-file `START_MARK`: pointer-only files must not be counted as
// duplicate lean-ctx sources (doctor overhead, #684).
const AGENTS_BLOCK_START: &str = crate::core::rules_canonical::AGENTS_BLOCK_START;
const AGENTS_BLOCK_END: &str = crate::core::rules_canonical::AGENTS_BLOCK_END;

fn ensure_project_agents_integration(cwd: &std::path::Path) {
    let lean_ctx_md = cwd.join(PROJECT_LEAN_CTX_MD);
    // Longform (#578): LEAN-CTX.md is opened on demand via the AGENTS.md
    // pointer, never auto-loaded, so it carries the verbose teaching profile.
    let desired = format!(
        "{PROJECT_LEAN_CTX_MD_MARKER}\n{}\n",
        crate::rules_inject::rules_longform_markdown()
    );

    if !lean_ctx_md.exists() {
        write_file(&lean_ctx_md, &desired);
    } else if std::fs::read_to_string(&lean_ctx_md)
        .unwrap_or_default()
        .contains(PROJECT_LEAN_CTX_MD_MARKER)
    {
        let current = std::fs::read_to_string(&lean_ctx_md).unwrap_or_default();
        let version_str = format!(
            "<!-- version: {} -->",
            crate::core::rules_canonical::RULES_VERSION
        );
        if !current.contains(&version_str) {
            write_file(&lean_ctx_md, &desired);
        }
    }

    // No `@` import: Claude Code expands `@file` references inline at session
    // start, so pointing at LEAN-CTX.md re-loaded the full ruleset into every
    // session on top of this block (GL #555). The block is self-contained;
    // the full ruleset stays in LEAN-CTX.md for on-demand reading.
    let block = format!(
        "{AGENTS_BLOCK_START}\n\
## lean-ctx\n\n\
lean-ctx is active — the MCP tools replace native equivalents.\n\
Full rules: {PROJECT_LEAN_CTX_MD} (open on demand — do not auto-load).\n\
{AGENTS_BLOCK_END}\n"
    );

    let agents_md = cwd.join(PROJECT_AGENTS_MD);
    if !agents_md.exists() {
        let content = format!("# Agent Instructions\n\n{block}");
        write_file(&agents_md, &content);
        if !mcp_server_quiet_mode() {
            eprintln!("Created AGENTS.md in project root (lean-ctx reference only).");
        }
        return;
    }

    let existing = std::fs::read_to_string(&agents_md).unwrap_or_default();

    // Marker checks are line-based (GL #1158): a prose mention of the marker
    // (as this repo's own AGENTS.md carries) must not trigger block surgery.
    let has_block = crate::marked_block::contains_marker_line(&existing, AGENTS_BLOCK_START);

    if existing.contains("CLI-first Token Optimization for Pi") && !has_block {
        let content = format!("# Agent Instructions\n\n{block}");
        write_file(&agents_md, &content);
        return;
    }

    if has_block {
        let updated = crate::marked_block::replace_marked_block(
            &existing,
            AGENTS_BLOCK_START,
            AGENTS_BLOCK_END,
            &block,
        );
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
    if !mcp_server_quiet_mode() {
        eprintln!("Updated AGENTS.md (added lean-ctx reference block).");
    }
}

/// #555: VS Code Copilot Chat auto-applies `.github/copilot-instructions.md` to
/// every request, but `init --agent copilot` previously wrote only a weak
/// AGENTS.md pointer — Claude-family models then ignored the lean-ctx tool
/// mapping while GPT-5.x mostly followed it. Write the strong dedicated ruleset
/// into a `<!-- lean-ctx-rules -->` marked block so it merges idempotently and
/// never clobbers the user's own instructions.
fn ensure_copilot_instructions(cwd: &std::path::Path) {
    let path = cwd.join(".github").join("copilot-instructions.md");
    let block = crate::rules_inject::rules_dedicated_markdown();
    let start = crate::core::rules_canonical::START_MARK;
    let end = crate::core::rules_canonical::END_MARK;
    let owned = format!("{}\n", block.trim_end());

    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let desired = if existing.trim().is_empty() {
        owned
    } else if existing.contains(start) {
        // Refresh our block; keep any surrounding user-authored content.
        let user = crate::marked_block::remove_content(&existing, start, end);
        if user.trim().is_empty() {
            owned
        } else {
            format!("{}\n\n{}\n", user.trim_end(), block.trim_end())
        }
    } else {
        // User-authored file with no lean-ctx block yet: append ours once.
        format!("{}\n\n{}\n", existing.trim_end(), block.trim_end())
    };

    if desired == existing {
        return;
    }
    if let Some(parent) = path.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return;
    }
    write_file(&path, &desired);
    if !mcp_server_quiet_mode() {
        eprintln!("Created/updated .github/copilot-instructions.md (Copilot/VS Code rules).");
    }
}

/// #555 safety net: VS Code applies instruction files when
/// `github.copilot.chat.codeGeneration.useInstructionFiles` is on (the default).
/// A user or org policy may have disabled it globally, so pin it on for this
/// project. Set only when the key is absent — an explicit user value is honoured.
fn ensure_vscode_instruction_files_setting(cwd: &std::path::Path) {
    const KEY: &str = "github.copilot.chat.codeGeneration.useInstructionFiles";
    let path = cwd.join(".vscode").join("settings.json");

    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut json = if existing.trim().is_empty() {
        serde_json::json!({})
    } else {
        match crate::core::jsonc::parse_jsonc(&existing) {
            Ok(v) if v.is_object() => v,
            // Never clobber an unparseable or non-object settings file.
            _ => return,
        }
    };
    let Some(obj) = json.as_object_mut() else {
        return;
    };
    if obj.contains_key(KEY) {
        return;
    }
    obj.insert(KEY.to_string(), serde_json::Value::Bool(true));

    if let Some(parent) = path.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return;
    }
    let Ok(formatted) = serde_json::to_string_pretty(&json) else {
        return;
    };
    if crate::config_io::write_atomic_with_backup(&path, &formatted).is_ok()
        && !mcp_server_quiet_mode()
    {
        eprintln!("Set {KEY} in .vscode/settings.json.");
    }
}

/// Compact pointer only (#578): Cursor already auto-loads the canonical full
/// ruleset from `~/.cursor/rules/lean-ctx.mdc`, so a project `.cursorrules`
/// that repeats it bills the same guidance twice in every session.
pub fn cursorrules_content() -> String {
    let start = crate::core::rules_canonical::START_MARK;
    let end = crate::core::rules_canonical::END_MARK;
    let version = crate::core::rules_canonical::RULES_VERSION;
    format!(
        "{start}\n<!-- version: {version} -->\n\n\
# lean-ctx\n\n\
{bullets}\n\n\
{never}\n\
Full rules: ~/.cursor/rules/lean-ctx.mdc (auto-loaded) \u{2014} do not duplicate here.\n\
{end}",
        bullets = crate::core::rules_canonical::BULLETS,
        never = crate::core::rules_canonical::NEVER,
    )
}

pub fn kiro_steering_content() -> String {
    use crate::core::rules_canonical;
    format!(
        "---\n\
inclusion: always\n\
---\n\n\
# Context Engineering Layer\n\n\
{start}\n\
<!-- version: {version} -->\n\n\
The workspace has the `lean-ctx` MCP server installed. \
You MUST prefer lean-ctx tools over native equivalents for token efficiency and caching.\n\n\
{bullets}\n\n\
{never}\n\n\
## When to use native Kiro tools instead\n\n\
- `fsWrite` / `fsAppend` \u{2014} always use native (lean-ctx doesn't write files)\n\
- `strReplace` \u{2014} always use native (precise string replacement)\n\
- `semanticRename` / `smartRelocate` \u{2014} always use native (IDE integration)\n\
- `getDiagnostics` \u{2014} always use native (language server diagnostics)\n\
- `deleteFile` \u{2014} always use native\n\
- Glob \u{2014} always use native glob\n\n\
{end}",
        start = rules_canonical::START_MARK,
        version = rules_canonical::RULES_VERSION,
        bullets = rules_canonical::BULLETS,
        never = rules_canonical::NEVER,
        end = rules_canonical::END_MARK,
    )
}
/// #281: whether the hooks layer may register the lean-ctx MCP server in an
/// agent's config. Honors `[setup] auto_update_mcp`. Hooks, rules and skills
/// still install when this is `false` — only the MCP-server writes are gated, so
/// MCP-disabled environments stay free of MCP entries. Centralised here so every
/// per-agent writer shares one source of truth (the shared JSON writer in
/// `support.rs` enforces the same gate for `mcpServers`-style agents).
pub(crate) fn should_register_mcp() -> bool {
    crate::core::config::Config::load()
        .setup
        .should_update_mcp()
}

pub fn install_agent_hook(agent: &str, global: bool) {
    install_agent_hook_with_mode(agent, global, HookMode::Mcp);
}

pub fn install_agent_hook_with_mode(agent: &str, global: bool, mode: HookMode) {
    let home = crate::core::home::resolve_home_dir().unwrap_or_default();
    match agent {
        "claude" | "claude-code" => install_claude_hook_with_mode(global, mode),
        "codebuddy" => install_codebuddy_hook_with_mode(global, mode),
        "cursor" => install_cursor_hook_with_mode(global, mode),
        "gemini" => {
            install_gemini_hook();
            if mode == HookMode::Replace {
                install_gemini_deny_hook(&home);
            }
            // Google is transitioning Gemini CLI → Antigravity CLI (`agy`), and
            // `gemini` setup also configures the Antigravity CLI MCP target. The
            // hooks must follow: `agy` reads hooks only from its plugin dir
            // (`~/.gemini/config/plugins/lean-ctx`), never from the legacy
            // `~/.gemini/settings.json`, so install the plugin too (#284).
            install_antigravity_cli_hook();
        }
        "grok" | "grok-build" => install_grok_mcp(),
        "antigravity" => install_antigravity_hook(),
        "antigravity-cli" => install_antigravity_cli_hook(),
        "augment" => install_mcp_json_agent(
            "Augment CLI",
            "~/.augment/settings.json",
            &crate::core::editor_registry::augment_cli_settings_path(&home),
        ),
        "codex" => {
            install_codex_hook();
        }
        "windsurf" => {
            install_windsurf_rules(global);
            if mode == HookMode::Replace {
                install_windsurf_hooks_replace(&home);
            }
        }
        "cline" | "roo" => install_cline_rules(global),
        "copilot" | "vscode" => install_copilot_hook(global),
        // VS Code Insiders needs no hook install of its own: the MCP entry in
        // its separate `Code - Insiders/User/mcp.json` is written by the
        // editor-registry writer (GH #694), and the Copilot hook layer is
        // user-global (`~/.copilot`), already covered by copilot/vscode.
        "vscode-insiders" => {}
        "pi" => install_pi_hook_with_mode(global, mode),
        "qoder" => install_qoder_hook_with_mode(mode),
        "qoderwork" => install_mcp_json_agent(
            "QoderWork",
            "~/.qoderwork/mcp.json",
            &home.join(".qoderwork/mcp.json"),
        ),
        "qwen" => install_mcp_json_agent(
            "Qwen Code",
            "~/.qwen/settings.json",
            &home.join(".qwen/settings.json"),
        ),
        "trae" => install_mcp_json_agent("Trae", "~/.trae/mcp.json", &home.join(".trae/mcp.json")),
        "amazonq" => install_mcp_json_agent(
            "Amazon Q Developer",
            "~/.aws/amazonq/default.json",
            &home.join(".aws/amazonq/default.json"),
        ),
        "jetbrains" => install_jetbrains_hook(),
        "kiro" => install_kiro_hook(),
        "verdent" => install_mcp_json_agent(
            "Verdent",
            "~/.verdent/mcp.json",
            &home.join(".verdent/mcp.json"),
        ),
        "opencode" => install_opencode_hook_with_mode(mode),
        "amp" => install_amp_hook(),
        "crush" => install_crush_hook_with_mode(mode),
        "openclaw" => install_openclaw_hook(),
        "hermes" => install_hermes_hook_with_mode(global, mode),
        "vibe" => install_vibe_hook(),
        "zed" => {
            let zed_path = crate::core::editor_registry::zed_settings_path(&home);
            let binary = resolve_binary_path();
            let entry = full_server_entry(&binary);
            install_named_json_server("Zed", "settings.json", &zed_path, "context_servers", entry);
        }
        "aider" => {
            install_mcp_json_agent("Aider", "~/.aider/mcp.json", &home.join(".aider/mcp.json"));
        }
        "continue" => install_mcp_json_agent(
            "Continue",
            "~/.continue/mcp.json",
            &home.join(".continue/mcp.json"),
        ),
        "neovim" => install_mcp_json_agent(
            "Neovim (mcphub.nvim)",
            "~/.config/mcphub/servers.json",
            &home.join(".config/mcphub/servers.json"),
        ),
        "emacs" => install_mcp_json_agent(
            "Emacs (mcp.el)",
            "~/.emacs.d/mcp.json",
            &home.join(".emacs.d/mcp.json"),
        ),
        "sublime" => install_mcp_json_agent(
            "Sublime Text",
            "~/.config/sublime-text/mcp.json",
            &home.join(".config/sublime-text/mcp.json"),
        ),
        _ => {
            eprintln!("Unknown agent: {agent}");
            eprintln!("  Supported: aider, amazonq, amp, antigravity, antigravity-cli, augment,");
            eprintln!(
                "    claude, cline, codebuddy, codex, continue, copilot, crush, cursor, emacs, gemini, grok,"
            );
            eprintln!(
                "    grok-build, hermes, jetbrains, kiro, neovim, openclaw, opencode, pi, qoder,"
            );
            eprintln!(
                "    qoderwork, qwen, roo, sublime, trae, verdent, vibe, vscode, windsurf, zed"
            );
            std::process::exit(1);
        }
    }
}

pub fn install_agent_project_hooks(agent: &str, cwd: &std::path::Path) {
    match agent {
        "claude" | "claude-code" => agents::install_claude_project_hooks(cwd),
        "codebuddy" => agents::install_codebuddy_project_hooks(cwd),
        _ => {}
    }
}

fn write_file(path: &std::path::Path, content: &str) {
    // Skip identical rewrites: re-running setup/init must not churn mtimes or
    // leave .bak files behind for content that did not change (GL #558).
    if std::fs::read_to_string(path).is_ok_and(|existing| existing == content) {
        return;
    }
    if let Err(e) = crate::config_io::write_atomic_with_backup(path, content) {
        tracing::error!("Error writing {}: {e}", path.display());
    }
}

/// Create a setup directory, surfacing a clear error instead of silently
/// swallowing it (#596).
///
/// A user may symlink `~/.claude` / `~/.codex` (or a child) into a dotfiles
/// repo; [`crate::config_io::ensure_dir`] follows such a symlink to its real
/// in-`$HOME` target and tolerates a dangling one. Returns `false` (after
/// printing the reason) when the directory cannot be prepared, so the caller can
/// skip the now-impossible writes rather than failing confusingly downstream.
fn ensure_state_dir(dir: &std::path::Path) -> bool {
    match crate::config_io::ensure_dir(dir) {
        Ok(()) => true,
        Err(e) => {
            // Always surface — a swallowed dir failure was the #596 footgun.
            eprintln!("lean-ctx setup: cannot prepare {}: {e}", dir.display());
            false
        }
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

/// Env key/value pairs for the lean-ctx MCP server entry written into agent
/// configs (Codex TOML + the JSON agents).
///
/// Deliberately does NOT pin `LEAN_CTX_DATA_DIR`: lean-ctx auto-detects its
/// per-category dirs (config/data/state/cache) at runtime, and pinning the data
/// dir would set that var in the server's environment, forcing single-dir mode
/// and collapsing config/state/cache onto the data dir — defeating the XDG split
/// (GH #408). Emits `LEAN_CTX_PROJECT_ROOT` and `LEAN_CTX_EXTRA_ROOTS` when known
/// (process env first, then config). Without these, a long-lived MCP server
/// spawned by the agent loses the project / worktree scope captured at `init`,
/// so an explicit path under a sibling worktree is wrongly rejected as a jail
/// escape (#403). Single source of truth so every agent installer stays consistent.
pub(crate) fn mcp_server_env_pairs() -> Vec<(String, String)> {
    let mut pairs = Vec::new();

    let cfg = crate::core::config::Config::load();

    let project_root = std::env::var("LEAN_CTX_PROJECT_ROOT")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| cfg.project_root.clone().filter(|v| !v.trim().is_empty()));
    if let Some(root) = project_root {
        pairs.push(("LEAN_CTX_PROJECT_ROOT".to_string(), root));
    }

    // Env override is already a platform path-list; config is a Vec we join the
    // same way `LEAN_CTX_EXTRA_ROOTS` is parsed (`std::env::split_paths`).
    let extra_roots = std::env::var("LEAN_CTX_EXTRA_ROOTS")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            let roots: Vec<&str> = cfg
                .extra_roots
                .iter()
                .map(String::as_str)
                .filter(|s| !s.trim().is_empty())
                .collect();
            if roots.is_empty() {
                return None;
            }
            std::env::join_paths(roots)
                .ok()
                .map(|s| s.to_string_lossy().to_string())
        });
    if let Some(extra) = extra_roots {
        pairs.push(("LEAN_CTX_EXTRA_ROOTS".to_string(), extra));
    }

    pairs
}

/// The MCP server env block as a JSON object, for the JSON-config agents.
pub(crate) fn mcp_server_env_json() -> serde_json::Value {
    let map: serde_json::Map<String, serde_json::Value> = mcp_server_env_pairs()
        .into_iter()
        .map(|(k, v)| (k, serde_json::Value::String(v)))
        .collect();
    serde_json::Value::Object(map)
}

fn full_server_entry(binary: &str) -> serde_json::Value {
    // No LEAN_CTX_FULL_TOOLS here: forcing the full toolset (69+ schemas,
    // ~15k tokens of tool definitions resent every turn) made lean-ctx one of
    // the biggest token consumers in users' sessions (GitHub #385). The server
    // defaults to the core toolset + ctx_call/ctx_expand for on-demand access;
    // power users opt in via `tool_profile = "power"` in config.toml.
    serde_json::json!({
        "command": binary,
        "env": mcp_server_env_json()
    })
}

pub(crate) fn install_mcp_json_agent(
    name: &str,
    display_path: &str,
    config_path: &std::path::Path,
) {
    let binary = resolve_binary_path();
    let entry = full_server_entry(&binary);
    install_named_json_server(name, display_path, config_path, "mcpServers", entry);
}

#[cfg(test)]
mod tests;
