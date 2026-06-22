# lean-ctx Comparisons

> **How does lean-ctx compare to other context and memory tools for AI agents?**

We believe in transparent, fact-based comparisons. Every page below includes real feature data, honest assessments of competitor strengths, and guidance on when each tool is the right choice.

## Quick Comparison Matrix

| | lean-ctx | Repomix | codebase-memory | claude-context | Aider repo-map | Mem0 |
|---|:---:|:---:|:---:|:---:|:---:|:---:|
| **Stars** | 1.8k+ | 25k+ | 3k+ | 11.5k+ | 43k+ | 55k+ |
| **MCP Tools** | **77** | 8 | 14 | 3 | 0 | 9 |
| **Read Modes** | **10** | 0 | 0 | 0 | 0 | 0 |
| **Token Compression** | **99%** | ~70% | 99%+ | ~40% | N/A | N/A |
| **Shell Compression** | **95+** | — | — | — | — | — |
| **PageRank Repo-Map** | **MCP** | — | — | — | CLI only | — |
| **Call Graph** | **Yes** | — | Yes | — | — | — |
| **Semantic Search** | **Hybrid** | — | Yes | Yes | — | Yes |
| **Session Memory** | **Yes** | — | — | — | — | Yes |
| **Knowledge Graph** | **Temporal** | — | Yes | — | — | Yes |
| **Multi-Agent** | **Yes** | — | — | — | — | Yes |
| **100% Local** | **Yes** | Yes | Yes | No | Yes | No |
| **Single Binary** | **Rust** | Node.js | C | Node.js | Python | Python |
| **Agents Supported** | **28** | Any MCP | 11 | 2-3 | 1 (Aider) | Any MCP |
| **Stability Contract** | **29 frozen/stable contracts, CI-enforced** | — | — | — | — | — |

## Which Tool Should I Use?

### "I want to pack my repo and paste it into ChatGPT"
**Use [Repomix](vs-repomix.md).** It's the simplest, most popular tool for one-shot codebase packing. `npx repomix` and you're done.

### "I need deep structural code intelligence (call paths, dead code, architecture)"
**Use [codebase-memory](vs-codebase-memory.md).** It's the fastest code indexer with 155 language support and sub-millisecond graph queries. Consider lean-ctx if you also need compression and session memory.

### "I need semantic code search for Claude Code"
**Use [lean-ctx](vs-claude-context.md) if you want local-first operation.** Use [claude-context](vs-claude-context.md) if you want cloud-scale embedding models. Both provide hybrid BM25 + vector search.

### "I want PageRank-based repo maps"
**Use [Aider](vs-aider-repomap.md) if you want a complete AI coding assistant.** Use [lean-ctx](vs-aider-repomap.md) if you want repo-maps in Cursor, Claude Code, or other MCP-compatible agents.

### "I need cross-session memory for my AI agents"
**Use [Mem0](vs-mem0.md) for general-purpose AI memory** (chatbots, assistants, customer support). Use [lean-ctx](vs-mem0.md) for code-specific memory with compression and structural intelligence.

### "I want to compress free-form prose / chat history / RAG context"
**Use [The Token Company](vs-token-company.md) for cloud ML prose compression.** Use [lean-ctx](vs-token-company.md) when the content is code or tool output, you need 100% local operation, or you need deterministic, prompt-cache-preserving output.

### "I want a drop-in `compress(messages)` library like Headroom"
**Use [Headroom](vs-headroom.md) for ML prose compression and the widest set of framework wrappers.** Use [lean-ctx](vs-headroom.md) when you need deterministic, prompt-cache-safe output, 100% local operation in a single binary, or compression alongside cached reads, search and memory.

### "I want all of the above in one tool"
**Use lean-ctx.** It's the only tool that combines compression, memory, code intelligence, semantic search, repo-maps, and observability in a single binary.

## Detailed Comparisons

| Comparison | Key Distinction | Read More |
|------------|----------------|-----------|
| [**lean-ctx vs Repomix**](vs-repomix.md) | Live context layer vs snapshot packer — 99% vs 70% compression | [Full comparison →](vs-repomix.md) |
| [**lean-ctx vs codebase-memory**](vs-codebase-memory.md) | Broad context layer vs deep code intelligence — 77 tools vs 14 | [Full comparison →](vs-codebase-memory.md) |
| [**lean-ctx vs claude-context**](vs-claude-context.md) | 100% local vs cloud-dependent — 77 tools vs 3 | [Full comparison →](vs-claude-context.md) |
| [**lean-ctx vs Aider repo-map**](vs-aider-repomap.md) | MCP-available vs CLI-locked — PageRank for 28 agents | [Full comparison →](vs-aider-repomap.md) |
| [**lean-ctx vs Mem0**](vs-mem0.md) | Code-specific vs general-purpose — local vs cloud | [Full comparison →](vs-mem0.md) |
| [**lean-ctx vs The Token Company**](vs-token-company.md) | Local deterministic code compression vs cloud ML prose compression | [Full comparison →](vs-token-company.md) |
| [**lean-ctx vs Headroom**](vs-headroom.md) | Deterministic, prompt-cache-safe `compress()` + full context layer vs ML compression library | [Full comparison →](vs-headroom.md) |

## What Makes lean-ctx Different

lean-ctx is the only tool that covers all three layers of AI agent context:

### Layer 1: Compression
10 file read modes, 95+ shell compression patterns, cached re-reads (~13 tokens). Every interaction uses fewer tokens.

### Layer 2: Memory
Temporal knowledge graph, session persistence, episodic memory. Context survives across chats and sessions.

### Layer 3: Intelligence
PageRank repo-maps, call graphs, blast radius analysis, hybrid semantic search. The agent understands your code structurally.

No other tool in this space covers all three layers. Most focus on one: Repomix on compression, Mem0 on memory, Aider on intelligence.

### And one guarantee none of them make: stability

Since v1.0, every lean-ctx surface is governed by a published stability policy
([CONTRACTS.md](../../CONTRACTS.md)): 29 protocol contracts classified
frozen/stable/experimental, frozen surfaces SHA-256-locked in CI, and a public
`/v1` API that can only grow. Integrations built on lean-ctx cannot silently
break — a claim no other tool in this matrix makes.

## Our Approach to Comparisons

- **Factual and data-driven**: real feature counts, real star counts, real capabilities
- **Honest about competitor strengths**: every comparison page includes a "where they lead" section
- **Updated regularly**: star counts and feature sets are verified against latest releases
- **No FUD**: we don't exaggerate weaknesses or minimize competitor accomplishments
- **Try both**: every page includes links to the competitor's GitHub and docs

---

*Last updated: June 2026. Star counts and features reflect latest public releases; the lean-ctx tool count is generated from the registry (`docs/reference/generated/mcp-tools.md`).*

[Get started with lean-ctx →](https://leanctx.com/docs/getting-started)
