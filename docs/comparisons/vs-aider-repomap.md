# lean-ctx vs Aider repo-map

> **Last updated:** May 2026 | Aider pioneered PageRank-based repo maps for AI coding. lean-ctx brings the same concept to every MCP-compatible agent.

## Overview

| | lean-ctx | Aider repo-map |
|---|---|---|
| **Approach** | MCP-available context layer with PageRank repo-map | Built-in feature of Aider CLI |
| **GitHub Stars** | 1,800+ (lean-ctx) | 43,000+ (Aider) |
| **Language** | Rust | Python |
| **Availability** | MCP server (works with 28 agents) | Locked to Aider CLI |
| **PageRank** | Session-aware personalized PageRank | Personalized PageRank |
| **Scope** | 72+ MCP tools (repo-map is one) | Repo-map + AI coding assistant |

## The Core Difference

**Aider** is a complete AI coding assistant with a built-in repo-map feature. The repo-map uses personalized PageRank to identify the most relevant files and symbols for the current conversation, presenting them as compact elided code views. It's proven technology — Aider consistently scores well on SWE-bench.

**lean-ctx** implements the same PageRank repo-map concept via `ctx_repomap`, but makes it available as an MCP tool that works with any MCP-compatible agent. It also adds session-aware personalization (recent files and task context influence rankings) and integrates with 67 other tools for a complete context engineering workflow.

The key distinction: Aider's repo-map is locked to Aider. lean-ctx's repo-map works with Cursor, Claude Code, Codex, Windsurf, Gemini, and 23 other agents.

## Feature Comparison

| Feature | lean-ctx ctx_repomap | Aider repo-map |
|---------|:--------------------:|:--------------:|
| PageRank algorithm | Personalized power iteration | Personalized PageRank (networkx) |
| Session-aware ranking | Recent files boosted, task context weighting | Chat files boosted |
| Token budget control | `max_tokens` parameter (default 1024) | `--map-tokens` (default 1k) |
| Binary search fitting | Yes | Yes |
| Tree-sitter parsing | 21 languages | 40+ languages |
| Symbol extraction | Functions, classes, traits, structs | Functions, classes, methods |
| Edge weighting | Proper casing +8, private x0.1, active session x50 | Frequency-based logarithmic |
| Caching | mtime-based invalidation | Persistent cache |
| Enhanced dependency maps | Via property graph | `--use-enhanced-map` (import-based) |
| MCP available | Yes (works with 28 agents) | No (Aider CLI only) |
| Embedding-based search | Hybrid BM25 + dense vector | Via Aider's codebase search |
| Shell compression | 95+ patterns | No |
| Session memory | Knowledge graph + temporal facts | Chat history |
| Call graph | Multi-hop BFS | No |
| Impact analysis | ctx_impact (6 actions) | No |
| Observability | Token tracking dashboard | No |

## The PageRank Algorithm

Both tools use the same core idea, inspired by Google's PageRank:

1. **Build a graph**: files are nodes, symbol definitions and references create edges
2. **Compute PageRank**: rank files by their graph centrality (how "connected" they are)
3. **Personalize**: boost files relevant to the current context
4. **Budget-fit**: binary search to select top-ranked symbols within a token limit

### Aider's Implementation

```
Source files → tree-sitter → definitions + references
                                    ↓
                          Graph (files as nodes, refs as edges)
                                    ↓
                          PageRank (personalized by chat files)
                                    ↓
                          Binary search → token budget fit
                                    ↓
                          Elided code view (scope-aware)
```

Aider also supports `--use-enhanced-map` which uses import statements to create a dependency estimator, reducing false edges from symbol name collisions.

### lean-ctx's Implementation

```
Source files → tree-sitter → definitions + references
                                    ↓
                          Property graph (SQLite, 8 node types, 14 edge types)
                                    ↓
                          PageRank (personalized by session state)
                                    ↓
                          Binary search → token budget fit
                                    ↓
                          Compressed signatures (lean-ctx format)
```

