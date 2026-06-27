use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::memory_policy::MemoryPolicy;

mod defaults_allowlist;
mod enums;
mod memory;
mod provenance;
mod proxy;
mod render;
pub mod risk;
pub mod schema;
mod sections;
mod serde_defaults;
pub mod setter;
mod shell_activation;
pub use render::render_annotated_config;
pub use sections::*;
#[cfg(test)]
mod tests;

pub(crate) use defaults_allowlist::{cloud_infra_commands, default_shell_allowlist};
pub use enums::{
    CompressionLevel, Effort, OutputDensity, PermissionInheritance, ResponseVerbosity,
    RulesInjection, RulesScope, SessionDegrade, TeeMode, TerseAgent,
};
pub use memory::{MemoryCleanup, MemoryGuardConfig, MemoryProfile, SavingsFooter};
pub use provenance::{ConfigProvenance, EnvOverride};
pub use proxy::{
    HistoryMode, ProseRanker, ProseRole, ProxyConfig, ProxyProvider, RoleAggressiveness,
    UpstreamDrift, Upstreams, diagnose_drift, env_upstream_override, is_local_proxy_url,
    normalize_url, normalize_url_opt,
};
pub use shell_activation::ShellActivation;

/// Default BM25 cache cap from config (also used by `bm25_index` heuristics).
pub fn default_bm25_max_cache_mb() -> u64 {
    serde_defaults::default_bm25_max_cache_mb()
}

/// Effective on-disk ceiling (MB) for the persisted BM25 index when nothing is
/// explicitly configured (no `bm25_max_cache_mb`, no `max_disk_mb` budget).
///
/// Deliberately decoupled from the RAM `MemoryProfile` (64/128/512 MB): this is
/// a *disk* file, and tying it to the profile silently refused persistence on
/// large repos under Low/Balanced, forcing a cold rebuild on every call (the
/// perpetual "index warming" of issue #249). 512 MB compressed covers
/// essentially every real repo; RAM pressure is governed separately by the
/// eviction orchestrator (which measures real heap).
pub const DEFAULT_BM25_PERSIST_MB: u64 = 512;

// Compile-time regression guard (#249): the default disk ceiling must stay well
// above the old RAM-profile caps (64/128 MB) that starved large repos.
const _: () = assert!(DEFAULT_BM25_PERSIST_MB >= 512);

