"""Behavior tests for public narrative-governance enforcement."""

from __future__ import annotations

import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/check-narrative-governance.py"
SPEC = importlib.util.spec_from_file_location("narrative_governance", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
GOVERNANCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GOVERNANCE)


PRODUCT = {
    "name": "LeanCTX",
    "category": "Context Gateway for AI Systems",
    "promise": "Control what your AI can see.",
    "components": ["LeanCTX Engine", "LeanCTX SDK"],
    "primary_story": ["Select", "Control", "Prove"],
}
PRIMARY = [
    "README.md",
    "VISION.md",
    "ARCHITECTURE.md",
    "docs/README.md",
    "docs/reference/README.md",
    "docs/guides/README.md",
    "docs/POSITIONING_CANONICAL.md",
    "docs/what-is-leanctx.md",
    "docs/where-leanctx-fits.md",
]


def valid_contract() -> dict[str, object]:
    return {
        "schema_version": 1,
        "product": PRODUCT,
        "primary_entrypoints": PRIMARY,
        "component_story_surfaces": [
            "docs/POSITIONING_CANONICAL.md",
            "docs/what-is-leanctx.md",
        ],
        "required_text": {"README.md": ["Get started", "Real-world scenarios"]},
        "forbidden_text": {},
        "status_guarded_records": [],
        "feature_statuses": {"ContextKits": "Research"},
        "canonical_reference": "docs/POSITIONING_CANONICAL.md",
        "historical_release_logs": ["CHANGELOG.md"],
        "discovery": {
            "entrypoint_indexes": [
                "README.md",
                "docs/README.md",
                "docs/reference/README.md",
                "llms.txt",
            ],
            "readme_globs": ["README.md", "**/README.md"],
            "metadata_globs": ["**/package.json", "**/manifest.json", "**/Cargo.toml", "**/PKGBUILD"],
            "excluded_prefixes": ["_archive", "vendor", "test-data"],
        },
        "legacy_definitions": [
            "AI Value Gate",
            "Context SDK for AI Agents",
            "Context OS",
            "Context Engineering Layer",
            "Cognitive Context Layer",
            "Context Intelligence for AI Systems",
        ],
        "scoped_technical_heading_exceptions": [
            {
                "path": "docs/reference/appendix-paths-and-config.md",
                "term": "AI Value Gate",
                "heading": "AI Value Gate configuration",
            }
        ],
        "claim_evidence_terms": [
            "workload",
            "baseline",
            "treatment",
            "methodology",
            "quality threshold",
            "version/date",
        ],
        "unsupported_claims": [
            {"name": "unscoped numerical savings", "pattern": r"(?:\b(?:saves?|saving|savings of)\s+(?:about\s+|up to\s+|~)?[0-9]+(?:\.[0-9]+)?\s*%|(?<![0-9])[0-9]+(?:\.[0-9]+)?\s*%\s+(?:savings|fewer tokens|less context|token reduction))"},
            {"name": "60–90%", "pattern": r"(?<![0-9])60\s*[-–—]\s*90\s*(?:%|percent)(?![A-Za-z])"},
            {"name": "5–10x", "pattern": r"(?<![0-9])5\s*[-–—]\s*10\s*[x×](?![A-Za-z])"},
            {"name": "nothing ever lost", "pattern": r"\bnothing\s+(?:(?:is|will be)\s+)?ever\s+lost\b"},
            {"name": "zero telemetry", "pattern": r"\bzero\s+telemetry\b"},
        ],
    }


def write_contract(root: Path, contract: dict[str, object] | None = None) -> None:
    path = root / "docs/contracts/public-product-claims-v1.md"
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = json.dumps(contract or valid_contract(), indent=2)
    path.write_text(
        "# Fixture contract\n\nStatus: Active.\n\n"
        f"```json narrative-governance-contract\n{payload}\n```\n",
        encoding="utf-8",
    )


