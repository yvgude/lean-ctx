# SPDX-License-Identifier: Apache-2.0
"""install.sh --download must fail closed on checksum problems (GHSA-rxv5-4c3x-g4q6 D).

The installer runs against a hermetic PATH: a fake `curl` serves release
fixtures, a fake `uname` pins the platform, and only the named system tools
are linked in, so the real lean-ctx on the host is never found or stopped.
"""

import hashlib
import io
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
TARGET = "x86_64-unknown-linux-musl"
ASSET = f"lean-ctx-{TARGET}.tar.gz"
TOOLS = (
    "awk", "basename", "cat", "chmod", "cut", "dirname", "grep", "gzip", "head",
    "install", "mkdir", "mktemp", "mv", "rm", "sed", "tar", "tr",
)


class InstallShChecksumTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory(prefix="leanctx-install-sh-")
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        self.bin = self.root / "bin"
        self.releases = self.root / "releases"
        self.install_dir = self.root / "install"
        for directory in (self.bin, self.releases):
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
            "src = pathlib.Path(os.environ['FIXTURE_RELEASES']) / url.rsplit('/', 1)[-1]\n"
            "if not src.is_file():\n"
            "    sys.exit(22)\n"
            "pathlib.Path(args[args.index('-o') + 1]).write_bytes(src.read_bytes())\n",
        )
        self.archive = self._archive()
        self.digest = hashlib.sha256(self.archive).hexdigest()
        (self.releases / ASSET).write_bytes(self.archive)
        self.env = {
            "PATH": str(self.bin),
            "HOME": str(self.root),
            "FIXTURE_RELEASES": str(self.releases),
            "LEAN_CTX_INSTALL_DIR": str(self.install_dir),
            "LEAN_CTX_NO_ONBOARD": "1",
            "LEAN_CTX_NO_PATH_FIX": "1",
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
            ["/bin/sh", str(ROOT / "install.sh"), "--download"], cwd=self.root, env=self.env,
            capture_output=True, text=True, timeout=60, check=False,
        )

    def assert_refused(self, result, message):
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(message, result.stdout + result.stderr)
        self.assertFalse((self.install_dir / "lean-ctx").exists())

    def test_matching_checksum_installs(self):
        self._hash_tool()
        self._sums(f"{'0' * 64}  {ASSET}.sig\n{self.digest}  {ASSET}\n")
        result = self._run()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Checksum verified", result.stdout)
        self.assertTrue((self.install_dir / "lean-ctx").is_file())

    def test_missing_checksum_file_aborts(self):
        self._hash_tool()
        self.assert_refused(self._run(), "could not download SHA256SUMS")

    def test_missing_entry_aborts(self):
        self._hash_tool()
        self._sums(f"{self.digest}  lean-ctx-aarch64-apple-darwin.tar.gz\n")
        self.assert_refused(self._run(), "no unique entry")

    def test_duplicate_entry_aborts(self):
        self._hash_tool()
        self._sums(f"{self.digest}  {ASSET}\n{'0' * 64}  {ASSET}\n")
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
