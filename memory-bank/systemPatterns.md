# System Patterns

## SSOT: Tool-Manifest
- **Source**: `rust/src/tool_defs.rs` (`granular_tool_defs`, `unified_tool_defs`)
- **Generator**: `rust/src/bin/gen_mcp_manifest.rs`
- **Output (repo-tracked)**: `website/generated/mcp-tools.json`
- **CI Gate**: `rust/tests/mcp_manifest_up_to_date.rs`

## Tool Dispatch
- MCP stdio: `LeanCtxServer` implementiert `rmcp::handler::server::ServerHandler` (in `rust/src/server.rs`).
- Unified Tool `ctx` dispatcht intern auf `ctx_*`.

## Rails / Harness Layer (Phase 2)
- `core/workflow/*`: deterministische State Machine mit erlaubten Tools + Evidence Requirements.
- Gatekeeper: filtert `list_tools` + blockt `call_tool` (immer erlaubt: `ctx`, `ctx_workflow`).
- Evidence Store: Tool-Receipts (Input/Output Hash) + Transition-Gates.

## Observability (local-first)
- `ctx_cost`: CostStore persistiert, wird zentral in `server.rs` befüllt.
- `ctx_heatmap`: Heatmap wird bei Reads instrumentiert (u.a. `ctx_read`, `ctx_multi_read`).

## Phase 3 Transport
- `lean-ctx serve`: MCP **Streamable HTTP** (rmcp transport) via `lean_ctx::http_server`.
- Defaults: loopback bind, Host-header validation (DNS rebinding protection), optional Bearer Auth bei non-loopback.

# System Patterns

## MCP Tool Registry + Dispatch
- **Schemas/Defs**: `rust/src/tool_defs.rs`
  - `granular_tool_defs()` (42 Tools)
  - `unified_tool_defs()` (5 Tools)
- **Dispatch**: `rust/src/server.rs`
  - `ctx` Meta-Tool resolved zu `ctx_*`
  - zentrale Instrumentation (Cost Attribution + Evidence receipts)

## Workflow Runtime
- **Core**: `rust/src/core/workflow/*`
  - `types.rs`: `WorkflowSpec`, `WorkflowRun`, Evidence
  - `engine.rs`: Validierung + Transition Gates
  - `store.rs`: local-first persistence `~/.lean-ctx/workflows/active.json`
- **Tool**: `rust/src/tools/ctx_workflow.rs`
  - actions: start/status/transition/complete/evidence_add/evidence_list/stop

## Tool Gatekeeper
- enforced in `server.rs`:
  - `list_tools`: filtert Tools nach `allowed_tools` des aktiven Workflow-States
  - `call_tool`: blockt disallowed Tools; `ctx` + `ctx_workflow` immer erlaubt

## SSOT Manifest
- Generator: `rust/src/core/mcp_manifest.rs` + `rust/src/bin/gen_mcp_manifest.rs`
- Repo-tracked output: `website/generated/mcp-tools.json`
- CI Gate: `rust/tests/mcp_manifest_up_to_date.rs`

# System Patterns — lean-ctx

## Architecture Overview

