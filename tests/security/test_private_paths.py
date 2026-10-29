# SPDX-License-Identifier: Apache-2.0
import importlib.util
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/check-private-paths.py"
SPEC = importlib.util.spec_from_file_location("check_private_paths", SCRIPT)
CHECK = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = CHECK
SPEC.loader.exec_module(CHECK)

GITIGNORE = """# ── Build ──
target/

# ── Internal strategy docs (confidential) ──
/docs/internal/
/docs/internal*.zip
.codex/vision-input/

# ── Editors ──
.idea/
"""


class PrivatePathTests(unittest.TestCase):
    def test_current_repository_passes(self):
        self.assertEqual(CHECK.main(["check", str(ROOT)]), 0)

    def test_a_tracked_denied_file_fails_even_if_it_predates_the_rule(self):
        findings = CHECK.find_violations(
            ".codex/vision-input/\ndocs/internal/\n",
            GITIGNORE,
            ["README.md", ".codex/vision-input/02-VISION.md"],
        )
        self.assertEqual(
            findings,
            ["[tracked] .codex/vision-input/02-VISION.md is listed in .github-ignore but tracked"],
        )

    def test_an_internal_gitignore_entry_missing_from_the_deny_list_fails(self):
        findings = CHECK.find_violations("docs/internal/\n", GITIGNORE, [])
        self.assertEqual(
            findings,
            [
                "[drift] internal .gitignore entry '.codex/vision-input/' "
                "is missing from .github-ignore"
            ],
        )

    def test_other_sections_and_lookalike_paths_are_not_flagged(self):
        findings = CHECK.find_violations(
            ".codex/vision-input/\ndocs/internal/\n",
            GITIGNORE,
            ["docs/internal-notes.md", ".codex/vision-input-public.md", ".idea/x"],
        )
        self.assertEqual(findings, [])

    def test_paid_detection_content_in_a_public_path_fails(self):
        texts = {
            "rust/src/core/detect.rs": "// brought up with GLiNER small\n",
            "docs/guides/eval.md": "Recall on MAPA-de\n",
            "rust/src/core/serde.rs": "use serde::de::MapAccess;\n",
            "rust/src/core/compliance_evidence/checks.rs": '    "SOC2-CC6.6" => egress,\n',
        }
        findings = CHECK.paid_detection_findings(
            [
                "rust/data/pii.onnx",
                "rust/src/core/policy/builtin/legal-ch.toml",
                "rust/src/core/policy/builtin/healthcare.toml",
                "rust/src/core/policy/builtin/baseline.toml",
                "rust/data/compliance/mappings/soc2.toml",
                "rust/src/core/compliance.rs",
                *texts,
            ],
            texts.get,
        )
        self.assertEqual(
            findings,
            [
                "[paid] docs/guides/eval.md:1 names the commercial detection runtime ('MAPA')",
                "[paid] rust/data/compliance/mappings/soc2.toml is compliance-framework content; "
                "it belongs to LeanCTX Enterprise",
                "[paid] rust/data/pii.onnx is a model file; model packs ship signed and private",
                "[paid] rust/src/core/compliance.rs is compliance-framework content; "
                "it belongs to LeanCTX Enterprise",
                "[paid] rust/src/core/compliance_evidence/checks.rs:1 carries a compliance-framework "
                "mapping ('SOC2-CC6'); mappings belong to the Enterprise runtime",
                "[paid] rust/src/core/detect.rs:1 names the commercial detection runtime ('GLiNER')",
                "[paid] rust/src/core/policy/builtin/healthcare.toml is a built-in policy pack outside "
                "the community floor; domain and compliance packs are commercial "
                "(community keeps baseline, strict-redaction, open-source)",
                "[paid] rust/src/core/policy/builtin/legal-ch.toml is a built-in policy pack outside "
                "the community floor; domain and compliance packs are commercial "
                "(community keeps baseline, strict-redaction, open-source)",
            ],
        )


if __name__ == "__main__":
    unittest.main()
