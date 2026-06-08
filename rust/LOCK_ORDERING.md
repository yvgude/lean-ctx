# Lock Ordering — lean-ctx Rust Codebase

This document catalogues every global/static lock and notable `Arc<Mutex/RwLock>` in the
codebase, defines the intended acquisition order, and records rules for async code.

---

## 1. Global / Static Locks

All `std::sync::Mutex` unless noted otherwise.

| # | Lock | File | Type | Purpose |
|---|------|------|------|---------|
| L1 | `REGISTRY` | `core/index_orchestrator.rs:57` | `OnceLock<Mutex<HashMap<String, Arc<Mutex<ProjectBuild>>>>>` | Outer map of per-project build state |
| L2 | per-project `ProjectBuild` | `core/index_orchestrator.rs:57` (inner) | `Arc<Mutex<ProjectBuild>>` | Individual project build progress |
| L3 | `HEATMAP_BUFFER` | `core/heatmap.rs:10` | `Mutex<Option<HeatMap>>` | Buffered access-frequency heatmap |
| L4 | `Config::CACHE` | `core/config/mod.rs:885` | `Mutex<Option<(Config, SystemTime, Option<SystemTime>)>>` | Config file cache with mtime check |
| L5 | `FEEDBACK_BUFFER` | `core/feedback.rs:9` | `Mutex<Option<(FeedbackStore, Instant)>>` | Buffered user feedback |
| L6 | `PREDICTOR_BUFFER` | `core/mode_predictor.rs:8` | `Mutex<Option<(Arc<ModePredictor>, Instant)>>` | Cached mode predictor model |
| L7 | `STATS_BUFFER` | `core/stats/mod.rs:13` | `Mutex<Option<(StatsStore, StatsStore, Instant)>>` | Token-savings statistics |
| L8 | `COST_BUFFER` | `core/a2a/cost_attribution.rs:69` | `Mutex<Option<CostStore>>` | A2A cost tracking |
| L9 | `GLOBAL_LIMITER` | `core/a2a/rate_limiter.rs:121` | `Mutex<Option<RateLimiter>>` | Global A2A rate limiter |
| L10 | `DETECTOR` | `core/anomaly.rs:222` | `OnceLock<Mutex<AnomalyDetector>>` | Anomaly detection state |
| L11 | `SLO_CONFIG` | `core/slo.rs:101` | `OnceLock<Mutex<Vec<SloDefinition>>>` | SLO definitions |
| L12 | `VIOLATION_LOG` | `core/slo.rs:102` | `OnceLock<Mutex<ViolationHistory>>` | SLO violation history |
| L13 | `EMIT_STATE` | `core/slo.rs:103` | `OnceLock<Mutex<HashMap<String, EmitState>>>` | SLO emission dedup state |
| L14 | `ACTIVE_ROLE_NAME` | `core/roles.rs:12` | `OnceLock<Mutex<String>>` | Currently active role name |
| L15 | `PROVIDER_CACHE` | `core/providers/cache.rs:5` | `LazyLock<Mutex<ProviderCache>>` | Cached provider metadata |
| L16 | `LAST_BANDIT_ARM` | `core/adaptive_thresholds.rs:337` | `Mutex<Option<(String, String, String)>>` | Last bandit arm selection for adaptive thresholds |
| L17 | `FILE_LOCKS` | `tools/registered/ctx_read.rs` | `OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>>` | Per-file read serialization for concurrent subagents |
| L18 | `LAST_HASH` | `core/audit_trail.rs:52` | `Mutex<Option<String>>` | Dedup hash for audit trail entries |
| L19 | `CACHE` (graph) | `core/graph_cache.rs:31` | `OnceLock<Mutex<HashMap<String, Entry>>>` | Property graph query result cache |
| L20 | `RECENT` | `core/auto_findings.rs:15` | `Mutex<Vec<RecentEntry>>` | Recent auto-finding entries |
| L21 | `LOCK` (home) | `core/home.rs:79` | `Mutex<()>` | Serialize home directory creation |
| L22 | `CLIENTS` | `lsp/router.rs:11` | `LazyLock<Mutex<HashMap<String, LspClient>>>` | LSP client connection registry |
| L23 | `BUDGETS` | `core/agent_budget.rs:6` | `Mutex<Option<HashMap<String, AgentBudget>>>` | Per-agent token budget tracking |
| L24 | `SHELL_ENV_LOCK` | `shell_hook.rs:928` | `Mutex<()>` | Serialize env-var access in shell hook |
| L25 | `TRACKER` | `core/search_delta.rs:49` | `Mutex<Option<SearchDeltaTracker>>` | Tracks search result changes between calls |
| L26 | `SESSION_ID` | `server/bypass_hint.rs:9` | `Mutex<Option<String>>` | Current bypass hint session ID |
| L27 | `CACHE` (git) | `core/git_cache.rs:10` | `LazyLock<Mutex<GitCache>>` | Cached git metadata (branch, status) |
| L28 | `STORE` (refs) | `server/reference_store.rs:15` | `OnceLock<Mutex<HashMap<String, RefEntry>>>` | Function reference store for Fn-ref system |
| L29 | `DB` | `core/archive_fts.rs:7` | `LazyLock<Mutex<Option<Connection>>>` | SQLite FTS archive connection |
| L30 | `LOCK` (prop-graph) | `core/property_graph/mod.rs:423` | `Mutex<()>` | Serialize property graph test access |
| L31 | `GLOBAL` (dyn-tools) | `server/dynamic_tools.rs:232` | `OnceLock<Mutex<DynamicToolState>>` | Dynamic tool registration state |
| L32 | `APPLIED_PACKAGES` | `core/context_package/auto_load.rs:6` | `Mutex<Option<HashSet<String>>>` | Track which context packages have been applied |
| L33 | `CACHE` (search) | `core/search_index.rs:401` | `OnceLock<Mutex<HashMap<String, CacheEntry>>>` | BM25 search index query cache |
| L34 | `GLOBAL` (capabilities) | `core/client_capabilities.rs:188` | `OnceLock<Mutex<ClientMcpCapabilities>>` | Client MCP capability flags |
| L35 | `LOCK` (doctor) | `doctor/workspace_scope.rs:132` | `Mutex<()>` | Serialize doctor workspace scope tests |
| L36 | `BUILD` | `core/call_graph.rs:54` | `OnceLock<Mutex<BuildState>>` | Call graph build state |
| L37 | `LAST_REAL` | `proxy/introspect.rs:54` | `Mutex<[Option<String>; 3]>` | Last 3 real (non-proxy) request paths |
| L38 | `GLOBAL_TRACKER` | `core/bounce_tracker.rs:226` | `OnceLock<Mutex<BounceTracker>>` | Tracks repeated tool-call bounces |
| L39 | `GLOBAL_REGISTRY` | `core/plugins/mod.rs:10` | `OnceLock<Mutex<PluginRegistry>>` | Loaded plugin registry |
| L40 | `GLOBAL_MANAGER` | `core/multi_repo.rs:363` | `OnceLock<Mutex<MultiRepoManager>>` | Multi-repo workspace manager |
| L41 | `KNOWLEDGE_LOCKS` | `core/knowledge/persist.rs` | `OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>>` | Per-project knowledge.json read-modify-write serialization |
| L42 | `LAST_PARSE_ERROR` | `core/config/mod.rs:440` | `Mutex<Option<String>>` | Most recent global `config.toml` parse error (surfaced by doctor/diagnostics) |
| L43 | `POLICY_CACHE` | `server/permission_inheritance.rs:53` | `OnceLock<Mutex<Option<CacheEntry>>>` | Cached host-IDE permission policy (TTL-bounded) for permission inheritance |
| L44 | `POOL` | `core/providers/mod.rs:28` | `OnceLock<Mutex<HashSet<&'static str>>>` | Provider string-interning pool; bounds per-construction leaks to the finite set of distinct provider ids/names/actions |

