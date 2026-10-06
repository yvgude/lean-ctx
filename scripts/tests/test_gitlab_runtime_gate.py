#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Guard the required private-MR runtime job; GitLab CI lint validates YAML."""

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]


class GitLabRuntimeGateTests(unittest.TestCase):
    def setUp(self):
        self.config = (ROOT / "ci/gitlab-ci.yml").read_text(encoding="utf-8")
        match = re.search(r"(?ms)^v4-runtime:\n(.*?)(?=^\S|\Z)", self.config)
        self.assertIsNotNone(match, "provenance alone is not runtime validation")
        self.job = "\n".join(
            line for line in match.group(1).splitlines()
            if not line.lstrip().startswith("#")
        )

    def test_runtime_gate_is_not_optional(self):
        for bypass in ("allow_failure:", "when: manual", "when: never", "rules:", "only:", "except:"):
            self.assertNotIn(bypass, self.job)
        # No pipeline variable may exempt a pipeline from the full suites.
        self.assertNotIn("LEANCTX_PROOF_BUILD", self.config)
        self.assertIn('CI_PIPELINE_SOURCE == "merge_request_event"', self.config)

    def test_runtime_gate_executes_workspace_not_just_contract_fixtures(self):
        self.assertIn("cargo +stable test --locked --workspace --all-features", self.job)
        self.assertIn("cargo +stable test --locked --workspace --no-fail-fast", self.job)
        self.assertIn("runtime-evidence/tests-default.log", self.job)
        self.assertIn("runtime-evidence/tests-all-features.log", self.job)
        self.assertIn("cargo +stable clippy --locked --all-features -- -D warnings", self.job)
        self.assertIn("cargo +stable fmt --all --check", self.job)

    def test_log_pipelines_preserve_cargo_failure(self):
        self.assertLess(self.job.index("set -o pipefail"), self.job.index("| tee"))
        self.assertNotIn("|| true", self.job)
        self.assertNotIn("|| :", self.job)

    def test_runtime_evidence_is_commit_bound_and_retained_on_failure(self):
        self.assertIn(
            'git -C "$CI_PROJECT_DIR" rev-parse HEAD | tee "$CI_PROJECT_DIR/runtime-evidence/commit.txt"',
            self.job,
        )
        self.assertIn("rustc +stable --version --verbose", self.job)
        self.assertRegex(self.job, r"artifacts:\s+when: always")
        self.assertIn("- runtime-evidence/", self.job)

    def test_expensive_checks_are_serialized(self):
        self.assertIn("resource_group: v4-rust-validation", self.job)
        # Optimized lib-test compilation exceeded the runner cgroup when
        # concurrent with other rustc jobs; serialize compilers, not assertions.
        self.assertIn('CARGO_BUILD_JOBS: "1"', self.job)

    def test_runtime_checks_use_an_unprivileged_job_local_identity(self):
        switch = "runuser --user ci-runner --preserve-environment -- /bin/bash -c"
        self.assertIn(switch, self.job)
        self.assertLess(self.job.index(switch), self.job.index("set -euo pipefail"))
        for command in (
            'test "$(id -un)" = ci-runner',
            'test "$(id -u)" -gt 0',
            'test "$(id -g)" -gt 0',
            'export HOME="$CI_PROJECT_DIR/rust/target/ci-home"',
            'export CARGO_HOME="$CI_PROJECT_DIR/rust/target/ci-cargo-home"',
            'export CARGO_TARGET_DIR="$CI_PROJECT_DIR/rust/target"',
            'git config --global --add safe.directory "$CI_PROJECT_DIR"',
        ):
            self.assertIn(command, self.job)
        self.assertNotIn("chown -R", self.job)

    def test_source_write_guard_excludes_only_job_output_roots(self):
        self.assertIn('-exec chmod go-w {} +', self.job)
        self.assertIn('test -L "$ci_source_path"', self.job)
        self.assertIn('unexpected CI source/output symlink', self.job)
        self.assertEqual(self.job.count('-path "$CI_PROJECT_DIR/rust/target"'), 2)
        self.assertEqual(self.job.count('-path "$CI_PROJECT_DIR/runtime-evidence"'), 2)
        self.assertIn('-writable -print -quit', self.job)
        self.assertIn('if test -n "$writable_source";', self.job)
        self.assertIn('CI source remains writable', self.job)
        self.assertIn('GIT_OPTIONAL_LOCKS=0 git -C "$CI_PROJECT_DIR" diff --exit-code HEAD --', self.job)
        self.assertNotIn('find -L', self.job)


if __name__ == "__main__":
    unittest.main()
