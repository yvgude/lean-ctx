use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

use super::memory_policy::MemoryPolicy;

mod memory;
mod proxy;
pub mod schema;
mod serde_defaults;
mod shell_activation;

pub use memory::{MemoryCleanup, MemoryGuardConfig, MemoryProfile, SavingsFooter};
pub use proxy::{is_local_proxy_url, normalize_url, normalize_url_opt, ProxyConfig, ProxyProvider};
pub use shell_activation::ShellActivation;

/// Default BM25 cache cap from config (also used by `bm25_index` heuristics).
pub fn default_bm25_max_cache_mb() -> u64 {
    serde_defaults::default_bm25_max_cache_mb()
}

/// Controls when shell output is tee'd to disk for later retrieval.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TeeMode {
    Never,
    #[default]
    Failures,
    Always,
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
/// Each level maps to specific component settings via `to_components()`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum CompressionLevel {
    #[default]
    Off,
    Lite,
    Standard,
    Max,
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
    /// 1. `LEAN_CTX_COMPRESSION` env var
    /// 2. `compression_level` in config
    /// 3. Legacy `ultra_compact` flag (maps to `Max`)
    /// 4. Legacy env vars (`LEAN_CTX_TERSE_AGENT`, `LEAN_CTX_OUTPUT_DENSITY`)
    /// 5. Legacy config fields (`terse_agent`, `output_density`)
    pub fn effective(config: &Config) -> Self {
        if let Some(env_level) = Self::from_env() {
            return env_level;
        }
        if config.compression_level != Self::Off {
            return config.compression_level.clone();
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

/// Global lean-ctx configuration loaded from `config.toml`, merged with project-local overrides.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub ultra_compact: bool,
    #[serde(default, deserialize_with = "serde_defaults::deserialize_tee_mode")]
    pub tee_mode: TeeMode,
    #[serde(default)]
    pub output_density: OutputDensity,
    pub checkpoint_interval: u32,
    pub excluded_commands: Vec<String>,
    pub passthrough_urls: Vec<String>,
    pub custom_aliases: Vec<AliasEntry>,
    /// Commands taking longer than this threshold (ms) are recorded in the slow log.
    /// Set to 0 to disable slow logging.
    pub slow_command_threshold_ms: u64,
    #[serde(default = "serde_defaults::default_theme")]
    pub theme: String,
    #[serde(default)]
    pub cloud: CloudConfig,
    #[serde(default)]
    pub autonomy: AutonomyConfig,
    #[serde(default)]
    pub proxy: ProxyConfig,
    #[serde(default = "serde_defaults::default_buddy_enabled")]
    pub buddy_enabled: bool,
    #[serde(default)]
    pub redirect_exclude: Vec<String>,
    /// Tools to exclude from the MCP tool list returned by list_tools.
    /// Accepts exact tool names (e.g. `["ctx_graph", "ctx_agent"]`).
    /// Empty by default — all tools listed, no behaviour change.
    #[serde(default)]
    pub disabled_tools: Vec<String>,
    #[serde(default)]
    pub loop_detection: LoopDetectionConfig,
    /// Controls where lean-ctx installs agent rule files.
    /// Values: "both" (default), "global" (home-dir only), "project" (repo-local only).
    /// Override via LEAN_CTX_RULES_SCOPE env var.
    #[serde(default)]
    pub rules_scope: Option<String>,
    /// Extra glob patterns to ignore in graph/overview/preload (repo-local).
    /// Example: `["externals/**", "target/**", "temp/**"]`
    #[serde(default)]
    pub extra_ignore_patterns: Vec<String>,
    /// Controls agent output verbosity via instructions injection.
    /// Values: "off" (default), "lite", "full", "ultra".
    /// Override via LEAN_CTX_TERSE_AGENT env var.
    #[serde(default)]
    pub terse_agent: TerseAgent,
    /// Unified compression level (replaces separate terse_agent + output_density).
    /// Values: "off" (default), "lite", "standard", "max".
    /// Override via LEAN_CTX_COMPRESSION env var.
    #[serde(default)]
    pub compression_level: CompressionLevel,
    /// Archive configuration for zero-loss compression.
    #[serde(default)]
    pub archive: ArchiveConfig,
    /// Memory policy (knowledge/episodic/procedural/lifecycle budgets & thresholds).
    #[serde(default)]
    pub memory: MemoryPolicy,
    /// Additional paths allowed by PathJail (absolute).
    /// Useful for multi-project workspaces where the jail root is a parent directory.
    /// Override via LEAN_CTX_ALLOW_PATH env var (path-list separator).
    #[serde(default)]
    pub allow_paths: Vec<String>,
    /// Enable content-defined chunking (Rabin-Karp) for cache-optimal output ordering.
    /// Stable chunks are emitted first to maximize prompt cache hits.
    #[serde(default)]
    pub content_defined_chunking: bool,
    /// Skip session/knowledge/gotcha blocks in MCP instructions to minimize token overhead.
    /// Override via LEAN_CTX_MINIMAL env var.
    #[serde(default)]
    pub minimal_overhead: bool,
    /// Disable shell hook injection (the _lc() function that wraps CLI commands).
    /// Override via LEAN_CTX_NO_HOOK env var.
    #[serde(default)]
    pub shell_hook_disabled: bool,
    /// Controls when the shell hook auto-activates aliases.
    /// - `always`: (Default) Aliases active in every interactive shell.
    /// - `agents-only`: Aliases only active when an AI agent env var is detected.
    /// - `off`: Aliases never auto-activate (user must call `lean-ctx-on` manually).
    ///
    /// Override via `LEAN_CTX_SHELL_ACTIVATION` env var.
    #[serde(default)]
    pub shell_activation: ShellActivation,
    /// Disable the daily version check against leanctx.com/version.txt.
    /// Override via LEAN_CTX_NO_UPDATE_CHECK env var.
    #[serde(default)]
    pub update_check_disabled: bool,
    /// Maximum BM25 cache file size in MB. Indexes exceeding this are quarantined on load
    /// and refused on save. Override via LEAN_CTX_BM25_MAX_CACHE_MB env var.
    #[serde(default = "serde_defaults::default_bm25_max_cache_mb")]
    pub bm25_max_cache_mb: u64,
    /// Maximum number of files scanned by the lightweight JSON graph index.
    /// Increase for large monorepos. Default: 5000.
    #[serde(default = "serde_defaults::default_graph_index_max_files")]
    pub graph_index_max_files: u64,
    /// Controls RAM vs feature trade-off. Values: "low", "balanced" (default), "performance".
    /// Override via LEAN_CTX_MEMORY_PROFILE env var.
    #[serde(default)]
    pub memory_profile: MemoryProfile,
    /// Controls how aggressively memory is freed when idle.
    /// Values: "aggressive" (default, 5 min TTL), "shared" (30 min TTL for multi-IDE use).
    /// Override via LEAN_CTX_MEMORY_CLEANUP env var.
    #[serde(default)]
    pub memory_cleanup: MemoryCleanup,
    /// Maximum percentage of system RAM that lean-ctx may use (default: 5).
    /// Override via LEAN_CTX_MAX_RAM_PERCENT env var.
    #[serde(default = "serde_defaults::default_max_ram_percent")]
    pub max_ram_percent: u8,
    /// Controls visibility of token savings footers in tool output.
    /// Values: "never" (default, suppress everywhere), "always", "auto" (legacy compatibility).
    /// Override via LEAN_CTX_SAVINGS_FOOTER env var.
    #[serde(default)]
    pub savings_footer: SavingsFooter,
    /// Explicit project root override. When set, lean-ctx uses this instead of auto-detection.
    /// This prevents accidental home-directory scans when running from $HOME.
    /// Override via LEAN_CTX_PROJECT_ROOT env var.
    #[serde(default)]
    pub project_root: Option<String>,
}

/// Settings for the zero-loss compression archive (large tool outputs saved to disk).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ArchiveConfig {
    pub enabled: bool,
    pub threshold_chars: usize,
    pub max_age_hours: u64,
    pub max_disk_mb: u64,
}

