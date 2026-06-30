//! Configuration enums and their behavior.
//!
//! Extracted from `config::mod` to keep the top-level config module focused on
//! the `Config` struct and loading logic. These types are re-exported from the
//! `config` module root, so external paths like `config::CompressionLevel`
//! continue to work unchanged.

use serde::{Deserialize, Serialize};
use std::sync::atomic::AtomicU8;

use super::Config;

static SESSION_DEGRADE_LEVEL: AtomicU8 = AtomicU8::new(0);

/// Unified reasoning-effort level for the cache-safe, cross-provider effort
/// control (#834). "Off" is represented by `Option::None`, not a variant — the
/// feature is strictly opt-in.
///
/// This type only carries the operator's *intent*; the wire translation into
/// each provider's native parameter (OpenAI `reasoning(_).effort`, Anthropic
/// `output_config.effort`) lives in [`crate::proxy::effort`]. The value is a
/// constant once configured, so it is identical on every request of every
/// conversation — the provider prompt-cache prefix stays byte-stable (#448/#498)
/// and only the model's reasoning depth changes. Per-turn effort switching is
/// deliberately *not* supported: it would invalidate the prompt cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Minimal,
    Low,
    Medium,
    High,
}

impl Effort {
    /// Parse a config/env token. `off`, empty, or anything unrecognized yields
    /// `None` (feature disabled) so a typo can never silently enable it.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "minimal" => Some(Self::Minimal),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            _ => None,
        }
    }

    /// Stable lowercase label (config display, logs, `/status`).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// Controls when shell output is tee'd to disk for later retrieval.
///
/// Default is `HighCompression` (not `Failures`): a heavily compressed but
/// *successful* command is exactly the case where an agent later needs the raw
/// bytes, and teeing them guarantees the MCP-free recovery path (a real file the
/// agent can read with any tool) always exists. The archive GC (TTL + size cap)
/// covers the extra files.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TeeMode {
    Never,
    Failures,
    #[default]
    HighCompression,
    Always,
}

/// Controls the reactive recovery footer surfaced on compressed tool output
/// (`ctx_read`, archive/firewall/spill handles, `ctx_shell` tee).
///
/// The proactive `RECOVER` rule teaches the vocabulary once in the system
/// prompt; this knob governs the per-output reminder that names the concrete
/// file path / handle at point-of-need:
/// * `Minimal` (default) — a single, non-MCP-first line on the *first* compressed
///   view of a file/handle per session.
/// * `Full` — the richer ladder (`mode=full` · `raw=true` · `ctx_retrieve` ·
///   `ctx_expand`); used by the `exploration`/`review` profiles.
/// * `Off` — suppresses the footer entirely (the proactive rule still ships, so
///   reversibility is never undiscoverable).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RecoveryHints {
    Off,
    #[default]
    Minimal,
    Full,
}

impl RecoveryHints {
    /// Parse a config/env token. Returns `None` for unrecognized input so a typo
    /// falls back to the default rather than silently disabling the feature.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "false" => Some(Self::Off),
            "minimal" | "min" | "on" | "true" => Some(Self::Minimal),
            "full" => Some(Self::Full),
            _ => None,
        }
    }

    /// Reads the recovery-hint tier from `LEAN_CTX_RECOVERY_HINTS` (ops/test
    /// override). Unset or unrecognized yields `None` (use the configured value).
    #[must_use]
    pub fn from_env() -> Option<Self> {
        Self::parse(&std::env::var("LEAN_CTX_RECOVERY_HINTS").ok()?)
    }

    /// Stable lowercase label (config display, schema, `/status`).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Full => "full",
        }
    }
}

/// Legacy: Controls agent output verbosity level injected into MCP instructions.
/// Superseded by `CompressionLevel`. Kept for backward compatibility with old config.toml files.
/// New setups use `compression_level` instead. See `CompressionLevel::effective()`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TerseAgent {
    #[default]
    Off,
    Lite,
    Full,
    Ultra,
}

