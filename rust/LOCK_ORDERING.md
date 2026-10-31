# Lock Ordering — lean-ctx Rust Codebase

This document catalogues every global/static lock and notable `Arc<Mutex/RwLock>` in the
codebase, defines the intended acquisition order, and records rules for async code.

---

## 1. Global / Static Locks

Canonical session saves reuse the existing per-session OS file lock. Each live
`SessionState` additionally has a serde-skipped `Arc<tokio::sync::Mutex<()>>`
save lane, shared by its clones. `save_shared` copies the lane under the state
read lock, drops that guard, then acquires the lane with a five-second timeout.
While holding the lane it briefly locks state to prepare, drops state before
`spawn_blocking` acquires the OS file lock, and reacquires state only after the
file lock is released, to acknowledge the actual committed primary. Never wait
for the lane while holding the session state lock. Synchronous saves use the
same OS writer and exact canonical base-digest check; they do not acquire the
async lane. The lane is scheduling only, not another persisted state authority.

The optional Intelligence installer holds one independent OS file lock,
`<explicit-private-install-root>/install.lock`, using nonblocking exclusive
acquisition. It never nests another lock, performs network I/O, starts a process,
or changes user context state while held. Package publication and the shared
updater atomic-write primitive run under this lock; public CLI updates use their
own separate lock and directory.

MCP read continuity copies only the completed ledger entry under the ledger
state guard, releases that guard, then uses `spawn_blocking` to acquire the
existing `context_ledger.json.lock` via bounded `agents::FileLock`. Under that
file lock it reloads and upserts the one entry, preserving other writers and
explicit pinned/excluded controls. No state lock or nested file lock is held.
Persistence failure rejects the MCP call; shutdown never flushes a stale ledger
snapshot. Legacy explicit snapshot save/reset/import paths remain unchanged.

Telemetry OS-file locks follow aggregate → one-shot → ledger, skipping levels
when not needed. A ledger operation never acquires either upstream lock.
Send holds the aggregate lease across HTTP, ledger append and acknowledgement;
acknowledgement takes the one-shot lock only after the ledger guard is released.
Version recording may take ledger under one-shot; purge/rotation callbacks must
not reacquire aggregate or one-shot. Each production acquisition has a 750-ms
retry budget (preview try-locks aggregate, then waits that budget for one-shot);
scheduling may delay return.
The telemetry-v2 batch POST has a ten-second global transport timeout; this does
not impose a deadline on filesystem I/O or promise cancellation of an in-flight
request after opt-out. Its fresh config read takes no config writer/cache lock.
An acknowledgement timeout preserves the frozen pending batch for at-least-once
retry. A purge/rotation acquisition timeout invokes no callback; callers must
report failure, not successful deletion. Existing later I/O/callback failures
are not a multi-file transaction. Lock files are derived with `with_extension`
from the aggregate, one-shot and heartbeat-ledger paths in the state directory.
Version observation propagates an unavailable ledger rather than persisting a
false first observation. CLI purge takes aggregate/one-shot before ledger purge.

All `std::sync::Mutex` unless noted otherwise.

