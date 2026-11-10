#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Guard the Rust gates of the private mirror; GitLab CI lint validates YAML.

Shape (owner 08.11.): every merge request runs the blocking fast gate
`mr-quick`; the full suite `v4-runtime` runs after the merge on integration/*
and the default branch.
"""

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]


def job_text(config: str, name: str) -> str:
    match = re.search(rf"(?ms)^{re.escape(name)}:\n(.*?)(?=^\S|\Z)", config)
    assert match is not None, f"missing job {name}"
    return "\n".join(
        line for line in match.group(1).splitlines()
        if not line.lstrip().startswith("#")
    ) + "\n"


def rule_blocks(job: str) -> list[str]:
    rules = re.search(r"(?ms)^  rules:\n(.*?)(?=^  \S)", job)
    assert rules is not None
    return re.split(r"(?m)^    - if: ", rules.group(1))[1:]


RUST_INPUTS = (
    '"*"', "rust/**/*", ".cargo/**/*", "ci/**/*", "docs/**/*", "tests/**/*",
    "packages/**/*", "kits/**/*", "product/**/*", "scripts/**/*",
    "security/**/*", "skills/**/*", "integrations/**/*",
)


class GitLabRuntimeGateTests(unittest.TestCase):
    def setUp(self):
        self.config = (ROOT / "ci/gitlab-ci.yml").read_text(encoding="utf-8")
        self.job = job_text(self.config, "v4-runtime")
        self.quick = job_text(self.config, "mr-quick")
        self.lint = job_text(self.config, "mr-lint")

    # -- v4-runtime: full suite, post-merge -------------------------------

    def test_full_suite_runs_on_every_branch_push_without_bypass(self):
        for bypass in ("allow_failure: true", "when: never", "when: manual", "only:", "except:", "changes:"):
            self.assertNotIn(bypass, self.job)
        blocks = rule_blocks(self.job)
        self.assertEqual(len(blocks), 1, blocks)
        self.assertIn("$CI_COMMIT_BRANCH =~ /^integration\\//", blocks[0])
        self.assertIn("$CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH", blocks[0])
        self.assertNotIn("merge_request_event", blocks[0])
        self.assertNotIn("LEANCTX_PROOF_BUILD", self.config)

    def test_only_a_superseded_run_may_end_as_a_warning(self):
        self.assertIn("allow_failure:\n    exit_codes:\n      - 75\n", self.job)
        self.assertEqual(self.job.count("exit 75"), 1)
        check = self.job.index('git ls-remote origin "refs/heads/$CI_COMMIT_BRANCH"')
        self.assertLess(check, self.job.index("exit 75"))
        self.assertIn('test "$tip" != "$CI_COMMIT_SHA"', self.job)
        # The check runs before any setup or build work.
        self.assertLess(self.job.index("exit 75"), self.job.index("useradd"))
        self.assertLess(self.job.index("exit 75"), self.job.index("mount -o remount"))
        # A check that itself exits 75 is a failure, never a warning.
        self.assertIn("' || { rc=$?; if test \"$rc\" = 75; then", self.job)
        self.assertIn('rc=1; fi; exit "$rc"; }', self.job)

    def test_one_full_run_per_branch(self):
        self.assertIn("resource_group: v4-rust-$CI_COMMIT_REF_SLUG\n", self.job)
        self.assertRegex(self.job, r"needs:\n\s+- job: v4-provenance\n")
        self.assertLess(self.job.index("needs:"), self.job.index("resource_group:"))
        self.assertIn("interruptible: true", self.job)

    def test_full_suite_executes_the_workspace(self):
        self.assertIn(
            'test_features="$(python3 -I "$CI_PROJECT_DIR/scripts/ci/mr_quick.py" --repo "$CI_PROJECT_DIR" --print-test-features)"',
            self.job,
        )
        nextest = ('cargo +stable nextest run --locked --workspace --features "$test_features"'
                   ' --profile ci --no-fail-fast 2>&1 | tee "$CI_PROJECT_DIR/runtime-evidence/tests.log"'
                   ' || nextest_rc=$?')
        self.assertIn(nextest, self.job)
        self.assertNotIn("cargo +stable test --locked --workspace", self.job)
        # nextest skips doctests; they run separately with the same features.
        doctests = 'cargo +stable test --locked --doc --workspace --features "$test_features"'
        self.assertIn(doctests, self.job)
        self.assertIn("runtime-evidence/doctests.log", self.job)
        self.assertLess(self.job.index('test -n "$test_features"'), self.job.index('--features "$test_features"'))
        # A failed run still fails the job, after the remaining checks.
        self.assertEqual(self.job.count("nextest_rc=0"), 1)
        self.assertEqual(self.job.count("doc_rc=0"), 1)
        verdict = 'if test "$nextest_rc" != 0 || test "$doc_rc" != 0; then'
        self.assertIn(verdict + ' echo "tests failed: nextest exit $nextest_rc, doctests exit $doc_rc" >&2; exit 1; fi', self.job)
        self.assertLess(self.job.index(nextest), self.job.index(verdict))
        self.assertLess(self.job.index(doctests), self.job.index(verdict))
        self.assertLess(self.job.index('diff --exit-code HEAD --'), self.job.index(verdict))
        # JUnit from the ci profile is kept with the evidence and reported.
        self.assertIn('cp "$CARGO_TARGET_DIR/nextest/ci/junit.xml" "$CI_PROJECT_DIR/runtime-evidence/junit.xml"', self.job)
        self.assertRegex(self.job, r"reports:\n\s+junit: runtime-evidence/junit\.xml\n")
        self.assertIn("cargo-nextest", self.job)
        nextest_toml = (ROOT / "rust/.config/nextest.toml").read_text(encoding="utf-8")
        ci = nextest_toml[nextest_toml.index("[profile.ci]"):]
        for setting in ('test-threads = "num-cpus"', "retries = 0", "fail-fast = false",
                        'slow-timeout = { period = "60s", terminate-after = 5 }',
                        '[profile.ci.junit]\npath = "junit.xml"'):
            self.assertIn(setting, ci)
        # No retry anywhere may mask a flake.
        self.assertNotRegex(nextest_toml, r"(?m)^retries = [1-9]")
        self.assertIn("runtime-evidence/tests.log", self.job)
        self.assertIn("runtime-evidence/test-features.txt", self.job)
        # The shipping feature set must compile on its own.
        self.assertIn("cargo +stable check --locked --workspace --all-targets", self.job)
        self.assertIn("runtime-evidence/check-default.log", self.job)
        self.assertIn("cargo +stable clippy --locked --all-features -- -D warnings", self.job)
        self.assertIn("cargo +stable fmt --all --check", self.job)
        self.assertIn('CARGO_BUILD_JOBS: "12"', self.job)

    def test_no_test_run_compiles_the_path_jail_out(self):
        # `no-jail` removes PathJail and its tests; --all-features would turn
        # it on. Only clippy (a type check) may use --all-features.
        for job in (self.job, self.quick, self.lint):
            for line in job.splitlines():
                if re.search(r"\bcargo \+stable (test|nextest|build)\b", line):
                    self.assertNotIn("--all-features", line)
                    self.assertNotIn("no-jail", line)

    def test_log_pipelines_preserve_cargo_failure(self):
        for job in (self.job, self.quick, self.lint):
            self.assertLess(job.index("set -o pipefail"), job.index("| tee"))
            self.assertNotIn("|| true", job)
            self.assertNotIn("|| :", job)

    def test_runtime_evidence_is_commit_bound_and_retained_on_failure(self):
        self.assertIn(
            'git -C "$CI_PROJECT_DIR" rev-parse HEAD | tee "$CI_PROJECT_DIR/runtime-evidence/commit.txt"',
            self.job,
        )
        self.assertIn("rustc +stable --version --verbose", self.job)
        self.assertRegex(self.job, r"artifacts:\s+when: always")
        self.assertIn("- runtime-evidence/", self.job)

    def test_cache_is_lock_keyed_and_written_only_by_the_full_suite(self):
        self.assertNotIn("CI_RUNNER_ID", self.job)
        self.assertNotIn("${CI_JOB_NAME}", self.job)
        for key_line in re.findall(r"(?m)^\s+(?:prefix|- key|key):.*$", self.job):
            self.assertNotIn("CI_COMMIT_REF_SLUG", key_line)
        self.assertRegex(self.job, r"files:\n\s+- rust/Cargo\.lock\n")
        self.assertIn("fallback_keys:", self.job)
        self.assertIn("    V4_CACHE_POLICY: pull-push\n", self.job)
        self.assertIn("    V4_TIP_CACHE_POLICY: push\n", self.job)
        self.assertNotIn("policy: pull-push", self.job)
        self.assertNotIn("policy: push", self.job)
        # The MR gate inherits the cache but never writes it.
        self.assertIn("    V4_CACHE_POLICY: pull\n", self.quick)
        self.assertIn("    V4_TIP_CACHE_POLICY: pull\n", self.quick)
        self.assertNotIn("push", self.quick)
        # Compiled output is never zipped; only downloaded crates are.
        for job in (self.job, self.quick):
            self.assertNotIn("rust/target/debug/", job)
            self.assertNotIn("ci-cargo-home/registry/src", job)
        self.assertIn("- rust/target/ci-cargo-home/registry/cache/", self.job)

    def test_jobs_use_the_prebuilt_image_and_install_nothing(self):
        build = (ROOT / "ci/images/rust/build.sh")
        self.assertTrue(build.exists())
        import hashlib
        digest = hashlib.sha256()
        for rel in ("ci/images/rust/Dockerfile", "ci/images/rust/packages.txt",
                    "rust-toolchain.toml", "rust/rust-toolchain.toml"):
            path = ROOT / rel
            if path.is_file():
                digest.update(f"{rel}\n{hashlib.sha256(path.read_bytes()).hexdigest()}\n".encode())
        image = f"lean-ctx-ci-rust:{digest.hexdigest()[:12]}"
        provenance = job_text(self.config, "v4-provenance")
        for job in (self.job, provenance):
            # The tag is the hash of the image inputs (build.sh), so a changed
            # Dockerfile without a rebuilt, re-tagged image fails here.
            self.assertIn(f"  image:\n    name: {image}\n    pull_policy: if-not-present\n", job)
            for install in ("apt-get", "rustup toolchain install", "cargo install", "curl "):
                self.assertNotIn(install, job)
        self.assertNotIn("rust:1.99.1-bookworm", self.config)
        self.assertIn("command -v \"$ci_tool\"", self.job)

    def test_compiler_cache_is_written_only_by_a_protected_post_merge_push(self):
        self.assertIn("    V4_SCCACHE_MODE: write\n", self.job)
        self.assertIn("    V4_SCCACHE_MODE: read\n", self.quick)
        self.assertIn('    CARGO_INCREMENTAL: "0"\n', self.job)
        self.assertNotIn("CARGO_INCREMENTAL", self.quick)
        self.assertNotIn("SCCACHE_LOCAL_RW_MODE=READ_WRITE", self.quick)
        self.assertEqual(self.job.count("SCCACHE_LOCAL_RW_MODE=READ_WRITE"), 1)
        write = self.job.index("SCCACHE_LOCAL_RW_MODE=READ_WRITE")
        guard = self.job.rindex("if test", 0, write)
        condition = self.job[guard:write]
        for required in (
            'test "${V4_SCCACHE_MODE:-}" = write',
            'test "${CI_COMMIT_REF_PROTECTED:-}" = true',
            'test "${CI_PIPELINE_SOURCE:-}" = push',
            'test "$(stat -c %u "$ci_sccache")" = 10999',
        ):
            self.assertIn(required, condition)
        # Every other run gets a read-only host directory or fails, and reads
        # it through a job-local overlay.
        self.assertIn('if ! mount -o remount,bind,ro "$ci_sccache"; then', self.job)
        self.assertIn('touch "$ci_sccache/.ci-write-probe" 2>/dev/null; then echo "sccache: $ci_sccache is still writable" >&2; exit 1; fi', self.job)
        self.assertIn("metacopy=on", self.job)
        self.assertIn("SCCACHE_LOCAL_RW_MODE=READ_ONLY RUSTC_WRAPPER=sccache", self.job)
        self.assertLess(self.job.index("mount -o remount,bind,ro"), self.job.index("SCCACHE_LOCAL_RW_MODE=READ_ONLY"))
        self.assertIn("unset RUSTC_WRAPPER SCCACHE_DIR SCCACHE_LOCAL_RW_MODE", self.job)
        self.assertLess(self.job.index("unset RUSTC_WRAPPER"), write)
        self.assertIn("! test -L \"$ci_sccache\"", self.job)
        # The cache owner is the fixed job identity.
        self.assertIn("useradd --system --uid 10999 --user-group", self.job)
        self.assertIn('test "$(id -u ci-runner)" = 10999', self.job)
        for job in (self.job, self.quick, self.lint):
            self.assertIn("runtime-evidence/sccache.txt", job)

    def test_linux_ci_builds_engine_and_binary_package_at_opt_level_zero(self):
        # Cargo.toml's opt-level 1 for these two is a macOS linker limit; on
        # Linux it only triples their compile time. Every Rust job installs
        # the override as CARGO_HOME config, so all jobs share cache keys.
        install = ("printf '%s\\n' '[profile.dev.package.lean-ctx-engine]' 'opt-level = 0'"
                   " '[profile.dev.package.lean-ctx]' 'opt-level = 0'")
        self.assertIn(install, self.job)
        self.assertIn('> "$CI_PROJECT_DIR/rust/target/ci-cargo-home/config.toml"', self.job)
        self.assertNotIn("profile.release", self.job)
        self.assertLess(self.job.index("install -d -m 0700 -o ci-runner"), self.job.index(install))
        self.assertLess(self.job.index(install), self.job.index("runuser --user ci-runner"))
        self.assertIn('export CARGO_HOME="$CI_PROJECT_DIR/rust/target/ci-cargo-home"', self.job)
        self.assertIn('    RUST_MIN_STACK: "8388608"\n', self.job)
        manifest = (ROOT / "rust/Cargo.toml").read_text(encoding="utf-8")
        self.assertIn("[profile.dev.package.lean-ctx-engine]\nopt-level = 1\n", manifest)

    def test_provenance_is_unconditional(self):
        provenance = re.search(r"(?ms)^v4-provenance:\n(.*?)(?=^\S|\Z)", self.config)
        self.assertIsNotNone(provenance)
        for bypass in ("allow_failure:", "when: manual", "when: never", "rules:"):
            self.assertNotIn(bypass, provenance.group(1))

    # -- mr-quick: blocking fast gate on every merge request --------------

    def test_quick_gate_blocks_every_rust_merge_request(self):
        self.assertIn("  extends: v4-runtime\n", self.quick)
        self.assertIn("  allow_failure: false\n", self.quick)
        for bypass in ("allow_failure: true", "when: never", "when: manual", "only:", "except:", "exit_codes"):
            self.assertNotIn(bypass, self.quick)
        blocks = rule_blocks(self.quick)
        # One rule, no IID condition: no merge request falls through.
        self.assertEqual(len(blocks), 1, blocks)
        self.assertIn("'$CI_PIPELINE_SOURCE == \"merge_request_event\"'\n", blocks[0])
        self.assertIn("changes:", blocks[0])
        self.assertNotIn("CI_MERGE_REQUEST_IID", blocks[0])
        for path in RUST_INPUTS:
            self.assertIn(f"          - {path}\n", blocks[0])
        # Serialized per merge request only (a newer push waits); no static
        # lanes that queue unrelated merge requests behind each other.
        self.assertIn("  resource_group: mr-quick-$CI_MERGE_REQUEST_IID\n", self.quick)
        self.assertNotIn("MR_QUICK_LANE", self.config)
        self.assertIn("  needs: []\n", self.quick)

    def test_lint_gate_runs_beside_the_quick_gate_on_every_merge_request(self):
        # Inherits mr-quick's single rule, needs: [], allow_failure: false and
        # read-only caches; overrides only the script and its own lane.
        self.assertIn("  extends: mr-quick\n", self.lint)
        self.assertIn("  resource_group: mr-lint-$CI_MERGE_REQUEST_IID\n", self.lint)
        for override in ("rules:", "needs:", "allow_failure", "variables:", "when: manual", "when: never", "only:", "except:", "push"):
            self.assertNotIn(override, self.lint)
        self.assertIn("cargo +stable fmt --all --check", self.lint)
        # Same clippy invocation as the full suite, so the cache applies.
        self.assertIn("cargo +stable clippy --locked --all-features -- -D warnings", self.lint)
        self.assertNotIn("mr_quick.py", self.lint)
        self.assertNotIn("nextest", self.lint)
        self.assertIn('GIT_OPTIONAL_LOCKS=0 git -C "$CI_PROJECT_DIR" diff --exit-code HEAD --', self.lint)
        # The quick gate no longer lints serially before its test build.
        self.assertNotIn("cargo +stable clippy", self.quick)
        self.assertNotIn("cargo +stable fmt", self.quick)

    def test_quick_gate_builds_everything_and_runs_selected_tests(self):
        self.assertIn('python3 -I -u "$CI_PROJECT_DIR/scripts/ci/mr_quick.py" --repo "$CI_PROJECT_DIR" --base "$CI_MERGE_REQUEST_DIFF_BASE_SHA"', self.quick)
        self.assertIn('id -u | tee "$CI_PROJECT_DIR/runtime-evidence/uid.txt"', self.quick)
        self.assertIn('GIT_OPTIONAL_LOCKS=0 git -C "$CI_PROJECT_DIR" diff --exit-code HEAD --', self.quick)
        script_path = ROOT / "scripts/ci/mr_quick.py"
        if not script_path.exists():
            self.skipTest("mr_quick.py stays private (PUBLISH_MANIFEST exclude)")
        script = script_path.read_text(encoding="utf-8")
        # Builds every workspace test target with the full suite's features
        # (`nextest list` builds and lists them), then runs a filterset of
        # them with the full suite's nextest profile.
        self.assertIn('"cargo", "+stable", "nextest", *args, "--locked", "--workspace",\n'
                      '            "--features", features]', script)
        self.assertIn('nextest_cmd(features, "list", "--message-format", "json")', script)
        self.assertIn('nextest_cmd(features, "run", "--profile", NEXTEST_PROFILE, "--no-fail-fast")', script)
        self.assertIn('NEXTEST_PROFILE = "ci"', script)
        self.assertIn('"nextest" / NEXTEST_PROFILE / "junit.xml"', script)
        self.assertNotIn('"--all-features"', script)
        self.assertIn('UNTESTED_FEATURES = {"no-jail"}', script)

    def test_checks_use_an_unprivileged_job_local_identity(self):
        switch = "runuser --user ci-runner --preserve-environment -- /bin/bash -c"
        for job in (self.job, self.quick, self.lint):
            self.assertIn(switch, job)
            self.assertLess(job.index(switch), job.index("set -euo pipefail"))
            for command in (
                'test "$(id -un)" = ci-runner',
                'test "$(id -u)" -gt 0',
                'test "$(id -g)" -gt 0',
                'export HOME=/home/ci-runner',
                'case "$HOME/" in "$CI_PROJECT_DIR"/*) echo "HOME inside the checkout" >&2; exit 1;; esac',
                'export CARGO_HOME="$CI_PROJECT_DIR/rust/target/ci-cargo-home"',
                'export CARGO_TARGET_DIR="$CI_PROJECT_DIR/rust/target"',
                'git config --global --add safe.directory "$CI_PROJECT_DIR"',
                'if test -n "$writable_source";',
            ):
                self.assertIn(command, job)
        self.assertNotIn("chown -R", self.job)

    def test_source_write_guard_excludes_only_job_output_roots(self):
        self.assertIn('-exec chmod go-w {} +', self.job)
        self.assertIn('test -L "$ci_source_path"', self.job)
        self.assertIn('unexpected CI source/output symlink', self.job)
        self.assertEqual(self.job.count('-path "$CI_PROJECT_DIR/rust/target"'), 2)
        self.assertEqual(self.job.count('-path "$CI_PROJECT_DIR/runtime-evidence"'), 2)
        self.assertIn('-writable -print -quit', self.job)
        self.assertIn('CI source remains writable', self.job)
        self.assertIn('GIT_OPTIONAL_LOCKS=0 git -C "$CI_PROJECT_DIR" diff --exit-code HEAD --', self.job)
        self.assertNotIn('find -L', self.job)


if __name__ == "__main__":
    unittest.main()
