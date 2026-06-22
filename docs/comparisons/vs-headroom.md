# lean-ctx vs Headroom

> **Last updated:** June 2026 | Both expose a drop-in `compress(messages, model)`
> library that strips boilerplate from agent context before it reaches the LLM.
> They differ most in *how* they compress and *what else* they bring.

## Overview

| | lean-ctx | Headroom |
|---|---|---|
| **Approach** | Local context-engineering layer with a deterministic compression funnel | Compression library + proxy with optional ML compression |
| **Drop-in API** | `compress(messages, model)` (Py + TS) | `compress(messages, model)` (Py + TS) |
| **Runtime** | Single Rust binary + loopback daemon | Python package (`headroom-ai`) / Node package |
| **License** | Apache-2.0 | Apache-2.0 |
| **Determinism** | Byte-stable output, prompt-cache safe (#498) | Not a stated contract |
| **Locality** | 100% local, no telemetry by default | Local library; proxy/ML modes optional |
| **Beyond compress()** | 77 MCP tools, session memory, code intelligence | Cross-agent memory, `headroom learn`, ML compression |

## The core difference

**Headroom** is a compression library first: `compress()` inline, a transparent
proxy for zero-code integration, an MCP server, and an optional ML compressor
("Kompress", requires `torch`). Its reach comes from a broad set of framework
wrappers (LiteLLM, LangChain, Agno, Strands) and agent-wrap commands.

**lean-ctx** is a context-engineering *layer*. The same `compress()` contract is
one surface of a Rust daemon that also does cached file reads (10 modes), 95+
shell-output compression patterns, hybrid semantic search, call-graph/impact
analysis, and a temporal knowledge graph — all 100% local, behind 29 published
stability contracts. Its `/v1/compress` is **deterministic by contract**: the
same `(messages, model)` produces byte-identical output, so Anthropic (90%) and
OpenAI (50%) prompt-cache discounts survive compression.

## Feature comparison

| Feature | lean-ctx | Headroom |
|---------|:--------:|:--------:|
| Drop-in `compress()` (Py + TS) | Yes | Yes |
| Transparent proxy | Yes (multi-provider) | Yes |
| MCP server | 77 tools | `headroom_compress/retrieve/stats` |
| Reversible (reference retrieval) | `/v1/references/{id}` | CCR store |
| Deterministic / prompt-cache safe | Yes (#498, CI-guarded) | Not stated |
| Vercel AI SDK middleware | `leanCtxMiddleware` / `withLeanCtx` | `headroomMiddleware` / `withHeadroom` |
| LiteLLM hook | `LeanCtxLiteLLMHandler` | `HeadroomCallback` |
| LangChain | `compress_messages` + retriever | wrap model |
| ML / learned compression | No (deterministic by design) | Yes (Kompress, torch) |
| Cross-agent shared memory | Knowledge graph + handoff | `SharedContext` |
| File-read compression | 10 modes, ~13-token cached re-reads | — |
| Shell-output compression | 95+ patterns | — |
| Semantic search / call graph | Hybrid BM25+vector / multi-hop | — |
| Single binary | Rust | Python / Node |

## Compression: deterministic funnel vs ML

lean-ctx's `/v1/compress` runs every text payload through a deterministic funnel
(dedup, structural prose squeeze, tool-output patterns). Because it is rule-based
it is **reproducible and cache-stable**, but it does not learn — highly novel
prose compresses modestly, while repetitive tool output, logs and RAG dumps
compress heavily (the same engine reaches up to ~99% on file reads and powers 95+
shell patterns).

Headroom additionally offers an **ML** compressor (Kompress) for prose, at the
cost of a `torch` dependency and non-deterministic output.

**Rule of thumb:** choose lean-ctx when the payload is code, tool output, logs or
RAG context and you need local, deterministic, cache-preserving output; consider
Headroom's ML mode when you specifically need learned prose compression.

## Benchmark (reproduce it yourself)

Numbers depend entirely on the corpus, so we ship a harness instead of cherry-
picked figures. It runs **both** libraries over the *same* files with the *same*
tokenizer and emits JSON (ratio + latency). A tool that is not installed is
reported `available: false` — never estimated.

```bash
# Optional head-to-head + accurate tokens:
pip install headroom-ai tiktoken
# lean-ctx side needs the daemon with /v1/compress:
lean-ctx dev-install

python bench/compress/benchmark.py --corpus docs/ --model gpt-4o --out report.json
```

A daemon-free lean-ctx data point (deterministic funnel, `o200k_base`) over this
repo's 27 `docs/reference/*.md` files, via
`cargo test -p lean-ctx --lib proxy::compress_api::tests::bench_real_corpus_o200k -- --ignored --nocapture`:

```json
{ "files": 27, "original_tokens": 69594, "compressed_tokens": 57615,
  "tokens_saved": 11979, "saved_pct": 17.2, "tokenizer": "o200k_base" }
```

Prose docs are a conservative corpus; tool-output / log payloads — the common
agent case — compress far more. See [`bench/compress/`](../../bench/compress/README.md).

## Where Headroom leads

- **Momentum & mindshare** — a fast-moving, popular library with broad adoption.
- **Learned compression** — the ML (Kompress) path can beat rule-based squeezing
  on free-form prose.
- **More framework wrappers out of the box** — Agno, Strands, agent-wrap commands.
- **Single-language install** — pure `pip install headroom-ai`, no separate daemon
  for the inline library path.

## Where lean-ctx leads

- **Determinism & prompt-cache safety** — byte-stable output is a CI-guarded
  contract (#498); compression never breaks Anthropic/OpenAI cache discounts.
- **It's a whole layer** — compression is 1 of 77 MCP tools alongside cached
  reads, shell compression, semantic search, code intelligence and memory.
- **100% local, single Rust binary** — no Python runtime, no telemetry by default.
- **Stability contracts** — 29 published contracts, frozen surfaces SHA-256-locked
  in CI; integrations can't silently break.

## When to use which

### Choose Headroom if you...
- Want a pure-Python (or Node) library with no separate daemon for the inline path
- Need learned/ML prose compression and accept a `torch` dependency
- Use Agno / Strands or want the widest set of prebuilt framework wrappers

### Choose lean-ctx if you...
- Need deterministic, prompt-cache-preserving compression
- Want compression *and* cached reads, shell compression, search, and memory
- Require 100% local operation in a single binary with stability guarantees
- Are compressing code, tool output, logs or RAG context

## Migration (Headroom → lean-ctx)

The contracts line up, so migration is mostly imports:

```python
# Headroom
from headroom import compress
result = compress(messages, model="gpt-4o")
messages = result.messages

# lean-ctx
from lean_ctx import ProxyClient
result = ProxyClient().compress(messages, model="gpt-4o")
messages = result.messages          # result.saved_tokens / result.saved_pct
```

```ts
// Headroom → lean-ctx (Vercel AI SDK)
- middleware: headroomMiddleware()
+ middleware: leanCtxMiddleware({ model: "gpt-4o" })
```

See the [compress() SDK cookbook](../guides/compress-sdk.md) for full recipes.

---

*Both projects are open source (Apache-2.0). Star counts and ML results move fast —
run the benchmark on your own corpus and choose what fits.*

[Get started with lean-ctx](https://leanctx.com/docs/getting-started) | [Headroom on GitHub](https://github.com/chopratejas/headroom)
