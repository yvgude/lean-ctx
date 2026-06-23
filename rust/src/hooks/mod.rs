use std::path::PathBuf;

pub mod agents;
mod support;

/// Controls how hooks instruct agents to access lean-ctx functionality.
///
/// * `Mcp` — MCP server only (extension/plugin-based agents without reliable shell).
/// * `Hybrid` — MCP server + shell hooks for command compression (best of both).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookMode {
    #[default]
    Mcp,
    Hybrid,
}

impl std::fmt::Display for HookMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Mcp => write!(f, "MCP"),
            Self::Hybrid => write!(f, "Hybrid"),
        }
    }
}

impl HookMode {
    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s.to_lowercase().replace('-', "").as_str() {
            "mcp" => Some(Self::Mcp),
            "hybrid" => Some(Self::Hybrid),
            _ => None,
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::Mcp => "MCP server only (extension/plugin-based agents without reliable shell)",
            Self::Hybrid => "MCP server + shell hooks for command compression (best of both)",
        }
    }
}

/// Auto-detect the best hook mode for a given agent key based on its shell capabilities.
///
/// Criteria (verified against provider docs May 2026):
///   Hybrid — MCP server (full Context OS) + shell hooks where available.
///            Read/Search via MCP (reliable, cached). Shell via hooks (zero overhead).
///   Mcp    — agent has no reliable direct shell tool (e.g. IDE plugin only)
/// Agents that get the Hybrid integration (MCP for reads/search + shell hooks
/// or rules for command compression). Kept as a single data list so it is
/// testable and so `refresh_installed_hooks` can prove it covers every one of
/// them (see `refresh_covers_every_hybrid_agent`).
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

