# Appendix — Configuration Keys (generated)

<!-- GENERATED FILE — do not edit by hand. Run: `cargo run --example gen_docs --features dev-tools` -->

Source of truth: `rust/src/core/config/schema.rs`.

lean-ctx reads `~/.lean-ctx/config.toml` (and a project `.lean-ctx.toml` overlay). Below is every recognized key with its type, default, and environment-variable override where one exists.

## Top-level keys

Top-level configuration keys

- `agent_token_budget` (usize, default `0`) — Default per-agent token budget. 0 = unlimited
- `allow_auto_reroot` (bool, default `false` — env `LEAN_CTX_ALLOW_REROOT`) — Allow automatic project-root re-rooting when absolute paths outside the jail are seen
- `allow_paths` (string[], default `[]` — env `LEAN_CTX_ALLOW_PATH`) — Additional paths allowed by PathJail (absolute)
- `auto_capture` (bool, default `true`) — Automatic knowledge capture from tool findings
- `bm25_max_cache_mb` (u64, default `128` — env `LEAN_CTX_BM25_MAX_CACHE_MB`) — Maximum BM25 cache file size in MB
- `buddy_enabled` (bool, default `true`) — Enable the buddy system for multi-agent coordination
- `cache_policy` (enum(aggressive|safe|off), default `aggressive` — env `LEAN_CTX_CACHE_POLICY`) — Cache policy for ctx_read: aggressive (13-tok stubs), safe (map on hit), off (always disk)
- `checkpoint_interval` (u32, default `15`) — Session checkpoint interval in minutes
- `compression_level` (enum: off | lite | standard | max, default `lite` — env `LEAN_CTX_COMPRESSION`) — Unified output-style level for the model's prose (not tool-output compression). lite=plain concise (default), standard/max=denser symbolic 'power modes'
- `content_defined_chunking` (bool, default `false`) — Enable Rabin-Karp chunking for cache-optimal output ordering
- `custom_aliases` (array, default `[]`) — Custom command aliases (array of {command, alias} entries)
- `default_tool_categories` (string[], default `[]`) — Tool categories active by default (core, arch, debug, memory, metrics, session). Override via LCTX_DEFAULT_CATEGORIES
- `disabled_tools` (string[], default `[]`) — Tools to exclude from the MCP tool list
- `enable_wakeup_ctx` (bool, default `true`) — Append wakeup briefing (facts, session summary) to ctx_overview output. Set false to reduce context bloat when calling ctx_overview frequently.
- `excluded_commands` (string[], default `[]`) — Commands to exclude from shell hook interception
- `extra_ignore_patterns` (string[], default `[]`) — Extra glob patterns to ignore in graph/overview/preload
- `extra_roots` (string[], default `[]` — env `LEAN_CTX_EXTRA_ROOTS`) — Extra project roots for multi-root workspaces (auto-added to PathJail allow-list)
- `graph_index_max_files` (u64, default `0`) — Maximum files in graph index. 0 = unlimited (default). Set >0 to cap for constrained systems
- `journal_enabled` (bool, default `true`) — Write human-readable activity journal to ~/.lean-ctx/journal.md
- `max_disk_mb` (u64, default `0` — env `LEAN_CTX_MAX_DISK_MB`) — Simplified disk budget in MB (0 = disabled). Distributes: archive ~25%, BM25 ~10%
- `max_ram_percent` (u8, default `5` — env `LEAN_CTX_MAX_RAM_PERCENT`) — Maximum percentage of system RAM that lean-ctx may use (1-50, default 5)
- `max_staleness_days` (u32, default `0` — env `LEAN_CTX_MAX_STALENESS_DAYS`) — Auto-purge data older than N days (0 = disabled). Flows into archive.max_age_hours
- `memory_cleanup` (enum: aggressive | shared, default `aggressive` — env `LEAN_CTX_MEMORY_CLEANUP`) — Controls how aggressively memory is freed when idle
- `memory_profile` (enum: low | balanced | performance, default `performance` — env `LEAN_CTX_MEMORY_PROFILE`) — Controls RAM vs feature trade-off (performance = max quality)
- `minimal_overhead` (bool, default `true` — env `LEAN_CTX_MINIMAL`) — Skip session/knowledge/gotcha blocks in MCP instructions
- `no_degrade` (boolean, default `false`) — Disable all automatic read-mode degradation. Override via LCTX_NO_DEGRADE=1
- `output_density` (enum: normal | terse | ultra, default `normal` — env `LEAN_CTX_OUTPUT_DENSITY`) — Controls how dense/compact MCP tool output is formatted
- `passthrough_urls` (string[], default `[]`) — URLs to pass through without proxy interception
- `permission_inheritance` (enum: off | on, default `off`) — Mirror the host IDE's permission rules onto lean-ctx tools (v1: OpenCode). When on, ctx_shell honors your bash/rm * rules instead of bypassing them. Override via LEAN_CTX_PERMISSION_INHERITANCE
- `preserve_compact_formats` (string[], default `["toon"]`) — Already-compact output formats preserved verbatim instead of recompressed (e.g. ["toon"]). Set to [] to disable
- `profile` (string, default `""`) — Persistent profile name. Checked after LEAN_CTX_PROFILE env var. Set via: lean-ctx config set profile passthrough
- `project_root` (string?, default `null` — env `LEAN_CTX_PROJECT_ROOT`) — Explicit project root directory. Prevents accidental home-directory scans
- `proxy_enabled` (bool?, default `null`) — Enable/disable the proxy layer. null = auto-detect, true = force on, false = force off
- `proxy_port` (u16?, default `null`) — Custom proxy port (default: 4444). Useful for multi-user systems. Env: LEAN_CTX_PROXY_PORT
- `proxy_timeout_ms` (u64?, default `null`) — Proxy reachability timeout in ms (default: 200). Override via LEAN_CTX_PROXY_TIMEOUT_MS
- `redirect_exclude` (string[], default `[]`) — URL patterns to exclude from proxy redirection
- `reference_results` (bool, default `false` — env `LEAN_CTX_REFERENCE_RESULTS`) — Store large tool outputs as references instead of inline content
- `response_verbosity` (enum: normal | compact | minimal, default `normal` — env `LEAN_CTX_RESPONSE_VERBOSITY`) — Controls how verbose tool responses are
- `rules_injection` (enum: shared | dedicated, default `shared`) — How rules load for CLAUDE.md/AGENTS.md/GEMINI.md agents: shared block, or dedicated (no shared-file edits; SessionStart hook / instructions[] / context.fileName). Override via LEAN_CTX_RULES_INJECTION
- `rules_scope` (enum: both | global | project, default `both`) — Where agent rule files are installed. Override via LEAN_CTX_RULES_SCOPE
- `sandbox_level` (u8, default `0` — env `LEAN_CTX_SANDBOX_LEVEL`) — Sandbox strictness level (0=default, 1=strict, 2=paranoid)
- `savings_footer` (enum: auto | always | never, default `always` — env `LEAN_CTX_SAVINGS_FOOTER`) — Controls visibility of token savings footers: always (default, show on every response), never, auto (context-dependent). Also: LEAN_CTX_SHOW_SAVINGS=1|0
- `shadow_mode` (bool, default `false` — env `LEAN_CTX_SHADOW_MODE`) — Transparently intercept native Read/Grep/Shell calls via hooks and route them through lean-ctx
- `shell_activation` (enum: always | agents-only | off, default `always` — env `LEAN_CTX_SHELL_ACTIVATION`) — Controls when the shell hook auto-activates aliases
- `shell_allowlist` (array, default `[]` — env `LEAN_CTX_SHELL_ALLOWLIST`) — Optional shell command allowlist. When non-empty, only listed binaries are permitted
- `shell_allowlist_extra` (array, default `[]`) — Commands merged on top of shell_allowlist without replacing the defaults. Managed via `lean-ctx allow <cmd>`
- `shell_hook_disabled` (bool, default `false` — env `LEAN_CTX_NO_HOOK`) — Disable shell hook injection
- `shell_strict_mode` (bool, default `false`) — Block $(), backticks, <() in shell arguments. Default false = warn only.
- `slow_command_threshold_ms` (u64, default `5000`) — Commands taking longer than this (ms) are recorded in the slow log. Set to 0 to disable
- `symbol_map_auto` (bool, default `false`) — Opt-in: α-code identifier substitution in aggressive reads (>50-file projects). Off by default — abbreviated symbols hinder editing/refactoring
- `tee_mode` (enum: never | failures | always, default `failures`) — Controls when shell output is tee'd to disk for later retrieval
- `terse_agent` (enum: off | lite | full | ultra, default `off` — env `LEAN_CTX_TERSE_AGENT`) — Controls agent output verbosity via instructions injection
- `theme` (string, default `default`) — Dashboard color theme
- `tool_profile` (enum: minimal | standard | power, default `""`) — Tool visibility profile: minimal (6 tools), standard (22), power (all). Override via LEAN_CTX_TOOL_PROFILE
- `tools_enabled` (string[], default `[]`) — Explicit list of enabled tool names (overrides tool_profile when non-empty)
- `ultra_compact` (bool, default `false`) — Legacy flag for maximum compression (use compression_level instead)
- `update_check_disabled` (bool, default `false` — env `LEAN_CTX_NO_UPDATE_CHECK`) — Disable the daily version check