### Test / Environment Locks (serialise env-var mutations)

| # | Lock | File | Purpose |
|---|------|------|---------|
| E1 | `ENV_LOCK` | `dashboard/mod.rs:537` | Serialize env-var access in dashboard tests |
| E2 | `ENV_LOCK` | `core/dense_backend.rs:412` | Serialize env-var access in dense-backend tests |
| E3 | `ENV_LOCK` | `core/workspace_config.rs:101` | Serialize env-var access in workspace-config tests |
| E4 | `LOCK` | `core/data_dir.rs:50` | Serialize data-dir creation |
| E5 | `LOCK` | `core/tokens.rs:190` | Serialize tokenizer tests |
| E6 | `LOCK` | `core/tokenizer_translation_driver.rs:248` | Serialize tokenizer-translation tests |

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
| `SharedSessions.session` | `core/context_os/shared_sessions.rs:31` | `Arc<tokio::sync::RwLock<SessionState>>` | Shared session state across channels |

---

## 3. Lock Acquisition Order

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

**Bounded waits (Issue #229 fix):** All lock acquisitions inside the spawned thread use
`try_lock()`/`try_write()` loops with 25s deadlines (inside the 30s `recv_timeout` guard).
When the `recv_timeout` fires, a cancellation flag is set so the thread exits promptly
instead of holding locks indefinitely. The auto-mode selection before the thread uses
`try_read()` with a fallback to "full" mode, ensuring no unbounded blocking.

```
thread::spawn {
    L17 outer (FILE_LOCKS map)       — held briefly to clone Arc, then dropped
     └─► L17 inner (per-file Mutex)  — try_lock() with 25s deadline
          └─► cache (session RwLock) — try_write() with 25s deadline
}
```

### Worker Thread Tuning

The Tokio runtime worker thread count defaults to `available_parallelism().clamp(1, 4)`.
Override via `LEAN_CTX_WORKER_THREADS` (positive integer) for environments with many
concurrent subagents. Example: `LEAN_CTX_WORKER_THREADS=8`. The blocking thread pool
is always `worker_threads * 4`, clamped to `[8, 32]`.

### Independent Static Locks (L3–L44)

All other static locks (L3–L44) are **independent singletons** — they protect isolated subsystem
state and are never nested inside each other. Each should be acquired in isolation:

- **Do not hold two static locks at the same time.** If a future change requires locking two
  subsystems, add the ordering rule here first.
- **Hold locks for the minimum duration.** Clone/copy data out, drop the guard, then do work.

### Session Locks (`tokio::sync::RwLock`)

Session-scoped `RwLock`s on `ToolContext` are logically independent:

- Acquire at most **one session lock per tool handler** at a time.
- If you must acquire two, acquire in field-declaration order (cache → session → tool_calls → …).
- **Never hold a session RwLock while locking a global static Mutex** — this risks priority
  inversion between the tokio runtime and OS threads.

### Test/Environment Locks (E1–E6)

These exist solely to serialise tests that mutate environment variables. They must not be held
across any other lock acquisition.

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
- **Team audit file** (`http_server/team.rs`) → `tokio::sync::Mutex` (correct: held across `tokio::fs::File` writes)
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
