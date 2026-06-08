# lean-ctx vs Repomix

> **Last updated:** May 2026 | Both tools help AI agents understand codebases — but they take fundamentally different approaches.

## Overview

| | lean-ctx | Repomix |
|---|---|---|
| **Approach** | Live context layer with session memory | Snapshot-based codebase packer |
| **GitHub Stars** | 1,800+ | 25,000+ |
| **Language** | Rust (single binary) | TypeScript (Node.js) |
| **License** | Apache 2.0 | MIT |
| **MCP Tools** | 68+ | 8 |
| **Compression** | Up to 99% (10 modes, context-aware) | ~70% (tree-sitter `--compress`) |

## The Core Difference

**Repomix** packs your entire codebase into a single file (XML, Markdown, or JSON) so you can paste it into an LLM prompt. It's a one-shot snapshot — great for quick questions about a repo you just cloned.

**lean-ctx** is a persistent context layer that sits between your AI agent and your codebase. It caches reads, compresses shell output in real-time, tracks session state, and builds a knowledge graph across conversations. It doesn't just pack — it *understands* and *remembers*.

## Feature Comparison

| Feature | lean-ctx | Repomix |
|---------|:--------:|:------:|
| File read compression | 10 modes (map, signatures, diff, entropy, ...) | Tree-sitter extract (`--compress`) |
| Token reduction | Up to 99% | ~70% |
| Cached re-reads | ~13 tokens | N/A (re-packs every time) |
| Shell output compression | 95+ patterns (git, npm, cargo, docker, ...) | No |
| Session memory | Knowledge graph + temporal facts | No |
| Multi-agent support | ctx_agent, ctx_handoff, diary, sync | No |
| Semantic search | Hybrid BM25 + dense vector | No |
| Call graph analysis | Multi-hop BFS + risk classification | No |
| Blast radius / impact | ctx_impact (6 actions) | No |
| Repo-map (PageRank) | ctx_repomap (session-aware) | No |
| Repo packing | ctx_pack (PR packs, .ctxpkg bundles) | Core feature (XML/MD/JSON/Plain) |
| Remote repo support | Via ctx_pack | Native (GitHub URLs) |
| Security scanning | PathJail, shell allowlist | Secretlint |
| Observability dashboard | Real-time token tracking, budgets | No |
| VS Code extension | Planned | No |
| Tree-sitter languages | 21 | 30+ |
| Agent support | 28 agents auto-configured | Works with any MCP client |
| Privacy | 100% local, no telemetry by default | 100% local |
| Installation | Single binary, `lean-ctx setup` | `npx repomix` or npm install |

## When to Use Which

### Choose Repomix if you...

- Need to quickly pack a repo and paste it into ChatGPT, Claude, or another web UI
- Want one-shot codebase context without installing anything (`npx repomix`)
- Work primarily with remote GitHub repos you don't have locally
- Prefer a simple tool that does one thing well

### Choose lean-ctx if you...

- Use AI coding agents daily (Cursor, Claude Code, Codex, Windsurf, ...)
- Want context to persist across chat sessions
- Work on medium/large codebases where re-reading files wastes tokens
- Need shell output compression (git, test runners, build tools)
- Want semantic search, call graphs, and impact analysis alongside context packing
- Care about real-time observability of context window usage

## Compression: 99% vs 70%

Repomix's `--compress` flag uses tree-sitter to extract key code elements, achieving approximately 70% token reduction. This is a static, one-pass operation.

lean-ctx offers 10 context-aware read modes that adapt to what the agent actually needs:

```bash
# Map mode: dependency graph + exports + key signatures
lean-ctx read src/server/mod.rs -m map        # ~95% reduction

# Signatures: API surface only
lean-ctx read src/server/mod.rs -m signatures # ~98% reduction

# Diff mode: only changed lines (after edits)
lean-ctx read src/server/mod.rs -m diff       # ~99% reduction

# Cached re-read: file hasn't changed
lean-ctx read src/server/mod.rs               # ~13 tokens
```

The key difference: lean-ctx compression is **context-aware**. It knows what you've already read, what changed, and what the current task requires. Repomix treats every pack as a fresh snapshot.

## Session Memory vs Snapshots

With Repomix, every interaction starts from zero. Pack the repo, feed it to the LLM, get an answer, repeat.

With lean-ctx, the agent builds cumulative knowledge:

```bash
# Session 1: Agent discovers architecture
# lean-ctx remembers: "Auth is in src/auth/, uses JWT, depends on user service"

# Session 2: Agent picks up where it left off
# lean-ctx recalls previous findings, decisions, and file context
# No need to re-read and re-analyze the entire codebase
```

This is especially valuable for multi-day refactoring, debugging sessions, or feature development across multiple chat conversations.

## Shell Compression

Repomix focuses exclusively on file content. lean-ctx also compresses shell output — which often dominates context window usage in real coding sessions:

```bash
# Raw git status: ~800 tokens
# lean-ctx compressed: ~120 tokens

# Raw npm install output: ~3000 tokens
# lean-ctx compressed: ~200 tokens

# Raw cargo test output: ~2000 tokens
# lean-ctx compressed: ~150 tokens
```

95+ pattern modules cover git, npm, cargo, docker, kubectl, terraform, and more.

## Migration from Repomix

If you're currently using Repomix and want to try lean-ctx:

### 1. Install lean-ctx

```bash
curl -fsSL https://leanctx.com/install.sh | sh
lean-ctx setup
```

### 2. Replace repo packing with live context

Instead of:
```bash
npx repomix --compress -o context.xml
# Then paste context.xml into your LLM
```

Use lean-ctx's MCP tools directly from your AI agent:
```
# Your agent can now call ctx_read, ctx_search, ctx_repomap
# No manual packing needed — context is served on demand
```

### 3. For one-shot packing, use ctx_pack

```bash
# Pack entire repo (like Repomix, but with lean-ctx compression)
lean-ctx pack create ./my-project -o context.ctxpkg

# Build a PR-focused context pack
lean-ctx pack --pr
```

### 4. Keep both

lean-ctx and Repomix don't conflict. You can use Repomix for quick one-off packing and lean-ctx as your daily context layer. They solve different problems at different scales.

## Summary

Repomix is an excellent tool for what it does: pack a codebase into an LLM-friendly format. With 25k+ stars, it's proven and well-maintained.

lean-ctx goes further by providing a complete context engineering layer — compression is just one of 72+ tools. If you use AI coding agents daily and want persistent memory, shell compression, semantic search, and real-time observability, lean-ctx is built for that workflow.

---

*Both projects are open source. We encourage you to try both and choose what fits your workflow.*

[Get started with lean-ctx](https://leanctx.com/docs/getting-started) | [Repomix on GitHub](https://github.com/yamadashy/repomix)
