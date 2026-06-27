# LeanCTX Feature Catalog (SSOT Snapshot)

**Version:** `3.8.1`  
**Updated:** `2026-05-15`  
**Primary Sources:** `website/generated/mcp-tools.json`, `rust/src/tool_defs/granular.rs`, `README.md`

---

## Purpose

This catalog is the single feature inventory for LeanCTX at release/runtime level:

- Which MCP tools exist now
- Which entry points are canonical vs deprecated aliases
- Which read modes are supported
- Which shell pattern modules exist
- Which CLI commands are available
- Which capabilities are part of the shipped product surface

---

## Runtime Surface (Current)

- Granular MCP tools: **79**
- Unified MCP tools: **5**
- MCP Resources: **5**
- MCP Prompts: **5**
- Dynamic Tool Categories: **6**
- Shell pattern modules: **56** (fd, just, ninja, clang, extended cargo run/bench added in 3.4.x)
- CLI commands: **80+** (knowledge, overview, compress, serve --daemon/--stop/--status, session, pack create/list/info/export/import/install/remove/auto-load subcommands added in 3.4.x)
- Read modes: **10** (`auto`, `full`, `map`, `signatures`, `diff`, `aggressive`, `entropy`, `task`, `reference`, `lines:N-M`)
- Positioning: Context Runtime for AI agents (shell hook + context server + setup integrations)

---

## Unified MCP Tools (5)

- `ctx`
- `ctx_read`
- `ctx_search`
- `ctx_shell`
- `ctx_tree`

---

## Granular MCP Tools (79)

### A) Read / Search / IO Surface

