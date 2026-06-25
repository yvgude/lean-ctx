//! Auxiliary configuration section structs.
//!
//! Nested config structs (secret-detection, setup, archive, providers,
//! autonomy, updates, cloud, gain, loop-detection, embedding, …) split out of
//! `config/mod.rs` to keep the top-level module focused on `Config` itself.
//! Re-exported via `pub use sections::*`, so external paths stay stable.

use super::serde_defaults;
#[allow(clippy::wildcard_imports)]
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SecretDetectionConfig {
    pub enabled: bool,
    pub redact: bool,
    pub custom_patterns: Vec<String>,
}

/// Controls what lean-ctx injects during `setup` and `update --rewire`.
/// Fresh installs default to non-invasive (rules/skills off, MCP on).
/// Users who ran setup interactively get explicit true/false.
/// `None` = undecided (legacy: check if rules already exist and preserve behavior).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SetupConfig {
    /// Inject agent rule files (CLAUDE.md, .cursor/rules/, etc.).
    /// None = undecided (legacy compat: inject if rules already present).
    /// Some(true) = always inject. Some(false) = never inject.
    pub auto_inject_rules: Option<bool>,
    /// Install SKILL.md files for supported agents.
    /// None = undecided. Some(true) = install. Some(false) = skip.
    pub auto_inject_skills: Option<bool>,
    /// Register lean-ctx as an MCP server in editor configs.
    #[serde(default = "serde_defaults::default_true")]
    pub auto_update_mcp: bool,
}

impl Default for SetupConfig {
    fn default() -> Self {
        Self {
            auto_inject_rules: None,
            auto_inject_skills: None,
            auto_update_mcp: true,
        }
    }
}

impl SetupConfig {
    /// Returns whether rules should be injected, considering legacy installs.
    /// If undecided (None), checks if lean-ctx rules markers already exist
    /// in any agent config — if so, keeps injecting for backward compat.
    #[must_use]
    pub fn should_inject_rules(&self) -> bool {
        match self.auto_inject_rules {
            Some(v) => v,
            None => Self::rules_already_present(),
        }
    }

    /// Returns whether skills should be installed.
    #[must_use]
    pub fn should_inject_skills(&self) -> bool {
        match self.auto_inject_skills {
            Some(v) => v,
            None => Self::rules_already_present(),
        }
    }

    /// Returns whether `setup`/`onboard`/`init` may (re)register the lean-ctx
    /// MCP server in editor configs. Honors `auto_update_mcp` (#281) so locked-
    /// down environments can keep MCP out of agent settings while still getting
    /// hooks, rules and skills.
    #[must_use]
    pub fn should_update_mcp(&self) -> bool {
        self.auto_update_mcp
    }

    /// Check if lean-ctx rules markers exist in any known agent config location.
    ///
    /// Delegates the per-agent path catalog to `rules_inject::any_rules_marker_present`
    /// (derived from the injector's own target list) so this never drifts behind
    /// newly supported agents again (#442). Claude Code and `CodeBuddy` have no
    /// rules *target* (they auto-load an inline block instead), so their legacy
    /// rule files are checked separately to keep honoring older installs.
    fn rules_already_present() -> bool {
        let Some(home) = dirs::home_dir() else {
            return false;
        };
        if crate::rules_inject::any_rules_marker_present(&home) {
            return true;
        }
        let legacy_paths = [
            crate::core::editor_registry::claude_rules_dir(&home).join("lean-ctx.md"),
            crate::core::editor_registry::codebuddy_rules_dir(&home).join("lean-ctx.md"),
        ];
        legacy_paths.iter().any(|p| {
            std::fs::read_to_string(p)
                .is_ok_and(|c| c.contains(crate::core::rules_canonical::START_MARK))
        })
    }
}

impl Default for SecretDetectionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            redact: true,
            custom_patterns: Vec::new(),
        }
    }
}

