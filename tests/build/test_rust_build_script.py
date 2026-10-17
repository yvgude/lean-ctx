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


if __name__ == "__main__":
    unittest.main()
