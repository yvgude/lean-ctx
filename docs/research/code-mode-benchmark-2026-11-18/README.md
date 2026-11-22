# Code Mode benchmark: native vs. Code Mode vs. Code Mode + LeanCTX (2026-11-18)

Claude Code's experimental Code Mode lets the model write a small script that
runs in a REPL instead of calling tools one by one. Intermediate results stay
in the REPL; only the result returns to the model. This benchmark measures how
much that saves, and what LeanCTX adds on top.

## Result

Cost tokens per accepted outcome (CPAO), Claude Sonnet 5.5, 5 tasks × 3 runs
per variant, 45 runs, all passed the answer check:

| Variant | CPAO | vs. native |
|---|---:|---:|
| Claude Code native | 38 885 | – |
| Code Mode | 26 394 | **−32 %** |
| **Code Mode + LeanCTX** | **23 603** | **−39 %** |

On the task that reads two large files (t5), weighted input goes from
76 401 (native) to 45 522 (Code Mode) to **33 486 tokens with LeanCTX: −56 %
against native and −26 % against Code Mode alone.** Every LeanCTX run of t5
was cheaper than every Code Mode run.

## Variants

| Variant | What runs |
|---|---|
| `native` | Claude Code without plugins, MCP servers or hooks |
| `code_mode` | the same with `CLAUDE_CODE_EXPERIMENTAL_CODE_MODE=1` |
| `code_mode_leanctx` | Code Mode plus the LeanCTX governor hooks ([`harness/leanctx-settings.json`](harness/leanctx-settings.json)) with R5: the first whole-file `Read` of a large code file returns its outline (symbols with line spans), so the model reads only the ranges it needs |

## How the numbers are calculated

Claude Code writes the token usage of every model call into the session
transcript (`~/.claude/projects/<project>/<session>.jsonl`, field
`message.usage`). Per run, [`harness/summarize.py`](harness/summarize.py) sums
it once per model message and weights it with Anthropic's price ratios:

```text
weighted_input = input + 1.25 × cache_write + 0.1 × cache_read
cost_tokens    = weighted_input + 5 × output

CPAO (per variant) = Σ cost_tokens over all runs / accepted outcomes
```

| Field | Meaning |
|---|---|
| `input` | new, uncached input tokens |
| `cache_write` | tokens written to the prompt cache: system prompt, tool list, new tool results |
| `cache_read` | tokens read from the cache on every later step; a large file in context is paid again at every step |
| `output` | generated tokens, priced at 5 × input |
| CPAO | cost per accepted outcome: a run counts only when its answer passes the check, so a cheap wrong answer does not improve the number |

Worked example (t5, native, run 2):

```text
input 12 · cache_write 38 694 · cache_read 280 212 · output 1 649

weighted_input = 12 + 1.25 × 38 694 + 0.1 × 280 212 = 76 400.7
cost_tokens    = 76 400.7 + 5 × 1 649              = 84 645.7
```

The same task with Code Mode + LeanCTX (run 3) has a weighted input of
33 485.6; its tool results shrink from 69 135 to 560 characters.

## Per task (weighted input, median of 3 runs)

| Task | native | Code Mode | + LeanCTX | vs. native | vs. Code Mode |
|---|---:|---:|---:|---:|---:|
| t1 find a function | 31 380 | 17 755 | 16 648 | −47 % | −6 % |
| t2 shell questions | 15 412 | 11 269 | 11 289 | −27 % | ±0 % |
| t3 explain a TS module | 26 779 | 21 442 | 19 956 | −25 % | −7 % |
| t4 crate overview | 20 929 | 16 175 | 18 527 | −11 % | +15 % |
| t5 review, large files | 76 401 | 45 522 | 33 486 | **−56 %** | **−26 %** |
| Sum | 170 900 | 112 162 | 99 905 | −42 % | −11 % |

All 45 runs: [`results/runs.csv`](results/runs.csv).

## Test harness

1. **A real session, not an API call.** [`harness/drive.exp`](harness/drive.exp)
   starts one interactive Claude Code session per run with a fresh session id,
   types the task prompt, waits for the line `ANSWER=…`, then sends `/exit`.
   What is measured is the product as developers use it.
2. **Isolated variants.** Every variant runs with
   `--setting-sources project,local --strict-mcp-config` and an empty MCP
   list, so no variant inherits plugins or hooks from the user's
   configuration. `ANTHROPIC_BASE_URL` is unset. LeanCTX enters only through
   `--settings leanctx-settings.json`.
3. **Five tasks with checkable answers** ([`harness/tasks/`](harness/tasks)):
   find and explain a function, two shell questions, explain a TypeScript
   module, a crate overview, and a multi-step review across two large files
   with counts. Each answer ends with one `ANSWER=` line.
4. **Warm-up and alternating order.** [`harness/run.sh`](harness/run.sh) runs
   one unrecorded session per variant first, so no measured run pays the cold
   prompt-cache write for system prompt and tool list (without it, one run
   wrote 26k instead of 7k cache tokens). Variant order alternates between
   forward and reverse across repetitions and tasks.
5. **Evaluation from the transcript.** `summarize.py` reads each session's
   JSONL, counts every model message once (streaming writes a message several
   times), sums the usage fields and checks the answer.

### Running it

Requirements: Claude Code, `expect`, Python 3, and for the LeanCTX variant
`lean-ctx` 3.12.7 or later on `PATH`.

```bash
cd docs/research/code-mode-benchmark-2026-11-18/harness
BENCH_REPO=/path/to/a/lean-ctx/checkout ./run.sh                 # 3 variants × 5 tasks × 3 runs
BENCH_REPO=... ./run.sh "native code_mode" "t5" 5                # a subset
python3 summarize.py                                             # tables
python3 summarize.py --csv > my-runs.csv                         # one row per run
```

Runs bill your Anthropic account (the full set is 45 sessions plus 3 warm-ups).

### Answer check

t1 and t3 have stable answers. t2 and t5 count workspace members, commits and
tests, so their expected values in `summarize.py` (`20,17` and `62,5,5`) hold
for the tree the benchmark ran on, a pre-release `lean-ctx` checkout from
2026-11-18. On another tree, recompute them before you run. t4 is an
open-ended overview; the check only verifies that a whole number came back.

## Limits

- Small sample: 5 tasks, one repository, one model, 3 repetitions. Native runs
  vary 0–4 % per task, Code Mode and LeanCTX runs up to about 30 %.
- On the small tasks t1–t4 the difference between Code Mode and
  Code Mode + LeanCTX is not resolved by this sample; on t4 LeanCTX costs 15 %
  more. The gain comes from tasks that read large files.
- Time does not improve: mean wall time per run was 21 s native, 19 s
  Code Mode, 20 s Code Mode + LeanCTX.
- Only tokens and the answer line are measured, not answer quality beyond it.
- The LeanCTX runs used a development build with R5, which ships in 3.12.7.
  The same session also ran LeanCTX without Code Mode: −15 % CPAO against
  native.
- Code Mode is experimental and may change with any Claude Code release.
  Measured with Claude Code 2.1.327.
- Proxy compression is not measured, because Code Mode refuses a custom
  `ANTHROPIC_BASE_URL`.
