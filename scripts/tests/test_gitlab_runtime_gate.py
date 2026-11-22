#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Guard the Rust gates of the private mirror; GitLab CI lint validates YAML.

Shape (owner 20.11., lean merge request pipelines): every merge request runs
`lint` (the light gates in one job), `mr-lint` (clippy) and `mr-quick` (the
affected tests), automatically and blocking; the full suite `v4-runtime` runs
after the merge on integration/* and the default branch, and Weave is the full
gate before main.
"""

from pathlib import Path
import importlib.util
import json
import re
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


def job_text(config: str, name: str) -> str:
    match = re.search(rf"(?ms)^{re.escape(name)}:\n(.*?)(?=^\S|\Z)", config)
    assert match is not None, f"missing job {name}"
    return "\n".join(
        line for line in match.group(1).splitlines()
        if not line.lstrip().startswith("#")
    ) + "\n"


# Weave schedule bisect (owner 20.11.): the rules that only match an API
# pipeline on weave-bisect/* with WEAVE_SCHEDULE_BISECT=1.
BISECT_GUARD = ("$CI_PIPELINE_SOURCE == \"api\" && $WEAVE_SCHEDULE_BISECT == \"1\" && "
                "$CI_COMMIT_BRANCH =~ /^weave-bisect\\//")
BISECT_RULE_RE = re.compile(r"(?m)^    - if: '[^'\n]*\$WEAVE_SCHEDULE_BISECT[^'\n]*'\n(?:      .*\n)*")


def bisect_rule(job: str) -> str:
    return f"    - if: '{BISECT_GUARD} && $WEAVE_BISECT_JOBS =~ /(^|,){job}(,|$)/'\n"


def bisect_first(job: str) -> str:
    """The first rule of a job that a Weave bisect pipeline may run."""
    return bisect_rule(job) + "      interruptible: false\n"


PUBLIC_API_BISECT_RULES = (
    bisect_first("(public-api|change-class)")
    + "    - if: '$WEAVE_SCHEDULE_BISECT == \"1\" && $CI_COMMIT_BRANCH =~ /^weave-bisect\\//'\n"
    "      when: never\n")


def rule_blocks(job: str) -> list[str]:
    rules = re.search(r"(?ms)^  rules:\n(.*?)(?=^  \S)", job)
    assert rules is not None
    return re.split(r"(?m)^    - if: ", rules.group(1))[1:]


def needs_of(job: str) -> list[tuple[str, bool]]:
    """The job's `needs:` as (job, optional) pairs, in order."""
    block = re.search(r"(?ms)^  needs:\n(.*?)(?=^  \S)", job)
    assert block is not None, "missing needs:"
    entries = re.split(r"(?m)^    - job: ", block.group(1))[1:]
    needs = []
    for entry in entries:
        name, _, rest = entry.partition("\n")
        # Gates never hand artifacts to the heavy jobs.
        assert "      artifacts: false\n" in rest, entry
        needs.append((name.strip(), "      optional: true\n" in rest))
    return needs


# Cheap gates in front of every heavy job (owner 10.11.). The two private-tree
# gates are optional (a checkout without them runs none); rust-fmt never is.
CHEAP_GATE_NEEDS = [
    ("rust-fmt", False),
    ("ip-boundary-skill-installed", True),
    ("scope-register", True),
]
V4_RUNTIME_NEEDS = CHEAP_GATE_NEEDS + [("v4-provenance", False)]
# On merge requests the cheap gates run inside `lint` (owner 20.11.), which
# mr-quick and mr-lint need (optional only where `lint` does not run).
MR_GATE_NEEDS = [("lint", True)]

# What clippy reads on a merge request (mr-lint's path rule).
RUST_GATE_INPUTS = ("rust/**/*", ".cargo/**/*", "ci/**/*")
MR_RULE = "'$CI_PIPELINE_SOURCE == \"merge_request_event\"'"
# Lean merge request pipelines (owner 20.11.): the merge request jobs run
# automatically, blocking and interruptible; the branch jobs skip merge
# requests through .not-on-mr.
AUTO_RULE_KEYS = "      interruptible: true\n"
NOT_ON_MR_RULES = (f"  rules:\n    - if: {MR_RULE}\n      when: never\n"
                   "    - when: on_success\n      interruptible: false\n")
# Lean main pipelines (owner 20.11.): the branch gates run in scheduled
# pipelines only (every 6 hours on main); pushes run main-smoke + public-api.
SCHEDULE_RULE = "'$CI_PIPELINE_SOURCE == \"schedule\"'"
# Fast MR mode (P5_FAST_MR, owner 20.11.2026): while the switch is on, the
# scheduled gates and the full suite also run on pushes to main.
FAST_MAIN_IF = ("'$P5_FAST_MR == \"1\" && $CI_PIPELINE_SOURCE == \"push\" && "
                "($CI_COMMIT_BRANCH =~ /^integration\\// || $CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH)'")