/// Settings for the zero-loss compression archive (large tool outputs saved to disk).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ArchiveConfig {
    pub enabled: bool,
    pub threshold_chars: usize,
    pub max_age_hours: u64,
    pub max_disk_mb: u64,
    pub ephemeral: bool,
    /// Minimum output tokens before the ephemeral firewall replaces an inline tool
    /// result with a summary + retrieval ref. Outputs below this stay fully inline.
    pub ephemeral_min_tokens: usize,
}

impl Default for ArchiveConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold_chars: 800,
            max_age_hours: 48,
            max_disk_mb: 500,
            ephemeral: true,
            ephemeral_min_tokens: 2000,
        }
    }
}

impl ArchiveConfig {
    #[must_use]
    pub fn ephemeral_effective(&self) -> bool {
        if let Ok(v) = std::env::var("LEAN_CTX_EPHEMERAL") {
            return !matches!(v.trim(), "0" | "false" | "off");
        }
        self.ephemeral && self.enabled
    }

    #[must_use]
    pub fn ephemeral_min_tokens_effective(&self) -> usize {
        if let Ok(v) = std::env::var("LEAN_CTX_EPHEMERAL_MIN_TOKENS")
            && let Ok(n) = v.trim().parse::<usize>()
        {
            return n;
        }
        self.ephemeral_min_tokens
    }
}

/// Configuration for external context providers (GitHub, GitLab, Jira, etc.).
/// Each provider can be enabled/disabled and configured with auth tokens.
/// Override individual tokens via env vars (`GITHUB_TOKEN`, `GITLAB_TOKEN`, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProvidersConfig {
    /// Master switch for the provider subsystem.
    pub enabled: bool,
    /// GitHub provider configuration.
    pub github: ProviderEntryConfig,
    /// GitLab provider configuration.
    pub gitlab: ProviderEntryConfig,
    /// Auto-ingest provider results into BM25/embedding indexes.
    pub auto_index: bool,
    /// Default cache TTL for provider results (seconds).
    pub cache_ttl_secs: u64,
    /// MCP Bridge providers: `{ "name" = { url = "...", description = "..." } }`.
    #[serde(default)]
    pub mcp_bridges: std::collections::HashMap<String, McpBridgeEntry>,
}

impl Default for ProvidersConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            github: ProviderEntryConfig::default(),
            gitlab: ProviderEntryConfig::default(),
            auto_index: true,
            cache_ttl_secs: 120,
            mcp_bridges: std::collections::HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpBridgeEntry {
    /// HTTP/SSE URL for remote MCP servers.
    #[serde(default)]
    pub url: Option<String>,
    /// Command to spawn a local MCP server (stdio transport).
    #[serde(default)]
    pub command: Option<String>,
    /// Arguments for the command.
    #[serde(default)]
    pub args: Vec<String>,
    /// Human-readable description.
    #[serde(default)]
    pub description: Option<String>,
    /// Environment variable name containing an auth token.
    #[serde(default)]
    pub auth_env: Option<String>,
}

/// Per-provider configuration entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderEntryConfig {
    /// Whether this specific provider is enabled.
    pub enabled: bool,
    /// Auth token (prefer env var; only use this for project-local overrides).
    pub token: Option<String>,
    /// API base URL override (for GitHub Enterprise, self-hosted GitLab, etc.).
    pub api_url: Option<String>,
    /// Default project/repo for this provider (auto-detected from git remote if empty).
    pub project: Option<String>,
}

impl Default for ProviderEntryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            token: None,
            api_url: None,
            project: None,
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
    #[serde(default = "serde_defaults::default_true")]
    pub cognition_loop_enabled: bool,
    #[serde(default = "serde_defaults::default_cognition_loop_interval")]
    pub cognition_loop_interval_secs: u64,
    #[serde(default = "serde_defaults::default_cognition_loop_max_steps")]
    pub cognition_loop_max_steps: u8,
    /// Minimum facts an entity needs before observation synthesis (#802) writes a
    /// summary. Synthesis itself is gated by `cognition_loop_max_steps >= 9`.
    #[serde(default = "serde_defaults::default_cognition_synthesis_min_cluster")]
    pub cognition_synthesis_min_cluster: usize,
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
            cognition_loop_enabled: true,
            cognition_loop_interval_secs: 3600,
            cognition_loop_max_steps: 9,
            cognition_synthesis_min_cluster: 3,
        }
    }
}

