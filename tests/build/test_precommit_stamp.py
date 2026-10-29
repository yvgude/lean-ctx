# SPDX-License-Identifier: Apache-2.0
"""The pre-commit hook checks a staged tree once and trusts only that stamp."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class PrecommitStampTests(unittest.TestCase):
    def test_a_checked_tree_is_skipped_and_any_change_is_checked_again(self):
        root = Path(__file__).resolve().parents[2]
        with tempfile.TemporaryDirectory(prefix="leanctx-precommit-") as tmp:
            repo = Path(tmp) / "repo"
            (repo / ".githooks").mkdir(parents=True)
            (repo / "rust/src").mkdir(parents=True)
            shutil.copyfile(root / ".githooks/pre-commit", repo / ".githooks/pre-commit")
            log = Path(tmp) / "cargo.log"
            bin_dir = Path(tmp) / "bin"
            bin_dir.mkdir()
            # A stand-in cargo records each call, so the test sees what ran.
            (bin_dir / "cargo").write_text(
                f'#!/bin/sh\necho "$*" >> "{log}"\n', encoding="utf-8"
            )
            (bin_dir / "cargo").chmod(0o755)
            env = dict(os.environ, PATH=f"{bin_dir}:{os.environ['PATH']}")
            env.pop("PRECOMMIT_FORCE_CARGO", None)

            def git(*args):
                subprocess.run(
                    ["git", *args], cwd=repo, env=env, check=True, capture_output=True
                )

            def hook():
                result = subprocess.run(
                    ["bash", ".githooks/pre-commit"],
                    cwd=repo,
                    env=env,
                    capture_output=True,
                    text=True,
                    timeout=60,
                    check=False,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                calls = log.read_text(encoding="utf-8").splitlines() if log.exists() else []
                log.unlink(missing_ok=True)
                return calls

            git("init", "-q")
            lib = repo / "rust/src/lib.rs"
            lib.write_text("pub fn a() {}\n", encoding="utf-8")
            git("add", "rust/src/lib.rs")

            self.assertTrue(hook(), "the first run checks the staged tree")
            self.assertEqual(hook(), [], "the same staged tree is not checked twice")

            lib.write_text("pub fn b() {}\n", encoding="utf-8")
            self.assertTrue(hook(), "unstaged Rust changes are checked, never trusted")
            self.assertTrue(hook(), "no stamp is written for a tree cargo did not see")

            git("add", "rust/src/lib.rs")
            self.assertTrue(hook(), "a new staged tree is checked")
            self.assertEqual(hook(), [])

            env["PRECOMMIT_FORCE_CARGO"] = "1"
            self.assertTrue(hook(), "forcing cargo ignores the stamp")


if __name__ == "__main__":
    unittest.main()