```
lean-ctx (single Rust binary, v1.9.0)
├── MCP Server (stdio, via rmcp crate)
│   ├── tools/ — 19 MCP tool implementations
│   │   ├── ctx_read.rs / ctx_multi_read.rs — smart file reading with 6 modes + caching
│   │   ├── ctx_tree.rs — token-efficient directory listings
│   │   ├── ctx_shell.rs — compressed command execution
│   │   ├── ctx_search.rs — pattern search with compact context
│   │   ├── ctx_compress.rs — context checkpoints
│   │   ├── ctx_benchmark.rs — strategy comparison with tiktoken
│   │   ├── ctx_analyze.rs — entropy analysis + mode recommendation
│   │   ├── ctx_metrics.rs — session statistics
│   │   ├── ctx_smart_read.rs — adaptive mode selection
│   │   ├── ctx_delta.rs — Myers diff incremental updates
│   │   ├── ctx_dedup.rs — cross-file deduplication
│   │   ├── ctx_fill.rs — priority-based context filling
│   │   ├── ctx_intent.rs — semantic intent detection
│   │   ├── ctx_response.rs — response compression
│   │   ├── ctx_context.rs — multi-turn context manager
│   │   ├── ctx_graph.rs — project intelligence graph
│   │   ├── ctx_discover.rs — shell history analysis
│   │   └── ctx_cache (inline in server.rs)
│   └── server.rs — tool registration, schema, dispatch
├── Shell Hook
│   ├── shell.rs — exec(), interactive(), save_tee(), mask_sensitive_data()
│   └── cli.rs — all CLI subcommands
├── Core
│   ├── cache.rs — in-memory session cache with MD5 hashing
│   ├── config.rs — ~/.lean-ctx/config.toml management
│   ├── stats.rs — persistent stats in ~/.lean-ctx/stats.json
│   ├── tokens.rs — tiktoken (o200k_base) token counting
│   ├── signatures.rs / signatures_ts.rs — tree-sitter AST extraction (14 languages)
│   ├── compressor.rs — aggressive/diff compression
│   ├── entropy.rs — Shannon entropy filtering + Jaccard dedup
│   ├── deps.rs — dependency graph extraction
│   ├── protocol.rs — structured headers (F1=file.ts [123L +] deps:[...])
│   ├── symbol_map.rs — TDD symbol mapping
│   └── patterns/ — 47 pattern modules for CLI compression (90+ patterns)
│       ├── git, docker, npm, pnpm, cargo, kubectl, gh, terraform
│       ├── pip, ruff, golang, ruby, eslint, prettier, typescript
│       ├── test (jest/vitest/pytest/go), playwright, make, maven
│       ├── dotnet, flutter, poetry, curl, wget, grep, find, ls
│       ├── aws, prisma, helm, bun, deno, swift, zig, cmake
│       ├── ansible, composer, mix, bazel, systemd, psql, mysql
│       └── env_filter, json_schema, log_dedup, next_build, deps_cmd
├── Dashboard
│   ├── mod.rs — HTTP server (localhost:3333)
│   └── dashboard.html — embedded web dashboard (matches leanctx.com design)
├── doctor.rs — 8 diagnostic checks
└── main.rs — CLI dispatch
```

## Key Design Patterns

### Session Cache
- Files cached in-memory with MD5 hash
- Re-reads return 13-token cache-hit message
- Auto-clears after 5 min inactivity (configurable via LEAN_CTX_CACHE_TTL)
- `fresh=true` parameter bypasses cache
- Auto-checkpoint every 10 tool calls (configurable via LEAN_CTX_CHECKPOINT_INTERVAL)

### Pattern Compression (Shell Hook)
- `patterns/mod.rs` routes commands to specific pattern modules
- Each module has `pub fn compress(command: &str, output: &str) -> Option<String>`
- Fallback chain: specific pattern → JSON schema → log dedup → generic truncation
- 47 modules covering 90+ individual command patterns across 34 categories

### Intelligence Tools (v1.9.0+)
- `ctx_smart_read` — auto-selects optimal mode based on file size, type, cache state
- `ctx_delta` — Myers diff algorithm, sends only changed hunks
- `ctx_fill` — priority-based context filling with configurable token budget
- `ctx_intent` — NLP-like intent detection (fix, add, refactor, understand, test, config, deploy)
- `ctx_graph` — builds project dependency graph from imports/exports
- `ctx_context` — tracks what the LLM has seen to avoid redundant re-reads

### Version Management
- Version is **hardcoded** in 7+ places (no env!("CARGO_PKG_VERSION") macro used):
  1. `main.rs` — `--version` output
  2. `main.rs` — `tracing::info!` MCP server log
  3. `main.rs` — `print_help()` header
  4. `server.rs` — `Implementation::new("lean-ctx", "X.Y.Z")`
  5. `shell.rs` — interactive shell prompt
  6. `dashboard/dashboard.html` — `<span class="version">`
  7. `core/stats.rs` — `gain` terminal footer

### Tee Log Security (v1.8.2+)
- `tee_on_error` defaults to **false** (opt-in)
- 7 regex patterns redact sensitive data before writing
- Auto-cleanup: logs older than 24h deleted on next write
- CLI: `lean-ctx tee [list|clear|show]`

### Data Files
- `~/.lean-ctx/stats.json` — persistent statistics
- `~/.lean-ctx/config.toml` — user configuration
- `~/.lean-ctx/tee/` — error output logs (redacted, 24h retention)

## Component Relationships

```
main.rs
  ├── shell::exec() → patterns::compress_output() → stats::record()
  ├── cli::cmd_*() → core modules
  ├── doctor::run() → 8 checks
  ├── dashboard::start() → serves dashboard.html at :3333
  └── tools::create_server() → server.rs → call_tool() → tools/*
```

## USD Calculation
- Standard rate: **$2.50 per 1M tokens**
- Consistent across: CLI gain, dashboard, MCP metrics
- Formula: `saved_tokens * 2.50 / 1_000_000`