/// Controls automatic update behavior. All defaults are OFF — auto-updates
/// require explicit opt-in via `lean-ctx setup` or `lean-ctx update --schedule`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdatesConfig {
    pub auto_update: bool,
    pub check_interval_hours: u64,
    pub notify_only: bool,
}

impl Default for UpdatesConfig {
    fn default() -> Self {
        Self {
            auto_update: false,
            check_interval_hours: 6,
            notify_only: false,
        }
    }
}

impl UpdatesConfig {
    #[must_use]
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Ok(v) = std::env::var("LEAN_CTX_AUTO_UPDATE") {
            cfg.auto_update = v == "1" || v.eq_ignore_ascii_case("true");
        }
        if let Ok(v) = std::env::var("LEAN_CTX_UPDATE_INTERVAL_HOURS")
            && let Ok(h) = v.parse::<u64>()
        {
            cfg.check_interval_hours = h.clamp(1, 168);
        }
        if let Ok(v) = std::env::var("LEAN_CTX_UPDATE_NOTIFY_ONLY") {
            cfg.notify_only = v == "1" || v.eq_ignore_ascii_case("true");
        }
        cfg
    }
}

impl AutonomyConfig {
    /// Creates an autonomy config from env vars, falling back to defaults.
    #[must_use]
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Ok(v) = std::env::var("LEAN_CTX_AUTONOMY")
            && (v == "false" || v == "0")
        {
            cfg.enabled = false;
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
        if let Ok(v) = std::env::var("LEAN_CTX_DEDUP_THRESHOLD")
            && let Ok(n) = v.parse()
        {
            cfg.dedup_threshold = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_CONSOLIDATE_EVERY_CALLS")
            && let Ok(n) = v.parse()
        {
            cfg.consolidate_every_calls = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_CONSOLIDATE_COOLDOWN_SECS")
            && let Ok(n) = v.parse()
        {
            cfg.consolidate_cooldown_secs = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_COGNITION_LOOP_ENABLED") {
            cfg.cognition_loop_enabled = v != "false" && v != "0";
        }
        if let Ok(v) = std::env::var("LEAN_CTX_COGNITION_LOOP_INTERVAL_SECS")
            && let Ok(n) = v.parse()
        {
            cfg.cognition_loop_interval_secs = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_COGNITION_LOOP_MAX_STEPS")
            && let Ok(n) = v.parse()
        {
            cfg.cognition_loop_max_steps = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_COGNITION_SYNTHESIS_MIN_CLUSTER")
            && let Ok(n) = v.parse()
        {
            cfg.cognition_synthesis_min_cluster = n;
        }
        cfg
    }

    /// Loads autonomy config from disk, with env var overrides applied.
    #[must_use]
    pub fn load() -> Self {
        let file_cfg = Config::load().autonomy;
        let mut cfg = file_cfg;
        if let Ok(v) = std::env::var("LEAN_CTX_AUTONOMY")
            && (v == "false" || v == "0")
        {
            cfg.enabled = false;
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
        if let Ok(v) = std::env::var("LEAN_CTX_DEDUP_THRESHOLD")
            && let Ok(n) = v.parse()
        {
            cfg.dedup_threshold = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_COGNITION_LOOP_ENABLED") {
            cfg.cognition_loop_enabled = v != "false" && v != "0";
        }
        if let Ok(v) = std::env::var("LEAN_CTX_COGNITION_LOOP_INTERVAL_SECS")
            && let Ok(n) = v.parse()
        {
            cfg.cognition_loop_interval_secs = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_COGNITION_LOOP_MAX_STEPS")
            && let Ok(n) = v.parse()
        {
            cfg.cognition_loop_max_steps = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_COGNITION_SYNTHESIS_MIN_CLUSTER")
            && let Ok(n) = v.parse()
        {
            cfg.cognition_synthesis_min_cluster = n;
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
    /// Auto-push the Pro Personal-Cloud surfaces (knowledge, commands, CEP,
    /// gotchas, buddy, feedback) from the background task — opt-in, once per
    /// day, offline-tolerant (GL #384). Toggle: `lean-ctx cloud autosync on`.
    pub auto_sync: bool,
    pub last_auto_sync: Option<String>,
    /// Auto-push the project's encrypted retrieval-index bundle (hosted
    /// Personal Index, GL #392) alongside the daily auto-sync — separate
    /// opt-in because index bundles are orders of magnitude larger than the
    /// other surfaces. Toggle: `lean-ctx cloud autoindex on`.
    pub auto_index: bool,
    /// Per-project debounce: `project_hash → YYYY-MM-DD` of the last
    /// successful background index push.
    pub last_index_push: std::collections::HashMap<String, String>,
}

/// Settings for publishing your token-savings recap (`gain --publish` / auto-publish).
///
/// Publishing is always opt-in: it sends a small, whitelisted *aggregate* payload (tokens
/// saved, $ avoided, compression % — never code, paths or counts) to the cloud.
/// `auto_publish` simply removes the need to re-run `gain --publish` by hand; it stays off
/// until the user explicitly enables it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GainConfig {
    /// When true, `lean-ctx gain` automatically (re)publishes the recap, throttled to
    /// `auto_publish_interval_hours`. Off by default.
    pub auto_publish: bool,
    /// When auto-publishing, also opt into the public leaderboard.
    pub leaderboard: bool,
    /// Optional display name for the published card / leaderboard entry.
    pub display_name: Option<String>,
    /// Minimum hours between automatic publishes (throttle).
    pub auto_publish_interval_hours: u64,
    /// Runtime state — RFC3339 timestamp of the last automatic publish. Managed by the
    /// tool, not meant to be set by hand.
    pub last_auto_publish: Option<String>,
}

impl Default for GainConfig {
    fn default() -> Self {
        Self {
            auto_publish: false,
            leaderboard: true,
            display_name: None,
            auto_publish_interval_hours: 24,
            last_auto_publish: None,
        }
    }
}

/// Model declaration for **measured-vs-estimated** cost reporting.
///
/// Proxy-routed clients (Claude Code, Codex, Pi, Gemini CLI, `OpenCode`) report
/// their real model and billed tokens, so lean-ctx prices them *measured* with
/// no configuration. MCP-only IDEs (Cursor, Copilot, Windsurf, VS Code, Zed)
/// send their LLM traffic straight to the provider, bypassing lean-ctx — their
/// real model is invisible. Declaring it here lets those *estimated* turns be
/// priced with the correct model instead of a blended fallback.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CostConfig {
    /// Fallback pricing model for any client without a per-client entry.
    /// Unset/empty → lean-ctx keeps its blended heuristic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// Per-client pricing model, keyed by client id (`cursor`, `copilot`,
    /// `windsurf`, `claude`, `codex`, …). Used for MCP-only IDEs whose real
    /// model lean-ctx cannot observe. Example:
    /// `[cost.models]` then `cursor = "claude-opus-4.5"`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub models: HashMap<String, String>,
}

impl CostConfig {
    /// Configured pricing model for a client id: the per-client entry first, then
    /// the global default. `None` when neither is set (the caller then falls back
    /// to the env override / heuristic). Blank entries are ignored.
    #[must_use]
    pub fn model_for_client(&self, client: &str) -> Option<String> {
        self.models
            .get(client)
            .or(self.default_model.as_ref())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
}

/// Settings for the code graph — in particular the *traversal* (co-access) edges
/// learned from real agent sessions (#289).
///
/// The static AST/import graph captures how code is wired structurally; it cannot
/// see which files an agent actually opens *together* while solving a task.
/// Traversal edges add that behavioural signal: files surfaced together are
/// associated with a decaying weight (Hebbian co-access), folded into the graph
/// as `co_access` edges and mixed into recall. The store is bounded and decays,
/// so stale associations fade.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GraphConfig {
    /// Record co-access between files surfaced together in a session, surface them
    /// as decaying `co_access` edges in the graph, and boost recall by them.
    /// On by default; set to `false` for a purely static (AST-only) graph.
    pub traversal_edges: bool,
}

impl Default for GraphConfig {
    fn default() -> Self {
        Self {
            traversal_edges: true,
        }
    }
}

/// Skillify (#290): mine the project's session diary + knowledge facts into
/// versioned, git-committable `.cursor/rules/skillify-*.mdc` rule files.
///
/// The miner is precision-biased — it only codifies recurring or high-confidence
/// patterns and never invents content. Runs on demand (`ctx_skillify` /
/// `lean-ctx skillify`); re-running merges (bumps version) only when the distilled
/// content actually changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillifyConfig {
    /// Master switch for the skillify miner. On by default; the miner only ever
    /// acts when explicitly invoked, so this never writes files unprompted.
    pub enabled: bool,
    /// Where generated rules are written: `project` (`<repo>/.cursor/rules`,
    /// git-committable, default) or `global` (`~/.cursor/rules`).
    pub scope: String,
    /// Minimum confidence for a single curated knowledge fact to be codified even
    /// without repetition. 0.0..=1.0.
    pub min_confidence: f32,
    /// Minimum number of reinforcements (confirmations / repeated mentions) before
    /// a pattern is codified when its confidence is below `min_confidence`.
    pub min_recurrence: u32,
}

impl Default for SkillifyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            scope: "project".to_string(),
            min_confidence: 0.7,
            min_recurrence: 2,
        }
    }
}

/// AI session summaries (#292): periodically distil the working session into a
/// compact, *semantically recallable* summary so a future session can answer
/// "what did I do last time on X?". Deterministic and local-first — recall uses
/// embeddings when the `embeddings` feature is on, else a lexical fallback.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SummariesConfig {
    /// Record periodic session summaries. On by default; recording is cheap and
    /// happens at most once per `every_n_turns` tool calls.
    pub enabled: bool,
    /// Tool calls between automatic summaries. The auto-checkpoint cadence still
    /// gates the check, so the effective minimum is the checkpoint interval.
    pub every_n_turns: u32,
    /// Maximum summaries kept per project (oldest pruned first).
    pub max_kept: u32,
}

impl Default for SummariesConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            every_n_turns: 25,
            max_kept: 100,
        }
    }
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

