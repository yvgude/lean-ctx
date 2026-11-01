# SPDX-License-Identifier: Apache-2.0
"""Exercise the legacy shell entrypoint without installing a runtime."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

# The downloaded engine fixture: records `cockpit install --from <dir>` when the
# directory holds the bundle files, and fails like the engine on request.
ENGINE_SCRIPT = (
    "#!/bin/sh\n"
    "if [ \"$1\" = cockpit ]; then\n"
    "  if [ -n \"${FAKE_ENGINE_COCKPIT_EXIT:-}\" ]; then echo 'refused: bad bundle' >&2; exit 1; fi\n"
    "  [ \"$2 $3\" = 'install --from' ] && [ -f \"$4/channel.sig\" ] || exit 3\n"
    "  printf 'cockpit-install %s %s\\n' \"$3\" \"$4\" >> \"$FAKE_ENGINE_LOG\"\n"
    "  echo '{}'; exit 0\n"
    "fi\n"
    "echo new\n"
)


class DevInstallTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="leanctx-installer-")
        self.addCleanup(self.tmp.cleanup)
        self.directory = Path(self.tmp.name)
        self.record = self.directory / "cargo.json"
        self.service_record = self.directory / "service-called"
        self.script = Path(__file__).resolve().parents[2] / "rust/dev-install.sh"
        executable = self.directory / "cargo"
        executable.write_text(
            f"#!{sys.executable}\n"
            "import json, os, pathlib, sys\n"
            "pathlib.Path(os.environ['BUILD_FLOW_ARGV']).write_text("
            "json.dumps({'args': sys.argv[1:], 'cwd': os.getcwd()}))\n"
            "sys.exit(79)\n",
            encoding="utf-8",
        )
        executable.chmod(0o755)
        service = self.directory / "lean-ctx"
        service.write_text(
            f"#!{sys.executable}\n"
            "import os, pathlib\n"
            "pathlib.Path(os.environ['BUILD_FLOW_SERVICE']).touch()\n",
            encoding="utf-8",
        )
        service.chmod(0o755)
        # No real runtime home is available to a regressed legacy script.
        self.env = {k: v for k, v in os.environ.items() if k not in ("HOME", "CARGO_HOME")}
        self.env["PATH"] = str(self.directory) + os.pathsep + os.defpath
        self.env["BUILD_FLOW_ARGV"] = str(self.record)
        self.env["BUILD_FLOW_SERVICE"] = str(self.service_record)

    def run_installer(self, args):
        return subprocess.run(
            ["bash", str(self.script), *args], cwd=self.directory, env=self.env,
            capture_output=True, text=True, timeout=10, check=False,
        )

    def test_help_and_invalid_arguments_never_build(self):
        for args, expected in [
            (["--help"], 0), (["-h"], 0), (["--unknown"], 2),
            (["--fast", "--release"], 2),
        ]:
            with self.subTest(args=args):
                result = self.run_installer(args)
                self.assertEqual(result.returncode, expected, result.stderr)
                self.assertFalse(self.record.exists())
                self.assertFalse(self.service_record.exists())


    def test_build_failure_propagates_before_any_runtime_operation(self):
        for args, profile in [([], "dev-fast"), (["--fast"], "dev-fast"), (["--release"], "release")]:
            with self.subTest(args=args):
                result = self.run_installer(args)
                self.assertEqual(result.returncode, 79, result.stderr)
                invocation = json.loads(self.record.read_text())
                command = invocation["args"]
                self.assertEqual(command[command.index("--profile") + 1], profile)
                self.assertEqual(command[command.index("--") + 1:], ["dev-install", *args])
                self.assertEqual(Path(invocation["cwd"]), self.script.parent)
                self.assertEqual("build.incremental=true" in command, profile == "dev-fast")
                self.assertFalse(self.service_record.exists())


class SourceInstallTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="leanctx-source-installer-")
        self.addCleanup(self.tmp.cleanup)
        self.directory = Path(self.tmp.name)
        self.repo = self.directory / "repo"
        (self.repo / "rust").mkdir(parents=True)
        self.script = self.repo / "install.sh"
        shutil.copyfile(Path(__file__).resolve().parents[2] / "install.sh", self.script)
        self.target = self.directory / "shared-target"
        self.install_dir = self.directory / "installed"
        self.install_dir.mkdir()
        self.installed = self.install_dir / "lean-ctx"
        # The installed binary records its own calls: install.sh stops only its
        # daemon, while the new binary waits beside it, never a full `stop`.
        self.old_binary = (
            f"#!{sys.executable}\nimport json, os, pathlib, sys\n"
            "assert list(pathlib.Path(__file__).parent.glob('.lean-ctx.new.*'))\n"
            "with pathlib.Path(os.environ['BUILD_FLOW_RECORD']).open('a') as stream:\n"
            "    stream.write(json.dumps(['installed', *sys.argv[1:]]) + '\\n')\n"
        )
        self.write_old_binary()
        self.record = self.directory / "events.jsonl"
        self.bin_dir = self.directory / "bin"
        self.bin_dir.mkdir()
        for name, body in {
            "cargo": (
                "args = sys.argv[1:]\n"
                "with record.open('a') as stream: stream.write(json.dumps(args) + '\\n')\n"
                "if args[0] == 'metadata':\n"
                "    print(json.dumps({'target_directory': os.environ['BUILD_FLOW_TARGET']}, separators=(',', ':')))\n"
                "else: sys.exit(int(os.environ.get('BUILD_FLOW_EXIT', '0')))\n"
            ),
            "lean-ctx": (
                "with record.open('a') as stream: stream.write(json.dumps(sys.argv[1:]) + '\\n')\n"
            ),
        }.items():
            executable = self.bin_dir / name
            executable.write_text(
                f"#!{sys.executable}\nimport json, os, pathlib, sys\n"
                "record = pathlib.Path(os.environ['BUILD_FLOW_RECORD'])\n" + body,
                encoding="utf-8",
            )
            executable.chmod(0o755)
        self.env = {k: v for k, v in os.environ.items() if k not in ("HOME", "CARGO_HOME")}
        self.env.update({
            "PATH": os.pathsep.join((str(self.bin_dir), str(self.install_dir), os.defpath)),
            "LEAN_CTX_INSTALL_DIR": str(self.install_dir),
            "LEAN_CTX_NO_ONBOARD": "1",
            "BUILD_FLOW_RECORD": str(self.record),
            "BUILD_FLOW_TARGET": str(self.target),
        })

    def write_old_binary(self):
        self.installed.write_text(self.old_binary, encoding="utf-8")
        self.installed.chmod(0o755)

    def run_installer(self, args):
        return subprocess.run(
            ["sh", str(self.script), *args], cwd=self.directory, env=self.env,
            capture_output=True, text=True, timeout=10, check=False,
        )

    def events(self):
        return [json.loads(line) for line in self.record.read_text().splitlines()]

    def test_failure_and_invalid_arguments_preserve_installed_binary(self):
        self.env["BUILD_FLOW_EXIT"] = "79"
        for args in ([], ["--release"], ["--build-only"], ["--build-only", "--release"]):
            with self.subTest(args=args):
                result = self.run_installer(args)
                self.assertEqual(result.returncode, 79, result.stderr)
                self.assertEqual(self.installed.read_text(), self.old_binary)
                self.assertNotIn(["stop"], self.events())
                self.assertNotIn(["installed", "daemon", "stop"], self.events())
        self.record.unlink()
        for args in (["--unknown"], ["--build-only", "--unknown"], ["--release", "--unknown"]):
            self.assertEqual(self.run_installer(args).returncode, 2)
        self.assertFalse(self.record.exists())

        (self.repo / "rust").rmdir()
        for args in (["--build-only"], ["--build-only", "--release"], ["--release"]):
            result = self.run_installer(args)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn("require the lean-ctx checkout", result.stderr)
        self.assertFalse(self.record.exists())

    def test_shared_target_is_copied_after_build_and_build_only_never_stops(self):
        for args, profile in (([], "dev-fast"), (["--release"], "release")):
            with self.subTest(args=args):
                binary = self.target / profile / "lean-ctx"
                binary.parent.mkdir(parents=True)
                binary.write_text("#!/bin/sh\necho fixture\n", encoding="utf-8")
                binary.chmod(0o755)
                self.write_old_binary()
                self.record.unlink(missing_ok=True)
                build_only = ["--build-only", *args]
                result = self.run_installer(build_only)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.installed.read_text(), self.old_binary)
                self.assertNotIn(["installed", "daemon", "stop"], self.events())
                expected = ["fast"] if profile == "dev-fast" else ["build", "--locked", "--release", "--timings"]
                self.assertEqual(self.events()[0], expected)
                result = self.run_installer(args)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.events()[-1], ["installed", "daemon", "stop"])
                self.assertNotIn(["stop"], self.events())
                self.assertEqual(self.installed.read_bytes(), binary.read_bytes())
                self.assertFalse(self.installed.is_symlink())
                binary.write_text("changed build artifact", encoding="utf-8")
                self.assertIn("fixture", self.installed.read_text())


class DownloadInstallTests(unittest.TestCase):
    """`install.sh --download` must fail closed when integrity is unprovable."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="leanctx-download-installer-")
        self.addCleanup(self.tmp.cleanup)
        self.directory = Path(self.tmp.name)
        self.install_dir = self.directory / "installed"
        self.install_dir.mkdir()
        self.installed = self.install_dir / "lean-ctx"
        payload = self.directory / "payload"
        payload.mkdir()
        (payload / "lean-ctx").write_text(ENGINE_SCRIPT, encoding="utf-8")
        (payload / "lean-ctx").chmod(0o755)
        self.tarball = self.directory / "fixture.tar.gz"
        subprocess.run(["tar", "-czf", str(self.tarball), "-C", str(payload), "lean-ctx"], check=True)
        bundle = self.directory / "cockpit-payload" / "lean-ctx-cockpit"
        bundle.mkdir(parents=True)
        for name in ("channel.json", "channel.sig", "archive.tar.gz", "manifest.json", "manifest.sig"):
            (bundle / name).write_text(name, encoding="utf-8")
        self.cockpit = self.directory / "cockpit.tar.gz"
        subprocess.run(["tar", "-czf", str(self.cockpit), "-C", str(bundle.parent), "lean-ctx-cockpit"],
                       check=True)
        self.engine_log = self.directory / "engine.log"
        self.bin_dir = self.directory / "bin"
        self.bin_dir.mkdir()
        curl = self.bin_dir / "curl"
        curl.write_text(
            f"#!{sys.executable}\n"
            "import hashlib, os, pathlib, shutil, sys\n"
            "args = sys.argv[1:]\n"
            "url = next(a for a in args if a.startswith('https://'))\n"
            "out = args[args.index('-o') + 1] if '-o' in args else None\n"
            "state = pathlib.Path(os.environ['FAKE_STATE'])\n"
            "tarball = pathlib.Path(os.environ['FAKE_TARBALL'])\n"
            "cockpit = pathlib.Path(os.environ['FAKE_COCKPIT_TARBALL'])\n"
            "cockpit_mode = os.environ.get('FAKE_COCKPIT', 'good')\n"
            "if 'api.github.com' in url:\n"
            "    print('{\"tag_name\": \"v9.9.9\"}')\n"
            "elif '/lean-ctx-cockpit-' in url:\n"
            "    assert '/download/v9.9.9/' in url, url\n"
            "    if cockpit_mode == 'missing': sys.exit(22)\n"
            "    pathlib.Path(os.environ['FAKE_COCKPIT_STATE']).write_text(url.rsplit('/', 1)[1])\n"
            "    shutil.copyfile(cockpit, out)\n"
            "elif url.endswith('/COCKPIT_SHA256SUMS'):\n"
            "    name = pathlib.Path(os.environ['FAKE_COCKPIT_STATE']).read_text()\n"
            "    digest = hashlib.sha256(cockpit.read_bytes()).hexdigest()\n"
            "    if cockpit_mode == 'mismatch': digest = '0' * 64\n"
            "    pathlib.Path(out).write_text(f'{digest}  lean-ctx-other.tar.gz\\n{digest}  {name}\\n')\n"
            "elif url.endswith('.tar.gz'):\n"
            "    state.write_text(url.rsplit('/', 1)[1])\n"
            "    shutil.copyfile(tarball, out)\n"
            "elif url.endswith('SHA256SUMS'):\n"
            "    mode = os.environ['FAKE_SUMS']\n"
            "    if mode == 'missing': sys.exit(22)\n"
            "    name = state.read_text() if mode == 'good' else 'lean-ctx-other.tar.gz'\n"
            "    digest = hashlib.sha256(tarball.read_bytes()).hexdigest()\n"
            "    pathlib.Path(out).write_text(f'{digest}  {name}\\n')\n"
            "else: sys.exit(22)\n",
            encoding="utf-8",
        )
        curl.chmod(0o755)
        self.env = {k: v for k, v in os.environ.items() if k not in ("HOME",)}
        self.env.update({
            "PATH": os.pathsep.join((str(self.bin_dir), str(self.install_dir), os.defpath)),
            "LEAN_CTX_INSTALL_DIR": str(self.install_dir),
            "LEAN_CTX_NO_ONBOARD": "1",
            "FAKE_STATE": str(self.directory / "asset-name"),
            "FAKE_TARBALL": str(self.tarball),
            "FAKE_COCKPIT_TARBALL": str(self.cockpit),
            "FAKE_COCKPIT_STATE": str(self.directory / "cockpit-asset-name"),
            "FAKE_ENGINE_LOG": str(self.engine_log),
            "FAKE_SUMS": "good",
        })
        self.script = Path(__file__).resolve().parents[2] / "install.sh"

    def run_download(self, **env):
        return subprocess.run(
            ["sh", str(self.script), "--download"], cwd=self.directory, env={**self.env, **env},
            capture_output=True, text=True, timeout=30, check=False,
        )

    def test_verified_cockpit_bundle_is_handed_to_the_engine(self):
        result = self.run_download()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Cockpit: installed and verified", result.stdout)
        self.assertEqual(self.installed.read_text(), ENGINE_SCRIPT)
        calls = self.engine_log.read_text().splitlines()
        self.assertEqual(len(calls), 1)
        verb, flag, bundle = calls[0].split(" ", 2)
        self.assertEqual((verb, flag), ("cockpit-install", "--from"))
        self.assertTrue(bundle.endswith("/lean-ctx-cockpit"), bundle)
        asset = (self.directory / "cockpit-asset-name").read_text()
        self.assertRegex(asset, r"^lean-ctx-cockpit-(x86_64|aarch64)-(apple-darwin|unknown-linux-(gnu|musl))\.tar\.gz$")
        self.assertNotIn("-cuda", asset)

    def test_cockpit_failures_never_fail_the_engine_install(self):
        for env, message in (
            ({"FAKE_COCKPIT": "mismatch"}, "checksum missing or mismatched"),
            ({"FAKE_COCKPIT": "missing"}, "download failed"),
            ({"FAKE_ENGINE_COCKPIT_EXIT": "1"}, "not installed (refused: bad bundle)"),
            ({"LEAN_CTX_NO_COCKPIT": "1"}, "skipped (LEAN_CTX_NO_COCKPIT=1)"),
        ):
            with self.subTest(env=env):
                self.engine_log.unlink(missing_ok=True)
                self.installed.write_text("old binary", encoding="utf-8")
                result = self.run_download(**env)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(self.installed.read_text(), ENGINE_SCRIPT)
                self.assertIn(message, result.stdout + result.stderr)
                if "FAKE_ENGINE_COCKPIT_EXIT" not in env:
                    self.assertFalse(self.engine_log.exists(), "unverified bundles never reach the engine")

    def test_missing_or_incomplete_checksums_abort_before_install(self):
        for mode, code in (("missing", 1), ("no-entry", 1), ("good", 0)):
            with self.subTest(sums=mode):
                self.installed.write_text("old binary", encoding="utf-8")
                result = self.run_download(FAKE_SUMS=mode)
                self.assertEqual(result.returncode, code, result.stdout + result.stderr)
                expected = ENGINE_SCRIPT if code == 0 else "old binary"
                self.assertEqual(self.installed.read_text(), expected)
                if code:
                    # GHSA-rxv5-4c3x-g4q6 D wording: "refusing to install an
                    # unverified binary" / "has no unique entry …; refusing to install".
                    self.assertIn("refusing to install", result.stderr.lower())


