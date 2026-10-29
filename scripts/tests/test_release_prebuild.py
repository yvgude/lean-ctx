# SPDX-License-Identifier: Apache-2.0
"""scripts/release-prebuild.py: only the release merge commit (or a dispatch)
prebuilds; a tag reuses a main prebuild only for the same commit, tree,
version, compiled-in build configuration, workflow and run, with every file
byte-identical to the attested manifest; anything else falls back to a build."""
import base64
import contextlib
import hashlib
import importlib.util
import io
import json
import os
import re
import tempfile
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("release_prebuild", ROOT / "scripts/release-prebuild.py")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)

REPO = "yvgude/lean-ctx"
SHA = "a" * 40
BEFORE = "e" * 40
TREE = "b" * 40
VERSION = "3.12.1"
RUN = 4242
CONFIG = MODULE.config_digest({})


def _run(**overrides):
    run = {"id": RUN, "run_number": 7, "run_attempt": 1, "head_sha": SHA, "head_branch": "main",
           "event": "push", "path": ".github/workflows/release.yml", "status": "completed",
           "conclusion": "success", "repository": {"full_name": REPO},
           "head_repository": {"full_name": REPO}}
    run.update(overrides)
    return run


def _artifacts(run_id=RUN, sha=SHA, drop=None, extra=(), **fields):
    names = [MODULE.MANIFEST_ARTIFACT, *(f"lean-ctx-{a}" for a in MODULE.ARTIFACTS), *extra]
    return [{"name": n, "expired": False, "workflow_run": {"id": run_id, "head_sha": sha}, **fields}
            for n in names if n != drop]


def _wheel(artifact):
    distribution = ("thinkery_leanctx_engine_cuda" if artifact.endswith("-cuda")
                    else "thinkery_leanctx_engine_windows_gnu" if artifact.endswith("windows-gnu")
                    else "thinkery_leanctx_engine")
    return f"{distribution}-{VERSION}-py3-none-any_{artifact.replace('-', '_')}.whl"


def _cargo(version):
    return f'[package]\nname = "lean-ctx"\nversion = "{version}"\n'


def _contents(version):
    return 200, {"encoding": "base64", "content": base64.b64encode(_cargo(version).encode()).decode()}


class DecideTests(unittest.TestCase):
    def test_only_the_version_change_push_or_a_dispatch_prebuilds(self):
        decide = MODULE.decide
        self.assertEqual(decide("push", "refs/tags/v3.12.1", VERSION), "release")
        self.assertEqual(decide("push", "refs/heads/main", VERSION, before_version="3.12.0", released=False),
                         "prebuild")
        # Same version as before the push: an ordinary main commit.
        self.assertEqual(decide("push", "refs/heads/main", VERSION, before_version=VERSION, released=False), "skip")
        # New branch / forced push / unknown before, or unknown publication state.
        self.assertEqual(decide("push", "refs/heads/main", VERSION, before_version=None, released=False), "skip")
        self.assertEqual(decide("push", "refs/heads/main", VERSION, before_version="3.12.0", released=None), "skip")
        self.assertEqual(decide("push", "refs/heads/main", VERSION, before_version="3.12.0", released=True), "skip")
        self.assertEqual(decide("push", "refs/heads/main", "3.13.0-rc.1", before_version=VERSION, released=False),
                         "skip")
        self.assertEqual(decide("workflow_dispatch", "refs/heads/main", VERSION), "prebuild")
        self.assertEqual(decide("workflow_dispatch", "refs/heads/main", "3.13.0-rc.1"), "skip")
        self.assertEqual(decide("workflow_dispatch", "refs/heads/feature", VERSION), "skip")
        self.assertEqual(decide("workflow_dispatch", "refs/tags/v3.12.1", VERSION), "skip")
        self.assertEqual(decide("push", "refs/heads/feature", VERSION, before_version="3.12.0", released=False),
                         "skip")

    def test_cargo_version_is_the_engine_version(self):
        self.assertTrue(MODULE.TAG_VERSION.fullmatch(MODULE.cargo_version(ROOT)))


class PlanTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        (self.root / "rust").mkdir()
        (self.root / "rust/Cargo.toml").write_text(_cargo(VERSION))
        self.output = self.root / "github_output"
        self.output.write_text("")

    def tearDown(self):
        self.tmp.cleanup()

    def _plan(self, event, ref, before="", api=None):
        calls = []

        def fake(path, _token):
            calls.append(path)
            return api(path) if api else (404, None)

        argv = ["plan", "--event", event, "--ref", ref, "--repo", REPO, "--before", before,
                "--root", str(self.root)]
        with mock.patch.object(MODULE, "_api", fake), \
                mock.patch.dict(os.environ, {"GITHUB_OUTPUT": str(self.output)}), \
                mock.patch.object(MODULE.time, "sleep", lambda _s: None), \
                contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            code = MODULE.main(argv)
        values = dict(line.split("=", 1) for line in self.output.read_text().splitlines())
        self.output.write_text("")
        return code, values, calls

    def test_release_merge_push_prebuilds(self):
        api = lambda path: _contents("3.12.0") if "/contents/" in path else (404, None)
        code, values, calls = self._plan("push", "refs/heads/main", BEFORE, api)
        self.assertEqual((code, values["mode"], values["version"]), (0, "prebuild", VERSION))
        self.assertIn(f"/repos/{REPO}/contents/rust/Cargo.toml?ref={BEFORE}", calls)

    def test_unchanged_version_skips_without_asking_for_releases(self):
        code, values, calls = self._plan("push", "refs/heads/main", BEFORE, lambda path: _contents(VERSION))
        self.assertEqual((code, values["mode"]), (0, "skip"))
        self.assertFalse(any("/releases/" in c for c in calls))

    def test_new_branch_push_never_prebuilds(self):
        code, values, calls = self._plan("push", "refs/heads/main", "0" * 40)
        self.assertEqual((code, values["mode"], calls), (0, "skip", []))

    def test_api_errors_mean_no_automatic_prebuild(self):
        def broken(path):
            if "/contents/" in path:
                return _contents("3.12.0")
            raise MODULE.PrebuildError("GitHub API down")
        code, values, _ = self._plan("push", "refs/heads/main", BEFORE, broken)
        self.assertEqual((code, values["mode"]), (0, "skip"))
        with mock.patch.object(MODULE, "_api", lambda *_a: (_ for _ in ()).throw(MODULE.PrebuildError("x"))), \
                contextlib.redirect_stderr(io.StringIO()):
            self.assertIsNone(MODULE.release_published(REPO, VERSION, None))
            self.assertIsNone(MODULE.version_at(REPO, BEFORE, None))

    def test_tags_are_releases_and_prereleases_never_reuse(self):
        code, values, _ = self._plan("push", "refs/tags/v3.12.1")
        self.assertEqual((code, values["mode"], values["version"], values["reusable"]),
                         (0, "release", VERSION, "true"))
        code, values, _ = self._plan("push", "refs/tags/v3.13.0-rc.1")
        self.assertEqual((code, values["mode"], values["version"], values["reusable"]),
                         (0, "release", "3.13.0-rc.1", "false"))

    def test_prerelease_main_version_is_no_automatic_prebuild(self):
        (self.root / "rust/Cargo.toml").write_text(_cargo("3.13.0-rc.1"))
        code, values, calls = self._plan("push", "refs/heads/main", BEFORE, lambda path: _contents(VERSION))
        self.assertEqual((code, values["mode"], calls), (0, "skip", []))
        code, values, _ = self._plan("workflow_dispatch", "refs/heads/main")
        self.assertEqual((code, values["mode"]), (0, "skip"))

    def test_dispatch_on_main_prebuilds(self):
        code, values, _ = self._plan("workflow_dispatch", "refs/heads/main")
        self.assertEqual((code, values["mode"]), (0, "prebuild"))


