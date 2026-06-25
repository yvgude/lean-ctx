# compress() Benchmark — lean-ctx vs Headroom

A head-to-head compression benchmark for the drop-in `compress(messages, model)`
contract. It runs **both** libraries over the *same* real corpus with the *same*
tokenizer and emits JSON (compression ratio + latency).

Numbers are always **measured, never fabricated**: a tool that is not installed
or whose daemon is unreachable is reported as `available: false` (with an install
hint) instead of being estimated.

## Prerequisites

| For | Install |
|-----|---------|
| lean-ctx side | a daemon serving `POST /v1/compress` — `lean-ctx dev-install` |
| Headroom side (optional) | `pip install headroom-ai` |
| Accurate token counts (optional) | `pip install tiktoken` (else character counts) |

The lean-ctx Python SDK is imported directly from `packages/python-lean-ctx`
(no `pip install` required).

## Run

```bash
# Default corpus = docs/reference/*.md, model = gpt-4o
python bench/compress/benchmark.py

# Custom corpus, write the JSON log to a file
python bench/compress/benchmark.py --corpus docs/ --model gpt-4o --out report.json

# Bound the corpus size
python bench/compress/benchmark.py --max-files 50 --max-bytes 200000
```

The corpus is built from **real on-disk files** (`.md .rs .py .ts .txt .json .log
.yaml .yml`) under `--corpus`; one `user` message per file. No fixtures, no mock
payloads.

## Output

```json
{
  "corpus":   { "path": "docs/reference", "messages": 27, "model": "gpt-4o", "tokenizer": "o200k_base" },
  "lean_ctx": { "available": true, "original_tokens": 69594, "compressed_tokens": 57615,
                "tokens_saved": 11979, "ratio": 0.172, "latency_ms": 1014.03 },
  "headroom": { "available": false, "install": "pip install headroom-ai" }
}
```

`ratio = 1 − compressed/original`, computed with the shared tokenizer for both
tools so the comparison is apples-to-apples.

## Daemon-free lean-ctx benchmark

To benchmark the deterministic funnel without a running daemon (calls the Rust
`compress_messages` directly, `o200k_base`):

```bash
cargo test -p lean-ctx --lib \
  proxy::compress_api::tests::bench_real_corpus_o200k -- --ignored --nocapture
```

## Extractive vs truncation (prose quality)

Prose is the one place a rule-based funnel can only truncate. This benchmark runs
the **extractive** ranker (centrality, reusing the shipped all-MiniLM model)
head-to-head against FIFO **truncation** at an identical 50% char budget over the
real `docs/reference` corpus, and reports both token savings AND a coverage
quality per method:

```bash
cargo test -p lean-ctx --lib --features embeddings \
  core::extractive::tests::bench_extractive_vs_truncation -- --ignored --nocapture
```

The report has two honest signals:

- **`avg_coverage`** (query-free): mean, over every full-document sentence, of its
  cosine to the **nearest kept sentence** — `1.0` means the kept set has a close
  match for every original sentence. Coverage is the fair extractive-quality proxy:
  a centroid cosine would *reward redundancy* (a contiguous prefix tracks the
  whole-doc centroid while MMR de-duplication deliberately diversifies away from
  it), whereas coverage rewards spreading the budget across the whole document.
- **`rag_query_recall`** (query-aware): a real sentence from each document's *back
  half* is used as the query; recall is the cosine of that query to its nearest
  kept sentence. `1.0` means the answer survived compression. This is the
  documented highest-value path (research / tool-result prose), and the place
  prefix truncation fails structurally — it drops the back half wholesale.

The model must already be present (`available: false` is reported honestly when it
is not).

> Honest caveat: `docs/reference` is structured reference material whose first half
> is already representative, so on the **query-free `avg_coverage`** signal,
> prefix truncation is a *strong* baseline (often ahead) — we report that as
> measured rather than spin it. Extractive's structural win shows up in
> **`rag_query_recall`**: when the answer is not in the prefix, truncation cannot
> recover it and query-aware extraction can. Point `--corpus` at long,
> non-front-loaded prose / RAG dumps to widen the coverage gap too.

## Notes

- Prose/markdown is a **conservative** corpus for the rule-based funnel; tool
  output, logs and RAG dumps (the common agent case) compress far more.
- lean-ctx output is deterministic and prompt-cache safe (#498); the benchmark
  re-running with identical input yields identical `lean_ctx` figures.

See the full positioning in [docs/comparisons/vs-headroom.md](../../docs/comparisons/vs-headroom.md)
and the recipes in [docs/guides/compress-sdk.md](../../docs/guides/compress-sdk.md).