/// lean-ctx tools whose sole purpose is editing the user's source files. When
/// `prefer_native_editor` is set (#454) these are hidden from `list_tools` and
/// refused at dispatch so the host's native editor handles edits instead.
///
/// Deliberately narrow: only the dedicated edit tool is blocked. LSP refactor
/// (`ctx_refactor`) also exposes read-only sub-actions (references/definition),
/// so it is left available; users wanting it gone can add it to `disabled_tools`.
pub const EDIT_TOOL_NAMES: &[&str] = &["ctx_edit"];

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
    /// Output formats that are already compact/token-oriented and must be
    /// preserved verbatim instead of being recompressed (#342). Matched against
    /// the *output shape* (not the command name), so any tool emitting the
    /// format is covered without enumerating commands in `excluded_commands`.
    /// Default: `["toon"]`. Set to `[]` to disable and always recompress.
    #[serde(default = "serde_defaults::default_preserve_compact_formats")]
    pub preserve_compact_formats: Vec<String>,
    /// Opt-in: apply the lossless JSON crusher to *verbatim* data commands
    /// (`gh api`, `jq`, `kubectl get -o json`, `curl` JSON). Off by default, so
    /// those outputs stay byte-for-byte verbatim. When on, an array-heavy JSON
    /// payload the crusher can at least halve is reshaped into a compact, fully
    /// reconstructible form; everything else stays verbatim. See
    /// [`Config::crush_verbatim_json_enabled`] (#936).
    #[serde(default)]
    pub crush_verbatim_json: bool,
    /// Commands taking longer than this threshold (ms) are recorded in the slow log.
    /// Set to 0 to disable slow logging.
    pub slow_command_threshold_ms: u64,
    #[serde(default = "serde_defaults::default_theme")]
    pub theme: String,
    #[serde(default)]
    pub cloud: CloudConfig,
    #[serde(default)]
    pub gain: GainConfig,
    /// Model declaration for measured-vs-estimated cost reporting (MCP-only IDEs).
    #[serde(default)]
    pub cost: CostConfig,
    #[serde(default)]
    pub autonomy: AutonomyConfig,
    #[serde(default)]
    pub providers: ProvidersConfig,
    #[serde(default)]
    pub proxy: ProxyConfig,
    /// Whether the API proxy is enabled. Tri-state:
    /// - None: undecided (fresh install, will prompt on interactive setup)
    /// - Some(true): user opted in, proxy managed by lean-ctx
    /// - Some(false): user opted out, never touch proxy or endpoints
    #[serde(default)]
    pub proxy_enabled: Option<bool>,
    #[serde(default)]
    pub proxy_port: Option<u16>,
    /// Proxy reachability timeout in milliseconds. Default: 200.
    /// Override via LEAN_CTX_PROXY_TIMEOUT_MS env var.
    #[serde(default)]
    pub proxy_timeout_ms: Option<u64>,
    /// Strict proxy auth: when true, authenticate ONLY via the Bearer token
    /// (`LEAN_CTX_PROXY_TOKEN`) and disable the provider-API-key fallback. Default
    /// false keeps the loopback-friendly behavior where any local AI tool's own
    /// provider key authenticates (the proxy never injects upstream credentials —
    /// it forwards the caller's key verbatim). Enable on shared/multi-user hosts to
    /// require the token; clients must then send `Authorization: Bearer <token>`.
    #[serde(default)]
    pub proxy_require_token: bool,
    #[serde(default = "serde_defaults::default_buddy_enabled")]
    pub buddy_enabled: bool,
    #[serde(default = "serde_defaults::default_true")]
    pub enable_wakeup_ctx: bool,
    #[serde(default)]
    pub redirect_exclude: Vec<String>,
    /// Tools to exclude from the MCP tool list returned by list_tools.
    /// Accepts exact tool names (e.g. `["ctx_graph", "ctx_agent"]`).
    /// Empty by default — all tools listed, no behaviour change.
    #[serde(default)]
    pub disabled_tools: Vec<String>,
    /// Prefer the host agent's native editor over lean-ctx edit operations (#454).
    /// When true, the lean-ctx edit tool(s) (see [`EDIT_TOOL_NAMES`]) are neither
    /// advertised in `list_tools` nor dispatchable (direct or via `ctx_call`), so
    /// the agent falls back to the host's built-in editing UI. Reads / search /
    /// shell / memory tools are unaffected. Override via
    /// `LEAN_CTX_PREFER_NATIVE_EDITOR=1`.
    #[serde(default)]
    pub prefer_native_editor: bool,
    /// Tool categories to activate by default for dynamic-tool-capable clients.
    /// Values: "core" (always on), "arch", "debug", "memory", "metrics", "session".
    /// Example: `default_tool_categories = ["core", "arch", "memory"]`
    /// Override via LCTX_DEFAULT_CATEGORIES env var (comma-separated).
    /// Empty = lean-ctx default (core + session).
    #[serde(default)]
    pub default_tool_categories: Vec<String>,
    /// Disable all automatic read-mode degradation (auto_degrade + context_gate pressure).
    /// When true, lean-ctx never downgrades requested read modes regardless of pressure.
    /// Override via LCTX_NO_DEGRADE=1 env var.
    #[serde(default)]
    pub no_degrade: bool,
    /// Serve explicit `full`/`lines:N-M` re-reads of session-cached files as
    /// deltas: when the file changed on disk since it was cached, the read
    /// returns `mode=diff` instead of re-emitting content the model already
    /// holds. First reads are unaffected; `fresh=true` always bypasses.
    /// Opt-in. Override via LCTX_DELTA_EXPLICIT=1/0 env var.
    #[serde(default)]
    pub delta_explicit: bool,
    /// Persistent profile name. Checked after LEAN_CTX_PROFILE env var.
    /// Set via `lean-ctx config set profile passthrough` or editing config.toml.
    #[serde(default)]
    pub profile: Option<String>,
    /// Tool visibility profile: "minimal" (10), "standard" (19), or "power" (all).
    /// Override via LEAN_CTX_TOOL_PROFILE env var.
    /// Existing installs default to "power" (backward compat).
    #[serde(default)]
    pub tool_profile: Option<String>,
    /// Explicit list of enabled tool names (overrides tool_profile when non-empty).
    /// Example: `tools_enabled = ["ctx_read", "ctx_shell", "ctx_search"]`
    #[serde(default)]
    pub tools_enabled: Vec<String>,
    /// Active context persona (`persona-spec-v1`). Selects the domain bundle —
    /// tool surface, read-mode/compressor/chunker defaults, intent taxonomy,
    /// sensitivity floor. Override via `LEAN_CTX_PERSONA`. Defaults to `coding`.
    #[serde(default)]
    pub persona: Option<String>,
    #[serde(default)]
    pub loop_detection: LoopDetectionConfig,
    /// Controls where lean-ctx installs agent rule files.
    /// Values: "both" (default), "global" (home-dir only), "project" (repo-local only).
    /// Override via LEAN_CTX_RULES_SCOPE env var.
    #[serde(default)]
    pub rules_scope: Option<String>,
    /// Controls how rules are injected for shared-instruction-file agents.
    /// Values: "shared" (default, marker block in CLAUDE.md/CODEBUDDY.md/AGENTS.md/GEMINI.md),
    /// "dedicated" (never touch those files; use each agent's config-driven
    /// auto-load: SessionStart hook / instructions[] / context.fileName, #343), or
    /// "off" (write no rules file at all — for hosts that supply their own
    /// tool-steering workflow or phase-isolated/non-caching harnesses, #361).
    /// Override via LEAN_CTX_RULES_INJECTION env var.
    #[serde(default)]
    pub rules_injection: Option<String>,
    /// Mirror the host IDE's tool-permission rules onto lean-ctx's own MCP tools.
    /// Values: "off" (default) or "on". When "on", lean-ctx reads the active
    /// IDE's permission config (v1: OpenCode) and applies the equivalent
    /// deny/ask/allow decision to the matching lean-ctx tool — so `ctx_shell`
    /// honors your `bash`/`rm *` rules instead of bypassing them.
    /// Override via LEAN_CTX_PERMISSION_INHERITANCE env var.
    #[serde(default)]
    pub permission_inheritance: Option<String>,
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
    /// Global compression intensity 0.0 (lossless) – 1.0 (max), mapped onto the
    /// read modes / entropy / IB stages (see `core::aggressiveness`). `None`
    /// (default) keeps each mode's built-in default. Override via the
    /// `LEAN_CTX_AGGRESSIVENESS` env var or the `ctx_read` `aggressiveness` arg.
    #[serde(default)]
    pub compression_aggressiveness: Option<f64>,
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
    /// Allow jailed tool access to home-level IDE config dirs (~/.cursor, VS Code,
    /// Cline/Roo, JetBrains, …). Tri-state: `None` = not asked yet (setup prompts
    /// once), `Some(false)` = declined, `Some(true)` = opted in. Those dirs can
    /// expose other agents' sessions, MCP configs and credentials, so the effective
    /// default is off. `~/.lean-ctx` (own data dir) is always allowed. The opt-in
    /// set is registry-derived, covering every supported editor. Override via
    /// LEAN_CTX_ALLOW_IDE_DIRS=1.
    #[serde(default)]
    pub allow_ide_config_dirs: Option<bool>,
    /// Extra project roots for multi-root workspaces.
    /// Tools like ctx_tree and ctx_search can scan across all roots in a single call.
    /// These paths are automatically added to PathJail's allow-list.
    /// Override via LEAN_CTX_EXTRA_ROOTS env var (path-list separator).
    #[serde(default)]
    pub extra_roots: Vec<String>,
    /// Read-only roots: sibling subtrees the agent may READ but never WRITE.
    /// Reads resolve as if they were extra_roots; every write tool (edit, refactor,
    /// handoff/session export, memory compaction) is default-denied inside these
    /// paths. Useful for reference repos mounted next to the project.
    /// Override via LEAN_CTX_READ_ONLY_ROOTS env var (path-list separator).
    #[serde(default)]
    pub read_only_roots: Vec<String>,
    /// Enable content-defined chunking (Rabin-Karp) for cache-optimal output ordering.
    /// Stable chunks are emitted first to maximize prompt cache hits.
    #[serde(default)]
    pub content_defined_chunking: bool,
    /// Skip session/knowledge/gotcha blocks in MCP instructions to minimize token overhead.
    /// Override via LEAN_CTX_MINIMAL env var.
    #[serde(default)]
    pub minimal_overhead: bool,
    /// Opt-in: substitute long identifiers with short α-codes (+ a `§MAP` table)
    /// in `aggressive` reads for projects with >50 source files. Off by default —
    /// the abbreviated form is confusing for editing/refactoring, where the agent
    /// needs the real package and symbol names. Enable for max exploration savings.
    #[serde(default)]
    pub symbol_map_auto: bool,
    /// Opt-in: bias `auto` toward structure-first reads (`map`) for medium code
    /// files on a cold read. Off by default — interactive sessions keep the
    /// conservative `full` floor that avoids a follow-up body read. Enable for
    /// phase-isolated harnesses (no warm-session cache payback), where a cold
    /// `full` read is pure overhead and structure-first reads aid localization.
    /// Override via the LEAN_CTX_STRUCTURE_FIRST env var.
    #[serde(default)]
    pub structure_first: bool,
    /// Opt-in: let the adaptive *learning* signals (predictor, bandit, heatmap,
    /// adaptive policy, bounce/path memory) participate in `auto` mode
    /// resolution. Off by default (#683): the default cascade is a deterministic
    /// function of (file, task) — only capability guards and the size/task
    /// heuristic decide — which keeps output byte-stable for provider prompt
    /// caching (#498) and avoids per-read disk I/O from the learning stores.
    /// Override via the LEAN_CTX_AUTO_MODE_LEARNING env var.
    #[serde(default)]
    pub auto_mode_learning: bool,
    /// Team server URL for opt-in savings roll-up.
    /// Set via `lean-ctx config set team_url https://...` or `[team] url` in config.toml.
    /// Override via LEAN_CTX_TEAM_URL env var.
    #[serde(default)]
    pub team_url: Option<String>,
    /// Bearer token for the team server (Authorization header on savings push /
    /// pull). Set via `lean-ctx config set team_token <tok>` or `team_token` in
    /// config.toml. Override via the LEAN_CTX_TEAM_TOKEN env var.
    #[serde(default)]
    pub team_token: Option<String>,
    /// Opt-in: when true, the running daemon periodically pushes this machine's
    /// signed savings batch to `team_url` so the team roll-up fills itself (no
    /// manual `savings push` per dev). Off by default; requires `team_url` +
    /// `team_token`. Set via `lean-ctx config set team_auto_push true`.
    #[serde(default)]
    pub team_auto_push: bool,
    /// Enable human-readable activity journal (~/.lean-ctx/journal.md).
    #[serde(default)]
    pub journal_enabled: bool,
    /// Opt-in: auto-persist interesting findings as knowledge facts.
    #[serde(default)]
    pub auto_capture: bool,
    /// Hybrid search weights (BM25/dense/candidates).
    #[serde(default)]
    pub search: crate::core::hybrid_search::HybridConfig,
    /// Code-graph settings, including traversal (co-access) edges (#289).
    #[serde(default)]
    pub graph: GraphConfig,
    /// Skillify miner settings (#290): codify recurring patterns into rules.
    #[serde(default)]
    pub skillify: SkillifyConfig,
    /// AI session-summary settings (#292): periodic, semantically-recallable summaries.
    #[serde(default)]
    pub summaries: SummariesConfig,
    /// Optional LLM enhancement (query expansion, contradiction explanation).
    #[serde(default)]
    pub llm: crate::core::llm_enhance::LlmConfig,
    /// Semantic-embedding engine settings (which local ONNX model to use).
    #[serde(default)]
    pub embedding: EmbeddingConfig,
    /// Disable shell hook injection (the _lc() function that wraps CLI commands).
    /// Override via LEAN_CTX_NO_HOOK env var.
    #[serde(default)]
    pub shell_hook_disabled: bool,
    /// Shadow mode: transparently intercepts native tool calls (Read/Grep/Shell)
    /// via hooks, strengthens MCP instructions to MUST-level, and activates
    /// immediate bypass hints on first native tool use. Enables "transparent
    /// replacement" so agents use ctx_* without explicit opt-in.
    #[serde(default)]
    pub shadow_mode: bool,
    /// Opt-in (#520): write a human-readable debug log of intercepted MCP tool
    /// calls and hook routing decisions (lean-ctx vs native, with reasons) to
    /// `<state_dir>/logs/debug.log`. Override via the LEAN_CTX_DEBUG_LOG env var.
    #[serde(default)]
    pub debug_log: bool,
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
    #[serde(default)]
    pub updates: UpdatesConfig,
    /// Fixed-context budget accounting for `doctor overhead` / `gain` (#964).
    #[serde(default)]
    pub context: ContextConfig,
    /// Maximum BM25 cache file size in MB. Indexes exceeding this are quarantined on load
    /// and refused on save. Override via LEAN_CTX_BM25_MAX_CACHE_MB env var.
    #[serde(default = "serde_defaults::default_bm25_max_cache_mb")]
    pub bm25_max_cache_mb: u64,
    /// Maximum number of files scanned by the lightweight JSON graph index.
    /// 0 = unlimited (default). Set >0 to cap for constrained systems.
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
    /// Simplified disk budget (MB). When set and detail values are at defaults,
    /// distributes proportionally: archive=25%, bm25=10%, remainder for stores.
    /// 0 = disabled (use individual settings). Override via LEAN_CTX_MAX_DISK_MB.
    #[serde(default)]
    pub max_disk_mb: u64,
    /// Auto-purge data older than this many days. 0 = disabled.
    /// Flows into archive.max_age_hours and lifecycle idle TTL.
    #[serde(default)]
    pub max_staleness_days: u32,
    /// Cap on the rayon worker threads used by the CPU-heavy index build
    /// (call graph etc.). 0 = rayon default (all cores). Set >0 to bound
    /// per-instance CPU so a fleet of concurrent sessions can't saturate the
    /// host on startup. Override via LEANCTX_INDEX_THREADS env var.
    #[serde(default)]
    pub max_index_threads: usize,
    /// Controls visibility of token savings footers in tool output.
    /// Values: "always" (default, show on every response), "never", "auto" (legacy compatibility).
    /// Override via LEAN_CTX_SAVINGS_FOOTER or LEAN_CTX_SHOW_SAVINGS=1|0 env var.
    #[serde(default)]
    pub savings_footer: SavingsFooter,
    /// Explicit project root override. When set, lean-ctx uses this instead of auto-detection.
    /// This prevents accidental home-directory scans when running from $HOME.
    /// Override via LEAN_CTX_PROJECT_ROOT env var.
    #[serde(default)]
    pub project_root: Option<String>,
    /// LSP server overrides. Map language name to custom binary path.
    /// Example: `[lsp]\nrust = "/opt/rust-analyzer"\npython = "~/.venvs/main/bin/pylsp"`
    #[serde(default)]
    pub lsp: std::collections::HashMap<String, String>,
    /// Per-IDE allowed paths. Restricts which directories lean-ctx will scan/index for each IDE.
    /// Example: `[ide_paths]\ncursor = ["/home/user/projects/app1"]\ncodex = ["/home/user/codex"]`
    /// When set, only these paths are indexed for the matching agent. Global `allow_paths` still applies.
    #[serde(default)]
    pub ide_paths: HashMap<String, Vec<String>>,
    /// Custom model context window overrides.
    /// Example: `[model_context_windows]\n"my-custom-model" = 500000`
    #[serde(default)]
    pub model_context_windows: HashMap<String, usize>,
    /// Controls how much detail tool responses include.
    ///
    /// - `full` (default): complete compressed output
    /// - `headers_only`: metadata line only (path, mode, token count)
    ///
    /// Override via `LEAN_CTX_RESPONSE_VERBOSITY` env var.
    #[serde(default)]
    pub response_verbosity: ResponseVerbosity,
    /// Bypass hint mode. When agents use native Read/Grep instead of lean-ctx tools,
    /// a hint is appended to the next tool response.
    /// Values: "on" (default), "off", "aggressive" (hint on every call, no cooldown).
    /// Override via LEAN_CTX_BYPASS_HINTS env var.
    #[serde(default)]
    pub bypass_hints: Option<String>,
    /// Cache policy for ctx_read. Controls behavior on cache hits.
    /// Values: "aggressive" (default, 13-tok stubs + compaction-aware reset),
    /// "safe" (delivers map instead of stub), "off" (no caching, always disk read).
    /// Override via LEAN_CTX_CACHE_POLICY env var.
    #[serde(default)]
    pub cache_policy: Option<String>,
    /// Token budget for the in-memory `ctx_read` cache. When the cached total
    /// plus an incoming read would exceed this, lean-ctx evicts the least-valuable
    /// entries *immediately* (RRF: recency × frequency × size) so the read always
    /// proceeds — eviction is never deferred to the staleness TTL. `0` uses the
    /// built-in default (500k). `LEAN_CTX_CACHE_MAX_TOKENS` env var overrides this.
    #[serde(default)]
    pub cache_max_tokens: usize,
    /// Cross-project boundary policy.
    /// Controls whether cross-project search/import is allowed and whether access is audited.
    #[serde(default)]
    pub boundary_policy: crate::core::memory_boundary::BoundaryPolicy,
    #[serde(default)]
    pub secret_detection: SecretDetectionConfig,
    /// Per-item sensitivity model with a uniform policy floor (#212).
    /// Disabled by default → fully no-op until `sensitivity.enabled = true`.
    #[serde(default)]
    pub sensitivity: crate::core::sensitivity::SensitivityConfig,
    /// MCP Tool-Catalog Gateway (#210): aggregate + query-route downstream MCP
    /// servers. Global-only (never merged from project-local config) and a full
    /// no-op until `gateway.enabled = true`.
    #[serde(default)]
    pub gateway: crate::core::gateway::GatewayConfig,
    /// Addon ecosystem security floor (#863): install policy, registry-signature
    /// requirement and sandboxing for spawned addon servers. Global-only (never
    /// merged from project-local config) and fully permissive by default.
    #[serde(default)]
    pub addons: crate::core::addons::AddonsConfig,
    /// Allow automatic project-root re-rooting when absolute paths outside the jail are seen.
    /// When false (default), absolute paths outside the jail are rejected without re-rooting.
    /// Override via LEAN_CTX_ALLOW_REROOT env var.
    #[serde(default)]
    pub allow_auto_reroot: bool,
    /// Disable PathJail entirely by setting `path_jail = false` in config.toml.
    /// Useful in container/Docker environments where the sandbox is the boundary.
    /// (The former `LEAN_CTX_NO_JAIL=1` env override was removed in v3.7.3.)
    #[serde(default)]
    pub path_jail: Option<bool>,
    /// Sandbox level for code execution (ctx_exec).
    /// 0 = subprocess only (current), 1 = OS-level restriction (Seatbelt/Landlock).
    /// Override via LEAN_CTX_SANDBOX_LEVEL env var.
    #[serde(default)]
    pub sandbox_level: u8,
    /// When true, large tool outputs (>4000 chars) are stored as references
    /// and a short URI is returned instead of the full content.
    /// Override via LEAN_CTX_REFERENCE_RESULTS env var.
    #[serde(default)]
    pub reference_results: bool,
    /// Default per-agent token budget. 0 means unlimited.
    /// Override per-agent via ctx_session or programmatically.
    #[serde(default)]
    pub agent_token_budget: usize,
    /// Optional shell command allowlist. When non-empty, only commands whose base binary
    /// is in this list are permitted by ctx_shell. Empty = disable allowlist (allow all).
    /// Default includes common dev tools. Set to `[]` to disable.
    /// Override via LEAN_CTX_SHELL_ALLOWLIST env var (comma-separated).
    #[serde(default = "default_shell_allowlist")]
    pub shell_allowlist: Vec<String>,

    /// Extra commands MERGED on top of the effective `shell_allowlist` without replacing
    /// the defaults. Setting `shell_allowlist` replaces the whole built-in list (a common
    /// footgun); entries here are purely additive, which is what `lean-ctx allow <cmd>`
    /// writes. Only applied in restricted mode (when the base allowlist is non-empty).
    #[serde(default)]
    pub shell_allowlist_extra: Vec<String>,

    /// When true, block command substitution ($(), backticks) and process substitution
    /// (<(), >()) in shell arguments. When false (default), only warn via tracing.
    /// Default false preserves backward compatibility — set true for maximum security.
    #[serde(default)]
    pub shell_strict_mode: bool,

    /// Shell-security mode for ctx_shell / `lean-ctx -c` command gating (GL #788):
    /// `enforce` (default, secure), `warn` (run checks, log violations, never
    /// block) or `off` (skip the allowlist + dangerous-pattern blocks entirely —
    /// a deliberate opt-out; compression stays active). Override via
    /// LEAN_CTX_SHELL_SECURITY. `None` resolves to `enforce`.
    #[serde(default)]
    pub shell_security: Option<String>,

    /// Default shell-command timeout in seconds for *normal* commands. `None`
    /// resolves to the built-in 2-minute default; heavy builds/tests use
    /// [`Config::shell_heavy_timeout_secs`]. Override via
    /// `LEAN_CTX_SHELL_TIMEOUT_SECS` (`LEAN_CTX_SHELL_TIMEOUT_MS` still wins over
    /// both, in milliseconds).
    #[serde(default)]
    pub shell_timeout_secs: Option<u64>,

    /// Shell-command timeout in seconds for *heavy* commands (cargo build/test,
    /// make, docker build, git commit/push, …). `None` resolves to the built-in
    /// 10-minute ceiling. Override via `LEAN_CTX_SHELL_HEAVY_TIMEOUT_SECS`.
    #[serde(default)]
    pub shell_heavy_timeout_secs: Option<u64>,

    /// When true, `ctx_shell` accepts shell file-write redirects (`>`, `>>`,
    /// `tee`, heredoc-to-file, `curl -o`, `wget` default mode). Default false —
    /// the native Write/Edit tool is preferred. Opt-in for power users who want
    /// classic shell syntax; the real command gating (allowlist,
    /// dangerous-pattern and interpreter-eval blocks) still applies. Override
    /// via `LEAN_CTX_SHELL_ALLOW_WRITES=1`.
    #[serde(default)]
    pub shell_allow_writes: bool,

    /// Setup behavior: controls what gets injected during setup and updates.
    #[serde(default)]
    pub setup: SetupConfig,
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
            preserve_compact_formats: serde_defaults::default_preserve_compact_formats(),
            crush_verbatim_json: false,
            slow_command_threshold_ms: 5000,
            theme: serde_defaults::default_theme(),
            cloud: CloudConfig::default(),
            gain: GainConfig::default(),
            cost: CostConfig::default(),
            autonomy: AutonomyConfig::default(),
            providers: ProvidersConfig::default(),
            proxy: ProxyConfig::default(),
            proxy_enabled: None,
            proxy_port: None,
            proxy_timeout_ms: None,
            proxy_require_token: false,
            buddy_enabled: serde_defaults::default_buddy_enabled(),
            enable_wakeup_ctx: true,
            redirect_exclude: Vec::new(),
            disabled_tools: Vec::new(),
            prefer_native_editor: false,
            default_tool_categories: Vec::new(),
            no_degrade: false,
            delta_explicit: false,
            profile: None,
            tool_profile: None,
            tools_enabled: Vec::new(),
            persona: None,
            loop_detection: LoopDetectionConfig::default(),
            rules_scope: None,
            rules_injection: None,
            permission_inheritance: None,
            extra_ignore_patterns: Vec::new(),
            terse_agent: TerseAgent::default(),
            compression_level: CompressionLevel::default(),
            compression_aggressiveness: None,
            archive: ArchiveConfig::default(),
            memory: MemoryPolicy::default(),
            allow_paths: Vec::new(),
            allow_ide_config_dirs: None,
            extra_roots: Vec::new(),
            read_only_roots: Vec::new(),
            content_defined_chunking: false,
            minimal_overhead: true,
            symbol_map_auto: false,
            structure_first: false,
            auto_mode_learning: false,
            team_url: None,
            team_token: None,
            team_auto_push: false,
            journal_enabled: true,
            auto_capture: true,
            search: crate::core::hybrid_search::HybridConfig::default(),
            graph: GraphConfig::default(),
            skillify: SkillifyConfig::default(),
            summaries: SummariesConfig::default(),
            llm: crate::core::llm_enhance::LlmConfig::default(),
            embedding: EmbeddingConfig::default(),
            shell_hook_disabled: false,
            shadow_mode: false,
            debug_log: false,
            shell_activation: ShellActivation::default(),
            update_check_disabled: false,
            updates: UpdatesConfig::default(),
            context: ContextConfig::default(),
            graph_index_max_files: serde_defaults::default_graph_index_max_files(),
            bm25_max_cache_mb: serde_defaults::default_bm25_max_cache_mb(),
            memory_profile: MemoryProfile::default(),
            memory_cleanup: MemoryCleanup::default(),
            max_ram_percent: serde_defaults::default_max_ram_percent(),
            max_disk_mb: 0,
            max_staleness_days: 0,
            max_index_threads: 0,
            savings_footer: SavingsFooter::default(),
            project_root: None,
            lsp: std::collections::HashMap::new(),
            ide_paths: HashMap::new(),
            model_context_windows: HashMap::new(),
            response_verbosity: ResponseVerbosity::default(),
            bypass_hints: None,
            cache_policy: None,
            cache_max_tokens: 0,
            boundary_policy: crate::core::memory_boundary::BoundaryPolicy::default(),
            secret_detection: SecretDetectionConfig::default(),
            sensitivity: crate::core::sensitivity::SensitivityConfig::default(),
            gateway: crate::core::gateway::GatewayConfig::default(),
            addons: crate::core::addons::AddonsConfig::default(),
            allow_auto_reroot: false,
            path_jail: None,
            sandbox_level: 0,
            reference_results: false,
            agent_token_budget: 0,
            shell_allowlist: default_shell_allowlist(),
            shell_allowlist_extra: Vec::new(),
            shell_strict_mode: false,
            shell_security: None,
            shell_timeout_secs: None,
            shell_heavy_timeout_secs: None,
            shell_allow_writes: false,
            setup: SetupConfig::default(),
        }
    }
}

