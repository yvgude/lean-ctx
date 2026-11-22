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
    "install", "mkdir", "mktemp", "mv", "rm", "sed", "sleep", "tar", "tr",
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
            "LEAN_CTX_DAEMON_WAIT_SECS": "0",
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

    # The release binary: it logs each call, and its `daemon status` reports
    # the daemon running from the UP_AFTER-th status call on (never when
    # unset); `daemon restart` exits with RESTART_EXIT (default 0).
    NEW_BINARY = (
        b"#!/bin/sh\n"
        b"echo \"new $*\" >> \"$CALLS\"\n"
        b"if [ \"$1 $2\" = 'daemon restart' ]; then exit \"${RESTART_EXIT:-0}\"; fi\n"
        b"if [ \"$1 $2\" = 'daemon status' ]; then\n"
        b"  n=$(grep -c '^new daemon status' \"$CALLS\")\n"
        b"  if [ \"$n\" -ge \"${UP_AFTER:-999}\" ]; then echo '  Status:    running (PID 43)';\n"
        b"  else echo '  Status:    not running'; fi\n"
        b"  exit 0\n"
        b"fi\n"
        b"echo 'lean-ctx 9.9.9'\n"
    )

    def _upgrade_result(self, status, **env):
        """Upgrade over an installed binary whose `daemon status` prints
        `status`; both binaries record their calls in one log."""
        calls = self.root / "calls"
        self.env["CALLS"] = str(calls)
        self.env.update(env)
        self.install_dir.mkdir()
        old = self.install_dir / "lean-ctx"
        old.write_text(
            "#!/bin/sh\necho \"old $*\" >> \"$CALLS\"\n"
            f"[ \"$1 $2\" = 'daemon status' ] && printf '%s\\n' '{status}'\nexit 0\n",
            encoding="utf-8",
        )
        old.chmod(0o755)
        self.archive = self._archive(self.NEW_BINARY)
        (self.releases / ASSET).write_bytes(self.archive)
        self._hash_tool()
        self._sums(f"{hashlib.sha256(self.archive).hexdigest()}  {ASSET}\n")
        result = self._run()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result, calls.read_text(encoding="utf-8").splitlines()

    def _upgrade(self, status, **env):
        return self._upgrade_result(status, **env)[1]

    def test_upgrade_stops_only_the_installed_daemon_and_restarts_it(self):
        # `lean-ctx stop` swept every lean-ctx process by name and unloaded the
        # proxy service; the upgrade now stops the replaced binary's daemon
        # alone and starts it again on the new binary.
        calls = self._upgrade("  Status:    running (PID 42)")
        self.assertEqual(calls[:2], ["old daemon status", "old daemon stop"])
        self.assertIn("new daemon restart", calls)
        self.assertNotIn("old stop", calls)

    def test_upgrade_confirms_the_daemon_runs_on_the_new_binary(self):
        # A zero exit from `daemon restart` only says a start was requested;
        # the installer now asks the new binary whether the daemon is up.
        result, calls = self._upgrade_result("  Status:    running (PID 42)", UP_AFTER="1")
        restart = calls.index("new daemon restart")
        self.assertIn("new daemon status", calls[restart + 1:])
        self.assertIn("Daemon running on the new binary.", result.stdout)
        self.assertNotIn("did not come back", result.stderr)

    def test_upgrade_waits_for_a_daemon_that_starts_late(self):
        # An autostart unit starts the daemon asynchronously: the second
        # status check sees it running, one second later.
        result, calls = self._upgrade_result(
            "  Status:    running (PID 42)", UP_AFTER="2", LEAN_CTX_DAEMON_WAIT_SECS="3",
        )
        self.assertEqual(calls.count("new daemon status"), 2, calls)
        self.assertIn("Daemon running on the new binary.", result.stdout)

    def test_upgrade_warns_when_the_daemon_does_not_come_back(self):
        result, calls = self._upgrade_result(
            "  Status:    running (PID 42)", LEAN_CTX_DAEMON_WAIT_SECS="1",
        )
        # One check right away and one after the single second of waiting.
        self.assertEqual(calls.count("new daemon status"), 2, calls)
        self.assertIn("the lean-ctx daemon did not come back", result.stderr)
        self.assertNotIn("Daemon running on the new binary.", result.stdout)

    def test_upgrade_reads_a_wait_with_a_leading_zero_as_decimal(self):
        # `$((08 - 1))` is an octal error that ends `set -eu` sh after the
        # binary swap; the wait now counts up and compares with `[`.
        result, calls = self._upgrade_result(
            "  Status:    running (PID 42)", UP_AFTER="2", LEAN_CTX_DAEMON_WAIT_SECS="08",
        )
        self.assertEqual(calls.count("new daemon status"), 2, calls)
        self.assertIn("Daemon running on the new binary.", result.stdout)
        self.assertNotIn("Illegal number", result.stderr)
        self.assertNotIn("value too great", result.stderr)

    def test_upgrade_ends_the_wait_on_a_value_too_large_for_test(self):
        result, calls = self._upgrade_result(
            "  Status:    running (PID 42)", LEAN_CTX_DAEMON_WAIT_SECS="99999999999999999999",
        )
        self.assertEqual(calls.count("new daemon status"), 1, calls)
        self.assertIn("the lean-ctx daemon did not come back", result.stderr)

    def test_upgrade_reports_a_failed_restart_without_waiting(self):
        result, calls = self._upgrade_result(
            "  Status:    running (PID 42)", RESTART_EXIT="1", LEAN_CTX_DAEMON_WAIT_SECS="5",
        )
        self.assertIn("Could not restart the daemon", result.stderr)
        self.assertNotIn("new daemon status", calls)

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