impl Default for ArchiveConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold_chars: 4096,
            max_age_hours: 48,
            max_disk_mb: 500,
        }
    }
}

/// Controls autonomous background behaviors (preload, dedup, consolidation).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AutonomyConfig {
    pub enabled: bool,
    pub auto_preload: bool,
    pub auto_dedup: bool,
    pub auto_related: bool,
    pub auto_consolidate: bool,
    pub silent_preload: bool,
    pub dedup_threshold: usize,
    pub consolidate_every_calls: u32,
    pub consolidate_cooldown_secs: u64,
}

impl Default for AutonomyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_preload: true,
            auto_dedup: true,
            auto_related: true,
            auto_consolidate: true,
            silent_preload: true,
            dedup_threshold: 8,
            consolidate_every_calls: 25,
            consolidate_cooldown_secs: 120,
        }
    }
}

impl AutonomyConfig {
    /// Creates an autonomy config from env vars, falling back to defaults.
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Ok(v) = std::env::var("LEAN_CTX_AUTONOMY") {
            if v == "false" || v == "0" {
                cfg.enabled = false;
            }
        }
        if let Ok(v) = std::env::var("LEAN_CTX_AUTO_PRELOAD") {
            cfg.auto_preload = v != "false" && v != "0";
        }
        if let Ok(v) = std::env::var("LEAN_CTX_AUTO_DEDUP") {
            cfg.auto_dedup = v != "false" && v != "0";
        }
        if let Ok(v) = std::env::var("LEAN_CTX_AUTO_RELATED") {
            cfg.auto_related = v != "false" && v != "0";
        }
        if let Ok(v) = std::env::var("LEAN_CTX_AUTO_CONSOLIDATE") {
            cfg.auto_consolidate = v != "false" && v != "0";
        }
        if let Ok(v) = std::env::var("LEAN_CTX_SILENT_PRELOAD") {
            cfg.silent_preload = v != "false" && v != "0";
        }
        if let Ok(v) = std::env::var("LEAN_CTX_DEDUP_THRESHOLD") {
            if let Ok(n) = v.parse() {
                cfg.dedup_threshold = n;
            }
        }
        if let Ok(v) = std::env::var("LEAN_CTX_CONSOLIDATE_EVERY_CALLS") {
            if let Ok(n) = v.parse() {
                cfg.consolidate_every_calls = n;
            }
        }
        if let Ok(v) = std::env::var("LEAN_CTX_CONSOLIDATE_COOLDOWN_SECS") {
            if let Ok(n) = v.parse() {
                cfg.consolidate_cooldown_secs = n;
            }
        }
        cfg
    }

    /// Loads autonomy config from disk, with env var overrides applied.
    pub fn load() -> Self {
        let file_cfg = Config::load().autonomy;
        let mut cfg = file_cfg;
        if let Ok(v) = std::env::var("LEAN_CTX_AUTONOMY") {
            if v == "false" || v == "0" {
                cfg.enabled = false;
            }
        }
        if let Ok(v) = std::env::var("LEAN_CTX_AUTO_PRELOAD") {
            cfg.auto_preload = v != "false" && v != "0";
        }
        if let Ok(v) = std::env::var("LEAN_CTX_AUTO_DEDUP") {
            cfg.auto_dedup = v != "false" && v != "0";
        }
        if let Ok(v) = std::env::var("LEAN_CTX_AUTO_RELATED") {
            cfg.auto_related = v != "false" && v != "0";
        }
        if let Ok(v) = std::env::var("LEAN_CTX_SILENT_PRELOAD") {
            cfg.silent_preload = v != "false" && v != "0";
        }
        if let Ok(v) = std::env::var("LEAN_CTX_DEDUP_THRESHOLD") {
            if let Ok(n) = v.parse() {
                cfg.dedup_threshold = n;
            }
        }
        cfg
    }
}

