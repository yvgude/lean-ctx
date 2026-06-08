# lean-ctx vs claude-context (Zilliz)

> **Last updated:** May 2026 | Both tools add semantic code search to AI agents — but with very different architectures and privacy models.

## Overview

| | lean-ctx | claude-context |
|---|---|---|
| **Approach** | Local-first cognitive context layer | Cloud-dependent semantic search plugin |
| **GitHub Stars** | 1,800+ | 11,500+ |
| **Language** | Rust (single binary) | TypeScript (Node.js monorepo) |
| **License** | Apache 2.0 | MIT |
| **MCP Tools** | 68+ | 3-4 |
| **Dependencies** | None (self-contained) | OpenAI API + Milvus/Zilliz Cloud |
| **Privacy** | 100% local | Code embeddings sent to external APIs |

## The Core Difference

**claude-context** (by Zilliz) adds semantic code search to Claude Code and other agents by indexing your codebase into a vector database (Milvus or Zilliz Cloud). It's a focused tool: index your code, search it semantically, done.

**lean-ctx** provides semantic search *as one of 72+ tools* in a comprehensive context layer. It runs entirely locally — no API keys, no external vector database, no Docker containers. Beyond search, it adds file compression, shell compression, session memory, multi-agent support, and observability.

## Feature Comparison

| Feature | lean-ctx | claude-context |
|---------|:--------:|:--------------:|
| Semantic search | Hybrid BM25 + dense vector (local) | Hybrid BM25 + dense vector (cloud) |
| File read compression | 10 modes (map, signatures, diff, ...) | No |
| Cached re-reads | ~13 tokens | No |
| Shell output compression | 95+ patterns | No |
| Session memory | Knowledge graph + temporal facts | No |
| Multi-agent support | ctx_agent, ctx_handoff, diary | No |
| Call graph analysis | Multi-hop BFS + risk classification | No |
| Blast radius / impact | ctx_impact (6 actions) | No |
| Architecture overview | ctx_architecture (9 actions) | No |
| Repo-map (PageRank) | ctx_repomap (session-aware) | No |
| Code packing | ctx_pack (.ctxpkg, PR packs) | No |
| Incremental indexing | Git-diff based updates | Merkle-tree auto-sync |
| AST-based chunking | Tree-sitter (21 languages) | Tree-sitter (14 languages) |
| Embedding providers | Built-in ONNX (local) | OpenAI, VoyageAI, Ollama, Gemini |
| Observability dashboard | Real-time token tracking | No |
| VS Code extension | Planned | Available |
| Agent support | 28 agents auto-configured | Claude Code, Cursor (manual config) |
| Installation | Single binary, `lean-ctx setup` | `npx` + API keys + Milvus setup |
| Privacy | 100% local, no external calls | Requires external embedding API |

## Privacy and Architecture

This is the most significant difference between the two tools.

### claude-context requires external services

```bash
claude mcp add claude-context \
  -e OPENAI_API_KEY=sk-your-key \
  -e MILVUS_ADDRESS=your-zilliz-endpoint \
  -e MILVUS_TOKEN=your-token \
  -- npx @zilliz/claude-context-mcp@latest
```

To use claude-context, you need:

1. **An OpenAI API key** (or VoyageAI/Gemini key) — your code chunks are sent to an external embedding API
2. **A Milvus instance or Zilliz Cloud account** — your code embeddings are stored in an external vector database
3. **Node.js runtime** — runs via `npx`

This means your code content leaves your machine during indexing. Every code chunk is sent to OpenAI (or another provider) for embedding generation.

### lean-ctx runs 100% locally

```bash
curl -fsSL https://leanctx.com/install.sh | sh
lean-ctx setup
# Done. No API keys, no external services, no Docker.
```

lean-ctx ships a built-in ONNX embedding model (~15 MB). All embedding generation and vector search happens locally. Your code never leaves your machine.

| Privacy Aspect | lean-ctx | claude-context |
|---------------|----------|---------------|
| Code leaves machine | Never | Yes (embedding API) |
| External API required | No | Yes (OpenAI/VoyageAI/Gemini) |
| External database | No (SQLite, local) | Yes (Milvus/Zilliz Cloud) |
| Docker required | No | Milvus requires Docker (unless using Zilliz Cloud) |
| Internet required | No (after install) | Yes (for every index/search) |
| SOC2 / compliance | Local-first (your responsibility) | Depends on Zilliz Cloud compliance |

