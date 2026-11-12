#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Guard the Rust gates of the private mirror; GitLab CI lint validates YAML.

Shape (owner 08.11.): every merge request runs the blocking fast gate
`mr-quick`; the full suite `v4-runtime` runs after the merge on integration/*
and the default branch.
"""

from pathlib import Path
import importlib.util
import json
import re
import shutil
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
# On merge requests every cheap gate is path-scoped (ci-slim 11.11.), so
# mr-quick and mr-lint need each of them optionally.
MR_GATE_NEEDS = [(name, True) for name, _ in CHEAP_GATE_NEEDS]

# What the Rust gates read on a merge request (.rust-changes-on-mr).
RUST_GATE_INPUTS = ("rust/**/*", ".cargo/**/*", "ci/**/*")
MR_RULE = "'$CI_PIPELINE_SOURCE == \"merge_request_event\"'"
TRAIN_RULE = ("'$CI_PIPELINE_SOURCE == \"merge_request_event\" && "
              "$CI_MERGE_REQUEST_EVENT_TYPE == \"merge_train\"'")


RUST_INPUTS = (
    '"*"', "rust/**/*", ".cargo/**/*", "ci/**/*", "docs/**/*", "tests/**/*",
    "packages/**/*", "kits/**/*", "product/**/*", "scripts/**/*",
    "security/**/*", "skills/**/*", "integrations/**/*",
)


