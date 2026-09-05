# Token Reduction Benchmark

From the repository root, run:

```bash
scripts/benchmark/token_reduction.sh
```

The script runs ten fixed repository operations once without LeanCTX and once
with LeanCTX. Raw operations use `LEAN_CTX_DISABLED=1` plus native `cat`,
`rg`, `find`, or shell commands; compressed operations use the matching
`lean-ctx` CLI command. Captured outputs are stored under `results/latest/`
with summary data in `results/token_reduction.tsv` and `results/report.md`.

Generate the Markdown report again with:

```bash
scripts/benchmark/report.sh
```

“Tokens” are a deliberately conservative approximation: output characters
divided by four (`chars / 4`, integer-truncated). This measures emitted input
size, not model-specific tokenizer output. Reduction is calculated from the
per-task token estimates; the report's final row is the arithmetic mean across
the ten tasks.

## Session-store lookup benchmark

From the repository root, build the release binary and create a temporary store with
one indexed project session plus 10,000 unrelated sessions:

```bash
cd rust && cargo build --release
cd .. && scripts/benchmark/session-store.sh 10000
```

The script measures warmed `lean-ctx -c /usr/bin/true` invocations and prints p50 in
milliseconds. It uses temporary directories only and removes them on exit. Set
`LEAN_CTX_BIN`, `RUNS`, or `WARMUP` to select a binary or adjust sampling.