## `[archive]`

Settings for the zero-loss compression archive (large tool outputs saved to disk)

- `enabled` (bool, default `true`) — Enable zero-loss compression archive
- `ephemeral` (bool, default `true`) — Replace large results with summary+ref (ctx_expand to retrieve). Env: LEAN_CTX_EPHEMERAL
- `ephemeral_min_tokens` (usize, default `2000`) — Minimum output tokens before the ephemeral firewall replaces inline body with summary+ref. Env: LEAN_CTX_EPHEMERAL_MIN_TOKENS
- `max_age_hours` (u64, default `48`) — Maximum age of archived entries before cleanup
- `max_disk_mb` (u64, default `500`) — Maximum total disk usage for the archive
- `threshold_chars` (usize, default `800`) — Minimum output size (chars) to trigger archiving

## `[autonomy]`

Controls autonomous background behaviors (preload, dedup, consolidation)

- `auto_consolidate` (bool, default `true`) — Auto-consolidate knowledge periodically
- `auto_dedup` (bool, default `true`) — Auto-deduplicate repeated reads
- `auto_preload` (bool, default `true`) — Auto-preload related files on first read
- `auto_related` (bool, default `true`) — Auto-load graph-related files
- `cognition_loop_enabled` (bool, default `true` — env `LEAN_CTX_COGNITION_LOOP_ENABLED`) — Enable the background cognition loop (periodic knowledge consolidation)
- `cognition_loop_interval_secs` (u64, default `3600` — env `LEAN_CTX_COGNITION_LOOP_INTERVAL_SECS`) — Seconds between cognition loop iterations
- `cognition_loop_max_steps` (u8, default `8` — env `LEAN_CTX_COGNITION_LOOP_MAX_STEPS`) — Maximum steps per cognition loop iteration
- `consolidate_cooldown_secs` (u64, default `120`) — Minimum seconds between consolidation runs
- `consolidate_every_calls` (u32, default `25`) — Consolidate knowledge every N tool calls
- `dedup_threshold` (usize, default `8`) — Number of repeated reads before dedup triggers
- `enabled` (bool, default `true`) — Enable autonomous background behaviors
- `silent_preload` (bool, default `true`) — Suppress preload notifications in output

