# SPDX-License-Identifier: Apache-2.0
"""Exercise Cargo's real invalidation and the source-contamination guard."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class BuildScriptTests(unittest.TestCase):
    def test_unchanged_build_is_fresh_and_new_source_is_guarded(self):
        root = Path(__file__).resolve().parents[2]
        with tempfile.TemporaryDirectory(prefix="leanctx-build-script-") as tmp:
            project = Path(tmp)
            (project / "src").mkdir()
            (project / "Cargo.toml").write_text(
                '[package]\nname = "leanctx-build-script-probe"\nversion = "0.1.0"\n'
                'edition = "2024"\n',
                encoding="utf-8",
            )
            (project / "src/lib.rs").write_text("pub fn probe() {}\n", encoding="utf-8")
            shutil.copyfile(root / "rust/build.rs", project / "build.rs")
            # build.rs includes the build-origin rules via #[path].
            (project / "src/core").mkdir()
            shutil.copyfile(
                root / "rust/src/core/build_origin.rs", project / "src/core/build_origin.rs"
            )
            env = dict(os.environ)
            env["CARGO_BUILD_JOBS"] = "3"
            # This isolated regression fixture owns and removes its tiny cache.
            env["CARGO_TARGET_DIR"] = str(project / "target")

            def build():
                return subprocess.run(
                    ["cargo", "build", "--offline", "--message-format=json", "-vv"],
                    cwd=project,
                    env=env,
                    capture_output=True,
                    text=True,
                    timeout=120,
                    check=False,
                )

            first = build()
            self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
            self.assertIn("Running", first.stderr)
            second = build()
            self.assertEqual(second.returncode, 0, second.stdout + second.stderr)
            # JSON reports cached build-script results too; verbose stderr
            # distinguishes executing a command from reusing its prior result.
            self.assertNotIn("Running", second.stderr)
            artifacts = [
                json.loads(line)
                for line in second.stdout.splitlines()
                if line.startswith("{")
            ]
            artifacts = [a for a in artifacts if a.get("reason") == "compiler-artifact"]
            self.assertTrue(artifacts, second.stdout)
            self.assertTrue(all(a["fresh"] for a in artifacts), second.stderr)

            # An unreferenced new file must still trigger the guard, rather than
            # passing merely because rustc never parses this file as a module.
            (project / "src/nested").mkdir()
            (project / "src/nested/new_file.rs").write_text(
                "--- lean-ctx: contamination regression fixture\n", encoding="utf-8"
            )
            blocked = build()
            self.assertNotEqual(blocked.returncode, 0)
            self.assertIn("BUILD BLOCKED", blocked.stderr)

    def test_git_stamp_opt_out_keeps_a_new_commit_fresh(self):
        """CI check jobs set LEANCTX_BUILD_GIT_STAMP=off (warm build farm):
        a new commit must not re-run the script, while the default stamp
        still follows HEAD."""
        root = Path(__file__).resolve().parents[2]
        with tempfile.TemporaryDirectory(prefix="leanctx-build-stamp-") as tmp:
            project = Path(tmp)
            (project / "src/core").mkdir(parents=True)
            (project / "Cargo.toml").write_text(
                '[package]\nname = "leanctx-build-stamp-probe"\nversion = "0.1.0"\n'
                'edition = "2024"\n',
                encoding="utf-8",
            )
            (project / "src/lib.rs").write_text(
                'pub const COMMIT: &str = env!("LEANCTX_BUILD_COMMIT");\n',
                encoding="utf-8",
            )
            shutil.copyfile(root / "rust/build.rs", project / "build.rs")
            shutil.copyfile(
                root / "rust/src/core/build_origin.rs", project / "src/core/build_origin.rs"
            )
            (project / ".gitignore").write_text("/target\n", encoding="utf-8")
            env = {
                k: v
                for k, v in os.environ.items()
                if not k.startswith(("GITHUB_", "LEANCTX_", "GIT_"))
            }
            env.update(
                CARGO_BUILD_JOBS="3",
                CARGO_TARGET_DIR=str(project / "target"),
                GIT_AUTHOR_NAME="probe",
                GIT_AUTHOR_EMAIL="probe@example.invalid",
                GIT_COMMITTER_NAME="probe",
                GIT_COMMITTER_EMAIL="probe@example.invalid",
                GIT_CONFIG_NOSYSTEM="1",
                GIT_CONFIG_GLOBAL=os.devnull,
            )

            def git(*args):
                subprocess.run(
                    ["git", *args], cwd=project, env=env, check=True, capture_output=True
                )

            def build(stamp):
                run_env = dict(env)
                if stamp == "off":
                    run_env["LEANCTX_BUILD_GIT_STAMP"] = "off"
                out = subprocess.run(
                    ["cargo", "build", "--offline", "-vv"],
                    cwd=project,
                    env=run_env,
                    capture_output=True,
                    text=True,
                    timeout=120,
                    check=False,
                )
                self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
                return out.stderr

            git("init", "-q", "-b", "main")
            git("add", "-A")
            git("commit", "-q", "-m", "one")
            self.assertIn("Running", build("off"))
            self.assertNotIn("Running", build("off"))
            git("commit", "-q", "--allow-empty", "-m", "two")
            self.assertNotIn("Running", build("off"))
            # The default stamp still re-runs on a new commit.
            self.assertIn("Running", build("on"))
            git("commit", "-q", "--allow-empty", "-m", "three")
            self.assertIn("Running", build("on"))


if __name__ == "__main__":
    unittest.main()
