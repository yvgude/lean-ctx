#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""A red post-merge run names the last green commit, the merges since it and
the failing tests (scripts/ci/postmerge_bisect.py, tiered CI 11.11.)."""

from pathlib import Path
import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("postmerge_bisect", ROOT / "scripts/ci/postmerge_bisect.py")
pb = importlib.util.module_from_spec(SPEC)
sys.modules.setdefault("postmerge_bisect", pb)
SPEC.loader.exec_module(pb)

JUNIT = """<?xml version="1.0"?>
<testsuites><testsuite name="lean-ctx-engine">
<testcase classname="lean-ctx-engine" name="core::a::tests::ok"/>
<testcase classname="lean-ctx-engine" name="core::a::tests::bad"><failure message="x"/></testcase>
</testsuite></testsuites>
"""


class PostmergeBisectTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        base = Path(self.tmp.name)
        self.repo, self.farm = base / "repo", base / "farm"
        self.repo.mkdir()
        self.git("init", "-q")
        self.shas = []
        for msg in ("green", "Merge branch a", "Merge branch b"):
            self.git("commit", "-q", "--allow-empty", "-m", msg)
            self.shas.append(self.git("rev-parse", "HEAD").strip())
        self.junit = base / "junit.xml"
        self.junit.write_text(JUNIT)

    def tearDown(self):
        self.tmp.cleanup()

    def git(self, *args):
        return subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@example.invalid",
                               "-c", "commit.gpgsign=false", "-C", str(self.repo), *args],
                              check=True, text=True, capture_output=True).stdout

    def publish(self, n, commit):
        gen = self.farm / "root-lean-ctx" / "integration-3-12-3" / "gens" / f"{n:06d}-{commit[:12]}"
        gen.mkdir(parents=True)
        (gen / ".warm-farm.json").write_text(json.dumps({"commit": commit}))

    def report(self):
        return pb.report(self.repo, self.farm, "root/lean-ctx", "integration/3.12.3", self.junit)

    def test_names_last_green_suspects_failures_and_bisect(self):
        self.publish(1, self.shas[0])
        text = self.report()
        self.assertIn(f"last green: {self.shas[0]} (warm-farm generation 000001-{self.shas[0][:12]})", text)
        self.assertIn("suspects (first-parent, oldest first): 2", text)
        self.assertLess(text.index("Merge branch a"), text.index("Merge branch b"))
        self.assertIn("failing tests: 1\n  lean-ctx-engine core::a::tests::bad\n", text)
        self.assertIn(f"git bisect start --first-parent {self.shas[2]} {self.shas[0]}", text)
        self.assertIn("binary_id(lean-ctx-engine) & test(=core::a::tests::bad)", text)

    def test_head_itself_and_unknown_commits_are_never_last_green(self):
        self.publish(1, self.shas[0])
        self.publish(2, "f" * 40)
        self.publish(3, self.shas[2])
        self.assertIn(f"last green: {self.shas[0]}", self.report())

    def test_without_a_farm_the_first_parent_is_the_suspect(self):
        text = pb.report(self.repo, None, "root/lean-ctx", "integration/3.12.3", self.junit)
        self.assertIn("last green: unknown", text)
        self.assertIn("suspects (first-parent, oldest first): 1", text)
        self.assertIn(f"one suspect: {self.shas[2]}", text)


if __name__ == "__main__":
    unittest.main()
