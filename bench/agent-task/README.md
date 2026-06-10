# Agent-Task-Benchmark v1 (GL #493)

Outcome evidence, not token arithmetic: **does lean-ctx change task success
rate and cost per solved task** for an agentic coding workload? Two identical
arms (Claude Code headless, native vs. lean-ctx MCP) over a deterministic
SWE-bench-Verified subset, judged by the official SWE-bench evaluation.

The methodology is pre-registered in [`PROTOCOL.md`](PROTOCOL.md) — read it
first; it is frozen, and changes require numbered amendments. Negative or
neutral results are published unchanged.

## Prerequisites

- Python ≥ 3.9, `pip install -r requirements.txt`
- `claude` (Claude Code CLI) on PATH, `ANTHROPIC_API_KEY` exported
- `lean-ctx` on PATH (pinned release; version is recorded per run)
- Docker (only for the evaluation step)
- Disk: repo mirrors (~2 GB) + SWE-bench evaluation images

> Safety: agent runs execute model-chosen shell commands with permissions
> skipped (standard for SWE-bench harnesses). Run on a disposable machine or
> container, not on a workstation with credentials you care about.

## Runbook

```bash
cd bench/agent-task

# 0. One-time: freeze the task set (writes tasks.lock.json; never re-run for v1)
python3 -m swebench_harness.select_tasks

# 1. Smoke (1 instance, both arms) — validates plumbing end to end
python3 -m swebench_harness.run_arm --arm native  --run-id smoke --instance <id-from-lock>
python3 -m swebench_harness.run_arm --arm leanctx --run-id smoke --instance <id-from-lock>

# 2. Full run (N=15 × 2 arms; resumable — finished runs are skipped)
python3 -m swebench_harness.run_arm --arm native  --run-id v1
python3 -m swebench_harness.run_arm --arm leanctx --run-id v1

# 3. Predictions + official evaluation (docker)
python3 -m swebench_harness.collect --run-id v1
python3 -m swebench.harness.run_evaluation \
  --dataset_name princeton-nlp/SWE-bench_Verified \
  --predictions_path runs/v1/predictions-native.jsonl  --run_id v1-native  --max_workers 4
python3 -m swebench.harness.run_evaluation \
  --dataset_name princeton-nlp/SWE-bench_Verified \
  --predictions_path runs/v1/predictions-leanctx.jsonl --run_id v1-leanctx --max_workers 4

# 4. Endpoints + verifiable artifact (+ optional signature)
python3 -m swebench_harness.report --run-id v1 \
  --eval-report native=claude-code-native.v1-native.json \
  --eval-report leanctx=claude-code-leanctx.v1-leanctx.json
```

## What gets recorded

| Artifact | Contents |
|---|---|
| `runs/<id>/<instance>/<arm>/transcript.jsonl` | full agent stream (turns, tool calls, usage) |
| `…/model_patch.diff` | the submitted patch (`git add -A && git diff --cached`) |
| `…/meta.json` | exit code, wall time, billed tokens, cost, versions, flags |
| `runs/<id>/predictions-<arm>.jsonl` | official SWE-bench prediction format |
| `runs/<id>/result-v1.json` | canonical result artifact, self-hashing, embeds protocol + lock SHA-256 |
| `runs/<id>/result-v1.md` | human-readable endpoint table |

## Design notes

- **Fresh HOME per run** — the operator's machine has lean-ctx globally
  installed; without isolation the native arm would be contaminated.
- **`--strict-mcp-config`** pins the MCP surface per arm explicitly: empty for
  native, exactly the `lean-ctx init --agent claude` output for leanctx.
- **Usage comes from the runtime's own final report** (stream-json `result`
  event) — no token estimation anywhere.
- **Selection is rule-based, not random** (sorted round-robin by repo), so the
  subset is reproducible from the public dataset without a seed argument.