CLIPPY_LIB = r'cargo +stable clippy --locked --all-features --config profile.clippy-lib.inherits=\"dev\" --profile clippy-lib -- -D warnings'


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
        self.assertIn('cp "$CARGO_TARGET_DIR/nextest/ci/junit.xml" "$CI_PROJECT_DIR/runtime-evidence/junit.xml"', self.job)
        self.assertRegex(self.job, r"reports:\n\s+junit: runtime-evidence/junit\.xml\n")
        self.assertIn("cargo-nextest", self.job)
        nextest_toml = (ROOT / "rust/.config/nextest.toml").read_text(encoding="utf-8")
        ci = nextest_toml[nextest_toml.index("[profile.ci]"):]
        # v4-runtime keeps one test thread per granted CPU (owner 08.11.);
        # owner 12.11. lets mr-quick alone pass --test-threads 8.
        for setting in ('test-threads = "num-cpus"', "retries = 0", "fail-fast = false",
                        'slow-timeout = { period = "60s", terminate-after = 5 }',
                        '[profile.ci.junit]\npath = "junit.xml"'):
            self.assertIn(setting, ci)
        # No retry anywhere may mask a flake.
        self.assertNotRegex(nextest_toml, r"(?m)^retries = [1-9]")
        self.assertIn("runtime-evidence/tests.log", self.job)
        self.assertIn("runtime-evidence/test-features.txt", self.job)
        self.assertIn(CLIPPY_LIB, self.job)
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
        self.assertEqual(self.config.count("cargo +stable check"), 1)
        self.assertIn(check, self.job)
        self.assertNotIn("--features", check)
        self.assertLess(self.job.index("cargo +stable test --locked --doc"), self.job.index(check))

    def test_failed_tests_are_retried_once_through_the_flake_gate(self):
        gate = ('python3 -I -u "$CI_PROJECT_DIR/scripts/ci/flake_gate.py"'
                ' --junit "$CI_PROJECT_DIR/runtime-evidence/junit.xml"'
                ' --rerun-junit "$CARGO_TARGET_DIR/nextest/ci/junit.xml"'
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
        copy = 'cp "$CARGO_TARGET_DIR/nextest/ci/junit.xml" "$CI_PROJECT_DIR/runtime-evidence/junit.xml"'
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
        for name in ("rust-fmt", "ip-boundary-skill-installed", "v4-runtime", "v4-provenance",
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
            self.assertIn("!reference [.interruptible-on-mr, rules]", job)
            self.assertIn('"${CI_MERGE_REQUEST_DIFF_BASE_SHA:-$(git rev-parse HEAD^1)}"', job)
        self.assertIn("  needs: []\n", api)
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
        for bypass in ("allow_failure:", "when: manual", "when: never", "changes:", "exists:", "only:", "except:"):
            self.assertNotIn(bypass, provenance.group(1))
        # Its only rules are the shared interruptible switch, which always
        # adds the job: an MR rule plus an unconditional catch-all, neither
        # of which can drop it.
        self.assertIn("  rules:\n    - !reference [.interruptible-on-mr, rules]\n  needs: []\n",
                      provenance.group(1))
        rules = job_text(self.config, ".interruptible-on-mr").rstrip("\n") + "\n"
        self.assertEqual(
            rules,
            "  rules:\n"
            "    - if: '$CI_PIPELINE_SOURCE == \"merge_request_event\"'\n"
            "      interruptible: true\n"
            "    - when: on_success\n"
            "      interruptible: false\n",
        )
        # On merge requests provenance runs beside the heavy jobs instead of
        # in front of them; a red provenance still stops the pipeline at once.
        self.assertIn("  auto_cancel:\n    on_new_commit: interruptible\n    on_job_failure: all\n",
                      self.config)

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
        # Waits only for the cheap gates (about a minute), never for
        # provenance, and overrides v4-runtime's list instead of inheriting it.
        self.assertEqual(needs_of(self.quick), MR_GATE_NEEDS)
        self.assertIn("  interruptible: true\n", self.quick)
        fmt = job_text(self.config, "rust-fmt")
        self.assertIn("  needs: []\n", fmt)
        self.assertIn("cargo +stable fmt --all --check", fmt)
        for bypass in ("allow_failure", "when: manual", "only:", "except:"):
            self.assertNotIn(bypass, fmt)

    # -- path-scoped merge request gates (ci-slim 11.11.) ------------------

    def test_rust_gate_inputs_cover_every_rust_input(self):
        shared = job_text(self.config, ".rust-changes-on-mr")
        # The anchor holds nothing but its rules.
        self.assertTrue(shared.startswith("  rules:\n"), shared)
        blocks = re.split(r"(?m)^    - if: ", shared)[1:]
        self.assertEqual(len(blocks), 2, blocks)
        # Merge trains test what lands: never path-scoped.
        self.assertTrue(blocks[0].startswith(TRAIN_RULE + "\n"), blocks[0])
        self.assertNotIn("changes:", blocks[0])
        self.assertNotIn("when:", blocks[0])
        self.assertTrue(blocks[1].startswith(MR_RULE + "\n"), blocks[1])
        for path in RUST_GATE_INPUTS:
            self.assertIn(f"          - {path}\n", blocks[1])
        self.assertNotIn("when:", blocks[1])

    def test_rust_fmt_always_runs_on_branches_and_on_rust_merge_requests(self):
        fmt = job_text(self.config, "rust-fmt")
        self.assertIn(
            "  rules:\n"
            "    - !reference [.rust-changes-on-mr, rules]\n"
            f"    - if: {MR_RULE}\n"
            "      when: never\n"
            "    - when: on_success\n"
            "      interruptible: false\n"
            "  needs: []\n", fmt)

    def test_lint_runs_on_rust_merge_requests_and_trains_only(self):
        self.assertIn("  rules:\n    - !reference [.rust-changes-on-mr, rules]\n", self.lint)
        # Nothing else: no branch catch-all (v4-runtime lints branches).
        rules = re.search(r"(?ms)^  rules:\n(.*?)(?=^  \S)", self.lint).group(1)
        self.assertEqual(rules, "    - !reference [.rust-changes-on-mr, rules]\n")

    def test_skill_gate_is_scoped_to_its_inputs_on_merge_requests_only(self):
        ip = job_text(self.config, "ip-boundary-skill-installed")
        blocks = rule_blocks(ip)
        self.assertEqual(len(blocks), 4, blocks)
        self.assertIn('$CI_PROJECT_PATH != "root/lean-ctx"', blocks[0])
        self.assertTrue(blocks[1].startswith(TRAIN_RULE + "\n"), blocks[1])
        self.assertNotIn("changes:", blocks[1])
        self.assertTrue(blocks[2].startswith(MR_RULE + "\n"), blocks[2])
        for path in (".agents/**/*", ".claude/**/*", ".cursor/**/*", "AGENTS.md", "ci/**/*"):
            self.assertIn(f"          - {path}\n", blocks[2])
        self.assertTrue(blocks[3].startswith(MR_RULE + "\n      when: never\n"), blocks[3])
        self.assertIn("    - when: on_success\n      interruptible: false\n  needs: []\n", ip)

    def test_scope_register_is_scoped_to_its_inputs_on_merge_requests_only(self):
        scope = ROOT / "ee/ci/scope-register.yml"
        if not scope.exists():
            self.skipTest("private tree absent")
        job = job_text(scope.read_text(encoding="utf-8"), "scope-register")
        blocks = rule_blocks(job)
        self.assertEqual(len(blocks), 3, blocks)
        self.assertTrue(blocks[0].startswith(TRAIN_RULE + "\n"), blocks[0])
        self.assertNotIn("changes:", blocks[0])
        self.assertTrue(blocks[1].startswith(MR_RULE + "\n"), blocks[1])
        for path in ("rust/src/**/*", "rust/crates/**/*", "packages/**/*", "sdk/**/*", "ee/**/*", "ci/**/*"):
            self.assertIn(f"          - {path}\n", blocks[1])
        self.assertTrue(blocks[2].startswith(MR_RULE + "\n      when: never\n"), blocks[2])
        self.assertIn("    - when: on_success\n      interruptible: false\n  needs: []\n", job)

    def test_lint_gate_runs_beside_the_quick_gate_on_every_merge_request(self):
        # Inherits mr-quick's cheap-gate needs, allow_failure: false and
        # read-only caches; overrides the script, its own lane and its rules
        # (Rust inputs only, test_lint_runs_on_rust_merge_requests_and_trains_only).
        self.assertIn("  extends: mr-quick\n", self.lint)
        self.assertIn("  resource_group: mr-lint-$CI_MERGE_REQUEST_IID\n", self.lint)
        for override in ("needs:", "allow_failure", "variables:", "when: manual", "when: never", "only:", "except:", "push"):
            self.assertNotIn(override, self.lint)
        # Format is rust-fmt's, which mr-lint needs (W1-08: no gate twice).
        self.assertNotIn("cargo +stable fmt", self.lint)
        # Same clippy invocation as the full suite, so the cache applies.
        self.assertIn(CLIPPY_LIB, self.lint)
        self.assertNotIn("mr_quick.py", self.lint)
        self.assertNotIn("nextest", self.lint)
        self.assertIn('GIT_OPTIONAL_LOCKS=0 git -C "$CI_PROJECT_DIR" diff --exit-code HEAD --', self.lint)
        # The quick gate no longer lints serially before its test build.
        self.assertNotIn("cargo +stable clippy", self.quick)
        self.assertNotIn("cargo +stable fmt", self.quick)

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
        self.assertIn('"cargo", "+stable", "nextest", *args, "--locked", "--workspace",\n'
                      '            "--features", features]', script)
        self.assertIn('nextest_cmd(features, "list", "--message-format", "json")', script)
        self.assertIn('nextest_cmd(features, "run", "--profile", NEXTEST_PROFILE, "--no-fail-fast",\n'
                      '                      "--test-threads", str(MR_QUICK_TEST_THREADS))', script)
        # Owner 12.11.: mr-quick alone runs 8 test threads on the 6-CPU job
        # cgroup (replaces the num-cpus rule of 08.11. for mr-quick only).
        self.assertIn("MR_QUICK_TEST_THREADS = 8\n", script)
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
        # One target per concurrent build directory (11.11., p5 job 32345:
        # two concurrent jobs in one target lost each other's test binaries).
        self.assertIn('    CI_TARGET_DIR: "/builds/.ci-cache/$CI_PROJECT_PATH_SLUG/slot-$CI_RUNNER_SHORT_TOKEN-$CI_CONCURRENT_PROJECT_ID/target"\n', self.job)
        self.assertIn('    CI_TARGET_CAP_GB: "30"\n', self.job)
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
            "case \"${CI_RUNNER_SHORT_TOKEN:-}\" in ''|*[!A-Za-z0-9_-]*)",
            "case \"${CI_CONCURRENT_PROJECT_ID:-}\" in ''|*[!0-9]*)",
            'ci_target_parent="/builds/.ci-cache/$CI_PROJECT_PATH_SLUG/slot-$CI_RUNNER_SHORT_TOKEN-$CI_CONCURRENT_PROJECT_ID"',
            'ci_target="$ci_target_parent/target"',
            'test "${CI_TARGET_DIR:-}" = "$ci_target" || {',
            'case "$ci_target/" in "$CI_PROJECT_DIR"/*) echo "target: inside the checkout" >&2; exit 1;; esac',
            'if test -L /builds; then',
            'for ci_dir in /builds/.ci-cache "/builds/.ci-cache/$CI_PROJECT_PATH_SLUG" "$ci_target_parent"; do',
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
        self.assertIn('du -sx --block-size=1G -- "$ci_target" 2>/dev/null || test "$?" = 1;', self.job)
        self.assertIn('elif test "$ci_target_gb" -gt "$CI_TARGET_CAP_GB"; then', self.job)
        self.assertIn('ci_target_state="wiped (size unknown)"', self.job)
        self.assertIn('ci_target_state="wiped ($ci_target_gb GB > cap)"', self.job)
        self.assertIn('"$(rustc +stable -vV)" | sha256sum', self.job)
        self.assertIn('printf \'%s\\n\' "$ci_stamp" > "$ci_target_parent/target.stamp"', self.job)
        self.assertIn('wiped*) rm -rf --one-file-system -- "$ci_target";;', self.job)
        self.assertIn('rm -rf --one-file-system -- "$ci_target/nextest"', self.job)
        self.assertNotIn("rm -rf --no-preserve-root", self.job)
        self.assertIn('echo "target: $ci_target_state" > "$CI_PROJECT_DIR/runtime-evidence/target.txt"', self.job)
        # Set up as root before any job code runs, after the identity exists.
        setup = self.job.index('ci_target_parent="/builds/.ci-cache/')
        self.assertLess(self.job.index("useradd --system --uid 10999"), setup)
        self.assertLess(setup, self.job.index("runuser --user ci-runner --preserve-environment"))
        # The checkout guards are unchanged: rust/target (CARGO_HOME) stays
        # pruned, and the persistent target is not inside the scanned tree.
        self.assertNotIn('-path "/builds/.ci-cache', self.job)


    def test_concurrent_jobs_of_a_slot_never_wipe_a_target_in_use(self):
        # 11.11. (p5 jobs 32317/32318/32321): jobs of one slot share the
        # target. Each holds a shared lock for the whole job; only a job that
        # gets the lock exclusively without waiting wipes or cleans up.
        lock = 'ci_target_lock="$ci_target_parent/target.lock"'
        self.assertIn(lock, self.job)
        self.assertIn('if test -L "$ci_target_lock"; then echo "target: $ci_target_lock is a symlink" >&2; exit 1; fi', self.job)
        self.assertIn('test "$(stat -c %u -- "$ci_target_lock")" != 0', self.job)
        self.assertIn('chmod 0600 -- "$ci_target_lock"', self.job)
        exclusive = self.job.index("if flock -xn 8; then")
        busy = self.job.index('ci_target_state="reused (slot busy: no wipe)"')
        shared = self.job.index('flock -s -w 1800 9 || {')
        self.assertLess(self.job.index('exec 8>>"$ci_target_lock"'), exclusive)
        # Every removal in the slot happens under the exclusive lock.
        for removal in (
            'wiped*) rm -rf --one-file-system -- "$ci_target";;',
            'rm -rf --one-file-system -- "$ci_target/nextest"',
            'du -sx --block-size=1G',
            'rm -f -- "$ci_target_parent/target.stamp"',
            'for ci_old in "$ci_target_parent/overlay" "$ci_target_parent/overlay.base" "$ci_target_parent"/overlay.[0-9]*; do',
        ):
            self.assertEqual(self.job.count(removal), 1, removal)
            self.assertLess(exclusive, self.job.index(removal))
            self.assertLess(self.job.index(removal), busy)
        # The exclusive fd is closed before the shared lock is taken, and the
        # target is (re)validated only after that.
        self.assertLess(busy, self.job.index("exec 8>&-"))
        self.assertLess(self.job.index("exec 8>&-"), self.job.index('exec 9>>"$ci_target_lock"'))
        self.assertLess(self.job.index('exec 9>>"$ci_target_lock"'), shared)
        self.assertLess(shared, self.job.index('install -d -m 0755 -o ci-runner -g ci-runner -- "$ci_target"'))
        self.assertLess(shared, self.job.index('mount -t overlay ci-warm-target'))
        self.assertIn(" flock ", self.job)  # the image must carry it
        # The lock stays held in script: each Rust job checks fd 9 first.
        fd_check = '{ : >&9; } 2>/dev/null || { echo "target: the slot lock is not held in script" >&2; exit 1; }'
        for job in (self.job, self.quick, self.lint):
            self.assertIn(fd_check, job)
            self.assertLess(job.index(fd_check), job.index("runuser --user ci-runner --preserve-environment"))
        self.assertNotIn("exec 9>&-", self.config)
        self.assertNotIn("flock -u 9", self.config)

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
        # push reads (and later publishes) its own branch.
        self.assertIn('ci_farm_branch="${CI_MERGE_REQUEST_TARGET_BRANCH_NAME:-}"', self.job)
        self.assertIn('elif test "${CI_COMMIT_REF_PROTECTED:-}" = true && test "${CI_PIPELINE_SOURCE:-}" = push; then\n'
                      '        ci_farm_branch="${CI_COMMIT_BRANCH:-}"', self.job)
        self.assertIn('if ! mount -o remount,bind,ro "$ci_farm"; then', self.job)
        self.assertIn('touch "$ci_farm/.ci-write-probe" 2>/dev/null; then echo "warm-farm: $ci_farm is still writable" >&2; exit 1; fi', self.job)
        self.assertLess(self.job.index('mount -o remount,bind,ro "$ci_farm"'), mount)
        # Each job its own upper layer (a concurrent job of the slot has its own).
        self.assertIn('ci_overlay="$ci_target_parent/overlay.$CI_JOB_ID"', self.job)
        self.assertIn('rm -rf --one-file-system -- "$ci_overlay"', self.job)
        self.assertIn('install -d -m 0755 -o ci-runner -g ci-runner -- "$ci_overlay/upper"', self.job)
        self.assertIn('upperdir=$ci_overlay/upper,workdir=$ci_overlay/work', self.job)
        # Publish reads this job's own stamp and base, never the slot's shared ones.
        self.assertIn('--stamp "$(cat -- /run/ci-target/stamp)"', self.job)
        self.assertIn('--started-file /run/ci-target/stamp', self.job)
        self.assertIn('printf \'%s\\n\' "${ci_golden##* }" > /run/ci-target/overlay.base', self.job)
        self.assertNotIn('"$ci_target_parent/overlay.base")"', self.job)
        # A missed cargo-home cache is seeded from the picked generation's
        # crates (owner 12.11.), by ci-runner, never over a restored cache.
        seed = self.job.index('ci_seed="${ci_golden%% *}/.warm-farm-cargo-home"')
        self.assertLess(self.job.index('> /run/ci-target/overlay.base'), seed)
        self.assertIn('! test -d "$CI_PROJECT_DIR/rust/target/ci-cargo-home/registry/index"; then', self.job)
        self.assertIn("runuser --user ci-runner -- /bin/sh -ec 'mkdir -p -- \"$2\" && cp -R --preserve=mode,timestamps"
                      " -- \"$1/.\" \"$2/\"' sh \"$ci_seed\" \"$CI_PROJECT_DIR/rust/target/ci-cargo-home\"", self.job)
        self.assertLess(seed, self.job.index('mount --bind "$CI_PROJECT_DIR" "$ci_view/src"'))
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

    def test_a_red_post_merge_run_names_its_suspects(self):
        # Tiered CI (owner 11.11.): MRs run only affected tests, so a red
        # post-merge run must point at the merges since the last green one.
        call = 'postmerge_bisect.py" --repo "$CI_PROJECT_DIR" --farm "${CI_WARM_FARM:-}"'
        self.assertEqual(self.job.count(call), 1)
        guard = self.job.index('rc=1; fi; if test "${CI_PIPELINE_SOURCE:-}" = push')
        self.assertLess(guard, self.job.index(call))
        self.assertLess(self.job.index(call), self.job.index('exit "$rc"; }'))
        self.assertIn("runtime-evidence/bisect.txt", self.job)

    def test_every_rust_job_builds_at_fixed_paths(self):
        # CI-v2 (11.11.): the checkout and slot target paths differ per runner
        # and slot, so merge-request jobs never matched a warm-farm generation
        # of the post-merge runner. Every job binds both to fixed paths.
        self.assertIn("      ci_view=/ci-build\n", self.job)
        self.assertIn('--project-dir "$ci_view/src" --cargo-home "$ci_view/src/rust/target/ci-cargo-home" --target-dir "$ci_view/target")"', self.job)
        self.assertNotIn('--project-dir "$CI_PROJECT_DIR" --cargo-home "$CI_PROJECT_DIR/rust/target/ci-cargo-home" --target-dir "$ci_target"', self.job)
        bind_src = self.job.index('mount --bind "$CI_PROJECT_DIR" "$ci_view/src"')
        bind_target = self.job.index('mount --bind "$ci_target" "$ci_view/target"')
        export = self.job.index('export CI_PROJECT_DIR="$ci_view/src" CI_TARGET_DIR="$ci_view/target"')
        # The overlay is mounted first, so the bind shows the warm target.
        self.assertLess(self.job.index('mount -t overlay ci-warm-target'), bind_target)
        self.assertLess(bind_src, export)
        self.assertLess(bind_target, export)
        self.assertLess(export, self.job.index("runuser --user ci-runner --preserve-environment"))
        # Publish records the fixed paths (it reads the exported variables).
        self.assertLess(export, self.job.index('warm_farm.py" publish'))
        self.assertIn('--project-dir "$CI_PROJECT_DIR" --cargo-home "$CI_PROJECT_DIR/rust/target/ci-cargo-home" --target-dir "$CI_TARGET_DIR"', self.job)
        self.assertIn('if test -e "$ci_view" || test -L "$ci_view"; then', self.job)
        for job in (self.job, self.quick, self.lint):
            self.assertIn('/ci-build/target/) ;;', job)
            self.assertNotIn('/builds/.ci-cache/*/target/) ;;', job)

    def test_build_scripts_honour_the_git_stamp_opt_out(self):
        for rel in ("rust/build.rs", "rust/crates/lean-ctx/build.rs"):
            text = (ROOT / rel).read_text(encoding="utf-8")
            self.assertIn("LEANCTX_BUILD_GIT_STAMP", text)
        engine = (ROOT / "rust/build.rs").read_text(encoding="utf-8")
        # A release build is stamped whatever the opt-out says.
        self.assertLess(engine.index("release_commit(&env)"), engine.index("if git_stamp_off()"))


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


if __name__ == "__main__":
    unittest.main()