impl TerseAgent {
    /// Reads the terse-agent level from the `LEAN_CTX_TERSE_AGENT` env var.
    pub fn from_env() -> Self {
        match std::env::var("LEAN_CTX_TERSE_AGENT")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "lite" => Self::Lite,
            "full" => Self::Full,
            "ultra" => Self::Ultra,
            _ => Self::Off,
        }
    }
}

/// Legacy: Controls how dense/compact MCP tool output is formatted.
/// Superseded by `CompressionLevel`. Kept for backward compatibility with old config.toml files.
/// New setups use `compression_level` instead. See `CompressionLevel::effective()`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum OutputDensity {
    #[default]
    Normal,
    Terse,
    Ultra,
}

impl OutputDensity {
    /// Reads the output density from the `LEAN_CTX_OUTPUT_DENSITY` env var.
    pub fn from_env() -> Self {
        match std::env::var("LEAN_CTX_OUTPUT_DENSITY")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "terse" => Self::Terse,
            "ultra" => Self::Ultra,
            _ => Self::Normal,
        }
    }
}

/// Unified compression level that replaces the 4 separate legacy concepts:
/// `terse_agent`, `output_density`, `terse_mode`, and `crp_mode`.
///
/// Controls how much detail tool responses include.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ResponseVerbosity {
    #[default]
    Full,
    HeadersOnly,
}

impl ResponseVerbosity {
    pub fn effective() -> Self {
        if let Ok(v) = std::env::var("LEAN_CTX_RESPONSE_VERBOSITY") {
            match v.trim().to_lowercase().as_str() {
                "headers_only" | "headers" | "minimal" => return Self::HeadersOnly,
                "full" | "" => return Self::Full,
                _ => {}
            }
        }
        Config::load().response_verbosity
    }

    pub fn is_headers_only(&self) -> bool {
        matches!(self, Self::HeadersOnly)
    }
}

/// Each level maps to specific component settings via `to_components()`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum CompressionLevel {
    Off,
    /// Default: plain-English "concise" guidance (bullets, no filler). Readable
    /// by humans inspecting their rules files, and still token-saving. The
    /// denser, symbolic styles (`Standard`/`Max`, which enable CRP and the
    /// `→ ∵ ∴` vocabulary) are opt-in "power modes" — set `compression_level`
    /// in config. This only shapes the model's prose; tool-output compression
    /// is governed separately and is unaffected.
    #[default]
    Lite,
    Standard,
    Max,
}

/// Outcome of [`CompressionLevel::degrade_action`]: what to do with the session
/// degrade given the current re-fetch pressure. Split from the dispatch so the
/// threshold logic is a pure, testable function.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SessionDegrade {
    /// Set the session degrade to this level.
    Set(CompressionLevel),
    /// Clear any session degrade (pressure fully relaxed).
    Clear,
    /// Leave the current degrade unchanged (intermediate pressure band).
    Leave,
}

impl CompressionLevel {
    /// Decomposes the unified level into legacy component settings.
    /// Returns (TerseAgent, OutputDensity, crp_mode_str, terse_mode_bool).
    pub fn to_components(&self) -> (TerseAgent, OutputDensity, &'static str, bool) {
        match self {
            Self::Off => (TerseAgent::Off, OutputDensity::Normal, "off", false),
            Self::Lite => (TerseAgent::Lite, OutputDensity::Terse, "off", true),
            Self::Standard => (TerseAgent::Full, OutputDensity::Terse, "compact", true),
            Self::Max => (TerseAgent::Ultra, OutputDensity::Ultra, "tdd", true),
        }
    }

    /// Infers a `CompressionLevel` from legacy config keys for backward compatibility.
    /// Priority: terse_agent > output_density (picks the highest implied level).
    pub fn from_legacy(terse_agent: &TerseAgent, output_density: &OutputDensity) -> Self {
        match (terse_agent, output_density) {
            (TerseAgent::Ultra, _) | (_, OutputDensity::Ultra) => Self::Max,
            (TerseAgent::Full, _) => Self::Standard,
            (TerseAgent::Lite, _) | (_, OutputDensity::Terse) => Self::Lite,
            _ => Self::Off,
        }
    }