- `ctx_read`
- `ctx_multi_read`
- `ctx_smart_read`
- `ctx_tree`
- `ctx_search`
- `ctx_semantic_search`
- `ctx_shell`
- `shell` _(alias of `ctx_shell` — gives Codex Desktop/Cloud the same shell-output compression; registered for all MCP clients)_
- `ctx_edit`
- `ctx_delta`
- `ctx_dedup`
- `ctx_fill`
- `ctx_outline`
- `ctx_symbol`
- `ctx_routes`
- `ctx_context`
- `ctx_compose`
- `ctx_explore` _(FastContext-style bounded multi-turn exploration → `path:start-end` citations; locates code across files at a fraction of `ctx_compose`'s tokens)_

### B) Architecture / Analysis / Discovery

- `ctx_graph` _(actions: build, related, symbol, impact, status, enrich, context, diagram)_
- `ctx_callgraph` _(direction=callers|callees)_
- `ctx_refactor` _(LSP: rename, references, definition, implementations)_
- `ctx_architecture`
- `ctx_impact`
- `ctx_review`
- `ctx_pack`
- `ctx_index`
- `ctx_artifacts`
- `ctx_intent`
- `ctx_task`
- `ctx_overview`
- `ctx_preload`
- `ctx_prefetch`
- `ctx_discover`
- `ctx_analyze`

### C) Session / Knowledge / Multi-Agent

- `ctx_session`
- `ctx_knowledge`
- `ctx_agent`
- `ctx_share`
- `ctx_handoff`
- `ctx_workflow`
- `ctx_feedback`

#### Knowledge CLI (`lean-ctx knowledge`)

Full CLI parity with MCP `ctx_knowledge`:

- `remember <value> --category <c> --key <k>` — store a fact
- `recall [query] [--category <c>] [--mode auto|semantic|hybrid]` — retrieve facts
- `search <query>` — cross-project knowledge search
- `export [--format json|jsonl|simple] [--output <path>]` — export knowledge (stdout or file)
- `import <path> [--merge replace|append|skip-existing] [--dry-run]` — import from JSON/JSONL
- `remove --category <c> --key <k>` — remove a fact
- `consolidate [--all]` — import latest session if present, run lifecycle, then leave 25% facts/history/procedures capacity free; `--all` repeats this for every stored project root
- `status` — knowledge base summary
- `health` — health report with quality metrics
- `lifecycle` — read-only lifecycle/capacity report

Import supports three formats: native `ProjectKnowledge` JSON, simple `[{category, key, value}]` array, and JSONL (one fact per line). The `simple` format serves as the community interop schema for migration from other tools.

### D) Compression / Metrics / Runtime Ops

- `ctx_cache`
- `ctx_compress`
- `ctx_expand` _(actions: retrieve, list, search_all — FTS5 cross-archive fulltext search)_
- `ctx_call`
- `ctx_compress_memory`
- `ctx_metrics`
- `ctx_cost`
- `ctx_heatmap`
- `ctx_gain` _(actions: wrapped, summary, delta)_
- `ctx_execute`
- `ctx_benchmark`
- `ctx_compare` _(preview compression — original vs the bytes lean-ctx would emit + token counts and line diff, read-only)_
- `ctx_response`
- `ctx_tools` _(MCP Tool-Catalog Gateway — actions: find, call, list, refresh; routes/proxies unlimited downstream MCP servers at constant context cost)_

---

## MCP Protocol Capabilities

### MCP Resources (5)

Subscribe-capable resources, gated by client capabilities:

- `lean-ctx://context/summary`
- `lean-ctx://context/pressure`
- `lean-ctx://context/plan`
- `lean-ctx://context/pinned`
- `lean-ctx://context/bounce`

### MCP Prompts (5)

Appear as slash commands in supporting IDEs:

- `/context-focus`
- `/context-review`
- `/context-reset`
- `/context-pin`
- `/context-budget`

### Elicitation

Rate-limited context decisions triggered by:

- Pressure >90%
- Large files >5k tok
- Budget exhaustion

Fallback hints emitted for non-supporting IDEs.

### Dynamic Tool Categories (6)

On-demand loading via `ctx_load_tools` + `notifications/tools/list_changed`:

- **core** (~27 tools, always loaded)
- **arch**
- **debug**
- **memory**
- **metrics**
- **session**

`ctx_load_tools` _(actions: load, unload, list)_ — explicit category management at runtime. After each change, `notifications/tools/list_changed` is sent to subscribed clients.

---

## Intelligence Layer

### Context Gate (Active)

Pre-dispatch mode override:

- **Overlay override** (pin/exclude/set_view)
- **Pressure-based auto-downgrade** (ForceCompression: full→map, EvictLeastRelevant: map→signatures)
- Bounce-prevention
- Intent-target (with real task from SessionState)
- Graph-proximity
- Knowledge-relevance

Post-dispatch:

- Ledger recording (with real task Φ computation)
- Reinjection plan (downgrade existing "full" entries to "map" under pressure)
- Eviction/elicitation hints
- `notifications/resources/updated` on significant ledger changes

### Bounce Detection

Tracks wasted tokens from compressed→full re-reads:

- Per-extension bounce rates
- Adjusts savings metrics to report honest numbers

### Client Capability Detection

Runtime detection of 9 IDE clients:

- Cursor, Claude Code, CodeBuddy, Windsurf, Zed, VS Code Copilot, Kiro, Codex, Antigravity, Gemini CLI

Tier 1–4 classification determines feature gating for resources, prompts, elicitation, and dynamic tools.

---

## Removed Aliases (v3.6.1+)

Previously deprecated aliases have been removed. Use the canonical tools:
- `ctx_callgraph direction=callers` (was: ctx_callers)
- `ctx_callgraph direction=callees` (was: ctx_callees)
- `ctx_graph action=diagram` (was: ctx_graph_diagram)
- `ctx_gain action=wrapped` (was: ctx_wrapped)

---

## Capabilities (3.4.x Additions)

### Daemon Mode
- Unix Domain Socket server via `lean-ctx serve --daemon`
- Control via `--stop`, `--status` flags
- Persistent background process for lower-latency MCP serving

### Multi-Tokenizer Support
- `o200k_base` (GPT-4o, default)
- `cl100k_base` (GPT-4 / GPT-3.5)
- Gemini tokenizer
- Llama tokenizer

### Hook Modes
- `HookMode::Mcp` — MCP server only (IDE-extension agents without reliable shell hooks)
- `HookMode::Hybrid` — MCP server + shell hooks (default where shell access exists)

### Smart Mode Selection in Setup
- `lean-ctx setup` auto-detects editor and agent, selects optimal hook mode

### SKILL.md Auto-Installation
- `lean-ctx init` writes `SKILL.md` to agent-specific skill directories
- Auto-detects Cursor, Claude Code, CodeBuddy, Codex, Gemini CLI, Kiro skill paths

### Compressed Output Cache
- `map` and `signatures` read modes cache compressed output
- Re-reads of compressed representations cost ~13 tokens

### Intent-Aware Read Mode Selection
- `ctx_read mode=auto` uses task intent to select optimal compression
- Factors: file type, file size, task signal, access history

### Prefix-Cache-Friendly Output Ordering
- Imports and type definitions emitted before function bodies
- Stable ordering maximizes LLM provider KV-cache hits

### Field-Wise Profile Merge
- Context profiles merged field-by-field (not replaced wholesale)
- Allows layered profile composition (base + project + role)

### Token Counting Deduplication
- Cross-file shared block detection via `ctx_dedup`
- Deduplicated blocks counted once in budget calculations

### Graph-Powered Context OS (3.4.7)

#### Multi-Edge Graph Queries
- Property Graph queries traverse `imports`, `calls`, `exports`, `type_ref`, `tested_by`
- Weighted BFS: imports=1.0, calls=0.8, exports=0.7, type_ref=0.5, tested_by=0.4
- New query: `related_files(path, limit)` — scored file neighbors
- New query: `file_connectivity(path)` — edge-type breakdown per file

#### Graph-Aware File Reads
- Every `ctx_read` includes a `[related: ...]` hint from the Property Graph
- Scored related files shown with relationship strength (e.g., `config.rs (80%)`)
- Agent understands file context immediately without extra tool calls

#### Calls + Exports Edge Materialization
- Graph build writes `EdgeKind::Calls` edges (symbol-to-symbol and file-to-file)
- Graph build writes `EdgeKind::Exports` for exported symbols (symbol-to-file)
- Impact analysis now tracks function-level blast radius, not just file-level

#### Incremental Graph Update
- `ctx_impact action="update"` uses `git diff --name-only` to detect changed files
- Only changed files are re-indexed (remove_file_nodes + re-parse)
- Deleted files have their nodes removed automatically
- Full build hints when incremental update is available

#### Session Survival Engine
- `build_compaction_snapshot()` generates structured recovery XML:
  - `<recovery_queries>`: executable `ctx_read`/`ctx_search` commands
  - `<knowledge_context>`: `ctx_knowledge recall` queries from task keywords
  - `<graph_context>`: dependency cluster references for touched files
- Budget-aware with automatic section shrinking

#### Knowledge-Enriched Overview
- `ctx_overview` includes top relevant Knowledge facts for the current task
- Graph architectural hotspots: files ranked by edge count (imports + calls)

#### Hybrid Search Fusion (RRF)
- `ctx_semantic_search` combines 3 signals via Reciprocal Rank Fusion:
  - BM25 (lexical), Dense Embeddings (semantic), Graph Proximity (structural)
- `score = sum(1 / (60 + rank_i))` over all signals
- Graph neighbors of recently touched files get score boost

#### Progressive Search Throttling
- Per-session tracker for repeated pattern+path combinations
- Calls 1-3: normal, 4-6: hint to use `ctx_knowledge remember`, 7+: throttle hint
- Auto-reset after 5 minutes idle

#### Sandbox-First Routing
- `ctx_shell` output >5KB: hint to use `ctx_execute` for compression
- `ctx_read` full mode >10K tokens: hint to use `map`/`aggressive`
- Once-per-session deduplication

#### Terse Mode
- `ctx_session action="configure" terse=true` enables concise response mode
- Injects instruction into resume block: focus on code/actions, avoid filler
- Survives context compaction via snapshot

#### Context Package System (3.4.7)

Context packages bundle Knowledge, Graph, Session, Patterns, and Gotchas into portable `.ctxpkg` files that can be shared, versioned, and auto-loaded across projects.

##### Package Layers
- **Knowledge**: facts, patterns, consolidated insights from ProjectKnowledge
- **Graph**: nodes + edges from the Property Graph (full SQLite export)
- **Session**: task description, findings, decisions, next steps, files touched
- **Patterns**: project patterns extracted from knowledge
- **Gotchas**: gotcha entries with triggers, resolutions, file patterns

##### CLI Commands (9 subcommands under `lean-ctx pack`)
- `create` — build package from current project context (knowledge, graph, session, gotchas)
- `list` — list all installed packages with layers, size, auto-load status
- `info` — detailed package view (stats, integrity, provenance, estimated tokens)
- `remove` — remove package from local registry
- `export` — export to portable `.ctxpkg` file (JSON bundle with SHA-256 integrity)
- `import` — import from `.ctxpkg` file into local registry
- `install` — apply package to current project (merge knowledge, import graph, import gotchas)
- `auto-load` — enable/disable automatic loading on `ctx_overview` session start
- `pr` — existing PR context pack (unchanged)

##### Premium Features
- **SHA-256 Content Integrity**: canonical compact JSON hashing, verified on every load
- **Atomic Writes**: tmp + rename pattern prevents corruption
- **Knowledge Merge**: duplicate detection (category + key + value), confidence capped at 0.8 for imports
- **Graph Overlay**: nodes and edges imported directly into the SQLite Property Graph
- **Gotcha Import**: ID-based dedup, severity mapping, confidence capping
- **Auto-Load**: packages flagged `auto_load=true` loaded on every `ctx_overview` call
- **Schema Versioning**: `CONTEXT_PACKAGE_V1_SCHEMA_VERSION` in `contracts.rs`
- **Compression Stats**: gzip-based compression ratio tracked in manifest

---

## Notes For Releases

- Tool counts and tool names must match `website/generated/mcp-tools.json`.
- Shell pattern module count must match `rust/src/core/patterns/mod.rs` module list.
- Any new tool or alias change requires synchronized updates in:
  - `README.md` and relevant package READMEs
  - `rust/src/templates/*` where applicable
  - this catalog
- Historical counts in old CHANGELOG entries remain unchanged by design.