FAST_MAIN_REF = "    - !reference [.fast-mr-main, rules]\n"
SCHEDULE_ONLY_RULES = (f"  rules:\n    - if: {SCHEDULE_RULE}\n      interruptible: false\n"
                       + FAST_MAIN_REF + "    - when: never\n")


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

    def test_full_suite_runs_in_scheduled_branch_pipelines_without_bypass(self):
        for bypass in ("allow_failure: true", "when: never", "when: manual", "only:", "except:", "changes:"):
            self.assertNotIn(bypass, self.job)
        blocks = rule_blocks(self.job)
        self.assertEqual(len(blocks), 1, blocks)
        # Lean main pipelines (owner 20.11.): scheduled pipelines only (every
        # 6 hours on main); pushes run main-smoke.
        self.assertTrue(blocks[0].startswith("'$CI_PIPELINE_SOURCE == \"schedule\" && ("), blocks[0])
        self.assertIn("$CI_COMMIT_BRANCH =~ /^integration\\//", blocks[0])
        self.assertIn("$CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH", blocks[0])
        self.assertNotIn("merge_request_event", blocks[0])
        self.assertNotIn("LEANCTX_PROOF_BUILD", self.config)
        # Fast MR mode: also on main pushes, right after the schedule rule.
        self.assertTrue(blocks[0].endswith("\n" + FAST_MAIN_REF), blocks[0])

    def test_fast_mr_mode_is_one_switch(self):
        # Owner 20.11.2026: one variable, default on; "0" restores every job.
        self.assertEqual(self.config.count('\n  P5_FAST_MR: "1"\n'), 1)
        self.assertIn("  rules:\n    - if: " + FAST_MAIN_IF + "\n      interruptible: false\n",
                      job_text(self.config, ".fast-mr-main"))
        self.assertEqual(self.config.count(FAST_MAIN_IF), 1)
        # The MR jobs read it at run time; Weave's lint job keeps its name.
        self.assertIn('if test "${P5_FAST_MR:-0}" = 1; then quick_mode="--fast"; fi', self.quick)
        self.assertIn('--evidence "$CI_PROJECT_DIR/runtime-evidence" $quick_mode 2>&1', self.quick)
        # Clippy blocks the land in fast mode too; only the kill switch
        # P5_FAST_MR_LINT=check falls back to the type check (21.11.).
        switch = 'if test "${P5_FAST_MR:-0}" = 1 && test "${P5_FAST_MR_LINT:-clippy}" = check; then\n'
        self.assertEqual(self.lint.count(switch), 1)
        fast, full = self.lint.split(switch, 1)[1].split("        else\n", 1)
        self.assertIn("cargo +stable check --locked --workspace --all-features --all-targets 2>&1", fast)
        self.assertNotIn("clippy --locked", fast)
        self.assertEqual(full.count("cargo +stable clippy --locked --all-features"), 2)
        self.assertNotIn("P5_FAST_MR_LINT", self.config.split("\nvariables:", 1)[1].split("\n\n", 1)[0])
        # No job is removed: every scheduled gate keeps its schedule rule.
        for name in ("rust-fmt", "ip-boundary-skill-installed", "v4-provenance", "change-class"):
            self.assertIn("!reference [.schedule-only, rules]", job_text(self.config, name), name)

    def test_pushes_run_only_the_main_smoke_and_public_api(self):
        smoke = job_text(self.config, "main-smoke")
        self.assertIn("  extends: v4-runtime\n", smoke)
        self.assertIn("  needs: []\n", smoke)
        # Never queued behind the scheduled full suite (v4-rust-<ref>).
        self.assertIn("  resource_group: main-smoke-$CI_COMMIT_REF_SLUG\n", smoke)
        self.assertIn("  resource_group: v4-rust-$CI_COMMIT_REF_SLUG\n", self.job)
        self.assertEqual(rule_blocks(smoke), [
            # Protected default branch: one stable target of its own, no farm
            # overlay (lean-ctx-3e, 20.11.: md0 writes).
            "'$CI_PIPELINE_SOURCE == \"push\" && $CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH && "
            "$CI_COMMIT_REF_PROTECTED == \"true\"'\n      interruptible: false\n"
            "      variables:\n"
            "        CI_TARGET_DIR: \"/builds/.ci-cache/$CI_PROJECT_PATH_SLUG/main-smoke/target\"\n"
            "        CI_WARM_FARM: \"\"\n"
            "        GIT_CLONE_PATH: $CI_BUILDS_DIR/main-smoke/$CI_PROJECT_PATH\n",
            "'$CI_PIPELINE_SOURCE == \"push\" && ($CI_COMMIT_BRANCH =~ /^integration\\// || "
            "$CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH)'\n      interruptible: false\n"])
        # Only a superseded run may end as a warning; never a flake verdict.
        self.assertIn("  allow_failure:\n    exit_codes:\n      - 75\n  ", smoke)
        # Reads the caches and the farm, never writes them.
        self.assertIn("    V4_CACHE_POLICY: pull\n    V4_TIP_CACHE_POLICY: pull\n", smoke)
        self.assertNotIn("warm_farm.py", smoke)
        for command in ("cargo +stable build --locked -p lean-ctx --bin lean-ctx",
                        '"$CARGO_TARGET_DIR/debug/lean-ctx" --version',
                        '"$CARGO_TARGET_DIR/debug/lean-ctx" cloud --help'):
            self.assertIn(command, smoke)
        for heavy in ("nextest", "clippy", "cargo +stable doc", "cargo +stable test"):
            self.assertNotIn(heavy, smoke)
        # Every other branch job is scheduled only, except public-api.
        on_push = []
        for name in re.findall(r"(?m)^([a-z][\w-]*):\n", self.config):
            job = job_text(self.config, name)
            if ("  stage: verify\n" not in job and "  extends: " not in job) or name in ("workflow", "variables"):
                continue
            # The merge request jobs and the scheduled full suite.
            # test-coverage-map: scheduled pipelines only, see its own test.
            if "!reference [.schedule-only, rules]" in job or name in ("v4-runtime", "mr-quick", "mr-lint", "lint",
                                                                          "test-coverage-map"):
                continue
            on_push.append(name)
        self.assertEqual(sorted(on_push), ["main-smoke", "public-api"])

    def test_the_coverage_map_job_is_scheduled_soft_and_switchable(self):
        cov = job_text(self.config, "test-coverage-map")
        self.assertEqual(rule_blocks(cov), [
            "'($P5_TEST_SELECT == \"1\" || $P5_TEST_SELECT == \"enforce\") && "
            "$CI_PIPELINE_SOURCE == \"schedule\" && "
            "$CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH && $CI_COMMIT_REF_PROTECTED == \"true\"'\n"
            "      when: always\n      interruptible: false\n"])
        self.assertIn("  allow_failure: true\n", cov)
        # Never writes the shared caches and never runs before the full suite.
        self.assertIn("    V4_CACHE_POLICY: pull\n", cov)
        self.assertIn("    V4_TIP_CACHE_POLICY: pull\n", cov)
        self.assertIn("  needs:\n    - job: v4-runtime\n", cov)
        self.assertNotIn("warm_farm.py", cov)
        # The instrumented build stays inside the slot target and is removed.
        self.assertIn('export CARGO_TARGET_DIR="$CI_TARGET_DIR/coverage/target"', cov)
        self.assertEqual(cov.count('rm -rf --one-file-system -- "$CI_TARGET_DIR/coverage"'), 3)
        self.assertIn('P5_TEST_SELECT: "1"\n', self.config)
        # mr-quick uses the map only behind the switch, and never fails on it.
        self.assertIn('if test "${P5_TEST_SELECT:-0}" = 1 || test "$select_mode" = enforce; then', self.quick)
        self.assertIn('select_tests.py" fetch --out "$HOME/test-map.json.gz"', self.quick)
        # Shadow by default: only the literal value "enforce" binds the selection.
        self.assertIn("select_mode=shadow\n", self.quick)
        self.assertIn('if test "${P5_TEST_SELECT:-0}" = enforce; then select_mode=enforce; fi', self.quick)
        self.assertIn("--test-select-mode $select_mode", self.quick)
        self.assertNotIn('P5_TEST_SELECT: "enforce"', self.config)
        # The wrapper writes into tests/ and samples/ of the coverage dir.
        self.assertIn('mkdir -p -- "$LEAN_CTX_COVERAGE_DIR/tests" "$LEAN_CTX_COVERAGE_DIR/samples"', cov)

    def test_scheduled_runs_are_never_superseded(self):
        """The superseded guard runs for branch pushes only. A scheduled run
        (test-coverage-map) is not repeated by a later push, so it must not
        exit 75 just because the branch moved on."""
        import os
        import subprocess
        import tempfile
        start = self.config.index("# Branch runs only: a newer push")
        end = self.config.index("\n        fi\n", start) + len("\n        fi\n")
        guard = "\n".join(line[6:] for line in
                          self.config[start:end].splitlines()) + "\n      fi\n"
        with tempfile.TemporaryDirectory() as tmp:
            fake = os.path.join(tmp, "git")
            with open(fake, "w", encoding="utf-8") as f:
                f.write("#!/bin/sh\nprintf 'newer\\trefs/heads/main\\n'\n")
            os.chmod(fake, 0o700)
            base = {"PATH": tmp + os.pathsep + os.environ["PATH"],
                    "CI_COMMIT_BRANCH": "main", "CI_COMMIT_SHA": "older"}

            def rc(source: str) -> int:
                env = dict(base, CI_PIPELINE_SOURCE=source)
                return subprocess.run(["sh", "-c", guard + "exit 0\n"], env=env,
                                      capture_output=True).returncode
            self.assertEqual(rc("push"), 75)
            self.assertEqual(rc("schedule"), 0)

    def test_only_a_superseded_run_or_the_flake_gate_may_end_as_a_warning(self):
        self.assertIn("allow_failure:\n    exit_codes:\n      - 75\n      - 76\n  ", self.job)
        self.assertEqual(self.job.count("exit 75"), 1)
        check = self.job.index('git ls-remote origin "refs/heads/$CI_COMMIT_BRANCH"')
        self.assertLess(check, self.job.index("exit 75"))
        self.assertIn('test "$tip" != "$CI_COMMIT_SHA"', self.job)
        # The check runs before any setup or build work.
        self.assertLess(self.job.index("exit 75"), self.job.index("useradd"))
        self.assertLess(self.job.index("exit 75"), self.job.index('ci_target_parent="/builds/.ci-cache/'))
        # A check that itself exits 75 is a failure, never a warning.
        self.assertIn("' || { rc=$?; if test \"$rc\" = 75; then", self.job)
        self.assertIn('rc=1; fi; exit "$rc"; }', self.job)
        # Exit 76 is the flake gate's warning only: the job exits 76 once,
        # and only when nextest's result is 76; any other 76 (one without
        # flake-verdict.txt saying `warning`) is a failure.
        self.assertEqual(self.job.count("exit 76"), 1)
        self.assertIn('if test "$nextest_rc" = 76; then', self.job)
        self.assertIn('if test "$rc" = 76 && ! grep -qx warning "$CI_PROJECT_DIR/runtime-evidence/flake-verdict.txt" 2>/dev/null; then', self.job)
        self.assertLess(self.job.index('if test "$doc_rc" != 0 ||'), self.job.index("exit 76"))

    def test_one_full_run_per_branch(self):
        self.assertIn("resource_group: v4-rust-$CI_COMMIT_REF_SLUG\n", self.job)
        # The full suite starts only after provenance and the cheap gates
        # passed; provenance and rust-fmt are never optional for it.
        self.assertEqual(needs_of(self.job), V4_RUNTIME_NEEDS)
        self.assertLess(self.job.index("needs:"), self.job.index("resource_group:"))
        # Post-merge runs always finish: each merge keeps its own verdict.
        self.assertIn("  interruptible: false\n", self.job)
        self.assertNotIn("interruptible: true", self.job)

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
        verdict = 'if test "$doc_rc" != 0 || { test "$nextest_rc" != 0 && test "$nextest_rc" != 76; }; then'
        self.assertIn(verdict + ' echo "tests failed: nextest exit $nextest_rc, doctests exit $doc_rc" >&2; exit 1; fi', self.job)
        self.assertLess(self.job.index(nextest), self.job.index(verdict))
        self.assertLess(self.job.index(doctests), self.job.index(verdict))
        self.assertLess(self.job.index('diff --exit-code HEAD --'), self.job.index(verdict))
        # JUnit from the ci profile is kept with the evidence and reported.
        self.assertIn('cp "$CI_PROJECT_DIR/rust/target/nextest/ci/junit.xml" "$CI_PROJECT_DIR/runtime-evidence/junit.xml"', self.job)
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
        self.assertIn("cargo +stable clippy --locked --all-features -- -D warnings", self.job)
        self.assertIn('CARGO_BUILD_JOBS: "12"', self.job)

    def test_no_gate_runs_twice(self):
        # W1-08: format is the rust-fmt job alone.
        fmt = job_text(self.config, "rust-fmt")
        self.assertIn("cargo +stable fmt --all --check", fmt)
        for job in (self.job, self.quick, self.lint):
            self.assertNotIn("cargo +stable fmt", job)
        self.assertEqual(self.config.count("cargo +stable fmt --all --check"), 1)
        # Clippy covers only --all-features; the shipping (default) feature
        # set is compiled exactly once, by the full suite, after the tests.
        check = 'cargo +stable check --locked --workspace 2>&1 | tee "$CI_PROJECT_DIR/runtime-evidence/check-default.log"'
        # Plus mr-lint's type check in fast MR mode (instead of clippy there),
        # which v4-runtime repeats so the warm farm carries its fingerprints.
        self.assertEqual(self.config.count("cargo +stable check"), 3)
        self.assertEqual(self.lint.count("cargo +stable check"), 1)
        self.assertIn(check, self.job)
        self.assertNotIn("--features", check)
        self.assertLess(self.job.index("cargo +stable test --locked --doc"), self.job.index(check))

    def test_mr_lint_uploads_no_junit_report(self):
        # It runs no tests; v4-runtime (and mr-quick) keep theirs.
        self.assertIn("      - runtime-evidence/\n    reports: null\n", self.lint)
        self.assertRegex(self.job, r"reports:\n\s+junit: runtime-evidence/junit\.xml\n")
        self.assertNotIn("reports: null", self.quick)

    def test_the_farm_generation_carries_the_fast_lint_check(self):
        lint_check = ('cargo +stable check --locked --workspace --all-features --all-targets 2>&1'
                      ' | tee "$CI_PROJECT_DIR/runtime-evidence/check-all-targets.log"')
        self.assertIn(lint_check, self.lint)
        gate = 'if test "${P5_FAST_MR:-0}" = 1 && test "${CI_WARM_FARM_LINT_CHECK:-1}" = 1; then\n'
        self.assertIn(gate + "          " + lint_check + "\n        fi\n", self.job)
        # Built before the farm publish step copies the target.
        self.assertLess(self.job.index(lint_check), self.job.index('ci_farm_min_age="${CI_WARM_FARM_MIN_AGE_MIN:-60}"'))

    def test_failed_tests_are_retried_once_through_the_flake_gate(self):
        gate = ('python3 -I -u "$CI_PROJECT_DIR/scripts/ci/flake_gate.py"'
                ' --junit "$CI_PROJECT_DIR/runtime-evidence/junit.xml"'
                ' --rerun-junit "$CI_PROJECT_DIR/rust/target/nextest/ci/junit.xml"'
                ' --quarantine "$CI_PROJECT_DIR/ci/flaky-quarantine.json"'
                ' --job v4-runtime --evidence "$CI_PROJECT_DIR/runtime-evidence" --'
                ' mold -run cargo +stable nextest run --locked --workspace --features "$test_features"'
                ' --profile ci --no-fail-fast 2>&1 | tee "$CI_PROJECT_DIR/runtime-evidence/flake-gate.log"'
                ' || flake_rc=$?')
        self.assertIn(gate, self.job)
        self.assertEqual(self.job.count("flake_gate.py"), 1)
        # Only after a failed first run, on its copied report, and the gate's
        # result replaces nextest's before the verdict.
        guard = 'if test "$nextest_rc" != 0; then'
        copy = 'cp "$CI_PROJECT_DIR/rust/target/nextest/ci/junit.xml" "$CI_PROJECT_DIR/runtime-evidence/junit.xml"'
        self.assertLess(self.job.index(copy), self.job.index(guard))
        self.assertLess(self.job.index(guard), self.job.index(gate))
        self.assertLess(self.job.index(gate), self.job.index('nextest_rc="$flake_rc"'))
        # The gate and its quarantine list are private-mirror only (excluded
        # from the public export together, PUBLISH_MANIFEST.toml).
        if (ROOT / "scripts/ci/flake_gate.py").is_file():
            self.assertTrue((ROOT / "ci/flaky-quarantine.json").is_file())
        # Jobs retry only for infrastructure failures, never a script failure.
        retry = job_text(self.config, ".flake-retry")
        self.assertIn("  retry:\n    max: 2\n    when:\n", retry)
        self.assertNotIn("script_failure", retry)
        for name in ("lint", "rust-fmt", "ip-boundary-skill-installed", "v4-runtime", "v4-provenance",
                     "public-api", "change-class"):
            self.assertIn("  extends: .flake-retry\n", job_text(self.config, name), name)
        scope = ROOT / "ee/ci/scope-register.yml"
        if scope.exists():
            text = scope.read_text(encoding="utf-8")
            self.assertIn(retry[retry.index("  retry:"):].rstrip("\n") + "\n", text)

    def test_api_snapshot_and_change_class_jobs_never_block(self):
        api = job_text(self.config, "public-api")
        cls = job_text(self.config, "change-class")
        for job in (api, cls):
            self.assertIn("  tags: [light]\n", job)
            self.assertIn("  allow_failure: true\n", job)
            self.assertIn('"${CI_MERGE_REQUEST_DIFF_BASE_SHA:-$(git rev-parse HEAD^1)}"', job)
        self.assertIn("  needs: []\n", api)
        # public-api runs on every push (part of the main smoke), change-class
        # in scheduled pipelines only (owner 20.11.).
        # Both also run in Weave bisect pipelines that name them.
        self.assertIn("  rules:\n" + PUBLIC_API_BISECT_RULES + "    - !reference [.not-on-mr, rules]\n", api)
        self.assertIn("  rules:\n" + bisect_first("change-class") + "    - !reference [.schedule-only, rules]\n", cls)
        self.assertIn("scripts/public-api-snapshot snapshot", api)
        self.assertIn("scripts/public-api-snapshot diff api-snapshot/base api-snapshot/head", api)
        self.assertIn("    - job: public-api\n      artifacts: true\n", cls)
        self.assertIn("--base-snapshots api-snapshot/base --head-snapshots api-snapshot/head", cls)
        self.assertIn("scripts/change-class", cls)
        for script in ("scripts/public-api-snapshot", "scripts/change-class"):
            self.assertTrue((ROOT / script).is_file(), script)

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
        # Only the lockfile entry of v4-runtime (with its tip fallback).
        self.assertIn("    - &v4_cargohome_lock\n      key:\n", self.job)
        self.assertIn("  cache:\n    - *v4_cargohome_lock\n", self.quick)
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

    def test_the_incremental_target_is_the_only_compiler_cache(self):
        # W1-08: no sccache; the slot's persistent target compiles
        # incrementally, and no wrapper may sit between cargo and rustc.
        self.assertIn('    CARGO_INCREMENTAL: "1"\n', self.job)
        self.assertNotIn("CARGO_INCREMENTAL", self.quick)
        self.assertNotIn("CARGO_INCREMENTAL", self.lint)
        for gone in ("V4_SCCACHE", "SCCACHE_CACHE_SIZE", "SCCACHE_IDLE_TIMEOUT", "RUSTC_WRAPPER=",
                     "ci-sccache", "sccache --stop-server"):
            self.assertNotIn(gone, self.job, gone)
            self.assertNotIn(gone, self.lint, gone)
        # The only remounts left are the warm farm's (W4-04), never a cache
        # directory of a compiler wrapper.
        self.assertEqual(self.job.count("mount -o remount"), self.job.count('mount -o remount,bind,ro "$ci_farm"')
                         + self.job.count('mount -o remount,bind,rw "$ci_farm"'))
        self.assertNotIn("mount -o remount", self.lint)
        self.assertNotIn("V4_SCCACHE", self.quick)
        self.assertIn("  - unset RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER SCCACHE_DIR\n", self.job)
        self.assertLess(self.job.index("unset RUSTC_WRAPPER"), self.job.index("  script:\n"))
        self.assertNotIn(" mount ", self.job[self.job.index("for ci_tool in"):self.job.index("command -v")])
        # The target owner is the fixed job identity.
        self.assertIn("useradd --system --uid 10999 --user-group", self.job)
        self.assertIn('test "$(id -u ci-runner)" = 10999', self.job)

    def test_linux_ci_builds_engine_and_binary_package_at_opt_level_zero(self):
        # Cargo.toml's opt-level 1 for these two is a macOS linker limit; on
        # Linux it only triples their compile time. Every Rust job installs
        # the override as CARGO_HOME config, so all jobs share cache keys.
        # strip = true: a package override makes cargo pass -C strip=debuginfo
        # whatever CARGO_PROFILE_*_STRIP says, so the overrides strip as well.
        install = ("printf '%s\\n' '[profile.dev.package.lean-ctx-engine]' 'opt-level = 0' 'strip = true'"
                   " '[profile.dev.package.lean-ctx]' 'opt-level = 0' 'strip = true'")
        self.assertIn(install, self.job)
        self.assertIn('> "$CI_PROJECT_DIR/rust/target/ci-cargo-home/config.toml"', self.job)
        self.assertNotIn("profile.release", self.job)
        self.assertLess(self.job.index("install -d -m 0700 -o ci-runner"), self.job.index(install))
        self.assertLess(self.job.index(install), self.job.index("runuser --user ci-runner"))
        self.assertIn('export CARGO_HOME="$CI_PROJECT_DIR/rust/target/ci-cargo-home"', self.job)
        self.assertIn('    RUST_MIN_STACK: "8388608"\n', self.job)
        manifest = (ROOT / "rust/Cargo.toml").read_text(encoding="utf-8")
        self.assertIn("[profile.dev.package.lean-ctx-engine]\nopt-level = 1\n", manifest)

    def test_provenance_runs_in_scheduled_pipelines_only(self):
        provenance = job_text(self.config, "v4-provenance")
        for bypass in ("when: never", "changes:", "exists:", "only:", "except:"):
            self.assertNotIn(bypass, provenance)
        # Lean pipelines (owner 20.11.): not on merge requests nor pushes
        # (Weave runs these checks on the merge result before it lands);
        # scheduled pipelines only, where v4-runtime needs it.
        # (and in Weave bisect pipelines that name it or v4-runtime).
        self.assertIn("  rules:\n" + bisect_first("(v4-provenance|v4-runtime)")
                      + "    - !reference [.schedule-only, rules]\n  needs: []\n", provenance)
        self.assertEqual(job_text(self.config, ".not-on-mr").rstrip("\n") + "\n", NOT_ON_MR_RULES)
        self.assertEqual(job_text(self.config, ".schedule-only").rstrip("\n") + "\n", SCHEDULE_ONLY_RULES)
        for gone in (".heavy-manual-on-mr", ".interruptible-on-mr", ".rust-changes-on-mr", "when: manual"):
            self.assertNotIn(gone, self.config)
        # A red gate still stops the pipeline at once.
        self.assertIn("  auto_cancel:\n    on_new_commit: interruptible\n    on_job_failure: all\n",
                      self.config)

    # -- mr-quick: automatic affected-test gate on merge requests (20.11.) --

    def test_quick_gate_runs_automatically_on_every_rust_merge_request(self):
        self.assertIn("  extends: v4-runtime\n", self.quick)
        for bypass in ("when: never", "only:", "except:", "exit_codes"):
            self.assertNotIn(bypass, self.quick)
        blocks = rule_blocks(self.quick)
        # One rule, no IID condition: every merge request with a Rust input
        # gets the job, automatic and blocking.
        self.assertEqual(len(blocks), 1, blocks)
        self.assertTrue(blocks[0].startswith(MR_RULE + "\n" + AUTO_RULE_KEYS + "      changes:\n"), blocks[0])
        self.assertIn("  allow_failure: false\n", self.quick)
        self.assertIn("changes:", blocks[0])
        self.assertNotIn("CI_MERGE_REQUEST_IID", blocks[0])
        for path in RUST_INPUTS:
            self.assertIn(f"          - {path}\n", blocks[0])
        # Serialized per merge request only (a newer push waits); no static
        # lanes that queue unrelated merge requests behind each other.
        self.assertIn("  resource_group: mr-quick-$CI_MERGE_REQUEST_IID\n", self.quick)
        self.assertNotIn("MR_QUICK_LANE", self.config)
        # Waits only for `lint` (the cheap gates), never for provenance, and
        # overrides v4-runtime's list instead of inheriting it.
        self.assertEqual(needs_of(self.quick), MR_GATE_NEEDS)
        self.assertIn("  interruptible: true\n", self.quick)
        fmt = job_text(self.config, "rust-fmt")
        self.assertIn("  needs: []\n", fmt)
        self.assertIn("cargo +stable fmt --all --check", fmt)
        for bypass in ("allow_failure", "when: manual", "only:", "except:"):
            self.assertNotIn(bypass, fmt)

    # -- lean merge request gates (owner 20.11.) -----------------------------

    def test_branch_gates_skip_merge_requests(self):
        # The light gates run as their own jobs on branches only; on merge
        # requests `lint` runs their scripts.
        # Since the lean main pipelines (owner 20.11.) in scheduled pipelines
        # only; public-api also on pushes (test_api_snapshot_...).
        # Weave bisect pipelines run them when named (rust-fmt also for
        # v4-runtime, which needs it).
        for name, bisect in (("rust-fmt", "(rust-fmt|v4-runtime)"), ("change-class", "change-class")):
            self.assertIn("  rules:\n" + bisect_first(bisect) + "    - !reference [.schedule-only, rules]\n",
                          job_text(self.config, name), name)
        ip = job_text(self.config, "ip-boundary-skill-installed")
        self.assertIn("  rules:\n    - if: '$CI_PROJECT_PATH != \"root/lean-ctx\"'\n      when: never\n"
                      + bisect_first("ip-boundary-skill-installed")
                      + "    - !reference [.schedule-only, rules]\n  needs: []\n", ip)
        fmt = job_text(self.config, "rust-fmt")
        self.assertIn("  needs: []\n", fmt)
        scope = ROOT / "ee/ci/scope-register.yml"
        if scope.exists():
            job = job_text(scope.read_text(encoding="utf-8"), "scope-register")
            self.assertIn("  rules:\n" + bisect_first("scope-register")
                          + SCHEDULE_ONLY_RULES.removeprefix("  rules:\n").replace(
                              FAST_MAIN_REF,
                              f"    - if: {FAST_MAIN_IF}\n      interruptible: false\n")
                          + "  needs: []\n", job)

    def test_light_gates_run_in_one_lint_job_on_merge_requests(self):
        lint = job_text(self.config, "lint")
        self.assertIn("  tags: [light]\n", lint)
        self.assertIn("  interruptible: true\n", lint)
        self.assertIn("  needs: []\n", lint)
        for bypass in ("when: manual", "allow_failure", "changes:", "only:", "except:"):
            self.assertNotIn(bypass, lint)
        self.assertEqual(rule_blocks(lint), [
            "'$CI_PROJECT_PATH != \"root/lean-ctx\"'\n      when: never\n",
            MR_RULE + "\n" + AUTO_RULE_KEYS,
        ])
        # The blocking parts are the branch jobs' scripts, one copy each.
        refs = re.findall(r"(?m)^    - !reference \[([\w-]+), script\]$", lint)
        self.assertEqual(refs, ["rust-fmt", "ip-boundary-skill-installed", "scope-register"])
        # The informational parts run last and never fail the job.
        for command in ("scripts/public-api-snapshot snapshot", "scripts/public-api-snapshot diff",
                        "scripts/change-class"):
            self.assertIn(command, lint)
        self.assertGreater(lint.index("scripts/public-api-snapshot"), lint.index("!reference [scope-register, script]"))
        self.assertIn("lint: public-api failed (informational, does not block)", lint)
        self.assertIn("lint: change-class failed (informational, does not block)", lint)

    def test_lint_runs_automatically_on_rust_merge_requests_only(self):
        # Rust inputs only, automatic and blocking (owner 20.11.); no branch
        # catch-all (v4-runtime lints branches).
        rules = re.search(r"(?ms)^  rules:\n(.*?)(?=^  \S)", self.lint).group(1)
        self.assertEqual(
            rules,
            f"    - if: {MR_RULE}\n" + AUTO_RULE_KEYS + "      changes:\n        paths:\n"
            + "".join(f"          - {path}\n" for path in RUST_GATE_INPUTS),
        )

    def test_lint_gate_runs_beside_the_quick_gate_on_every_merge_request(self):
        # Inherits mr-quick's cheap-gate needs and read-only caches;
        # overrides the script, its own lane and its rules (Rust inputs only:
        # test_lint_runs_automatically_on_rust_merge_requests_only).
        self.assertIn("  extends: mr-quick\n", self.lint)
        self.assertIn("  resource_group: mr-lint-$CI_MERGE_REQUEST_IID\n", self.lint)
        for override in ("needs:", "variables:", "when: never", "only:", "except:", "push"):
            self.assertNotIn(override, self.lint)
        # Format is rust-fmt's, which mr-lint needs (W1-08: no gate twice).
        self.assertNotIn("cargo +stable fmt", self.lint)
        # Same clippy invocation as the full suite, so the cache applies.
        self.assertIn("cargo +stable clippy --locked --all-features -- -D warnings", self.lint)
        self.assertNotIn("mr_quick.py", self.lint)
        self.assertNotIn("nextest", self.lint)
        self.assertIn('GIT_OPTIONAL_LOCKS=0 git -C "$CI_PROJECT_DIR" diff --exit-code HEAD --', self.lint)
        # The quick gate no longer lints serially before its test build.
        self.assertNotIn("cargo +stable clippy", self.quick)
        self.assertNotIn("cargo +stable fmt", self.quick)

    def test_rustdoc_and_generated_docs_are_gated_after_the_merge(self):
        # Light local hooks (owner 19.11.) moved rustdoc and gen_docs drift
        # into CI; lean merge request pipelines (owner 20.11.) leave them to
        # v4-runtime after every merge (and Weave), no merge request job.
        self.assertNotIn("\nmr-docs:\n", self.config)
        rustdoc = ('RUSTDOCFLAGS="-D warnings" cargo +stable doc --locked --no-deps --all-features'
                   ' 2>&1 | tee "$CI_PROJECT_DIR/runtime-evidence/rustdoc.log"')
        gen_docs = ('cargo +stable run --locked -p lean-ctx-server --quiet --example gen_docs'
                    ' --features dev-tools -- --check 2>&1 | tee "$CI_PROJECT_DIR/runtime-evidence/gen-docs.log"')
        # The registry snapshot, which preflight and GitHub ci.yml also check.
        gen_registry = ('cargo +stable run --locked -p lean-ctx-engine --quiet --example gen_registry'
                        ' --features dev-tools -- --check 2>&1 | tee "$CI_PROJECT_DIR/runtime-evidence/gen-registry.log"')
        self.assertIn(rustdoc, self.job)
        self.assertIn(gen_docs, self.job)
        self.assertIn(gen_registry, self.job)
        self.assertLess(self.job.index("set -o pipefail"), self.job.index(rustdoc))
        self.assertLess(self.job.index(gen_docs), self.job.index(gen_registry))
        # Post-merge: after the default-feature check, before the verdict.
        self.assertLess(self.job.index("check-default.log"), self.job.index(rustdoc))

    def test_quick_gate_builds_everything_and_runs_selected_tests(self):
        self.assertIn('python3 -I -u "$CI_PROJECT_DIR/scripts/ci/mr_quick.py" --repo "$CI_PROJECT_DIR" --base "$MR_BASE"', self.quick)
        # An MR without file changes has no CI_MERGE_REQUEST_DIFF_BASE_SHA; under
        # set -u every use needs a fallback (pipeline 9257: unbound variable).
        self.assertIn('MR_BASE="${CI_MERGE_REQUEST_DIFF_BASE_SHA:-${CI_MERGE_REQUEST_TARGET_BRANCH_SHA:-}}"', self.quick)
        self.assertIn('INPUT_CHECK_SPEC_COMMIT="$MR_BASE"', self.quick)
        self.assertNotIn('"$CI_MERGE_REQUEST_DIFF_BASE_SHA"', self.quick)
        self.assertIn('id -u | tee "$CI_PROJECT_DIR/runtime-evidence/uid.txt"', self.quick)
        self.assertIn('GIT_OPTIONAL_LOCKS=0 git -C "$CI_PROJECT_DIR" diff --exit-code HEAD --', self.quick)
        script_path = ROOT / "scripts/ci/mr_quick.py"
        if not script_path.exists():
            self.skipTest("mr_quick.py stays private (PUBLISH_MANIFEST exclude)")
        script = script_path.read_text(encoding="utf-8")
        # Builds every workspace test target with the full suite's features
        # (`nextest list` builds and lists them), then runs a filterset of
        # them with the full suite's nextest profile.
        # Fast MR mode (P5_FAST_MR) narrows to `-p` only when the engine is touched.
        self.assertIn('scope = ["--workspace"] if packages is None else', script)
        self.assertIn('"cargo", "+stable", "nextest", *args, "--locked", *scope]', script)
        self.assertIn('nextest_cmd(features, "list", "--message-format", "json", packages=packages)', script)
        self.assertIn('nextest_cmd(features, "run", "--profile", NEXTEST_PROFILE,\n'
                      '                      *([] if args.fast else ["--no-fail-fast"]),\n'
                      '                      "--test-threads", str(MR_QUICK_TEST_THREADS), packages=fast_pkgs)', script)
        # Owner 12.11.: mr-quick alone runs 8 test threads on the 6-CPU job
        # cgroup (replaces the num-cpus rule of 08.11. for mr-quick only).
        self.assertIn("MR_QUICK_TEST_THREADS = 8\n", script)
        self.assertIn('NEXTEST_PROFILE = "ci"', script)
        self.assertIn("junit = nextest_junit(rust)", script)
        self.assertIn("return rust / store / profile / \"junit.xml\"", script)
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
                'export CARGO_TARGET_DIR="$CI_TARGET_DIR"',
                'case "$CARGO_TARGET_DIR/" in "$CI_PROJECT_DIR"/*) echo "CARGO_TARGET_DIR inside the checkout" >&2; exit 1;;',
                'if test -L "$CARGO_TARGET_DIR" || ! test -d "$CARGO_TARGET_DIR" || ! test -O "$CARGO_TARGET_DIR"; then',
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

    def test_persistent_target_stays_outside_the_checkout_and_the_trust_boundary(self):
        # Compiled output persists per runner slot outside the checkout; the
        # last component stays `target` (tests match /target/debug/deps).
        self.assertIn('    CI_TARGET_DIR: "/builds/.ci-cache/$CI_PROJECT_PATH_SLUG/slot-$CI_CONCURRENT_ID/target"\n', self.job)
        self.assertIn('    CI_TARGET_CAP_GB: "40"\n', self.job)
        self.assertNotIn("CI_TARGET_DIR:", self.quick)
        self.assertNotIn("CI_TARGET_DIR:", self.lint)
        for job in (self.job, self.quick, self.lint):
            self.assertNotIn('CARGO_TARGET_DIR="$CI_PROJECT_DIR', job)
        # git clean still wipes the whole checkout; no exclusions.
        self.assertIn("  GIT_CLEAN_FLAGS: -ffdx\n", self.config)
        self.assertNotRegex(self.config, r"GIT_CLEAN_FLAGS:.*-e")
        # Root-side setup, fail closed: fixed builds dir, sane slug, target
        # outside the checkout, root-owned real parent directories.
        for check in (
            'test "${CI_BUILDS_DIR:-}" = /builds || {',
            "case \"${CI_PROJECT_PATH_SLUG:-}\" in ''|*[!a-z0-9-]*)",
            "case \"${CI_TARGET_CAP_GB:-}\" in ''|*[!0-9]*)",
            "case \"${CI_CONCURRENT_ID:-}\" in ''|*[!0-9]*|????*)",
            'ci_target_project="/builds/.ci-cache/$CI_PROJECT_PATH_SLUG"',
            'ci_target_slot="slot-$CI_CONCURRENT_ID"',
            'ci_target_parent="$ci_target_project/$ci_target_slot"',
            'ci_target="$ci_target_parent/target"',
            'test "${CI_TARGET_DIR:-}" = "$ci_target" || {',
            'case "$ci_target/" in "$CI_PROJECT_DIR"/*) echo "target: inside the checkout" >&2; exit 1;; esac',
            'if test -L /builds; then',
            'for ci_dir in /builds/.ci-cache "$ci_target_project" "$ci_target_parent"; do',
            'if test -L "$ci_dir"; then echo "target: $ci_dir is a symlink" >&2; exit 1; fi',
            'if test "$(stat -c %u -- "$ci_dir")" != 0; then',
            'chown -h root:root -- "$ci_dir"',
            'chmod 0755 -- "$ci_dir"',
            'if test -L "$ci_target"; then echo "target: $ci_target is a symlink" >&2; exit 1; fi',
            'test "$(stat -c %u -- "$ci_target")" != 10999',
            'install -d -m 0755 -o ci-runner -g ci-runner -- "$ci_target"',
        ):
            self.assertIn(check, self.job)
        # Size cap and toolchain stamp (written by root beside the target,
        # where job code cannot write) wipe without following links.
        self.assertIn('du -sx --block-size=1G -- "$ci_target"', self.job)
        self.assertIn('if test "$ci_target_gb" -gt "$CI_TARGET_CAP_GB"; then', self.job)
        self.assertIn('ci_target_state="wiped ($ci_target_gb GB > cap)"', self.job)
        self.assertIn('"$(rustc +stable -vV)" | sha256sum', self.job)
        self.assertIn('printf \'%s\\n\' "$ci_stamp" > "$ci_target_parent/target.stamp"', self.job)
        self.assertIn('wiped*) rm -rf --one-file-system -- "$ci_target";;', self.job)
        self.assertIn('rm -rf --one-file-system -- "$ci_target/nextest"', self.job)
        # nextest's store is in the checkout: cleared as the job identity before the run.
        store = 'rm -rf --one-file-system -- "$CI_PROJECT_DIR/rust/target/nextest"'
        self.assertEqual(self.job.count(store), 1)
        self.assertLess(self.job.index("runuser --user ci-runner"), self.job.index(store))
        self.assertLess(self.job.index(store), self.job.index("mold -run cargo +stable nextest run"))
        self.assertNotIn("rm -rf --no-preserve-root", self.job)
        self.assertIn('echo "target: $ci_target_state" > "$CI_PROJECT_DIR/runtime-evidence/target.txt"', self.job)
        # Set up as root before any job code runs, after the identity exists.
        setup = self.job.index('ci_target_project="/builds/.ci-cache/')
        self.assertLess(self.job.index("useradd --system --uid 10999"), setup)
        self.assertLess(setup, self.job.index("runuser --user ci-runner --preserve-environment"))
        # The checkout guards are unchanged: rust/target (CARGO_HOME) stays
        # pruned, and the persistent target is not inside the scanned tree.
        self.assertNotIn('-path "/builds/.ci-cache', self.job)


    def test_each_runner_slot_owns_its_target_state(self):
        # Pipeline 13180 (MR !493): mr-quick (slot 0) wiped the one shared
        # target over the cap while mr-lint (slot 1) built in it; both died
        # with EPERM/ENOENT. Two concurrent slots must never resolve to the
        # same target, stamp, overlay or overlay.base.
        declared = re.search(r'(?m)^    CI_TARGET_DIR: "([^"]+)"$', self.job)
        self.assertIsNotNone(declared)
        # The declared target itself names the slot.
        self.assertIn("/slot-$CI_CONCURRENT_ID/", declared.group(1))
        lines = self.job.splitlines()
        assigned = [line.strip() for line in lines if re.match(r"\s*ci_target(_project|_parent)?=", line)]
        # before_script derives project, parent and target in that order; the
        # publish step (script) derives the parent once more on its own.
        setup = assigned[:3]
        self.assertEqual([line.split("=", 1)[0] for line in setup],
                         ["ci_target_project", "ci_target_parent", "ci_target"])
        publish = [line for line in assigned[3:] if line.startswith("ci_target_parent=")]
        self.assertTrue(publish, "the publish step derives the slot parent too")
        # The slot number is validated before any path is derived from it.
        check = self.job.find('case "${CI_CONCURRENT_ID:-}" in')
        self.assertGreaterEqual(check, 0)
        self.assertLess(check, self.job.index(setup[0]))

        # The whole root-side derivation, including main-smoke's exception.
        start = self.job.index(setup[0])
        last = 'ci_target="$ci_target_parent/target"'
        derive = self.job[start:self.job.index(last, start) + len(last)]

        def resolve(slot: str, declared_dir: str = declared.group(1), **ci: str) -> dict[str, str]:
            env = {"CI_PROJECT_PATH_SLUG": "root-lean-ctx", "CI_CONCURRENT_ID": slot, **ci}
            expand = lambda text: re.sub(r"\$\{?(\w+)\}?", lambda m: env.get(m.group(1), m.group(0)), text)
            env["CI_TARGET_DIR"] = expand(declared_dir)
            script = derive + '\nprintf "%s\\n%s\\n" "$ci_target_parent" "$ci_target"'
            out = subprocess.run(["bash", "-c", script], env=env, check=True,
                                 capture_output=True, text=True).stdout.split("\n")
            published = subprocess.run(["bash", "-c", publish[0] + '; printf "%s" "$ci_target_parent"'],
                                       env=env, check=True, capture_output=True, text=True).stdout
            return {"declared": env["CI_TARGET_DIR"], "parent": out[0], "target": out[1],
                    "publish_parent": published}

        main_push = {"CI_PIPELINE_SOURCE": "push", "CI_COMMIT_REF_PROTECTED": "true",
                     "CI_DEFAULT_BRANCH": "main", "CI_COMMIT_BRANCH": "main",
                     "CI_PROJECT_PATH": "root/lean-ctx", "CI_PROJECT_DIR": "/builds/main-smoke/root/lean-ctx"}
        slots = [resolve("0"), resolve("1"),
                 # Every other job keeps the slot target, also on main.
                 resolve("0", CI_JOB_NAME="v4-runtime", **{**main_push, "CI_PIPELINE_SOURCE": "schedule"}),
                 resolve("1", CI_JOB_NAME="v4-runtime", **main_push)]
        for slot in slots:
            # The before_script's fail-closed equality and the publish step
            # agree with the declared variable in every slot.
            self.assertEqual(slot["target"], slot["declared"])
            self.assertEqual(slot["target"], slot["parent"] + "/target")
            self.assertEqual(slot["publish_parent"], slot["parent"])
            self.assertTrue(slot["target"].endswith("/target"))
        self.assertNotEqual(slots[0]["declared"], slots[1]["declared"])
        self.assertNotEqual(slots[0]["parent"], slots[1]["parent"])
        # Neither slot's state directory contains the other's.
        self.assertFalse(slots[1]["parent"].startswith(slots[0]["parent"] + "/"))
        self.assertFalse(slots[0]["parent"].startswith(slots[1]["parent"] + "/"))
        # Nothing per-job lives directly in the shared project directory.
        self.assertNotRegex(self.config, r"/builds/\.ci-cache/\$CI_PROJECT_PATH_SLUG/(target|overlay)")
        self.assertNotRegex(self.config, r'(?m)ci_target_parent="/builds/\.ci-cache/\$CI_PROJECT_PATH_SLUG"$')

        # main-smoke on the protected default branch (lean-ctx-3e, 20.11.):
        # one stable target whatever the slot, declared by its rule and
        # derived identically by the before_script; nobody else resolves to it.
        smoke_rule = re.search(r'(?m)^        CI_TARGET_DIR: "([^"]+)"$', job_text(self.config, "main-smoke"))
        self.assertIsNotNone(smoke_rule)
        self.assertEqual(smoke_rule.group(1), "/builds/.ci-cache/$CI_PROJECT_PATH_SLUG/main-smoke/target")
        smoke = [resolve(slot, smoke_rule.group(1), CI_JOB_NAME="main-smoke", **main_push) for slot in ("0", "1")]
        for run in smoke:
            self.assertEqual(run["target"], run["declared"])
            self.assertEqual(run["target"], "/builds/.ci-cache/root-lean-ctx/main-smoke/target")
            self.assertEqual(run["parent"], "/builds/.ci-cache/root-lean-ctx/main-smoke")
            for other in slots:
                self.assertNotEqual(run["parent"], other["parent"])
                self.assertFalse(run["parent"].startswith(other["parent"] + "/"))
                self.assertFalse(other["parent"].startswith(run["parent"] + "/"))
        # Off the protected default-branch push, main-smoke keeps the slot
        # target (integration/*, an unprotected ref, a forged job name on a
        # merge request).
        for ci in ({**main_push, "CI_COMMIT_BRANCH": "integration/x"},
                   {**main_push, "CI_COMMIT_REF_PROTECTED": "false"},
                   {**main_push, "CI_PIPELINE_SOURCE": "merge_request_event"},
                   {**main_push, "CI_DEFAULT_BRANCH": ""}):
            run = resolve("1", CI_JOB_NAME="main-smoke", **ci)
            self.assertEqual(run["parent"], "/builds/.ci-cache/root-lean-ctx/slot-1")
            self.assertEqual(run["target"], run["declared"])
        # A main-smoke declaration outside that case fails closed.
        mismatch = subprocess.run(
            ["bash", "-c", derive + '\ntest "${CI_TARGET_DIR:-}" = "$ci_target"'],
            env={"CI_PROJECT_PATH_SLUG": "root-lean-ctx", "CI_CONCURRENT_ID": "0", "CI_JOB_NAME": "main-smoke",
                 **{**main_push, "CI_COMMIT_BRANCH": "integration/x"},
                 "CI_TARGET_DIR": "/builds/.ci-cache/root-lean-ctx/main-smoke/target"},
            capture_output=True, text=True)
        self.assertNotEqual(mismatch.returncode, 0)

        # F1 (bd review !521): the stable target is paired 1:1 with a stable
        # checkout. The rule pins GIT_CLONE_PATH, and the before_script
        # refuses a main-smoke run on main from any other checkout (a slot's).
        smoke_job = job_text(self.config, "main-smoke")
        clone = re.search(r'(?m)^        GIT_CLONE_PATH: (\S+)$', smoke_job)
        self.assertIsNotNone(clone)
        self.assertEqual(clone.group(1).replace("$CI_BUILDS_DIR", "/builds").replace("$CI_PROJECT_PATH", "root/lean-ctx"),
                         main_push["CI_PROJECT_DIR"])
        for checkout in ("/builds/0/root/lean-ctx", "/builds/1/root/lean-ctx", "/builds/main-smoke/other/x", ""):
            run = subprocess.run(["bash", "-c", derive],
                                 env={"CI_PROJECT_PATH_SLUG": "root-lean-ctx", "CI_CONCURRENT_ID": "1", "CI_JOB_NAME": "main-smoke",
                                      **{**main_push, "CI_PROJECT_DIR": checkout}},
                                 capture_output=True, text=True)
            self.assertNotEqual(run.returncode, 0, checkout)
            self.assertIn("main-smoke checkout", run.stderr)
        # Every other job keeps its slot checkout (job-level default).
        self.assertIn("    GIT_CLONE_PATH: $CI_BUILDS_DIR/$CI_CONCURRENT_ID/$CI_PROJECT_PATH\n", self.job)

    def test_main_smoke_directory_has_one_writer(self):
        # F3 (bd review !521): the resource group serialises main-smoke per
        # ref; a root flock on main-smoke/ backs it up, held by the job's
        # shell and never handed to job code.
        smoke = job_text(self.config, "main-smoke")
        self.assertIn("  resource_group: main-smoke-$CI_COMMIT_REF_SLUG\n", smoke)
        lock = ('      if test "$ci_target_slot" = main-smoke; then\n'
                '        if test -L "$ci_target_parent/lock"; then echo "target: $ci_target_parent/lock is a symlink" >&2; exit 1; fi\n'
                '        exec 9>>"$ci_target_parent/lock"\n'
                '        flock -w 2400 9 || { echo "target: $ci_target_parent is locked by another job" >&2; exit 1; }\n'
                '      fi\n')
        self.assertIn(lock, self.job)
        # Taken after the root-owned parent exists, before anything in it
        # is wiped or mounted.
        self.assertLess(self.job.index('chmod 0755 -- "$ci_dir"'), self.job.index(lock))
        self.assertLess(self.job.index(lock), self.job.index('rm -rf --one-file-system -- "$ci_target"'))
        self.assertIn(" flock ", self.job[self.job.index("for ci_tool in"):self.job.index("command -v")])
        # The job user's shell does not inherit the lock descriptor.
        self.assertIn("\n      ' 9>&-\n", smoke)
        self.assertEqual(self.config.count("exec 9>>"), 1)

    def test_warm_farm_is_written_by_protected_pushes_and_read_only_elsewhere(self):
        # W4-04: one host farm, root-owned; the stamp opt-out for every job.
        self.assertIn("    CI_WARM_FARM: /warm-farm\n", self.job)
        self.assertIn('    LEANCTX_BUILD_GIT_STAMP: "off"\n', self.job)
        for job in (self.quick, self.lint):
            self.assertNotIn("CI_WARM_FARM:", job)
            self.assertNotIn("LEANCTX_BUILD_GIT_STAMP", job)
            self.assertNotIn("warm_farm.py\" publish", job)
        # Merge-request jobs: read-only farm or fail, a job-local upper
        # layer, and the mtime normalization that must not half-fail.
        mount = self.job.index('mount -t overlay ci-warm-target')
        # Merge requests read their target branch's farm; only a protected
        # push or scheduled run reads its own branch (and the scheduled
        # v4-runtime publishes it; main-smoke has its own script).
        self.assertIn('ci_farm_branch="${CI_MERGE_REQUEST_TARGET_BRANCH_NAME:-}"', self.job)
        self.assertIn('elif test "${CI_COMMIT_REF_PROTECTED:-}" = true && { test "${CI_PIPELINE_SOURCE:-}" = push || test "${CI_PIPELINE_SOURCE:-}" = schedule; }; then\n'
                      '        ci_farm_branch="${CI_COMMIT_BRANCH:-}"', self.job)
        self.assertIn('if ! mount -o remount,bind,ro "$ci_farm"; then', self.job)
        self.assertIn('touch "$ci_farm/.ci-write-probe" 2>/dev/null; then echo "warm-farm: $ci_farm is still writable" >&2; exit 1; fi', self.job)
        self.assertLess(self.job.index('mount -o remount,bind,ro "$ci_farm"'), mount)
        # Without a mount the overlay dir is removed whole (exact line, not
        # the prefix of the tmpfs branch below).
        self.assertIn('\n          rm -rf --one-file-system -- "$ci_target_parent/overlay"\n', self.job)
        # A tmpfs mounted at overlay/ (lean-ctx-3e): never removed (EBUSY),
        # root reset to root:root 0755, contents wiped, and too little free
        # space falls back to the slot target before a generation is picked.
        self.assertIn("case \"${CI_OVERLAY_MIN_FREE_GB:-20}\" in ''|*[!0-9]*|?????*) echo \"target: unexpected CI_OVERLAY_MIN_FREE_GB\" >&2; exit 1;; esac", self.job)
        tmpfs = self.job.index('if test -d "$ci_target_parent/overlay" && mountpoint -q -- "$ci_target_parent/overlay"; then\n'
                               '          chown -h root:root -- "$ci_target_parent/overlay"\n'
                               '          chmod 0755 -- "$ci_target_parent/overlay"\n'
                               '          find "$ci_target_parent/overlay" -mindepth 1 -maxdepth 1 -exec rm -rf --one-file-system -- {} +\n')
        self.assertIn("stat -f -c '%a %S' -- \"$ci_target_parent/overlay\" | awk", self.job)
        self.assertIn('if test "$ci_overlay_free_gb" -lt "${CI_OVERLAY_MIN_FREE_GB:-20}"; then\n'
                      '            echo "warm-farm: overlay tmpfs only $ci_overlay_free_gb GB free; using the slot target" >&2\n'
                      '            ci_overlay_usable=false\n', self.job)
        pick = self.job.index('if test "$ci_overlay_usable" = true && test "$(stat -c %u -- "$ci_farm")" = 0')
        self.assertLess(tmpfs, pick)
        self.assertLess(pick, self.job.index('warm_farm.py" pick'))
        self.assertIn('elif test "$ci_overlay_usable" = true; then\n'
                      '          echo "warm-farm: no usable generation; using the slot target" >&2', self.job)
        self.assertIn('install -d -m 0755 -o ci-runner -g ci-runner -- "$ci_target_parent/overlay/upper"', self.job)
        self.assertIn('test "$(stat -c %u -- "$ci_farm")" = 0', self.job)
        self.assertIn('|| { echo "warm-farm: normalize failed" >&2; exit 1; }', self.job)
        # Set up as root, before the target evidence and any job code.
        self.assertLess(self.job.index('install -d -m 0755 -o ci-runner -g ci-runner -- "$ci_target"'), mount)
        self.assertLess(mount, self.job.index('echo "target: $ci_target_state" >'))
        self.assertLess(mount, self.job.index("runuser --user ci-runner --preserve-environment"))
        # Publishing: protected push only, as root after the green checks.
        publish = self.job.index('warm_farm.py" publish')
        guard = self.job.rindex('ci_farm="${CI_WARM_FARM:-}"', 0, publish)
        for required in (
            'test "${CI_COMMIT_REF_PROTECTED:-}" = true',
            'test "${CI_PIPELINE_SOURCE:-}" = push',
            'mountpoint -q "$ci_farm"',
        ):
            self.assertIn(required, self.job[guard:publish])
        self.assertLess(self.job.index('rc=1; fi; exit "$rc"; }'), publish)
        # Read-only for the whole run; writable for root's publish only.
        self.assertLess(self.job.index('mount -o remount,bind,rw "$ci_farm"'), publish)
        self.assertLess(self.job.index('rc=1; fi; exit "$rc"; }'), self.job.index('mount -o remount,bind,rw "$ci_farm"'))
        self.assertEqual(self.job.count("remount,bind,rw"), 1)
        self.assertEqual(self.job.count('warm_farm.py" publish'), 1)

    def test_only_merge_requests_fall_back_to_the_default_branch_generation(self):
        # A merge request into a feature branch has no generation of its
        # target; it takes the default branch's (kill switch
        # CI_WARM_FARM_FALLBACK=0). Protected runs never set a fallback.
        mr = self.job.index('if test "${CI_PIPELINE_SOURCE:-}" = merge_request_event; then\n'
                            '        ci_farm_branch="${CI_MERGE_REQUEST_TARGET_BRANCH_NAME:-}"')
        protected = self.job.index('elif test "${CI_COMMIT_REF_PROTECTED:-}" = true && { test "${CI_PIPELINE_SOURCE:-}" = push')
        fallback = self.job.index('if test "${CI_WARM_FARM_FALLBACK:-1}" = 1; then ci_farm_fallback="${CI_DEFAULT_BRANCH:-}"; fi')
        self.assertLess(mr, fallback)
        self.assertLess(fallback, protected)
        self.assertEqual(self.job.count('ci_farm_fallback="'), 2)
        self.assertLess(self.job.index('ci_farm_fallback=""'), mr)
        self.assertIn('--target-dir "$ci_build_target" --fallback-branch "$ci_farm_fallback")"', self.job)
        self.assertNotIn("--fallback-branch", self.job[self.job.index('warm_farm.py" publish'):])

    def test_the_slot_target_is_measured_only_when_a_job_builds_in_it(self):
        # A farm overlay hides the slot target and nothing writes into it, so
        # the du for the cap runs after the lookup, only without an overlay
        # (CI_TARGET_DU_DEFER=0: before, as it used to), at idle priority.
        self.assertIn('ci_idle="nice -n 19"', self.job)
        self.assertIn('if command -v ionice >/dev/null 2>&1; then ci_idle="$ci_idle ionice -c3"; fi', self.job)
        self.assertIn('ci_target_gb="$($ci_idle du -sx --block-size=1G -- "$ci_target" 2>/dev/null | cut -f1)"', self.job)
        define = self.job.index("ci_measure_target() {")
        defer = self.job.index('elif test "${CI_TARGET_DU_DEFER:-1}" = 1; then\n'
                               '          ci_target_state="reused (size unmeasured)"\n'
                               '        else\n'
                               '          ci_measure_target\n')
        deferred = self.job.index('if test "$ci_target_state" = "reused (size unmeasured)"; then\n'
                                  '        ci_measure_target\n')
        self.assertLess(define, defer)
        # After the overlay lookup (a mounted overlay renames the state), and
        # wiped and recreated before the bind at the fixed path and job code.
        self.assertLess(self.job.index('mount -t overlay ci-warm-target'), deferred)
        self.assertLess(self.job.index('ci_target_state="warm-farm ${ci_golden%% *} (slot target: $ci_target_state)"'), deferred)
        tail = self.job[deferred:]
        wipe = tail.index('rm -rf --one-file-system -- "$ci_target"\n')
        self.assertLess(wipe, tail.index('install -d -m 0755 -o ci-runner -g ci-runner -- "$ci_target"'))
        self.assertLess(wipe, tail.index('mount --bind "$ci_target" "$ci_build_target"'))
        self.assertLess(wipe, tail.index("runuser --user ci-runner --preserve-environment"))
    def test_warm_farm_publish_respects_the_io_budget(self):
        # lean-ctx-3e, 19.11.: one checkout path for every lane, so MR jobs
        # find the main generation (inherited by mr-quick and mr-lint).
        self.assertIn("    GIT_CLONE_PATH: $CI_BUILDS_DIR/$CI_CONCURRENT_ID/$CI_PROJECT_PATH\n", self.job)
        for job in (self.quick, self.lint):
            self.assertNotIn("GIT_CLONE_PATH", job)
        self.assertIn('    CI_WARM_FARM_MIN_AGE_MIN: "60"\n', self.job)
        self.assertIn('    CI_WARM_FARM_MAX_MIBPS: "100"\n', self.job)
        publish = self.job.index('warm_farm.py" publish')
        line = self.job[publish:self.job.index("\n", publish)]
        self.assertIn('--min-age-minutes "$ci_farm_min_age" --max-mib-per-s "$ci_farm_mibps"', line)
        self.assertIn("case \"$ci_farm_min_age\" in ''|*[!0-9]*|?????*) ci_farm_min_age=60;; esac", self.job)
        self.assertIn("case \"$ci_farm_mibps\" in ''|*[!0-9]*|?????*) ci_farm_mibps=100;; esac", self.job)
        # The overlay is unmounted before its upper layer is wiped, only after
        # the publish, and only when the target really is the overlay.
        umount = self.job.index('if umount -- "$CI_TARGET_DIR"; then')
        wipe = self.job.index('find "$ci_target_parent/overlay" -mindepth 1 -maxdepth 1 -exec rm -rf --one-file-system -- {} +', umount)
        self.assertLess(publish, umount)
        self.assertLess(umount, wipe)
        guard = self.job[publish:umount]
        self.assertIn('test "$(stat -f -c %T -- "$CI_TARGET_DIR")" = overlayfs', guard)
        self.assertIn('! test -L "$ci_target_parent/overlay"', guard)
        self.assertLess(wipe, self.job.index("  artifacts:\n", publish))

    def test_cargo_builds_at_slot_independent_paths(self):
        # Warm runners (owner 20.11.): a farm generation matches only the
        # paths it was built at, and both the checkout and the slot target
        # name the slot. Cargo builds at fixed in-container bind mounts, so a
        # generation published from slot 0 matches every slot.
        self.assertIn('    CI_FIXED_BUILD_PATHS: "1"\n', self.job)
        for job in (self.quick, self.lint):
            self.assertNotIn("CI_FIXED_BUILD_PATHS", job)
        src_bind = self.job.index('mount --bind "$CI_PROJECT_DIR" "/ci-build/src/$CI_PROJECT_PATH"')
        pick = self.job.index('warm_farm.py" pick')
        overlay = self.job.index('mount -t overlay ci-warm-target')
        target_bind = self.job.index('mount --bind "$ci_target" "$ci_build_target"')
        override = self.job.index('export CI_PROJECT_DIR="$ci_build_src" CI_TARGET_DIR="$ci_build_target"')
        # Set up as root after the slot target, before the pick (which must
        # name the fixed paths), and the target bind after the overlay.
        self.assertLess(self.job.index('install -d -m 0755 -o ci-runner -g ci-runner -- "$ci_target"'), src_bind)
        self.assertLess(src_bind, pick)
        line = self.job[pick:self.job.index("\n", pick)]
        self.assertIn('--project-dir "$ci_build_src" --cargo-home "$ci_build_src/rust/target/ci-cargo-home" --target-dir "$ci_build_target"', line)
        self.assertLess(overlay, target_bind)
        self.assertLess(target_bind, override)
        self.assertLess(override, self.job.index('echo "target: $ci_target_state" >'))
        self.assertLess(override, self.job.index("runuser --user ci-runner --preserve-environment"))
        # The project path is validated before it becomes a mount point; an
        # existing /ci-build or a failed checkout bind keeps the slot paths;
        # a failed target bind falls back on a slot target, fails closed on an overlay.
        self.assertLess(self.job.index("case \"${CI_PROJECT_PATH:-}\" in ''|/*|*..*|*//*|*[!A-Za-z0-9._/-]*)"), src_bind)
        # a failed target bind falls back to the slot paths (no runner change needed)
        self.assertIn('echo "build paths: cannot bind the target; building at the slot paths" >&2', self.job)
        self.assertIn('umount -- "$ci_build_src" || { echo "build paths: cannot drop the checkout bind" >&2; exit 1; }', self.job)
        self.assertIn('echo "build paths: cannot bind the target over a warm-farm overlay" >&2; exit 1', self.job)
        # a bind that cannot be dropped skips the overlay teardown (no double umount)
        self.assertIn('ci_teardown=false', self.job)
        self.assertIn('if test "$ci_teardown" = true && test -f "$ci_target_parent/overlay.base"', self.job)
        # The job user's guard accepts the fixed target, still outside the checkout.
        for job in (self.job, self.quick, self.lint):
            self.assertIn('/builds/.ci-cache/*/target/|/ci-build/target/) ;;', job)
        # Publish records the fixed paths; the bind is dropped before the
        # overlay is unmounted at the slot path.
        publish = self.job.index('warm_farm.py" publish')
        unbind = self.job.index('if mountpoint -q -- "$CI_TARGET_DIR" && umount -- "$CI_TARGET_DIR"; then')
        self.assertLess(publish, unbind)
        self.assertLess(unbind, self.job.index('if umount -- "$CI_TARGET_DIR"; then\n'))

    def test_main_smoke_links_with_mold(self):
        smoke = job_text(self.config, "main-smoke")
        self.assertIn("mold -run cargo +stable build --locked -p lean-ctx --bin lean-ctx", smoke)

    def test_build_scripts_honour_the_git_stamp_opt_out(self):
        for rel in ("rust/build.rs", "rust/crates/lean-ctx/build.rs"):
            text = (ROOT / rel).read_text(encoding="utf-8")
            self.assertIn("LEANCTX_BUILD_GIT_STAMP", text)
        engine = (ROOT / "rust/build.rs").read_text(encoding="utf-8")
        # A release build is stamped whatever the opt-out says.
        self.assertLess(engine.index("release_commit(&env)"), engine.index("if git_stamp_off()"))