/// Holds the most recent global `config.toml` parse error, if the file currently
/// fails to parse. When that happens `Config::load()` silently falls back to the
/// built-in defaults and only logs to stderr — which is invisible over an MCP/stdio
/// transport. Recording it here lets callers (e.g. the shell-allowlist diagnostic
/// and `lean-ctx doctor`) surface "you're on defaults because your config is broken".
static LAST_PARSE_ERROR: Mutex<Option<String>> = Mutex::new(None);

/// Returns the most recent global config parse error, or `None` if the current
/// `config.toml` parsed successfully (or no config file exists).
#[must_use]
pub fn last_config_parse_error() -> Option<String> {
    LAST_PARSE_ERROR.lock().ok().and_then(|g| g.clone())
}

fn record_parse_error(err: Option<String>) {
    if let Ok(mut guard) = LAST_PARSE_ERROR.lock() {
        *guard = err;
    }
}

/// Reset every SECURITY-sensitive field of a parsed project-local `Config` back
/// to its default, returning the names of the ones that actually carried an
/// override. Used by [`Config::merge_local`] for untrusted workspaces: clearing a
/// field to its default makes the downstream "== default ⇒ no override" merge
/// guards skip it automatically, so a single list here gates every sensitive key
/// without touching the per-field merge arms (security audit #4).
///
/// Sensitive = anything that can widen lean-ctx's own boundaries or steer the
/// agent: the shell allowlist, path-jail roots, proxy upstreams, command
/// aliases, network passthrough, rules scope/injection, tool disabling and
/// permission inheritance. Comfort/perf knobs are intentionally NOT listed.
fn strip_sensitive_overrides(local: &mut Config) -> Vec<&'static str> {
    let mut withheld: Vec<&'static str> = Vec::new();

    if local.shell_allowlist != default_shell_allowlist() {
        local.shell_allowlist = default_shell_allowlist();
        withheld.push("shell_allowlist");
    }
    if !local.shell_allowlist_extra.is_empty() {
        local.shell_allowlist_extra.clear();
        withheld.push("shell_allowlist_extra");
    }
    if !local.allow_paths.is_empty() {
        local.allow_paths.clear();
        withheld.push("allow_paths");
    }
    if !local.extra_roots.is_empty() {
        local.extra_roots.clear();
        withheld.push("extra_roots");
    }
    if !local.custom_aliases.is_empty() {
        local.custom_aliases.clear();
        withheld.push("custom_aliases");
    }
    if !local.passthrough_urls.is_empty() {
        local.passthrough_urls.clear();
        withheld.push("passthrough_urls");
    }
    if local.proxy.anthropic_upstream.is_some()
        || local.proxy.openai_upstream.is_some()
        || local.proxy.chatgpt_upstream.is_some()
        || local.proxy.gemini_upstream.is_some()
    {
        local.proxy.anthropic_upstream = None;
        local.proxy.openai_upstream = None;
        local.proxy.chatgpt_upstream = None;
        local.proxy.gemini_upstream = None;
        withheld.push("proxy.*_upstream");
    }
    if local.rules_scope.is_some() {
        local.rules_scope = None;
        withheld.push("rules_scope");
    }
    if local.rules_injection.is_some() {
        local.rules_injection = None;
        withheld.push("rules_injection");
    }
    if local.permission_inheritance.is_some() {
        local.permission_inheritance = None;
        withheld.push("permission_inheritance");
    }
    if !local.disabled_tools.is_empty() {
        local.disabled_tools.clear();
        withheld.push("disabled_tools");
    }

    withheld
}

