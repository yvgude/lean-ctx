# SPDX-License-Identifier: Apache-2.0
import importlib.util
import os
import io
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/check-no-internal-artifacts.py"
SPEC = importlib.util.spec_from_file_location("check_no_internal_artifacts", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
GUARD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GUARD)

POLICY = "cloud/\nLICENSE_MATRIX.toml\n"


def make_repo(case, policy=POLICY, files=()):
    """A real, bounded temporary Git repository with a .github-ignore policy."""
    temp = tempfile.TemporaryDirectory(prefix="boundary-guard-")
    case.addCleanup(temp.cleanup)
    root = Path(temp.name)
    git = ["git", "-c", "core.hooksPath=" + os.devnull, "-c", "commit.gpgsign=false",
           "-c", "init.templateDir="]
    subprocess.run([*git, "init", "-q", str(root)], check=True)
    for name, body in files:
        target = root / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(body, encoding="utf-8")
    if policy is not None:
        (root / ".github-ignore").write_text(policy, encoding="utf-8")
    subprocess.run([*git, "-C", str(root), "add", "-A"], check=True)
    subprocess.run(
        [*git, "-C", str(root), "-c", "user.email=t@example.com", "-c", "user.name=t",
         "commit", "-q", "--allow-empty", "-m", "fixture"],
        check=True,
    )
    return root


