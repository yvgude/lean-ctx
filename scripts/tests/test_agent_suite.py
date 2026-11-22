#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Tests for scripts/benchmark/agent_suite.py, the agent release benchmark gate (rel12 WP4)."""
from __future__ import annotations

import contextlib
import copy
import importlib.util
import io
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/benchmark/agent_suite.py"
FIXTURES = ROOT / "scripts/tests/fixtures/agent-suite"
SUITE = FIXTURES / "suite.json"
BASELINE = FIXTURES / "baseline.json"
PASS = FIXTURES / "traces-pass.json"
REGRESSED = FIXTURES / "traces-regressed.json"


def load():
    spec = importlib.util.spec_from_file_location("agent_suite", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


suite_mod = load()


def checks_by_id(report: dict) -> dict:
    return {c["id"]: c for c in report["checks"]}


class ScoreTest(unittest.TestCase):
    def test_passing_fixture_metrics(self):
        suite = suite_mod.load_suite(SUITE)
        scored = suite_mod.score(suite, suite_mod.load_traces(PASS, suite))
        self.assertEqual(scored["metrics"], {"compose_first": 83.33, "step_share.acp": 63.89,
                                             "step_share.claude": 83.33, "tokens_saved_median": 47.5})
        self.assertEqual(scored["failed_tasks"], [])

    def test_run_shape(self):
        # Edits and neutral tools are outside the share; unknown native tools count against it.
        self.assertEqual(suite_mod.run_shape(["ctx_compose", "Edit", "TodoWrite", "Bash"]), (50.0, True))
        self.assertEqual(suite_mod.run_shape(["Read", "ctx_compose"]), (50.0, False))
        self.assertEqual(suite_mod.run_shape(["ctx_search", "SomeNewTool"]), (50.0, False))
        self.assertEqual(suite_mod.run_shape([]), (0.0, False))

    def test_normalize_tool(self):
        self.assertEqual(suite_mod.normalize_tool("mcp__lean-ctx__ctx_read"), "ctx_read")
        self.assertEqual(suite_mod.normalize_tool("fs/read_text_file"), "fs_read_text_file")
        for bad in ("src/main.rs", "src/main rs", "a b", "", "1abc", "x" * 80):
            with self.assertRaises(suite_mod.Refusal):
                suite_mod.normalize_tool(bad)

    def test_codex_mcp_names_in_both_forms(self):
        # Codex qualifies MCP tools as `mcp__<server>__<tool>` or `<server>__<tool>`.
        for raw in ("mcp__lean-ctx__ctx_compose", "lean-ctx__ctx_compose", "lean_ctx__ctx_compose",
                    "leanctx__ctx_compose"):
            self.assertEqual(suite_mod.normalize_tool(raw), "ctx_compose", raw)
        # Another server's tool is not unwrapped and stays a native step.
        other = suite_mod.normalize_tool("github__search_code")
        self.assertEqual(other, "github__search_code")
        steps = [suite_mod.normalize_tool("lean-ctx__ctx_read"), other]
        self.assertEqual(steps, ["ctx_read", "github__search_code"])
        self.assertEqual(suite_mod.run_shape(steps), (50.0, False))


class GateTest(unittest.TestCase):
    def gate(self, traces=PASS, baseline=BASELINE, suite=SUITE, version=None):
        return suite_mod.gate(suite, baseline, traces, version)

    def write(self, directory: str, name: str, data: dict) -> Path:
        path = Path(directory) / name
        path.write_text(json.dumps(data), encoding="utf-8")
        return path

    def test_passing_fixture_is_allowed(self):
        report = self.gate()
        self.assertEqual(report["result"], "ALLOW", report["checks"])
        self.assertEqual(report["targets"]["step_share.claude"], {"target": 80.0, "met": True})

    def test_seeded_regression_is_refused(self):
        report = self.gate(traces=REGRESSED)
        self.assertEqual(report["result"], "REFUSE")
        failed = [c["id"] for c in report["checks"] if not c["ok"]]
        self.assertEqual(failed, ["regression:step_share.claude"])
        self.assertEqual(report["metrics"]["step_share.claude"], 75.0)

    def test_limit_is_strictly_more_than_five_points(self):
        base = json.loads(BASELINE.read_text(encoding="utf-8"))
        with tempfile.TemporaryDirectory() as tmp:
            base["metrics"]["step_share.claude"] = 88.33  # drop 5.00: allowed
            self.assertEqual(self.gate(baseline=self.write(tmp, "b.json", base))["result"], "ALLOW")
            base["metrics"]["step_share.claude"] = 88.34  # drop 5.01: refused
            self.assertEqual(self.gate(baseline=self.write(tmp, "b.json", base))["result"], "REFUSE")

    def test_missing_inputs_fail_closed(self):
        # With a baseline present the gate blocks: missing traces or suite refuse.
        with tempfile.TemporaryDirectory() as tmp:
            missing = Path(tmp) / "absent.json"
            for kwargs in ({"traces": missing}, {"suite": missing}, {"suite": missing, "baseline": missing}):
                report = self.gate(**kwargs)
                self.assertEqual(report["result"], "REFUSE", kwargs)
                self.assertIn("missing", checks_by_id(report)["input"]["detail"])

    def test_baseline_present_and_traces_missing_refuses(self):
        with tempfile.TemporaryDirectory() as tmp:
            report = self.gate(traces=Path(tmp) / "absent.json")
        self.assertEqual(report["mode"], "blocking")
        self.assertEqual(report["result"], "REFUSE")
        self.assertIn("traces is missing", checks_by_id(report)["input"]["detail"])

    def test_baseline_present_and_regression_refuses(self):
        report = self.gate(traces=REGRESSED)
        self.assertEqual(report["mode"], "blocking")
        self.assertEqual(report["result"], "REFUSE")

    def test_no_baseline_is_report_only(self):
        # Until the owner commits a baseline, the gate reports and never blocks,
        # whether the traces are absent, valid or regressed.
        with tempfile.TemporaryDirectory() as tmp:
            absent = Path(tmp) / "baseline.json"
            for traces in (Path(tmp) / "traces.json", PASS, REGRESSED):
                report = self.gate(traces=traces, baseline=absent)
                self.assertEqual(report["mode"], "report-only", traces)
                self.assertEqual(report["result"], "REPORT_ONLY", traces)
                self.assertIn("no baseline recorded yet", checks_by_id(report)["baseline"]["detail"])
                self.assertIsNone(report["baseline"])
            suite = suite_mod.load_suite(SUITE)
            expected = suite_mod.score(suite, suite_mod.load_traces(PASS, suite))["metrics"]
            self.assertEqual(self.gate(traces=PASS, baseline=absent)["metrics"], expected)

    def test_report_only_still_refuses_an_invalid_suite(self):
        with tempfile.TemporaryDirectory() as tmp:
            report = self.gate(suite=Path(tmp) / "absent.json", baseline=Path(tmp) / "b.json")
        self.assertEqual(report["result"], "REFUSE")

    def test_present_but_invalid_baseline_blocks(self):
        # Presence decides the mode: an unreadable baseline must not fall back to report-only.
        with tempfile.TemporaryDirectory() as tmp:
            broken = Path(tmp) / "baseline.json"
            broken.write_text("{not json", encoding="utf-8")
            report = self.gate(baseline=broken)
            self.assertEqual(report["mode"], "blocking")
            self.assertEqual(report["result"], "REFUSE")
            directory = Path(tmp) / "dir.json"
            directory.mkdir()
            self.assertEqual(self.gate(baseline=directory)["result"], "REFUSE")

    def test_failed_task_is_refused(self):
        data = json.loads(PASS.read_text(encoding="utf-8"))
        data["runs"][0]["outcome"] = "fail"
        with tempfile.TemporaryDirectory() as tmp:
            report = self.gate(traces=self.write(tmp, "t.json", data))
        self.assertFalse(checks_by_id(report)["tasks"]["ok"])
        self.assertEqual(report["result"], "REFUSE")

    def test_incomplete_or_foreign_traces_are_refused(self):
        # Each case must refuse as invalid input, not through a metric drop.
        data = json.loads(PASS.read_text(encoding="utf-8"))
        dropped = copy.deepcopy(data)
        dropped["runs"].pop()
        doubled = copy.deepcopy(data)
        doubled["runs"].append(copy.deepcopy(data["runs"][0]))
        outside = copy.deepcopy(data)
        outside["runs"][0]["task"] = "t9"
        foreign = copy.deepcopy(data)
        foreign["suite"] = "other-suite"
        cases = ((dropped, "missing"), (doubled, "recorded twice"), (outside, "not in the suite"),
                 (foreign, "another suite"))
        with tempfile.TemporaryDirectory() as tmp:
            for case, reason in cases:
                report = self.gate(traces=self.write(tmp, "t.json", case))
                self.assertEqual(report["result"], "REFUSE", reason)
                self.assertFalse(checks_by_id(report)["input"]["ok"], reason)
                self.assertIn(reason, checks_by_id(report)["input"]["detail"])
                self.assertIsNone(report["metrics"], reason)

    def test_missing_run_refuses_even_without_a_metric_drop(self):
        # Drop a run whose removal leaves every metric within the limit.
        data = json.loads(PASS.read_text(encoding="utf-8"))
        suite = suite_mod.load_suite(SUITE)
        for index in range(len(data["runs"])):
            trial = copy.deepcopy(data)
            trial["runs"].pop(index)
            agents = {r["agent"] for r in trial["runs"]}
            if agents != {a["id"] for a in suite["agents"]}:
                continue
            metrics = suite_mod.score(suite, trial)["metrics"]
            base = json.loads(BASELINE.read_text(encoding="utf-8"))["metrics"]
            if all(round(base[m] - metrics[m], 2) <= suite["regression_limit_pp"] for m in metrics):
                break
        else:
            self.fail("fixture has no run whose removal keeps the metrics in limit")
        with tempfile.TemporaryDirectory() as tmp:
            report = self.gate(traces=self.write(tmp, "t.json", trial))
        self.assertEqual(report["result"], "REFUSE")
        self.assertIn("missing", checks_by_id(report)["input"]["detail"])

    def test_traces_stay_content_free(self):
        data = json.loads(PASS.read_text(encoding="utf-8"))
        with_path = copy.deepcopy(data); with_path["runs"][0]["steps"].append("rust/src/main.rs")
        with_args = copy.deepcopy(data); with_args["runs"][0]["args"] = ["secret"]
        with_content = copy.deepcopy(data); with_content["runs"][0]["tokens"]["content"] = "x"
        with tempfile.TemporaryDirectory() as tmp:
            for case in (with_path, with_args, with_content):
                report = self.gate(traces=self.write(tmp, "t.json", case))
                self.assertEqual(report["result"], "REFUSE")
                self.assertEqual(checks_by_id(report)["input"]["ok"], False)

    def test_release_gate_does_not_require_the_engine_version(self):
        # Owner decision: the release job passes no version, so traces of any
        # Engine version are scored and no engine-version check is emitted.
        report = self.gate(version=None)
        self.assertEqual(report["result"], "ALLOW")
        self.assertNotIn("engine-version", checks_by_id(report))

    def test_optional_engine_version_check(self):
        self.assertEqual(self.gate(version="0.0.0-fixture")["result"], "ALLOW")
        report = self.gate(version="9.9.9")
        self.assertFalse(checks_by_id(report)["engine-version"]["ok"])
        self.assertEqual(report["result"], "REFUSE")

    def test_blocking_targets(self):
        suite = json.loads(SUITE.read_text(encoding="utf-8"))
        suite["targets_block"] = True
        suite["targets"]["step_share.claude"] = 90.0
        with tempfile.TemporaryDirectory() as tmp:
            report = self.gate(suite=self.write(tmp, "s.json", suite))
        self.assertFalse(checks_by_id(report)["target:step_share.claude"]["ok"])

    def test_report_is_deterministic(self):
        self.assertEqual(json.dumps(self.gate(), sort_keys=True), json.dumps(self.gate(), sort_keys=True))

    def test_cli_exit_codes_as_in_the_release_job(self):
        with tempfile.TemporaryDirectory() as tmp:
            def run(traces: Path) -> int:
                return subprocess.run(
                    [sys.executable, "-I", str(SCRIPT), "gate", "--suite", str(SUITE), "--baseline",
                     str(BASELINE), "--traces", str(traces),
                     "--output", str(Path(tmp) / "report.json")],
                    capture_output=True, text=True, check=False).returncode
            self.assertEqual(run(PASS), 0)
            self.assertEqual(run(REGRESSED), 1)
            self.assertEqual(json.loads((Path(tmp) / "report.json").read_text())["result"], "REFUSE")
            self.assertEqual(run(Path(tmp) / "absent.json"), 1)

    def test_cli_report_only_without_baseline(self):
        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp) / "report.json"
            result = subprocess.run(
                [sys.executable, "-I", str(SCRIPT), "gate", "--suite", str(SUITE), "--baseline",
                 str(Path(tmp) / "baseline.json"), "--traces", str(Path(tmp) / "traces.json"),
                 "--output", str(output)],
                capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("no baseline recorded yet — report only", result.stdout)
            self.assertIn("agent-suite: REPORT_ONLY", result.stdout)
            self.assertNotIn("REFUSE", result.stdout)
            self.assertEqual(json.loads(output.read_text())["result"], "REPORT_ONLY")

    def test_repository_mode_follows_the_committed_baseline(self):
        # The release job runs with the defaults: blocking exactly when baseline.json is committed.
        report = suite_mod.gate(suite_mod.DEFAULT_SUITE, suite_mod.DEFAULT_BASELINE,
                                suite_mod.DEFAULT_TRACES, None)
        if suite_mod.DEFAULT_BASELINE.exists():
            self.assertEqual(report["mode"], "blocking")
        else:
            self.assertEqual(report["mode"], "report-only")
            self.assertEqual(report["result"], "REPORT_ONLY")


class RecordTest(unittest.TestCase):
    def test_record_keeps_only_tool_names_and_counters(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            transcript = tmp / "t.jsonl"
            lines = [
                {"type": "assistant", "message": {"content": [
                    {"type": "text", "text": "reading /home/user/secret.rs"},
                    {"type": "tool_use", "name": "mcp__lean-ctx__ctx_compose", "input": {"task": "x"}},
                    {"type": "tool_use", "name": "Read", "input": {"file_path": "/home/user/secret.rs"}}]}},
                {"type": "response_item", "payload": {"type": "function_call", "name": "shell",
                                                      "arguments": "{\"cmd\": \"cat /etc/passwd\"}"}},
            ]
            transcript.write_text("\n".join(json.dumps(l) for l in lines) + "\n", encoding="utf-8")
            stats = tmp / "stats.json"
            stats.write_text(json.dumps({"body": {"session_id": "s", "project_root": "/home/user/p",
                                                  "tokens": {"input": 100, "saved": 40, "tool_calls": 3}}}))
            traces = tmp / "traces.json"
            with contextlib.redirect_stdout(io.StringIO()):
                code = suite_mod.main(["record", "--suite", str(SUITE), "--traces", str(traces),
                                       "--task", "t1", "--agent", "claude", "--outcome", "pass",
                                       "--engine-version", "0.0.0-fixture", "--stats", str(stats),
                                       "--transcript", str(transcript)])
            self.assertEqual(code, 0)
            text = traces.read_text(encoding="utf-8")
            self.assertNotIn("/home/user", text)
            self.assertNotIn("passwd", text)
            run = json.loads(text)["runs"][0]
            self.assertEqual(run, {"agent": "claude", "outcome": "pass", "task": "t1",
                                   "steps": ["ctx_compose", "Read", "shell"],
                                   "tokens": {"input": 100, "saved": 40}})

    def record(self, tmp: Path, lines: list, *extra: str, traces: Path | None = None,
               task: str = "t1", agent: str = "claude") -> tuple[int, str]:
        transcript = tmp / "t.jsonl"
        transcript.write_text("\n".join(l if isinstance(l, str) else json.dumps(l) for l in lines) + "\n",
                              encoding="utf-8")
        stats = tmp / "stats.json"
        stats.write_text(json.dumps({"tokens": {"input": 10, "saved": 5}}), encoding="utf-8")
        traces = traces or tmp / "traces.json"
        err = io.StringIO()
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(err):
            code = suite_mod.main(["record", "--suite", str(SUITE), "--traces", str(traces),
                                   "--task", task, "--agent", agent, "--outcome", "pass",
                                   "--engine-version", "0.0.0-fixture", "--stats", str(stats),
                                   "--transcript", str(transcript), *extra])
        return code, err.getvalue()

    def steps(self, tmp: Path) -> list[str]:
        return json.loads((tmp / "traces.json").read_text(encoding="utf-8"))["runs"][0]["steps"]

    def test_subagent_calls_stay_out_of_the_parent_run(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            lines = [
                {"type": "assistant", "message": {"content": [{"type": "tool_use", "name": "Task"}]}},
                {"type": "assistant", "isSidechain": True,
                 "message": {"content": [{"type": "tool_use", "name": "Grep"}]}},
                {"type": "progress", "data": {"message": {"type": "assistant", "message": {
                    "content": [{"type": "tool_use", "name": "Bash"}]}}}},
                {"type": "user", "toolUseResult": {"content": [{"type": "tool_use", "name": "Glob"}]}},
                {"type": "assistant", "message": {"content": [
                    {"type": "tool_use", "name": "mcp__lean-ctx__ctx_search"}]}},
            ]
            self.assertEqual(self.record(tmp, lines)[0], 0)
            self.assertEqual(self.steps(tmp), ["Task", "ctx_search"])

    def test_codex_rollout_items(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            lines = [
                {"type": "response_item", "payload": {"type": "function_call", "name": "lean-ctx__ctx_compose"}},
                {"type": "response_item", "payload": {"type": "local_shell_call",
                                                      "action": {"command": ["cat", "/etc/hosts"]}}},
                {"type": "response_item", "payload": {"type": "custom_tool_call", "name": "apply_patch",
                                                      "input": "*** Begin Patch"}},
                {"type": "function_call", "name": "mcp__lean-ctx__ctx_read"},
            ]
            self.assertEqual(self.record(tmp, lines, agent="codex")[0], 0)
            steps = self.steps(tmp)
            self.assertEqual(steps, ["ctx_compose", "local_shell_call", "apply_patch", "ctx_read"])
            # The shell run counts against the share, the patch is an edit.
            share, composed_first = suite_mod.run_shape(steps)
            self.assertAlmostEqual(share, 200 / 3)
            self.assertTrue(composed_first)
            self.assertNotIn("/etc/hosts", (tmp / "traces.json").read_text(encoding="utf-8"))

    def test_record_refuses_bad_input_without_a_traceback(self):
        tool = {"type": "assistant", "message": {"content": [{"type": "tool_use", "name": "Read"}]}}
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            malformed = tmp / "malformed.json"
            malformed.write_text(json.dumps({"schema": suite_mod.TRACES_SCHEMA, "suite": "fixture-suite",
                                             "engine_version": "0.0.0-fixture",
                                             "runs": [{"task": "t1"}]}), encoding="utf-8")
            cases = [
                ({"lines": [tool], "traces": malformed}, "trace run"),
                ({"lines": [tool], "task": "t9"}, "not in suite"),
                ({"lines": [tool], "agent": "gemini"}, "not in suite"),
                ({"lines": ["[" * 100000 + "]" * 100000]}, "not readable JSON"),
                ({"lines": ["{not json"]}, "not readable JSON"),
            ]
            for kwargs, reason in cases:
                code, err = self.record(tmp, kwargs.pop("lines"), **kwargs)
                self.assertEqual(code, 1, reason)
                self.assertIn("REFUSE", err, reason)
                self.assertIn(reason, err)
            saved = suite_mod.MAX_TRANSCRIPT_BYTES
            suite_mod.MAX_TRANSCRIPT_BYTES = 16
            try:
                code, err = self.record(tmp, [tool])
            finally:
                suite_mod.MAX_TRANSCRIPT_BYTES = saved
            self.assertEqual(code, 1)
            self.assertIn("larger than", err)


class RepositorySuiteTest(unittest.TestCase):
    def test_release_suite_is_valid_and_fixed(self):
        suite = suite_mod.load_suite(suite_mod.DEFAULT_SUITE)
        self.assertEqual(len(suite["tasks"]), 10)
        self.assertEqual(suite["regression_limit_pp"], 5.0)
        self.assertEqual(suite["targets"], {"compose_first": 70.0, "step_share.acp": 60.0,
                                            "step_share.claude": 80.0, "tokens_saved_median": 40.0})
        self.assertEqual({t["repo"] for t in suite["tasks"]}, {"lean-ctx", "leanctl"})


if __name__ == "__main__":
    unittest.main()