/// Cloud sync and contribution settings (pattern sharing, model pulls).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CloudConfig {
    pub contribute_enabled: bool,
    pub last_contribute: Option<String>,
    pub last_sync: Option<String>,
    pub last_gain_sync: Option<String>,
    pub last_model_pull: Option<String>,
}

/// A user-defined command alias mapping for shell compression patterns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AliasEntry {
    pub command: String,
    pub alias: String,
}

/// Thresholds for detecting and throttling repetitive agent tool call loops.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LoopDetectionConfig {
    pub normal_threshold: u32,
    pub reduced_threshold: u32,
    pub blocked_threshold: u32,
    pub window_secs: u64,
    pub search_group_limit: u32,
    pub tool_total_limits: HashMap<String, u32>,
}

impl Default for LoopDetectionConfig {
    fn default() -> Self {
        let mut tool_total_limits = HashMap::new();
        tool_total_limits.insert("ctx_read".to_string(), 100);
        tool_total_limits.insert("ctx_search".to_string(), 80);
        tool_total_limits.insert("ctx_shell".to_string(), 50);
        tool_total_limits.insert("ctx_semantic_search".to_string(), 60);
        Self {
            normal_threshold: 2,
            reduced_threshold: 4,
            blocked_threshold: 0,
            window_secs: 300,
            search_group_limit: 10,
            tool_total_limits,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ultra_compact: false,
            tee_mode: TeeMode::default(),
            output_density: OutputDensity::default(),
            checkpoint_interval: 15,
            excluded_commands: Vec::new(),
            passthrough_urls: Vec::new(),
            custom_aliases: Vec::new(),
            slow_command_threshold_ms: 5000,
            theme: serde_defaults::default_theme(),
            cloud: CloudConfig::default(),
            autonomy: AutonomyConfig::default(),
            proxy: ProxyConfig::default(),
            buddy_enabled: serde_defaults::default_buddy_enabled(),
            redirect_exclude: Vec::new(),
            disabled_tools: Vec::new(),
            loop_detection: LoopDetectionConfig::default(),
            rules_scope: None,
            extra_ignore_patterns: Vec::new(),
            terse_agent: TerseAgent::default(),
            compression_level: CompressionLevel::default(),
            archive: ArchiveConfig::default(),
            memory: MemoryPolicy::default(),
            allow_paths: Vec::new(),
            content_defined_chunking: false,
            minimal_overhead: false,
            shell_hook_disabled: false,
            shell_activation: ShellActivation::default(),
            update_check_disabled: false,
            graph_index_max_files: serde_defaults::default_graph_index_max_files(),
            bm25_max_cache_mb: serde_defaults::default_bm25_max_cache_mb(),
            memory_profile: MemoryProfile::default(),
            memory_cleanup: MemoryCleanup::default(),
            max_ram_percent: serde_defaults::default_max_ram_percent(),
            savings_footer: SavingsFooter::default(),
            project_root: None,
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

impl Config {
    /// Returns the effective rules scope, preferring env var over config file.
    pub fn rules_scope_effective(&self) -> RulesScope {
        let raw = std::env::var("LEAN_CTX_RULES_SCOPE")
            .ok()
            .or_else(|| self.rules_scope.clone())
            .unwrap_or_default();
        match raw.trim().to_lowercase().as_str() {
            "global" => RulesScope::Global,
            "project" => RulesScope::Project,
            _ => RulesScope::Both,
        }
    }

    fn parse_disabled_tools_env(val: &str) -> Vec<String> {
        val.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// Returns the effective disabled tools list, preferring env var over config file.
    pub fn disabled_tools_effective(&self) -> Vec<String> {
        if let Ok(val) = std::env::var("LEAN_CTX_DISABLED_TOOLS") {
            Self::parse_disabled_tools_env(&val)
        } else {
            self.disabled_tools.clone()
        }
    }

    /// Returns `true` if minimal overhead is enabled via env var or config.
    pub fn minimal_overhead_effective(&self) -> bool {
        std::env::var("LEAN_CTX_MINIMAL").is_ok() || self.minimal_overhead
    }

    /// Returns `true` if minimal overhead should be enabled for this MCP client.
    ///
    /// This is a superset of `minimal_overhead_effective()`:
    /// - `LEAN_CTX_OVERHEAD_MODE=minimal` forces minimal overhead
    /// - `LEAN_CTX_OVERHEAD_MODE=full` disables client/model heuristics (still honors LEAN_CTX_MINIMAL / config)
    /// - In auto mode (default), certain low-context clients/models are treated as minimal to prevent
    ///   large metadata blocks from destabilizing smaller context windows (e.g. Hermes + MiniMax).
    pub fn minimal_overhead_effective_for_client(&self, client_name: &str) -> bool {
        if let Ok(raw) = std::env::var("LEAN_CTX_OVERHEAD_MODE") {
            match raw.trim().to_lowercase().as_str() {
                "minimal" => return true,
                "full" => return self.minimal_overhead_effective(),
                _ => {}
            }
        }

        if self.minimal_overhead_effective() {
            return true;
        }

        let client_lower = client_name.trim().to_lowercase();
        if !client_lower.is_empty() {
            if let Ok(list) = std::env::var("LEAN_CTX_MINIMAL_CLIENTS") {
                for needle in list.split(',').map(|s| s.trim().to_lowercase()) {
                    if !needle.is_empty() && client_lower.contains(&needle) {
                        return true;
                    }
                }
            } else if client_lower.contains("hermes") || client_lower.contains("minimax") {
                return true;
            }
        }

        let model = std::env::var("LEAN_CTX_MODEL")
            .or_else(|_| std::env::var("LCTX_MODEL"))
            .unwrap_or_default();
        let model = model.trim().to_lowercase();
        if !model.is_empty() {
            let m = model.replace(['_', ' '], "-");
            if m.contains("minimax")
                || m.contains("mini-max")
                || m.contains("m2.7")
                || m.contains("m2-7")
            {
                return true;
            }
        }

        false
    }

    /// Returns `true` if shell hook injection is disabled via env var or config.
    pub fn shell_hook_disabled_effective(&self) -> bool {
        std::env::var("LEAN_CTX_NO_HOOK").is_ok() || self.shell_hook_disabled
    }

    /// Returns the effective shell activation mode (env var > config > default).
    pub fn shell_activation_effective(&self) -> ShellActivation {
        ShellActivation::effective(self)
    }

    /// Returns `true` if the daily update check is disabled via env var or config.
    pub fn update_check_disabled_effective(&self) -> bool {
        std::env::var("LEAN_CTX_NO_UPDATE_CHECK").is_ok() || self.update_check_disabled
    }

    pub fn memory_policy_effective(&self) -> Result<MemoryPolicy, String> {
        let mut policy = self.memory.clone();
        policy.apply_env_overrides();
        policy.validate()?;
        Ok(policy)
    }
}

#[cfg(test)]
mod disabled_tools_tests {
    use super::*;

    #[test]
    fn config_field_default_is_empty() {
        let cfg = Config::default();
        assert!(cfg.disabled_tools.is_empty());
    }

    #[test]
    fn effective_returns_config_field_when_no_env_var() {
        // Only meaningful when LEAN_CTX_DISABLED_TOOLS is unset; skip otherwise.
        if std::env::var("LEAN_CTX_DISABLED_TOOLS").is_ok() {
            return;
        }
        let cfg = Config {
            disabled_tools: vec!["ctx_graph".to_string(), "ctx_agent".to_string()],
            ..Default::default()
        };
        assert_eq!(
            cfg.disabled_tools_effective(),
            vec!["ctx_graph", "ctx_agent"]
        );
    }

    #[test]
    fn parse_env_basic() {
        let result = Config::parse_disabled_tools_env("ctx_graph,ctx_agent");
        assert_eq!(result, vec!["ctx_graph", "ctx_agent"]);
    }

    #[test]
    fn parse_env_trims_whitespace_and_skips_empty() {
        let result = Config::parse_disabled_tools_env(" ctx_graph , , ctx_agent ");
        assert_eq!(result, vec!["ctx_graph", "ctx_agent"]);
    }

    #[test]
    fn parse_env_single_entry() {
        let result = Config::parse_disabled_tools_env("ctx_graph");
        assert_eq!(result, vec!["ctx_graph"]);
    }

    #[test]
    fn parse_env_empty_string_returns_empty() {
        let result = Config::parse_disabled_tools_env("");
        assert!(result.is_empty());
    }

    #[test]
    fn disabled_tools_deserialization_defaults_to_empty() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.disabled_tools.is_empty());
    }

    #[test]
    fn disabled_tools_deserialization_from_toml() {
        let cfg: Config = toml::from_str(r#"disabled_tools = ["ctx_graph", "ctx_agent"]"#).unwrap();
        assert_eq!(cfg.disabled_tools, vec!["ctx_graph", "ctx_agent"]);
    }
}

#[cfg(test)]
mod rules_scope_tests {
    use super::*;

    #[test]
    fn default_is_both() {
        let cfg = Config::default();
        assert_eq!(cfg.rules_scope_effective(), RulesScope::Both);
    }

    #[test]
    fn config_global() {
        let cfg = Config {
            rules_scope: Some("global".to_string()),
            ..Default::default()
        };
        assert_eq!(cfg.rules_scope_effective(), RulesScope::Global);
    }

    #[test]
    fn config_project() {
        let cfg = Config {
            rules_scope: Some("project".to_string()),
            ..Default::default()
        };
        assert_eq!(cfg.rules_scope_effective(), RulesScope::Project);
    }

    #[test]
    fn unknown_value_falls_back_to_both() {
        let cfg = Config {
            rules_scope: Some("nonsense".to_string()),
            ..Default::default()
        };
        assert_eq!(cfg.rules_scope_effective(), RulesScope::Both);
    }

    #[test]
    fn deserialization_none_by_default() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.rules_scope.is_none());
        assert_eq!(cfg.rules_scope_effective(), RulesScope::Both);
    }

    #[test]
    fn deserialization_from_toml() {
        let cfg: Config = toml::from_str(r#"rules_scope = "project""#).unwrap();
        assert_eq!(cfg.rules_scope.as_deref(), Some("project"));
        assert_eq!(cfg.rules_scope_effective(), RulesScope::Project);
    }
}

#[cfg(test)]
mod loop_detection_config_tests {
    use super::*;

    #[test]
    fn defaults_are_reasonable() {
        let cfg = LoopDetectionConfig::default();
        assert_eq!(cfg.normal_threshold, 2);
        assert_eq!(cfg.reduced_threshold, 4);
        // 0 = blocking disabled by default (LeanCTX philosophy: always help, never block)
        assert_eq!(cfg.blocked_threshold, 0);
        assert_eq!(cfg.window_secs, 300);
        assert_eq!(cfg.search_group_limit, 10);
    }

    #[test]
    fn deserialization_defaults_when_missing() {
        let cfg: Config = toml::from_str("").unwrap();
        // 0 = blocking disabled by default
        assert_eq!(cfg.loop_detection.blocked_threshold, 0);
        assert_eq!(cfg.loop_detection.search_group_limit, 10);
    }

    #[test]
    fn deserialization_from_toml() {
        let cfg: Config = toml::from_str(
            r"
            [loop_detection]
            normal_threshold = 1
            reduced_threshold = 3
            blocked_threshold = 5
            window_secs = 120
            search_group_limit = 8
            ",
        )
        .unwrap();
        assert_eq!(cfg.loop_detection.normal_threshold, 1);
        assert_eq!(cfg.loop_detection.reduced_threshold, 3);
        assert_eq!(cfg.loop_detection.blocked_threshold, 5);
        assert_eq!(cfg.loop_detection.window_secs, 120);
        assert_eq!(cfg.loop_detection.search_group_limit, 8);
    }

    #[test]
    fn partial_override_keeps_defaults() {
        let cfg: Config = toml::from_str(
            r"
            [loop_detection]
            blocked_threshold = 10
            ",
        )
        .unwrap();
        assert_eq!(cfg.loop_detection.blocked_threshold, 10);
        assert_eq!(cfg.loop_detection.normal_threshold, 2);
        assert_eq!(cfg.loop_detection.search_group_limit, 10);
    }
}

impl Config {
    /// Returns the path to the global config file (`~/.lean-ctx/config.toml`).
    pub fn path() -> Option<PathBuf> {
        crate::core::data_dir::lean_ctx_data_dir()
            .ok()
            .map(|d| d.join("config.toml"))
    }

    /// Returns the path to the project-local config override file.
    pub fn local_path(project_root: &str) -> PathBuf {
        PathBuf::from(project_root).join(".lean-ctx.toml")
    }

    fn find_project_root() -> Option<String> {
        if let Ok(env_root) = std::env::var("LEAN_CTX_PROJECT_ROOT") {
            if !env_root.is_empty() {
                return Some(env_root);
            }
        }

        let cwd = std::env::current_dir().ok();

        if let Some(root) =
            crate::core::session::SessionState::load_latest().and_then(|s| s.project_root)
        {
            let root_path = std::path::Path::new(&root);
            let cwd_is_under_root = cwd.as_ref().is_some_and(|c| c.starts_with(root_path));
            let has_marker = root_path.join(".git").exists()
                || root_path.join("Cargo.toml").exists()
                || root_path.join("package.json").exists()
                || root_path.join("go.mod").exists()
                || root_path.join("pyproject.toml").exists()
                || root_path.join(".lean-ctx.toml").exists();

            if cwd_is_under_root || has_marker {
                return Some(root);
            }
        }

        if let Some(ref cwd) = cwd {
            let git_root = std::process::Command::new("git")
                .args(["rev-parse", "--show-toplevel"])
                .current_dir(cwd)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .output()
                .ok()
                .and_then(|o| {
                    if o.status.success() {
                        String::from_utf8(o.stdout)
                            .ok()
                            .map(|s| s.trim().to_string())
                    } else {
                        None
                    }
                });
            if let Some(root) = git_root {
                return Some(root);
            }
            return Some(cwd.to_string_lossy().to_string());
        }
        None
    }

    /// Loads config from disk with caching, merging global + project-local overrides.
    pub fn load() -> Self {
        static CACHE: Mutex<Option<(Config, SystemTime, Option<SystemTime>)>> = Mutex::new(None);

        let Some(path) = Self::path() else {
            return Self::default();
        };

        let local_path = Self::find_project_root().map(|r| Self::local_path(&r));

        let mtime = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);

        let local_mtime = local_path
            .as_ref()
            .and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());

        if let Ok(guard) = CACHE.lock() {
            if let Some((ref cfg, ref cached_mtime, ref cached_local_mtime)) = *guard {
                if *cached_mtime == mtime && *cached_local_mtime == local_mtime {
                    return cfg.clone();
                }
            }
        }

        let mut cfg: Config = match std::fs::read_to_string(&path) {
            Ok(content) => match toml::from_str(&content) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("config parse error in {}: {e}", path.display());
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        };

        if let Some(ref lp) = local_path {
            if let Ok(local_content) = std::fs::read_to_string(lp) {
                cfg.merge_local(&local_content);
            }
        }

        if let Ok(mut guard) = CACHE.lock() {
            *guard = Some((cfg.clone(), mtime, local_mtime));
        }

        cfg
    }

