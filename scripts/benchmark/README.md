# Token Reduction Benchmark

Captured files under `results/` are historical workload evidence and may
contain the product wording at capture time. They are not current onboarding
or general savings claims. For the product definition, see
[canonical positioning](../../docs/POSITIONING_CANONICAL.md).

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

## Agent release suite and gate

`agent-suite/suite-v1.json` fixes ten real tasks (lean-ctx and the app repo)
that each listed agent (Claude, and Codex over ACP) runs with the app attach
profile. `agent_suite.py` scores the recorded runs:

| Metric | Definition | Target |
|---|---|---|
| `step_share.claude` / `step_share.acp` | mean per run of `ctx_*` calls / (`ctx_*` + native file, search and shell calls); edits and planning tools are outside both | 80 % / 60 % |
| `compose_first` | runs with `ctx_compose` before the first file read | 70 % |
| `tokens_saved_median` | median per run of `tokens.saved / tokens.input` from `lean-ctx session stats` | 40 % |

The release job `agent-bench-gate` (`.github/workflows/release.yml`) runs
`agent_suite.py gate` on the tagged tree on every release. Its mode is decided
by the presence of `agent-suite/baseline.json`:

- **Report only** (no baseline committed yet): the gate prints
  `no baseline recorded yet — report only`, scores `agent-suite/traces.json` if
  it exists (invalid or missing traces are notes), writes the report with
  `"result": "REPORT_ONLY"` and exits 0. The release is not blocked.
- **Blocking** (baseline committed): it refuses when the baseline or
  `agent-suite/traces.json` is missing or invalid, when a run failed its task,
  or when any metric dropped more than `regression_limit_pp` (5) percentage
  points below the baseline. Anything at the baseline path, even an unreadable
  file, selects this mode.

Targets are reported; they block only once `targets_block` is set in the suite.
The gate is a pure function of the three files: no agent runs in CI. The release
job does not require the traces to match the tag's Engine version; the Engine
version recorded in the traces and baseline is reported. For a manual check
against one version, pass `gate --engine-version <X.Y.Z>`, which then refuses
traces of any other version.

Traces are content-free: per run only the task id, agent id, outcome, the two
token counters and the ordered tool names. A step must be a bare tool name (no
slash, dot or space), so no path, argument or file content fits.

The recorder counts the parent agent's own calls only. From a Claude transcript
it takes the `tool_use` blocks of `assistant` messages; subagent lines
(`isSidechain`) and progress entries are skipped, and the parent's `Task`/`Agent`
call is neutral. From a Codex rollout it takes `function_call` and
`custom_tool_call` by name and counts each `local_shell_call` as a native step.
LeanCTX tools are recognised as `mcp__<server>__ctx_*` and as Codex's
`<server>__ctx_*`; any other MCP tool counts as a native step. Transcripts over
64 MiB are refused.

### Recording traces (manual, with the owner)

Recording uses real agent accounts and is never run by CI; it is done with the
owner, not per release. For each task and agent, on the chosen Engine binary:

1. Start a fresh app session of the agent in the task's repository with the app
   attach profile, give it the task's `prompt` verbatim, and judge the result
   against `done_when` (`pass` or `fail`).
2. Export the session counters: `lean-ctx session stats --json --session <id> > stats.json`.
3. Append the run (re-recording a run replaces it; the task and agent must be
   in the suite, and an existing bundle must be valid and of the same Engine
   version):

```bash
python3 scripts/benchmark/agent_suite.py record --task <task> --agent claude \
  --outcome pass --engine-version <X.Y.Z> --stats stats.json \
  --transcript ~/.claude/projects/<project>/<session>.jsonl
# Codex: --transcript <rollout>.jsonl; other ACP agents: --tools tools.txt (one tool name per line)
```

4. Check the bundle with `python3 scripts/benchmark/agent_suite.py score`, then
   commit `agent-suite/traces.json` (and add it to `new_paths` in
   `PUBLISH_MANIFEST.toml` the first time).

To accept a new baseline (only from a bundle where every run passed, with the
owner's approval), run `agent_suite.py baseline` and commit
`agent-suite/baseline.json` together with the complete `traces.json` (and add
both to `new_paths` in `PUBLISH_MANIFEST.toml` the first time). Committing the
first baseline switches the release gate from report-only to blocking; from then
on a release without valid traces is refused. Removing the baseline turns the
gate back to report-only, so that is a reviewed owner decision, never a fix for
a red gate. The scorer contract is tested by `scripts/tests/test_agent_suite.py`,
including a seeded 10-point regression that the gate must refuse and both modes.