class WeaveScheduleBisectTests(unittest.TestCase):
    """Weave schedule bisect (owner 20.11.): an API pipeline on weave-bisect/*
    with WEAVE_SCHEDULE_BISECT=1 runs exactly the jobs WEAVE_BISECT_JOBS names
    (plus their mandatory needs), never publishes, and queues in weave-bisect.
    """

    # Job -> the WEAVE_BISECT_JOBS names that start it. A name also starts the
    # mandatory needs of its job, or GitLab rejects the pipeline. v4-runtime
    # and main-smoke run as fixed copies (weave-bisect-<job>).
    BISECT_JOBS = {
        "rust-fmt": "(rust-fmt|v4-runtime)",
        "ip-boundary-skill-installed": "ip-boundary-skill-installed",
        "scope-register": "scope-register",
        "v4-provenance": "(v4-provenance|v4-runtime)",
        "public-api": "(public-api|change-class)",
        "change-class": "change-class",
        "weave-bisect-v4-runtime": "v4-runtime",
        "weave-bisect-main-smoke": "main-smoke",
    }

    # Jobs defined in the private ee/ tree, which the public export omits.
    PRIVATE_JOBS = ("scope-register",)

    def setUp(self):
        self.config = (ROOT / "ci/gitlab-ci.yml").read_text(encoding="utf-8")
        scope = ROOT / "ee/ci/scope-register.yml"
        self.scope = scope.read_text(encoding="utf-8") if scope.exists() else ""
        # In the public export the private jobs are absent, not unflagged.
        self.bisect_jobs = {
            name: names for name, names in self.BISECT_JOBS.items()
            if self.scope or name not in self.PRIVATE_JOBS
        }

    def jobs(self) -> dict[str, str]:
        jobs = {}
        for config in (self.config, self.scope):
            for name in re.findall(r"(?m)^([a-z][\w-]*):\n", config):
                if name not in ("workflow", "stages", "include", "variables", "default"):
                    jobs[name] = job_text(config, name)
        return jobs

    def test_workflow_admits_only_flagged_api_pipelines_on_bisect_branches(self):
        rules = re.search(r"(?ms)^workflow:\n.*?^  rules:\n(.*?)(?=^\S)", self.config).group(1)
        rules = "\n".join(line for line in rules.splitlines() if not line.lstrip().startswith("#")) + "\n"
        # The job list must be plain comma-separated job names (no spaces).
        self.assertTrue(rules.startswith(
            "    - if: '$CI_COMMIT_BRANCH =~ /^weave-bisect\\// && $CI_PIPELINE_SOURCE == \"api\" && "
            "$WEAVE_SCHEDULE_BISECT == \"1\" && $WEAVE_BISECT_JOBS =~ /^[a-z0-9-]+(,[a-z0-9-]+)*$/'\n"
            "      auto_cancel:\n        on_new_commit: none\n        on_job_failure: none\n"
            "    - if: '$CI_COMMIT_BRANCH =~ /^weave-bisect\\//'\n      when: never\n"
            "    - if: '$CI_PIPELINE_SOURCE == \"merge_request_event\" && "), rules)
        self.assertEqual(rules.count("weave-bisect"), 2)
        # No lane override: the bisect jobs take lane-p5-main like a schedule,
        # whose runners only take protected refs (weave-bisect/* is protected).
        self.assertIn("\n  CI_LANE_TAG: lane-p5-main\n", self.config)

    def test_only_the_named_jobs_run(self):
        jobs = self.jobs()
        for name, text in jobs.items():
            expected = self.bisect_jobs.get(name)
            if expected is None:
                # Merge request jobs, the push/schedule Rust jobs, anything new.
                self.assertNotIn("WEAVE_BISECT_JOBS", text, name)
                continue
            body = re.search(r"(?ms)^  rules:\n(.*?)(?=^  \S|\Z)", text).group(1)
            # The bisect rule comes first (after the project-path guard).
            self.assertTrue(
                body.startswith(bisect_first(expected))
                or body.startswith("    - if: '$CI_PROJECT_PATH != \"root/lean-ctx\"'\n      when: never\n"
                                   + bisect_first(expected)), (name, body))
        self.assertEqual(sorted(n for n, t in jobs.items() if "WEAVE_BISECT_JOBS" in t), sorted(self.bisect_jobs))
        for name in ("lint", "mr-quick", "mr-lint", "v4-runtime", "main-smoke"):
            self.assertIn(name, jobs)

    def test_mandatory_needs_come_along(self):
        jobs = self.jobs()
        starts = {name: set(names.strip("()").split("|")) for name, names in self.BISECT_JOBS.items()}
        inherited = {"weave-bisect-v4-runtime": "v4-runtime", "weave-bisect-main-smoke": "main-smoke"}
        for name in self.bisect_jobs:
            text = jobs[inherited.get(name, name)]
            for need, keys in re.findall(r"(?m)^    - job: ([\w-]+)\n((?:      .*\n)*)", text):
                if "optional: true" in keys:
                    continue
                self.assertIn(need, starts, (name, need))
                self.assertLessEqual(starts[name], starts[need], (name, need))

    def test_public_api_skips_bisect_pipelines_it_is_not_named_in(self):
        self.assertIn("  rules:\n" + PUBLIC_API_BISECT_RULES, job_text(self.config, "public-api"))

    def test_rust_jobs_queue_in_weave_bisect_and_never_write(self):
        base = job_text(self.config, ".weave-bisect-rust")
        # Fixed in the YAML: no pipeline variable moves the queue or the cache.
        self.assertTrue(base.startswith("  resource_group: weave-bisect\n  cache:\n"), base)
        self.assertEqual(re.findall(r"(?m)^      policy: (.*)$", base), ["pull"])
        self.assertNotIn("$", base.replace("${CI_PROJECT_ID}", ""))
        for job in ("v4-runtime", "main-smoke"):
            text = job_text(self.config, f"weave-bisect-{job}")
            self.assertEqual(text.rstrip("\n") + "\n", f"  extends:\n    - {job}\n    - .weave-bisect-rust\n"
                                   f"  rules:\n{bisect_first(job)}")
        # The push/schedule jobs keep their own fixed queues.
        self.assertIn("  resource_group: v4-rust-$CI_COMMIT_REF_SLUG\n", job_text(self.config, "v4-runtime"))
        self.assertIn("  resource_group: main-smoke-$CI_COMMIT_REF_SLUG\n", job_text(self.config, "main-smoke"))
        self.assertNotIn("V4_RESOURCE_GROUP", self.config)
        # The warm farm is picked and published only for a merge request, push
        # or schedule; a bisect pipeline is an API pipeline.
        runtime = job_text(self.config, "v4-runtime")
        guard = ('test "${CI_COMMIT_REF_PROTECTED:-}" = true && { test "${CI_PIPELINE_SOURCE:-}" = push '
                 '|| test "${CI_PIPELINE_SOURCE:-}" = schedule; }')
        self.assertEqual(runtime.count(guard), 2)
        self.assertNotIn("WEAVE_SCHEDULE_BISECT:-", self.config)