## `[boundary_policy]`

Cross-project boundary and access control policies

- `audit_cross_access` (bool, default `true`) — Log audit events when cross-project access occurs
- `cross_project_import` (bool, default `false`) — Allow importing knowledge from other projects
- `cross_project_search` (bool, default `false`) — Allow searching across project boundaries
- `universal_gotchas_enabled` (bool, default `true`) — Load universal (cross-project) gotchas

## `[cloud]`

Cloud feature settings

- `contribute_enabled` (bool, default `false`) — Enable contributing anonymized stats to lean-ctx cloud

## `[custom_aliases]`

Custom command aliases (array of {command, alias} entries). Note: field names are 'command' and 'alias' (not 'name')

- `alias` (string, default `""`) — The alias definition to execute
- `command` (string, default `""`) — The command pattern to match (e.g. 'deploy')

## `[embedding]`

Semantic-embedding engine settings (model selection for ctx_semantic_search)

- `model` (string, default `minilm` — env `LEAN_CTX_EMBEDDING_MODEL`) — Local ONNX embedding model for ctx_semantic_search. One of: minilm (all-MiniLM-L6-v2, 384d, default), jina-code-v2 (768d, code-optimized), nomic (768d). Switching models re-indexes once on the next search.

## `[gain]`

Token-savings recap publishing (gain --publish / auto-publish)

- `auto_publish` (bool, default `false`) — Automatically (re)publish your Wrapped recap when you run `lean-ctx gain` (opt-in, off by default; throttled and sends only an aggregate payload)
- `auto_publish_interval_hours` (u64, default `24`) — Minimum hours between automatic publishes (throttle; default 24)
- `display_name` (string?, default `null`) — Optional display name shown on your published card / leaderboard entry
- `leaderboard` (bool, default `true`) — When auto-publishing, also list the card on the public opt-in leaderboard

## `[gateway]`

