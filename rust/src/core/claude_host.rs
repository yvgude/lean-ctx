// SPDX-License-Identifier: Apache-2.0
//! Claude Code host facts the engine reads without the hook installers.
//!
//! - the governor setup arms and the rule block each writes (measured by
//!   `context_overhead`, installed by `hooks::agents::claude_governor`) and
//!   whether that setup is installed (checked by the standard installers);
//! - recognising lean-ctx's own status line command (HUD, statusline installer);
//! - the Claude Code mod sources the binary embeds (`rule_artifacts` mirrors
//!   them, `hooks::agents::claude_mod` installs them).

/// The governor setup variant (owner decision O-2: both are measured).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GovernorArm {
    /// No MCP server: 0 schema tokens, strong tools through Bash.
    NoMcp,
    /// MCP server without instructions, schemas deferred behind Tool Search.
    McpDeferred,
}

impl GovernorArm {
    /// `--mode` value → arm. `None` for every non-governor mode.
    pub fn from_mode(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "governor" | "governor-nomcp" => Some(Self::NoMcp),
            "governor-mcp" => Some(Self::McpDeferred),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::NoMcp => "governor",
            Self::McpDeferred => "governor-mcp",
        }
    }
}

const RULES_MCP: &str = "\
<!-- lean-ctx -->
<!-- lean-ctx-governor-v1 -->
lean-ctx keeps native Read/Bash results lossless; an identical repeat may show `[unchanged …]` (the full result is earlier in this conversation).
Optional `ctx_*` tools (e.g. ctx_compose for ranked files with source) load on demand via tool search.
<!-- /lean-ctx -->";

/// The rule block the arm writes — the only per-turn text it adds. The no-MCP
/// arm writes none.
pub fn rules_block(arm: GovernorArm) -> Option<&'static str> {
    match arm {
        GovernorArm::NoMcp => None,
        GovernorArm::McpDeferred => Some(RULES_MCP),
    }
}

/// True when the settings carry the governor's PostToolUse hook. Standard
/// installers and refreshes check this so they never fight the profile.
pub fn is_governor_installed(home: &std::path::Path) -> bool {
    std::fs::read_to_string(
        crate::core::editor_registry::claude_state_dir(home).join("settings.json"),
    )
    .is_ok_and(|c| c.contains(" hook govern\""))
}

/// True when Claude Code's MCP config (`~/.claude.json`, or the one under
/// `CLAUDE_CONFIG_DIR`) registers the lean-ctx server. The standard setup's
/// Grep/Glob deny is only safe with it: the deny hides native search in
/// favour of `ctx_search`/`ctx_glob`, which only this entry provides.
pub fn has_lean_ctx_mcp_entry(home: &std::path::Path) -> bool {
    lean_ctx_mcp_entry_state(home) == Some(true)
}

/// Like [`has_lean_ctx_mcp_entry`], but `None` when the file exists and does
/// not parse (Claude Code may be rewriting it): callers that would remove
/// something on `false` leave it unchanged instead.
pub fn lean_ctx_mcp_entry_state(home: &std::path::Path) -> Option<bool> {
    match std::fs::read_to_string(crate::core::editor_registry::claude_mcp_json_path(home)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
        Ok(c) if c.trim().is_empty() => Some(false),
        Ok(c) => crate::core::jsonc::parse_jsonc(&c)
            .ok()
            .map(|v| v.pointer("/mcpServers/lean-ctx").is_some()),
    }
}

/// `… lean-ctx … statusline [args]` — the binary may be quoted or portable.
pub fn is_lean_ctx_statusline(cmd: &str) -> bool {
    statusline_args(cmd).is_some()
}

/// The part after the `statusline` subcommand (with its leading space), when
/// `cmd` runs lean-ctx's status line.
pub fn statusline_args(cmd: &str) -> Option<&str> {
    let idx = cmd.find(" statusline")?;
    let (bin, rest) = cmd.split_at(idx);
    let rest = &rest[" statusline".len()..];
    let ends_token = rest.is_empty() || rest.starts_with(' ');
    (bin.contains("lean-ctx") && ends_token).then_some(rest)
}

pub const REGISTER_TS: &str = include_str!("../templates/claude_mod/register.ts");
pub const HOOKS_JSON: &str = include_str!("../templates/claude_mod/hooks.json");
/// The cockpit (sidebar, pulse line, overlays), imported by `register.ts`.
pub const COCKPIT_TSX: &str = include_str!("../templates/claude_mod/cockpit.tsx");
/// ctx_patch / ctx_read / ctx_search results drawn as diffs and code,
/// imported by `register.ts`.
pub const RENDER_TSX: &str = include_str!("../templates/claude_mod/render.tsx");
/// The `$.state` contract the manifest names as `types`.
pub const TYPES_DTS: &str = include_str!("../templates/claude_mod/cockpit.d.ts");
/// Source manifest; `hooks::agents::claude_mod::plugin_manifest` stamps the engine version into it.
pub const PLUGIN_JSON: &str = include_str!("../templates/claude_mod/plugin.json");