/// Semantic-embedding engine settings.
///
/// `model` selects which local ONNX embedding model lean-ctx downloads and uses for
/// `ctx_semantic_search`. Accepts the same aliases as the `LEAN_CTX_EMBEDDING_MODEL` env
/// var: `minilm` (all-MiniLM-L6-v2, 384d — the default), `nomic` (768d) — or any
/// `HuggingFace` repo with an ONNX export via `hf:org/repo[@revision]` (GL #397), e.g.
/// `hf:jinaai/jina-embeddings-v2-base-code` for code-specialized embeddings. When the
/// env var is set it takes precedence; an
/// unset/`None` value uses the default model. Switching models triggers a one-time
/// re-index on the next semantic search (vector dimensions follow from the model).
///
/// `dimensions` is only consulted for `hf:` custom models as the declared fallback
/// width; the real width is probed from the ONNX graph at load time. Built-ins ignore it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EmbeddingConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<usize>,
    /// Allow downloading the embedding model on first semantic need (#551).
    /// `None` (unset) means **allowed** — the soft default that activates the
    /// semantic features without manual setup. Set `false` for air-gapped
    /// machines. The `LEAN_CTX_EMBEDDINGS_AUTO_DOWNLOAD` env var, when set,
    /// overrides this in either direction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_download: Option<bool>,
    /// Pin embedding inference to a single CPU thread (no GPU EP) so vectors are
    /// bit-identical across machines, not just run-to-run on one host (#895).
    /// `None`/`false` keeps the multi-threaded GPU-capable path. Extractive prose
    /// ranking is already deterministic via score quantization + stable tiebreak;
    /// this flag is the extra hardening for cross-machine reproducibility. The
    /// `LEAN_CTX_EMBEDDING_DETERMINISTIC` env var overrides this either way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deterministic: Option<bool>,
}
