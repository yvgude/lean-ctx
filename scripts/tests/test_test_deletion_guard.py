#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""scripts/ci/test-deletion-guard: removed and weakened tests in a diff."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
TOOL = ROOT / "scripts" / "ci" / "test-deletion-guard"

RUST_BASE = """\
pub fn add(a: i32, b: i32) -> i32 { a + b }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds() {
        assert_eq!(add(1, 2), 3);
        assert!(add(0, 0) == 0);
    }

    #[tokio::test]
    async fn adds_async() {
        assert_eq!(add(2, 2), 4);
    }
}
"""

PY_BASE = """\
import unittest


class T(unittest.TestCase):
    def test_one(self):
        self.assertEqual(1, 1)

    def test_two(self):
        assert 2 == 2
"""


class GuardTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory(prefix="test-deletion-guard-")
        self.addCleanup(tmp.cleanup)
        self.repo = Path(tmp.name)
        self.env = {
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "HOME": tmp.name,
            "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_AUTHOR_NAME": "t",
            "GIT_AUTHOR_EMAIL": "t@example.invalid",
            "GIT_COMMITTER_NAME": "t",
            "GIT_COMMITTER_EMAIL": "t@example.invalid",
        }
        self.git("init", "-q", "-b", "main")
        self.put("src/lib.rs", RUST_BASE)
        self.put("tests/test_x.py", PY_BASE)
        self.put("README.md", "x\n")
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "base")
        self.git("checkout", "-q", "-b", "feat")

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.repo, env=self.env, check=True,
                              capture_output=True, text=True).stdout

    def put(self, rel, text):
        path = self.repo / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def guard(self, *extra, code=0):
        self.git("add", "-A")
        self.git("commit", "-q", "--allow-empty", "-m", "change")
        proc = subprocess.run([sys.executable, str(TOOL), "--base", "main", *extra], cwd=self.repo,
                              env=self.env, capture_output=True, text=True)
        self.assertEqual(proc.returncode, code, proc.stderr)
        return json.loads(proc.stdout)

    def kinds(self, report):
        return sorted((f["kind"], f.get("name") or f["text"]) for f in report["findings"])

    def test_clean_change(self):
        self.put("src/lib.rs", RUST_BASE.replace("a + b }", "b + a }"))
        self.put("README.md", "y\n")
        report = self.guard("--fail-on-escalation")
        self.assertEqual(report["schema"], "leanctx.test-deletion-guard/v1")
        self.assertFalse(report["t2_escalation"])
        self.assertEqual(report["findings"], [])

    def test_added_test_is_fine(self):
        self.put("src/lib.rs", RUST_BASE.replace("}\n}\n", "}\n\n    #[test]\n    fn more() { assert!(true); }\n}\n"))
        self.assertFalse(self.guard()["t2_escalation"])

    def test_deleted_rust_test(self):
        start = RUST_BASE.index("    #[tokio::test]")
        self.put("src/lib.rs", RUST_BASE[:start].rstrip() + "\n}\n")
        report = self.guard("--fail-on-escalation", code=1)
        self.assertTrue(report["t2_escalation"])
        self.assertIn(("deleted_test", "adds_async"), self.kinds(report))
        self.assertEqual(report["counts"]["deleted_test"], 1)

    def test_removed_assert(self):
        self.put("src/lib.rs", RUST_BASE.replace("        assert!(add(0, 0) == 0);\n", ""))
        report = self.guard()
        self.assertEqual(self.kinds(report), [("removed_assert", "assert!(add(0, 0) == 0);")])
        self.assertEqual(report["findings"][0]["line"], 10)

    def test_rewritten_assert_is_not_flagged(self):
        self.put("src/lib.rs", RUST_BASE.replace("assert!(add(0, 0) == 0);", "assert_eq!(add(0, 0), 0);"))
        self.assertFalse(self.guard()["t2_escalation"])

    def test_ignore_added(self):
        self.put("src/lib.rs", RUST_BASE.replace("    #[test]\n", "    #[test]\n    #[ignore]\n"))
        self.put("tests/test_x.py", PY_BASE.replace("    def test_two", "    @unittest.skip('flaky')\n    def test_two"))
        report = self.guard()
        self.assertEqual(report["counts"]["ignored_test"], 2)
        self.assertTrue(report["t2_escalation"])

    def test_deleted_python_test_and_file(self):
        self.put("tests/test_x.py", PY_BASE.split("    def test_two")[0])
        report = self.guard()
        self.assertIn(("deleted_test", "test_two"), self.kinds(report))
        self.assertIn(("removed_assert", "assert 2 == 2"), self.kinds(report))
        self.git("rm", "-q", "tests/test_x.py")
        report = self.guard()
        self.assertIn(("deleted_test", "test_one"), self.kinds(report))

    def test_moved_test_is_not_flagged(self):
        start = RUST_BASE.index("    #[tokio::test]")
        moved = RUST_BASE[start:RUST_BASE.index("}\n}\n") + 2]
        self.put("src/lib.rs", RUST_BASE[:start].rstrip() + "\n}\n")
        self.put("src/more.rs", "#[cfg(test)]\nmod tests {\n    use crate::*;\n\n" + moved + "}\n")
        report = self.guard()
        self.assertFalse(report["t2_escalation"], report["findings"])

    def test_diff_from_stdin(self):
        self.put("src/lib.rs", RUST_BASE.replace("        assert!(add(0, 0) == 0);\n", ""))
        self.git("add", "-A")
        diff = self.git("diff", "--cached")
        proc = subprocess.run([sys.executable, str(TOOL), "--diff", "-"], input=diff, capture_output=True,
                              text=True, env=self.env, cwd=self.repo)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        report = json.loads(proc.stdout)
        self.assertEqual(report["counts"]["removed_assert"], 1)
        self.assertIsNone(report["base"])

    def test_usage_error(self):
        proc = subprocess.run([sys.executable, str(TOOL)], capture_output=True, text=True, cwd=self.repo, env=self.env)
        self.assertEqual(proc.returncode, 2)


if __name__ == "__main__":
    unittest.main()
