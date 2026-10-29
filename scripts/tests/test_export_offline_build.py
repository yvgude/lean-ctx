# SPDX-License-Identifier: Apache-2.0

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPTS = Path(__file__).resolve().parents[1]
TOOL = SCRIPTS / "export-offline-build.py"
MANIFEST = 'schema = 1\ninclude = ["rust/**"]\nforbidden_prefixes = ["ee/"]\n'
# A stand-in toolchain: the producer's contract is under test, not a compiler.
# Behaviour is steered by files beside it, so one script serves every case.
STUB_CARGO = """#!{python}
import os, pathlib, sys
here = pathlib.Path(__file__).parent
command = sys.argv[1]
if command == "--version":
    print("cargo 0.0.0-stub"); sys.exit(0)
print("stub cargo", *sys.argv[1:])
if (here / ("fail-" + command)).exists():
    sys.exit(101)
if command == "build":
    if (here / "touch-source").exists():
        pathlib.Path("src/lib.rs").write_text("// changed by the build\\n")
    binary = pathlib.Path(os.environ["CARGO_TARGET_DIR"]) / "release" / "lean-ctx"
    binary.parent.mkdir(parents=True, exist_ok=True)
    version = (here / "report-version").read_text().strip()
    binary.write_text("#!/bin/sh\\necho lean-ctx " + version + "\\n")
    binary.chmod(0o755)
if os.environ.get("CARGO_NET_OFFLINE") != "true" or "--offline" not in sys.argv:
    sys.exit(102)
"""


@unittest.skipIf(os.name == "nt", "the stand-in toolchain is a POSIX script")
class ExportOfflineBuildTest(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.base = Path(self.directory.name)
        repo = self.base / "source"
        (repo / "rust" / "src").mkdir(parents=True)
        (repo / "rust" / "Cargo.toml").write_text('[package]\nname = "e"\nversion = "3.11.0"\n')
        (repo / "rust" / "src" / "lib.rs").write_text("pub fn f() {}\n")
        env = dict(os.environ, GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.invalid",
                   GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.invalid",
                   GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_SYSTEM=os.devnull)
        for arguments in (("init", "-q"), ("add", "-A"), ("commit", "-q", "-m", "base")):
            subprocess.run(["git", "-C", str(repo), *arguments], check=True, env=env,
                           capture_output=True)
        commit = subprocess.run(["git", "-C", str(repo), "rev-parse", "HEAD"], check=True, env=env,
                                capture_output=True, text=True).stdout.strip()
        self.manifest = self.base / "manifest.toml"
        self.manifest.write_text(MANIFEST)
        self.tree = self.base / "export"
        made = subprocess.run(
            [sys.executable, str(SCRIPTS / "public-export.py"), "export", "--repo", str(repo),
             "--source-commit", commit, "--manifest", str(self.manifest), "--out", str(self.tree)],
            check=True, capture_output=True, text=True)
        self.receipt_sha256 = made.stdout.rsplit("receipt ", 1)[1].strip()
        self.toolchain = self.base / "toolchain"
        self.toolchain.mkdir()
        for name in ("cargo", "rustc"):
            stub = self.toolchain / name
            stub.write_text(STUB_CARGO.format(python=sys.executable) if name == "cargo"
                            else f"#!{sys.executable}\nprint('rustc 0.0.0-stub')\n")
            stub.chmod(0o755)
        (self.toolchain / "report-version").write_text("3.11.0")
        self.evidence = self.base / "evidence.json"
        self.arguments = {"--target-dir": str(self.base / "target"),
                          "--receipt-sha256": self.receipt_sha256}

    def run_tool(self) -> tuple[int, dict | None]:
        command = [sys.executable, str(TOOL), "--tree", str(self.tree), "--manifest",
                   str(self.manifest), "--receipt", str(self.tree / "PUBLIC-EXPORT-RECEIPT.json"),
                   "--cargo", str(self.toolchain / "cargo"), "--cargo-home", str(self.base / "home"),
                   "--log-dir", str(self.base / "logs"), "--output", str(self.evidence)]
        for flag, value in self.arguments.items():
            command += [flag, value]
        done = subprocess.run(command, check=False, capture_output=True, text=True)
        return done.returncode, (json.loads(self.evidence.read_text())
                                 if self.evidence.exists() else None)

    def test_records_a_passed_run_bound_to_the_tree_and_the_toolchain(self) -> None:
        code, evidence = self.run_tool()
        self.assertEqual((code, evidence["result"]), (0, "PASS"), evidence)
        self.assertEqual(evidence["tree_sha256"],
                         json.loads((self.tree / "PUBLIC-EXPORT-RECEIPT.json").read_text())["tree_sha256"])
        self.assertEqual([step["exit_code"] for step in evidence["steps"]], [0, 0, 0])
        self.assertEqual(evidence["toolchain"]["cargo"], "cargo 0.0.0-stub")
        for step in evidence["steps"]:
            self.assertTrue((self.base / "logs" / step["log"]).read_text().startswith("stub cargo"))

    def test_anything_short_of_a_clean_run_is_not_a_pass(self) -> None:
        cases = {
            "a step fails": (lambda: (self.toolchain / "fail-build").touch(), 1, "FAIL"),
            "the build changes its sources": (
                lambda: (self.toolchain / "touch-source").touch(), 1, "FAIL"),
            "the binary reports another version": (
                lambda: (self.toolchain / "report-version").write_text("3.10.5"), 1, "FAIL"),
            "the tree is not the pinned export": (
                lambda: self.arguments.update({"--receipt-sha256": "0" * 64}), 2, None),
            "output inside the tree": (
                lambda: self.arguments.update({"--target-dir": str(self.tree / "target")}), 2, None),
        }
        for name, (change, code, result) in cases.items():
            with self.subTest(name):
                self.setUp()
                change()
                got, evidence = self.run_tool()
                self.assertEqual(got, code, name)
                self.assertEqual(evidence["result"] if evidence else None, result, name)
        # The failing step ended the run: the test step did not start.
        self.setUp()
        (self.toolchain / "fail-build").touch()
        _, evidence = self.run_tool()
        self.assertEqual([step["id"] for step in evidence["steps"]],
                         ["resolve-offline", "build-release"])


if __name__ == "__main__":
    unittest.main()