class NoInternalArtifactsTests(unittest.TestCase):
    """Default internal-only behavior — unchanged."""

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

    def test_private_receipt_families_are_forbidden(self):
        for path in (
            "docs/contracts/v4-any-future-receipt.md",
            "docs/contracts/V4-UPPER.md",
            "docs/architecture/V4_AGENT_BUS_PREMIUM_CONCEPT_2026-09-11.md",
            "docs/architecture/v4_lower.txt",
        ):
            self.assertTrue(GUARD.is_forbidden(path), path)

    def test_public_v4_documents_stay_allowed(self):
        for path in (
            "docs/architecture/v4-overview.md",
            "docs/licensing/v4-licensing.md",
            "docs/contracts/v4-nested/readme.md",
            "docs/contracts/team-context-v1.md",
        ):
            self.assertFalse(GUARD.is_forbidden(path), path)

    def test_stdin_paths_mode_reports_only_forbidden_entries(self):
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--stdin-paths"],
            input=b"README.md\ndocs/contracts/v4-new.md\0docs/internal/x.md\n",
            capture_output=True,
            check=False,
        )
        self.assertEqual(1, result.returncode)
        self.assertIn(b"docs/contracts/v4-new.md", result.stderr)
        self.assertIn(b"docs/internal/x.md", result.stderr)
        self.assertNotIn(b"README.md", result.stderr)

    def test_stdin_paths_mode_passes_clean_list(self):
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--stdin-paths"],
            input=b"README.md\ndocs/architecture/v4-overview.md\n",
            capture_output=True,
            check=False,
        )
        self.assertEqual(0, result.returncode, result.stderr)

    def test_pre_push_hook_runs_pattern_guard_for_github(self):
        hook = (ROOT / ".githooks/pre-push").read_text(encoding="utf-8")
        self.assertIn('scripts/check-no-internal-artifacts.py" --stdin-paths', hook)

    def test_pre_push_hook_is_tracked_executable(self):
        # Git silently skips a non-executable hook, which disables the guard.
        entry = subprocess.run(
            ["git", "-C", str(ROOT), "ls-files", "-s", "--", ".githooks/pre-push"],
            capture_output=True, text=True, check=True,
        ).stdout
        self.assertTrue(entry.startswith("100755 "), entry)

    def test_legacy_hook_delegates_to_canonical_hook(self):
        legacy = (ROOT / ".github/hooks/pre-push-proprietary-guard.sh").read_text(encoding="utf-8")
        self.assertIn('exec "$REPO_ROOT/.githooks/pre-push" "$@"', legacy)
        self.assertNotIn("PROPRIETARY_PATTERNS", legacy)

    def test_current_tracked_tree_is_clean(self):
        self.assertEqual([], GUARD.find_forbidden_tracked_paths(ROOT))

    def test_default_mode_ignores_public_policy_entries(self):
        root = make_repo(self, files=[("cloud/deploy.tf", "x"), ("docs/keep.md", "x")])
        self.assertEqual([], GUARD.find_forbidden_tracked_paths(root))

    def test_guard_is_mandatory_in_lightweight_ci(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        command = "python3 scripts/check-no-internal-artifacts.py"
        self.assertEqual(1, workflow.count(command))
        job_start = workflow.index("  narrative-governance:")
        job_end = workflow.index("\n  package-assets:", job_start)
        job = workflow[job_start:job_end]
        self.assertIn(command, job)
        self.assertNotIn("continue-on-error", job)


class PublicBoundaryMatchingTests(unittest.TestCase):
    def test_blocked_directory_prefix(self):
        root = make_repo(self, files=[("cloud/infra/main.tf", "x")])
        self.assertEqual(
            [("cloud/infra/main.tf", "cloud/")], GUARD.find_public_violations(root)
        )

    def test_blocked_exact_file(self):
        root = make_repo(self, files=[("LICENSE_MATRIX.toml", "x")])
        self.assertEqual(
            [("LICENSE_MATRIX.toml", "LICENSE_MATRIX.toml")],
            GUARD.find_public_violations(root),
        )

    def test_index_only_entry_still_detected(self):
        root = make_repo(self, files=[("cloud/infra/main.tf", "x")])
        os.remove(root / "cloud/infra/main.tf")
        self.assertEqual(
            [("cloud/infra/main.tf", "cloud/")], GUARD.find_public_violations(root)
        )

    def test_prefix_lookalikes_are_not_blocked(self):
        root = make_repo(self,
            files=[
                ("cloudy/readme.md", "x"),
                ("cloud-docs/readme.md", "x"),
                ("docs/LICENSE_MATRIX.toml", "x"),
                ("LICENSE_MATRIX.toml.bak", "x"),
            ]
        )
        self.assertEqual([], GUARD.find_public_violations(root))

    def test_no_exceptions_every_blocked_entry_reported(self):
        root = make_repo(self,
            files=[("cloud/a.tf", "x"), ("cloud/b.tf", "x"), ("LICENSE_MATRIX.toml", "x")]
        )
        self.assertEqual(
            [
                ("LICENSE_MATRIX.toml", "LICENSE_MATRIX.toml"),
                ("cloud/a.tf", "cloud/"),
                ("cloud/b.tf", "cloud/"),
            ],
            GUARD.find_public_violations(root),
        )

    def test_comments_and_blank_lines_are_skipped(self):
        self.assertEqual(["cloud/", "a.md"], GUARD.policy_entries("# c\n\ncloud/\na.md # why\n"))

    def test_literal_spaces_match_the_push_hook_normalization(self):
        self.assertEqual(["cloud/"], GUARD.policy_entries(" c l o u d / # comment\n"))

    def test_public_mode_retains_internal_archive_guard(self):
        root = make_repo(self, files=[("docs/internal-backup.zip", "fixture")])
        self.assertEqual(
            [("docs/internal-backup.zip", "internal-artifact")],
            GUARD.find_public_violations(root),
        )

    def test_overlapping_rules_report_each_path_once(self):
        root = make_repo(self, policy="docs/internal/\n", files=[("docs/internal/x.md", "fixture")])
        self.assertEqual([("docs/internal/x.md", "docs/internal/")], GUARD.find_public_violations(root))

    def test_public_cli_reports_clean_blocked_and_invalid_root(self):
        clean = make_repo(self, files=[("public.md", "fixture")])
        blocked = make_repo(self, files=[("cloud/private.md", "fixture")])
        nested = blocked / "cloud"
        (nested / ".github-ignore").write_text("unrelated/\n", encoding="utf-8")
        for root, expected in ((clean, 0), (blocked, 1), (nested, 2)):
            with self.subTest(expected=expected):
                result = subprocess.run(
                    [sys.executable, str(SCRIPT), "--public", "--root", str(root)],
                    capture_output=True, timeout=10, check=False,
                )
                self.assertEqual(result.returncode, expected, result.stderr)


class PublicBoundaryFailClosedTests(unittest.TestCase):
    def assert_fails_closed(self, root, needle):
        with self.assertRaises(GUARD.GuardError) as caught:
            GUARD.find_public_violations(Path(root))
        self.assertIn(needle, str(caught.exception))

    def test_missing_policy(self):
        root = make_repo(self, policy=None, files=[("a.md", "x")])
        self.assert_fails_closed(root, "is missing")

    def test_empty_policy(self):
        root = make_repo(self, policy="")
        self.assert_fails_closed(root, "is empty")

    def test_policy_without_entries(self):
        root = make_repo(self, policy="# only comments\n\n")
        self.assert_fails_closed(root, "no blocked entries")

    def test_symlink_policy(self):
        root = make_repo(self, policy=None, files=[("real-policy", "cloud/\n")])
        os.symlink(root / "real-policy", root / ".github-ignore")
        self.assert_fails_closed(root, "not a symlink")

    def test_unsafe_traversal_entry(self):
        root = make_repo(self, policy="../../etc/passwd\n")
        self.assert_fails_closed(root, "relative segments")

    def test_absolute_entry(self):
        root = make_repo(self, policy="/etc/passwd\n")
        self.assert_fails_closed(root, "repository-relative")

    def test_glob_entry_is_not_silently_ignored(self):
        root = make_repo(self, policy="cloud/**/*.tf\n")
        self.assert_fails_closed(root, "unsupported pattern")

    def test_malformed_non_utf8_policy(self):
        root = make_repo(self, policy=None)
        (root / ".github-ignore").write_bytes(b"cloud/\n\xff\xfe\n")
        self.assert_fails_closed(root, "not valid UTF-8")

    def test_unreadable_policy(self):
        root = make_repo(self)
        with mock.patch.object(Path, "open", side_effect=PermissionError("fixture denied")):
            with self.assertRaises(PermissionError):
                GUARD.load_policy(root)

    def test_git_failure_fails_closed(self):
        temp = tempfile.TemporaryDirectory(prefix="boundary-guard-nogit-")
        self.addCleanup(temp.cleanup)
        outside = Path(temp.name)
        (outside / ".github-ignore").write_text(POLICY, encoding="utf-8")
        for flags, label in (([], "internal-artifact"), (["--public"], "public-boundary")):
            result = subprocess.run(
                [sys.executable, str(SCRIPT), "--root", str(outside), *flags],
                env={**os.environ, "GIT_CEILING_DIRECTORIES": str(outside.parent.resolve())},
                capture_output=True, text=True, timeout=10,
            )
            self.assertEqual(2, result.returncode)
            self.assertIn(f"{label} guard failed closed:", result.stderr)

    def test_entry_bound_is_enforced(self):
        entries = "\n".join(f"dir{index}/" for index in range(GUARD.MAX_POLICY_ENTRIES + 1))
        with self.assertRaises(GUARD.GuardError) as caught:
            GUARD.policy_entries(entries)
        self.assertIn("more than", str(caught.exception))

    def test_control_characters_and_repeated_slashes_fail_closed(self):
        for entry in ("\tcloud/", "cloud/\t", "cloud/\r", "cloud/\x85", "cloud//", "a//b"):
            with self.subTest(entry=repr(entry)):
                with self.assertRaises(GUARD.GuardError):
                    GUARD.policy_entries(entry)

    def test_non_ascii_policy_fails_closed_in_actual_cli(self):
        for entry in ("\ufeffcloud/", "\xa0cloud/", "cloud/\u200b", "données/"):
            with self.subTest(entry=repr(entry)):
                root = make_repo(self, policy=entry + "\n", files=[("cloud/a.md", "fixture")])
                result = subprocess.run(
                    [sys.executable, str(SCRIPT), "--public", "--root", str(root)],
                    capture_output=True, text=True, timeout=10,
                )
                self.assertEqual(2, result.returncode)
                self.assertIn("unsupported pattern or unsafe character", result.stderr)

    def test_actual_read_is_bounded_when_policy_grows_after_stat(self):
        root = make_repo(self)
        stream = io.BytesIO(b"x" * (GUARD.MAX_POLICY_BYTES + 2))
        with mock.patch.object(Path, "open", return_value=stream):
            with self.assertRaisesRegex(GUARD.GuardError, "exceeds"):
                GUARD.load_policy(root)

    def test_nested_root_cannot_hide_parent_policy_entries(self):
        root = make_repo(self, files=[("cloud/private.md", "fixture")])
        nested = root / "cloud"
        (nested / ".github-ignore").write_text("unrelated/\n", encoding="utf-8")
        self.assert_fails_closed(nested, "Git worktree root")

    def test_tracked_path_bound_is_enforced(self):
        result = subprocess.CompletedProcess([], 0, bytes([97, 0, 98, 0]), b"")
        with mock.patch.object(GUARD.subprocess, "run", return_value=result):
            with mock.patch.object(GUARD, "MAX_TRACKED_PATHS", 1):
                with self.assertRaisesRegex(GUARD.GuardError, "tracked paths"):
                    GUARD.tracked_paths(ROOT)
            with mock.patch.object(GUARD, "MAX_GIT_OUTPUT_BYTES", 1):
                with self.assertRaisesRegex(GUARD.GuardError, "bounded size"):
                    GUARD.tracked_paths(ROOT)


class DiagnosticEscapingTests(unittest.TestCase):
    def test_control_and_non_ascii_bytes_are_escaped(self):
        self.assertEqual("a\\x0ab", GUARD.escape("a\nb"))
        self.assertEqual("\\x5c", GUARD.escape("\\"))
        self.assertEqual("\\xc3\\xa4", GUARD.escape("ä"))

    def test_plain_paths_are_unchanged(self):
        self.assertEqual("cloud/infra/main.tf", GUARD.escape("cloud/infra/main.tf"))


class PublicWorkflowWiringTests(unittest.TestCase):
    """The public workflow must call the canonical guard, not a copied list."""

    @classmethod
    def setUpClass(cls):
        cls.workflow = (ROOT / ".github/workflows/security-check.yml").read_text(
            encoding="utf-8"
        )

    def test_guardrail_invokes_canonical_public_guard(self):
        start = self.workflow.index("      - name: Proprietary code guardrail")
        step = self.workflow[start : self.workflow.index("\n      - name: ", start + 1)]
        self.assertIn(
            "run: python3 scripts/check-no-internal-artifacts.py --public --root .", step
        )
        self.assertNotIn("continue-on-error", step)

    def test_no_hand_maintained_private_path_list(self):
        self.assertNotIn("PRIVATE_PATHS", self.workflow)


if __name__ == "__main__":
    unittest.main()
