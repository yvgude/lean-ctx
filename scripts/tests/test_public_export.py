# SPDX-License-Identifier: Apache-2.0

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "public-export.py"
MANIFEST = """
schema = 1
include = ["README.md", "rust/**"]
exclude = ["rust/private/**"]
forbidden_prefixes = ["ee/", "sdk/", "docs/internal/"]
"""


def git(repo: Path, *args: str) -> str:
    env = dict(os.environ, GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.invalid",
               GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.invalid",
               GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_SYSTEM=os.devnull)
    done = subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True,
                          text=True, env=env)
    return done.stdout.strip()


class PublicExportTest(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.base = Path(self.directory.name)
        self.repo = self.base / "private"
        self.repo.mkdir()
        git(self.repo, "init", "-q")
        for name, text in {
            "README.md": "public\n",
            "rust/src/lib.rs": "pub fn f() {}\n",
            "rust/private/notes.md": "excluded\n",
            "ee/service/main.rs": "private\n",
            "unlisted/tool.sh": "not named by the manifest\n",
        }.items():
            path = self.repo / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", "base")
        self.commit = git(self.repo, "rev-parse", "HEAD")
        self.manifest = self.base / "manifest.toml"
        self.manifest.write_text(MANIFEST, encoding="utf-8")
        self.out = self.base / "export"

    def run_tool(self, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run([sys.executable, str(SCRIPT), *args], check=False,
                              capture_output=True, text=True)

    def export(self, commit: str | None = None) -> subprocess.CompletedProcess[str]:
        return self.run_tool("export", "--repo", str(self.repo), "--source-commit",
                             commit or self.commit, "--manifest", str(self.manifest),
                             "--out", str(self.out))

    def verify(self, *binding: str) -> subprocess.CompletedProcess[str]:
        if not binding:
            binding = ("--repo", str(self.repo), "--source-commit", self.commit)
        return self.run_tool("verify", "--tree", str(self.out), "--manifest", str(self.manifest),
                             *binding)

    def forge_readme(self) -> None:
        """Change an allowed file and recompute every receipt digest for it."""
        changed = "altered after export\n"
        (self.out / "README.md").write_text(changed, encoding="utf-8")
        receipt_path = self.out / "PUBLIC-EXPORT-RECEIPT.json"
        receipt = json.loads(receipt_path.read_text())
        row = next(r for r in receipt["files"] if r["path"] == "README.md")
        row["sha256"] = hashlib.sha256(changed.encode()).hexdigest()
        row["size"] = len(changed)
        receipt["tree_sha256"] = hashlib.sha256("".join(
            f"{r['mode']} {r['sha256']} {r['path']}\n" for r in receipt["files"]).encode()
        ).hexdigest()
        receipt_path.write_text(json.dumps(receipt, indent=1, sort_keys=True) + "\n")

    def exported_paths(self) -> set[str]:
        return {p.relative_to(self.out).as_posix() for p in self.out.rglob("*") if p.is_file()}

    def test_exports_only_what_the_manifest_names_from_the_pinned_commit(self) -> None:
        # A working-tree change after the commit must not reach the export.
        (self.repo / "README.md").write_text("dirty\n", encoding="utf-8")
        result = self.export()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.exported_paths(),
                         {"README.md", "rust/src/lib.rs", "PUBLIC-EXPORT-RECEIPT.json"})
        self.assertEqual((self.out / "README.md").read_text(encoding="utf-8"), "public\n")
        self.assertEqual(self.verify().returncode, 0)

    def test_same_input_gives_the_same_tree_digest(self) -> None:
        self.assertEqual(self.export().returncode, 0)
        first = json.loads((self.out / "PUBLIC-EXPORT-RECEIPT.json").read_text())["tree_sha256"]
        self.out = self.base / "again"
        self.assertEqual(self.export().returncode, 0)
        second = json.loads((self.out / "PUBLIC-EXPORT-RECEIPT.json").read_text())["tree_sha256"]
        self.assertEqual(first, second)

    def test_refuses_a_moving_ref(self) -> None:
        result = self.export("HEAD")
        self.assertEqual(result.returncode, 1)
        self.assertIn("40-hex", result.stderr)

    def test_refuses_a_manifest_that_selects_a_forbidden_prefix(self) -> None:
        self.manifest.write_text(MANIFEST.replace('"rust/**"', '"rust/**", "ee/**"'),
                                 encoding="utf-8")
        result = self.export()
        self.assertEqual(result.returncode, 1)
        self.assertIn("forbidden prefix", result.stderr)
        self.assertFalse((self.out / "ee").exists())

    def test_verify_rejects_any_change_to_the_whole_tree(self) -> None:
        cases = {
            # R01: an otherwise valid export with one extra file outside every included root.
            "unlisted file outside the included roots":
                lambda: self.write("unlisted/tool.sh", "x\n"),
            "private file": lambda: self.write("ee/service/main.rs", "x\n"),
            "excluded file inside an included root":
                lambda: self.write("rust/private/notes.md", "x\n"),
            "changed content": lambda: self.write("rust/src/lib.rs", "pub fn g() {}\n"),
            "missing file": lambda: (self.out / "README.md").unlink(),
            "executable bit": lambda: (self.out / "README.md").chmod(0o755),
        }
        for index, (name, change) in enumerate(cases.items()):
            with self.subTest(name):
                self.out = self.base / f"case-{index}"
                self.assertEqual(self.export().returncode, 0)
                change()
                result = self.verify()
                self.assertEqual(result.returncode, 1, name)
                self.assertIn("refused", result.stderr)

    def test_verify_rejects_a_receipt_extended_by_hand(self) -> None:
        self.assertEqual(self.export().returncode, 0)
        receipt_path = self.out / "PUBLIC-EXPORT-RECEIPT.json"
        receipt = json.loads(receipt_path.read_text())
        self.write("unlisted/tool.sh", "x\n")
        receipt["files"].append({"path": "unlisted/tool.sh", "mode": "100644", "blob": "0" * 40,
                                 "sha256": "0" * 64, "size": 2})
        receipt_path.write_text(json.dumps(receipt), encoding="utf-8")
        result = self.verify()
        self.assertEqual(result.returncode, 1)
        self.assertIn("refused", result.stderr)

    def test_a_self_consistent_receipt_is_not_the_source(self) -> None:
        # R07: an allowed file changed, with every receipt digest recomputed to match.
        result = self.export()
        self.assertEqual(result.returncode, 0, result.stderr)
        pinned = result.stdout.rsplit("receipt ", 1)[1].strip()
        self.assertEqual(self.verify("--receipt-sha256", pinned).returncode, 0)
        self.forge_readme()
        for name, binding in {
            "expected source": (),
            "pinned receipt": ("--receipt-sha256", pinned),
        }.items():
            with self.subTest(name):
                result = self.verify(*binding)
                self.assertEqual(result.returncode, 1)
                self.assertIn("differs from", result.stderr)
        unbound = self.run_tool("verify", "--tree", str(self.out), "--manifest", str(self.manifest))
        self.assertEqual(unbound.returncode, 1)
        self.assertIn("needs an expectation", unbound.stderr)

    def test_the_receipt_cannot_name_its_own_baseline(self) -> None:
        self.assertEqual(self.export().returncode, 0)
        receipt_path = self.out / "PUBLIC-EXPORT-RECEIPT.json"
        receipt = json.loads(receipt_path.read_text())
        receipt["baseline_commit"] = self.commit
        receipt_path.write_text(json.dumps(receipt, indent=1, sort_keys=True) + "\n")
        result = self.verify()
        self.assertEqual(result.returncode, 1)
        self.assertIn("different public baseline", result.stderr)

    def generated_manifest(self, path: str, content: str, required: bool, extra: str = "") -> None:
        digest = hashlib.sha256(content.encode()).hexdigest()
        self.manifest.write_text(
            MANIFEST + f'[[generated]]\npath = "{path}"\nsha256 = "{digest}"\n'
            f"required = {str(required).lower()}\n" + extra, encoding="utf-8")

    def test_generated_files_are_named_bound_and_regular(self) -> None:
        self.generated_manifest("NOTICE.generated", "notice\n", required=False)
        self.assertEqual(self.export().returncode, 0)
        self.assertEqual(self.verify().returncode, 0, "staging: an optional file may be absent")
        self.write("NOTICE.generated", "notice\n")
        self.assertEqual(self.verify().returncode, 0)
        (self.out / "NOTICE.generated").unlink()
        outside = self.base / "payload.txt"
        outside.write_text("notice\n", encoding="utf-8")
        os.symlink(outside, self.out / "NOTICE.generated")
        result = self.verify()
        self.assertEqual(result.returncode, 1)
        self.assertIn("not a regular file", result.stderr)

        self.out = self.base / "required"
        self.generated_manifest("NOTICE.generated", "notice\n", required=True)
        self.assertEqual(self.export().returncode, 0)
        binding = ("--repo", str(self.repo), "--source-commit", self.commit)
        self.assertEqual(self.verify(*binding, "--staging").returncode, 0)
        result = self.verify()
        self.assertEqual(result.returncode, 1, "a release needs the required generated file")
        self.assertIn("required generated file is missing", result.stderr)

    def test_refuses_unsafe_generated_declarations(self) -> None:
        row = '[[generated]]\npath = "{}"\nsha256 = "' + "0" * 64 + '"\nrequired = false\n'
        cases = {
            "under a forbidden prefix": row.format("ee/NOTICE"),
            "escaping the tree": row.format("../NOTICE"),
            "listed twice": row.format("NOTICE") + row.format("NOTICE"),
            "colliding with a source file": row.format("README.md"),
            "without an explicit presence": '[[generated]]\npath = "NOTICE"\nsha256 = "'
                                            + "0" * 64 + '"\n',
        }
        for index, (name, rows) in enumerate(cases.items()):
            with self.subTest(name):
                self.out = self.base / f"generated-{index}"
                self.manifest.write_text(MANIFEST + rows, encoding="utf-8")
                result = self.export()
                self.assertEqual(result.returncode, 1, name)
                self.assertIn("refused", result.stderr)

    def test_refuses_a_selected_symlink(self) -> None:
        os.symlink("../ee/service/main.rs", self.repo / "rust" / "link.rs")
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", "link")
        result = self.export(git(self.repo, "rev-parse", "HEAD"))
        self.assertEqual(result.returncode, 1)
        self.assertIn("not a regular file", result.stderr)

    def test_a_path_new_to_the_public_must_be_named(self) -> None:
        baseline = self.commit
        (self.repo / "rust" / "src" / "added.rs").write_text("pub fn new() {}\n", encoding="utf-8")
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", "a file inside an included directory")
        self.commit = git(self.repo, "rev-parse", "HEAD")
        pinned = MANIFEST + f'baseline_commit = "{baseline}"\n'
        cases = {
            "unnamed": ("", 1, "not in the public baseline"),
            "named": ('new_paths = ["rust/src/added.rs"]\n', 0, ""),
            "stale name": ('new_paths = ["rust/src/added.rs", "rust/src/lib.rs"]\n', 1,
                           "not a new selected file"),
        }
        for index, (name, (rows, code, message)) in enumerate(cases.items()):
            with self.subTest(name):
                self.out = self.base / f"baseline-{index}"
                self.manifest.write_text(pinned + rows, encoding="utf-8")
                result = self.export()
                self.assertEqual(result.returncode, code, result.stderr)
                self.assertIn(message, result.stderr)

    def test_a_public_path_dropped_by_the_export_needs_an_approved_sunset(self) -> None:
        baseline = self.commit
        git(self.repo, "rm", "-q", "README.md")
        git(self.repo, "commit", "-q", "-m", "a public file removed in the private tree")
        self.commit = git(self.repo, "rev-parse", "HEAD")
        pinned = MANIFEST + f'baseline_commit = "{baseline}"\n'
        cases = {
            "unapproved": ("", 1, "without an approved sunset"),
            "approved": ('removed_paths = ["README.md"]\n', 0, ""),
            "stale approval": ('removed_paths = ["README.md", "rust/src/lib.rs"]\n', 1,
                               "does not drop"),
        }
        for index, (name, (rows, code, message)) in enumerate(cases.items()):
            with self.subTest(name):
                self.out = self.base / f"sunset-{index}"
                self.manifest.write_text(pinned + rows, encoding="utf-8")
                result = self.export()
                self.assertEqual(result.returncode, code, result.stderr)
                self.assertIn(message, result.stderr)

    def write(self, name: str, text: str) -> None:
        path = self.out / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")


if __name__ == "__main__":
    unittest.main()
