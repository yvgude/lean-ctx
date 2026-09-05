import base64
import csv
from fnmatch import fnmatch
import hashlib
import importlib.util
import io
from pathlib import Path
import stat
import tempfile
import unittest
import zipfile


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "build-python-engine-wheel.py"
SPEC = importlib.util.spec_from_file_location("engine_wheel", SCRIPT)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def record_digest(data: bytes) -> str:
    encoded = base64.urlsafe_b64encode(hashlib.sha256(data).digest())
    return "sha256=" + encoded.rstrip(b"=").decode("ascii")


class PythonEngineWheelTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.binary = self.root / "lean-ctx"
        self.binary.write_bytes(b"#!/bin/sh\nprintf 'lean-ctx 3.10.1\\n'\n")
        self.binary.chmod(0o755)
        self.license = self.root / "LICENSE"
        self.license.write_text("Apache License fixture\n", encoding="utf-8")

    def tearDown(self):
        self.temporary.cleanup()

    def build(self, output="first"):
        return MODULE.build_wheel(
            binary=self.binary,
            distribution="thinkery-leanctx-engine",
            version="3.10.1",
            platform_tag="manylinux_2_35_x86_64",
            output_dir=self.root / output,
            license_file=self.license,
        )

    def test_wheel_is_deterministic_and_record_is_exact(self):
        first = self.build("first")
        second = self.build("second")
        self.assertEqual(first.read_bytes(), second.read_bytes())
        with zipfile.ZipFile(first) as archive:
            names = archive.namelist()
            self.assertEqual(names, sorted(names))
            self.assertEqual(len(names), len(set(names)))
            record_name = next(name for name in names if name.endswith(".dist-info/RECORD"))
            rows = list(csv.reader(io.StringIO(archive.read(record_name).decode("utf-8"))))
            self.assertEqual({row[0] for row in rows}, set(names))
            for name, digest, size in rows:
                if name == record_name:
                    self.assertEqual((digest, size), ("", ""))
                else:
                    data = archive.read(name)
                    self.assertEqual(digest, record_digest(data))
                    self.assertEqual(size, str(len(data)))

            binary_name = "thinkery_leanctx_engine/bin/lean-ctx"
            mode = archive.getinfo(binary_name).external_attr >> 16
            self.assertTrue(mode & stat.S_IXUSR)
            self.assertIn(b"Version: 3.10.1", archive.read(
                "thinkery_leanctx_engine-3.10.1.dist-info/METADATA"
            ))
            self.assertIn(b"py3-none-manylinux_2_35_x86_64", archive.read(
                "thinkery_leanctx_engine-3.10.1.dist-info/WHEEL"
            ))

    def test_variants_use_distinct_import_packages(self):
        for distribution, package in (
            ("thinkery-leanctx-engine-cuda", "thinkery_leanctx_engine_cuda"),
            ("thinkery-leanctx-engine-windows-gnu", "thinkery_leanctx_engine_windows_gnu"),
        ):
            wheel = MODULE.build_wheel(
                binary=self.binary,
                distribution=distribution,
                version="3.10.1",
                platform_tag="win_amd64",
                output_dir=self.root / distribution,
                license_file=self.license,
            )
            with zipfile.ZipFile(wheel) as archive:
                self.assertIn(f"{package}/launcher.py", archive.namelist())
                self.assertIn(f"{package}/bin/lean-ctx.exe", archive.namelist())

    def test_invalid_inputs_fail_closed(self):
        cases = (
            {"distribution": "other", "version": "3.10.1", "platform_tag": "win_amd64"},
            {"distribution": "thinkery-leanctx-engine", "version": "latest", "platform_tag": "win_amd64"},
            {"distribution": "thinkery-leanctx-engine", "version": "3.10.1-rc.1", "platform_tag": "win_amd64"},
            {"distribution": "thinkery-leanctx-engine", "version": "3.10.1", "platform_tag": "any"},
            {"distribution": "thinkery-leanctx-engine", "version": "3.10.1", "platform_tag": "../../bad"},
        )
        for case in cases:
            with self.subTest(case=case), self.assertRaises(ValueError):
                MODULE.build_wheel(
                    binary=self.binary,
                    output_dir=self.root / "invalid",
                    license_file=self.license,
                    **case,
                )

        symlink = self.root / "linked"
        try:
            symlink.symlink_to(self.binary)
        except OSError:
            self.skipTest("symlinks unavailable")
        with self.assertRaises(ValueError):
            MODULE.build_wheel(
                binary=symlink,
                distribution="thinkery-leanctx-engine",
                version="3.10.1",
                platform_tag="manylinux_2_35_x86_64",
                output_dir=self.root / "invalid-symlink",
                license_file=self.license,
            )

    def test_release_smoke_handles_foreign_musllinux_tags(self):
        workflow = (ROOT / ".github" / "workflows" / "release.yml").read_text(
            encoding="utf-8"
        )
        self.assertNotIn("musl.cc", workflow)
        for expected in (
            'test "$(uname -m)" = "aarch64"',
            "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc",
            'if [[ "${{ matrix.musl }}" == "true" ]]',
            '--platform "${{ matrix.wheel_platform }}"',
            "--python-version 3 --implementation py --abi none",
            "-path '*/bin/lean-ctx' -type f -print -quit",
        ):
            with self.subTest(expected=expected):
                self.assertIn(expected, workflow)

    def test_release_fetches_tag_history_for_changelog_link(self):
        workflow = (ROOT / ".github" / "workflows" / "release.yml").read_text(
            encoding="utf-8"
        )
        release_job = workflow.split("  release:\n", 1)[1].split(
            "\n  jetbrains-plugin:", 1
        )[0]
        self.assertIn("fetch-depth: 0", release_job)
        self.assertIn("PREV_TAG=$(git tag --sort=-v:refname", release_job)

    def test_release_uses_one_trusted_publisher_per_distribution(self):
        workflow = (ROOT / ".github" / "workflows" / "release.yml").read_text(
            encoding="utf-8"
        )
        publish_job = workflow.split("  publish-python-engine:\n", 1)[1].split(
            "\n  update-homebrew:", 1
        )[0]
        publishers = (
            (
                "thinkery-leanctx-engine",
                "pypi-engine",
                "thinkery_leanctx_engine",
                7,
            ),
            (
                "thinkery-leanctx-engine-cuda",
                "pypi-engine-cuda",
                "thinkery_leanctx_engine_cuda",
                1,
            ),
            (
                "thinkery-leanctx-engine-windows-gnu",
                "pypi-engine-windows-gnu",
                "thinkery_leanctx_engine_windows_gnu",
                1,
            ),
        )
        wheel_names = {
            "thinkery_leanctx_engine-3.10.1-py3-none-win_amd64.whl",
            "thinkery_leanctx_engine_cuda-3.10.1-py3-none-manylinux.whl",
            "thinkery_leanctx_engine_windows_gnu-3.10.1-py3-none-win_amd64.whl",
        }
        for project, environment, prefix, expected_count in publishers:
            with self.subTest(project=project):
                contract = (
                    f"- project: {project}\n"
                    f"            environment: {environment}\n"
                    f"            wheel_prefix: {prefix}\n"
                    f"            expected_wheels: {expected_count}"
                )
                self.assertEqual(publish_job.count(contract), 1)
                matches = {
                    name for name in wheel_names if fnmatch(name, f"{prefix}-*.whl")
                }
                expected = {
                    name for name in wheel_names if name.startswith(f"{prefix}-")
                }
                self.assertEqual(matches, expected)

        self.assertEqual(publish_job.count("name: ${{ matrix.environment }}"), 1)
        self.assertEqual(publish_job.count('--pattern "${WHEEL_PREFIX}-*.whl"'), 1)
        self.assertEqual(publish_job.count("contents: read"), 1)
        self.assertEqual(publish_job.count("id-token: write"), 1)
        self.assertEqual(
            publish_job.count(
                "pypa/gh-action-pypi-publish@"
                "dc37677b2e1c63e2034f94d8a5b11f265b73ba33"
            ),
            1,
        )
        self.assertNotIn("PYPI_TOKEN", publish_job)
        self.assertNotIn("thinkery_leanctx_engine*.whl", publish_job)
        announce_needs = (
            "needs: [release, publish-crates, publish-npm, "
            "publish-python-engine, update-homebrew]"
        )
        self.assertEqual(workflow.count(announce_needs), 2)


if __name__ == "__main__":
    unittest.main()