| # | Lock | File | Type | Purpose |
|---|------|------|------|---------|
| L1 | `REGISTRY` | `core/index_orchestrator.rs:87` | `OnceLock<Mutex<HashMap<String, Arc<Mutex<ProjectBuild>>>>>` | Outer map of per-project build state |
| L2 | per-project `ProjectBuild` | `core/index_orchestrator.rs:87` (inner) | `Arc<Mutex<ProjectBuild>>` | Individual project build progress |
| L3 | `HEATMAP_BUFFER` | `core/heatmap.rs:10` | `Mutex<Option<HeatMap>>` | Buffered access-frequency heatmap |
| L4 | `Config::CACHE` | `core/config/loader.rs:424` | `Mutex<Option<(Config, SystemTime, Option<SystemTime>)>>` | Config file cache with mtime check |
| L5 | `FEEDBACK_BUFFER` | `core/feedback.rs:9` | `Mutex<Option<(FeedbackStore, Instant)>>` | Buffered user feedback |
| L6 | `PREDICTOR_BUFFER` | `core/mode_predictor.rs:8` | `Mutex<Option<(Arc<ModePredictor>, Instant)>>` | Cached mode predictor model |
| L7 | `STATS_BUFFER` | `core/stats/mod.rs:13` | `Mutex<Option<(StatsStore, StatsStore, Instant)>>` | Token-savings statistics |
| L8 | `COST_BUFFER` | `core/a2a/cost_attribution.rs:74` | `Mutex<Option<CostStore>>` | A2A cost tracking |
| L9 | `GLOBAL_LIMITER` | `core/a2a/rate_limiter.rs:121` | `Mutex<Option<RateLimiter>>` | Global A2A rate limiter |
| L10 | `STATE` (egress rate window) | `core/egress.rs:163` | `OnceLock<Mutex<VecDeque<Instant>>>` | Sliding 60 s window of outbound-egress timestamps for `check_rate`; held only to prune and push one `Instant`, no I/O and no other lock under it. Poison denies the request (fail-closed). |
| L14 | `ACTIVE_ROLE_NAME` | `core/roles.rs:12` | `OnceLock<Mutex<String>>` | Currently active role name |
| L15 | `PROVIDER_CACHE` | `core/providers/cache.rs:34` | `LazyLock<Mutex<ProviderCache>>` | Cached provider metadata |
| L17 | `FILE_LOCKS` | `core/path_locks.rs:30` | `OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>>` | Per-file read serialization for concurrent subagents |
| L19 | `CACHE` (graph) | `core/graph_cache.rs:38` | `OnceLock<Mutex<HashMap<String, Entry>>>` | Property graph query result cache |
| L20 | `RECENT` | `core/auto_findings.rs:15` | `Mutex<Vec<RecentEntry>>` | Recent auto-finding entries |
| L21 | `LOCK` (home) | `core/home.rs:259` | `Mutex<()>` | Serialize home directory creation |
| L22 | `BACKENDS` | `lsp/router.rs:57` | `LazyLock<Mutex<HashMap<String, Box<dyn LspBackend>>>>` | Per-language code-intelligence backend registry (rust-analyzer / JetBrains); replaces the former `CLIENTS` map. Held across `Config::load` (L4) — see §3 |
| L23 | `BUDGETS` | `core/agent_budget.rs:6` | `Mutex<Option<HashMap<String, AgentBudget>>>` | Per-agent token budget tracking |
| L24 | `SHELL_ENV_LOCK` | `shell_hook.rs:1211` | `Mutex<()>` | Serialize env-var access in shell hook |
| L25 | `TRACKER` | `core/search_delta.rs:49` | `Mutex<Option<SearchDeltaTracker>>` | Tracks search result changes between calls |
| L26 | `SESSION_ID` | `server/bypass_hint.rs:9` | `Mutex<Option<String>>` | Current bypass hint session ID |
| L27 | `CACHE` (git) | `core/git_cache.rs:10` | `LazyLock<Mutex<GitCache>>` | Cached git metadata (branch, status) |
| L28 | `STORE` (refs) | `server/reference_store.rs:22` | `OnceLock<Mutex<HashMap<String, RefEntry>>>` | Function reference store for Fn-ref system |
| L29 | `DB` | `core/archive_fts.rs:29` | `LazyLock<Mutex<Option<Connection>>>` | SQLite FTS archive connection |
| L31 | `GLOBAL` (dyn-tools) | `server/dynamic_tools.rs:422` | `OnceLock<Mutex<DynamicToolState>>` | Dynamic tool registration state |
| L32 | `APPLIED_PACKAGES` | `core/context_package/auto_load.rs:6` | `Mutex<Option<HashSet<String>>>` | Track which context packages have been applied |
| L33 | `CACHE` (search) | `core/search_index.rs:655` | `OnceLock<Mutex<HashMap<String, CacheEntry>>>` | BM25 search index query cache |
| L34 | `GLOBAL` (capabilities) | `core/client_capabilities.rs:384` | `OnceLock<Mutex<ClientMcpCapabilities>>` | Client MCP capability flags |
| L36 | `BUILD` | `core/call_graph.rs:305` | `OnceLock<Mutex<BuildState>>` | Call graph build state |
| L37 | `LAST_REAL` | `proxy/introspect.rs:54` | `Mutex<[Option<String>; 3]>` | Last 3 real (non-proxy) request paths |
| L38 | `GLOBAL_TRACKER` | `core/bounce_tracker.rs:568` | `OnceLock<Mutex<BounceTracker>>` | Tracks repeated tool-call bounces |
| L40 | `GLOBAL_MANAGER` | `core/multi_repo.rs:387` | `OnceLock<Mutex<MultiRepoManager>>` | Multi-repo workspace manager |
| L41 | `KNOWLEDGE_LOCKS` | `core/knowledge/persist.rs:29` | `OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>>` | Per-project knowledge.json read-modify-write serialization |
| L42 | `LAST_PARSE_ERROR` | `core/config/loader.rs:96` | `Mutex<Option<String>>` | Most recent global `config.toml` parse error (surfaced by doctor/diagnostics) |
| L43 | `POLICY_CACHE` | `server/permission_inheritance.rs:53` | `OnceLock<Mutex<Option<CacheEntry>>>` | Cached host-IDE permission policy (TTL-bounded) for permission inheritance |
| L44 | `POOL` | `core/providers/mod.rs:36` | `OnceLock<Mutex<HashSet<&'static str>>>` | Provider string-interning pool; bounds per-construction leaks to the finite set of distinct provider ids/names/actions |
| L47 | `SOURCE_COUNTS` | `core/auto_mode_resolver.rs:12` | `Mutex<Option<HashMap<&'static str, u64>>>` | Per-process counters of which signal decided each auto-mode resolution; surfaced by `ctx_metrics` (#496) |
| L48 | `NO_GIT_ROOTS` | `core/git_signals.rs:23` | `Mutex<Option<HashSet<String>>>` | Roots probed and found non-git — negative cache so each root is probed at most once per process |
| L49 | `BASELINE` | `core/datadog_push.rs:46` | `Mutex<Option<Baseline>>` | Last pushed counter totals for the Datadog agentless push — deltas are computed against it each interval (#401) |
| L50 | `LINE_EMBED_CACHE` | `core/entropy.rs:343` | `Mutex<Option<HashMap<u64, Vec<f32>>>>` | Per-line embedding cache (line-hash → vector) for the semantic redundancy filter (#544); capacity-bounded, never blocks on model loads |
| L51 | `SELECTED_ARMS` | `core/adaptive_thresholds.rs:347` | `Mutex<Option<SelectedArmRegistry>>` | Registry of recently selected bandit arms (per project root) so real bounce/edit-fail signals are attributed to the arm that produced the compression (#593); capacity-bounded (64 paths, oldest-first eviction) |
| L52 | `ACTIVE_PROFILE_OVERRIDE` | `core/profiles/loading.rs:319` | `RwLock<Option<String>>` | In-process active-profile override set by `set_active_profile`; replaces the former `std::env::set_var("LEAN_CTX_PROFILE")` so profile switching is data-race-free under the multi-threaded MCP runtime (Edition 2024). Read on every `active_profile_name()` (override → env → config → "coder") |
| L53 | `REGISTRY` (introspect) | `core/introspect.rs:102` | `LazyLock<Mutex<Registry>>` | Cognition v2 activity registry — per-subsystem tick counters + last-run; flushed debounced to a project-scoped JSON so `introspect cognition` / `doctor` can report wired/active across processes (#cognition-v2) |
| L54 | `ACTIVE_WEIGHTS` | `core/context_field.rs:266` | `RwLock<Option<FieldWeights>>` | In-process learned Φ field-weights cache set by `set_active_weights` (bandit-chosen arm), read by `active_weights()` / `compute_phi`; deterministic by default, sampling only under `LEAN_CTX_STOCHASTIC` (#cognition-v2) |
| L56 | `MEMO` | `proxy/prose_ranker.rs:30` | `Mutex<Option<HashMap<u64, String>>>` | Cache-safe wire-prose squeeze memo (#895): the first squeeze of a `(content, budget)` is frozen for the process lifetime so a later warm recompute returns identical bytes (provider prompt-cache stability, #448/#498); capacity-bounded (8192), independent leaf lock, never nested |
| L57 | `SEEN` | `core/conversation.rs:85` | `OnceLock<RwLock<Vec<(String, Instant)>>>` | Recent sightings of distinct conversation ids (`id` → last-seen instant) feeding the multi-conversation stub-gate detector (#1040/#1042); capacity-pruned, independent leaf lock, never nested |
| L58 | `SNAPSHOT` | `proxy/policy_gate.rs:91` | `RwLock<Option<CachedSnapshot>>` | TTL-cached org-policy gate rules (enterprise#25) so the forward path re-verifies the signed policy at most once per minute; independent leaf lock, never nested |
| L59 | `LEDGER` | `proxy/policy_gate.rs:286` | `OnceLock<Mutex<BudgetLedger>>` | In-process person/day + project/month spend counters backing hard budget caps (enterprise#25); fed from the metering choke-point, seeded from Postgres when available; independent leaf lock, never nested |
| L60 | `RATE` | `proxy/policy_gate.rs:320` | `OnceLock<Mutex<RateLedger>>` | Per-person accepted-request counts for the current UTC minute backing the org-policy rate limit (enterprise#66); reset on every minute roll, at most one entry per active person; independent leaf lock, never nested |
| L61 | `CLI_OVERLAY` | `core/index_filter.rs:31` | `RwLock<Option<CliOverlay>>` | Per-run index corpus filter overlay (#735): written once by the `index` CLI dispatch before builders start, read by every index walk via `IndexFileFilter::resolve`; independent leaf lock, never nested |
| L62 | `TARGETS` | `core/eviction_orchestrator.rs:48` | `LazyLock<Mutex<EvictionRegistry>>` | Process-wide registry of live `EvictionOrchestrator` weak refs (RAM guardian, multi-session): `register()`/`on_memory_pressure()` lock it only to clone/upgrade the target list, then drop it before calling `on_pressure()` on each target — never held while a per-target `controller` lock (or any other lock) is acquired; independent leaf lock, never nested |
| L63 | `BLOCK` | `core/events.rs:287` | `OnceLock<Mutex<LocalBlock>>` (fn-local static) | Per-process cache of the current pre-reserved event-id block (`next`/`last`) inside `next_event_id()`: most calls just bump `next` under the lock; on exhaustion the lock is held across the refill call to `reserve_event_id_block_at`, which takes its own independent OS file lock (`events.seq.lock`, not a Rust static) to persist the next block — never nested with another *static* Rust lock |
| L64 | `SESSION_READ_ONLY_ROOTS` | `core/pathjail.rs:153` | `OnceLock<Mutex<Vec<PathBuf>>>` | Session-scoped read-only roots auto-detected from language caches (#899); consulted by both read allow-list (jail) and `is_read_only_path` (write-deny) so a cache root is readable but never writable; independent leaf lock, never nested |
| L65 | `SESSIONS` | `proxy/sticky_tools.rs:30` | `OnceLock<Mutex<HashSet<u64>>>` (fn-local static) | CCR active conversation tracking; capacity-bounded (`MAX_TRACKED`), oldest-first eviction on overflow; independent leaf lock, never nested |
| L66 | `JOBS` | `server/background_shell.rs:52` | `LazyLock<Mutex<HashMap<String, Job>>>` | Content-addressed background shell jobs; lock is held only to insert, inspect, or remove a job, then released before executing or joining its worker thread; independent leaf lock, never nested |
| L67 | `BUILD_GATE` | `core/graph_provider.rs:891` | `LazyLock<Mutex<()>>` | Serializes synchronous graph-index builds; acquired with `try_lock()` and held only while the spawned builder is awaited, never nested with another static lock |
| L68 | `BUDGET_STORE` | `core/ocla/wire_api.rs:72` | `OnceLock<Mutex<BudgetStore>>` | OCLA budget enforcement store; tracks per-scope daily token/USD budgets; independent leaf lock, never nested |
| L69 | `DASHBOARD_PROJECT` | `core/runtime_flags.rs:10` | `OnceLock<Mutex<Option<String>>>` | Caches the active project name for dashboard display; independent leaf lock, never nested |
| L70 | `ALLOW_PATHS` | `core/runtime_flags.rs:12` | `OnceLock<Mutex<Vec<PathBuf>>>` | Runtime-configured additional allowed paths for PathJail; independent leaf lock, never nested |
| L72 | `IDENTITY_LEDGER` | `core/context_kernel/proxy_bridge.rs:13` | `OnceLock<Mutex<IdentityLedger>>` | Per-user identity tracking in the proxy-to-kernel bridge; updated after proxy ETPAO tracking, independent leaf lock |
| L73 | `ETPAO_TRACKER` | `core/context_kernel/proxy_bridge.rs:14` | `OnceLock<Mutex<EtpaoLive>>` | ETPAO efficiency tracking for proxy requests; locked and released before `IDENTITY_LEDGER` |
| L74 | `CHAIN` | `core/context_kernel/receipt_chain.rs:37` | `OnceLock<Mutex<ReceiptChain>>` | Evidence chain for context delivery lifecycles and monotonic ids; independent leaf lock, never nested |
| L75 | `RECEIPTS` | `core/context_kernel/mcp_receipt.rs:46` | `OnceLock<Mutex<McpReceiptStore>>` | Receipt recording and honest accounting for MCP tool calls; independent leaf lock, never nested |
| L76 | `DEDUP` | `core/context_kernel/dedup_wiring.rs:10` | `OnceLock<Mutex<ContextDedup>>` | Global content-deduplication cache for context delivery hot paths; acquired before `SEEN_PATHS`/`STATS` in reset paths |
| L77 | `SEEN_PATHS` | `core/context_kernel/dedup_wiring.rs:11` | `OnceLock<Mutex<HashSet<String>>>` | Tracks previously seen file paths for dedup modified-vs-fresh distinction; may be touched after `DEDUP` |
| L78 | `STATS` | `core/context_kernel/dedup_wiring.rs:12` | `OnceLock<Mutex<DedupStats>>` | Cumulative content-deduplication counters; may be reset after `DEDUP` and `SEEN_PATHS` |
| L79 | `MCP_ETPAO` | `core/context_kernel/mcp_bridge.rs:11` | `OnceLock<Mutex<EtpaoLive>>` | ETPAO efficiency tracking for MCP tool calls; locked and released before `MCP_IDENTITY` |
| L80 | `MCP_IDENTITY` | `core/context_kernel/mcp_bridge.rs:12` | `OnceLock<Mutex<IdentityLedger>>` | Per-user identity tracking for MCP clients; independent leaf lock |
| L81 | `SAVINGS` | `core/context_kernel/schema_wiring.rs:47` | `OnceLock<Mutex<SavingsState>>` | Cumulative schema optimization savings tracking; independent leaf lock, never nested |
| L82 | `NORMALIZER` | `core/context_kernel/usage_normalizer.rs:73` | `OnceLock<Mutex<SessionUsage>>` | Session-level token usage aggregation from canonical envelopes; independent leaf lock, never nested |
| L83 | `FEATURES` | `core/context_kernel/kernel_config.rs:47` | `OnceLock<Mutex<KernelFeatures>>` | Central runtime feature toggles for all kernel modules; independent leaf lock, never nested |
| L84 | `QUERY_TOKENS` | `tools/search_kernel.rs:10` | `LazyLock<Mutex<HashMap<u64, usize>>>` | Per-session repeated-query tracking for the search tool (r29): maps a query hash to its result-token count so repeat searches are counted; locked only to record or read a single entry, never held across another lock or an `.await`; independent leaf lock, never nested |
| L85 | `PROVIDER_STATS` | `core/context_kernel/envelope_bridge.rs:37` | `OnceLock<Mutex<HashMap<ProviderKind, ProviderAccum>>>` | Per-provider token/cost accumulation from canonical envelopes (r32); locked briefly to record or read a single entry; independent leaf lock, never nested |
| L87 | `PROVIDER_CACHE` | `core/providers/cache.rs:34` | `LazyLock<Mutex<ProviderCache>>` | LRU cache for provider responses (E8); locked briefly to read, insert, or evict entries; independent leaf lock, never nested |
| L89 | `INJECTED` | `core/rule_discovery.rs:24` | `Mutex<Option<HashSet<String>>>` | Per-session rule injection deduplication for ctx_read (#1325); locked briefly to check/record a source/content key; independent leaf lock, never nested |
| L90 | `DIR_CACHE` | `core/rule_discovery.rs:150` | `Mutex<Option<HashMap<String, Vec<DiscoveredRule>>>>` | Directory-scoped rule discovery cache for ctx_read (#1325); locked briefly to read or store discovered rules, then released before path filtering; independent leaf lock, never nested |
| L91 | `SIG_QUERY_CACHE` | `core/signatures_ts/query_cache.rs:9` | `OnceLock<Mutex<HashMap<Language, Arc<Query>>>>` | Compiled tree-sitter signature queries per language; locked briefly to get or insert a cached query; independent leaf lock, never nested |
| L92 | `BLOCKED` | `core/signatures.rs:295` | `Mutex<Option<HashSet<String>>>` | Per-session tree-sitter language blocklist; tracks extensions whose grammars panicked so subsequent calls skip straight to regex fallback; locked briefly to check or insert; independent leaf lock, never nested |
| L99 | `TURN_BUDGETS` | `core/agent_budget.rs:7` | `Mutex<Option<HashMap<String, TurnBudget>>>` | Per-agent per-turn budget tracking; independent leaf lock, never nested |
| L100 | `ADOPTION_PASSES` | `core/legacy_adoption.rs:170` | `OnceLock<Mutex<HashSet<String>>>` | Records the projects whose automatic legacy-context adoption pass already ran in this process; locked only to check-and-insert, released before any adoption work; independent leaf lock, never nested |
| L101 | `PRELOADER` | `server/predictive_preload.rs:127` | `OnceLock<Mutex<PredictivePreloader>>` | Predictive file preloader state; independent leaf lock, never nested |
| L102 | `SUMMARY_CACHE` | `core/savings_ledger/store.rs:283` | `Mutex<Option<(u64, LedgerSummary)>>` | Ledger summary cache keyed by file length; independent leaf lock, never nested |
| L104 | `SESSION_LINEAGES` | `core/task_spine.rs:27` | `OnceLock<Arc<Mutex<HashMap<String, String>>>>` | Maps session IDs to parent lineage; independent leaf lock, never nested |
| L108 | `PROXY_VALUE_METRICS` | `proxy/value_gate_proxy.rs:23` | `Mutex<ProxyValueMetrics>` | Proxy-level value gate metrics counters; independent leaf lock, never nested |
| L109 | `PROXY_STARTED` | `proxy/web_app/dashboard.rs:9` | `LazyLock<Mutex<Option<Instant>>>` | Proxy start timestamp for uptime display; independent leaf lock, never nested |
| L110 | `ROI_CACHE` | `dashboard/routes/roi.rs:14` | `Mutex<Option<(Instant, String)>>` | Cached ROI dashboard response; independent leaf lock, never nested |
| L111 | `DOCTOR_CACHE` | `dashboard/routes/doctor.rs:17` | `Mutex<Option<(Instant, String)>>` | Cached doctor check response; independent leaf lock, never nested |
| L114 | `PENDING` | `core/security_events.rs:346` | `Mutex<SecurityCounts>` | Security-event counters pending for the session snapshot; leaf lock, taken after the audit-trail write returns, never nested |
| L115 | `CURRENT_SESSION` | `core/value/mod.rs:28` | `RwLock<Option<String>>` | Session id committed into ledger and security-event hashes; leaf lock, never nested |
| L116 | `FOLDED` | `core/telemetry_aggregate.rs:917` | `OnceLock<Mutex<HashMap<PathBuf, CounterCheckpoint>>>` | Per-process tool counters already folded into each telemetry one-shot sidecar; taken under the one-shot OS file lock but never nested with another Rust static lock, guard dropped before any I/O |
| L117 | `NEXT` | `core/graph_enricher.rs:465` | `LazyLock<Mutex<HashMap<String, Instant>>>` | Earliest next backend-triggered semantic refresh per project root (`schedule_semantic_refresh`). Leaf lock: taken briefly under a router backend *slot* lock (never under L22 `BACKENDS`) and in the refresh worker; never held across I/O, thread spawn or another lock |
| L118 | `CACHE` | `core/semantic/coverage.rs:102` | `LazyLock<Mutex<HashMap<(String, String), (Option<String>, Instant)>>>` | Per (language, project root) language-server availability for status surfaces (60 s TTL). Leaf lock: released before server resolution (config load, `PATH` probe) and re-taken only to store the answer |
| L119 | `STORE_LOCK` | `core/cli_cache.rs:107` | `Mutex<()>` | In-process half of the CLI cache store guard: taken before the store's OS file lock and released after it (field drop order); independent leaf lock, never nested with another static lock. Poison is recovered. |
| L120 | `ARCHIVE_PASS` | `core/legacy_adoption.rs:131` | `OnceLock<Mutex<ArchivePass>>` | Bounded automatic archive adoption: candidate list, session→project cache and per-project progress. Locked only for short reads and inserts; never held across a metadata scan, a session read or an archive write, never nested with another static lock. |
| L121 | `LEGACY_PROVIDER_PRUNED` | `tools/ctx_provider.rs:668` | `OnceLock<Mutex<HashSet<String>>>` | Records, per project and provider resource, that the untagged pre-AP09 chunk prefix was already pruned in this process; locked only for one check-and-insert, independent leaf lock, never nested. |
| L122 | `KNOWLEDGE_DRIFT` | `core/legacy_adoption.rs:374` | `OnceLock<Mutex<HashMap<String, Option<String>>>>` | Caches, per data directory and project root, which earlier knowledge partition (if any) recorded that root; locked only for one lookup or insert, never held across the bounded directory scan, never nested. |
| L123 | `DISCOVERED` | `lsp/editor_bridge.rs:205` | `LazyLock<Mutex<HashMap<PathBuf, (Option<BridgeFile>, Instant)>>>` | Editor semantic-bridge discovery per canonical project root (10 s TTL). Leaf lock: released before the directory scan and `/health` ping, re-taken only to store or drop an answer |
| L124 | `OBSERVED` | `core/context_store/task_signals.rs:157` | `OnceLock<Mutex<HashSet<Origin>>>` | Deduplicates task observation records per process; guard is released before spawning the recorder thread or performing I/O, independent leaf lock |
| L125 | `KNOWN` | `core/intelligence_runtime/semantic_detectors.rs:204` | `OnceLock<Mutex<HashMap<[u8; 32], (CategorySet, Option<Instant>)>>>` | Which semantic categories the authenticated Runtime package of given settings declares (`offer`). Leaf lock: taken for one lookup and one insert, released before the package is read or a worker is asked; never nested with another static lock |
| L126 | `ISSUED` | `core/context_admission/receipt_store/references.rs:23` | `OnceLock<Mutex<VecDeque<String>>>` | Recovery ids this process issued, bounded to 1024 (`note_recovery`, `recovery_in`). Leaf lock: pure in-memory queue, no I/O and no other lock while held |
| L127 | `LOADED` | `core/context_kernel/providers.rs:542` | `Mutex<Option<(Key, Arc<BM25Index>)>>` | One loaded BM25 index for the kernel's search provider, keyed by root, index fingerprint and store-admission policy digest. The key (including `StoreAdmission::current()`, which takes its own observation mutex) is computed before this lock; the guard is held while a missing index is loaded from disk and no other lock is taken under it. Poison is recovered. |
| L128 | `RUNTIME_INIT` | `core/context_os/mod.rs:89` | `Mutex<()>` | Serializes first construction of the process-wide Context OS runtime so concurrent first callers do not open and migrate the same SQLite database at once (Windows sharing/lock errors). Held only across `ContextOsRuntime::try_new` (directory create + SQLite open/migrate); `RUNTIME` (a `OnceLock`) is re-checked under it. Independent leaf lock, never nested with another static lock. Poison is recovered. |
| L129 | `PERSIST` | `core/bm25_index/mod.rs:291` | `Mutex<()>` | One load–change–save of a persisted BM25 index at a time in this process (provider ingests, eviction after a search), so two writers never race through the one temp file. Taken via `persist_lock()` by `consolidation` (provider ingest) and `BM25Index::evict_sources`, held across load/build–change–save; config reads during load/build take `Config::CACHE` (L4) under it. Poison is recovered. |
| L130 | `SEEN_INJECTIONS` | `core/security_events.rs:435` | `Mutex<Option<LruCache<String, ()>>>` | SHA-256 digests of prompt-injection-flagged objects already recorded by this process (LRU, capacity 4,096), so a re-delivered object is recorded once. Held only for one lookup-and-insert, released before the audit-trail write and `PENDING` (L114); independent leaf lock. Poison counts as already seen (undercount). |
| L131 | `HANDSHAKE` | `core/client_capabilities.rs:346` | `OnceLock<Mutex<Option<ClientHandshake>>>` | The MCP client's name and version from `initialize`. Held only to copy or replace the value; independent leaf lock, never nested. Poison is recovered. |
| L132 | `VERIFIED` (presence checks) | `tools/server_lifecycle.rs:24` | `OnceLock<Mutex<HashMap<String, Instant>>>` | Last registry verification per presence id, throttling re-checks to once a minute. The guard is dropped before the registry is read or written; independent leaf lock. Poison is recovered. |
| L133 | `STORE` (last model) | `proxy/usage_meter.rs` | `OnceLock<Mutex<Option<LastModel>>>` | The proxy's last foreground model and its time. Written in `record`, read in `persist` and `resume_from_disk`; the guard is held only to copy or replace the value and is never nested with the usage stores. Independent leaf lock. Poison is recovered. |
| L134 | `.maintenance.lock` (receipt store) | `core/context_admission/receipt_store/retention.rs:62` | `fs2` exclusive file lock on `<data>/gateway/.maintenance.lock` | One maintenance pass of the gateway evidence store at a time, across processes. Try-only, never waited for: a holder elsewhere means the pass is skipped. Taken before the capacity ledger lock (`.usage.lock`, taken and released per booking batch) and never while that lock is held. |
| L135 | `CANDIDATES` | `core/providers/credentials.rs:83` | `RwLock<Option<BTreeMap<String, String>>>` | Candidate provider settings a `check --stdin` installs for the duration of the check (`set_candidates`, `clear_candidates`); read by `var`/`is_set`. Held only to copy a value or replace the map, no I/O while held; independent leaf lock, never nested. Poison is recovered. |
| L136 | `INDEXES` | `core/dashboard_pro/ledger.rs:646` | `OnceLock<Mutex<HashMap<PathBuf, LedgerIndex>>>` | The cockpit Ledger page's per-file savings index. Held across one `refresh` (an append-only read of that ledger file) and the caller's closure on it (`browse`, `report`, `proxy_days`, `recorded_price`, and `export_entries`, whose callback builds the export in memory); takes no other static lock and performs no network or write I/O while held. Poison is recovered. |
| L137 | `RING` | `tools/ctx_edit/applied_edits.rs:54` | `OnceLock<Mutex<Ring>>` | The last 64 edits `ctx_edit` applied in this process (path, old and new string digests), so a repeat of one answers "No change". Held only to push or look up one entry; no I/O while held (paths are canonicalized before); independent leaf lock, never nested. A poisoned lock only skips the record or the check. |
| L138 | `STATES` | `core/providers/emspace.rs:519` | `OnceLock<Mutex<HashMap<String, RootState>>>` | Per project root: whether a linked emspace space is readable (rechecked every 10 min) and when it was last indexed or its sessions read. Held for one entry lookup and the closure on it; the 10-minute recheck stats `.emspace` in the root and each ancestor and looks for the CLI on PATH and in ~/.local/bin while held (no process spawn, no network); the emspace calls and the separate fs2 index lock run outside it. Takes no other static lock; independent leaf lock. Poison is recovered. |

### CLI cache transaction file lock

`core::cli_cache::CacheTransaction` acquires the existing bounded `agents::FileLock`
on `<data_dir>/cli-cache/cache.lock` before loading `cache.json` and retains it
through mutation and atomic replacement. Read, invalidate, clear, project reset,
and statistics use this same authority. Acquisition waits at most 750 ms. Cache
failure returns full content for reads and an error for explicit cache commands
or post-refactor eviction; it never permits an unlocked write. No callbacks,
network, awaits, or nested cache transactions occur while held. Refactor callers
may already hold L22; the cache transaction never acquires L22. The data-directory
resolver and content read finish before this file lock is acquired.

### Config file lock

`config_io::ConfigWriteGuard` is an RAII hold on the existing bounded
`core::migration_lock` OS file lock, `<data dir>/locks/<blake3(resolved path)>.<file>.v4-migration.lock`
(beside the target only when no data dir resolves, so project files never get a
lock sibling).
No new lock is introduced; despite the migration-era filename, ordinary
setters take this same lock. Acquisition waits at most **5 s** and then fails
rather than writing unlocked. The guard owns both the `MigrationLock` and the
canonical resolved path (`resolve_write_target` +
`canonicalize_existing_prefix`, the same derivation the migration writer
uses), and both fields are private — a guarded write cannot be aimed at a
file the guard is not excluding, and a symlinked config collapses to one
identity so aliases cannot race.
Guarded entry points read, merge, back up and write this same resolved path;
backups live beside the canonical target, not beside an alias. Existing
alias-side backups are left untouched.

**Scope — stated narrowly.** This lock serializes *holders of this lock file*.
It is **not** a universal config-write authority, and should not be described
as one:

- `core::tool_profiles` and `core::update_scheduler` mutate the global config
  via `load_toml_document` → `write_toml_document`. They read **unlocked** and
  take the lock only for the write, so they remain vulnerable to the same
  read-modify-write race. Out of LR-TEL-01 scope; fixing them means moving
  them onto a guard.
- The same `write_atomic_with_backup` writer backs many **non-config** files
  (editor configs, ledgers, skills, hook files), each getting its own lock file
  under `<data dir>/locks/`.

What LR-TEL-01 changed is the **span** for the guarded paths:
`Config::try_update_global_at` takes the guard, loads the global file fresh
from disk inside the critical section, applies the caller's closure, and
writes — closing the window in which a concurrent writer could commit between
the load and the save and be overwritten. Note the load was *always* fresh
(`try_load_global_from`, no `Config::CACHE`, no project merge); the fix is the
single critical section, not the freshness. `setter::set_many_by_key` uses the
same span for its read/merge/write. The merge read inside
`write_toml_preserving_minimal` is now inside the lock too, because that merge
decides the output from the current on-disk document.

**Not reentrant.** It is an exclusive `flock` taken on a freshly opened
descriptor, so a second acquisition on the same call path does not recurse —
it spins to the 5 s deadline and fails. A guard holder must write through the
guard (`ConfigWriteGuard::write_toml_preserving_minimal`,
`Config::save_to_locked`, which takes `&ConfigWriteGuard` rather than a path);
the lock-free writer cores are private to `config_io`. The public, self-locking
writers (`Config::save`, `save_to`, `write_toml_preserving_minimal`,
`write_atomic_with_backup*`) must not be called from inside a critical
section.

While held: no network, no user callback doing I/O, and no nested config
write. Update closures must be pure in-memory mutation: network work happens
outside the lock and only a small delta is applied inside it. No static lock is acquired under this file lock: the in-section
load uses `Config::try_load_global_from` (a direct file read), never
`Config::load`, so **L4 `Config::CACHE`** is not nested here and no edge is
added to the sanctioned L22 → L4 pair. A future closure calling
`Config::load()` would introduce a *config file lock → L4* edge and needs an
ordering rule added here first.

The v3→v4 migration writer (`write_atomic_config_migration_checked`) takes the
same lock as a leaf from the load path and holds it through journal and atomic
file persistence; it never calls a self-locking config writer while holding it.

Keep update closures limited to in-memory mutation: nested config writes time
out on the same lock; additional locks require an explicit ordering rule, and
network I/O would make lock hold time depend on an external service.

### Test / Environment Locks (serialise env-var mutations)

| # | Lock | File | Purpose |
|---|------|------|---------|
| S1 | `RECENT_EDITS` | `hook_handlers/solution_capture.rs:35` | `Mutex<RecentEdits>` deduplication cache for recent native edit observations |
| S2 | `TRACKER_LOCK` | `core/config/solution_tests.rs:4` | `Mutex<()>` serialize solution-config integration tests |
| E5 | `LOCK` | `core/tokens.rs:222` | Serialize tokenizer tests |
| E7 | `TEST_ENV_MUTEX` | `core/data_dir.rs:189` | `OnceLock<Mutex<()>>` reentrant test env lock — serialize env-mutating tests across threads (`std::env::set_var` is not thread-safe); same thread may re-acquire without deadlocking |
| E8 | `COMPRESSOR_INPUT` | `proxy/forward/tests.rs:1609` | `Mutex<Option<String>>` test-only (file is the `#[cfg(test)]` module of `proxy/forward`): what the compression stage was handed in the reintroducing-compressor tests; taken alone, never nested |
| S4 | `FENCE_KEY` | `core/sanitize.rs:139` | `Mutex<Option<(PathBuf, [u8; 32])>>` product lock: the fence key last read with the file it came from; held across the read or first creation of the `fence_key` file so that one process settles on one key per file; takes no other lock; a poisoned lock falls back to the process key, never to a fence without a key |
| S5 | `REGISTER` | `proxy/sealed_origin.rs:257` | `Mutex<Register>` product lock: bounded register of sealed items and their producers (age and count bound enforced under the lock); product paths take it alone; only the `#[cfg(test)]` bound reset takes `PARTS` (S6) while holding it — order S5 → S6, never the reverse |
| S6 | `PARTS` | `proxy/sealed_origin.rs:328` | `LazyLock<Mutex<Parts>>` product lock: bounded table of assistant parts and the lineage each is bound to; product paths take it alone, never while holding another lock; inner lock of S5 → S6 in the test-only reset |
| S7 | `TABLE` | `proxy/sealed_origin_engine_stand_in.rs:62` | `Mutex<Table>` test-only (the file is compiled only into the test build of `proxy/sealed_origin` via `#[cfg(test)] #[path]`): stand-in entries a test fills in place of the Engine's delivered-content register; each access takes and releases it alone, never nested |
| S8 | `LINK` | `shell/containment/spawner.rs:29` | `Mutex<Option<Arc<Link>>>` product lock: the connection to this process's spawn helper; held while a missing or gone helper is started (bounded by the helper's answer budget), so that concurrent calls start one helper, and for the `take` at shutdown; requests are sent on the cloned `Arc<Link>` after release; takes no other static lock |

---

## 2. Arc-wrapped Session Locks (per-MCP-session, `tokio::sync::RwLock`)

Defined in `tools/mod.rs` on `ToolContext`:

| Field | Type | Purpose |
|-------|------|---------|
| `cache` | `Arc<RwLock<SessionCache>>` | File content cache |
| `session` | `Arc<RwLock<SessionState>>` | Session metadata |
| `tool_calls` | `Arc<RwLock<Vec<ToolCallRecord>>>` | Call log |
| `last_call` | `Arc<RwLock<Instant>>` | Idle-timeout tracking |
| `agent_id` | `Arc<RwLock<Option<String>>>` | Current agent identifier |
| `client_name` | `Arc<RwLock<String>>` | Connected client name |
| `loop_detector` | `Arc<RwLock<LoopDetector>>` | Loop-detection state |
| `workflow` | `Arc<RwLock<Option<WorkflowRun>>>` | Active workflow run |
| `ledger` | `Arc<RwLock<ContextLedger>>` | Context ledger |
| `pipeline_stats` | `Arc<RwLock<PipelineStats>>` | Pipeline statistics |
| `context_ir` | `Option<Arc<RwLock<ContextIrV1>>>` | Context IR state |

These are all **`tokio::sync::RwLock`** and are scoped to a single session — no cross-session
nesting is expected. Within a single tool handler, acquire at most one at a time.

### Other Arc-wrapped Locks

| Lock | File | Type | Purpose |
|------|------|------|---------|
| `SharedProtocol` | `mcp_stdio.rs:30` | `Arc<Mutex<Option<WireProtocol>>>` | MCP stdio wire protocol (std::sync) |
| `SessionEntry.session` | `core/context_os/shared_sessions.rs:31` | `Arc<tokio::sync::RwLock<SessionState>>` | Shared session state across channels |
| `RENEWED` | `core/agents/registry.rs:43` | `Mutex<BTreeSet<String>>` | Agents whose expired registration a heartbeat renewed, until reported (leaf, std::sync) |
| `VERDICTS` | `core/knowledge/protection.rs:61` | `LazyLock<Mutex<HashMap<[u8; 32], bool>>>` | Admission verdicts of facts by content key (leaf, std::sync) |
| `PARSED` | `core/knowledge/persist.rs:91` | `OnceLock<Mutex<HashMap<PathBuf, (Key, ProjectKnowledge)>>>` | Parsed knowledge stores by file identity (leaf, std::sync) |
| `SCOPES` | `core/savings_ledger/project_scope.rs:23` | `Mutex<Vec<(PathBuf, Instant, ProjectScope)>>` | Recent project scopes of the savings ledger (leaf, std::sync) |
| `KEYS` | `core/savings_ledger/project_scope.rs:164` | `Mutex<Vec<(PathBuf, Option<String>)>>` | Project keys of folders (leaf, std::sync) |
| `SCOPED_SUMMARY_CACHE` | `core/savings_ledger/store.rs:305` | `Mutex<Vec<(PathBuf, PathBuf, u64, LedgerSummary)>>` | Per-project ledger summaries, invalidated like `SUMMARY_CACHE` (leaf, std::sync) |

---

## 3. Lock Acquisition Order

Native host outcome observation reuses the existing task file lock through
receipt-chain append and optional personal learning. Order: host task lock →
individual ledger/artifact operations (released on return) → personal-store
opening lock → SQLite transaction. No ledger lock survives into learning;
the personal store never takes the host task lock. No new lock is introduced.

Host checkpoint export follows the same task lock → individual ledger/artifact
operation order, retaining the task lock from receipt-head selection through
immutable checkpoint publication. It introduces no additional lock or live store.

Host-scoped checkpoint resume shares that receipt/head authority, then acquires the
existing session save-file and bounded project-index locks while retaining the task
lock. The session writer does not acquire a task lock in reverse order. Resume adds
no new lock and publishes a fresh compatibility session, preserving the signed source.

Legacy session import reuses the existing live-session → per-session save file
lock → bounded project-index lock order. It publishes a fresh session with atomic
no-replace semantics before swapping live state; no additional lock is introduced.

Canonical session saves retain that same save-file lock while validating the
committed checkpoint head and retaining its predecessor through the existing
immutable artifact writer. Artifact publication acquires no task/session lock;
there is no reverse task-lock edge or additional live-state store. Shared-session
copies read the committed owner instead of advancing an independent checkpoint.

### Rule: always acquire outer → inner, lower number → higher number.

```
L1 (REGISTRY outer map)
 └─► L2 (per-project ProjectBuild)     — NEVER hold L1 while locking L2
```

The `entry_for()` function in `index_orchestrator.rs` enforces this: it locks L1, clones the
`Arc<Mutex<ProjectBuild>>`, **drops** L1, then the caller locks L2 independently. This avoids
deadlock by ensuring L1 and L2 are never held simultaneously.

### Per-file Path Lock (L17)

L17 lives in the shared `core::path_locks` registry and is used by **both** `ctx_read` and
`ctx_edit`. It uses the same outer/inner pattern as L1/L2: the outer `Mutex<HashMap>` is held
briefly to clone the per-path `Arc<Mutex<()>>`, then dropped before the per-file lock is acquired.
The per-file lock is acquired before the global cache lock. This serializes concurrent operations
on the *same* path so only one thread at a time contends on the global cache lock per file; threads
operating on different files proceed independently.

This prevents the thundering-herd scenario where N concurrent subagents all requesting the same
file simultaneously contend on the global cache lock, each holding it during disk I/O.

**Edit path (Issue #320 fix):** `ctx_edit` acquires the L17 per-file lock (bounded `try_lock()`
loop, 30s deadline) and then performs **all** disk I/O — read preimage, replace, TOCTOU recheck,
atomic rename — *without* holding the global cache write-lock. The global cache lock is taken only
twice, each for a sub-millisecond instant: a brief shared `read()` to fetch the recorded read-mode
(for auto-escalation) before the I/O, and a brief exclusive `write()` to apply the deferred
`CacheEffect` (invalidate / store-full) after the I/O. Previously the global cache write-lock was
held across the entire edit, so concurrent agents editing *different* files serialized on it and
the second edit could hit the 10s write-lock timeout. Same-file edit correctness is still guaranteed
by the TOCTOU preimage guard plus the atomic temp-file rename inside `run_io`, not by the cache lock.

**Read path Two-Phase Read (Issue #1098 fix):** `ctx_read` now follows the same pattern as
`ctx_edit` in its slow path (spawned thread):

1. **Phase 1 (shared lock):** Try the `[unchanged]` stub under `cache.try_read()`. This is the
   ~70% case (re-reads of unchanged files) and avoids the write lock entirely. Previously
   missing in the slow path, forcing *every* slow-path call into the write lock.
2. **Phase 2a (no lock):** Disk I/O (`read_file_lossy`) under per-file lock only, *without*
   the cache lock. This is the main contention fix — parallel reads of different files no
   longer serialize on the global cache lock during disk I/O.
3. **Phase 2b (brief write lock):** `handle_with_preread()` receives the pre-read content and
   performs the cache store + compression under the write lock. The lock hold time is reduced
   to CPU-bound work only (hash computation, compression), no longer including disk I/O.
4. **Graph hints:** `graph_related_hint()` (SQLite query, ~50–200ms) is computed *after* the
   cache lock is released, in the registered handler. Previously computed inside
   `handle_with_options_inner` under the write lock.

The fast path (inline, no thread) retains the existing Phase 1 read-lock stub check and uses
the original `handle_with_task_resolved_tuned` (which may still do disk I/O under the
immediately-acquired write lock for the ~10% of calls that miss the fast path).

**Bounded waits (Issue #229 fix):** All lock acquisitions inside the spawned thread use
`try_lock()`/`try_write()` loops with 25s deadlines (inside the 30s `recv_timeout` guard).
When the `recv_timeout` fires, a cancellation flag is set so the thread exits promptly
instead of holding locks indefinitely. The auto-mode selection before the thread uses
`try_read()` with a fallback to "full" mode, ensuring no unbounded blocking.

```
thread::spawn {
    L17 outer (FILE_LOCKS map)       — held briefly to clone Arc, then dropped
     └─► L17 inner (per-file Mutex)  — try_lock() with 25s deadline
          ├─► Phase 1: cache.try_read() — stub hit? → return early
          ├─► Phase 2a: read_file_lossy() — disk I/O, NO cache lock
          └─► Phase 2b: cache.try_write() — brief store + compress
}
```

### Backend Registry (L22) → Config (L4)

`router::with_backend` locks **L22 (`BACKENDS`)** and, on a cache miss, calls
`select_backend → Config::load`, which briefly acquires **L4 (`Config::CACHE`)**. This is
the one sanctioned nested static pair: **L22 (outer) → L4 (inner)**. It is deadlock-free
because `Config::CACHE` is a self-contained cache lock — always released inside
`Config::load` and **never** held while acquiring `BACKENDS`, so no cycle exists.
Rule: never acquire L4 and then L22.

### Worker Thread Tuning

The Tokio runtime worker thread count defaults to `available_parallelism().clamp(1, 4)`.
Override via `LEAN_CTX_WORKER_THREADS` (positive integer) for environments with many
concurrent subagents. Example: `LEAN_CTX_WORKER_THREADS=8`. The blocking thread pool
is always `worker_threads * 4`, clamped to `[8, 32]`.

### Independent Static Locks (L3–L137)

All other static locks (L3–L137) — **except the L22 → L4 pair documented above** — are
**independent singletons**: they protect isolated subsystem state and are never nested inside
each other. Each should be acquired in isolation:

- **Do not hold two static locks at the same time.** If a future change requires locking two
  subsystems, add the ordering rule here first.
- **Hold locks for the minimum duration.** Clone/copy data out, drop the guard, then do work.

### Canonical local observation writes

`ContextBus::Inner::write_conn` remains the SQLite write authority for shared
events and private observations. Observation appends acquire it with a
five-second bounded wait, execute one SQLite transaction, and release it before
returning. SQLite busy waits are bounded at five seconds. No runtime
initialization, callbacks, network, awaits or additional mutex acquisitions occur
while the observation write guard is held. No new mutex is added.

`ContextBus::Inner::read_pool` is acquired separately for observation
checkout/return only; connection opening and row reads occur after that guard
is released. Observation checkouts/returns reject poison and time out after
five seconds.

Legacy event projection and historical-file cache mutexes use the same bounded
observation lock helper. Neither is held during runtime initialization or a
ContextBus transaction. Commit the canonical observation first, release its
writer lock, then update the ring; cache reads finish before canonical reads.

Global OCLA emission reserves its process-local ID before acquiring the canonical
writer. It releases that writer before acquiring the existing bounded OCLA ring
mutex. Failed writes change neither the success counter nor the ring projection.

### Canonical execution replay

`ExecutionLifecycle` owns the existing `DecisionSpine::runs` mutex over
`HashMap<TaskReplayKey, TaskContext>`; typed keys preserve field and optional-value
boundaries. The map lock is released before driver execution or completion.
Each `SharedRun` owns dispatch, completion-state and stage mutexes; the replay
path may acquire dispatch then completion-state, never the reverse while held.
Neither is held across an await or a surface policy callback. A cached result
type mismatch returns `ReplayTypeMismatch`, leaves the cached result intact and
never poisons the dispatch mutex. This change adds no mutex or second authority.

### Session Locks (`tokio::sync::RwLock`)

Session-scoped `RwLock`s on `ToolContext` are logically independent:

- Acquire at most **one session lock per tool handler** at a time.
- If you must acquire two, acquire in field-declaration order (cache → session → tool_calls → …).
- **Never hold a session RwLock while locking a global static Mutex** — this risks priority
  inversion between the tokio runtime and OS threads.

### Test/Environment Locks (E5–E8)

These exist solely to serialise tests that mutate environment variables. They must not be held
across any other lock acquisition.

### Archive filesystem lock

`core/archive.rs` uses the advisory `archives/.lock` file to serialize archive
stores, capacity admission, metadata updates, and cleanup across processes.
Archive code releases this lock before acquiring **L29** for FTS updates.
The FTS cap path already holds **L29** when it calls `remove_files`, so the only
permitted nested order is **L29 → `archives/.lock`**; never acquire them in the
opposite order.

---

## 4. Async Code: `tokio::sync::Mutex` vs `std::sync::Mutex`

| Use | When |
|-----|------|
| `std::sync::Mutex` | Lock held briefly (no `.await` while held), data is `Send` only, or lock is static/global |
| `tokio::sync::Mutex` | Lock must be held **across** `.await` points, or guards must be `Send` for spawned futures |
| `tokio::sync::RwLock` | Readers dominate, writers are rare; lock may be held across `.await` |

### Current usage

- **Global statics** → all `std::sync::Mutex` (correct: locks are held for microseconds, no await)
- **HTTP rate limiter** (`http_server/mod.rs`) → `tokio::sync::Mutex` (correct: held in async handler)
- **Session state** (`tools/mod.rs`) → `tokio::sync::RwLock` (correct: accessed from async tool handlers)
- **Shared sessions** (`core/context_os/shared_sessions.rs`) → `tokio::sync::RwLock` (correct: shared across async channels)

### Rules

1. **Never `.await` while holding a `std::sync::Mutex` guard.** The tokio runtime thread will
   block, starving other tasks.
2. **Prefer `std::sync::Mutex` for global caches** where the critical section is a quick
   read/write with no I/O.
3. **Use `tokio::sync::Mutex` only when the critical section contains `.await`.**
4. A `std::sync::MutexGuard` is `!Send` — you cannot hold it across an `.await` even if you
   wanted to. The compiler enforces this.

---

## 5. Adding New Locks — Checklist

1. Determine scope: global static vs per-session vs per-request.
2. Choose `std::sync` vs `tokio::sync` per Section 4.
3. Assign a lock number (append to Section 1) and document the acquisition order here.
4. If nesting is required, document the outer → inner relationship in Section 3.
5. Run `cargo check --all-features` to verify `Send`/`Sync` bounds.