/// Names of the SECURITY-sensitive overrides a project-local `.lean-ctx.toml`
/// carries — the keys `strip_sensitive_overrides` would withhold for an
/// untrusted workspace. Read-only (parses a throwaway `Config`); used by
/// `lean-ctx trust` to tell the user exactly what trusting will enable.
#[must_use]
pub fn local_sensitive_overrides(local_toml: &str) -> Vec<&'static str> {
    match toml::from_str::<Config>(local_toml) {
        Ok(mut parsed) => strip_sensitive_overrides(&mut parsed),
        Err(_) => Vec::new(),
    }
}

impl Config {
    /// Whether opt-in lossless JSON crushing of verbatim data commands (#936) is
    /// active. `LEAN_CTX_CRUSH_VERBATIM_JSON` (any value) wins, then the
    /// `crush_verbatim_json` config flag, else `false`.
    pub fn crush_verbatim_json_enabled(&self) -> bool {
        std::env::var("LEAN_CTX_CRUSH_VERBATIM_JSON").is_ok() || self.crush_verbatim_json
    }

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

    /// Returns the effective rules injection mode, preferring env var over config.
    /// Default is `Shared` (zero-config discovery via a CLAUDE.md/CODEBUDDY.md/AGENTS.md block).
    pub fn rules_injection_effective(&self) -> RulesInjection {
        let raw = std::env::var("LEAN_CTX_RULES_INJECTION")
            .ok()
            .or_else(|| self.rules_injection.clone())
            .unwrap_or_default();
        match raw.trim().to_lowercase().as_str() {
            "dedicated" => RulesInjection::Dedicated,
            "off" | "none" | "disabled" => RulesInjection::Off,
            _ => RulesInjection::Shared,
        }
    }

