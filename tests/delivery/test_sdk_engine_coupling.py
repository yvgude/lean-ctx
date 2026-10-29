# SPDX-License-Identifier: Apache-2.0
"""The release gate refuses missing, mismatched or externally substituted SDKs."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("coupling", ROOT / "scripts/check-sdk-engine-coupling.py")
coupling = importlib.util.module_from_spec(spec)
spec.loader.exec_module(coupling)

TRIPLE = '{"agent_tools_interface_version": "1.0.0", "schema_version": 1, "transport_version": 1}'
ROW = ('{"agent_tools_interface_version": "%s", "schema_version": 1, "transport_version": 1,'
       ' "engine_version": "%s", %s}')
LEGACY = '"basis": "exact_legacy", "evidence": null'
PAIR = ROW % ("1.0.0", "3.11.0", LEGACY)


class CouplingTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.write("rust/Cargo.toml", '[package]\nname="test"\nversion="3.11.0"\n')
        self.write(coupling.SDK_CONSTANT_PATH, 'SUPPORTED_AGENT_TOOLS_ENGINE_VERSION = "3.11.0"\n')
        self.write(".github/workflows/release.yml",
                   'run: python scripts/verify-engine-agent-tools.py\n'
                   'run: PYTHONPATH=sdk/src python sdk/scripts/verify_agent_context_e2e.py\n')
        self.compat, validator = coupling._compatibility()
        self.write(coupling.SDK_VALIDATOR_COPY, validator.decode())
        self.write("sdk/" + self.compat.CONTRACT,
                   '{"version": "1.0.0", "schema_version": 1, "transport_version": 1,'
                   ' "engine_version": "3.11.0"}')
        self.matrix(PAIR)
        self.mirrors("3.11.0")

    def matrix(self, pairs, declared=TRIPLE):
        self.write("sdk/" + self.compat.MATRIX, (
            '{"contract": "leanctx-agent-tools-compatibility", "schema_version": 1,'
            ' "sdk_version": "1.2.0", "rule": "r",'
            ' "basis": {"exact_legacy": "l", "tested_pair": "t"},'
            ' "declared_interfaces": [%s], "pairs": [%s]}' % (declared, pairs)))

    def mirrors(self, engine, differing=None):
        for name, path in self.compat.MIRRORS.items():
            text = ('# agent-tools-compatibility: begin\n("1.0.0", 1, 1, "%s"),\n'
                    '# agent-tools-compatibility: end\n'
                    % ("3.11.1" if name == differing else engine))
            if "sdk/" + path == coupling.SDK_CONSTANT_PATH:
                text = 'SUPPORTED_AGENT_TOOLS_ENGINE_VERSION = "3.11.0"\n' + text
            self.write("sdk/" + path, text)

    def write(self, path, text):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)

    def test_current_checkout_and_matching_fixture(self):
        # The complete checkout couples its SDK; the Apache host export has none
        # and is reported as Engine-only. Either is a statement about ROOT itself.
        current = coupling.verify(ROOT)
        self.assertTrue(current["same_checkout"] or current.get("engine_only"))
        self.assertTrue(coupling.verify(self.root)["coupled"])

    def test_engine_only_checkout_needs_no_sdk_but_its_own_gate(self):
        import shutil
        shutil.rmtree(self.root / "sdk")
        self.assertTrue(coupling.verify(self.root)["engine_only"])
        # Commercial sources without the SDK: incomplete, not Engine-only.
        (self.root / "ee").mkdir()
        with self.assertRaises(FileNotFoundError):
            coupling.verify(self.root)
        (self.root / "ee").rmdir()
        # The workflow's rules hold without an SDK too.
        self.write(".github/workflows/release.yml",
                   'run: python scripts/verify-engine-agent-tools.py\nrepository: Thinkery-AG/leanctx-sdk\n')
        with self.assertRaisesRegex(ValueError, "external SDK"):
            coupling.verify(self.root)
        self.write(".github/workflows/release.yml", "run: PYTHONPATH=sdk/src python sdk/scripts/verify_agent_context_e2e.py\n")
        with self.assertRaisesRegex(ValueError, "own Agent Tools gate"):
            coupling.verify(self.root)

    def test_refuses_mismatched_engine(self):
        self.write(coupling.SDK_CONSTANT_PATH, 'SUPPORTED_AGENT_TOOLS_ENGINE_VERSION = "3.10.5"\n')
        with self.assertRaisesRegex(ValueError, "requires Engine"):
            coupling.verify(self.root)

    def test_only_the_whole_built_pair_in_a_valid_mirrored_matrix_couples(self):
        tested = ('"basis": "tested_pair", "evidence": {"report": "r.json", "sha256": "%s"}'
                  % ("a" * 64))
        refused = {
            # The Engine version appears in a row, but for another interface:
            # the pair that is built (1.0.0, 1, 1, 3.11.0) is not a row.
            "own pair": (ROW % ("1.0.0", "3.10.5", tested) + ", "
                           + ROW % ("2.0.0", "3.11.0", tested),
                           TRIPLE + ", " + TRIPLE.replace("1.0.0", "2.0.0")),
            "undeclared interface": (ROW % ("2.0.0", "3.11.0", LEGACY), TRIPLE),
            # A legacy row claims no run; a tested row has to name its report.
            "no valid basis": (ROW % ("1.0.0", "3.11.0", tested.replace("a" * 64, "abc")),
                               TRIPLE),
            "no valid basis ": (ROW % ("1.0.0", "3.11.0",
                                       LEGACY.replace("null", '{"report": "r", "sha256": "x"}')),
                                TRIPLE),
            # Legacy is the one historical pair. Another Engine or another
            # interface without evidence is not legacy — also when the old
            # row and every mirror are kept.
            "'3.11.1'\\) states no valid basis": (
                PAIR + ", " + ROW % ("1.0.0", "3.11.1", LEGACY), TRIPLE),
            "\\('2.0.0', 1, 1, '3.11.0'\\) states no valid basis": (
                PAIR + ", " + ROW % ("2.0.0", "3.11.0", LEGACY),
                TRIPLE + ", " + TRIPLE.replace("1.0.0", "2.0.0")),
            "wrong fields": (PAIR.replace('"evidence": null', '"evidence": null, "note": 1'),
                             TRIPLE),
            "not a version triple": (PAIR.replace('"schema_version": 1', '"schema_version": 1.0'),
                                     TRIPLE),
            "given twice": (PAIR.replace('"engine_version": "3.11.0"',
                                         '"engine_version": "9.9.9", "engine_version": "3.11.0"'),
                            TRIPLE),
            "listed twice": (PAIR + ", " + PAIR, TRIPLE),
        }
        for message, (pairs, declared) in refused.items():
            with self.subTest(message=message):
                self.matrix(pairs, declared)
                with self.assertRaisesRegex(ValueError, message.strip()):
                    coupling.verify(self.root)
        # A well-formed tested row couples: its form is all this gate reads.
        self.matrix(ROW % ("1.0.0", "3.11.0", tested))
        self.assertEqual(coupling.verify(self.root)["sdk_built_pair"], ["1.0.0", 1, 1, "3.11.0"])
        # One package accepts a pair the matrix does not list.
        self.matrix(PAIR)
        self.mirrors("3.11.0", differing="go")
        with self.assertRaisesRegex(ValueError, "mirror of go differs"):
            coupling.verify(self.root)
        # The SDK's copy of the validator is not the gate's.
        self.mirrors("3.11.0")
        self.write(coupling.SDK_VALIDATOR_COPY, "def check(*_, **__):\n    return {}\n")
        with self.assertRaisesRegex(ValueError, "validator differs"):
            coupling.verify(self.root)

    def test_missing_sdk_is_failure(self):
        (self.root / coupling.SDK_CONSTANT_PATH).unlink()
        with self.assertRaises(FileNotFoundError):
            coupling.verify(self.root)

    def test_external_checkout_is_refused(self):
        self.write(".github/workflows/release.yml",
                   'run: python scripts/verify-engine-agent-tools.py\nrepository: Thinkery-AG/leanctx-sdk\n')
        with self.assertRaisesRegex(ValueError, "external SDK"):
            coupling.verify(self.root)

    def test_ambiguous_constant_is_refused(self):
        self.write(coupling.SDK_CONSTANT_PATH, 'SUPPORTED_AGENT_TOOLS_ENGINE_VERSION = "3.11.0"\n' * 2)
        with self.assertRaisesRegex(ValueError, "exactly one"):
            coupling.verify(self.root)


if __name__ == "__main__":
    unittest.main()
