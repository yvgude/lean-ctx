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
    def _archive(payload=b"#!/bin/sh\necho 'lean-ctx 9.9.9'\n"):
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
        # The closing line names the installed version, not just "installed".
        self.assertIn("Done! lean-ctx 9.9.9.", result.stdout)

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

    def _cockpit(self, sums):
        """A Cockpit bundle in the release, with `sums(digest)` as its sums file."""
        payload = b"{}"
        buffer = io.BytesIO()
        with tarfile.open(fileobj=buffer, mode="w:gz") as archive:
            info = tarfile.TarInfo("lean-ctx-cockpit/manifest.json")
            info.size = len(payload)
            archive.addfile(info, io.BytesIO(payload))
        bundle = f"lean-ctx-cockpit-{TARGET}.tar.gz"
        (self.releases / bundle).write_bytes(buffer.getvalue())
        digest = hashlib.sha256(buffer.getvalue()).hexdigest()
        (self.releases / "COCKPIT_SHA256SUMS").write_text(sums(digest, bundle), encoding="utf-8")

    def test_cockpit_with_one_matching_entry_installs(self):
        self._hash_tool()
        self._sums(f"{self.digest}  {ASSET}\n")
        self._cockpit(lambda digest, bundle: f"{'0' * 64}  other.tar.gz\n{digest}  {bundle}\n")
        result = self._run()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Cockpit: installed and verified", result.stdout)

    def test_cockpit_with_a_duplicate_entry_is_refused(self):
        # The first matching line used to win; a second entry now refuses the
        # bundle, as for the engine archive. The engine install still succeeds.
        self._hash_tool()
        self._sums(f"{self.digest}  {ASSET}\n")
        self._cockpit(lambda digest, bundle: f"{digest}  {bundle}\n{'0' * 64}  {bundle}\n")
        result = self._run()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("refusing an unverified Cockpit", result.stderr)
        self.assertNotIn("Cockpit: installed", result.stdout)

    def _upgrade(self, status):
        """An installed binary whose `daemon status` prints `status`, and a
        release binary; both record their calls in one log."""
        calls = self.root / "calls"
        self.env["CALLS"] = str(calls)
        self.install_dir.mkdir()
        old = self.install_dir / "lean-ctx"
        old.write_text(
            "#!/bin/sh\necho \"old $*\" >> \"$CALLS\"\n"
            f"[ \"$1 $2\" = 'daemon status' ] && printf '%s\\n' '{status}'\nexit 0\n",
            encoding="utf-8",
        )
        old.chmod(0o755)
        self.archive = self._archive(
            b"#!/bin/sh\necho \"new $*\" >> \"$CALLS\"\necho 'lean-ctx 9.9.9'\n"
        )
        (self.releases / ASSET).write_bytes(self.archive)
        self._hash_tool()
        self._sums(f"{hashlib.sha256(self.archive).hexdigest()}  {ASSET}\n")
        result = self._run()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return calls.read_text(encoding="utf-8").splitlines()

    def test_upgrade_stops_only_the_installed_daemon_and_restarts_it(self):
        # `lean-ctx stop` swept every lean-ctx process by name and unloaded the
        # proxy service; the upgrade now stops the replaced binary's daemon
        # alone and starts it again on the new binary.
        calls = self._upgrade("  Status:    running (PID 42)")
        self.assertEqual(calls[:2], ["old daemon status", "old daemon stop"])
        self.assertIn("new daemon restart", calls)
        self.assertNotIn("old stop", calls)

    def test_upgrade_leaves_a_stopped_daemon_stopped(self):
        calls = self._upgrade("  Status:    not running")
        self.assertEqual(calls[:2], ["old daemon status", "old daemon stop"])
        self.assertFalse([call for call in calls if call.startswith("new daemon")], calls)

    def test_installers_never_run_the_full_stop(self):
        for script in ("install.sh", "install.ps1"):
            code = "\n".join(
                line for line in (ROOT / script).read_text(encoding="utf-8").splitlines()
                if not line.lstrip().startswith("#")
            )
            self.assertNotRegex(code, r"(lean-ctx|\$installed|Source|Binary)\"? stop\b", script)
            self.assertNotRegex(code, r"pkill|killall|taskkill", script)


if __name__ == "__main__":
    unittest.main()
