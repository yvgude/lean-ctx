# Context-engine benchmark

A real, runnable head-to-head harness that compares **hermes-lean-ctx** against
Hermes' built-in `ContextCompressor` and [hermes-lcm](https://github.com/stephenschoettler/hermes-lcm)
on a long, needle-laden transcript.

This is the **release gate** for the plugin (plan Phase 4): we do not claim
"better" — we measure it.

## What it measures

For each engine's `compress()` over the same input window:

| Metric | Meaning |
|---|---|
| `savings_pct` / `saved_tokens` | Token reduction of the compacted window |
| `verbatim_recall` | Fraction of *needle facts* whose exact text survives **in the window** |
| `compress_latency_ms` | Wall-clock cost of one `compress()` call (averaged over `--repeats`) |
| `output_messages` | Message count after compaction |

### Reading `verbatim_recall` honestly

`verbatim_recall` only credits facts that remain **literally in the returned
window**. It deliberately does **not** credit lean-ctx's recoverability: facts
that get summarized out are still retrievable on demand via the injected recall
tools (`ctx_search`, `ctx_semantic_search`, `ctx_expand`, `ctx_read`,
`ctx_knowledge`) and are offloaded into durable session memory.

So a lower verbatim-recall with high token-savings is *expected and fair* for a
faithful-but-recoverable engine — the metric is intentionally conservative
toward lean-ctx. The recoverability itself is proven by the live integration
tests (`tests/test_live_daemon.py`), not by this in-window metric.

## Corpus

`corpus.py` builds a **deterministic** synthetic transcript (SHA-256 derived
pseudo-text, no randomness) with unique `NEEDLE-NNN` facts planted in early
turns — exactly the region a compactor must summarize. Same parameters →
byte-identical corpus, so numbers are reproducible.

This is benchmark *input data*, not mocked application data. For a real dataset,
pass a transcript JSON (a list of messages or `{"messages": [...]}`) via
`--transcript` (e.g. a LOCOMO conversation export).

## Running

```bash
# lean-ctx only, offline (exercises the deterministic local-fallback compaction):
python benchmarks/run.py --turns 200 --needles 12 --context-length 8000 \
    --base-url http://127.0.0.1:9

# Against a live daemon (exercises the daemon's ctx_transcript_compact core tool):
lean-ctx serve --host 127.0.0.1 --port 8080 --auth-token test-token &
LEANCTX_TOKEN=test-token python benchmarks/run.py --base-url http://127.0.0.1:8080

# On a real transcript:
python benchmarks/run.py --transcript /path/to/conversation.json
```

Results are written to `benchmarks/results/latest.json` (gitignored); pass
`--no-write` to skip.

## Including the competitors

The competitor engines are **auto-detected and import-guarded** — when a package
is missing they are reported as `(skipped …: <reason>)`, never faked. To include
them, make them importable in the same environment:

- **Hermes built-in `ContextCompressor`** — install / check out Hermes Agent so
  `agent.context_compressor.ContextCompressor` (or equivalent) imports.
- **hermes-lcm** — `pip install hermes-lcm` (or check it out) so `hermes_lcm`
  imports.

The adapters in `engines.py` try the documented constructor shapes and a
`compress(messages, current_tokens)` call; extend the candidate lists there if a
version exposes a different entry point.

## Layout

| File | Role |
|---|---|
| `corpus.py` | Deterministic corpus + needle facts; `load_transcript` for real data |
| `metrics.py` | Token savings, latency, verbatim recall |
| `engines.py` | lean-ctx adapter + import-guarded competitor adapters |
| `run.py` | CLI runner + comparison table + results JSON |

Covered by `tests/test_benchmark.py` (runs fully offline in CI).