    fn merge_local(&mut self, local_toml: &str) {
        let local: Config = match toml::from_str(local_toml) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("local config parse error: {e}");
                return;
            }
        };
        if local.ultra_compact {
            self.ultra_compact = true;
        }
        if local.tee_mode != TeeMode::default() {
            self.tee_mode = local.tee_mode;
        }
        if local.output_density != OutputDensity::default() {
            self.output_density = local.output_density;
        }
        if local.checkpoint_interval != 15 {
            self.checkpoint_interval = local.checkpoint_interval;
        }
        if !local.excluded_commands.is_empty() {
            self.excluded_commands.extend(local.excluded_commands);
        }
        if !local.passthrough_urls.is_empty() {
            self.passthrough_urls.extend(local.passthrough_urls);
        }
        if !local.custom_aliases.is_empty() {
            self.custom_aliases.extend(local.custom_aliases);
        }
        if local.slow_command_threshold_ms != 5000 {
            self.slow_command_threshold_ms = local.slow_command_threshold_ms;
        }
        if local.theme != "default" {
            self.theme = local.theme;
        }
        if !local.buddy_enabled {
            self.buddy_enabled = false;
        }
        if !local.redirect_exclude.is_empty() {
            self.redirect_exclude.extend(local.redirect_exclude);
        }
        if !local.disabled_tools.is_empty() {
            self.disabled_tools.extend(local.disabled_tools);
        }
        if !local.extra_ignore_patterns.is_empty() {
            self.extra_ignore_patterns
                .extend(local.extra_ignore_patterns);
        }
        if local.rules_scope.is_some() {
            self.rules_scope = local.rules_scope;
        }
        if local.proxy.anthropic_upstream.is_some() {
            self.proxy.anthropic_upstream = local.proxy.anthropic_upstream;
        }
        if local.proxy.openai_upstream.is_some() {
            self.proxy.openai_upstream = local.proxy.openai_upstream;
        }
        if local.proxy.gemini_upstream.is_some() {
            self.proxy.gemini_upstream = local.proxy.gemini_upstream;
        }
        if !local.autonomy.enabled {
            self.autonomy.enabled = false;
        }
        if !local.autonomy.auto_preload {
            self.autonomy.auto_preload = false;
        }
        if !local.autonomy.auto_dedup {
            self.autonomy.auto_dedup = false;
        }
        if !local.autonomy.auto_related {
            self.autonomy.auto_related = false;
        }
        if !local.autonomy.auto_consolidate {
            self.autonomy.auto_consolidate = false;
        }
        if local.autonomy.silent_preload {
            self.autonomy.silent_preload = true;
        }
        if !local.autonomy.silent_preload && self.autonomy.silent_preload {
            self.autonomy.silent_preload = false;
        }
        if local.autonomy.dedup_threshold != AutonomyConfig::default().dedup_threshold {
            self.autonomy.dedup_threshold = local.autonomy.dedup_threshold;
        }
        if local.autonomy.consolidate_every_calls
            != AutonomyConfig::default().consolidate_every_calls
        {
            self.autonomy.consolidate_every_calls = local.autonomy.consolidate_every_calls;
        }
        if local.autonomy.consolidate_cooldown_secs
            != AutonomyConfig::default().consolidate_cooldown_secs
        {
            self.autonomy.consolidate_cooldown_secs = local.autonomy.consolidate_cooldown_secs;
        }
        if local_toml.contains("compression_level") {
            self.compression_level = local.compression_level;
        }
        if local_toml.contains("terse_agent") {
            self.terse_agent = local.terse_agent;
        }
        if !local.archive.enabled {
            self.archive.enabled = false;
        }
        if local.archive.threshold_chars != ArchiveConfig::default().threshold_chars {
            self.archive.threshold_chars = local.archive.threshold_chars;
        }
        if local.archive.max_age_hours != ArchiveConfig::default().max_age_hours {
            self.archive.max_age_hours = local.archive.max_age_hours;
        }
        if local.archive.max_disk_mb != ArchiveConfig::default().max_disk_mb {
            self.archive.max_disk_mb = local.archive.max_disk_mb;
        }
        let mem_def = MemoryPolicy::default();
        if local.memory.knowledge.max_facts != mem_def.knowledge.max_facts {
            self.memory.knowledge.max_facts = local.memory.knowledge.max_facts;
        }
        if local.memory.knowledge.max_patterns != mem_def.knowledge.max_patterns {
            self.memory.knowledge.max_patterns = local.memory.knowledge.max_patterns;
        }
        if local.memory.knowledge.max_history != mem_def.knowledge.max_history {
            self.memory.knowledge.max_history = local.memory.knowledge.max_history;
        }
        if local.memory.knowledge.contradiction_threshold
            != mem_def.knowledge.contradiction_threshold
        {
            self.memory.knowledge.contradiction_threshold =
                local.memory.knowledge.contradiction_threshold;
        }

        if local.memory.episodic.max_episodes != mem_def.episodic.max_episodes {
            self.memory.episodic.max_episodes = local.memory.episodic.max_episodes;
        }
        if local.memory.episodic.max_actions_per_episode != mem_def.episodic.max_actions_per_episode
        {
            self.memory.episodic.max_actions_per_episode =
                local.memory.episodic.max_actions_per_episode;
        }
        if local.memory.episodic.summary_max_chars != mem_def.episodic.summary_max_chars {
            self.memory.episodic.summary_max_chars = local.memory.episodic.summary_max_chars;
        }

        if local.memory.procedural.min_repetitions != mem_def.procedural.min_repetitions {
            self.memory.procedural.min_repetitions = local.memory.procedural.min_repetitions;
        }
        if local.memory.procedural.min_sequence_len != mem_def.procedural.min_sequence_len {
            self.memory.procedural.min_sequence_len = local.memory.procedural.min_sequence_len;
        }
        if local.memory.procedural.max_procedures != mem_def.procedural.max_procedures {
            self.memory.procedural.max_procedures = local.memory.procedural.max_procedures;
        }
        if local.memory.procedural.max_window_size != mem_def.procedural.max_window_size {
            self.memory.procedural.max_window_size = local.memory.procedural.max_window_size;
        }

        if local.memory.lifecycle.decay_rate != mem_def.lifecycle.decay_rate {
            self.memory.lifecycle.decay_rate = local.memory.lifecycle.decay_rate;
        }
        if local.memory.lifecycle.low_confidence_threshold
            != mem_def.lifecycle.low_confidence_threshold
        {
            self.memory.lifecycle.low_confidence_threshold =
                local.memory.lifecycle.low_confidence_threshold;
        }
        if local.memory.lifecycle.stale_days != mem_def.lifecycle.stale_days {
            self.memory.lifecycle.stale_days = local.memory.lifecycle.stale_days;
        }
        if local.memory.lifecycle.similarity_threshold != mem_def.lifecycle.similarity_threshold {
            self.memory.lifecycle.similarity_threshold =
                local.memory.lifecycle.similarity_threshold;
        }

        if local.memory.embeddings.max_facts != mem_def.embeddings.max_facts {
            self.memory.embeddings.max_facts = local.memory.embeddings.max_facts;
        }
        if !local.allow_paths.is_empty() {
            self.allow_paths.extend(local.allow_paths);
        }
        if local.minimal_overhead {
            self.minimal_overhead = true;
        }
        if local.shell_hook_disabled {
            self.shell_hook_disabled = true;
        }
        if local.shell_activation != ShellActivation::default() {
            self.shell_activation = local.shell_activation.clone();
        }
        if local.bm25_max_cache_mb != default_bm25_max_cache_mb() {
            self.bm25_max_cache_mb = local.bm25_max_cache_mb;
        }
        if local.memory_profile != MemoryProfile::default() {
            self.memory_profile = local.memory_profile;
        }
        if local.memory_cleanup != MemoryCleanup::default() {
            self.memory_cleanup = local.memory_cleanup;
        }
    }

    /// Persists the current config to the global config file.
    pub fn save(&self) -> std::result::Result<(), super::error::LeanCtxError> {
        let path = Self::path().ok_or_else(|| {
            super::error::LeanCtxError::Config("cannot determine home directory".into())
        })?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)
            .map_err(|e| super::error::LeanCtxError::Config(e.to_string()))?;
        std::fs::write(&path, content)?;
        Ok(())
    }

    /// Formats the current config as a human-readable string with file paths.
    pub fn show(&self) -> String {
        let global_path = Self::path().map_or_else(
            || "~/.lean-ctx/config.toml".to_string(),
            |p| p.to_string_lossy().to_string(),
        );
        let content = toml::to_string_pretty(self).unwrap_or_default();
        let mut out = format!("Global config: {global_path}\n\n{content}");

        if let Some(root) = Self::find_project_root() {
            let local = Self::local_path(&root);
            if local.exists() {
                out.push_str(&format!("\n\nLocal config (merged): {}\n", local.display()));
            } else {
                out.push_str(&format!(
                    "\n\nLocal config: not found (create {} to override per-project)\n",
                    local.display()
                ));
            }
        }
        out
    }
}