    /// Returns the effective permission-inheritance mode, preferring the
    /// `LEAN_CTX_PERMISSION_INHERITANCE` env var over config. Default is `Off`.
    /// Accepts `on`/`true`/`1` as enabled.
    #[must_use]
    pub fn permission_inheritance_effective(&self) -> PermissionInheritance {
        let raw = std::env::var("LEAN_CTX_PERMISSION_INHERITANCE")
            .ok()
            .or_else(|| self.permission_inheritance.clone())
            .unwrap_or_default();
        match raw.trim().to_lowercase().as_str() {
            "on" | "true" | "1" | "inherit" => PermissionInheritance::On,
            _ => PermissionInheritance::Off,
        }
    }

    /// True when lean-ctx should inject its rules via each agent's dedicated,
    /// non-polluting auto-load path *and* global rules are in scope.
    ///
    /// Gates the Claude/Codex `SessionStart` `additionalContext` summary: it
    /// stands in for the (now-skipped) shared CLAUDE.md/CODEBUDDY.md/AGENTS.md block, so it
    /// only fires when injection is `Dedicated` and the scope isn't project-only.
    #[must_use]
    pub fn dedicated_session_context_active(&self) -> bool {
        self.rules_injection_effective() == RulesInjection::Dedicated
            && self.rules_scope_effective() != RulesScope::Project
    }