pub fn recommend_hook_mode(agent_key: &str) -> HookMode {
    if HYBRID_AGENTS.contains(&agent_key) {
        HookMode::Hybrid
    } else {
        // No reliable direct shell tool → MCP only.
        HookMode::Mcp
    }
}
use agents::{
    install_amp_hook, install_antigravity_cli_hook, install_antigravity_hook,
    install_claude_hook_config, install_claude_hook_scripts, install_claude_hook_with_mode,
    install_claude_project_hooks, install_cline_rules, install_codebuddy_hook_config,
    install_codebuddy_hook_scripts, install_codebuddy_hook_with_mode,
    install_codebuddy_project_hooks, install_codex_hook, install_copilot_hook,
    install_crush_hook_with_mode, install_cursor_hook_config, install_cursor_hook_scripts,
    install_cursor_hook_with_mode, install_gemini_hook, install_gemini_hook_config,
    install_gemini_hook_scripts, install_hermes_hook_with_mode, install_jetbrains_hook,
    install_kiro_hook, install_openclaw_hook, install_opencode_hook_with_mode,
    install_pi_hook_with_mode, install_qoder_hook, install_qoder_hook_with_mode,
    install_windsurf_hooks, install_windsurf_rules,
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
fn refresh_agent_hooks(agent: &str, home: &std::path::Path) {
    match agent {
        "claude" => {
            install_claude_hook_scripts(home);
            install_claude_hook_config(home);
        }
        "codebuddy" => {
            install_codebuddy_hook_scripts(home);
            install_codebuddy_hook_config(home);
        }
        "cursor" => {
            install_cursor_hook_scripts(home);
            install_cursor_hook_config(home);
        }
        "gemini" => {
            install_gemini_hook_scripts(home);
            install_gemini_hook_config(home);
        }
        "codex" => install_codex_hook(),
        "windsurf" => install_windsurf_hooks(home),
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
fn resolve_binary_path() -> String {
    crate::core::portable_binary::resolve_portable_binary()
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
    format!(
        r#"#!/usr/bin/env bash
# lean-ctx PreToolUse hook — rewrites bash commands to lean-ctx equivalents
set -euo pipefail

LEAN_CTX_BIN="{binary}"

INPUT=$(cat)
TOOL=$(echo "$INPUT" | grep -oE '"tool_name":"([^"\\]|\\.)*"' | head -1 | sed 's/^"tool_name":"//;s/"$//' | sed 's/\\"/"/g;s/\\\\/\\/g')

case "$TOOL" in
  Bash|bash|PowerShell|powershell) ;;
  *) exit 0 ;;
esac

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
}

const PROJECT_LEAN_CTX_MD_MARKER: &str = "<!-- lean-ctx-owned: PROJECT-LEAN-CTX.md v1 -->";
const PROJECT_LEAN_CTX_MD: &str = "LEAN-CTX.md";
const PROJECT_AGENTS_MD: &str = "AGENTS.md";
// The AGENTS.md pointer block keeps its own marker pair, independent of the
// dedicated rules-file `START_MARK`: pointer-only files must not be counted as
// duplicate lean-ctx sources (doctor overhead, #684).
const AGENTS_BLOCK_START: &str = crate::core::rules_canonical::AGENTS_BLOCK_START;
const AGENTS_BLOCK_END: &str = crate::core::rules_canonical::AGENTS_BLOCK_END;

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

    if existing.contains("CLI-first Token Optimization for Pi")
        && !existing.contains(AGENTS_BLOCK_START)
    {
        let content = format!("# Agent Instructions\n\n{block}");
        write_file(&agents_md, &content);
        return;
    }

    if existing.contains(AGENTS_BLOCK_START) {
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
            // Google is transitioning Gemini CLI → Antigravity CLI (`agy`), and
            // `gemini` setup also configures the Antigravity CLI MCP target. The
            // hooks must follow: `agy` reads hooks only from its plugin dir
            // (`~/.gemini/config/plugins/lean-ctx`), never from the legacy
            // `~/.gemini/settings.json`, so install the plugin too (#284).
            install_antigravity_cli_hook();
        }
        "antigravity" => install_antigravity_hook(),
        "antigravity-cli" => install_antigravity_cli_hook(),
        "augment" => install_mcp_json_agent(
            "Augment CLI",
            "~/.augment/settings.json",
            &crate::core::editor_registry::augment_cli_settings_path(&home),
        ),
        "codex" => install_codex_hook(),
        "windsurf" => install_windsurf_rules(global),
        "cline" | "roo" => install_cline_rules(global),
        "copilot" | "vscode" => install_copilot_hook(global),
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
                "    claude, cline, codebuddy, codex, continue, copilot, crush, cursor, emacs, gemini,"
            );
            eprintln!("    hermes, jetbrains, kiro, neovim, openclaw, opencode, pi, qoder,");
            eprintln!("    qoderwork, qwen, roo, sublime, trae, verdent, vscode, windsurf, zed");
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
mod tests {
    use super::*;

    #[test]
    fn refresh_covers_every_hybrid_agent() {
        // Every Hybrid agent must be in exactly one of the two sets, so a newly
        // added agent can never silently skip the post-update hook refresh.
        for agent in HYBRID_AGENTS {
            let refreshed = REFRESHABLE_HOOK_AGENTS.contains(agent);
            let exempt = REFRESH_EXEMPT_HYBRID_AGENTS.contains(agent);
            assert!(
                refreshed ^ exempt,
                "hybrid agent `{agent}` must be either refreshed or explicitly exempt (exactly one)"
            );
        }
    }

    #[test]
    fn refresh_sets_reference_only_hybrid_agents() {
        for agent in REFRESHABLE_HOOK_AGENTS {
            assert!(
                HYBRID_AGENTS.contains(agent),
                "refreshable agent `{agent}` is not a Hybrid agent"
            );
        }
        for agent in REFRESH_EXEMPT_HYBRID_AGENTS {
            assert!(
                HYBRID_AGENTS.contains(agent),
                "exempt agent `{agent}` is not a Hybrid agent (stale exemption?)"
            );
        }
    }

    #[test]
    fn mcp_env_pairs_propagate_project_and_extra_roots_from_env() {
        // #403: init must bake the captured project/worktree scope into the MCP
        // server entry, otherwise the long-lived server rejects explicit paths
        // under sibling worktrees as jail escapes.
        let _iso = crate::core::data_dir::isolated_data_dir();
        crate::test_env::set_var("LEAN_CTX_PROJECT_ROOT", "/work/main");
        crate::test_env::set_var("LEAN_CTX_EXTRA_ROOTS", "/work/wt-a:/work/wt-b");

        let pairs = mcp_server_env_pairs();
        let get = |k: &str| pairs.iter().find(|(p, _)| p == k).map(|(_, v)| v.as_str());
        assert!(
            get("LEAN_CTX_DATA_DIR").is_none(),
            "data dir is auto-detected at runtime, never pinned into the config (GH #408)"
        );
        assert_eq!(get("LEAN_CTX_PROJECT_ROOT"), Some("/work/main"));
        assert_eq!(get("LEAN_CTX_EXTRA_ROOTS"), Some("/work/wt-a:/work/wt-b"));

        // The JSON view mirrors the pairs for the JSON-config agents.
        let json = mcp_server_env_json();
        assert_eq!(json["LEAN_CTX_PROJECT_ROOT"].as_str(), Some("/work/main"));

        crate::test_env::remove_var("LEAN_CTX_PROJECT_ROOT");
        crate::test_env::remove_var("LEAN_CTX_EXTRA_ROOTS");
    }

    #[test]
    fn mcp_env_pairs_omit_roots_when_unset() {
        // No project context configured anywhere ⇒ no env vars are emitted: the
        // data dir is auto-detected (never pinned, GH #408) and we never write
        // empty/placeholder root keys into agent configs.
        let _iso = crate::core::data_dir::isolated_data_dir();
        crate::test_env::remove_var("LEAN_CTX_PROJECT_ROOT");
        crate::test_env::remove_var("LEAN_CTX_EXTRA_ROOTS");

        let pairs = mcp_server_env_pairs();
        let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
        assert!(!keys.contains(&"LEAN_CTX_DATA_DIR"));
        assert!(!keys.contains(&"LEAN_CTX_PROJECT_ROOT"));
        assert!(!keys.contains(&"LEAN_CTX_EXTRA_ROOTS"));
    }

    #[test]
    fn hooks_installed_for_is_false_without_artifacts() {
        let tmp = unique_tmp_dir("leanctx_refresh_empty");
        for agent in REFRESHABLE_HOOK_AGENTS {
            // `codex` resolves its dir via the global CODEX_HOME-aware resolver
            // (not the passed home), so it cannot be isolated to a temp dir here;
            // its detection is exercised by the marker-content test instead.
            if *agent == "codex" {
                continue;
            }
            assert!(
                !hooks_installed_for(agent, &tmp),
                "`{agent}` should not be detected as installed in an empty home"
            );
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn hooks_installed_for_detects_marker_content() {
        let tmp = unique_tmp_dir("leanctx_refresh_marker");
        let hooks = tmp.join(".codeium/windsurf/hooks.json");
        std::fs::create_dir_all(hooks.parent().unwrap()).unwrap();

        // A foreign hooks.json must not trigger a refresh.
        std::fs::write(&hooks, "{\"hooks\":{}}").unwrap();
        assert!(!hooks_installed_for("windsurf", &tmp));

        // Once it mentions lean-ctx, it is ours and must be refreshed.
        std::fs::write(&hooks, "{\"hooks\":{\"cmd\":\"lean-ctx hook rewrite\"}}").unwrap();
        assert!(hooks_installed_for("windsurf", &tmp));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    fn unique_tmp_dir(prefix: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!("{prefix}_{}_{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

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

    // MSYS2 drive mapping applies on Windows hosts only — on Linux/macOS
    // /c/… is a literal directory and must pass through (GH #397).
    #[cfg(windows)]
    #[test]
    fn normalize_msys2_path() {
        assert_eq!(
            normalize_tool_path("/c/Users/game/Downloads/project"),
            "C:/Users/game/Downloads/project"
        );
        assert_eq!(
            normalize_tool_path("/d/Projects/app/src"),
            "D:/Projects/app/src"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn normalize_msys2_path_untouched_on_unix() {
        assert_eq!(
            crate::core::pathutil::normalize_tool_path_lexical("/c/Users/game/Downloads/project"),
            "/c/Users/game/Downloads/project"
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
        assert!(script.contains(r"rg\ *"), "script missing rg pattern");
        assert!(script.contains(r"ls\ *"), "script missing ls pattern");
        assert!(
            script.contains("LEAN_CTX_BIN=\"/usr/bin/lean-ctx\""),
            "script missing binary path"
        );
        assert!(
            script.contains("PowerShell|powershell"),
            "rewrite script must accept PowerShell tool names for Windows compatibility"
        );
    }

    #[test]
    fn compact_rewrite_script_uses_registry_pattern() {
        let script = generate_compact_rewrite_script("/usr/bin/lean-ctx");
        assert!(script.contains(r"git\ *"), "compact script missing git");
        assert!(script.contains(r"cargo\ *"), "compact script missing cargo");
        assert!(script.contains(r"rg\ *"), "compact script missing rg");
    }

    #[test]
    fn rewrite_scripts_contain_all_registry_commands() {
        let script = generate_rewrite_script("lean-ctx");
        let compact = generate_compact_rewrite_script("lean-ctx");
        for entry in crate::rewrite_registry::REWRITE_COMMANDS {
            if matches!(entry.category, crate::rewrite_registry::Category::FileRead) {
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

    #[test]
    fn codex_is_hybrid() {
        assert_eq!(recommend_hook_mode("codex"), HookMode::Hybrid);
    }

    #[test]
    fn cursor_is_hybrid() {
        assert_eq!(recommend_hook_mode("cursor"), HookMode::Hybrid);
    }

    #[test]
    fn gemini_is_hybrid() {
        assert_eq!(recommend_hook_mode("gemini"), HookMode::Hybrid);
    }

    #[test]
    fn claude_is_hybrid() {
        assert_eq!(recommend_hook_mode("claude"), HookMode::Hybrid);
    }

    #[test]
    fn unknown_agent_falls_back_to_mcp() {
        assert_eq!(recommend_hook_mode("unknown-agent"), HookMode::Mcp);
    }

    // Drive translation only applies on Windows hosts (GH #397).
    #[cfg(windows)]
    #[test]
    fn from_bash_to_native_converts_msys_drive() {
        assert_eq!(
            from_bash_to_native_path("/c/Users/ABC/lean-ctx"),
            "C:/Users/ABC/lean-ctx"
        );
        assert_eq!(
            from_bash_to_native_path("/d/Program Files/lean-ctx.exe"),
            "D:/Program Files/lean-ctx.exe"
        );
    }

    #[test]
    fn from_bash_to_native_unix_path_unchanged() {
        assert_eq!(
            from_bash_to_native_path("/usr/local/bin/lean-ctx"),
            "/usr/local/bin/lean-ctx"
        );
    }

    #[test]
    fn from_bash_to_native_bare_name() {
        assert_eq!(from_bash_to_native_path("lean-ctx"), "lean-ctx");
    }

    #[test]
    fn windows_path_to_bash_form() {
        let native = r"C:\Users\ABC\AppData\Local\lean-ctx\lean-ctx.exe";
        let bash = to_bash_compatible_path(native);
        assert_eq!(bash, "/c/Users/ABC/AppData/Local/lean-ctx/lean-ctx.exe");
    }

    // The bash→native return leg only translates on Windows hosts (GH #397).
    #[cfg(windows)]
    #[test]
    fn roundtrip_windows_path() {
        let native = r"C:\Users\ABC\AppData\Local\lean-ctx\lean-ctx.exe";
        let bash = to_bash_compatible_path(native);
        let back = from_bash_to_native_path(&bash);
        assert_eq!(back, "C:/Users/ABC/AppData/Local/lean-ctx/lean-ctx.exe");
    }

    #[test]
    fn roundtrip_unix_path() {
        let native = "/usr/local/bin/lean-ctx";
        let bash = to_bash_compatible_path(native);
        assert_eq!(bash, native);
        let back = from_bash_to_native_path(&bash);
        assert_eq!(back, native);
    }
}
