# lean-ctx vs codebase-memory-mcp

> **Last updated:** May 2026 | Two high-performance code intelligence MCP servers — similar goals, different strengths.

## Overview

| | lean-ctx | codebase-memory-mcp |
|---|---|---|
| **Approach** | Cognitive context layer (compress + memory + governance) | Code intelligence engine (knowledge graph + structural queries) |
| **GitHub Stars** | 1,800+ | 3,000+ |
| **Language** | Rust | C |
| **License** | Apache 2.0 | Proprietary |
| **MCP Tools** | 68+ | 14 |
| **Tree-sitter Languages** | 21 | 155 |
| **Token Reduction** | Up to 99% (context-aware, 10 modes) | 99%+ (graph-derived structural queries) |

## The Core Difference

**codebase-memory-mcp** excels at structural code intelligence: it parses your entire codebase into a persistent knowledge graph and answers structural queries (call paths, architecture, dead code) in sub-millisecond time. It's the fastest indexer in the space — the Linux kernel (28M LOC) in 3 minutes.

**lean-ctx** is a broader context engineering layer that includes structural intelligence *and* file read compression, shell output compression, session memory, multi-agent coordination, observability, and governance. Where codebase-memory focuses deep on the graph, lean-ctx covers the full agent workflow.

## Feature Comparison

| Feature | lean-ctx | codebase-memory-mcp |
|---------|:--------:|:-------------------:|
| Knowledge graph | SQLite property graph (8 node types, 14 edge types) | SQLite knowledge graph |
| Call graph | Multi-hop BFS + risk classification | Call-path tracing |
| Blast radius / impact | ctx_impact (6 actions, file + symbol level) | Impact analysis |
| Architecture overview | ctx_architecture (9 actions) | get_architecture |
| Dead code detection | Via property graph queries | Dedicated tool |
| Semantic search | Hybrid BM25 + dense vector (embeddings) | Semantic search (v0.6.0+) |
| Cross-service linking | Via property graph | HTTP route matching (REST, gRPC, GraphQL) |
| Cross-repo intelligence | Multi-repo serve mode | CROSS_* edges (v0.6.1+) |
| LSP type resolution | ctx_refactor (rust-analyzer, tsserver, pylsp, gopls) | Go, C, C++, TypeScript/JSX |
| File read compression | 10 modes (map, signatures, diff, entropy, ...) | No |
| Cached re-reads | ~13 tokens | No |
| Shell output compression | 95+ patterns (git, npm, cargo, docker, ...) | No |
| Session memory | Knowledge graph + temporal facts + findings | No |
| Multi-agent support | ctx_agent, ctx_handoff, diary, sync | No |
| Repo packing | ctx_pack (.ctxpkg bundles, PR packs) | Team-shared graph artifacts (.db.zst) |
| PageRank repo-map | ctx_repomap (session-aware) | No |
| Observability dashboard | Real-time token tracking, budgets, SLOs | No |
| Context proof / verification | ctx_proof, ctx_verify (4-layer engine) | No |
| Plugin system | Hook-based (pre_read, post_compress, ...) | No |
| ADR management | Via knowledge graph | manage_adr tool |
| Cypher queries | No | Direct Cypher support |
| Agent auto-setup | 28 agents | 11 agents |
| Privacy | 100% local, no telemetry by default | 100% local |
| Installation | Single binary + `lean-ctx setup` | Single static binary + `install` |

## Shared Strengths

Both tools share important qualities that set them apart from lighter alternatives:

- **Single binary, zero dependencies** — no Docker, no Node.js runtime, no Python
- **100% local** — your code never leaves your machine
- **Knowledge graph architecture** — structural understanding, not just text search
- **Tree-sitter parsing** — real AST analysis, not regex
- **Call graph and blast radius** — understand impact before making changes
- **Sub-second queries** — both use SQLite for fast graph operations
- **Cross-repo support** — work across multiple repositories

## Where codebase-memory Leads

### Language Coverage
codebase-memory supports 155 languages via tree-sitter (expanded from 66 in v0.6.1). lean-ctx currently supports 21. For polyglot codebases with uncommon languages (COBOL, Fortran, Verilog, GLSL), codebase-memory has broader coverage.

### Indexing Speed
codebase-memory claims the Linux kernel (28M LOC, 75K files) indexes in 3 minutes with sub-millisecond query latency. It's specifically optimized for raw structural indexing speed.

