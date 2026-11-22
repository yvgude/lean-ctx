#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Release benchmark suite for agents: score recorded traces, gate the release.

A fixed suite of real tasks (`agent-suite/suite-v1.json`) is run by each listed
agent with the app attach profile. Every run is recorded as a content-free trace:
the ordered tool *names* the agent called, LeanCTX's token counters of the
session (`lean-ctx session stats --json`) and the task outcome. Nothing else:
no arguments, no file contents, no paths. The scorer below is a pure function
of the suite, the traces and the stored baseline, so the release gate is
deterministic and costs no agent run.

  record   append one run to a trace bundle (manual, after a real agent run)
  score    print the metrics of a trace bundle
  baseline write a baseline from an accepted trace bundle
  gate     refuse the release on missing or invalid input, any failed task, or
           a drop of more than `regression_limit_pp` percentage points of any
           metric against the baseline

The gate fails closed once a baseline is committed: a missing suite or trace
bundle, or an invalid baseline, refuses. Until `agent-suite/baseline.json`
exists the gate runs in report-only mode: it scores whatever traces exist,
prints "no baseline recorded yet — report only" and exits 0. The mode is
decided by the presence of the baseline path alone, so committing the first
baseline is what turns the gate blocking.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import statistics
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent / "agent-suite"
DEFAULT_SUITE = HERE / "suite-v1.json"
DEFAULT_BASELINE = HERE / "baseline.json"
DEFAULT_TRACES = HERE / "traces.json"

SUITE_SCHEMA = "leanctx.agent-suite/v1"
TRACES_SCHEMA = "leanctx.agent-suite-traces/v1"
BASELINE_SCHEMA = "leanctx.agent-suite-baseline/v1"
REPORT_SCHEMA = "leanctx.agent-suite-gate/v1"

CLASSES = ("claude", "acp")
METRICS = ("step_share.claude", "step_share.acp", "compose_first", "tokens_saved_median")
OUTCOMES = ("pass", "fail")
# A step is a bare tool name: no slash, dot or space, so no path or argument fits.
TOOL_NAME = re.compile(r"[A-Za-z][A-Za-z0-9_]{0,63}")
ID = re.compile(r"[a-z0-9][a-z0-9.-]{0,63}")

# LeanCTX's own edit tools are edits, not context steps.
LC_EDITS = frozenset({"ctx_patch", "ctx_edit"})
NATIVE_EDITS = frozenset({"Edit", "MultiEdit", "Write", "NotebookEdit", "apply_patch", "edit_file",
                          "write_file", "fs_write_text_file", "str_replace", "create_file"})
# Neither context nor edit: planning, delegation, web. Any other non-ctx tool
# counts as a native context step, so an unknown name can only lower the share.
NEUTRAL = frozenset({"Task", "Agent", "TodoWrite", "TodoRead", "WebFetch", "WebSearch", "ToolSearch",
                     "Skill", "ExitPlanMode", "update_plan", "think", "SubagentHandback"})
FILE_READS = frozenset({"Read", "NotebookRead", "read_file", "fs_read_text_file", "view", "cat",
                        "ctx_read", "ctx_multi_read"})
COMPOSE = "ctx_compose"
# Codex rollout items without a tool name of their own: built-in shell runs.
LOCAL_SHELL = "local_shell_call"
# The recorder reads local files only; these bound what one manual run may load.
MAX_TRANSCRIPT_BYTES = 64 * 1024 * 1024
MAX_STATS_BYTES = 1024 * 1024


class Refusal(Exception):
    pass


def normalize_tool(raw: str) -> str:
    """Bare tool name of one call.

    `mcp__lean-ctx__ctx_read` (Claude, newer Codex) and `lean-ctx__ctx_read`
    (older Codex) → `ctx_read`; `fs/read_text_file` → `fs_read_text_file`.
    """
    name = raw.strip()
    if name.startswith("mcp__") and name.count("__") >= 2:
        name = name.split("__", 2)[2]
    elif "__" in name and name.rsplit("__", 1)[1].startswith("ctx_"):
        # Codex `<server>__<tool>`: only LeanCTX tools are unwrapped, so any
        # other server's tool keeps counting as a native step.
        name = name.rsplit("__", 1)[1]
    # Dots stay invalid, so a file name can never pass as a tool name.
    name = re.sub(r"[/\-]", "_", name)
    if not TOOL_NAME.fullmatch(name):
        raise Refusal(f"not a bare tool name: {raw[:40]!r}")
    return name