#[cfg(test)]
mod compression_level_tests {
    use super::*;

    #[test]
    fn default_is_off() {
        assert_eq!(CompressionLevel::default(), CompressionLevel::Off);
    }

    #[test]
    fn to_components_off() {
        let (ta, od, crp, tm) = CompressionLevel::Off.to_components();
        assert_eq!(ta, TerseAgent::Off);
        assert_eq!(od, OutputDensity::Normal);
        assert_eq!(crp, "off");
        assert!(!tm);
    }

    #[test]
    fn to_components_lite() {
        let (ta, od, crp, tm) = CompressionLevel::Lite.to_components();
        assert_eq!(ta, TerseAgent::Lite);
        assert_eq!(od, OutputDensity::Terse);
        assert_eq!(crp, "off");
        assert!(tm);
    }

    #[test]
    fn to_components_standard() {
        let (ta, od, crp, tm) = CompressionLevel::Standard.to_components();
        assert_eq!(ta, TerseAgent::Full);
        assert_eq!(od, OutputDensity::Terse);
        assert_eq!(crp, "compact");
        assert!(tm);
    }

    #[test]
    fn to_components_max() {
        let (ta, od, crp, tm) = CompressionLevel::Max.to_components();
        assert_eq!(ta, TerseAgent::Ultra);
        assert_eq!(od, OutputDensity::Ultra);
        assert_eq!(crp, "tdd");
        assert!(tm);
    }