def load_expand():
    """bd's expansion of the CI config (gitlab_ci_expand.py, !520 review)."""
    spec = importlib.util.spec_from_file_location("gitlab_ci_expand", Path(__file__).with_name("gitlab_ci_expand.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class PipelineExpansionTests(unittest.TestCase):
    """Source x ref x flag x job list (bd, !520 review): what each pipeline
    runs, on which runner lane, in which resource group, with which cache
    policies, including pipeline variables that try to override them."""

    @classmethod
    def setUpClass(cls):
        try:
            import yaml  # noqa: F401
        except ImportError:
            raise unittest.SkipTest("PyYAML not installed")
        cls.ex = load_expand()
        cls.cfg = cls.ex.expand(str(ROOT))
        cls.private = (ROOT / "ee/ci/scope-register.yml").exists()

    def run_pipeline(self, source, branch, flag=None, jobs=None, extra=None):
        """{job: (tags, resource_group, cache policies)}, or None for no pipeline."""
        got = self.ex.pipeline(self.cfg, self.ex.base_env(source, branch, flag, jobs, extra))
        if got is None or not got["jobs"]:
            return None
        # GitLab rejects a pipeline whose mandatory needs do not run.
        self.assertEqual(got["errors"], [], (source, branch, flag, jobs))
        return {name: (job.get("tags_expanded"), job.get("resource_group_expanded"), job.get("cache_policies"),
                       job.get("rule_if", ""))
                for name, job in got["jobs"].items()}

    @staticmethod
    def summary(jobs):
        return None if jobs is None else {name: job[:3] for name, job in jobs.items()}

    def test_flag_off_or_not_a_bisect_branch_runs_no_bisect_rule(self):
        for source, branch, flag, jobs in self.ex.scenarios():
            case = (source, branch, flag, jobs)
            if (source == "api" and flag == "1" and branch.startswith("weave-bisect/") and jobs is not None
                    and re.fullmatch(r"[a-z0-9-]+(,[a-z0-9-]+)*", jobs)):
                continue
            got = self.run_pipeline(source, branch, flag, jobs)
            if branch.startswith("weave-bisect/") and source != "merge_request_event":
                # Push, api without the flag, a list with spaces: nothing.
                self.assertIsNone(got, case)
            # The flags change nothing anywhere else.
            self.assertEqual(self.summary(got), self.summary(self.run_pipeline(source, branch)), case)
            for job in (got or {}).values():
                self.assertNotIn("WEAVE_", job[3], case)

    def test_today_unchanged(self):
        main_lane = ["lane-p5-main"]
        lean_main = {"main-smoke": (main_lane, "main-smoke-main", ["pull", "pull"]),
                     "public-api": (["light"], None, None)}
        # P5_FAST_MR=0 (after the release): the lean main pipeline as before.
        self.assertEqual(self.summary(self.run_pipeline("push", "main", extra={"P5_FAST_MR": "0"})), lean_main)
        sched = self.summary(self.run_pipeline("schedule", "main"))
        self.assertEqual(sched["v4-runtime"], (main_lane, "v4-rust-main", ["pull-push", "push"]))
        self.assertEqual(sorted(sched), sorted(["change-class", "public-api", "rust-fmt", "v4-provenance", "v4-runtime"]
                                               + (["ip-boundary-skill-installed", "scope-register"] if self.private else [])))
        # Fast MR mode (default on, owner 20.11.2026): every main push runs the
        # scheduled pipeline's full suite and gates, plus main-smoke.
        self.assertEqual(self.summary(self.run_pipeline("push", "main")), {**sched, **lean_main})
        mr = self.summary(self.run_pipeline("merge_request_event", "feature/x"))
        self.assertEqual(mr["mr-quick"][:2], (["lane-p5-mr"], "mr-quick-520"))
        self.assertEqual(sorted(mr), ["lint", "mr-lint", "mr-quick"])
        # MR jobs pull the lockfile entry only (no never-written tip key).
        self.assertEqual(mr["mr-quick"][2], ["pull"])
        self.assertEqual(mr["mr-lint"][2], ["pull"])
        self.assertEqual(mr, self.summary(self.run_pipeline("merge_request_event", "feature/x",
                                                            extra={"P5_FAST_MR": "0"})))
        self.assertIsNone(self.run_pipeline("push", "TAG"))
        self.assertIsNone(self.run_pipeline("push", "feature/x"))

    def test_bisect_pipelines_run_the_named_jobs_in_weave_bisect(self):
        expected = {
            "v4-runtime": {"rust-fmt", "v4-provenance", "weave-bisect-v4-runtime"},
            "rust-fmt,public-api": {"rust-fmt", "public-api"},
            "main-smoke,v4-runtime": {"rust-fmt", "v4-provenance", "weave-bisect-v4-runtime", "weave-bisect-main-smoke"},
            "change-class": {"change-class", "public-api"},
            "v4-provenance": {"v4-provenance"},
            "scope-register": {"scope-register"} if self.private else None,
            "lint,mr-quick,mr-lint": None,
            "no-such-job": None,
        }
        # A caller cannot move the queue, the cache or the lane through the
        # variables the jobs read (M2).
        overrides = ({}, {"V4_CACHE_POLICY": "pull-push", "V4_TIP_CACHE_POLICY": "push",
                          "V4_RESOURCE_GROUP": "v4-rust-main", "V4_TIP_CACHE_KEY": "x"})
        for jobs, names in expected.items():
            for extra in overrides:
                got = self.run_pipeline("api", "weave-bisect/x", "1", jobs, extra)
                if names is None:
                    self.assertIsNone(got, jobs)
                    continue
                self.assertEqual(set(got), names, jobs)
                for name, (tags, group, cache, _) in got.items():
                    if name.startswith("weave-bisect-"):
                        # The protected main lane: weave-bisect/* is a
                        # protected branch (Maintainers), like a schedule (B1).
                        self.assertEqual(tags, ["lane-p5-main"], name)
                        self.assertEqual(group, "weave-bisect", name)
                        self.assertEqual(cache, ["pull"], name)
                    else:
                        self.assertEqual(tags, ["light"], name)
                        self.assertIsNone(group, name)


class TeamSeatCiFragmentTests(unittest.TestCase):
    """team-seat-value-v1 pins the CI files only in the lines that run its
    verifiers: an unrelated pipeline edit keeps the pin, a change to those
    lines fails it, and every other pinned file stays pinned whole."""

    PACK = "docs/contracts/team-seat-value-v1"

    def setUp(self):
        spec = importlib.util.spec_from_file_location(
            "verify_team_seat_value_v1", ROOT / "scripts/verify-team-seat-value-v1.py")
        self.verifier = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.verifier)
        manifest = json.loads((ROOT / self.PACK / "frozen-hashes.json").read_text(encoding="utf-8"))
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        for rel in [*manifest["sha256"], *manifest["ci_fragment_sha256"],
                    *manifest["registry_fragment_sha256"], self.PACK + "/frozen-hashes.json"]:
            (self.root / rel).parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / rel, self.root / rel)
        self.verifier.ROOT = self.root
        self.verifier.PACK = self.root / self.PACK

    def edit(self, rel, change):
        path = self.root / rel
        path.write_text(change(path.read_text(encoding="utf-8")), encoding="utf-8")

    def test_the_committed_pins_hold(self):
        self.verifier.verify_frozen()

    def test_another_contract_in_the_shared_registry_keeps_the_pin(self):
        # The registry files list every contract; only the lines naming this pack are frozen.
        manifest = json.loads((ROOT / self.PACK / "frozen-hashes.json").read_text(encoding="utf-8"))
        self.assertEqual(sorted(manifest["registry_fragment_sha256"]), sorted(self.verifier.REGISTRY_FILES))
        for rel in self.verifier.REGISTRY_FILES:
            self.assertNotIn(rel, manifest["sha256"])
            self.edit(rel, lambda text: text.replace("\n", "\n// another contract v9\n", 3))
        self.verifier.verify_frozen()

    def test_editing_or_dropping_a_pack_registry_line_fails_with_the_repin_command(self):
        rel = "rust/src/core/contracts.rs"
        self.edit(rel, lambda text: text.replace("team_seat_value_v1.schema_version",
                                                 "team_seat_value_v1.schema_version_v2"))
        with self.assertRaisesRegex(
                ValueError, r"frozen registry fragment: rust/src/core/contracts\.rs - the lines that name "
                r"team-seat-value-v1 changed.*repin-frozen docs/contracts/team-seat-value-v1/"
                r"frozen-hashes\.json rust/src/core/contracts\.rs"):
            self.verifier.verify_frozen()
        self.setUp()
        rel = "docs/contracts/README.md"
        self.edit(rel, lambda text: "".join(
            line for line in text.splitlines(keepends=True) if "team-seat-value" not in line))
        with self.assertRaisesRegex(ValueError, "frozen registry fragment: docs/contracts/README.md .*no longer"):
            self.verifier.verify_frozen()

    def test_a_frozen_hash_failure_names_the_repin_command(self):
        rel = self.PACK + "/README.md"
        self.edit(rel, lambda text: text + "\n")
        with self.assertRaisesRegex(ValueError, "contract freeze.*derivations.py repin-frozen " + self.PACK
                                    + "/frozen-hashes.json " + rel):
            self.verifier.verify_frozen()

    def test_an_unrelated_ci_edit_keeps_the_pin(self):
        for rel in self.verifier.CI_FILES:
            self.edit(rel, lambda text: text.replace("\n", "\n# unrelated pipeline edit\n", 3))
        self.verifier.verify_frozen()

    def test_dropping_or_editing_a_verifier_line_fails(self):
        script = "scripts/verify-team-seat-value-rust-parity.py"
        for rel in self.verifier.CI_FILES:
            with self.subTest(rel=rel, change="drop"):
                self.setUp()
                self.edit(rel, lambda text: "".join(
                    line for line in text.splitlines(keepends=True) if script not in line))
                with self.assertRaisesRegex(ValueError, "frozen CI fragment: .*no longer runs every team verifier"):
                    self.verifier.verify_frozen()
            with self.subTest(rel=rel, change="edit"):
                self.setUp()
                self.edit(rel, lambda text: text.replace(script, script + " || true"))
                with self.assertRaisesRegex(ValueError, "frozen CI fragment: .*lines that run the team verifiers changed"):
                    self.verifier.verify_frozen()

    def test_the_ci_files_are_not_also_pinned_whole_and_other_files_stay_pinned(self):
        manifest = json.loads((ROOT / self.PACK / "frozen-hashes.json").read_text(encoding="utf-8"))
        self.assertEqual(sorted(manifest["ci_fragment_sha256"]), sorted(self.verifier.CI_FILES))
        for rel in self.verifier.CI_FILES:
            self.assertNotIn(rel, manifest["sha256"])
        self.edit(self.PACK + "/README.md", lambda text: text + "\n")
        with self.assertRaisesRegex(ValueError, "frozen hash: " + self.PACK + "/README.md"):
            self.verifier.verify_frozen()

    LIB = "rust/crates/lean-ctx-protocol/src/lib.rs"
    LIB_FRAGMENT = r"frozen registry fragment: rust/crates/lean-ctx-protocol/src/lib\.rs - the lines"

    def assert_lib_edit_fails(self, change, pattern=LIB_FRAGMENT):
        self.setUp()
        self.edit(self.LIB, change)
        with self.assertRaisesRegex(ValueError, pattern):
            self.verifier.verify_frozen()

    def prefix_seat_module(self, prefix):
        return lambda text: text.replace("\nmod seat_value;", "\n" + prefix + "mod seat_value;", 1)

    def test_an_attribute_on_a_pack_module_fails_across_blank_and_comment_lines(self):
        # F2: the attribute that switches the module off or swaps its body is part of the item.
        for prefix in ("#[cfg(any())]\n",
                       "#[cfg(any())]\n\n",
                       "#[cfg(any())]\n// harmless looking comment\n",
                       "#[cfg(any())]\n/* a\n   block comment */\n",
                       "#[cfg(\n    any()\n)]\n",
                       '#[path = "elsewhere.rs"]\n',
                       '#[path = "elsewhere.rs"]\n\n// comment\n'):
            with self.subTest(prefix=prefix):
                self.assert_lib_edit_fails(self.prefix_seat_module(prefix))

    NESTED = r"frozen registry fragment: rust/crates/lean-ctx-protocol/src/lib\.rs nests a team-seat-value-v1 line"

    def wrap_pack_line(self, line, opener, closer="}\n"):
        def change(text):
            self.assertIn("\n" + line + "\n", text)
            return text.replace("\n" + line + "\n",
                                "\n" + opener + line + "\nmod evil;\npub use evil::*;\n" + closer, 1)
        return change

    def test_a_pack_line_inside_a_disabled_block_or_macro_fails(self):
        # N1: a pack line moved into a switched-off block or a swallowing macro.
        for line in ("mod seat_value;", "pub use seat_value::*;"):
            for opener, closer in (("#[cfg(any())]\nmod hidden {\n", "}\n"),
                                   ("#[cfg(any())]\n// comment\nmod hidden\n{\n", "}\n"),
                                   ("drop_all! {\n", "}\n"),
                                   ("drop_all!(\n", ");\n"),
                                   ("const _: () = {\n", "};\n")):
                with self.subTest(line=line, opener=opener):
                    self.assert_lib_edit_fails(self.wrap_pack_line(line, opener, closer), self.NESTED)

    def test_a_disabled_block_around_a_nested_pack_line_changes_the_fragment(self):
        # N1 outside the crate root: the opener chain (with its attributes) is pinned.
        text = "fn f() {\n    let v = vec![\n        (\"a\", SEAT_VALUE),\n    ];\n}\n"
        base = self.verifier.rust_registry_fragment(text)
        self.assertIn('block: fn f() {', base)
        for wrapped in (text.replace("    let v", "    #[cfg(any())]\n    {\n    let v", 1)
                        .replace("    ];\n", "    ];\n    }\n", 1),
                        text.replace("    let v", "    drop_all! {\n    let v", 1)
                        .replace("    ];\n", "    ];\n    }\n", 1)):
            with self.subTest(wrapped=wrapped):
                self.assertNotEqual(self.verifier.rust_registry_fragment(wrapped), base)
        unrelated = text.replace("fn f() {\n", "fn f() {\n    let u = [1, 2];\n", 1)
        self.assertEqual(self.verifier.rust_registry_fragment(unrelated), base)

    MASKED = r"frozen registry fragment: rust/crates/lean-ctx-protocol/src/lib\.rs names team-seat-value-v1 only inside"
    HIDERS = (("/*\n", "\n*/"),
              ("// ", ""),
              ('const _: &str = r#"\n', '\n"#;'),
              ('const _: &str = "\n', '\n";'),
              ('const _: &core::ffi::CStr = c"\n', '\n";'),
              # N3: a raw C string whose body holds `";` must not end at that quote.
              ('const _: &core::ffi::CStr = cr#"x";\n', '\n";"#;'))

    def test_a_pack_line_inside_a_comment_or_string_fails_in_the_crate_root(self):
        # N2: a commented-out or quoted pack line must not equal the real line.
        for line in ("mod seat_value;", "pub use seat_value::*;"):
            for before, after in self.HIDERS:
                with self.subTest(line=line, hider=before):
                    self.assert_lib_edit_fails(
                        lambda text, line=line, before=before, after=after: text.replace(
                            "\n" + line + "\n", "\n" + before + line + after + "\n", 1),
                        self.MASKED)

    def test_a_pack_line_inside_a_comment_or_string_changes_a_nested_fragment(self):
        # N2 outside the crate root: the hidden line is pinned as `comment:`.
        text = "fn f() {\n    x(SEAT_VALUE);\n}\n"
        base = self.verifier.rust_registry_fragment(text)
        self.assertIn("x(SEAT_VALUE);", base)
        for before, after in self.HIDERS:
            hidden = text.replace("    x(SEAT_VALUE);", before + "x(SEAT_VALUE);" + after, 1)
            with self.subTest(hider=before):
                fragment = self.verifier.rust_registry_fragment(hidden)
                self.assertNotEqual(fragment, base)
                self.assertNotIn("x(SEAT_VALUE);", fragment)

    def test_an_attribute_on_an_unrelated_item_above_a_pack_module_keeps_the_pin(self):
        self.edit(self.LIB, self.prefix_seat_module("#[cfg(test)]\nmod unrelated_tests {}\n\n"))
        self.verifier.verify_frozen()

    def test_a_comment_between_pack_lines_keeps_the_pin(self):
        self.edit(self.LIB, self.prefix_seat_module("// a comment that names no pack\n\n"))
        self.verifier.verify_frozen()

    def test_an_explicit_reexport_that_shadows_a_pack_item_fails(self):
        # F3: an explicit item beats the glob `pub use seat_value::*`.
        for shadow in ("pub use money::MoneyV1 as SeatUsageV1;\n",
                       "pub use money::{\n    MoneyV1 as SeatUsageV1,\n};\n",
                       "pub struct SeatUsageV1;\n"):
            with self.subTest(shadow=shadow):
                self.assert_lib_edit_fails(
                    lambda text, shadow=shadow: text + shadow,
                    r"lib\.rs shadows a team-seat-value-v1 item outside the pack's own lines")

    def test_rerouting_a_building_block_the_pack_imports_fails(self):
        # F3: `use crate::{TeamId, ..}` in the pack resolves through the crate root.
        public, imported = self.verifier.pack_module_names()
        self.assertIn("TeamId", imported)
        self.assertNotIn("TeamId", public)
        self.assert_lib_edit_fails(lambda text: text + "pub use money::MoneyV1 as TeamId;\n")
        self.assert_lib_edit_fails(lambda text: text.replace("pub use team_context::*;",
                                                             "pub use team_context::{TeamId};", 1))

    def test_an_unrelated_root_reexport_keeps_the_pin(self):
        self.edit(self.LIB, lambda text: text + "\npub use money::MoneyV1 as UnrelatedMoneyV1;\n")
        self.verifier.verify_frozen()

    def test_rust_code_blanks_comments_and_literals_but_keeps_lines(self):
        text = 'a // x [\nb /* [ /* ] */ */ "s [" r#"q " ]"# \'[\' c\n'
        code = self.verifier.rust_code(text)
        self.assertEqual(len(code), len(text))
        self.assertEqual(code.count("\n"), text.count("\n"))
        self.assertNotIn("[", code)
        self.assertNotIn("]", code)

    def test_rust_code_blanks_c_strings_and_raw_c_strings(self):
        # N3: edition 2024 `c"..."` and `cr#"..."#` are literals too.
        for text in ('x c"a [ b" y\n', 'x cr#"a"; [ ]"# y\n', 'x cr"a [" y\n', 'x br#"a"; ["# y\n'):
            with self.subTest(text=text):
                code = self.verifier.rust_code(text)
                self.assertEqual(len(code), len(text))
                self.assertNotIn("[", code)
                self.assertTrue(code.startswith("x ") and code.endswith(" y\n"), code)


if __name__ == "__main__":
    unittest.main()
