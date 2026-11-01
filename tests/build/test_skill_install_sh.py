# SPDX-License-Identifier: Apache-2.0
"""The skill's `curl | bash` installer (skills/lean-ctx/scripts/install.sh).

It runs against a hermetic PATH: a fake `curl` serves release fixtures, a fake
`uname` pins the platform, and only the named system tools are linked in, so
the real lean-ctx on the host is never found or changed.
"""

import hashlib
import io
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "skills" / "lean-ctx" / "scripts" / "install.sh"
TEMPLATE = ROOT / "rust" / "src" / "templates" / "skill_install.sh"
ASSET = "lean-ctx-x86_64-unknown-linux-musl.tar.gz"
TOOLS = (
    "awk", "cat", "chmod", "cut", "grep", "gzip", "head", "mkdir", "mktemp", "mv",
    "rm", "sed", "tar",
)


class SkillInstallShTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory(prefix="leanctx-skill-install-")
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        self.bin = self.root / "bin"
        self.releases = self.root / "releases"
        self.tmpdir = self.root / "tmp"
        for directory in (self.bin, self.releases, self.tmpdir):
            directory.mkdir()
        for tool in TOOLS:
            found = shutil.which(tool)
            self.assertIsNotNone(found, tool)
            (self.bin / tool).symlink_to(found)
        self._script("uname", "import sys\nprint({'-s': 'Linux', '-m': 'x86_64'}.get(sys.argv[1], 'Linux'))\n")
        self._script(
            "curl",
            "import os, pathlib, sys\n"
            "args = sys.argv[1:]\n"
            "url = [a for a in args if a.startswith('https://')][-1]\n"
            "if 'api.github.com' in url:\n"
            "    print('  \"tag_name\": \"v9.9.9\",')\n"
            "    sys.exit(0)\n"
            "assert '/download/v9.9.9/' in url, url\n"
            "src = pathlib.Path(os.environ['FIXTURE_RELEASES']) / url.rsplit('/', 1)[-1]\n"
            "if not src.is_file():\n"
            "    sys.exit(22)\n"
            "pathlib.Path(args[args.index('-o') + 1]).write_bytes(src.read_bytes())\n",
        )
        archive = self._archive()
        self.digest = hashlib.sha256(archive).hexdigest()
        (self.releases / ASSET).write_bytes(archive)
        self.env = {
            "PATH": str(self.bin),
            "HOME": str(self.root),
            "TMPDIR": str(self.tmpdir),
            "FIXTURE_RELEASES": str(self.releases),
        }

    def _script(self, name, body):
        path = self.bin / name
        path.write_text(f"#!{sys.executable}\n{body}", encoding="utf-8")
        path.chmod(0o755)

    @staticmethod
    def _archive():
        payload = b"#!/bin/sh\necho 'lean-ctx 9.9.9'\n"
        buffer = io.BytesIO()
        with tarfile.open(fileobj=buffer, mode="w:gz") as archive:
            info = tarfile.TarInfo("lean-ctx")
            info.size, info.mode = len(payload), 0o755
            archive.addfile(info, io.BytesIO(payload))
        return buffer.getvalue()

    def _hash_tool(self):
        for tool in ("sha256sum", "shasum"):
            found = shutil.which(tool)
            if found:
                (self.bin / tool).symlink_to(found)
                return
        self.skipTest("no sha256sum/shasum on this host")

    def _sums(self, text):
        (self.releases / "SHA256SUMS").write_text(text, encoding="utf-8")

    def _run(self):
        return subprocess.run(
            ["/bin/bash", str(SCRIPT)], cwd=self.root, env=self.env,
            capture_output=True, text=True, timeout=60, check=False,
        )

    def assert_refused(self, result, message):
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(message, result.stderr)
        self.assertFalse((self.root / ".local" / "bin" / "lean-ctx").exists())
        self.assertEqual(list(self.tmpdir.iterdir()), [], "the temp dir was left behind")

    def test_template_is_the_committed_skill_copy(self):
        # `lean-ctx init` writes the template; skill marketplaces ship the copy.
        self.assertEqual(TEMPLATE.read_bytes(), SCRIPT.read_bytes())

    def test_success_exits_zero_and_removes_its_temp_dir(self):
        self._hash_tool()
        self._sums(f"{'0' * 64}  {ASSET}.sig\n{self.digest}  {ASSET}\n")
        result = self._run()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Checksum verified.", result.stdout)
        self.assertIn("lean-ctx v9.9.9 installed and configured.", result.stdout)
        self.assertTrue((self.root / ".local" / "bin" / "lean-ctx").is_file())
        self.assertEqual(list(self.tmpdir.iterdir()), [], "the temp dir was left behind")

    def test_missing_checksum_file_aborts(self):
        self._hash_tool()
        self.assert_refused(self._run(), "could not download SHA256SUMS")

    def test_missing_or_duplicate_entry_aborts(self):
        self._hash_tool()
        for sums in (
            f"{self.digest}  lean-ctx-aarch64-apple-darwin.tar.gz\n",
            f"{self.digest}  {ASSET}\n{'0' * 64}  {ASSET}\n",
        ):
            self._sums(sums)
            self.assert_refused(self._run(), "no unique entry")

    def test_mismatch_aborts(self):
        self._hash_tool()
        self._sums(f"{'0' * 64}  {ASSET}\n")
        self.assert_refused(self._run(), "checksum mismatch")

    def test_missing_hash_tool_aborts(self):
        self._sums(f"{self.digest}  {ASSET}\n")
        self.assert_refused(self._run(), "neither sha256sum nor shasum")


if __name__ == "__main__":
    unittest.main()