lean-ctx uses its existing property graph (which also powers call graphs, impact analysis, and search ranking) instead of a separate networkx graph. The personalization vector draws from:
- **Active session files**: files read or edited in the current session get a boost
- **Task context**: if the agent has an active task, related files rank higher
- **Knowledge graph**: previously learned architectural relationships influence ranking

## MCP Availability: The Key Advantage

Aider's repo-map is arguably the most effective codebase orientation tool for AI agents. But it's only available inside Aider's CLI — you can't use it in Cursor, Claude Code, Windsurf, or any other tool.

lean-ctx makes the same capability available as an MCP tool:

```bash
# From any MCP-compatible agent (Cursor, Claude Code, Codex, ...)
# The agent calls ctx_repomap automatically when it needs codebase orientation

# Or from the CLI
lean-ctx repomap ./my-project --max-tokens 2048
```

This means you get PageRank-based codebase orientation regardless of which AI coding tool you use.

## Beyond Repo-Map: The Full Stack

Aider is a complete AI coding assistant — repo-map is one feature among many (inline editing, git integration, voice coding, etc.).

lean-ctx is a context engineering layer — repo-map is one tool among 68+. The difference is that lean-ctx doesn't try to be the AI coding assistant. It enhances whatever assistant you already use:

| lean-ctx Feature | Complements repo-map by... |
|-----------------|--------------------------|
| ctx_read (10 modes) | Compressing the files that repo-map identifies as important |
| ctx_search (hybrid) | Finding specific code when repo-map gives the overview |
| ctx_callgraph | Tracing execution paths through repo-map's ranked symbols |
| ctx_impact | Understanding blast radius of changes to top-ranked files |
| Session memory | Remembering which parts of the repo-map were explored |
| Shell compression | Compressing build/test output after making changes |

## Language Support

Aider supports 40+ languages through tree-sitter. lean-ctx currently supports 21. For codebases in less common languages, Aider has broader coverage. lean-ctx's language support is actively expanding.

| Language Category | lean-ctx | Aider |
|-------------------|:--------:|:-----:|
| Major (JS/TS/Python/Rust/Go/Java) | Yes | Yes |
| Common (C/C++/C#/Ruby/PHP/Swift) | Yes | Yes |
| Emerging (Zig, Elixir, Dart) | Partial | Yes |
| Niche (COBOL, Fortran, Verilog) | No | Partial |

## When to Use Which

### Choose Aider if you...

- Want a complete AI coding assistant (not just context tools)
- Prefer a CLI-based workflow with inline editing
- Need repo-map for 40+ languages
- Value SWE-bench proven performance
- Don't need the repo-map in other AI tools

### Choose lean-ctx if you...

- Use Cursor, Claude Code, Codex, or other MCP-compatible agents
- Want PageRank repo-map in your existing workflow (without switching to Aider)
- Need compression, memory, and code intelligence alongside repo-map
- Run multi-agent workflows where context needs to be shared
- Want real-time observability of context window usage

### Use Both

lean-ctx and Aider can coexist. lean-ctx supports Aider as an MCP client (`lean-ctx init --agent aider`). You can use Aider with lean-ctx providing additional context tools — including using lean-ctx's repo-map as a complement to or replacement for Aider's built-in one.

## Summary

Aider deserves credit for pioneering PageRank-based repo maps in AI coding — it's a proven concept that significantly improves AI agent performance. lean-ctx brings this same capability to the broader MCP ecosystem, making it available to 28 agents instead of just one.

If you're an Aider user, lean-ctx can enhance your workflow with additional compression and memory tools. If you use other AI coding tools, lean-ctx gives you access to PageRank repo-maps that were previously Aider-exclusive.

---

*Aider is an excellent AI coding tool. We recommend trying both and choosing what fits your workflow.*

[Get started with lean-ctx](https://leanctx.com/docs/getting-started) | [Aider on GitHub](https://github.com/Aider-AI/aider) | [Aider repo-map docs](https://aider.chat/docs/repomap.html)