    #[test]
    fn from_legacy_ultra_agent_maps_to_max() {
        assert_eq!(
            CompressionLevel::from_legacy(&TerseAgent::Ultra, &OutputDensity::Normal),
            CompressionLevel::Max
        );
    }

    #[test]
    fn from_legacy_ultra_density_maps_to_max() {
        assert_eq!(
            CompressionLevel::from_legacy(&TerseAgent::Off, &OutputDensity::Ultra),
            CompressionLevel::Max
        );
    }

    #[test]
    fn from_legacy_full_agent_maps_to_standard() {
        assert_eq!(
            CompressionLevel::from_legacy(&TerseAgent::Full, &OutputDensity::Normal),
            CompressionLevel::Standard
        );
    }

    #[test]
    fn from_legacy_lite_agent_maps_to_lite() {
        assert_eq!(
            CompressionLevel::from_legacy(&TerseAgent::Lite, &OutputDensity::Normal),
            CompressionLevel::Lite
        );
    }

    #[test]
    fn from_legacy_terse_density_maps_to_lite() {
        assert_eq!(
            CompressionLevel::from_legacy(&TerseAgent::Off, &OutputDensity::Terse),
            CompressionLevel::Lite
        );
    }

    #[test]
    fn from_legacy_both_off_maps_to_off() {
        assert_eq!(
            CompressionLevel::from_legacy(&TerseAgent::Off, &OutputDensity::Normal),
            CompressionLevel::Off
        );
    }