class FallbackUninstallTests(unittest.TestCase):
    """`install.sh --uninstall` when lean-ctx is not on PATH (sandbox HOME)."""

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)
        self.home = self.directory / "home"
        self.install_dir = self.directory / "installed"
        self.install_dir.mkdir(parents=True)
        self.ours = self.install_dir / "lean-ctx"
        self.ours.write_text("engine", encoding="utf-8")
        self.dirs = [self.home / p for p in (
            ".lean-ctx", ".config/lean-ctx", ".local/share/lean-ctx/cockpit-runtime", ".local/state/lean-ctx")]
        for path in self.dirs:
            path.mkdir(parents=True)
            (path / "file").write_text("x", encoding="utf-8")
        self.system_bin = self.directory / "usr-local-bin" / "lean-ctx"
        self.system_bin.parent.mkdir()
        self.script = Path(__file__).resolve().parents[2] / "install.sh"

    def uninstall(self, *args):
        env = {"HOME": str(self.home), "PATH": os.defpath, "LEAN_CTX_INSTALL_DIR": str(self.install_dir),
               "LEAN_CTX_SYSTEM_BIN": str(self.system_bin)}
        result = subprocess.run(["sh", str(self.script), "--uninstall", *args], env=env,
                                capture_output=True, text=True, timeout=30, check=False)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result.stdout

    def test_dry_run_lists_everything_and_removes_nothing(self):
        self.system_bin.symlink_to(self.ours)
        out = self.uninstall("--dry-run")
        for path in [*self.dirs[:2], self.dirs[2].parent, self.dirs[3], self.system_bin, self.ours]:
            self.assertIn(f"Would remove {path}", out)
            self.assertTrue(path.exists() or path.is_symlink(), path)
        self.assertIn("Dry run: nothing was removed.", out)

    def test_removes_config_data_state_and_our_binaries(self):
        self.system_bin.symlink_to(self.ours)
        self.uninstall()
        for path in [*self.dirs, self.ours]:
            self.assertFalse(path.exists(), path)
        self.assertFalse(self.system_bin.is_symlink())

    def test_a_same_file_copy_counts_as_ours(self):
        shutil.copyfile(self.ours, self.system_bin)
        self.uninstall()
        self.assertFalse(self.system_bin.exists())

    def test_another_installs_binary_stays(self):
        for make in (lambda: self.system_bin.write_text("homebrew engine", encoding="utf-8"),
                     lambda: self.system_bin.symlink_to(self.directory / "Cellar-lean-ctx")):
            with self.subTest(make=make):
                if self.system_bin.is_symlink() or self.system_bin.exists():
                    self.system_bin.unlink()
                self.ours.write_text("engine", encoding="utf-8")
                make()
                out = self.uninstall()
                self.assertIn(f"Kept {self.system_bin} (not installed by this script)", out)
                self.assertTrue(self.system_bin.is_symlink() or self.system_bin.exists())
                self.assertFalse(self.ours.exists())
