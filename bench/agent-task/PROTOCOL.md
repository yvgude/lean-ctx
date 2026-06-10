# Agent-Task-Benchmark v1 — Pre-Registered Protocol (GL #493)

Status: **FROZEN at commit time.** Changes after the first recorded run require a
numbered amendment (`A1`, `A2`, …) appended under *Amendments* — the original
text is never edited. This mirrors the standard set by the external tokbench
study (GH #361): protocol first, runs second, publication third.

## 1. Question

Does lean-ctx change the **outcome** of an agentic coding workload — task
success rate and cost per solved task — compared to the identical agent
without lean-ctx?

This deliberately measures outcome, not token arithmetic. Token deltas are
reported, but the primary endpoints are:

- **resolved rate** (per the official SWE-bench evaluation harness), and
- **cost per resolved task** (billed USD / resolved count).

## 2. Workload

- **Dataset:** `princeton-nlp/SWE-bench_Verified` (test split), the
  human-validated 500-instance subset.
- **Subset size:** N = 15 instances.
- **Selection rule (deterministic, no cherry-picking):** sort all instances by
  `instance_id` ascending; group by `repo`; visit repos in ascending name
  order round-robin, taking the lexicographically first untaken instance from
  each repo per cycle, until N instances are selected. The result is committed
  as `tasks.lock.json` (content-hashed into the result artifact). The lock is
  generated once by `select_tasks.py` and never regenerated for v1.
- Instances are independent; each run starts from a clean checkout of
  `base_commit`.

## 3. Arms

Two arms, identical in every respect except lean-ctx:

| | `native` | `leanctx` |
|---|---|---|
| Agent | Claude Code headless (`claude -p`), pinned version recorded in `meta.json` | identical |
| Prompt | `PROMPT.md` template, identical text | identical |
| MCP config | none (`--strict-mcp-config` with empty config) | exactly the registration `lean-ctx init --agent claude` writes, extracted into an explicit config and pinned via `--strict-mcp-config`; missing registration aborts the run; lean-ctx version recorded |
| Rules file | none | `CLAUDE.md`/rules as written by `lean-ctx init` (stock; no hand-tuning) |
| HOME | fresh per-run temp HOME (no user-level config bleed) | identical |
| Max turns | 40 | identical |
| Model | the agent runtime's pinned default model, recorded per run | identical |

The fresh-HOME isolation exists because the operator's real machine has
lean-ctx globally installed; without it the `native` arm would be
contaminated.

## 4. Measurement

- **Patch:** after the agent exits, `git add -A && git diff --cached` in the
  task workspace is the submitted `model_patch`.
- **Resolution:** official `swebench.harness.run_evaluation` (dockerized) on
  the per-arm `predictions.jsonl`. An instance counts as resolved iff the
  official report lists it as resolved. No manual judging.
- **Tokens & cost:** taken from the agent runtime's own final usage report
  per run (stream-json `result` event: input/output/cache tokens,
  `total_cost_usd`). We do not estimate; if the runtime reports no usage the
  run is marked `usage_missing` and excluded from cost endpoints (counted in
  resolution endpoints).
- **Wall time:** harness-measured per run.

## 5. Endpoints

Primary:
1. resolved-rate per arm (resolved / N),
2. cost per resolved task per arm (sum billed USD / resolved).

Secondary: billed input tokens per run (median), output tokens, wall time,
turns used, lean-ctx tool-call adoption in the `leanctx` arm (from
transcripts).

## 6. Honesty constraints

- Both arms run from the same task lock, same prompt, same limits.
- All transcripts (`transcript.jsonl`), patches and the evaluation report are
  retained as raw artifacts and published alongside the result.
- Negative or neutral results are published unchanged.
- The result artifact embeds the SHA-256 of this protocol and of
  `tasks.lock.json`; `report.py` emits the artifact digest for signing
  (`ssh-keygen -Y sign`) so third parties can verify nothing moved after the
  fact.
- Known limitation, stated up front: N=15 with one seed is a pilot-grade
  sample — confidence intervals are wide; the claim is directional, not a
  leaderboard. Provider-side prompt caching is active for both arms equally
  (it is part of the product reality being measured).

## 7. Amendments

*(none yet)*
