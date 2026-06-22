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

## Notes

- Prose/markdown is a **conservative** corpus for the rule-based funnel; tool
  output, logs and RAG dumps (the common agent case) compress far more.
- lean-ctx output is deterministic and prompt-cache safe (#498); the benchmark
  re-running with identical input yields identical `lean_ctx` figures.

See the full positioning in [docs/comparisons/vs-headroom.md](../../docs/comparisons/vs-headroom.md)
and the recipes in [docs/guides/compress-sdk.md](../../docs/guides/compress-sdk.md).