    #[test]
    fn labels_match() {
        assert_eq!(CompressionLevel::Off.label(), "off");
        assert_eq!(CompressionLevel::Lite.label(), "lite");
        assert_eq!(CompressionLevel::Standard.label(), "standard");
        assert_eq!(CompressionLevel::Max.label(), "max");
    }

    #[test]
    fn is_active_false_for_off() {
        assert!(!CompressionLevel::Off.is_active());
    }

    #[test]
    fn is_active_true_for_all_others() {
        assert!(CompressionLevel::Lite.is_active());
        assert!(CompressionLevel::Standard.is_active());
        assert!(CompressionLevel::Max.is_active());
    }

    #[test]
    fn deserialization_defaults_to_off() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.compression_level, CompressionLevel::Off);
    }

    #[test]
    fn deserialization_from_toml() {
        let cfg: Config = toml::from_str(r#"compression_level = "standard""#).unwrap();
        assert_eq!(cfg.compression_level, CompressionLevel::Standard);
    }

    #[test]
    fn roundtrip_all_levels() {
        for level in [
            CompressionLevel::Off,
            CompressionLevel::Lite,
            CompressionLevel::Standard,
            CompressionLevel::Max,
        ] {
            let (ta, od, crp, tm) = level.to_components();
            assert!(!crp.is_empty());
            if level == CompressionLevel::Off {
                assert!(!tm);
                assert_eq!(ta, TerseAgent::Off);
                assert_eq!(od, OutputDensity::Normal);
            } else {
                assert!(tm);
            }
        }
    }
}

