# lean-ctx vs naive RAG

> **Last updated:** June 2026 | "Naive RAG" here means the common pattern: chunk
> everything, embed the chunks into a vector DB, retrieve top-k by cosine
> similarity, stuff the results into the prompt. It's a great default for large,
> unstructured corpora — and a poor default for a codebase.

## Overview

| | lean-ctx | Naive RAG |
|---|---|---|
| **Core idea** | Two halves under one pipeline: *compress into the window* when the material fits, *retrieve when it's too big* | Retrieve top-k chunks by vector similarity, always |
| **Retrieval** | Hybrid: BM25 + learned-sparse SPLADE + dense vectors, fused with RRF, then reranked | Single-signal: dense cosine top-k |
| **Structure awareness** | Tree-sitter AST + code graph (calls, deps, blast radius) | None — text chunks only |
| **Determinism** | Byte-stable outputs (prompt-cache safe) | Depends on the embedding/index; often not |
| **Locality** | 100% local single binary | Usually an external vector DB + embedding API |
| **Portability of memory** | OKF (Markdown) + signed ctxpkg | Vendor-specific index |

## The core difference: two problems, not one

Knowledge management for agents is really *two* problems, and naive RAG only
addresses one of them:

1. **"It fits, but it's verbose."** A file, a diff, a shell log, a handful of
   docs. The right move is to **compress it into the window** losslessly — read
   modes, structural crushing, cached re-reads. Embedding-and-retrieving here
   *loses* information you already had room for. This is lean-ctx's default and
   its origin.

2. **"It's too big to fit."** A large or dynamic knowledge base. Now you must
   **retrieve** — and lean-ctx does, with a *hybrid* retriever (lexical BM25 +
   dense embeddings, fused with RRF, optionally reranked), not a single cosine
   signal.

Naive RAG applies tool #2 to *everything*, including material that never needed
retrieval. That's the "context-stuffing" failure mode: more chunks, lower signal,
higher cost, and answers that quietly drift because the model is reasoning over
lossy fragments. lean-ctx picks the right half for the material, under one
pipeline.

## The structure-aware moat

A codebase is not a bag of paragraphs. Functions call functions; a change has a
blast radius; a symbol has a definition and references. lean-ctx is
**structure-aware**: it parses 20+ languages with tree-sitter, builds a code
graph, and can answer "who calls this / what breaks if I change it" — questions a
pile of embedded text chunks fundamentally cannot answer.

Naive vector search treats `getUser()` and the string "get user" as roughly the
same thing. lean-ctx knows one is a symbol with callers and the other is prose.
That structural signal is the moat: it's what makes retrieval *precise* on code,
and it's why lean-ctx doesn't rely on embeddings alone. The graph is used at
*retrieval* time too — associative spreading activation (ACT-R style) surfaces
structurally close code, and the reranker is grounded in 2025 code-retrieval
research (CoRNStack, SACL, SweRank). Embeddings come from a **local ONNX model**
(swappable; a model2vec fast path skips the attention pass), so this runs with no
external vector DB, no embedding API, and no minutes-long, CPU-melting index
build — the exact overhead that makes people wary of RAG.

## Trust: a reproducible retrieval floor

lean-ctx ships a **benchmark scorecard** (recall@5 / recall@10 / MRR) with a
`determinism_digest`, plus a dual-arm cost evaluation — so the retrieval quality
floor and the savings are numbers you can re-run, not marketing. Embeddings are
default-on in the hybrid retriever; the lexical BM25 floor is deterministic and
reproducible on its own.

## Portable, not locked in

When knowledge leaves a naive-RAG system, it leaves as a vendor-specific vector
index. lean-ctx exports knowledge as **[OKF](../guides/okf-interop.md)** — plain,
git-diffable Markdown — or as a signed, verifiable **ctxpkg** for distribution.
Your accumulated project knowledge is yours to read, edit, review, and move.

## Where naive RAG is the right call

We're honest about this — naive RAG (or a plain vector DB) is a better fit when:

- **The corpus is huge and unstructured** — millions of documents, support
  tickets, web pages, PDFs — where there is no structure to exploit and
  brute-force semantic recall is exactly what you want.
- **You need cross-domain semantic search at scale**, decoupled from any one
  repo, as a standalone service many apps query.
- **You already run a vector DB** and want the simplest possible "embed + top-k"
  path with no code-structure requirements.

lean-ctx is built for **coding agents on a codebase**. If your problem is
"semantic search over a giant text corpus," a dedicated vector database is the
right tool — and lean-ctx's hybrid retriever can still complement it for the code
half.

## When to use which

| Your situation | Use |
|---|---|
| A coding agent that reads files, runs shells, navigates a repo | **lean-ctx** |
| Knowledge that must be small in-window, precise, and cheap | **lean-ctx** (compress) |
| A large project knowledge base that needs recall | **lean-ctx** (hybrid retrieve) |
| A giant, unstructured, cross-app text corpus | a dedicated **vector DB / RAG** |
| You want portable, hand-editable, no-lock-in memory | **lean-ctx** (OKF) |

## Summary

Naive RAG answers one question — "retrieve top-k similar chunks." lean-ctx
answers the real question — "get the right knowledge into the window, whether by
compressing what fits or retrieving what doesn't" — and does it with structural
awareness of code, deterministic output, and portable memory. Different tools for
different jobs; for coding agents, structure and the two-halves pipeline win.

---

*See also: [How retrieval works](https://leanctx.com/docs/concepts/how-retrieval-works),
[Context Infrastructure](../guides/context-infrastructure.md),
[Dense Backends (local / Qdrant)](../guides/dense-backends.md),
[Knowledge Formats](../guides/knowledge-formats.md),
[lean-ctx vs Mem0](vs-mem0.md), [lean-ctx vs claude-context](vs-claude-context.md).*
