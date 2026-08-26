import importlib.util
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/check-no-internal-artifacts.py"
SPEC = importlib.util.spec_from_file_location("check_no_internal_artifacts", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
GUARD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GUARD)


class NoInternalArtifactsTests(unittest.TestCase):
    def test_directory_violation(self):
        self.assertTrue(GUARD.is_forbidden("docs/internal/strategy.md"))

    def test_exact_zip_violation(self):
        self.assertTrue(GUARD.is_forbidden("docs/internal.zip"))

    def test_suffixed_zip_violation(self):
        self.assertTrue(GUARD.is_forbidden("docs/internal-backup-1.zip"))

    def test_known_internal_archive_root_violation(self):
        self.assertTrue(GUARD.is_forbidden("docs/archive/strategy.md"))

    def test_unrelated_zip_is_allowed(self):
        self.assertFalse(GUARD.is_forbidden("docs/releases/lean-ctx.zip"))

    def test_current_tracked_tree_is_clean(self):
        self.assertEqual([], GUARD.find_forbidden_tracked_paths(ROOT))

    def test_guard_is_mandatory_in_lightweight_ci(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        command = "python3 scripts/check-no-internal-artifacts.py"
        self.assertEqual(1, workflow.count(command))
        job_start = workflow.index("  narrative-governance:")
        job_end = workflow.index("\n  package-assets:", job_start)
        job = workflow[job_start:job_end]
        self.assertIn(command, job)
        self.assertNotIn("continue-on-error", job)


if __name__ == "__main__":
    unittest.main()
