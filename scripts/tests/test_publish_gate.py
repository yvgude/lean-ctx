# SPDX-License-Identifier: Apache-2.0

from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPTS = Path(__file__).resolve().parents[1]
GATE = SCRIPTS / "publish-gate.py"
STUB_GATES = ("export-secret-gate.py", "check-source-layout.py", "ip-provenance-audit.py")
PASSING_STUB = "import sys\nprint('stub: PASS')\nsys.exit(0)\n"
MANIFEST = 'schema = 1\ninclude = ["README.md", "rust/**"]\nforbidden_prefixes = ["ee/"]\n'


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


class PublishGateTest(unittest.TestCase):
    """Local gates are stubs here: what is under test is the decision around them."""

    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.base = Path(self.directory.name)
        repo = self.base / "source"
        (repo / "rust").mkdir(parents=True)
        (repo / "README.md").write_text("public\n", encoding="utf-8")
        (repo / "rust" / "Cargo.toml").write_text('[package]\nname = "e"\nversion = "3.11.0"\n',
                                                  encoding="utf-8")
        env = dict(os.environ, GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.invalid",
                   GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.invalid",
                   GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_SYSTEM=os.devnull)
        for arguments in (("init", "-q"), ("add", "-A"), ("commit", "-q", "-m", "base")):
            subprocess.run(["git", "-C", str(repo), *arguments], check=True, env=env,
                           capture_output=True)
        commit = subprocess.run(["git", "-C", str(repo), "rev-parse", "HEAD"], check=True, env=env,
                                capture_output=True, text=True).stdout.strip()
        self.tools = self.base / "tools"
        self.tools.mkdir()
        for real in ("public-export.py", "export-offline-build.py"):
            shutil.copy(SCRIPTS / real, self.tools / real)
        for name in STUB_GATES:
            (self.tools / name).write_text(PASSING_STUB, encoding="utf-8")
        self.manifest = self.base / "manifest.toml"
        self.manifest.write_text(MANIFEST, encoding="utf-8")
        self.tree = self.base / "export"
        made = subprocess.run(
            [sys.executable, str(self.tools / "public-export.py"), "export", "--repo", str(repo),
             "--source-commit", commit, "--manifest", str(self.manifest), "--out", str(self.tree)],
            check=True, capture_output=True, text=True)
        self.receipt = self.tree / "PUBLIC-EXPORT-RECEIPT.json"
        # The publication clone: every exported file added, nothing else.
        self.env = env
        for arguments in (("init", "-q"), ("add", "-A", "-f")):
            subprocess.run(["git", "-C", str(self.tree), *arguments], check=True, env=env,
                           capture_output=True)
        self.pins_sha256 = None
        tree_sha256 = json.loads(self.receipt.read_text())["tree_sha256"]
        self.build = self.base / "build-evidence.json"
        # Shaped like the producer's evidence; the producer itself has its own module.
        self.build_fields = {
            "schema": "leanctx.export-build-evidence/v1", "result": "PASS",
            "tree_sha256": tree_sha256, "engine_version": "3.11.0",
            "receipt_sha256": made.stdout.rsplit("receipt ", 1)[1].strip(),
            "producer_sha256": sha256(self.tools / "export-offline-build.py"),
            "toolchain": {"cargo": "cargo (fixture)"}}
        self.build.write_text(json.dumps(self.build_fields))
        self.contract = self.base / "agent-tools-v1.json"
        self.contract.write_text(json.dumps({"engine_version": "3.11.0"}))
        self.artifact = self.base / "engine.tar.gz"
        self.artifact.write_bytes(b"release bytes")
        self.pins = {
            "schema": "leanctx.publish-pins/v1",
            "source_commit": commit,
            "manifest_sha256": sha256(self.manifest),
            "receipt_sha256": made.stdout.rsplit("receipt ", 1)[1].strip(),
            "tools": {p.name: sha256(p) for p in self.tools.iterdir()},
            "engine_version": "3.11.0",
            "sdk": {"contract_sha256": sha256(self.contract)},
            "evidence": {"R05-offline-build": sha256(self.build)},
            "reviews": {"R04-content-review": None},
            "artifacts": {"engine": sha256(self.artifact)},
        }
        self.arguments = {"--sdk-contract": str(self.contract), "--build-evidence": str(self.build),
                          "--artifact": f"engine={self.artifact}"}

    def decide(self) -> tuple[int, dict]:
        pins = self.base / "pins.json"
        pins.write_text(json.dumps(self.pins), encoding="utf-8")
        output = self.base / "decision.json"
        command = [sys.executable, str(GATE), "--pins", str(pins), "--tree", str(self.tree),
                   "--manifest", str(self.manifest), "--receipt", str(self.receipt),
                   "--tools-dir", str(self.tools), "--work", str(self.base / "work"),
                   "--output", str(output),
                   "--pins-sha256", self.pins_sha256 or sha256(pins)]
        for flag, value in self.arguments.items():
            command += [flag, value]
        done = subprocess.run(command, check=False, capture_output=True, text=True)
        return done.returncode, json.loads(output.read_text())

    def failed(self, report: dict) -> set[str]:
        return {item["id"] for item in report["checks"] if not item["ok"]}

    def test_allows_only_the_fully_pinned_and_passed_state(self) -> None:
        code, report = self.decide()
        self.assertEqual((code, report["result"]), (0, "ALLOW"), report)

    def produce(self, *changes: str, drop: tuple[str, ...] = ()) -> subprocess.CompletedProcess[str]:
        arguments = {"--tree": str(self.tree), "--manifest": str(self.manifest),
                     "--receipt": str(self.receipt), "--tools-dir": str(self.tools),
                     "--sdk-contract": str(self.contract), "--build-evidence": str(self.build),
                     "--artifact": f"engine={self.artifact}",
                     "--output": str(self.base / "produced-pins.json")}
        command = [sys.executable, str(SCRIPTS / "publish-pins.py"), "--no-content-review"]
        for flag, value in arguments.items():
            if flag not in drop:
                command += [flag, value]
        return subprocess.run(command + list(changes), check=False, capture_output=True, text=True)

    def test_the_producer_writes_what_the_gate_accepts_and_fills_no_gap(self) -> None:
        self.assertEqual(self.produce().returncode, 1, "check mode: nothing on disk yet")
        made = self.produce("--write")
        self.assertEqual(made.returncode, 0, made.stderr)
        self.pins = json.loads((self.base / "produced-pins.json").read_text())
        code, report = self.decide()
        self.assertEqual((code, report["result"]), (0, "ALLOW"), report)
        self.assertEqual(self.produce().returncode, 0, "check mode: file equals what is derived")

        refusals = {
            "no build evidence": lambda: self.produce(drop=("--build-evidence",)),
            "no artifact and no explicit none": lambda: self.produce(drop=("--artifact",)),
            "SDK neither named nor excluded": lambda: self.produce(drop=("--sdk-contract",)),
            "tree is not the receipt's": lambda: (
                (self.tree / "README.md").write_text("edited\n"), self.produce())[1],
        }
        for name, attempt in refusals.items():
            with self.subTest(name):
                result = attempt()
                self.assertEqual(result.returncode, 2, name + result.stdout)
                self.assertIn("refused", result.stderr)

    def test_each_single_deviation_refuses(self) -> None:
        def gate_says_review_required() -> None:
            # Re-pinned on purpose: the tool is the approved one, its verdict is not a pass.
            stub = self.tools / "export-secret-gate.py"
            stub.write_text("import sys\nprint('REVIEW_REQUIRED')\nsys.exit(1)\n")
            self.pins["tools"][stub.name] = sha256(stub)

        def other_build_tree() -> None:
            self.build.write_text(json.dumps(dict(self.build_fields, tree_sha256="0" * 64)))
            self.pins["evidence"]["R05-offline-build"] = sha256(self.build)

        def build_by_another_producer() -> None:
            self.build.write_text(json.dumps(dict(self.build_fields, producer_sha256="0" * 64)))
            self.pins["evidence"]["R05-offline-build"] = sha256(self.build)

        cases = {
            "gate verdict is not a pass": (gate_says_review_required, {"R04-secrets-tree"}),
            "gate tool changed after pinning": (
                lambda: (self.tools / "ip-provenance-audit.py").write_text(PASSING_STUB + "#\n"),
                {"R03-provenance"}),
            "file changed in the tree": (
                lambda: (self.tree / "README.md").write_text("edited\n"),
                {"export-binding", "engine-version", "publication-index", "R04-secrets-tree",
                 "R02-community-closure", "R03-provenance", "R05-offline-build"}),
            "another source commit pinned": (
                lambda: self.pins.update(source_commit="1" * 40),
                {"export-binding", "engine-version", "publication-index", "R04-secrets-tree",
                 "R02-community-closure", "R03-provenance", "R05-offline-build"}),
            "exported file not in the git index": (
                lambda: subprocess.run(["git", "-C", str(self.tree), "rm", "-q", "--cached",
                                        "README.md"], check=True, env=self.env),
                {"publication-index", "R04-secrets-tree", "R02-community-closure",
                 "R03-provenance"}),
            "pins are not the release environment's": (
                lambda: setattr(self, "pins_sha256", "0" * 64), {"pins"}),
            "content review supplied but not pinned": (
                lambda: self.arguments.update({"--content-review": str(self.build)}),
                {"R04-secrets-tree"}),
            "another Engine version pinned": (
                lambda: self.pins.update(engine_version="3.11.1"),
                {"engine-version", "sdk-pair", "R05-offline-build"}),
            "build evidence absent": (
                lambda: self.arguments.pop("--build-evidence"), {"R05-offline-build"}),
            "build evidence rewritten, not re-pinned": (
                lambda: self.build.write_text(self.build.read_text() + "\n"), {"R05-offline-build"}),
            "build evidence about another tree": (other_build_tree, {"R05-offline-build"}),
            "build evidence not from the pinned producer": (
                build_by_another_producer, {"R05-offline-build"}),
            "artifact changed": (lambda: self.artifact.write_bytes(b"other"), {"artifacts"}),
            "artifact not supplied": (lambda: self.arguments.pop("--artifact"), {"artifacts"}),
            "SDK contract not supplied": (
                lambda: self.arguments.pop("--sdk-contract"), {"sdk-pair"}),
            "unknown key in the pins": (lambda: self.pins.update(waiver="yes"), {"pins"}),
        }
        for name, (change, expected) in cases.items():
            with self.subTest(name):
                self.setUp()
                change()
                code, report = self.decide()
                self.assertEqual((code, report["result"]), (1, "REFUSE"), name)
                self.assertEqual(self.failed(report), expected, name)


if __name__ == "__main__":
    unittest.main()