## Semantic Search: Quality Comparison

Both tools provide hybrid search (BM25 + dense vector), but with different trade-offs:

### claude-context strengths
- Access to state-of-the-art cloud embedding models (OpenAI text-embedding-3-large, VoyageAI code models)
- Zilliz Cloud scales to very large codebases (millions of vectors)
- Ollama option for local embeddings (if you run your own models)

### lean-ctx strengths
- Zero-latency local embeddings (no API round-trip)
- Property graph proximity boosts search ranking (files connected in the code graph rank higher)
- Session-aware: recent files and active task context influence search results
- Search results integrate with compression (found code is returned in the optimal read mode)

```bash
# lean-ctx semantic search
# Hybrid BM25 + dense vector + graph proximity, ranked via RRF
lean-ctx search "where is authentication handled"

# Results include: file path, relevance score, compressed code snippet
# Graph proximity boosts files connected to recently active context
```

## Beyond Search: What lean-ctx Adds

claude-context is specifically a semantic search plugin. lean-ctx provides semantic search as part of a larger system:

### Compression (saves tokens on every interaction)
```bash
# 10 read modes — agent gets exactly the level of detail it needs
lean-ctx read src/auth/middleware.ts -m map        # architecture overview
lean-ctx read src/auth/middleware.ts -m signatures  # API surface only
lean-ctx read src/auth/middleware.ts -m diff        # only what changed

# Shell output compression
lean-ctx -c "git log --oneline -20"    # 80% fewer tokens
lean-ctx -c "npm test"                  # 90%+ fewer tokens
```

### Session Memory (context persists across chats)
```bash
# Agent decisions, findings, and file context survive chat restarts
# No need to re-index or re-discover architecture every session
```

### Code Intelligence (structural understanding)
```bash
# Call graph with multi-hop traversal
# Impact analysis before making changes
# Architecture overview in a single call
# PageRank-based repo map for codebase orientation
```

### Multi-Agent Coordination
```bash
# Hand off context between agents
# Shared knowledge graph across agent instances
# Diary system for cross-agent communication
```

## Installation Comparison

### claude-context setup
```bash
# 1. Get an OpenAI API key ($$$)
# 2. Set up Milvus (Docker) or create Zilliz Cloud account
docker run -d --name milvus -p 19530:19530 milvusdb/milvus:latest
# 3. Configure MCP with environment variables
claude mcp add claude-context \
  -e OPENAI_API_KEY=sk-... \
  -e MILVUS_ADDRESS=localhost:19530 \
  -- npx @zilliz/claude-context-mcp@latest
# 4. Index your codebase (sends code to OpenAI)
```

### lean-ctx setup
```bash
# 1. Install
curl -fsSL https://leanctx.com/install.sh | sh
# 2. Setup (auto-detects your AI tools)
lean-ctx setup
# 3. Done. Restart your shell and editor.
```

## When to Use Which

### Choose claude-context if you...

- Want the highest possible embedding quality (cloud models)
- Already use Zilliz Cloud or have Milvus infrastructure
- Only need semantic search (not compression, memory, or code intelligence)
- Are comfortable with code being processed by external APIs
- Primarily use Claude Code

### Choose lean-ctx if you...

- Need 100% local operation (compliance, air-gapped, or privacy-first)
- Want compression, memory, and code intelligence alongside search
- Use multiple AI agents (28 supported vs 2-3)
- Don't want to manage Docker containers or external API keys
- Want token savings on every interaction, not just search queries
- Need session memory that persists across conversations

## Summary

claude-context is a well-built semantic search plugin backed by Zilliz's vector database expertise. With 11.5k+ stars, it has strong community adoption.

The fundamental trade-off is architecture: claude-context requires external services (embedding APIs + vector database) in exchange for access to state-of-the-art cloud models. lean-ctx runs entirely locally with no external dependencies, providing semantic search as one capability in a comprehensive 72+ tool context layer.

If privacy and local-first operation matter to you — or if you want more than just search — lean-ctx is the more complete solution.

---

*Both projects are open source and under active development.*

[Get started with lean-ctx](https://leanctx.com/docs/getting-started) | [claude-context on GitHub](https://github.com/zilliztech/claude-context)
