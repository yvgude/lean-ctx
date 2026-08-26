use serde::{Deserialize, Serialize};

#[allow(clippy::wildcard_imports)]
use super::*;
fn default_true() -> bool {
    true
}

/// Global lean-ctx configuration loaded from `config.toml`, merged with project-local overrides.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub ultra_compact: bool,
    #[serde(default, deserialize_with = "serde_defaults::deserialize_tee_mode")]
    pub tee_mode: TeeMode,
    /// Verbosity of the reactive recovery footer on compressed output
    /// (`off|minimal|full`, default `minimal`). See [`RecoveryHints`].
    #[serde(default)]
    pub recovery_hints: RecoveryHints,
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
    /// Anonymous opt-in telemetry heartbeat (version, OS, arch — no code/PII).
    #[serde(default)]
    pub telemetry: TelemetryConfig,
    #[serde(default)]
    pub cloud: CloudConfig,
    #[serde(default)]
    pub gain: GainConfig,
    /// Model declaration for measured-vs-estimated cost reporting (MCP-only IDEs).
    #[serde(default)]
    pub cost: CostConfig,
    #[serde(default)]
    pub decision_loop: DecisionLoopConfig,
    /// Code-health engine: cognitive complexity, naming, coupling, edit-gate.
    #[serde(default)]
    pub code_health: CodeHealthConfig,
    #[serde(default)]
    pub autonomy: AutonomyConfig,
    #[serde(default)]
    pub providers: ProvidersConfig,
    #[serde(default)]
    pub knowledge_routing: KnowledgeRoutingConfig,
    #[serde(default)]
    pub proxy: ProxyConfig,
    /// Conversation-history compression (`[conversation]`, opt-in; #1123).
    #[serde(default)]
    pub conversation: ConversationConfig,
    /// Proxy-layer response shaping (`[response_shaping]`, #1125).
    #[serde(default)]
    pub response_shaping: ResponseShapingConfig,
    #[serde(default)]
    pub ocla: OclaConfig,
    /// Counterfactual Shadow Mode reporting (`[shadow]`), opt-in.
    #[serde(default)]
    pub shadow: ShadowConfig,
    /// Generalized L1/L2/L3 cache settings (`[cache]`).
    #[serde(default)]
    pub cache: CacheConfig,
    #[serde(default)]
    pub agents: sections::AgentsConfig,
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
    /// require the token; clients must then send `Authorization: Bearer [REDACTED:Authorization header]
    #[serde(default)]
    pub proxy_require_token: bool,
    /// Skip ALL proxy authentication on loopback-bound listeners (#755).
    /// When true **and** the proxy binds a loopback address, every request is
    /// accepted without a Bearer token or provider API key — MCP clients,
    /// browser dashboards, and CLI tools all work without auth setup.
    /// Ignored on non-loopback binds (gateway mode always requires auth).
    /// Env override: `LEAN_CTX_PROXY_LOOPBACK_OPEN`.
    #[serde(default)]
    pub proxy_loopback_open: bool,
    /// Bind address for the proxy listener (gateway mode, enterprise#8).
    /// Default `None` = `127.0.0.1` — local-safe, nothing changes for existing
    /// installs. Set `"0.0.0.0"` (or a specific interface IP) to serve a whole
    /// org from one host; any non-loopback bind hard-disables the provider-key
    /// auth fallback (Bearer token becomes mandatory) and enables the
    /// `proxy_allowed_hosts` Host-header allowlist. Env override:
    /// `LEAN_CTX_PROXY_BIND_HOST`. An unparseable value falls back to loopback,
    /// never to an open bind.
    #[serde(default)]
    pub proxy_bind_host: Option<String>,
    /// Host-header allowlist for a non-loopback proxy bind (gateway mode):
    /// DNS-rebinding protection. Entries are hostnames or IPs without port
    /// (e.g. `"gateway.example.com"`). Loopback names are always allowed.
    /// Ignored (loopback-only guard, today's behavior) while the bind is
    /// loopback. Empty + non-loopback bind = only loopback Host headers pass,
    /// so configure this when exposing the gateway.
    #[serde(default)]
    pub proxy_allowed_hosts: Vec<String>,
    /// Proxy-wide request rate limit in requests/second (token bucket, burst =
    /// 2x). `None` (default) = unlimited on a loopback bind — today's behavior —
    /// and 50 rps with burst 100 on a non-loopback bind (gateway mode ships a
    /// sane floor, enterprise#37). `0` disables the limiter even in gateway
    /// mode (explicit opt-out).
    #[serde(default)]
    pub proxy_max_rps: Option<u32>,
    /// Require Bearer-token authentication for the dashboard. Default `true`:
    /// the dashboard generates (or uses the pinned) token and rejects `/api/*`
    /// and `/metrics` without it. Set to `false` to run the dashboard with **no
    /// auth token** — useful for a local/Docker setup where managing a token is
    /// inconvenient. No-auth mode is not unprotected: cross-origin and CSRF
    /// attacks from a malicious local website are blocked by request-header
    /// validation instead (`Sec-Fetch-Site`, `Origin`/`Host` same-origin, and a
    /// `Host` allowlist against DNS rebinding — see `dashboard::no_auth_request_ok`).
    /// Override per-run via the `--no-auth` / `--auth=<bool>` flag or the
    /// `LEAN_CTX_DASHBOARD_AUTH` env var.
    #[serde(default = "serde_defaults::default_true")]
    pub dashboard_auth: bool,
    /// Provider prompt-cache hit rate for net-of-injection calculation (#1104).
    /// Anthropic ~90%, OpenAI ~50%. Default 0.75 (conservative cross-provider).
    #[serde(default)]
    pub dashboard_cache_hit_rate: Option<f64>,
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
    /// Empty = lean-ctx default (`core`). Add `session` explicitly to evaluate
    /// experimental local collaboration tools.
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
    /// Active task-specific Context Kit. `LEAN_CTX_KIT` takes precedence.
    /// Set with `lean-ctx kit load <name>`.
    #[serde(default)]
    pub active_kit: Option<String>,
    /// Named configuration overlay selected from `[profiles.<name>]`.
    /// `LEAN_CTX_CONFIG_PROFILE` takes precedence over this persisted selector.
    #[serde(default)]
    pub config_profile: Option<String>,
    /// Partial configuration overlays keyed by profile name. Each overlay is
    /// recursively merged over the base configuration at load time.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub profiles: std::collections::BTreeMap<String, toml::Table>,
    /// Tool visibility profile: "minimal" (5), "standard" (15), or "power" (all).
    /// Override via LEAN_CTX_TOOL_PROFILE env var.
    /// Existing installs default to "power" (backward compat).
    #[serde(default)]
    pub tool_profile: Option<String>,
    /// Explicit list of enabled tool names. Used only when no tool_profile is pinned (tool_profile takes precedence); leave tool_profile unset to apply this list.
    /// The universal invoker `ctx_call` stays advertised so unlisted tools remain
    /// reachable — add `ctx_call` to `disabled_tools` to make this allowlist authoritative.
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
    /// Science-driven cognitive features mode (#science).
    #[serde(default)]
    pub cognitive_mode: CognitiveMode,
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
    /// Extra trusted roots OUTSIDE `$HOME` that lean-ctx may follow when an agent
    /// config file/dir (`~/.claude.json`, `~/.codex/config.toml`, …) is a symlink
    /// pointing there (#596). Empty by default → the strict `$HOME`-only boundary
    /// stays in force (a planted symlink can never redirect a config write out of
    /// the user's home, preserving the GL#442 symlink-hijack protection). Add a
    /// parent like `/opt/dotfiles` only for a location you own and trust. Like
    /// `extra_roots`, security-sensitive: stripped from untrusted project-local
    /// configs. Override via LEAN_CTX_ALLOW_SYMLINK_ROOTS env var (path-list sep).
    #[serde(default)]
    pub allow_symlink_roots: Vec<String>,
    /// Enable content-defined chunking (Rabin-Karp) for cache-optimal output ordering.
    /// Stable chunks are emitted first to maximize prompt cache hits.
    #[serde(default)]
    pub content_defined_chunking: bool,
    /// Skip session/knowledge/gotcha blocks in MCP instructions to minimize token overhead.
    /// Override via LEAN_CTX_MINIMAL env var.
    ///
    /// Default `true` (deliberate): initialize-time instructions stay byte-stable
    /// across sessions, which keeps the provider prompt-cache prefix warm (#498)
    /// and holds the fixed per-session cost at the `doctor overhead --gate`
    /// budget. Session continuity is NOT lost — the wakeup briefing (task,
    /// findings, knowledge) is delivered through the first tool call's
    /// `--- AUTO CONTEXT ---` block instead, which only bills when the agent
    /// actually works. Set to `false` to additionally inject the ACTIVE SESSION
    /// / PROJECT MEMORY blocks directly into the MCP `initialize` instructions.
    /// #1540: this key never disables the archive/ephemeral firewall — only
    /// the `LEAN_CTX_MINIMAL` env escape hatch skips that safety net.
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
    /// Progressive disclosure for first-time reads (LCLM arXiv 2606.09659).
    /// When true (default), large files default to compact overviews on first read:
    ///     - Below progressive_threshold_lines: full content
    ///     - Below progressive_signatures_max_lines: signatures mode
    ///     - Above: map (manifest) mode
    /// Models can always bypass with explicit mode= or lines= parameters.
    /// Override via LEAN_CTX_PROGRESSIVE_DISCLOSURE env var.
    #[serde(default = "serde_defaults::default_true")]
    pub progressive_disclosure: bool,
    /// Files with fewer lines than this threshold are always delivered in full.
    /// Default: 100 lines.
    #[serde(default = "serde_defaults::default_progressive_threshold_lines")]
    pub progressive_threshold_lines: u32,
    /// Files between threshold and this limit get signatures mode.
    /// Files above get map (manifest) mode. Default: 500 lines.
    #[serde(default = "serde_defaults::default_progressive_signatures_max")]
    pub progressive_signatures_max: u32,
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
    /// Index-time file filters (#735): include/exclude globs + gitignore
    /// handling, applied by every index builder via `core::index_filter`.
    #[serde(default)]
    pub index: IndexConfig,
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
    /// Shadow mode (default: true): denies native Read/Grep/Glob at the
    /// permission level, forcing agents to use ctx_* MCP tools for maximum
    /// compression. Without this, many harnesses silently prefer native tools,
    /// negating lean-ctx's token savings. Disable with `shadow_mode = false`.
    /// Native Shell/Bash coverage is governed separately by `shell_hook_mode`.
    #[serde(default = "serde_defaults::default_true")]
    pub shadow_mode: bool,
    /// Shell hook mode (#1278): how the PreToolUse shell hook treats native
    /// Bash/Shell calls.
    /// - `rewrite` (default): rewrite known read/search/list commands to
    ///   lean-ctx equivalents; anything unrecognized passes through raw.
    /// - `deny`: block native shell calls outright so every command goes
    ///   through `ctx_shell` (same fail-opens as the deny hook: lean-ctx
    ///   disabled, MCP daemon dead, explicit shadow-only tool surface).
    /// - `auto`: currently equals `rewrite`; reserved for a future default flip.
    ///
    /// Env override: `LEAN_CTX_SHELL_HOOK_MODE`. Lives in config.toml, so the
    /// choice survives reinstalls and updates.
    #[serde(default)]
    pub shell_hook_mode: Option<String>,
    /// Per-turn tool-precedence reinjection (#1288): the UserPromptSubmit hook
    /// emits a one-line `additionalContext` reminder that ctx_* tools are the
    /// mandated path, so it always post-dates any session-level harness
    /// instruction preferring native Bash. `on` | `off` | `auto` (default:
    /// auto = active only while `shadow_mode` is on). Env override:
    /// `LEAN_CTX_PROMPT_REINJECT`. Costs ~45 tokens per turn when active.
    #[serde(default)]
    pub prompt_reinject: Option<String>,
    /// Global hook mode override. When set, overrides the per-agent auto-detection.
    /// - `replace`: Native Read/Grep/Glob/Shell denied, lean-ctx MCP is the only path
    /// - `hybrid`: MCP + shell hooks for compression (legacy)
    /// - `mcp`: MCP server only, no hooks
    ///
    /// Default: unset (auto-detect per agent via `recommend_hook_mode`)
    #[serde(default)]
    pub hook_mode: Option<String>,
    /// MCP tool surface mode. Controls how many tools `tools/list` advertises.
    /// - `auto` (default): shadow-only (`ctx_call` only) for hook-covered
    ///   clients, full lazy-core surface for all others.
    /// - `mcp`: always advertise the full lazy-core/profile surface.
    /// - `shadow`: always advertise only `ctx_call` (requires installed hooks).
    #[serde(default)]
    pub tool_surface: Option<String>,
    /// Opt-in (#520): write a human-readable debug log of intercepted MCP tool
    /// calls and hook routing decisions (lean-ctx vs native, with reasons) to
    /// `<state_dir>/logs/debug.log`. Override via the LEAN_CTX_DEBUG_LOG env var.
    #[serde(default)]
    pub debug_log: bool,
    /// Controls when the shell hook auto-activates aliases.
    /// - `agents-only`: (Default since #699) Aliases only active when an AI
    ///   agent env var is detected — transparent in plain human terminals.
    /// - `always`: Aliases active in every interactive shell (pre-#699 default).
    /// - `off`: Aliases never auto-activate (user must call `lean-ctx-on` manually).
    ///
    /// Override via `LEAN_CTX_SHELL_ACTIVATION` env var.
    #[serde(default)]
    pub shell_activation: ShellActivation,
    /// Do not install agent CLI aliases (`claude`, `codex`, `gemini`,
    /// `codebuddy`) into `~/.zshrc` / `~/.bashrc` during `onboard` / `setup`.
    /// Existing alias blocks are removed when this is toggled on (#754).
    /// Does NOT affect the shell compression hook (`_lc()`) — use
    /// `shell_hook_disabled` for that. Orthogonal to `shell_activation` which
    /// controls *when* aliases activate, not *whether* they are installed.
    #[serde(default)]
    pub skip_agent_aliases: bool,
    /// Controls the native-Read → `ctx_read` redirect hook (#637).
    /// - `auto`: (Default) redirect everywhere except hosts with a native
    ///   read-before-write guard (Claude Code / CodeBuddy), where the path-swap
    ///   would break native Write/Edit.
    /// - `on`: always redirect (legacy behavior).
    /// - `off`: never redirect native Read.
    ///
    /// Override via the `LEAN_CTX_READ_REDIRECT` env var.
    #[serde(default)]
    pub read_redirect: ReadRedirect,
    /// Controls the PostToolUse native-Read re-read dedup (GL #1140).
    /// - `auto`: (Default) replace only re-reads of unchanged files, and only on
    ///   guard hosts (Claude Code / CodeBuddy) where the PreToolUse redirect is
    ///   disabled — the guard-safe way to win the dedup savings back.
    /// - `on`: dedup wherever the PostToolUse hook fires.
    /// - `off`: never replace a Read result.
    ///
    /// Override via the `LEAN_CTX_READ_DEDUP` env var.
    #[serde(default)]
    pub read_dedup: ReadDedup,
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
    /// Values: "shared" (default, 1h TTL), "aggressive" (5 min TTL for low-memory devices).
    /// Override via LEAN_CTX_MEMORY_CLEANUP env var.
    #[serde(default)]
    pub memory_cleanup: MemoryCleanup,
    /// Soft process-RSS target as a percentage of system RAM (default: 5).
    /// The guardian throttles and evicts above it, but this is not an OS hard cap.
    /// Use a cgroup/container MemoryMax when strict isolation is required.
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
    /// Controls compression annotation style in savings footers.
    /// Values: "quantized" (default, round to 10% buckets), "full" (exact %), "none" (suppress all).
    /// Override via LEAN_CTX_COMPRESSION_ANNOTATION env var.
    #[serde(default)]
    pub compression_annotation: CompressionAnnotation,
    /// Minimum savings percentage to emit a footer annotation. Below this threshold,
    /// annotations are suppressed (the savings are too small to be worth the token cost).
    /// Default: 5 (suppress annotations for savings below 5%).
    #[serde(default = "serde_defaults::default_annotation_threshold_pct")]
    pub annotation_threshold_pct: u8,
    /// In-band behavior hints (heavy full/raw reads, search→read→search
    /// chains): "auto" (default, one hint per pattern per session) or "off".
    /// Detections are always counted for `tools health`, hints or not.
    #[serde(default = "serde_defaults::default_behavior_nudges")]
    pub behavior_nudges: String,
    /// Maximum fresh tokens per single tool response (turn budget).
    /// 0 = unlimited. Default: 4096. Prevents context bloat from oversized responses.
    /// Override via LEAN_CTX_TURN_FRESH_LIMIT env var.
    #[serde(default = "serde_defaults::default_turn_fresh_limit")]
    pub turn_fresh_limit: usize,
    /// Maximum cumulative fresh tokens per session. 0 = unlimited.
    /// Default: 200000. Progressive compression kicks in at 50/75/90%.
    /// Override via LEAN_CTX_SESSION_TOKEN_LIMIT env var.
    #[serde(default = "serde_defaults::default_session_token_limit")]
    pub session_token_limit: usize,
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
    /// built-in default (2M). `LEAN_CTX_CACHE_MAX_TOKENS` env var overrides this.
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
    pub gateway: crate::core::mcp_catalog::GatewayConfig,
    /// Self-hosted org gateway server (`[gateway_server]`, enterprise#20):
    /// deployment parameters for the usage cockpit — seat count for the
    /// org-wide projection, display label, and the central admin API the local
    /// cockpit may read from. All optional; absent = local-only behavior.
    #[serde(default)]
    pub gateway_server: GatewayServerConfig,
    /// Enterprise Suite connection (`[enterprise]`): connects this Runtime to
    /// a LeanCTX Enterprise Gateway for economics tracking and model routing.
    #[serde(default)]
    pub enterprise: EnterpriseConfig,
    /// Deprecated: auto-reroot is now always active when the jail root is
    /// suspicious, None, or has no project marker. Kept for config-file
    /// backward compatibility but no longer controls behavior.
    #[serde(default = "default_true")]
    pub allow_auto_reroot: bool,
    /// Verbatim binary path/expression for generated agent-hook commands
    /// (#708). Users who sync agent settings (`~/.claude/settings.json`, …)
    /// across machines with different usernames need an env-based form like
    /// `$HOME/.local/bin/lean-ctx` — agent hosts run hook commands through a
    /// shell, which expands it. When set (env `LEAN_CTX_HOOK_BINARY` wins,
    /// then this key), every hook writer emits the value verbatim instead of
    /// the machine-absolute exe path, so `init`/`doctor --fix`/`update` stop
    /// rewriting synced files into sync ping-pong. Autostart plists/services
    /// and daemon spawns are NOT affected — launchd/systemd do not expand
    /// shell variables, so those keep the real absolute path. Empty (default)
    /// = automatic absolute-path resolution (#367).
    #[serde(default)]
    pub hook_binary: Option<String>,
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

    /// Extra command prefixes that get the heavy timeout ceiling. Merged with
    /// the built-in list. Useful for project-specific long-running scripts.
    /// Example: `shell_heavy_prefixes = ["python3 ", "./scripts/"]`
    #[serde(default)]
    pub shell_heavy_prefixes: Vec<String>,
    /// When true, `ctx_shell` accepts shell file-write redirects (`>`, `>>`,
    /// `tee`, heredoc-to-file, `curl -o`, `wget` default mode). Default false —
    /// the native Write/Edit tool is preferred. Opt-in for power users who want
    /// classic shell syntax; the real command gating (allowlist,
    /// dangerous-pattern and interpreter-eval blocks) still applies. Override
    /// via `LEAN_CTX_SHELL_ALLOW_WRITES=1`.
    #[serde(default)]
    pub shell_allow_writes: bool,
    /// Absolute paths where shell redirects and `tee` may capture output.
    /// Empty uses the operating system's temporary directories. Project files
    /// remain denied even when a configured path overlaps the project root.
    #[serde(default)]
    pub write_allow_paths: Vec<String>,

    /// #814: opt-in to allow `python3 -c`, `node -e`, etc. in ctx_shell.
    /// Default `false` — inline code is blocked because it leaves no auditable
    /// artifact. Override via `LEAN_CTX_SHELL_ALLOW_INLINE_SCRIPTS=1`.
    #[serde(default)]
    pub shell_allow_inline_scripts: bool,

    /// Setup behavior: controls what gets injected during setup and updates.
    #[serde(default)]
    pub setup: SetupConfig,
    #[serde(default)]
    pub solution: super::solution::SolutionConfig,
    #[serde(default)]
    pub provenance: super::provenance::ProvenanceConfig,
    pub cross_agent: super::provenance::CrossAgentConfig,
}
