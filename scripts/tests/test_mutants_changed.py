# SPDX-License-Identifier: Apache-2.0
"""Tests for scripts/mutants-changed (W3-08, cargo-mutants on changed items)."""

from __future__ import annotations

import importlib.machinery
import importlib.util
import json
import os
import stat
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "mutants-changed"


def load_module():
    loader = importlib.machinery.SourceFileLoader("mutants_changed", str(SCRIPT))
    spec = importlib.util.spec_from_loader("mutants_changed", loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


mc = load_module()

# A stand-in for `cargo` that answers `cargo mutants --version` and, for a
# run, records its arguments and writes the outcomes.json the test supplies.
FAKE_CARGO = textwrap.dedent(
    """\
    #!{python}
    import json, os, pathlib, sys
    args = sys.argv[1:]
    if args[:2] == ["mutants", "--version"]:
        print("cargo-mutants 27.1.0")
        sys.exit(0)
    pathlib.Path(os.environ["FAKE_ARGS_LOG"]).write_text(json.dumps(args))
    diff = args[args.index("--in-diff") + 1]
    pathlib.Path(os.environ["FAKE_ARGS_LOG"] + ".diff").write_text(open(diff).read())
    if "--list" in args:
        print(os.environ.get("FAKE_LIST", "[]"))
        sys.exit(int(os.environ.get("FAKE_EXIT", "0")))
    out = pathlib.Path(args[args.index("--output") + 1]) / "mutants.out"
    if "FAKE_OUTCOMES" in os.environ:
        out.mkdir(parents=True, exist_ok=True)
        (out / "outcomes.json").write_text(os.environ["FAKE_OUTCOMES"])
    sys.exit(int(os.environ.get("FAKE_EXIT", "0")))
    """
)


def mutant(name: str, summary: str) -> dict:
    return {
        "scenario": {"Mutant": {"name": name, "file": name.split(":")[0]}},
        "summary": summary,
    }


def outcomes(*entries: dict) -> dict:
    return {"outcomes": [{"scenario": "Baseline", "summary": "Success"}, *entries]}


def git(repo: Path, *args: str) -> str:
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        check=True,
        capture_output=True,
        text=True,
        env={**os.environ, "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t",
             "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@t"},
    ).stdout.strip()


class SummarizeTests(unittest.TestCase):
    def test_score_excludes_unviable_and_baseline(self):
        doc = outcomes(
            mutant("src/a.rs:1:1: replace f -> bool with true", "CaughtMutant"),
            mutant("src/a.rs:2:1: replace f -> bool with false", "CaughtMutant"),
            mutant("src/a.rs:3:1: replace > with <", "MissedMutant"),
            mutant("src/a.rs:4:1: replace g with ()", "Unviable"),
        )
        got = mc.summarize_outcomes(doc)
        self.assertEqual(got["caught"], 2)
        self.assertEqual(got["missed"], 1)
        self.assertEqual(got["unviable"], 1)
        self.assertEqual(got["mutants"], 4)
        self.assertEqual(got["score"], 66.67)
        self.assertEqual(got["missed_mutants"], ["src/a.rs:3:1: replace > with <"])

    def test_timeouts_count_against_the_score(self):
        doc = outcomes(
            mutant("src/a.rs:1:1: x", "CaughtMutant"),
            mutant("src/a.rs:2:1: y", "Timeout"),
        )
        self.assertEqual(mc.summarize_outcomes(doc)["score"], 50.0)

    def test_no_viable_mutant_has_null_score(self):
        doc = outcomes(mutant("src/a.rs:1:1: x", "Unviable"))
        self.assertIsNone(mc.summarize_outcomes(doc)["score"])
        self.assertIsNone(mc.summarize_outcomes({"outcomes": []})["score"])

    def test_label_falls_back_without_name(self):
        label = mc.mutant_label(
            {"scenario": {"Mutant": {"file": "src/b.rs", "span": {"start": {"line": 7}},
                                      "replacement": "0"}}}
        )
        self.assertEqual(label, "src/b.rs:7: replace with 0")