def _load(path: Path, what: str) -> dict:
    if not path.is_file():
        raise Refusal(f"{what} is missing: {path.name}")
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError, RecursionError) as error:
        raise Refusal(f"{what} is not readable JSON: {error}") from error
    if not isinstance(data, dict):
        raise Refusal(f"{what} must be a JSON object")
    return data


def _keys(obj: object, expected: set[str], what: str) -> dict:
    if not isinstance(obj, dict) or set(obj) != expected:
        found = sorted(obj) if isinstance(obj, dict) else type(obj).__name__
        raise Refusal(f"{what}: keys must be exactly {sorted(expected)}, found {found}")
    return obj


def _percent(value: object, what: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not 0 <= value <= 100:
        raise Refusal(f"{what} must be a percentage between 0 and 100")
    return float(value)


def load_suite(path: Path) -> dict:
    suite = _keys(_load(path, "suite"), {"schema", "id", "regression_limit_pp", "targets",
                                         "targets_block", "agents", "tasks"}, "suite")
    if suite["schema"] != SUITE_SCHEMA or not ID.fullmatch(str(suite["id"])):
        raise Refusal("suite: unknown schema or id")
    _percent(suite["regression_limit_pp"], "suite: regression_limit_pp")
    targets = _keys(suite["targets"], set(METRICS), "suite targets")
    for metric in METRICS:
        _percent(targets[metric], f"suite target {metric}")
    if not isinstance(suite["targets_block"], bool):
        raise Refusal("suite: targets_block must be true or false")
    agents = suite["agents"]
    if not isinstance(agents, list) or not agents:
        raise Refusal("suite: agents must be a non-empty list")
    for agent in agents:
        _keys(agent, {"id", "class"}, "suite agent")
        if not ID.fullmatch(str(agent["id"])) or agent["class"] not in CLASSES:
            raise Refusal(f"suite agent: bad id or class {agent!r}")
    if {a["class"] for a in agents} != set(CLASSES):
        raise Refusal("suite: needs at least one claude and one acp agent")
    tasks = suite["tasks"]
    if not isinstance(tasks, list) or not tasks:
        raise Refusal("suite: tasks must be a non-empty list")
    for task in tasks:
        _keys(task, {"id", "repo", "kind", "prompt", "done_when"}, "suite task")
        if not ID.fullmatch(str(task["id"])):
            raise Refusal(f"suite task: bad id {task['id']!r}")
    for name, rows in (("agent", agents), ("task", tasks)):
        ids = [row["id"] for row in rows]
        if len(ids) != len(set(ids)):
            raise Refusal(f"suite: duplicate {name} id")
    return suite


def _counter(value: object, what: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise Refusal(f"{what} must be a non-negative integer")
    return value


def load_traces(path: Path, suite: dict, complete: bool = True) -> dict:
    """Validated trace bundle; `complete=False` (recording) allows missing runs."""
    bundle = _keys(_load(path, "traces"), {"schema", "suite", "engine_version", "runs"}, "traces")
    if bundle["schema"] != TRACES_SCHEMA or bundle["suite"] != suite["id"]:
        raise Refusal("traces: unknown schema or recorded for another suite")
    if not isinstance(bundle["engine_version"], str) or not bundle["engine_version"]:
        raise Refusal("traces: engine_version is required")
    if not isinstance(bundle["runs"], list):
        raise Refusal("traces: runs must be a list")
    agents = {a["id"]: a["class"] for a in suite["agents"]}
    expected = {(t["id"], a) for t in suite["tasks"] for a in agents}
    seen: set[tuple[str, str]] = set()
    for run in bundle["runs"]:
        _keys(run, {"task", "agent", "outcome", "tokens", "steps"}, "trace run")
        key = (run["task"], run["agent"])
        if key not in expected:
            raise Refusal(f"traces: run {key} is not in the suite")
        if key in seen:
            raise Refusal(f"traces: run {key} recorded twice")
        seen.add(key)
        if run["outcome"] not in OUTCOMES:
            raise Refusal(f"traces: run {key} outcome must be pass or fail")
        tokens = _keys(run["tokens"], {"input", "saved"}, f"trace run {key} tokens")
        if _counter(tokens["saved"], "saved") > _counter(tokens["input"], "input"):
            raise Refusal(f"traces: run {key} saved more tokens than its input")
        if not isinstance(run["steps"], list) or not all(isinstance(s, str) for s in run["steps"]):
            raise Refusal(f"traces: run {key} steps must be a list of tool names")
        for step in run["steps"]:
            if not TOOL_NAME.fullmatch(step):
                raise Refusal(f"traces: run {key} has a step that is not a bare tool name")
    missing = sorted(expected - seen)
    if missing and complete:
        raise Refusal(f"traces: {len(missing)} run(s) missing, first {missing[0]}")
    return bundle


def load_baseline(path: Path, suite: dict) -> dict:
    baseline = _keys(_load(path, "baseline"), {"schema", "suite", "engine_version", "metrics"},
                     "baseline")
    if baseline["schema"] != BASELINE_SCHEMA or baseline["suite"] != suite["id"]:
        raise Refusal("baseline: unknown schema or stored for another suite")
    metrics = _keys(baseline["metrics"], set(METRICS), "baseline metrics")
    for metric in METRICS:
        _percent(metrics[metric], f"baseline {metric}")
    return baseline


def run_shape(steps: list[str]) -> tuple[float, bool]:
    """(LeanCTX step share in %, compose before the first file read) of one run."""
    lc = native = 0
    first_read = first_compose = None
    for index, step in enumerate(steps):
        if step == COMPOSE and first_compose is None:
            first_compose = index
        if step in FILE_READS and first_read is None:
            first_read = index
        if step in LC_EDITS or step in NATIVE_EDITS or step in NEUTRAL:
            continue
        if step.startswith("ctx_"):
            lc += 1
        else:
            native += 1
    share = 100.0 * lc / (lc + native) if lc + native else 0.0
    composed_first = first_compose is not None and (first_read is None or first_compose < first_read)
    return share, composed_first


def score(suite: dict, bundle: dict) -> dict:
    classes = {a["id"]: a["class"] for a in suite["agents"]}
    shares: dict[str, list[float]] = {c: [] for c in CLASSES}
    composed = 0
    saved: list[float] = []
    failed = []
    for run in bundle["runs"]:
        share, first = run_shape(run["steps"])
        shares[classes[run["agent"]]].append(share)
        composed += first
        tokens = run["tokens"]
        saved.append(100.0 * tokens["saved"] / tokens["input"] if tokens["input"] else 0.0)
        if run["outcome"] != "pass":
            failed.append(f"{run['task']}/{run['agent']}")
    runs = len(bundle["runs"])
    metrics = {f"step_share.{c}": statistics.fmean(shares[c]) for c in CLASSES}
    metrics["compose_first"] = 100.0 * composed / runs
    metrics["tokens_saved_median"] = statistics.median(saved)
    return {"metrics": {k: round(v, 2) for k, v in sorted(metrics.items())},
            "runs": runs, "failed_tasks": sorted(failed)}


def _report_only(suite: dict, traces_path: Path, report: dict, check) -> dict:
    """No baseline committed yet: score whatever traces exist, never block."""
    check("baseline", True, "no baseline recorded yet — report only (the gate blocks once "
          "agent-suite/baseline.json is committed)")
    try:
        bundle = load_traces(traces_path, suite)
    except Refusal as error:
        check("traces", False, f"not scored: {error}")
    else:
        check("traces", True, f"{len(bundle['runs'])} runs, Engine {bundle['engine_version']}")
        scored = score(suite, bundle)
        report["metrics"] = scored["metrics"]
        report["targets"] = {
            m: {"target": suite["targets"][m], "met": scored["metrics"][m] >= suite["targets"][m]}
            for m in METRICS}
    report["result"] = "REPORT_ONLY"
    return report


def gate(suite_path: Path, baseline_path: Path, traces_path: Path,
         engine_version: str | None) -> dict:
    checks: list[dict] = []

    def check(identity: str, ok: bool, detail: str) -> None:
        checks.append({"id": identity, "ok": ok, "detail": detail})

    # The mode is decided by the presence of the baseline path alone: anything
    # there (even an unreadable file, a directory or a dangling link) blocks.
    blocking = os.path.lexists(baseline_path)
    report: dict = {"schema": REPORT_SCHEMA, "mode": "blocking" if blocking else "report-only",
                    "checks": checks, "metrics": None, "baseline": None, "targets": None}
    try:
        suite = load_suite(suite_path)
        check("suite", True, f"{suite['id']}: {len(suite['tasks'])} tasks x {len(suite['agents'])} agents")
        if not blocking:
            return _report_only(suite, traces_path, report, check)
        baseline = load_baseline(baseline_path, suite)
        check("baseline", True, f"recorded with Engine {baseline['engine_version']}")
        bundle = load_traces(traces_path, suite)
        check("traces", True, f"{len(bundle['runs'])} runs, Engine {bundle['engine_version']}")
    except Refusal as error:
        check("input", False, str(error))
        report["result"] = "REFUSE"
        return report
    if engine_version is not None:
        check("engine-version", bundle["engine_version"] == engine_version,
              f"traces recorded with {bundle['engine_version']}, release is {engine_version}")
    scored = score(suite, bundle)
    report["metrics"] = scored["metrics"]
    report["baseline"] = baseline["metrics"]
    check("tasks", not scored["failed_tasks"],
          "all runs passed" if not scored["failed_tasks"] else
          "failed: " + ", ".join(scored["failed_tasks"]))
    limit = suite["regression_limit_pp"]
    targets = {}
    for metric in METRICS:
        now, before = scored["metrics"][metric], baseline["metrics"][metric]
        drop = round(before - now, 2)
        check(f"regression:{metric}", drop <= limit,
              f"{now:.2f} vs baseline {before:.2f} (drop {max(drop, 0):.2f} pp, limit {limit} pp)")
        target = suite["targets"][metric]
        targets[metric] = {"target": target, "met": now >= target}
        if suite["targets_block"]:
            check(f"target:{metric}", now >= target, f"{now:.2f} vs target {target}")
    report["targets"] = targets
    report["result"] = "ALLOW" if all(c["ok"] for c in checks) else "REFUSE"
    return report


def _line_tools(entry: object) -> list[str]:
    """Tool calls of one transcript line of the parent agent.

    Claude transcript: the `tool_use` blocks of an `assistant` message. Lines of
    a subagent (`isSidechain: true`) and progress entries that embed subagent
    calls are not the parent's steps and are skipped; the parent's own
    `Task`/`Agent` call is neutral. Codex rollout (`response_item` envelope or a
    bare item): `function_call` and `custom_tool_call` by name, and
    `local_shell_call` as a native shell step.
    """
    if not isinstance(entry, dict):
        return []
    if entry.get("type") == "assistant":
        if entry.get("isSidechain") is True:
            return []
        message = entry.get("message")
        content = message.get("content") if isinstance(message, dict) else None
        if not isinstance(content, list):
            return []
        return [block["name"] for block in content if isinstance(block, dict)
                and block.get("type") == "tool_use" and isinstance(block.get("name"), str)]
    item = entry.get("payload") if entry.get("type") == "response_item" else entry
    if not isinstance(item, dict):
        return []
    if item.get("type") in ("function_call", "custom_tool_call") and isinstance(item.get("name"), str):
        return [item["name"]]
    if item.get("type") == LOCAL_SHELL:
        return [LOCAL_SHELL]
    return []


def _read_bounded(path: Path, limit: int, what: str) -> str:
    if not path.is_file():
        raise Refusal(f"{what} is missing: {path.name}")
    if path.stat().st_size > limit:
        raise Refusal(f"{what} is larger than {limit} bytes")
    return path.read_text(encoding="utf-8")


def _json(text: str, what: str) -> object:
    try:
        return json.loads(text)
    except (ValueError, RecursionError) as error:
        raise Refusal(f"{what} is not readable JSON") from error


def steps_from(transcript: Path | None, tools: Path | None) -> list[str]:
    names: list[str] = []
    if transcript is not None:
        text = _read_bounded(transcript, MAX_TRANSCRIPT_BYTES, "transcript")
        for number, line in enumerate(text.splitlines(), 1):
            if line.strip():
                names += _line_tools(_json(line, f"transcript line {number}"))
    if tools is not None:
        text = _read_bounded(tools, MAX_TRANSCRIPT_BYTES, "tool list")
        names += [line for line in text.splitlines() if line.strip()]
    return [normalize_tool(name) for name in names]


def tokens_from(stats: Path) -> dict:
    """Only the two counters of `lean-ctx session stats --json` (bare or in an envelope)."""
    doc = _json(_read_bounded(stats, MAX_STATS_BYTES, "session stats"), "session stats")
    for key in ("body", "data", "result"):
        if isinstance(doc, dict) and "tokens" not in doc and isinstance(doc.get(key), dict):
            doc = doc[key]
    tokens = doc.get("tokens") if isinstance(doc, dict) else None
    if not isinstance(tokens, dict):
        raise Refusal("session stats: no tokens object")
    return {"input": _counter(tokens.get("input"), "tokens.input"),
            "saved": _counter(tokens.get("saved"), "tokens.saved")}


def record(args: argparse.Namespace) -> int:
    suite = load_suite(args.suite)
    if args.transcript is None and args.tools is None:
        raise Refusal("record needs --transcript or --tools")
    if args.task not in {t["id"] for t in suite["tasks"]}:
        raise Refusal(f"record: task {args.task!r} is not in suite {suite['id']}")
    if args.agent not in {a["id"] for a in suite["agents"]}:
        raise Refusal(f"record: agent {args.agent!r} is not in suite {suite['id']}")
    if args.traces.is_file():
        bundle = load_traces(args.traces, suite, complete=False)
        if bundle["engine_version"] != args.engine_version:
            raise Refusal("traces: existing bundle was recorded with another Engine version")
    else:
        bundle = {"schema": TRACES_SCHEMA, "suite": suite["id"],
                  "engine_version": args.engine_version, "runs": []}
    run = {"task": args.task, "agent": args.agent, "outcome": args.outcome,
           "tokens": tokens_from(args.stats), "steps": steps_from(args.transcript, args.tools)}
    bundle["runs"] = [r for r in bundle["runs"] if (r["task"], r["agent"]) != (args.task, args.agent)]
    bundle["runs"].append(run)
    bundle["runs"].sort(key=lambda r: (r["task"], r["agent"]))
    _write(args.traces, bundle)
    print(f"agent-suite: recorded {args.task}/{args.agent}: {len(run['steps'])} steps")
    return 0


def _write(path: Path, data: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=1, sort_keys=True) + "\n", encoding="utf-8")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)
    for name in ("record", "score", "baseline", "gate"):
        p = sub.add_parser(name)
        p.add_argument("--suite", type=Path, default=DEFAULT_SUITE)
        p.add_argument("--traces", type=Path, default=DEFAULT_TRACES)
        if name == "gate":
            p.add_argument("--baseline", type=Path, default=DEFAULT_BASELINE)
            p.add_argument("--engine-version",
                           help="optional: refuse traces recorded with another Engine "
                                "(the release job does not pass it)")
            p.add_argument("--output", type=Path, required=True)
        if name == "baseline":
            p.add_argument("--output", type=Path, default=DEFAULT_BASELINE)
        if name == "record":
            p.add_argument("--task", required=True)
            p.add_argument("--agent", required=True)
            p.add_argument("--outcome", choices=OUTCOMES, required=True)
            p.add_argument("--engine-version", required=True)
            p.add_argument("--stats", type=Path, required=True,
                           help="output of `lean-ctx session stats --json --session ID`")
            p.add_argument("--transcript", type=Path, help="Claude transcript or Codex rollout JSONL")
            p.add_argument("--tools", type=Path, help="one tool name per line (other agents)")
    args = parser.parse_args(argv)
    try:
        if args.command == "record":
            return record(args)
        if args.command == "gate":
            report = gate(args.suite, args.baseline, args.traces, args.engine_version)
            _write(args.output, report)
            # Report-only findings are notes, not refusals: nothing blocks yet.
            bad = "note  " if report["result"] == "REPORT_ONLY" else "REFUSE"
            for item in report["checks"]:
                print(f"agent-suite: {'ok    ' if item['ok'] else bad} {item['id']}: {item['detail']}")
            print(f"agent-suite: {report['result']}")
            return 0 if report["result"] in ("ALLOW", "REPORT_ONLY") else 1
        suite = load_suite(args.suite)
        bundle = load_traces(args.traces, suite)
        scored = score(suite, bundle)
        if args.command == "score":
            print(json.dumps(scored, indent=1, sort_keys=True))
            return 0
        if scored["failed_tasks"]:
            raise Refusal("a baseline needs every run passed")
        _write(args.output, {"schema": BASELINE_SCHEMA, "suite": suite["id"],
                             "engine_version": bundle["engine_version"], "metrics": scored["metrics"]})
        print(f"agent-suite: baseline written from {scored['runs']} runs")
        return 0
    except (Refusal, OSError, ValueError, RecursionError) as error:
        print(f"agent-suite: REFUSE {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