    /// Reads the compression level from the `LEAN_CTX_COMPRESSION` env var.
    pub fn from_env() -> Option<Self> {
        std::env::var("LEAN_CTX_COMPRESSION").ok().and_then(|v| {
            match v.trim().to_lowercase().as_str() {
                "off" => Some(Self::Off),
                "lite" => Some(Self::Lite),
                "standard" => Some(Self::Standard),
                "max" => Some(Self::Max),
                _ => None,
            }
        })
    }

    /// Returns the effective compression level with resolution order:
    /// 0. Session-level degrade override (set by correction-loop feedback)
    /// 1. `LEAN_CTX_COMPRESSION` env var
    /// 2. `compression_level` in config
    /// 3. Legacy `ultra_compact` flag (maps to `Max`)
    /// 4. Legacy env vars (`LEAN_CTX_TERSE_AGENT`, `LEAN_CTX_OUTPUT_DENSITY`)
    /// 5. Legacy config fields (`terse_agent`, `output_density`)
    pub fn effective(config: &Config) -> Self {
        if let Some(degraded) = Self::session_degrade_level() {
            return degraded;
        }
        if let Some(env_level) = Self::from_env() {
            return env_level;
        }
        if config.compression_level != Self::Off {
            return config.compression_level;
        }
        if config.ultra_compact {
            return Self::Max;
        }
        let ta_env = TerseAgent::from_env();
        let od_env = OutputDensity::from_env();
        let ta = if ta_env == TerseAgent::Off {
            config.terse_agent.clone()
        } else {
            ta_env
        };
        let od = if od_env == OutputDensity::Normal {
            config.output_density.clone()
        } else {
            od_env
        };
        Self::from_legacy(&ta, &od)
    }

    /// Session-level degrade: correction loop detected, temporarily reduce compression.
    /// 0 = no override, 1 = Off, 2 = Lite
    pub fn session_degrade_level() -> Option<Self> {
        match SESSION_DEGRADE_LEVEL.load(std::sync::atomic::Ordering::Relaxed) {
            1 => Some(Self::Off),
            2 => Some(Self::Lite),
            _ => None,
        }
    }

    /// Sets a session-level compression degrade (called by correction loop detection).
    pub fn set_session_degrade(level: &Self) {
        let val = match level {
            Self::Off => 1u8,
            Self::Lite => 2u8,
            _ => 0u8,
        };
        SESSION_DEGRADE_LEVEL.store(val, std::sync::atomic::Ordering::Relaxed);
    }

    /// Clears the session-level degrade (recovery after correction rate drops).
    pub fn clear_session_degrade() {
        SESSION_DEGRADE_LEVEL.store(0, std::sync::atomic::Ordering::Relaxed);
    }

    /// Maps re-fetch *pressure* to a session-degrade decision. Pressure is the
    /// stronger of the correction-loop count (re-reads/re-runs) and the CCR
    /// retrieve count (`ctx_expand`/`ctx_retrieve`) — two views of the same "too
    /// aggressive" signal (#941): 5+ degrades to `Off`, 3+ to `Lite`, 0 clears,
    /// and the 1–2 band leaves the current degrade untouched.
    ///
    /// Pure and total so the thresholds are unit-testable without the dispatch
    /// path — the regression guard for the brittle source-grep test this replaced
    /// (#957).
    pub fn degrade_action(correction_count: u32, retrieve_count: u32) -> SessionDegrade {
        let pressure = correction_count.max(retrieve_count);
        if pressure >= 5 {
            SessionDegrade::Set(Self::Off)
        } else if pressure >= 3 {
            SessionDegrade::Set(Self::Lite)
        } else if pressure == 0 {
            SessionDegrade::Clear
        } else {
            SessionDegrade::Leave
        }
    }

    /// Applies a [`SessionDegrade`] decision to the process-global session state.
    pub fn apply_degrade_action(action: SessionDegrade) {
        match action {
            SessionDegrade::Set(level) => Self::set_session_degrade(&level),
            SessionDegrade::Clear => Self::clear_session_degrade(),
            SessionDegrade::Leave => {}
        }
    }