#[cfg(test)]
mod memory_cleanup_tests {
    use super::*;

    #[test]
    fn default_is_aggressive() {
        assert_eq!(MemoryCleanup::default(), MemoryCleanup::Aggressive);
    }

    #[test]
    fn aggressive_ttl_is_300() {
        assert_eq!(MemoryCleanup::Aggressive.idle_ttl_secs(), 300);
    }

    #[test]
    fn shared_ttl_is_1800() {
        assert_eq!(MemoryCleanup::Shared.idle_ttl_secs(), 1800);
    }

    #[test]
    fn index_retention_multiplier_values() {
        assert!(
            (MemoryCleanup::Aggressive.index_retention_multiplier() - 1.0).abs() < f64::EPSILON
        );
        assert!((MemoryCleanup::Shared.index_retention_multiplier() - 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn deserialization_defaults_to_aggressive() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.memory_cleanup, MemoryCleanup::Aggressive);
    }

    #[test]
    fn deserialization_from_toml() {
        let cfg: Config = toml::from_str(r#"memory_cleanup = "shared""#).unwrap();
        assert_eq!(cfg.memory_cleanup, MemoryCleanup::Shared);
    }

    #[test]
    fn effective_uses_config_when_no_env() {
        let cfg = Config {
            memory_cleanup: MemoryCleanup::Shared,
            ..Default::default()
        };
        let eff = MemoryCleanup::effective(&cfg);
        assert_eq!(eff, MemoryCleanup::Shared);
    }
}
