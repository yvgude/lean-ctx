// SPDX-License-Identifier: Apache-2.0
//! Hook-mode policy shared by the engine and the agent-hook installers.
//!
//! Which integration mode an agent gets (`Mcp` / `Hybrid` / `Replace`), the
//! agent lists behind that choice, and the MCP server env block every
//! installer writes. Pure policy over `Config` and the environment: the
//! installers in `hooks` consume it, and engine modules (config, telemetry,
//! host coverage, context overhead) read it without depending on `hooks`.

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

/// Agents with reliable shell + hook infrastructure that *can* run Replace mode
/// (native tools denied, lean-ctx MCP is the only path). These agents have
/// either `permissions.deny` support or PreToolUse deny-hook capability.
///
/// Replace is opt-in (`hook_mode = "replace"`): denying native tools made
/// agents fail and retry instead of working, so these agents default to Hybrid
/// like every other hook-capable agent.
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
    "omp",
    "qoder",
    "qodercli",
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

/// First-class agents that get MCP registration only — no shell hooks, because
/// their hook surface does not (yet) carry lean-ctx's contract.
///
/// CodeWhale (#1402) is the first entry: its hooks are TUI-only and follow
/// observer/steering semantics rather than the pre-tool rewrite/deny contract
/// every `HYBRID_AGENTS` entry relies on, so installing lean-ctx's shell hooks
/// there would ship a script whose behaviour we cannot honestly predict.
/// Membership here does not enable any hook install — it only marks the agent
/// as genuinely supported by `init --agent` / `setup` so `wrap` can say so.
pub const MCP_ONLY_AGENTS: &[&str] = &["codewhale"];

/// True when `agent` is integrated via `init --agent` / `setup` (hooks, rules,
/// MCP registration). GH #1520: used by `wrap`'s error path to point users of
/// a supported-but-not-proxy-wrappable agent (e.g. opencode) at the working
/// command instead of a bare "unsupported agent".
pub fn is_supported_agent(agent: &str) -> bool {
    let key = agent.to_ascii_lowercase();
    REPLACE_AGENTS.contains(&key.as_str())
        || HYBRID_AGENTS.contains(&key.as_str())
        || MCP_ONLY_AGENTS.contains(&key.as_str())
}

/// Auto-detect the best hook mode for a given agent key.
///
/// Priority: disabled/shadow_mode cap > config override > Hybrid > Mcp
/// - Replace: native tools denied, MCP-only path — only when the user chose it
///   (`hook_mode = "replace"`, `LEAN_CTX_HOOK_MODE=replace`, `harden --hard`)
/// - Hybrid: MCP + shell hooks; native tools keep working, known commands
///   are rewritten through lean-ctx (default for every hook-capable agent)
/// - Mcp: MCP server only (no shell hooks available)
///
/// `LEAN_CTX_DISABLED=1` or `shadow_mode = false` cap the mode at Hybrid so
/// the deny-list is never injected when the user opted out (#1037).
pub fn recommend_hook_mode(agent_key: &str) -> HookMode {
    if let Some(override_mode) = crate::core::config::Config::load().hook_mode_override() {
        return if is_deny_suppressed() {
            override_mode.cap_at_hybrid()
        } else {
            override_mode
        };
    }
    if REPLACE_AGENTS.contains(&agent_key) || HYBRID_AGENTS.contains(&agent_key) {
        HookMode::Hybrid
    } else {
        HookMode::Mcp
    }
}

/// True when native-tool denial is in effect: the user chose Replace and has
/// not suppressed denials (#1037). Deny hooks consult this at call time, so a
/// hook entry written while Replace was the default stops blocking the moment
/// the binary updates — no settings rewrite has to happen first.
#[must_use]
pub fn replace_mode_chosen() -> bool {
    crate::core::config::Config::load().hook_mode_override() == Some(HookMode::Replace)
        && !is_deny_suppressed()
}

/// Whether Claude Code (and CodeBuddy, which shares its contract) keeps native
/// Grep/Glob out of the model's tool list via `permissions.deny`.
///
/// Unlike a hook denial this costs no error and no retry: Claude Code drops a
/// denied tool from the list, so the model never calls it. And it keeps
/// searches on the compressed path — native Glob cannot be redirected (#1033).
/// Measured 2026-10-13 over 30 days: 0 Grep/Glob calls and 0 denials in 380
/// Claude sessions with the list, while native Glob returned ~12× the tokens
/// of `ctx_glob` for the same patterns. So the default (auto) mode keeps it;
/// an explicit `hook_mode = "hybrid"`/`"mcp"`, `shadow_mode = false` or
/// `LEAN_CTX_DISABLED` give the tools back.
#[must_use]
pub fn claude_hides_native_search(mode: HookMode) -> bool {
    match mode {
        HookMode::Replace => true,
        HookMode::Hybrid => {
            crate::core::config::Config::load()
                .hook_mode_override()
                .is_none()
                && !is_deny_suppressed()
        }
        HookMode::Mcp => false,
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

/// Env key/value pairs for the lean-ctx MCP server entry written into agent
/// configs (Codex TOML + the JSON agents).
///
/// Deliberately does NOT pin `LEAN_CTX_DATA_DIR`: lean-ctx auto-detects its
/// per-category dirs (config/data/state/cache) at runtime, and pinning the data
/// dir would set that var in the server's environment, forcing single-dir mode
/// and collapsing config/state/cache onto the data dir — defeating the XDG split
/// (GH #408).
///
/// Deliberately does NOT pin a project either. Agent configs are user-global:
/// one `~/.codex/config.toml` serves every Codex session on the machine. The
/// #403 build copied the *installing* process's `LEAN_CTX_PROJECT_ROOT` /
/// `LEAN_CTX_EXTRA_ROOTS` in here, so whichever project last ran `setup`,
/// `doctor --fix` or an MCP-start hook refresh became the project of every
/// later session — and each switch needed a config edit plus an MCP restart.
/// The server derives its project per session instead (host roots, host env,
/// working directory) and reads `extra_roots` from config.toml itself; stale
/// pins are removed by `hooks::project_pins`.
///
/// Single source of truth so every agent installer stays consistent; it is
/// currently empty on purpose.
pub fn mcp_server_env_pairs() -> Vec<(String, String)> {
    Vec::new()
}

/// The MCP server env block as a JSON object, for the JSON-config agents.
pub fn mcp_server_env_json() -> serde_json::Value {
    let map: serde_json::Map<String, serde_json::Value> = mcp_server_env_pairs()
        .into_iter()
        .map(|(k, v)| (k, serde_json::Value::String(v)))
        .collect();
    serde_json::Value::Object(map)
}
