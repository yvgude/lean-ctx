# Dense Backends — where your vectors live

The dense (embedding) half of lean-ctx's hybrid retriever needs a vector store.
By default that store is **in-process and on disk** — no service, no container,
nothing to operate. For teams that already run a vector database, lean-ctx can
delegate dense search to it instead.

| Backend | Runs where | Setup | Best for |
|---|---|---|---|
| **local** (default) | inside the lean-ctx process, persisted next to the BM25 index | none | individuals and most teams — zero-ops, deterministic |
| **qdrant** | your [Qdrant](https://qdrant.tech) server (self-hosted or cloud) | `LEANCTX_QDRANT_URL` | fleets sharing one index, corpora beyond one machine's RAM |

Both backends consume the **same embedding pipeline** — the local ONNX model
selected via [`[embedding].model`](custom-embeddings.md) produces the vectors;
only the storage and the nearest-neighbor search move. BM25, SPLADE, RRF fusion
and reranking are unaffected by the backend choice.

## Selecting a backend

```bash
# Explicit
export LEANCTX_DENSE_BACKEND=local    # default
export LEANCTX_DENSE_BACKEND=qdrant

# Implicit: setting a Qdrant URL selects the qdrant backend automatically
export LEANCTX_QDRANT_URL=http://127.0.0.1:6333
```

An unknown value fails fast with a clear error rather than silently falling
back — retrieval quality should never degrade without you noticing.

## Qdrant configuration

```bash
export LEANCTX_QDRANT_URL=http://127.0.0.1:6333   # required
export LEANCTX_QDRANT_API_KEY=…                   # optional (Qdrant Cloud / secured instances)
export LEANCTX_QDRANT_TIMEOUT_SECS=10             # optional, default 10
export LEANCTX_QDRANT_COLLECTION_PREFIX=lctx_code_ # optional, default shown
```

- **Collections are per project and per model dimension** — the collection name
  is derived from the project root's namespace hash and the embedding model's
  vector width, so switching models can never mix incompatible vectors.
- **Sync is incremental.** On each dense search lean-ctx upserts only chunks of
  files that changed since the last sync (delete-by-file, then re-upsert). A
  fresh collection is populated once.
- **The build stays quiet.** The `qdrant` build feature is enabled in release
  binaries; requesting the backend without the feature produces an explicit
  error, not a silent local fallback.

Run a local Qdrant for testing:

```bash
docker run -p 6333:6333 qdrant/qdrant
LEANCTX_QDRANT_URL=http://127.0.0.1:6333 lean-ctx search --semantic "auth flow"
```

## What stays true regardless of backend

- Embeddings are produced **locally** (ONNX; swappable via
  [`hf:org/repo`](custom-embeddings.md)) — no embedding API, no per-token fees.
- The lexical BM25 floor is always available: if the dense backend is
  unreachable, hybrid search degrades to BM25 with a warning instead of failing
  the query.
- Chunk identity is content-derived, so re-indexing an unchanged corpus is a
  no-op on the store.

*See also: [Context Infrastructure](context-infrastructure.md),
[Custom Embedding Models](custom-embeddings.md),
[lean-ctx vs naive RAG](../comparisons/vs-naive-rag.md).*