MCP Tool-Catalog Gateway: aggregate + query-route downstream MCP servers (#210). Global-only.

- `cache_ttl_secs` (integer, default `300`) — Aggregated-catalog cache lifetime in seconds
- `call_timeout_secs` (integer, default `30`) — Per-operation timeout for downstream connect/list/call (seconds)
- `enabled` (bool, default `false`) — Enable the MCP Tool-Catalog Gateway (no-op when false)
- `top_n` (integer, default `5`) — How many tools `ctx_tools find` returns per query (clamped 1..=50)

## `[gateway.servers]`

Downstream MCP servers (array of tables: `[[gateway.servers]]`)

- `args` (array, default `[]`) — Arguments for the spawned command (stdio transport)
- `command` (string, default `""`) — Executable to spawn (stdio transport)
- `enabled` (bool, default `true`) — Per-server switch (default true)
- `env` (table, default `{}`) — Extra environment variables for the child process (stdio transport)
- `headers` (table, default `{}`) — Extra request headers, e.g. Authorization (http transport)
- `name` (string, default `""`) — Stable server id; becomes the catalog namespace (`name::tool`)
- `transport` (string, default `stdio`) — Transport: stdio (spawn command) or http (connect to url)
- `url` (string, default `""`) — Streamable-HTTP endpoint (http transport)

## `[ide_paths]`

Per-IDE allowed paths. Keys are agent names (cursor, codex, opencode, antigravity, etc.), values are arrays of paths to index for that agent

_No sub-keys (presence of the section toggles the feature)._

## `[llm]`

Optional LLM enhancement settings (query expansion, contradiction explanation). Deterministic fallback when disabled or unreachable.

- `api_key` (string, default `""`) — API key for OpenRouter or Anthropic backends
- `backend` (enum: ollama | openrouter | anthropic, default `ollama`) — LLM backend provider
- `enabled` (bool, default `false`) — Enable optional LLM enhancements (query expansion, contradiction explanation)
- `model` (string, default `llama3.2`) — Model name for the selected backend
- `timeout_secs` (u64, default `10`) — HTTP timeout for LLM requests

## `[loop_detection]`

Loop detection settings for preventing repeated identical tool calls

- `blocked_threshold` (u32, default `0`) — Repetitions before blocking. 0 = disabled
- `normal_threshold` (u32, default `2`) — Repetitions before reducing output
- `reduced_threshold` (u32, default `4`) — Repetitions before further reducing output
- `search_group_limit` (u32, default `10`) — Maximum unique searches within a loop window
- `tool_total_limits` (table, default `{"ctx_read":100,"ctx_search":80,"ctx_semantic_search":60,"ctx_shell":50}`) — Per-tool total call limits within a session. Keys are tool names, values are max calls
- `window_secs` (u64, default `300`) — Time window in seconds for loop detection

## `[lsp]`

LSP server binary overrides. Map language name to custom binary path

- `go` (string?, default `null`) — Custom path to gopls binary
- `python` (string?, default `null`) — Custom path to pylsp binary
- `rust` (string?, default `null`) — Custom path to rust-analyzer binary
- `typescript` (string?, default `null`) — Custom path to typescript-language-server binary

## `[memory.embeddings]`

Embeddings memory settings for semantic search

- `max_facts` (usize, default `2000`) — Maximum number of embedding facts stored

## `[memory.episodic]`

Episodic memory budgets (session episodes)

- `max_actions_per_episode` (usize, default `50`) — Maximum actions tracked per episode
- `max_episodes` (usize, default `500`) — Maximum number of episodes retained
- `summary_max_chars` (usize, default `200`) — Maximum characters in episode summary

## `[memory.gotcha]`

Gotcha memory settings (project-specific warnings and pitfalls)

- `default_decay_rate` (f32, default `0.03`) — Default decay rate for gotcha importance
- `max_gotchas_per_project` (usize, default `100`) — Maximum gotchas stored per project
- `retrieval_budget_per_room` (usize, default `10`) — Maximum gotchas retrieved per room per query

## `[memory.knowledge]`

Knowledge memory budgets (facts, patterns, gotchas)

- `contradiction_threshold` (f32, default `0.5`) — Confidence threshold for contradiction detection
- `max_facts` (usize, default `200`) — Maximum number of knowledge facts stored per project
- `max_history` (usize, default `100`) — Maximum history entries retained
- `max_patterns` (usize, default `50`) — Maximum number of patterns stored
- `recall_facts_limit` (usize, default `10`) — Maximum facts returned per recall query
- `relations_limit` (usize, default `40`) — Maximum number of relations returned
- `rooms_limit` (usize, default `25`) — Maximum number of rooms returned
- `timeline_limit` (usize, default `25`) — Maximum number of timeline entries returned

## `[memory.lifecycle]`

Knowledge lifecycle policy (decay, staleness, dedup)

- `decay_rate` (f32, default `0.01`) — Rate at which knowledge confidence decays over time
- `low_confidence_threshold` (f32, default `0.3`) — Threshold below which facts are considered low-confidence
- `similarity_threshold` (f32, default `0.85`) — Similarity threshold for deduplication
- `stale_days` (i64, default `30`) — Days after which unused facts are considered stale

## `[memory.procedural]`

Procedural memory budgets (learned patterns)

- `max_procedures` (usize, default `100`) — Maximum number of learned procedures stored
- `max_window_size` (usize, default `10`) — Maximum window size for pattern analysis
- `min_repetitions` (usize, default `3`) — Minimum repetitions before a pattern is stored
- `min_sequence_len` (usize, default `2`) — Minimum sequence length for procedure detection

## `[providers]`

External context providers (GitHub, GitLab, Jira, MCP bridges, etc.). Set tokens via env vars (GITHUB_TOKEN, GITLAB_TOKEN). MCP bridges connect external MCP servers as context sources.

- `auto_index` (bool, default `true`) — Auto-ingest provider results into BM25/embedding indexes
- `cache_ttl_secs` (u64, default `120`) — Default cache TTL for provider results (seconds)
- `enabled` (bool, default `true`) — Master switch for the provider subsystem (GitHub, GitLab, etc.)
- `github.api_url` (string, default `null`) — GitHub API base URL (for GitHub Enterprise)
- `github.enabled` (bool, default `true`) — Enable/disable GitHub provider
- `gitlab.api_url` (string, default `null`) — GitLab API base URL (for self-hosted instances)
- `gitlab.enabled` (bool, default `true`) — Enable/disable GitLab provider
- `mcp_bridges.<name>.args` (array, default `[]`) — Arguments for the MCP server command
- `mcp_bridges.<name>.auth_env` (string, default `null`) — Environment variable name containing auth token for MCP server
- `mcp_bridges.<name>.command` (string, default `null`) — Command to spawn a local MCP server (stdio transport)
- `mcp_bridges.<name>.url` (string, default `null`) — HTTP/SSE URL for a remote MCP server

## `[proxy]`

Proxy upstream configuration for API routing

- `anthropic_upstream` (string?, default `null`) — Custom upstream URL for Anthropic API proxy
- `gemini_upstream` (string?, default `null`) — Custom upstream URL for Gemini API proxy
- `openai_upstream` (string?, default `null`) — Custom upstream URL for OpenAI API proxy

## `[search]`

Hybrid search weights for ctx_semantic_search (BM25 + dense vector + SPLADE + graph proximity)

- `bm25_candidates` (usize, default `75`) — Number of BM25 candidates to retrieve before fusion
- `bm25_weight` (f64, default `1.0`) — BM25 lexical search weight in RRF fusion
- `dense_candidates` (usize, default `75`) — Number of dense candidates to retrieve before fusion
- `dense_weight` (f64, default `1.0`) — Dense vector search weight in RRF fusion
- `splade_weight` (f64, default `0.5`) — SPLADE expansion weight (0.0 to disable)

## `[secret_detection]`

Secret/credential detection and redaction settings

- `custom_patterns` (array, default `[]`) — Additional regex patterns to detect as secrets
- `enabled` (bool, default `true`) — Enable secret/credential detection in tool outputs
- `redact` (bool, default `true`) — Redact detected secrets from output

## `[sensitivity]`

Per-item sensitivity model with a uniform policy floor (#212)

- `action` (string, default `redact`) — How to enforce the floor: redact (mask spans) or drop (withhold item)
- `enabled` (bool, default `false`) — Enable the per-item sensitivity policy floor (no-op when false)
- `policy_floor` (string, default `secret`) — Block items at/above this level: public|internal|confidential|secret

## `[setup]`

Controls what lean-ctx injects during setup and updates. Fresh installs default to non-invasive (rules/skills off, MCP on).

- `auto_inject_rules` (bool?, default `null`) — Inject agent rule files during setup/update. null=auto (inject if already present), true=always, false=never
- `auto_inject_skills` (bool?, default `null`) — Install SKILL.md files during setup/update. null=auto (install if rules present), true=always, false=never
- `auto_update_mcp` (bool, default `true`) — Register lean-ctx MCP server in editor configs during setup/update

## `[updates]`

Automatic update configuration

- `auto_update` (bool, default `false`) — Enable automatic updates (requires explicit opt-in)
- `check_interval_hours` (u64, default `6`) — How often to check for updates (hours)
- `notify_only` (bool, default `false`) — Only notify about updates, don't install automatically

