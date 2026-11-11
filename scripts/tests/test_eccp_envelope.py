#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""scripts/eccp-envelope: write, show and verify change envelopes in a temp repo."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
TOOL = ROOT / "scripts" / "eccp-envelope"
SCHEMA = ROOT / "docs" / "contracts" / "change-envelope-v1.schema.json"


class EnvelopeTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory(prefix="eccp-envelope-")
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
        self.put("rust-toolchain.toml", '[toolchain]\nchannel = "1.90.0"\n')
        self.put("Cargo.toml", '[workspace]\nmembers = ["crates/alpha", "crates/beta"]\n')
        self.put("crates/alpha/Cargo.toml", '[package]\nname = "alpha"\nversion = "0.1.0"\n')
        self.put("crates/alpha/src/lib.rs", "pub fn a() {}\n")
        self.put("crates/beta/Cargo.toml", '[package]\nname = "beta"\nversion = "0.1.0"\n')
        self.put("crates/beta/src/lib.rs", "pub fn b() {}\n")
        self.put("codegen/spec.txt", "v1\n")
        self.commit("base")
        self.base = self.rev("HEAD")
        self.git("checkout", "-q", "-b", "feat")
        self.put("crates/alpha/src/lib.rs", "pub fn a() { }\n")
        self.put("tests/accept.rs", "#[test] fn t() {}\n")
        self.put("ci/derivations.toml", '[[derivation]]\noutput = "gen/out.rs"\ninputs = ["codegen/spec.txt"]\n')
        self.commit("change")
        self.first = self.rev("HEAD")
        self.put("README", "docs\n")
        self.commit("docs")
        self.head = self.rev("HEAD")

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.repo, env=self.env, check=True, capture_output=True, text=True).stdout

    def rev(self, ref):
        return self.git("rev-parse", ref).strip()

    def put(self, path, text):
        target = self.repo / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")

    def commit(self, message):
        self.git("add", "-A")
        self.git("commit", "-q", "-m", message)

    def tool(self, *args, extra_env=None):
        env = dict(self.env, **(extra_env or {}))
        return subprocess.run([sys.executable, str(TOOL), *args], cwd=self.repo, env=env, capture_output=True, text=True)

    def write(self, *extra, extra_env=None):
        reads = self.repo / ".reads"
        reads.write_text("crates/alpha/src/lib.rs\ncrates/beta/src/lib.rs\n", encoding="utf-8")
        return self.tool(
            "write", "--base-ref", "main", "--task", "157",
            "--plan-url", "https://example.invalid/plan",
            "--acceptance", "tests/accept.rs",
            "--check", "unit=pass", "--check", f"clippy=fail@{self.first}",
            "--observed-reads", str(reads), "--rustc-version", "rustc 1.90.0 (test)",
            *extra, extra_env=extra_env,
        )

    def test_write_show_verify_round_trip(self):
        written = self.write()
        self.assertEqual(written.returncode, 0, written.stderr)
        self.assertIn(self.head, self.git("notes", "--ref", "refs/notes/eccp", "list"))

        shown = self.tool("show", self.head)
        self.assertEqual(shown.returncode, 0, shown.stderr)
        env = json.loads(shown.stdout)
        self.assertEqual(env["schema"], "leanctx.change-envelope/v1")
        self.assertEqual(env["head"], self.head)
        self.assertEqual(env["intent"], {"task": "157", "plan_url": "https://example.invalid/plan", "acceptance": ["tests/accept.rs"]})
        self.assertEqual(env["base"], {"ref": "main", "commit": self.base})
        self.assertIsNone(env["class"])
        self.assertEqual(env["read_set"]["crates"], ["alpha"])
        self.assertEqual(env["read_set"]["toolchain"]["rust_toolchain"]["path"], "rust-toolchain.toml")
        self.assertEqual(env["read_set"]["toolchain"]["rustc"], "rustc 1.90.0 (test)")
        self.assertEqual(env["read_set"]["observed_reads"], ["crates/alpha/src/lib.rs", "crates/beta/src/lib.rs"])
        self.assertEqual(env["derivations"], [{"output": "gen/out.rs", "inputs": [{"path": "codegen/spec.txt", "blob": self.rev("HEAD:codegen/spec.txt")}]}])
        self.assertEqual(env["evidence"]["checks"], [
            {"name": "clippy", "result": "fail", "commit": self.first},
            {"name": "unit", "result": "pass", "commit": self.head},
        ])

        verified = self.tool("verify", self.head)
        self.assertEqual(verified.returncode, 0, verified.stderr)
        self.assertIn("ok:", verified.stdout)

        # Rewriting is deterministic and replaces the note.
        self.assertEqual(self.write().returncode, 0)
        self.assertEqual(self.tool("show", self.head).stdout, shown.stdout)

    def test_matches_json_schema_shape(self):
        schema = json.loads(SCHEMA.read_text(encoding="utf-8"))
        self.assertEqual(schema["properties"]["schema"]["const"], "leanctx.change-envelope/v1")
        env = json.loads(self.write("--dry-run").stdout)
        self.assertEqual(sorted(env), sorted(schema["required"]))
        self.assertEqual(set(env["read_set"]) - set(schema["properties"]["read_set"]["properties"]), set())
        self.assertEqual(self.git("notes", "--ref", "refs/notes/eccp", "list"), "")

    def test_verify_rejects_base_that_is_not_an_ancestor(self):
        self.assertEqual(self.write().returncode, 0)
        env = json.loads(self.tool("show", self.head).stdout)
        self.git("checkout", "-q", "-b", "side", self.base)
        self.put("other", "x\n")
        self.commit("side")
        env["base"]["commit"] = self.rev("HEAD")
        self.git("notes", "--ref", "eccp", "add", "-f", "-m", json.dumps(env), self.head)
        verified = self.tool("verify", self.head)
        self.assertEqual(verified.returncode, 1)
        self.assertIn("is not an ancestor", verified.stderr)

    def test_verify_rejects_schema_violations(self):
        self.assertEqual(self.write().returncode, 0)
        env = json.loads(self.tool("show", self.head).stdout)
        env["class"] = "X"
        del env["evidence"]
        self.git("notes", "--ref", "eccp", "add", "-f", "-m", json.dumps(env), self.head)
        verified = self.tool("verify", self.head)
        self.assertEqual(verified.returncode, 1)
        self.assertIn("missing evidence", verified.stderr)

    def test_missing_note_is_a_usage_error(self):
        self.assertEqual(self.tool("verify", self.head).returncode, 2)
        self.assertEqual(self.tool("show", self.head).returncode, 2)

    def test_classifier_hook(self):
        classifier = self.repo / ".classify"
        classifier.write_text('#!/bin/sh\n[ -n "$1" ] && [ -n "$2" ] && echo I\n', encoding="utf-8")
        classifier.chmod(0o700)
        env = json.loads(self.write("--dry-run", extra_env={"ECCP_CLASSIFIER": str(classifier)}).stdout)
        self.assertEqual(env["class"], "I")
        self.assertEqual(json.loads(self.write("--dry-run", "--class", "G").stdout)["class"], "G")


if __name__ == "__main__":
    unittest.main()