def write_fixture(root: Path) -> None:
    write_contract(root)
    for relative_path in PRIMARY:
        path = root / relative_path
        path.parent.mkdir(parents=True, exist_ok=True)
        lines = [
            f"# {PRODUCT['name']}",
            "",
            f"**{PRODUCT['category']}**",
            "",
            f"**{PRODUCT['promise']}**",
            "",
            "Get started with LeanCTX. Real-world scenarios are documented here.",
        ]
        if relative_path in {
            "docs/POSITIONING_CANONICAL.md",
            "docs/what-is-leanctx.md",
        }:
            lines.extend(
                [
                    "",
                    "LeanCTX Engine and LeanCTX SDK.",
                    "",
                    "Select → Control → Prove.",
                ]
            )
        path.write_text("\n".join(lines) + "\n", encoding="utf-8")

    (root / "README.md").write_text(
        (root / "README.md").read_text(encoding="utf-8")
        + "\n[Documentation](docs/README.md)\n",
        encoding="utf-8",
    )
    docs_index = root / "docs/README.md"
    docs_index.write_text(
        docs_index.read_text(encoding="utf-8") + "\n[Guides](../new-guide/README.md)\n",
        encoding="utf-8",
    )
    reference_index = root / "docs/reference/README.md"
    reference_index.write_text(
        reference_index.read_text(encoding="utf-8")
        + "\n[Config reference](appendix-paths-and-config.md)\n",
        encoding="utf-8",
    )
    config_reference = root / "docs/reference/appendix-paths-and-config.md"
    config_reference.write_text(
        "# Configuration reference\n\n"
        "This technical reference describes supported configuration fields.\n\n"
        "## AI Value Gate configuration\n\n"
        "The heading names a scoped existing config implementation.\n",
        encoding="utf-8",
    )
    (root / "new-guide/README.md").parent.mkdir(parents=True, exist_ok=True)
    (root / "new-guide/README.md").write_text(
        "# Guide\n\nA scoped guide for using LeanCTX.\n", encoding="utf-8"
    )
    (root / "llms.txt").write_text("LeanCTX public documentation index.\n", encoding="utf-8")

    generated_tools = root / "docs/reference/generated/mcp-tools.md"
    generated_tools.parent.mkdir(parents=True, exist_ok=True)
    generated_tools.write_text(
        "ctx_delta avoids resending unchanged content.\n", encoding="utf-8"
    )
    package = root / "packages/new-tool/package.json"
    package.parent.mkdir(parents=True, exist_ok=True)
    package.write_text(
        json.dumps({"name": "new-tool", "description": "A LeanCTX integration."}),
        encoding="utf-8",
    )