### Cross-Service Detection
codebase-memory has dedicated detection for REST routes, gRPC services, GraphQL schemas, and tRPC endpoints with confidence-scored HTTP call site matching. lean-ctx handles cross-service relationships through its property graph but doesn't have specialized protocol detection.

### Cypher Queries
codebase-memory exposes direct Cypher query support, letting power users write arbitrary graph queries. lean-ctx uses its own property graph API.

## Where lean-ctx Leads

### Compression and Token Efficiency (daily savings)

lean-ctx's core value proposition — compressing every file read and shell command — has no equivalent in codebase-memory:

```bash
# lean-ctx: 10 read modes adapt to what the agent needs
lean-ctx read src/main.rs -m map          # ~95% reduction
lean-ctx read src/main.rs -m signatures   # ~98% reduction
lean-ctx read src/main.rs -m diff         # only changed lines

# Cached re-read: ~13 tokens (file unchanged)
lean-ctx read src/main.rs
```

codebase-memory answers structural queries efficiently but doesn't compress raw file reads or shell output. When an agent needs to actually read a file, it reads the full file.

### Shell Output Compression

95+ pattern modules compress git, npm, cargo, docker, kubectl, terraform output. This alone can save thousands of tokens per session:

```bash
# Raw `git log --oneline -20`: ~400 tokens
# Through lean-ctx: ~80 tokens

# Raw `cargo test` output: ~2000 tokens
# Through lean-ctx: ~150 tokens
```

### Session Memory and Knowledge Persistence

lean-ctx maintains a temporal knowledge graph that persists across chat sessions:

- Facts with validity windows (`was_valid_at()` queries)
- Session findings, decisions, and blockers
- Episodic and procedural memory
- Structured recovery from context compaction

codebase-memory persists its code graph but doesn't track agent decisions, task progress, or conversational knowledge.

### Multi-Agent Coordination

lean-ctx provides dedicated tools for multi-agent workflows: `ctx_agent` for handoffs with context transfer bundles, diary system for cross-agent communication, and synchronized shared state.

### Observability and Governance

lean-ctx includes a real-time dashboard (`lean-ctx dashboard`), token budget controls, SLO policies, and cryptographic context proofs — enterprise-grade observability for context window management.

## Architecture Comparison

```
codebase-memory-mcp:
  Source Code → tree-sitter → Knowledge Graph → MCP Query Tools
                                    ↓
                              Graph Artifacts (.db.zst)

lean-ctx:
  Source Code → tree-sitter → Property Graph → MCP Intelligence Tools
       ↓                          ↓
  File Reads → Compression → Session Cache → MCP Read Tools
       ↓                          ↓
  Shell Output → Pattern Matching → Compressed Output
       ↓                          ↓
  Agent State → Knowledge Graph → Session Memory → Multi-Agent Sync
                                          ↓
                                    Observability Dashboard
```

## When to Use Which

### Choose codebase-memory if you...

- Need structural intelligence across 155+ languages
- Primarily ask graph-based questions (call paths, architecture, dead code)
- Want the fastest possible indexing of very large codebases
- Need cross-service linking (REST/gRPC/GraphQL detection)
- Want team-shared graph artifacts committed to your repo

### Choose lean-ctx if you...

- Use AI coding agents daily and want token savings on every interaction
- Need shell output compression alongside code intelligence
- Want session memory that persists across conversations
- Run multi-agent workflows with handoffs and shared state
- Care about observability and governance of context window usage
- Want repo packing, semantic search, and code intelligence in one tool

### Use Both

The tools don't conflict. You could run codebase-memory for deep structural queries and lean-ctx for compression, session memory, and shell output. Both are single-binary MCP servers that coexist without issues.

## Summary

codebase-memory-mcp and lean-ctx are the two most capable code intelligence MCP servers available. codebase-memory leads in language coverage (155 vs 18) and raw indexing speed. lean-ctx leads in breadth — 72+ tools covering compression, memory, search, governance, and multi-agent support alongside structural intelligence.

The choice depends on your workflow: if you primarily need a fast, deep code graph, codebase-memory excels. If you want a comprehensive context layer that saves tokens on every interaction, lean-ctx covers more ground.

---

*Both projects are under active development. Numbers reflect May 2026 releases.*

[Get started with lean-ctx](https://leanctx.com/docs/getting-started) | [codebase-memory on GitHub](https://github.com/DeusData/codebase-memory-mcp)