class ScriptTests(unittest.TestCase):
    """The script end to end against a scratch repository and a fake cargo."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        base = Path(self.tmp.name)
        self.repo = base / "repo"
        (self.repo / "rust" / "src").mkdir(parents=True)
        (self.repo / "rust" / "Cargo.toml").write_text('[package]\nname = "demo"\n')
        (self.repo / "rust" / "src" / "lib.rs").write_text("pub fn a() -> i32 { 1 }\n")
        (self.repo / "README.md").write_text("demo\n")
        git(self.repo, "init", "-q")
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-qm", "base")
        self.base = git(self.repo, "rev-parse", "HEAD")

        cargo = base / "bin" / "cargo"
        cargo.parent.mkdir()
        cargo.write_text(FAKE_CARGO.format(python=sys.executable))
        cargo.chmod(cargo.stat().st_mode | stat.S_IXUSR)
        self.cargo = cargo
        self.args_log = base / "args.json"

    def tearDown(self):
        self.tmp.cleanup()

    def commit(self, rel: str, text: str) -> None:
        path = self.repo / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-qm", f"change {rel}")

    def run_script(self, *args: str, env: dict | None = None):
        full_env = {**os.environ, "CARGO": str(self.cargo), "FAKE_ARGS_LOG": str(self.args_log)}
        full_env.pop("CI_MERGE_REQUEST_DIFF_BASE_SHA", None)
        full_env.update(env or {})
        proc = subprocess.run(
            [sys.executable, str(SCRIPT), *args],
            cwd=self.repo,
            env=full_env,
            capture_output=True,
            text=True,
            check=False,
        )
        summary = json.loads(proc.stdout) if proc.stdout.strip() else None
        return proc.returncode, summary, proc.stderr

    def test_missing_base_is_a_usage_error(self):
        code, summary, err = self.run_script()
        self.assertEqual(code, 2)
        self.assertIsNone(summary)
        self.assertIn("CI_MERGE_REQUEST_DIFF_BASE_SHA", err)

    def test_base_comes_from_merge_request_env(self):
        self.commit("README.md", "changed\n")
        code, summary, _ = self.run_script(env={"CI_MERGE_REQUEST_DIFF_BASE_SHA": self.base})
        self.assertEqual(code, 0)
        self.assertEqual(summary["base"], self.base)

    def test_no_rust_change_skips_cargo_mutants(self):
        self.commit("README.md", "changed\n")
        code, summary, _ = self.run_script("--base", self.base)
        self.assertEqual(code, 0)
        self.assertEqual(summary["status"], "no-changes")
        self.assertEqual(summary["changed_files"], [])
        self.assertIsNone(summary["score"])
        self.assertFalse(self.args_log.exists())

    def test_rust_outside_workspace_is_ignored(self):
        self.commit("tools/x.rs", "fn main() {}\n")
        code, summary, _ = self.run_script("--base", self.base)
        self.assertEqual(summary["status"], "no-changes")

    def test_missing_cargo_mutants_is_reported(self):
        self.commit("rust/src/lib.rs", "pub fn a() -> i32 { 2 }\n")
        missing = Path(self.tmp.name) / "bin" / "no-such-cargo"
        code, summary, err = self.run_script("--base", self.base, env={"CARGO": str(missing)})
        self.assertEqual(code, 3)
        self.assertEqual(summary["status"], "unavailable")
        self.assertIn("cargo install --locked cargo-mutants", err)

    def test_runs_in_diff_relative_to_workspace_and_scores(self):
        self.commit("rust/src/lib.rs", "pub fn a() -> i32 { 1 }\npub fn b() -> bool { true }\n")
        doc = outcomes(
            mutant("src/lib.rs:2:1: replace b -> bool with false", "CaughtMutant"),
            mutant("src/lib.rs:2:1: replace b -> bool with true", "MissedMutant"),
        )
        summary_file = Path(self.tmp.name) / "summary.json"
        code, summary, _ = self.run_script(
            "--base", self.base, "-p", "demo", "--summary", str(summary_file),
            "--", "--lib",
            env={"FAKE_OUTCOMES": json.dumps(doc), "FAKE_EXIT": "2"},
        )
        self.assertEqual(code, 0)
        self.assertEqual(summary["status"], "missed")
        self.assertEqual(summary["changed_files"], ["src/lib.rs"])
        self.assertEqual(summary["score"], 50.0)
        self.assertEqual(summary["cargo_mutants_exit"], 2)
        self.assertEqual(json.loads(summary_file.read_text()), summary)

        args = json.loads(self.args_log.read_text())
        self.assertEqual(args[0], "mutants")
        self.assertEqual(
            Path(args[args.index("--dir") + 1]).resolve(), (self.repo / "rust").resolve()
        )
        self.assertEqual(args[args.index("--package") + 1], "demo")
        self.assertEqual(args[-2:], ["--", "--lib"])
        diff = Path(str(self.args_log) + ".diff").read_text()
        self.assertIn("+++ b/src/lib.rs", diff)
        self.assertNotIn("rust/src/lib.rs", diff)

    def test_min_score_fails_below_threshold(self):
        self.commit("rust/src/lib.rs", "pub fn a() -> i32 { 3 }\n")
        doc = outcomes(mutant("src/lib.rs:1:1: x", "MissedMutant"))
        env = {"FAKE_OUTCOMES": json.dumps(doc), "FAKE_EXIT": "2"}
        code, summary, err = self.run_script("--base", self.base, "--min-score", "80", env=env)
        self.assertEqual(code, 1)
        self.assertEqual(summary["score"], 0.0)
        self.assertIn("--min-score", err)

    def test_all_caught_is_ok(self):
        self.commit("rust/src/lib.rs", "pub fn a() -> i32 { 4 }\n")
        doc = outcomes(mutant("src/lib.rs:1:1: x", "CaughtMutant"))
        code, summary, _ = self.run_script(
            "--base", self.base, "--min-score", "100",
            env={"FAKE_OUTCOMES": json.dumps(doc), "FAKE_EXIT": "0"},
        )
        self.assertEqual(code, 0)
        self.assertEqual(summary["status"], "ok")
        self.assertEqual(summary["score"], 100.0)

    def test_baseline_failure_is_failed(self):
        self.commit("rust/src/lib.rs", "pub fn a() -> i32 { 5 }\n")
        code, summary, err = self.run_script("--base", self.base, env={"FAKE_EXIT": "4"})
        self.assertEqual(code, 4)
        self.assertEqual(summary["status"], "failed")
        self.assertIn("baseline", err)

    def test_list_counts_mutants(self):
        self.commit("rust/src/lib.rs", "pub fn a() -> i32 { 6 }\n")
        listed = json.dumps([{"name": "a"}, {"name": "b"}, {"name": "c"}])
        code, summary, _ = self.run_script("--base", self.base, "--list", env={"FAKE_LIST": listed})
        self.assertEqual(code, 0)
        self.assertEqual(summary["status"], "listed")
        self.assertEqual(summary["mutants"], 3)
        self.assertIn("--list", json.loads(self.args_log.read_text()))


if __name__ == "__main__":
    unittest.main()