    fn parse_disabled_tools_env(val: &str) -> Vec<String> {
        val.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// Returns the effective disabled tools list, preferring env var over config
    /// file. When `prefer_native_editor` is active, the lean-ctx edit tools are
    /// folded in so they are hidden from `list_tools` (#454).
    pub fn disabled_tools_effective(&self) -> Vec<String> {
        let mut list = if let Ok(val) = std::env::var("LEAN_CTX_DISABLED_TOOLS") {
            Self::parse_disabled_tools_env(&val)
        } else {
            self.disabled_tools.clone()
        };
        if self.prefer_native_editor_effective() {
            for name in EDIT_TOOL_NAMES {
                if !list.iter().any(|t| t == name) {
                    list.push((*name).to_string());
                }
            }
        }
        list
    }

    /// Whether lean-ctx edit operations are disabled in favour of the host's
    /// native editor (#454). `LEAN_CTX_PREFER_NATIVE_EDITOR` wins over config.
    pub fn prefer_native_editor_effective(&self) -> bool {
        match std::env::var("LEAN_CTX_PREFER_NATIVE_EDITOR") {
            Ok(raw) => matches!(
                raw.trim().to_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            ),
            Err(_) => self.prefer_native_editor,
        }
    }

    /// Cap on the rayon index-build worker threads. `LEANCTX_INDEX_THREADS` wins
    /// over config; `0` means "no cap" — rayon's all-cores default is kept.
    pub fn max_index_threads_effective(&self) -> usize {
        std::env::var("LEANCTX_INDEX_THREADS")
            .ok()
            .and_then(|raw| raw.trim().parse::<usize>().ok())
            .unwrap_or(self.max_index_threads)
    }

    /// Whether `name` is a lean-ctx edit operation that must be blocked from
    /// dispatch (direct and via `ctx_call`) when [`Self::prefer_native_editor_effective`]
    /// is set (#454). Read/search/shell/memory tools are never blocked.
    pub fn edit_tool_blocked(&self, name: &str) -> bool {
        self.prefer_native_editor_effective() && EDIT_TOOL_NAMES.contains(&name)
    }

    /// Returns `true` if minimal overhead is enabled via env var or config.
    pub fn minimal_overhead_effective(&self) -> bool {
        std::env::var("LEAN_CTX_MINIMAL").is_ok() || self.minimal_overhead
    }

    /// Returns `true` if structure-first auto reads are enabled.
    ///
    /// The `LEAN_CTX_STRUCTURE_FIRST` env var wins over the config field, and
    /// accepts the usual truthy/falsy spellings so a harness can flip it per run
    /// (`LEAN_CTX_STRUCTURE_FIRST=0` forces it off even if config enables it).
    pub fn structure_first_effective(&self) -> bool {
        match std::env::var("LEAN_CTX_STRUCTURE_FIRST") {
            Ok(raw) => matches!(
                raw.trim().to_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            ),
            Err(_) => self.structure_first,
        }
    }

    /// Returns `true` when the adaptive learning signals may participate in
    /// `auto` mode resolution (#683). Off by default for a deterministic,
    /// I/O-light cascade; the `LEAN_CTX_AUTO_MODE_LEARNING` env var wins over the
    /// config field and accepts the usual truthy/falsy spellings.
    pub fn auto_mode_learning_effective(&self) -> bool {
        match std::env::var("LEAN_CTX_AUTO_MODE_LEARNING") {
            Ok(raw) => matches!(
                raw.trim().to_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            ),
            Err(_) => self.auto_mode_learning,
        }
    }

    /// Returns `true` when probabilistic exploration (Thompson sampling,
    /// Boltzmann-temperature eviction, simulated annealing) may influence
    /// decisions. Off by default so tool output stays a deterministic, byte-
    /// stable function of (content, mode, task) — the determinism contract
    /// (#498) that lets provider prompt caching apply. The `LEAN_CTX_STOCHASTIC`
    /// env var wins (the usual truthy/falsy spellings); otherwise it follows
    /// [`Self::auto_mode_learning_effective`], which is itself off by default.
    pub fn is_stochastic_enabled(&self) -> bool {
        match std::env::var("LEAN_CTX_STOCHASTIC") {
            Ok(raw) => matches!(
                raw.trim().to_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            ),
            Err(_) => self.auto_mode_learning_effective(),
        }
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

    /// Returns `true` if `ctx_shell` may accept shell file-write redirects.
    /// `LEAN_CTX_SHELL_ALLOW_WRITES` (`1`/`true`/`yes`/`on`) overrides
    /// `config.toml`. The real command gating still applies either way.
    pub fn shell_allow_writes_effective(&self) -> bool {
        match std::env::var("LEAN_CTX_SHELL_ALLOW_WRITES") {
            Ok(raw) => matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            ),
            Err(_) => self.shell_allow_writes,
        }
    }

    /// Returns `true` if the daily update check is disabled via env var or config.
    pub fn update_check_disabled_effective(&self) -> bool {
        std::env::var("LEAN_CTX_NO_UPDATE_CHECK").is_ok() || self.update_check_disabled
    }

    pub fn memory_policy_effective(&self) -> Result<MemoryPolicy, String> {
        let mut policy = self.memory.clone();
        policy.apply_env_overrides();

        let budget = self.max_disk_mb_effective();
        if budget > 0 {
            let scale_factor = (budget as f64 / 500.0).clamp(0.5, 10.0);
            let default_policy = MemoryPolicy::default();
            if policy.knowledge.max_facts == default_policy.knowledge.max_facts {
                policy.knowledge.max_facts = (200.0 * scale_factor) as usize;
            }
            if policy.knowledge.max_patterns == default_policy.knowledge.max_patterns {
                policy.knowledge.max_patterns = (50.0 * scale_factor) as usize;
            }
            if policy.episodic.max_episodes == default_policy.episodic.max_episodes {
                policy.episodic.max_episodes = (500.0 * scale_factor) as usize;
            }
            if policy.procedural.max_procedures == default_policy.procedural.max_procedures {
                policy.procedural.max_procedures = (100.0 * scale_factor) as usize;
            }
        }

        policy.validate()?;
        Ok(policy)
    }

    /// Returns the effective set of default tool categories.
    /// Priority: LCTX_DEFAULT_CATEGORIES env var > config.toml > hardcoded default.
    pub fn default_tool_categories_effective(&self) -> Vec<String> {
        if let Ok(val) = std::env::var("LCTX_DEFAULT_CATEGORIES") {
            return val
                .split(',')
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
        }
        if !self.default_tool_categories.is_empty() {
            return self
                .default_tool_categories
                .iter()
                .map(|s| s.to_lowercase())
                .collect();
        }
        vec!["core".to_string(), "session".to_string()]
    }

    /// Returns the effective tool profile.
    /// Priority: LEAN_CTX_TOOL_PROFILE env > config tool_profile > config
    /// tools_enabled > active persona's tool surface > power.
    ///
    /// Explicit settings win (backward compatible); when none are set, the
    /// active persona supplies the tool surface (the `coding` default resolves
    /// to `power`, so existing installs are unaffected).
    pub fn tool_profile_effective(&self) -> super::tool_profiles::ToolProfile {
        super::persona::Persona::resolve(self).effective_tool_profile(self)
    }

    /// Returns `true` if all automatic read-mode degradation is disabled.
    /// Checks LCTX_NO_DEGRADE env var first, then config.toml field.
    pub fn no_degrade_effective(&self) -> bool {
        if let Ok(val) = std::env::var("LCTX_NO_DEGRADE") {
            return val == "1" || val.eq_ignore_ascii_case("true");
        }
        self.no_degrade
    }

    /// Returns `true` if explicit `full`/`lines:N-M` re-reads of
    /// cached-but-changed files should be served as deltas (`mode=diff`)
    /// instead of re-emitting full content.
    ///
    /// Checks the `LCTX_DELTA_EXPLICIT` env var first, then the config.toml
    /// field. Unlike a presence-only knob, an explicit `0`/`false` in the env
    /// forces the feature OFF even when the config field is `true`, so the env
    /// can fully override config in both directions.
    pub fn delta_explicit_effective(&self) -> bool {
        if let Ok(val) = std::env::var("LCTX_DELTA_EXPLICIT") {
            return val == "1" || val.eq_ignore_ascii_case("true");
        }
        self.delta_explicit
    }

    /// Effective max_disk_mb from env or config.
    pub fn max_disk_mb_effective(&self) -> u64 {
        std::env::var("LEAN_CTX_MAX_DISK_MB")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(self.max_disk_mb)
    }

    /// Effective max_staleness_days from env or config.
    pub fn max_staleness_days_effective(&self) -> u32 {
        std::env::var("LEAN_CTX_MAX_STALENESS_DAYS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(self.max_staleness_days)
    }

    /// Effective fixed-context budget (tokens) from env or config (#964). `0`
    /// (env or config) disables the warning; otherwise the per-session footprint
    /// is checked against this in `doctor overhead` and `gain`.
    pub fn context_budget_tokens_effective(&self) -> usize {
        std::env::var("LEAN_CTX_CONTEXT_BUDGET_TOKENS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(self.context.budget_tokens)
    }

    /// Archive max_disk_mb derived from simplified max_disk_mb if the detail
    /// value is still at its default. Explicit overrides take priority.
    pub fn archive_max_disk_mb_effective(&self) -> u64 {
        let budget = self.max_disk_mb_effective();
        if budget > 0 && self.archive.max_disk_mb == ArchiveConfig::default().max_disk_mb {
            budget * 25 / 100
        } else {
            self.archive.max_disk_mb
        }
    }

    /// Archive max_age_hours derived from max_staleness_days if the detail
    /// value is still at its default. Explicit overrides take priority.
    pub fn archive_max_age_hours_effective(&self) -> u64 {
        let staleness = self.max_staleness_days_effective();
        if staleness > 0 && self.archive.max_age_hours == ArchiveConfig::default().max_age_hours {
            staleness as u64 * 24
        } else {
            self.archive.max_age_hours
        }
    }

    /// Effective on-disk ceiling (MB) for the persisted BM25 index. Single source
    /// of truth for `save`/`load`, `cache prune`, and the doctor health check.
    ///
    /// Priority: explicit `bm25_max_cache_mb` › `max_disk_mb` budget (10%) ›
    /// generous default ([`DEFAULT_BM25_PERSIST_MB`]). The default is decoupled
    /// from the RAM profile so large repos persist instead of rebuilding forever
    /// (issue #249).
    pub fn bm25_max_cache_mb_effective(&self) -> u64 {
        if self.bm25_max_cache_mb != serde_defaults::default_bm25_max_cache_mb() {
            return self.bm25_max_cache_mb;
        }
        let budget = self.max_disk_mb_effective();
        if budget > 0 {
            return budget * 10 / 100;
        }
        DEFAULT_BM25_PERSIST_MB
    }
}

impl Config {
    /// Returns the path to the global config file (`$XDG_CONFIG_HOME/lean-ctx/config.toml`).
    ///
    /// Resolves via [`crate::core::paths::config_dir`] so config lives in the
    /// RO-safe config category. Behavior-neutral today: `config_dir()` equals the
    /// legacy data dir for existing/single-dir installs (GH #408 / GL #602).
    pub fn path() -> Option<PathBuf> {
        crate::core::paths::config_dir()
            .ok()
            .map(|d| d.join("config.toml"))
    }

    /// `Some(path)` when the global config the runtime *resolves* does not exist,
    /// so lean-ctx is silently on built-in defaults. `None` when a config file is
    /// present (or HOME is unresolvable).
    ///
    /// The directory is layout-dependent (XDG `~/.config/lean-ctx` vs legacy
    /// `~/.lean-ctx` vs `$LEAN_CTX_DATA_DIR`) and an MCP client may launch the
    /// server in a sandbox/container with a different `$HOME`. An edit made to a
    /// *different* `config.toml` than this one is silently ignored; the block
    /// messages use this to say so out loud over MCP, where the stderr path is
    /// invisible (#540).
    #[must_use]
    pub fn missing_config_path() -> Option<PathBuf> {
        match Self::path() {
            Some(p) if !p.exists() => Some(p),
            _ => None,
        }
    }

    /// Returns the path to the project-local config override file.
    pub fn local_path(project_root: &str) -> PathBuf {
        PathBuf::from(project_root).join(".lean-ctx.toml")
    }

    /// Resolves the active project root (env override → session → git toplevel →
    /// cwd), cached for the process. Exposed crate-wide so workspace-trust and the
    /// CLI agree with config loading on *which* directory a `.lean-ctx.toml`
    /// belongs to (GH security audit, finding 4).
    pub(crate) fn find_project_root() -> Option<String> {
        static ROOT_CACHE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
        ROOT_CACHE
            .get_or_init(Self::find_project_root_inner)
            .clone()
    }

    fn find_project_root_inner() -> Option<String> {
        if let Ok(env_root) = std::env::var("LEAN_CTX_PROJECT_ROOT")
            && !env_root.is_empty()
        {
            return Some(env_root);
        }

        let cwd = std::env::current_dir().ok();

        if let Some(root) =
            crate::core::session::SessionState::load_latest().and_then(|s| s.project_root)
        {
            let root_path = std::path::Path::new(&root);
            let cwd_is_under_root = cwd.as_ref().is_some_and(|c| c.starts_with(root_path));
            // Route the marker probe through the TCC-guarded helper and never
            // adopt a ~/Documents project root from a launchd-standalone process
            // (#356): doing so would later stat its `.lean-ctx.toml`/markers and
            // pop the macOS privacy prompt in lean-ctx's own name.
            let has_marker = crate::core::pathutil::has_project_marker(root_path);

            if (cwd_is_under_root || has_marker) && crate::core::pathutil::may_probe_path(root_path)
            {
                return Some(root);
            }
        }

        if let Some(ref cwd) = cwd {
            // A launchd-standalone process must not shell out to `git` (which
            // stats the working tree) or adopt cwd as the project root when cwd
            // is under a TCC-protected dir (#356).
            let may_probe_cwd = crate::core::pathutil::may_probe_path(cwd);
            let git_root = if may_probe_cwd {
                std::process::Command::new("git")
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
                    })
            } else {
                None
            };
            if let Some(root) = git_root {
                return Some(root);
            }
            if may_probe_cwd && !crate::core::pathutil::is_broad_or_unsafe_root(cwd) {
                return Some(cwd.to_string_lossy().to_string());
            }
        }
        None
    }

    /// Loads config from disk with caching, merging global + project-local overrides.
    ///
    /// The cache is keyed on a **content hash** of the global + project-local
    /// files, not their mtime. mtime-only invalidation silently served a stale
    /// `Config` whenever a content edit preserved the mtime (coarse filesystem
    /// mtime resolution, `cp -p`, atomic save-then-rename, two edits within the
    /// same second). A long-lived MCP server then kept the old value (e.g.
    /// `path_jail`) while a fresh `lean-ctx doctor` process — with an empty
    /// cache — saw the new one (#406). Config files are tiny, so reading +
    /// hashing them on every load is negligible and guarantees liveness.
    pub fn load() -> Self {
        static CACHE: Mutex<Option<(Config, Option<String>, Option<String>)>> = Mutex::new(None);

        let Some(path) = Self::path() else {
            return Self::default();
        };

        let project_root = Self::find_project_root();
        let local_path = project_root.as_deref().map(Self::local_path);

        // Read raw content up front so the cache key is a content hash.
        let global_content = std::fs::read_to_string(&path).ok();
        // TCC (#356): never read a project-local `.lean-ctx.toml` under
        // ~/Documents from a launchd-standalone process — the read pops the
        // macOS privacy prompt. `find_project_root` already avoids returning
        // such roots; this also guards the explicit `LEAN_CTX_PROJECT_ROOT` path.
        let local_content = local_path
            .as_ref()
            .filter(|p| crate::core::pathutil::may_probe_path(p.as_path()))
            .and_then(|p| std::fs::read_to_string(p).ok());

        let global_hash = global_content.as_deref().map(crate::core::hasher::hash_str);
        let local_hash = local_content.as_deref().map(crate::core::hasher::hash_str);

        if let Ok(guard) = CACHE.lock()
            && let Some((ref cfg, ref cached_global, ref cached_local)) = *guard
            && *cached_global == global_hash
            && *cached_local == local_hash
        {
            return cfg.clone();
        }

        let mut cfg: Config = if let Some(ref content) = global_content {
            match toml::from_str(content) {
                Ok(c) => {
                    record_parse_error(None);
                    c
                }
                Err(e) => {
                    record_parse_error(Some(format!("{e}")));
                    tracing::warn!("config parse error in {}: {e}", path.display());
                    eprintln!(
                        "\x1b[33m[lean-ctx] WARNING: config parse error in {}: {e}\n  \
                         Using defaults. Run `lean-ctx doctor --fix` to repair.\x1b[0m",
                        path.display()
                    );
                    Self::default()
                }
            }
        } else {
            record_parse_error(None);
            Self::default()
        };

        if let Some(ref local) = local_content {
            // Finding 4: a project-local `.lean-ctx.toml`'s SECURITY-sensitive
            // overrides (shell allowlist, path-jail widening, proxy upstream, …)
            // are honoured only for a workspace the user has explicitly trusted.
            // `local_hash` is exactly the content hash workspace-trust pins, so
            // editing the file after trust re-gates it (see `workspace_trust`).
            let trusted = project_root.as_deref().is_some_and(|r| {
                crate::core::workspace_trust::is_trusted_for(
                    std::path::Path::new(r),
                    local_hash.as_deref().unwrap_or_default(),
                )
            });
            cfg.merge_local(local, trusted);
        }

        if let Ok(mut guard) = CACHE.lock() {
            *guard = Some((cfg.clone(), global_hash, local_hash));
        }

        cfg
    }

    /// Merge a project-local `.lean-ctx.toml` onto `self`.
    ///
    /// `trusted` reflects [`crate::core::workspace_trust`]: when `false`, the
    /// security-sensitive overrides (shell allowlist, path-jail widening, proxy
    /// upstream, command aliases, rules scope, …) are withheld and a warning is
    /// emitted — comfort-only overrides (compression, theme, memory tuning) still
    /// apply. This stops a cloned, untrusted repo from silently weakening
    /// lean-ctx's own boundaries through its bundled config (security audit #4).
    fn merge_local(&mut self, local_toml: &str, trusted: bool) {
        let mut local: Config = match toml::from_str(local_toml) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("local config parse error: {e}");
                eprintln!(
                    "\x1b[33m[lean-ctx] WARNING: local .lean-ctx.toml parse error: {e}\n  \
                     Local overrides skipped.\x1b[0m"
                );
                return;
            }
        };
        if !trusted {
            let withheld = strip_sensitive_overrides(&mut local);
            if !withheld.is_empty() {
                tracing::warn!(
                    "[SECURITY] untrusted workspace: ignoring {} security-sensitive \
                     .lean-ctx.toml override(s): {} — run `lean-ctx trust` to apply them",
                    withheld.len(),
                    withheld.join(", ")
                );
            }
        }
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
        // Additive merge with dedup: project-local config can add formats on top
        // of the global default (`["toon"]`) without re-listing it.
        for fmt in local.preserve_compact_formats {
            if !self
                .preserve_compact_formats
                .iter()
                .any(|f| f.eq_ignore_ascii_case(&fmt))
            {
                self.preserve_compact_formats.push(fmt);
            }
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
        if !local.enable_wakeup_ctx {
            self.enable_wakeup_ctx = false;
        }
        if !local.redirect_exclude.is_empty() {
            self.redirect_exclude.extend(local.redirect_exclude);
        }
        if !local.disabled_tools.is_empty() {
            self.disabled_tools.extend(local.disabled_tools);
        }
        if local.prefer_native_editor {
            self.prefer_native_editor = true;
        }
        if !local.extra_ignore_patterns.is_empty() {
            self.extra_ignore_patterns
                .extend(local.extra_ignore_patterns);
        }
        if local.rules_scope.is_some() {
            self.rules_scope = local.rules_scope;
        }
        if local.rules_injection.is_some() {
            self.rules_injection = local.rules_injection;
        }
        if local.permission_inheritance.is_some() {
            self.permission_inheritance = local.permission_inheritance;
        }
        if local.proxy.anthropic_upstream.is_some() {
            self.proxy.anthropic_upstream = local.proxy.anthropic_upstream;
        }
        if local.proxy.openai_upstream.is_some() {
            self.proxy.openai_upstream = local.proxy.openai_upstream;
        }
        if local.proxy.chatgpt_upstream.is_some() {
            self.proxy.chatgpt_upstream = local.proxy.chatgpt_upstream;
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
        if !local.autonomy.cognition_loop_enabled {
            self.autonomy.cognition_loop_enabled = false;
        }
        if local.autonomy.cognition_loop_interval_secs
            != AutonomyConfig::default().cognition_loop_interval_secs
        {
            self.autonomy.cognition_loop_interval_secs =
                local.autonomy.cognition_loop_interval_secs;
        }
        if local.autonomy.cognition_loop_max_steps
            != AutonomyConfig::default().cognition_loop_max_steps
        {
            self.autonomy.cognition_loop_max_steps = local.autonomy.cognition_loop_max_steps;
        }
        if local_toml.contains("compression_level") {
            self.compression_level = local.compression_level;
        }
        if local_toml.contains("compression_aggressiveness") {
            self.compression_aggressiveness = local.compression_aggressiveness;
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
        if !local.archive.ephemeral {
            self.archive.ephemeral = false;
        }
        if local.archive.ephemeral_min_tokens != ArchiveConfig::default().ephemeral_min_tokens {
            self.archive.ephemeral_min_tokens = local.archive.ephemeral_min_tokens;
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
        if local.memory.lifecycle.reclaim_headroom_pct != mem_def.lifecycle.reclaim_headroom_pct {
            self.memory.lifecycle.reclaim_headroom_pct =
                local.memory.lifecycle.reclaim_headroom_pct;
        }
        if local.memory.lifecycle.reclaim_enabled != mem_def.lifecycle.reclaim_enabled {
            self.memory.lifecycle.reclaim_enabled = local.memory.lifecycle.reclaim_enabled;
        }

        if local.memory.embeddings.max_facts != mem_def.embeddings.max_facts {
            self.memory.embeddings.max_facts = local.memory.embeddings.max_facts;
        }
        if !local.allow_paths.is_empty() {
            self.allow_paths.extend(local.allow_paths);
        }
        if !local.extra_roots.is_empty() {
            self.extra_roots.extend(local.extra_roots);
        }
        // Project-local config may only ADD read-only roots (tighten the write
        // boundary), never remove them — merge mirrors extra_roots (#475).
        if !local.read_only_roots.is_empty() {
            self.read_only_roots.extend(local.read_only_roots);
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
        // Only override when the local file actually defines `shell_allowlist`.
        // The field carries `#[serde(default = "default_shell_allowlist")]`, so a
        // local `.lean-ctx.toml` that omits the key still deserializes to the full
        // 201-entry built-in list — an `is_empty()` guard would then silently clobber
        // a deliberately shorter global allowlist with the defaults. Comparing against
        // the default (the same pattern used for every other merged field) treats
        // "omitted" as "no override".
        if local.shell_allowlist != default_shell_allowlist() {
            self.shell_allowlist = local.shell_allowlist;
        }
        if !local.shell_allowlist_extra.is_empty() {
            self.shell_allowlist_extra
                .extend(local.shell_allowlist_extra);
        }
        if !local.default_tool_categories.is_empty() {
            self.default_tool_categories = local.default_tool_categories;
        }
        if local.tool_profile.is_some() {
            self.tool_profile = local.tool_profile;
        }
        if !local.tools_enabled.is_empty() {
            self.tools_enabled = local.tools_enabled;
        }
        if local.no_degrade {
            self.no_degrade = true;
        }
        if local.delta_explicit {
            self.delta_explicit = true;
        }
        if local.profile.is_some() {
            self.profile = local.profile;
        }
        if local.proxy_timeout_ms.is_some() {
            self.proxy_timeout_ms = local.proxy_timeout_ms;
        }
    }

    /// Loads ONLY the global config file — never merging project-local
    /// `.lean-ctx.toml` overrides, and bypassing the in-memory cache. Every
    /// PERSIST path must use this (or [`Config::update_global`]): [`Config::load`]
    /// folds per-project overrides into the struct, and [`Config::save`] writes
    /// the whole struct back to the GLOBAL file — so a `load → mutate → save`
    /// round-trip silently leaks per-project values (and, historically, reset
    /// customized keys) into the global config (#443). Reading global-only makes
    /// the save leak-free by construction.
    pub fn load_global() -> Self {
        Self::path().map_or_else(Self::default, |p| Self::load_global_from(&p))
    }

    /// Path-parameterized core of [`Config::load_global`] (unit-testable without
    /// the real config dir). Missing, empty, or unparseable files yield
    /// defaults; persisting callers that must not clobber a corrupt file use
    /// [`Config::update_global`], which refuses instead.
    fn load_global_from(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(raw) if !raw.trim().is_empty() => toml::from_str(&raw).unwrap_or_default(),
            _ => Self::default(),
        }
    }

    /// Safely mutate and persist the GLOBAL config. Reads the global file only
    /// (no project-local merge), applies `f`, then writes minimally. Refuses
    /// (returns `Err`) when the file exists but is unparseable, so a typo can
    /// never clobber a customized config (#443). Returns the saved `Config`.
    ///
    /// This is the canonical persistence entry point: prefer it over
    /// `Config::load()` followed by `save()`, which leaks project-local
    /// overrides into the global file.
    pub fn update_global<F>(f: F) -> std::result::Result<Self, super::error::LeanCtxError>
    where
        F: FnOnce(&mut Self),
    {
        let path = Self::path().ok_or_else(|| {
            super::error::LeanCtxError::Config("cannot determine home directory".into())
        })?;
        Self::update_global_at(&path, f)
    }

    /// Path-parameterized core of [`Config::update_global`] (unit-testable).
    fn update_global_at<F>(
        path: &Path,
        f: F,
    ) -> std::result::Result<Self, super::error::LeanCtxError>
    where
        F: FnOnce(&mut Self),
    {
        let mut cfg = match std::fs::read_to_string(path) {
            Ok(raw) if !raw.trim().is_empty() => toml::from_str::<Self>(&raw).map_err(|e| {
                super::error::LeanCtxError::Config(format!(
                    "refusing to modify an unparseable config.toml ({e}); fix it \
                     manually or run `lean-ctx doctor --fix`, then retry"
                ))
            })?,
            _ => Self::default(),
        };
        f(&mut cfg);
        cfg.save_to(path)?;
        Ok(cfg)
    }

    /// Persists the current config to the global config file.
    ///
    /// Preserves user comments, formatting, and unknown keys, keeps the file
    /// minimal (defaults that were never set on disk stay implicit), and writes
    /// atomically with a `.bak` backup so customizations are always recoverable.
    pub fn save(&self) -> std::result::Result<(), super::error::LeanCtxError> {
        let path = Self::path().ok_or_else(|| {
            super::error::LeanCtxError::Config("cannot determine home directory".into())
        })?;
        self.save_to(&path)
    }

    /// Path-parameterized core of [`Config::save`] (unit-testable).
    fn save_to(&self, path: &Path) -> std::result::Result<(), super::error::LeanCtxError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)
            .map_err(|e| super::error::LeanCtxError::Config(e.to_string()))?;
        // Baseline = what loading an empty config yields. This honors serde's
        // field-level `#[serde(default)]` (which can diverge from the struct's
        // `Default` impl), so minimal mode skips exactly the keys that a fresh
        // load would produce — no spurious lines on save.
        let baseline = toml::from_str::<Self>("").unwrap_or_else(|_| Self::default());
        let defaults = toml::to_string_pretty(&baseline)
            .map_err(|e| super::error::LeanCtxError::Config(e.to_string()))?;
        crate::config_io::write_toml_preserving_minimal(path, &content, &defaults)
            .map_err(super::error::LeanCtxError::Config)?;
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