class FindTests(unittest.TestCase):
    def test_only_trusted_main_runs_of_this_commit_count(self):
        for bad in ({"head_sha": "c" * 40}, {"head_branch": "v3.12.1"}, {"event": "pull_request"},
                    {"event": "workflow_run"}, {"path": ".github/workflows/ci.yml"},
                    {"repository": {"full_name": "fork/lean-ctx"}},
                    {"head_repository": {"full_name": "fork/lean-ctx"}}):
            with self.subTest(bad=bad):
                self.assertEqual(MODULE.classify_runs([_run(**bad, status="in_progress")], REPO, SHA), ([], False))
        self.assertEqual(MODULE.classify_runs([_run(conclusion="failure")], REPO, SHA), ([], False))
        self.assertEqual(MODULE.classify_runs([_run(status="queued")], REPO, SHA), ([], True))
        self.assertEqual(MODULE.classify_runs(["junk", None], REPO, SHA), ([], False))
        done, _ = MODULE.classify_runs([_run(id=1, run_number=1), _run(id=2, run_number=2)], REPO, SHA)
        self.assertEqual([r["id"] for r in done], [2, 1])

    def test_artifact_set_must_be_complete_and_from_the_run(self):
        MODULE.check_artifacts(_artifacts(), RUN, SHA)
        # Unrelated artifacts (reports, build-config sidecars) do not matter.
        MODULE.check_artifacts(_artifacts(extra=["release-build-config-aarch64-apple-darwin"]), RUN, SHA)
        for artifacts in (_artifacts(drop="lean-ctx-aarch64-apple-darwin"),
                          _artifacts(drop=MODULE.MANIFEST_ARTIFACT),
                          _artifacts(extra=["lean-ctx-extra"]),
                          _artifacts(run_id=RUN + 1), _artifacts(sha="c" * 40),
                          _artifacts(expired=True), _artifacts() + _artifacts()[:1],
                          _artifacts() + ["junk"]):
            with self.assertRaises(MODULE.PrebuildError):
                MODULE.check_artifacts(artifacts, RUN, SHA)

    def test_find_waits_for_a_pending_prebuild_then_skips_runs_without_artifacts(self):
        clock = [0.0]
        states = iter([[_run(status="in_progress")], [_run(id=1, run_number=1), _run(id=RUN, run_number=2)]])

        def api(path, _token):
            if "/workflows/" in path:
                return 200, {"workflow_runs": next(states)}
            return 200, {"artifacts": _artifacts() if f"/runs/{RUN}/" in path else []}

        with contextlib.redirect_stderr(io.StringIO()):
            found = MODULE.find(REPO, SHA, None, 600, 30, api=api,
                                sleep=lambda s: clock.__setitem__(0, clock[0] + s), clock=lambda: clock[0])
        self.assertEqual(found, RUN)
        self.assertEqual(clock[0], 30)

    def test_find_gives_up_after_the_deadline(self):
        clock = [0.0]
        with contextlib.redirect_stderr(io.StringIO()):
            found = MODULE.find(REPO, SHA, None, 60, 30,
                                api=lambda path, _t: (200, {"workflow_runs": [_run(status="in_progress")]}),
                                sleep=lambda s: clock.__setitem__(0, clock[0] + s), clock=lambda: clock[0])
        self.assertIsNone(found)


class SealVerifyTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.split = self.root / "split"
        self.merged = self.root / "merged"
        self.config = self.root / "config"
        for artifact in MODULE.ARTIFACTS:
            for base in (self.split / f"lean-ctx-{artifact}", self.merged):
                (base / "dist-engine-wheels").mkdir(parents=True, exist_ok=True)
                (base / MODULE.archive_name(artifact)).write_bytes(f"archive {artifact}".encode())
                (base / "dist-engine-wheels" / _wheel(artifact)).write_bytes(f"wheel {artifact}".encode())
            self._write_config(artifact, CONFIG)

    def tearDown(self):
        self.tmp.cleanup()

    def _write_config(self, artifact, value):
        directory = self.config / f"release-build-config-{artifact}"
        directory.mkdir(parents=True, exist_ok=True)
        (directory / f"build-config-{artifact}.sha256").write_text(value + "\r\n")

    def _seal(self, **overrides):
        args = dict(repo=REPO, sha=SHA, tree=TREE, version=VERSION, run_id=RUN, run_attempt=1,
                    build_config=CONFIG)
        args.update(overrides)
        return MODULE.seal(self.split, **args)

    def _attestation(self, manifest_sha, **cert_overrides):
        workflow = f"https://github.com/{REPO}/.github/workflows/release.yml@refs/heads/main"
        cert = {"issuer": MODULE.OIDC_ISSUER, "buildSignerURI": workflow, "buildConfigURI": workflow,
                "sourceRepositoryURI": f"https://github.com/{REPO}", "sourceRepositoryDigest": SHA,
                "sourceRepositoryRef": "refs/heads/main", "buildTrigger": "push",
                "runnerEnvironment": "github-hosted",
                "runInvocationURI": f"https://github.com/{REPO}/actions/runs/{RUN}/attempts/1"}
        cert.update(cert_overrides)
        return [{"verificationResult": {"signature": {"certificate": cert},
                                        "statement": {"subject": [{"digest": {"sha256": manifest_sha}}]}}}]

    def test_seal_records_every_file_and_verifies(self):
        manifest = self._seal()
        self.assertEqual(len(manifest["artifacts"]), 10)
        self.assertEqual(sum(len(f) for f in manifest["artifacts"].values()), 20)
        MODULE.verify_manifest(manifest, REPO, SHA, TREE, VERSION, RUN, CONFIG)
        MODULE.verify_files(manifest, self.merged)

    def test_build_configuration_comes_from_all_ten_build_jobs(self):
        self.assertEqual(MODULE.common_build_config(self.config), CONFIG)
        self._write_config("x86_64-pc-windows-gnu", "0" * 64)
        with self.assertRaises(MODULE.PrebuildError):
            MODULE.common_build_config(self.config)
        self._write_config("x86_64-pc-windows-gnu", "not a digest")
        with self.assertRaises(MODULE.PrebuildError):
            MODULE.common_build_config(self.config)
        (self.config / "release-build-config-x86_64-pc-windows-gnu"
         / "build-config-x86_64-pc-windows-gnu.sha256").unlink()
        with self.assertRaises(MODULE.PrebuildError):
            MODULE.common_build_config(self.config)

    def test_seal_refuses_an_incomplete_or_foreign_payload(self):
        extra = self.split / "lean-ctx-x86_64-pc-windows-gnu" / "extra"
        extra.write_bytes(b"x")
        with self.assertRaises(MODULE.PrebuildError):
            self._seal()
        extra.unlink()
        with self.assertRaises(MODULE.PrebuildError):
            self._seal(version="3.12.2")
        archive = self.split / "lean-ctx-x86_64-pc-windows-gnu" / "lean-ctx-x86_64-pc-windows-gnu.zip"
        archive.unlink()
        archive.symlink_to(self.merged / "lean-ctx-x86_64-pc-windows-gnu.zip")
        with self.assertRaises(MODULE.PrebuildError):
            self._seal()

    def test_manifest_must_match_the_tag_run(self):
        manifest = self._seal()
        for key, value in (("sha", "c" * 40), ("tree", "d" * 40), ("version", "3.12.2"),
                           ("version", "3.12.1-rc.1"), ("run_id", RUN + 1), ("build_config", "0" * 64),
                           ("repo", "fork/lean-ctx")):
            args = dict(repo=REPO, sha=SHA, tree=TREE, version=VERSION, run_id=RUN, build_config=CONFIG)
            args[key] = value
            with self.subTest(key=key, value=value), self.assertRaises(MODULE.PrebuildError):
                MODULE.verify_manifest(manifest, **args)
        with self.assertRaises(MODULE.PrebuildError):
            MODULE.verify_manifest(["not", "a", "dict"], REPO, SHA, TREE, VERSION, RUN, CONFIG)

    def test_build_configuration_change_invalidates_the_prebuild(self):
        self.assertNotEqual(CONFIG, MODULE.config_digest({"LEANCTX_PRODUCTION_RUNTIME_CHANNEL_URL": "https://x"}))

    def test_downloaded_files_must_be_byte_identical_regular_files(self):
        manifest = self._seal()
        archive = self.merged / "lean-ctx-aarch64-apple-darwin.tar.gz"
        archive.write_bytes(b"tampered")
        with self.assertRaises(MODULE.PrebuildError):
            MODULE.verify_files(manifest, self.merged)
        archive.write_bytes(b"archive aarch64-apple-darwin")
        MODULE.verify_files(manifest, self.merged)
        (self.merged / "lean-ctx-extra.tar.gz").write_bytes(b"x")
        with self.assertRaises(MODULE.PrebuildError):
            MODULE.verify_files(manifest, self.merged)
        (self.merged / "lean-ctx-extra.tar.gz").unlink()
        # A link to identical bytes is still refused.
        copy = self.root / "copy.tar.gz"
        copy.write_bytes(archive.read_bytes())
        archive.unlink()
        archive.symlink_to(copy)
        with self.assertRaises(MODULE.PrebuildError):
            MODULE.verify_files(manifest, self.merged)
        archive.unlink()
        archive.write_bytes(copy.read_bytes())
        wheels = self.merged / "dist-engine-wheels"
        wheels.rename(self.root / "wheels")
        wheels.symlink_to(self.root / "wheels")
        with self.assertRaises(MODULE.PrebuildError):
            MODULE.verify_files(manifest, self.merged)

    def test_attestation_must_name_release_workflow_on_main_at_this_commit_and_run(self):
        digest = hashlib.sha256(b"manifest").hexdigest()
        MODULE.verify_attestation(self._attestation(digest), digest, REPO, SHA, RUN)
        other = f"https://github.com/{REPO}/.github/workflows/ci.yml@refs/heads/main"
        for override in ({"buildSignerURI": other}, {"buildConfigURI": other},
                         {"buildSignerURI": f"https://github.com/{REPO}/.github/workflows/release.yml@refs/tags/v1.0.0"},
                         {"sourceRepositoryURI": "https://github.com/fork/lean-ctx"},
                         {"sourceRepositoryDigest": "c" * 40}, {"sourceRepositoryRef": "refs/heads/x"},
                         {"buildTrigger": "pull_request"}, {"runnerEnvironment": "self-hosted"},
                         {"runInvocationURI": f"https://github.com/{REPO}/actions/runs/{RUN + 1}/attempts/1"},
                         {"runInvocationURI": None}, {"issuer": "https://example.invalid"}):
            with self.subTest(override=override), self.assertRaises(MODULE.PrebuildError):
                MODULE.verify_attestation(self._attestation(digest, **override), digest, REPO, SHA, RUN)
        for results in ([], {}, "x", None, ["junk"], [{"verificationResult": "x"}],
                        [{"verificationResult": {"signature": "x", "statement": {}}}],
                        [{"verificationResult": {"signature": {"certificate": []}, "statement": {"subject": []}}}],
                        self._attestation("0" * 64)):
            with self.subTest(results=results), self.assertRaises(MODULE.PrebuildError):
                MODULE.verify_attestation(results, digest, REPO, SHA, RUN)
        good = self._attestation(digest)
        good[0]["verificationResult"]["statement"]["subject"] = ["junk", {"digest": "x"},
                                                                  {"digest": {"sha256": digest}}]
        MODULE.verify_attestation(["junk", *good], digest, REPO, SHA, RUN)

    def test_cli_verify_round_trip_and_clean_refusals(self):
        manifest_path = self.root / "m.json"
        manifest_path.write_text(json.dumps(self._seal()))
        attestation = self.root / "a.json"
        attestation.write_text(json.dumps(self._attestation(MODULE.sha256_file(manifest_path))))
        base = ["verify", "--manifest", str(manifest_path), "--attestation", str(attestation), "--repo", REPO,
                "--sha", SHA, "--tree", TREE, "--version", VERSION, "--run-id", str(RUN)]
        with mock.patch.dict(os.environ, {n: "" for n in MODULE.BUILD_CONFIG_VARIABLES}), \
                contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()) as err:
            self.assertEqual(MODULE.main([*base, "--dir", str(self.merged)]), 0)
            self.assertEqual(MODULE.main([*base[:-2], "--run-id", str(RUN + 1)]), 1)
            attestation.write_text(json.dumps([{"verificationResult": {"signature": None}}]))
            self.assertEqual(MODULE.main(base), 1)
            manifest_path.write_text(json.dumps({"schema": MODULE.SCHEMA, "artifacts": "x"}))
            self.assertEqual(MODULE.main(base), 1)
        self.assertIn("release-prebuild: refused:", err.getvalue())
        self.assertNotIn("Traceback", err.getvalue())