class NarrativeGovernanceTests(unittest.TestCase):
    def run_archived_guard(self, mutate=None) -> subprocess.CompletedProcess[str]:
        tree = subprocess.check_output(["git", "write-tree"], cwd=ROOT, text=True).strip()
        archive = subprocess.check_output(
            ["git", "archive", "--format=tar", tree], cwd=ROOT
        )

        with tempfile.TemporaryDirectory() as temporary_directory:
            archive_root = Path(temporary_directory).resolve()
            with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
                for member in tar.getmembers():
                    destination = (archive_root / member.name).resolve()
                    self.assertFalse(member.issym() or member.islnk())
                    self.assertTrue(
                        destination == archive_root or archive_root in destination.parents
                    )
                tar.extractall(archive_root)
            if mutate is not None:
                mutate(archive_root)
            return subprocess.run(
                ["python3", str(archive_root / "scripts/check-narrative-governance.py")],
                cwd=archive_root,
                check=False,
                capture_output=True,
                text=True,
            )

    def test_clean_archive_contains_current_checker_and_contract(self) -> None:
        result = self.run_archived_guard()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Narrative governance passed.", result.stdout)

    def test_forbidden_public_claim_fails_closed(self) -> None:
        def add_stale_claim(archive_root: Path) -> None:
            readme = archive_root / "README.md"
            readme.write_text(
                readme.read_text(encoding="utf-8") + "\n83 MCP tools\n",
                encoding="utf-8",
            )

        result = self.run_archived_guard(add_stale_claim)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("forbidden public claim '83 MCP tools'", result.stderr)
    def test_clean_worktree_archive_contains_current_checker_and_contract(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            index_path = Path(temporary_directory) / "index"
            env = {**os.environ, "GIT_INDEX_FILE": str(index_path)}
            subprocess.run(["git", "read-tree", "HEAD"], cwd=ROOT, env=env, check=True)
            subprocess.run(
                [
                    "git",
                    "add",
                    "--all",
                    "--",
                    ".",
                    ":(exclude,glob)**/.k*",
                    ":(exclude,glob)**/TASK*",
                ],
                cwd=ROOT,
                env=env,
                check=True,
            )
            tree = subprocess.check_output(
                ["git", "write-tree"], cwd=ROOT, env=env, text=True
            ).strip()
            archive = subprocess.check_output(
                ["git", "archive", "--format=tar", tree], cwd=ROOT
            )

            with tempfile.TemporaryDirectory() as archive_directory:
                archive_root = Path(archive_directory).resolve()
                with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
                    for member in tar.getmembers():
                        destination = (archive_root / member.name).resolve()
                        self.assertFalse(member.issym() or member.islnk())
                        self.assertTrue(
                            destination == archive_root or archive_root in destination.parents
                        )
                    tar.extractall(archive_root)

                result = subprocess.run(
                    ["python3", str(Path("scripts/check-narrative-governance.py"))],
                    cwd=archive_root,
                    check=False,
                    capture_output=True,
                    text=True,
                )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Narrative governance passed.", result.stdout)

    def test_primary_page_category_and_promise_are_required(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            write_fixture(root)
            page = root / "docs/reference/README.md"
            page.write_text(
                page.read_text(encoding="utf-8").replace(PRODUCT["category"], "Agent toolkit"),
                encoding="utf-8",
            )

            failures = GOVERNANCE.check_repository(root)

        self.assertTrue(
            any("docs/reference/README.md: missing required current product category" in item for item in failures),
            failures,
        )

    def test_new_readme_and_metadata_surfaces_are_scanned(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            write_fixture(root)
            new_page = root / "docs/new-positioning/README.md"
            new_page.parent.mkdir(parents=True, exist_ok=True)
            new_page.write_text("# Context OS\n\nA new product entry point.\n", encoding="utf-8")
            package = root / "packages/new-tool/package.json"
            package.write_text(
                json.dumps({"name": "new-tool", "description": "Context SDK for AI Agents."}),
                encoding="utf-8",
            )

            failures = GOVERNANCE.check_repository(root)

        self.assertTrue(any("docs/new-positioning/README.md" in item and "Context OS" in item for item in failures), failures)
        self.assertTrue(any("packages/new-tool/package.json" in item and "Context SDK for AI Agents" in item for item in failures), failures)

    def test_primary_identity_cannot_be_buried_below_intro(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            write_fixture(root)
            page = root / "README.md"
            original = page.read_text(encoding="utf-8")
            page.write_text("# LeanCTX\n\n" + ("Implementation detail. " * 100) + original, encoding="utf-8")
            failures = GOVERNANCE.check_repository(root)
        self.assertTrue(any("product category in opening copy" in item for item in failures), failures)
        self.assertTrue(any("product promise in opening copy" in item for item in failures), failures)

    def test_aur_package_description_is_scanned_without_execution(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            write_fixture(root)
            package = root / "aur/lean-ctx/PKGBUILD"
            package.parent.mkdir(parents=True)
            package.write_text(
                'pkgdesc="Context Engineering Layer for AI Coding"\n'
                'exit 99\n', encoding="utf-8",
            )
            failures = GOVERNANCE.check_repository(root)
            self.assertTrue(any("PKGBUILD" in item and "Context Engineering Layer" in item for item in failures), failures)
            package.write_text('pkgdesc="LeanCTX Engine — Context Gateway for AI Systems"\nexit 99\n', encoding="utf-8")
            self.assertEqual(GOVERNANCE.check_repository(root), [])
            for value in ['$description', '"$(print-category)"', '"${description}"', '"`print-category`"']:
                package.write_text('pkgdesc=' + value + '\n', encoding="utf-8")
                failures = GOVERNANCE.check_repository(root)
                self.assertTrue(any("PKGBUILD" in item and "cannot be parsed" in item for item in failures), failures)

    def test_hidden_identity_does_not_satisfy_primary_copy(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            write_fixture(root)
            page = root / "README.md"
            original = page.read_text(encoding="utf-8")
            for hidden in ["```text\n" + original + "\n```", "<!--\n" + original + "\n-->"]:
                page.write_text("# LeanCTX\n\nA context utility.\n\n" + hidden, encoding="utf-8")
                failures = GOVERNANCE.check_repository(root)
                self.assertTrue(any("product category in opening copy" in item for item in failures), failures)
                self.assertTrue(any("product promise in opening copy" in item for item in failures), failures)

    def test_new_savings_percentage_requires_scoped_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            write_fixture(root)
            page = root / "README.md"
            original = page.read_text(encoding="utf-8")
            page.write_text(original + "\n## Outcomes\n\nLeanCTX delivers 99.4% savings.\n", encoding="utf-8")
            failures = GOVERNANCE.check_repository(root)
            self.assertTrue(any("unscoped numerical savings" in item for item in failures), failures)
            page.write_text(original + "\nworkload=fixture; baseline=plain; treatment=LeanCTX; methodology=replay; quality threshold=preserved; version/date=2026-10-02; measured 99.4% savings.\n", encoding="utf-8")
            self.assertEqual(GOVERNANCE.check_repository(root), [])

    def test_versioned_history_is_retained_but_unreleased_claims_are_checked(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            write_fixture(root)
            page = root / "CHANGELOG.md"
            history = "# Changelog\n\n[Current positioning](docs/POSITIONING_CANONICAL.md)\n\n## [Unreleased]\n\nMaintenance.\n\n## [1.0.0]\n\nLeanCTX saves 60–90% tokens.\n"
            page.write_text(history, encoding="utf-8")
            index = root / "README.md"
            index.write_text(index.read_text(encoding="utf-8") + "\n[Changelog](CHANGELOG.md)\n", encoding="utf-8")
            self.assertEqual(GOVERNANCE.check_repository(root), [])
            page.write_text(history.replace("Maintenance.", "LeanCTX saves 60–90% tokens."), encoding="utf-8")
            failures = GOVERNANCE.check_repository(root)
            self.assertTrue(any("CHANGELOG.md" in item and "unsupported blanket claim" in item for item in failures), failures)

    def test_later_product_redefinition_fails_but_scoped_discussion_passes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            write_fixture(root)
            page = root / "README.md"
            original = page.read_text(encoding="utf-8")
            page.write_text(original + "\n## Roadmap\n\nLeanCTX is growing from a context layer into a full **cognitive context\nlayer**.\n", encoding="utf-8")
            failures = GOVERNANCE.check_repository(root)
            self.assertTrue(any("explicit current product redefinition" in item for item in failures), failures)
            page.write_text(original + "\n## Boundaries\n\nLeanCTX is not a Context OS. The historical Cognitive Context Layer proposal is background only.\n\n```text\nLeanCTX is a Context OS.\n```\n", encoding="utf-8")
            self.assertEqual(GOVERNANCE.check_repository(root), [])

    def test_historical_mentions_need_status_and_current_canonical_link(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            write_fixture(root)
            history = root / "docs/history/README.md"
            history.parent.mkdir(parents=True, exist_ok=True)
            history.write_text(
                "Status: Historical — superseded by [current positioning](../POSITIONING_CANONICAL.md).\n\n"
                "# Context OS\n\nThis record describes the retired name.\n",
                encoding="utf-8",
            )
            docs_index = root / "docs/README.md"
            docs_index.write_text(
                docs_index.read_text(encoding="utf-8") + "\n[History](history/README.md)\n",
                encoding="utf-8",
            )

            self.assertEqual(GOVERNANCE.check_repository(root), [])

            history.write_text(
                "Status: Historical.\n\n# Context OS\n\nThis record describes the retired name.\n",
                encoding="utf-8",
            )
            failures = GOVERNANCE.check_repository(root)

        self.assertTrue(any("docs/history/README.md" in item and "Context OS" in item for item in failures), failures)

    def test_tagline_does_not_hide_wrapped_primary_definition(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            write_fixture(root)
            page = root / "new-guide/README.md"
            page.write_text(
                "# LeanCTX\n\n### Control what your AI can see.\n\n"
                "> LeanCTX is the Context SDK for AI\n> Agents.\n\n## Install\n",
                encoding="utf-8",
            )
            failures = GOVERNANCE.check_repository(root)
        self.assertTrue(any("new-guide/README.md" in item and "Context SDK" in item for item in failures), failures)

    def test_blockquote_historical_banner_allows_old_title(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            write_fixture(root)
            page = root / "new-guide/README.md"
            page.write_text(
                "# Context Engineering Layer\n\n"
                "> **Status: Historical / Research.** Retained design record.\n"
                "> See [current positioning](../docs/POSITIONING_CANONICAL.md).\n",
                encoding="utf-8",
            )
            self.assertEqual(GOVERNANCE.check_repository(root), [])

    def test_scoped_feature_heading_exception_is_exact_and_path_specific(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            write_fixture(root)
            config_reference = root / "docs/reference/appendix-paths-and-config.md"
            self.assertEqual(GOVERNANCE.check_repository(root), [])

            config_reference.write_text(
                config_reference.read_text(encoding="utf-8").replace(
                    "## AI Value Gate configuration", "## AI Value Gate"
                ),
                encoding="utf-8",
            )
            failures = GOVERNANCE.check_repository(root)

        self.assertTrue(any("AI Value Gate" in item for item in failures), failures)

    def test_private_content_is_not_scanned_and_blanket_claim_needs_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            write_fixture(root)
            private = root / "docs/internal/README.md"
            private.parent.mkdir(parents=True, exist_ok=True)
            private.write_text("# Context OS\n\nInternal note.\n", encoding="utf-8")
            readme = root / "README.md"
            original = readme.read_text(encoding="utf-8")
            readme.write_text(original + "\n## Performance\n\nLeanCTX saves 60–90% tokens.\n", encoding="utf-8")

            failures = GOVERNANCE.check_repository(root)
            self.assertTrue(any("unsupported blanket claim" in item for item in failures), failures)
            self.assertFalse(any("docs/internal/README.md" in item for item in failures), failures)

            evidence = (
                "workload=fixture; baseline=plain; treatment=LeanCTX; methodology=replay; "
                "quality threshold=preserved; version/date=2026-10-02; measured 60–90% tokens."
            )
            readme.write_text(original + "\n" + evidence + "\n", encoding="utf-8")
            self.assertEqual(GOVERNANCE.check_repository(root), [])

    def test_invalid_contract_paths_fail_before_index_reads(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            write_fixture(root)
            contract = valid_contract()
            contract["discovery"]["entrypoint_indexes"] = ["docs/internal/secret.md"]
            write_contract(root, contract)
            secret = root / "docs/internal/secret.md"
            secret.parent.mkdir(parents=True, exist_ok=True)
            secret.write_text("must not be read", encoding="utf-8")

            failures = GOVERNANCE.check_repository(root)

        self.assertTrue(any("discovery.entrypoint_indexes" in item for item in failures), failures)
        self.assertFalse(any("secret.md" in item for item in failures), failures)


if __name__ == "__main__":
    unittest.main()