PS_HARNESS = r"""
$ErrorActionPreference = 'Stop'
$ast = [System.Management.Automation.Language.Parser]::ParseFile($env:INSTALL_PS1, [ref]$null, [ref]$null)
$fn = $ast.Find({ param($n) $n -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -eq 'Restore-InstalledDaemon' }, $true)
if (-not $fn) { throw 'Restore-InstalledDaemon not found' }
. ([scriptblock]::Create($fn.Extent.Text))
$global:statusCalls = 0
$fake = {
    $call = $args -join ' '
    Add-Content -LiteralPath $env:CALLS -Value "new $call"
    if ($call -eq 'daemon restart') { $global:LASTEXITCODE = [int]$env:RESTART_EXIT; return }
    if ($call -eq 'daemon status') {
        if ($env:STATUS_STDERR -eq '1') { Write-Error 'a newer lean-ctx is available' }
        $global:statusCalls += 1
        if ($global:statusCalls -ge [int]$env:UP_AFTER) { '  Status:    running (PID 43)' } else { '  Status:    not running' }
    }
    $global:LASTEXITCODE = 0
}
$ok = Restore-InstalledDaemon -Binary $fake
"RESULT=$ok"
"""


@unittest.skipUnless(shutil.which("pwsh") or shutil.which("powershell"), "no PowerShell on this host")
class InstallPs1DaemonRestartTests(unittest.TestCase):
    def _run(self, up_after, wait, restart_exit=0, status_stderr=False):
        with tempfile.TemporaryDirectory(prefix="leanctx-install-ps1-") as tmp:
            calls = Path(tmp) / "calls"
            calls.write_text("", encoding="utf-8")
            env = dict(os.environ)
            env.update({
                "INSTALL_PS1": str(ROOT / "install.ps1"), "CALLS": str(calls),
                "UP_AFTER": str(up_after), "RESTART_EXIT": str(restart_exit),
                "LEAN_CTX_DAEMON_WAIT_SECS": str(wait),
                "STATUS_STDERR": "1" if status_stderr else "0",
            })
            harness = Path(tmp) / "harness.ps1"
            harness.write_text(PS_HARNESS, encoding="utf-8")
            shell = shutil.which("pwsh") or shutil.which("powershell")
            result = subprocess.run(
                [shell, "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass",
                 "-File", str(harness)],
                env=env, capture_output=True, text=True, timeout=120, check=False,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            lines = calls.read_text(encoding="utf-8").split()
            return result.stdout + result.stderr, " ".join(lines).count("daemon status")

    def test_confirms_the_daemon_runs(self):
        out, status_calls = self._run(up_after=1, wait=0)
        self.assertIn("RESULT=True", out)
        self.assertIn("Daemon running on the new binary.", out)
        self.assertEqual(status_calls, 1)

    def test_waits_for_a_daemon_that_starts_late(self):
        out, status_calls = self._run(up_after=2, wait=3)
        self.assertIn("RESULT=True", out)
        self.assertEqual(status_calls, 2)

    def test_warns_when_the_daemon_does_not_come_back(self):
        out, status_calls = self._run(up_after=999, wait=1)
        self.assertIn("RESULT=False", out)
        self.assertIn("did not come back", out)
        self.assertEqual(status_calls, 2)

    def test_reads_a_wait_with_a_leading_zero_as_decimal(self):
        out, status_calls = self._run(up_after=2, wait="08")
        self.assertIn("RESULT=True", out)
        self.assertEqual(status_calls, 2)

    def test_an_overflowing_wait_falls_back_without_ending_the_install(self):
        # `[int]'99999999999'` threw under the script-wide 'Stop'.
        out, status_calls = self._run(up_after=1, wait="99999999999")
        self.assertIn("RESULT=True", out)
        self.assertEqual(status_calls, 1)

    def test_a_non_ascii_digit_wait_falls_back(self):
        out, status_calls = self._run(up_after=1, wait="\u0661\u0662")
        self.assertIn("RESULT=True", out)
        self.assertEqual(status_calls, 1)

    def test_stderr_from_daemon_status_does_not_hide_a_running_daemon(self):
        # The harness runs under 'Stop' like install.ps1, where an error
        # record (stderr in Windows PowerShell 5.1) would end the call.
        out, status_calls = self._run(up_after=1, wait=2, status_stderr=True)
        self.assertIn("RESULT=True", out)
        self.assertEqual(status_calls, 1)

    def test_reports_a_failed_restart_without_waiting(self):
        out, status_calls = self._run(up_after=1, wait=5, restart_exit=1)
        self.assertIn("RESULT=False", out)
        self.assertIn("Could not restart the daemon", out)
        self.assertEqual(status_calls, 0)


if __name__ == "__main__":
    unittest.main()