class WorkflowContractTests(unittest.TestCase):
    WORKFLOW = (ROOT / ".github/workflows/release.yml").read_text(encoding="utf-8")

    def _job(self, name, following):
        return self.WORKFLOW.split(f"\n  {name}:\n", 1)[1].split(f"\n  {following}:", 1)[0]

    def test_artifact_list_matches_the_release_matrix(self):
        matrix_rows = []
        for job in ("build", "build-windows"):
            block = self.WORKFLOW.split(f"\n  {job}:\n", 1)[1].split("\n    steps:", 1)[0]
            for row in re.split(r"\n          - target: ", block)[1:]:
                target = row.split("\n", 1)[0].strip()
                artifact = re.search(r"\n            artifact: (\S+)", row)
                matrix_rows.append(artifact[1].strip("'") if artifact and artifact[1] != "''" else target)
        self.assertEqual(sorted(matrix_rows), sorted(MODULE.ARTIFACTS))

    def test_build_config_variables_match_the_build_step(self):
        for name in MODULE.BUILD_CONFIG_VARIABLES:
            self.assertIn(f"{name}: ${{{{ vars.{name} }}}}", self.WORKFLOW)
        self.assertIn('python scripts/release-prebuild.py config-digest > "build-config/build-config-${artifact}.sha256"',
                      self.WORKFLOW)
        self.assertIn("name: release-build-config-${{ matrix.artifact || matrix.target }}", self.WORKFLOW)

    def test_seal_attests_only_the_manifest(self):
        seal = self._job("prebuild-seal", "release")
        attest = seal.split("uses: actions/attest-build-provenance@", 1)[1].split("\n      - ", 1)[0]
        self.assertIn("subject-path: ${{ runner.temp }}/prebuild-manifest/release-prebuild-manifest.json", attest)
        self.assertNotIn(".whl", attest)
        self.assertNotIn(".tar.gz", attest)
        self.assertIn('--config-dir "$RUNNER_TEMP/prebuild-config"', seal)
        self.assertNotIn("retention-days: 30", self.WORKFLOW)

    def test_plan_compares_with_the_version_before_the_push(self):
        plan = self._job("plan", "resolve-prebuild")
        self.assertIn("BEFORE: ${{ github.event.before }}", plan)
        self.assertIn('--before "${BEFORE}"', plan)

    def test_reuse_kill_switch_and_prerelease_tags(self):
        resolve = self._job("resolve-prebuild", "sdk-gate")
        self.assertIn("REUSE_SWITCH: ${{ vars.RELEASE_PREBUILD_REUSE }}", resolve)
        self.assertIn('if [[ "$REUSE_SWITCH" == "false" ]]; then', resolve)
        self.assertIn('elif [[ "$REUSABLE" != "true" ]]; then', resolve)
        self.assertIn('--dir "$RUNNER_TEMP/prebuild-files"', resolve)

    def test_publishing_stays_on_tag_pushes(self):
        release = self._job("release", "cockpit-assets")
        self.assertIn("needs.plan.outputs.mode == 'release'", release)
        self.assertIn("github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')", release)
        self.assertIn("python3 scripts/release-prebuild.py verify", release)
        self.assertIn("RELEASE_PREBUILD_REUSE=false", release)


if __name__ == "__main__":
    unittest.main()
