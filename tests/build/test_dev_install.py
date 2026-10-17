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
        self.installed.write_text("old binary", encoding="utf-8")
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
                "installed = pathlib.Path(os.environ['LEAN_CTX_INSTALL_DIR']) / 'lean-ctx'\n"
                "assert installed.read_text() == 'old binary'\n"
                "assert list(installed.parent.glob('.lean-ctx.new.*'))\n"
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
                self.assertEqual(self.installed.read_text(), "old binary")
                self.assertNotIn(["stop"], self.events())
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
                self.installed.write_text("old binary", encoding="utf-8")
                self.record.unlink(missing_ok=True)
                build_only = ["--build-only", *args]
                result = self.run_installer(build_only)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.installed.read_text(), "old binary")
                self.assertNotIn(["stop"], self.events())
                expected = ["fast"] if profile == "dev-fast" else ["build", "--locked", "--release", "--timings"]
                self.assertEqual(self.events()[0], expected)
                result = self.run_installer(args)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.events()[-1], ["stop"])
                self.assertEqual(self.installed.read_bytes(), binary.read_bytes())
                self.assertFalse(self.installed.is_symlink())
                binary.write_text("changed build artifact", encoding="utf-8")
                self.assertIn("fixture", self.installed.read_text())
