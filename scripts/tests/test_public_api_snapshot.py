#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Tests for scripts/public-api-snapshot (Integration Engine W1b-01)."""

from __future__ import annotations

import importlib.machinery
import importlib.util
import io
import json
import subprocess
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def load():
    path = str(ROOT / "scripts" / "public-api-snapshot")
    loader = importlib.machinery.SourceFileLoader("public_api_snapshot", path)
    spec = importlib.util.spec_from_loader("public_api_snapshot", loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


pas = load()

WORKSPACE = '[workspace]\nmembers = [".", "crates/tool"]\n'
ENGINE = '[package]\nname = "engine"\n'
TOOL = '[package]\nname = "tool"\n'


class PublicApiSnapshotTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.tmp = Path(tmp.name)
        self.repo = self.tmp / "repo"
        self.repo.mkdir()
        self.git("init", "-q")
        self.write({
            "rust/Cargo.toml": WORKSPACE + ENGINE,
            "rust/src/lib.rs": "pub fn keep() {}\npub fn change(a: u8) {}\npub fn drop_me() {}\nfn private() {}\n",
            # A binary-only member: an empty snapshot, never an error.
            "rust/crates/tool/Cargo.toml": TOOL,
            "rust/crates/tool/src/main.rs": "fn main() {}\n",
        })
        self.base = self.commit("base")
        self.write({"rust/src/lib.rs": "pub fn keep() {}\npub fn change(a: u16) {}\npub fn added() {}\nfn private2() {}\n"})
        self.head = self.commit("head")

    def git(self, *args):
        return subprocess.run(["git", "-C", str(self.repo), *args], check=True,
                              capture_output=True, text=True).stdout.strip()

    def write(self, files):
        for rel, text in files.items():
            path = self.repo / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)

    def commit(self, msg):
        self.git("add", "-A")
        self.git("-c", "user.name=t", "-c", "user.email=t@localhost", "commit", "-q", "-m", msg)
        return self.git("rev-parse", "HEAD")

    def run_main(self, *argv):
        out, err = io.StringIO(), io.StringIO()
        with redirect_stdout(out), redirect_stderr(err):
            rc = pas.main(list(argv))
        return rc, out.getvalue(), err.getvalue()

    def test_snapshot_covers_every_workspace_crate(self):
        out = self.tmp / "snap"
        rc, stdout, _ = self.run_main("snapshot", self.base, "--out", str(out), "--repo", str(self.repo))
        self.assertEqual(rc, 0, stdout)
        manifest = json.loads((out / "SNAPSHOT.json").read_text())
        self.assertEqual(manifest["schema"], "leanctx.public-api-snapshot/v1")
        self.assertEqual(manifest["commit"], self.base)
        self.assertEqual(sorted(manifest["crates"]), ["engine", "tool"])
        self.assertIsNone(manifest["crates"]["tool"]["lib"])
        self.assertEqual(manifest["crates"]["tool"]["items"], 0)
        engine = (out / "engine.txt").read_text()
        self.assertTrue(engine.startswith("# crate engine\n# extractor textual-v1\n"))
        self.assertIn("keep", engine)
        self.assertNotIn("private", engine)
        self.assertEqual(manifest["crates"]["engine"]["items"],
                         len([l for l in engine.splitlines() if l and not l.startswith("#")]))

    def test_snapshot_is_deterministic(self):
        a, b = self.tmp / "a", self.tmp / "b"
        for out in (a, b):
            self.assertEqual(self.run_main("snapshot", self.head, "--out", str(out), "--repo", str(self.repo))[0], 0)
        for name in ("engine.txt", "tool.txt", "SNAPSHOT.json"):
            self.assertEqual((a / name).read_bytes(), (b / name).read_bytes())

    def test_diff_shows_every_interface_change_and_feeds_change_class(self):
        base, head = self.tmp / "base", self.tmp / "head"
        self.run_main("snapshot", self.base, "--out", str(base), "--repo", str(self.repo))
        self.run_main("snapshot", self.head, "--out", str(head), "--repo", str(self.repo))
        out_json = self.tmp / "diff.json"
        rc, stdout, _ = self.run_main("diff", str(base), str(head), "--json", str(out_json))
        self.assertEqual(rc, 0)
        result = json.loads(out_json.read_text())
        self.assertEqual((result["base"], result["head"]), (self.base, self.head))
        self.assertEqual(result["crates_changed"], ["engine"])
        engine = result["crates"]["engine"]
        self.assertEqual(len(engine["added"]), 1)
        self.assertIn("added", engine["added"][0])
        self.assertEqual(len(engine["removed"]), 1)
        self.assertIn("drop_me", engine["removed"][0])
        self.assertEqual(len(engine["changed"]), 1)
        self.assertIn("u16", engine["changed"][0]["new"][0])
        self.assertIn("crate engine: +1 -1 ~1", stdout)
        # The same directories drive the classifier: removed/changed -> G.
        cc = pas.cc
        reasons = cc.api_reasons(cc.diff_api(cc.read_snapshot_dir(str(base)), cc.read_snapshot_dir(str(head))))
        self.assertEqual(cc.classify(reasons), "G")

    def test_no_change_is_an_empty_diff(self):
        a, b = self.tmp / "a", self.tmp / "b"
        self.run_main("snapshot", self.head, "--out", str(a), "--repo", str(self.repo))
        self.run_main("snapshot", self.head, "--out", str(b), "--repo", str(self.repo))
        rc, stdout, _ = self.run_main("diff", str(a), str(b))
        self.assertEqual(rc, 0)
        self.assertIn("0 changed", stdout)

    def test_errors_exit_2(self):
        rc, _, err = self.run_main("snapshot", "no-such-rev", "--out", str(self.tmp / "x"), "--repo", str(self.repo))
        self.assertEqual(rc, 2)
        self.assertIn("git failed", err)
        rc, _, err = self.run_main("diff", str(self.tmp / "missing"), str(self.tmp / "missing2"))
        self.assertEqual(rc, 2)


if __name__ == "__main__":
    unittest.main()