    pub fn from_str_label(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "off" => Some(Self::Off),
            "lite" => Some(Self::Lite),
            "standard" | "std" => Some(Self::Standard),
            "max" => Some(Self::Max),
            _ => None,
        }
    }

    pub fn is_active(&self) -> bool {
        !matches!(self, Self::Off)
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Lite => "lite",
            Self::Standard => "standard",
            Self::Max => "max",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::Off => "No compression — full verbose output",
            Self::Lite => "Light compression — concise output, basic terse filtering",
            Self::Standard => {
                "Standard compression — dense output, compact protocol, pattern-aware"
            }
            Self::Max => "Maximum compression — expert mode, TDD protocol, all layers active",
        }
    }
}

/// Where agent rule files are installed: global home dir, project-local, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RulesScope {
    Both,
    Global,
    Project,
}

/// How agent rules are injected for AGENTS.md/CLAUDE.md/CODEBUDDY.md/GEMINI.md consumers.
///
/// - `Shared` (default): write a marker-delimited block into the user's shared
///   instruction file (`CLAUDE.md`, `CODEBUDDY.md`, `AGENTS.md`, `GEMINI.md`) — zero-config
///   discoverability, but touches a file the user also authors.
/// - `Dedicated`: never write into those shared files. Instead use each agent's
///   config-driven, fully-removable auto-load path (Claude/Codex `SessionStart`
///   hook `additionalContext`, OpenCode `instructions[]`, Gemini
///   `context.fileName`) plus a lean-ctx-owned rules file. See issue #343.
/// - `Off`: never write any rules file. For hosts that already supply their own
///   tool-steering workflow (e.g. an embedded extension) or for phase-isolated /
///   non-caching harnesses where the injected prefix is pure re-billed overhead
///   with no cached-re-read dividend to amortize it. See GitHub #361.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RulesInjection {
    Shared,
    Dedicated,
    Off,
}

/// Whether lean-ctx mirrors the host IDE's tool-permission rules onto its own
/// MCP tools ("permission inheritance").
///
/// - `Off` (default): lean-ctx tools are governed only by lean-ctx's own layers
///   (role policy, shell allowlist). lean-ctx's `ctx_shell` therefore runs
///   independently of the IDE's `bash`/`rm *` permission rules.
/// - `On`: before dispatching, lean-ctx reads the active IDE's permission config
///   (v1: OpenCode `opencode.json[c]`) and applies the equivalent decision to
///   the matching lean-ctx tool — `deny` blocks, `ask` is held back (MCP cannot
///   prompt for these tools), `allow` proceeds. Read-only; lean-ctx never writes
///   the IDE's `permission` block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionInheritance {
    Off,
    On,
}

#[cfg(test)]
mod tests {
    use super::{CompressionLevel, SessionDegrade};

    #[test]
    fn degrade_action_thresholds_are_pressure_based() {
        use CompressionLevel::{Lite, Off};
        use SessionDegrade::{Clear, Leave, Set};
        // 5+ pressure → Off, driven by EITHER the correction-loop or the CCR
        // retrieve count (the stronger of the two), per #941.
        assert_eq!(CompressionLevel::degrade_action(5, 0), Set(Off));
        assert_eq!(CompressionLevel::degrade_action(0, 5), Set(Off));
        assert_eq!(CompressionLevel::degrade_action(9, 1), Set(Off));
        // 3–4 pressure → Lite.
        assert_eq!(CompressionLevel::degrade_action(3, 0), Set(Lite));
        assert_eq!(CompressionLevel::degrade_action(0, 4), Set(Lite));
        assert_eq!(CompressionLevel::degrade_action(4, 4), Set(Lite));
        // 0 pressure → clear; the 1–2 band holds the current degrade.
        assert_eq!(CompressionLevel::degrade_action(0, 0), Clear);
        assert_eq!(CompressionLevel::degrade_action(2, 1), Leave);
        assert_eq!(CompressionLevel::degrade_action(1, 2), Leave);
    }
}
