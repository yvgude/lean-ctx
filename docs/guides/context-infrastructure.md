# lean-ctx as Context Infrastructure

lean-ctx is more than a file-read compressor: it is the **infrastructure layer
between your agents and everything they need to know**. This page is the map of
that layer — which sources flow in, what one shared pipeline does with them, how
retrieval gets them back out, and where the escape hatches are. Every capability
below exists today; each section links to its reference.

```text
 SOURCES                    ONE PIPELINE                     RETRIEVAL
 ─────────                  ────────────                     ─────────
 repo files      ┐                                     ┌─ ctx_search (BM25)
 linked repos    │   chunk → index → consolidate       ├─ ctx_search --semantic
 docs/artifacts  ├──▶ BM25 + SPLADE + dense vectors ──▶├─   (hybrid: RRF + rerank)
 GitHub/GitLab   │   + code graph + knowledge facts    ├─ ctx_multi_repo
 Jira/Postgres   │                                     ├─ ctx_graph / ctx_callgraph
 any MCP server  ┘                                     └─ ctx_knowledge
                                                            │
                          PORTABILITY: OKF (Markdown) · signed .ctxpkg
                          EXTENSION:   addons behind the gateway
```

## Sources — what flows in

| Source | How it enters | Reference |
|---|---|---|
| **The repo** | tree-sitter AST chunking (20+ languages), incremental BM25 + embedding index | [How retrieval works](https://leanctx.com/docs/concepts/how-retrieval-works) |
| **More repos** | `ctx_multi_repo` roots, or linked workspace projects fused into one result list | [Monorepo guide](monorepo.md) |
| **Docs & artifacts** | project doc folders declared in `.lean-ctx-artifacts.json`, indexed separately from code | `ctx_search action=semantic artifacts=true` |
| **Issue trackers & DBs** | first-class providers: GitHub, GitLab, Jira, Postgres schema | `lean-ctx provider …`, `ctx_provider` |
| **Any MCP server** | the MCP bridge provider + gateway addons | [Addons](addons.md) |

Provider results don't just pass through: with `providers.auto_index = true`
they are **consolidated** — chunked into the BM25 index, linked into the code
graph, distilled into knowledge facts. An issue that references `src/auth.rs`
becomes findable from the file, and vice versa.

## Retrieval — one hybrid engine, not one signal

Every retrieval surface runs the same engine: lexical **BM25** + learned-sparse
**SPLADE** + **dense vectors** from a local ONNX model, fused with **Reciprocal
Rank Fusion**, sharpened by code-aware reranking and graph spreading activation.
No external embedding API, no index-build marathon — and each piece degrades
gracefully (cold dense index → BM25 floor, never a failed query).

Two dials matter in practice:

- **The embedding model is swappable** — any HuggingFace ONNX export via
  `[embedding].model = "hf:org/repo@rev"`, including code-specialized models.
  See [Custom Embedding Models](custom-embeddings.md).
- **The vector store is swappable** — in-process by default, or delegate dense
  search to a self-hosted Qdrant. See [Dense Backends](dense-backends.md).

The quality floor is measurable, not asserted: the benchmark scorecard
(recall@5/10, MRR, determinism digest) ships with the repo.

## Memory — what the layer learns

Sessions distill into **knowledge**: facts, patterns, decisions and typed
relations per project (`ctx_knowledge`, `ctx_session`). Retrieval consults this
store alongside code — and it is never locked in:

- **[OKF](okf-interop.md)** — export/import the knowledge base as vendor-neutral,
  git-diffable Markdown (one concept per file, relations as links).
- **[ctxpkg](knowledge-formats.md)** — the same snapshot as a signed, verifiable
  bundle for distribution.

## Extension — when you want more than the core

The core stays local and lean on purpose. Heavier machinery — external RAG
stacks, graph databases, specialized doc search — plugs in as
**[addons](addons.md)** behind the gateway instead of bloating the binary:
one `lean-ctx addon add <name>`, and the tool's MCP surface joins your agent's
toolbox with scrubbed env, pinned versions and typed output adapters.

Examples from the registry relevant to this page: `qmd` (local Markdown/notes
search), `memgraph-ingester` (structure-aware RAG on a Memgraph code graph),
`cognee` (GraphRAG knowledge graphs).

## Design commitments

1. **Local first.** Embeddings, indexes and knowledge live on your machine
   unless you explicitly point them elsewhere.
2. **Deterministic outputs.** Tool output is a pure function of content + mode,
   which keeps provider prompt caches hot (#498).
3. **The right half of the pipeline.** Compress what fits into the window;
   retrieve only what doesn't. See [lean-ctx vs naive
   RAG](../comparisons/vs-naive-rag.md) for when a dedicated vector DB is
   genuinely the better tool.
4. **No lock-in.** Everything the layer accumulates leaves as OKF or ctxpkg.
