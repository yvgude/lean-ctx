import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/verify-ocla-contract-suite.py"
SPEC = importlib.util.spec_from_file_location("ocla_contract_suite", SCRIPT)
SUITE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = SUITE
SPEC.loader.exec_module(SUITE)
FIXTURES = ROOT / "clients/rust/lean-ctx-client/tests/fixtures"


def verifier_binary() -> Path:
    configured = os.environ.get("OCLA_VERIFIER_BIN")
    if configured:
        return Path(configured).resolve()
    crate = ROOT / "clients/rust/lean-ctx-client"
    subprocess.run(
        ["cargo", "build", "--locked", "--bin", "lean-ctx-ocla-verify"],
        cwd=crate,
        check=True,
        timeout=120,
    )
    suffix = ".exe" if os.name == "nt" else ""
    return crate / "target/debug" / f"lean-ctx-ocla-verify{suffix}"


class OclaContractSuiteTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.verifier = verifier_binary()

    def invoke(self) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(SCRIPT), "--verifier", str(self.verifier)],
            cwd=ROOT,
            check=False,
            capture_output=True,
            text=True,
            timeout=60,
        )

    def test_reference_verifier_passes_without_certification_overclaim(self) -> None:
        result = self.invoke()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        report = json.loads(result.stdout)
        self.assertTrue(report["all_passed"])
        self.assertFalse(report["certification_claimed"])
        self.assertEqual(report["profile"], SUITE.PROFILE)
        self.assertEqual(len(report["results"]), 18)
        self.assertTrue(all(item["passed"] for item in report["results"]))

    def test_report_is_byte_deterministic(self) -> None:
        first = self.invoke()
        second = self.invoke()
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertEqual(first.stdout, second.stdout)

    def test_non_regular_verifier_fails_without_path_echo(self) -> None:
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--verifier", str(FIXTURES)],
            cwd=ROOT,
            check=False,
            capture_output=True,
            text=True,
            timeout=10,
        )
        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stdout, "")
        self.assertEqual(
            result.stderr,
            "OCLA contract suite failed: invalid_or_unsafe_input\n",
        )

    def test_output_budget_is_shared_and_fail_closed(self) -> None:
        output = SUITE.BoundedOutput()
        output.append("stdout", b"a" * SUITE.MAX_OUTPUT_BYTES)
        output.append("stderr", b"b")
        self.assertTrue(output.overflow.is_set())
        self.assertEqual(len(output.stdout), SUITE.MAX_OUTPUT_BYTES)
        self.assertEqual(output.stderr, b"")

    @unittest.skipUnless(
        os.name == "posix" and hasattr(os, "fork"),
        "POSIX process groups are unavailable",
    )
    def test_parent_exit_child_pipe_is_killed_within_deadline(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            verifier = root / "retained-pipe-verifier"
            verifier.write_text(
                "#!/usr/bin/env python3\n"
                "import os\n"
                "import time\n"
                "if os.fork() == 0:\n"
                "    time.sleep(30)\n"
                "    os._exit(0)\n"
                "os._exit(0)\n"
            )
            verifier.chmod(0o700)
            case = SUITE.Case("retained_pipe", "token", b"{}", 0)

            started = time.monotonic()
            result = SUITE.execute_case(verifier, case, root)
            elapsed = time.monotonic() - started

        self.assertEqual(result, {
            "case": "retained_pipe",
            "passed": False,
            "reason": "output_reader",
        })
        self.assertLess(elapsed, 4.0)

    @unittest.skipUnless(hasattr(os, "mkfifo"), "FIFO is unavailable")
    def test_fifo_fixture_is_rejected_without_open_block(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            os.mkfifo(root / "wire.json", 0o600)
            descriptor = SUITE.open_fixture_directory(root)
            try:
                with self.assertRaises(SUITE.SuiteError):
                    SUITE.read_fixture(root, descriptor, "wire.json")
            finally:
                if descriptor is not None:
                    os.close(descriptor)

    @unittest.skipUnless(hasattr(os, "symlink"), "symlink is unavailable")
    def test_verifier_symlink_is_rejected_without_following(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            link = Path(temporary) / "verifier"
            link.symlink_to(self.verifier)
            result = subprocess.run(
                [sys.executable, str(SCRIPT), "--verifier", str(link)],
                cwd=ROOT,
                check=False,
                capture_output=True,
                text=True,
                timeout=10,
            )
        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stdout, "")
        self.assertEqual(
            result.stderr,
            "OCLA contract suite failed: invalid_or_unsafe_input\n",
        )



if __name__ == "__main__":
    unittest.main()
