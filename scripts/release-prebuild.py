#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Build once on main, reuse on the release tag (.github/workflows/release.yml).

    release-prebuild.py plan          --event E --ref R --repo O/N [--before SHA] [--root .]
    release-prebuild.py find          --repo O/N --sha SHA [--wait-seconds N]
    release-prebuild.py seal          --artifacts-dir D --config-dir C --repo O/N --sha SHA
                                      --tree T --version V --run-id ID --run-attempt N --output M
    release-prebuild.py verify        --manifest M --attestation A --repo O/N --sha SHA
                                      --tree T --version V --run-id ID [--dir D]
    release-prebuild.py config-digest

The push to main that changes the workspace Cargo version to a stable,
unpublished X.Y.Z (the release merge commit), or a manual dispatch on main,
builds the full release matrix (the jobs a tag runs) and seals it: a manifest
of every file digest and of the build configuration each build job compiled
in, attested by release.yml@refs/heads/main. The tag run of the same commit
reuses that build only when the run, its manifest, the manifest's Sigstore
attestation and every file digest agree with the tag; otherwise it builds as
before. Nothing here signs or publishes a release.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import re
import sys
import time
import tomllib
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

SCHEMA = "leanctx.release-prebuild/v1"
WORKFLOW_PATH = ".github/workflows/release.yml"
MAIN_REF = "refs/heads/main"
MANIFEST_ARTIFACT = "release-prebuild-manifest"
MANIFEST_NAME = "release-prebuild-manifest.json"
CONFIG_ARTIFACT_PREFIX = "release-build-config-"
OIDC_ISSUER = "https://token.actions.githubusercontent.com"
# The release matrix of release.yml (`build` and `build-windows`), by artifact
# name. tests/test_release_prebuild.py keeps the two in step.
ARTIFACTS = (
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
    "x86_64-unknown-linux-gnu-cuda",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "x86_64-pc-windows-msvc",
    "x86_64-pc-windows-msvc-cuda",
    "x86_64-pc-windows-gnu",
)
# Variables a build compiles in (build.rs, option_env!). A tag run reuses a
# prebuild only when they are unchanged since it was built.
BUILD_CONFIG_VARIABLES = (
    "LEANCTX_PRODUCTION_RUNTIME_CHANNEL_URL",
    "LEANCTX_PRODUCTION_RUNTIME_CHANNEL_SIGNATURE_URL",
    "LEANCTX_PRODUCTION_RUNTIME_CHANNEL_ROOT_KEY_HEX",
    "LEANCTX_PRODUCTION_COCKPIT_CHANNEL_URL",
    "LEANCTX_PRODUCTION_COCKPIT_CHANNEL_SIGNATURE_URL",
)
STABLE = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)")
# The tag grammar of scripts/check-release-tag.py (stable or prerelease).
TAG_VERSION = re.compile(
    r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
    r"(?:-(?:0|[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
)
SHA = re.compile(r"[0-9a-f]{40}")
DIGEST = re.compile(r"[0-9a-f]{64}")
REPO = re.compile(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+")
WHEEL = re.compile(r"thinkery_leanctx_engine(?:_cuda|_windows_gnu)?-(?P<version>[^-]+)-py3-none-[A-Za-z0-9_.]+\.whl")


class PrebuildError(Exception):
    """A prebuild is unusable; the tag run builds instead."""


def archive_name(artifact: str) -> str:
    return f"lean-ctx-{artifact}.zip" if "-windows-" in artifact else f"lean-ctx-{artifact}.tar.gz"


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _regular(path: Path) -> Path:
    if path.is_symlink() or not path.is_file():
        raise PrebuildError(f"{path.name}: not a regular file")
    return path


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise PrebuildError(message)


def _warn(message: str) -> None:
    print(f"::warning::{message}", file=sys.stderr)


# ---- plan ------------------------------------------------------------------

def _package_version(cargo_toml: str) -> str | None:
    version = tomllib.loads(cargo_toml).get("package", {}).get("version")
    return version if isinstance(version, str) else None


def cargo_version(root: Path) -> str:
    version = _package_version((root / "rust/Cargo.toml").read_text(encoding="utf-8"))
    _require(version is not None and TAG_VERSION.fullmatch(version) is not None, "Cargo version is not a semver")
    return version


def is_stable(version: str | None) -> bool:
    return isinstance(version, str) and STABLE.fullmatch(version) is not None


def decide(event: str, ref: str, version: str, *, before_version: str | None = None,
           released: bool | None = None) -> str:
    """Return `release`, `prebuild` or `skip` for one release.yml run.

    Automatic prebuilds sign and store Windows binaries, so only the push
    that moves the workspace version to a new stable, unpublished X.Y.Z
    (the release merge commit) or an explicit dispatch on main prebuilds.
    """
    if event == "push" and ref.startswith("refs/tags/v"):
        return "release"
    if ref != MAIN_REF or not is_stable(version):
        return "skip"
    if event == "workflow_dispatch":
        return "prebuild"
    if event == "push" and before_version is not None and before_version != version and released is False:
        return "prebuild"
    return "skip"


def _api(path: str, token: str | None) -> tuple[int, object]:
    request = urllib.request.Request(
        f"https://api.github.com{path}",
        headers={"Accept": "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28",
                 "User-Agent": "lean-ctx-release-prebuild",
                 **({"Authorization": f"Bearer {token}"} if token else {})},
    )
    last: Exception | None = None
    for attempt in range(4):
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                return response.status, json.load(response)
        except urllib.error.HTTPError as error:
            if error.code == 404:
                return 404, None
            last = error
            if error.code < 500 and error.code != 429:
                break
        except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as error:
            last = error
        time.sleep(2 ** attempt)
    raise PrebuildError(f"GitHub API {path}: {last}")


def release_published(repo: str, version: str, token: str | None) -> bool | None:
    """True/False, or None when GitHub cannot tell (then no automatic prebuild)."""
    try:
        # Drafts are not served by this endpoint: only a published release counts.
        status, _ = _api(f"/repos/{repo}/releases/tags/v{version}", token)
    except PrebuildError as error:
        _warn(f"{error}; no automatic prebuild")
        return None
    return status == 200


def version_at(repo: str, sha: str, token: str | None) -> str | None:
    """The workspace Cargo version at `sha`, or None (new branch, unknown)."""
    if SHA.fullmatch(sha or "") is None or set(sha) == {"0"}:
        return None
    try:
        status, body = _api(f"/repos/{repo}/contents/rust/Cargo.toml?ref={urllib.parse.quote(sha)}", token)
        if status != 200 or not isinstance(body, dict) or body.get("encoding") != "base64":
            return None
        return _package_version(base64.b64decode(body["content"]).decode("utf-8"))
    except (PrebuildError, KeyError, ValueError, TypeError, AttributeError) as error:
        _warn(f"Cargo version before the push is unknown ({error}); no automatic prebuild")
        return None


def plan(event: str, ref: str, repo: str, before: str | None, root: Path, token: str | None) -> dict[str, str]:
    if event == "push" and ref.startswith("refs/tags/v"):
        version = ref.removeprefix("refs/tags/v")
        _require(re.fullmatch(r"[0-9A-Za-z.+-]{1,64}", version) is not None, "release tag has unsafe characters")
        # A prerelease (or malformed) tag still runs as a release; the delivery
        # gate judges it, and it never reuses a prebuild.
        return {"mode": "release", "version": version, "reusable": str(is_stable(version)).lower()}
    version = cargo_version(root)
    before_version = released = None
    if event == "push" and ref == MAIN_REF and is_stable(version):
        before_version = version_at(repo, before or "", token)
        if before_version is not None and before_version != version:
            released = release_published(repo, version, token)
    mode = decide(event, ref, version, before_version=before_version, released=released)
    return {"mode": mode, "version": version, "reusable": "false"}


# ---- find ------------------------------------------------------------------

def classify_runs(runs: list, repo: str, sha: str) -> tuple[list[dict], bool]:
    """Completed-successful trusted main runs of release.yml for `sha` (newest
    first), and whether a trusted one is still pending."""
    done, pending = [], False
    for run in runs:
        if not isinstance(run, dict):
            continue
        trusted = (
            run.get("head_sha") == sha
            and run.get("head_branch") == "main"
            and run.get("event") in ("push", "workflow_dispatch")
            and run.get("path") == WORKFLOW_PATH
            and (run.get("repository") or {}).get("full_name") == repo
            and (run.get("head_repository") or {}).get("full_name") == repo
        )
        if not trusted:
            continue
        if run.get("status") != "completed":
            pending = True
        elif run.get("conclusion") == "success":
            done.append(run)
    done.sort(key=lambda run: (run.get("run_number", 0), run.get("run_attempt", 0)), reverse=True)
    return done, pending


def check_artifacts(artifacts: list, run_id: int, sha: str) -> None:
    by_name: dict[str, dict] = {}
    for artifact in artifacts:
        _require(isinstance(artifact, dict), "malformed artifact listing")
        name = artifact.get("name")
        _require(name not in by_name, f"duplicate artifact {name}")
        by_name[name] = artifact
    expected = {MANIFEST_ARTIFACT, *(f"lean-ctx-{a}" for a in ARTIFACTS)}
    # The release job downloads `lean-ctx-*`: no other artifact may match it.
    extra = sorted(n for n in by_name if isinstance(n, str) and n.startswith("lean-ctx-") and n not in expected)
    _require(not extra, f"run {run_id} has unexpected artifacts {extra}")
    for name in sorted(expected):
        artifact = by_name.get(name)
        _require(artifact is not None, f"run {run_id} lacks artifact {name}")
        _require(not artifact.get("expired"), f"artifact {name} expired")
        origin = artifact.get("workflow_run") or {}
        _require(origin.get("id") == run_id and origin.get("head_sha") == sha,
                 f"artifact {name} does not belong to run {run_id} at {sha}")


def find(repo: str, sha: str, token: str | None, wait_seconds: int, poll_seconds: int,
         api=None, sleep=time.sleep, clock=time.monotonic) -> int | None:
    api = api or _api
    deadline = clock() + wait_seconds
    workflow = WORKFLOW_PATH.rsplit("/", 1)[1]
    while True:
        _, body = api(f"/repos/{repo}/actions/workflows/{workflow}/runs?head_sha={sha}&branch=main&per_page=100", token)
        done, pending = classify_runs((body or {}).get("workflow_runs", []), repo, sha)
        for run in done:
            _, listing = api(f"/repos/{repo}/actions/runs/{run['id']}/artifacts?per_page=100", token)
            try:
                check_artifacts((listing or {}).get("artifacts", []), run["id"], sha)
            except PrebuildError as error:
                # A main run that planned `skip` succeeds without artifacts.
                print(f"run {run['id']}: {error}", file=sys.stderr)
                continue
            return int(run["id"])
        if not pending or clock() >= deadline:
            return None
        print(f"prebuild for {sha} still running; waiting", file=sys.stderr)
        sleep(poll_seconds)


# ---- seal / verify ----------------------------------------------------------

def config_digest(environ=os.environ) -> str:
    payload = "".join(f"{name}={environ.get(name, '')}\n" for name in BUILD_CONFIG_VARIABLES)
    return sha256_bytes(payload.encode("utf-8"))


def common_build_config(config_dir: Path) -> str:
    """Each build job records the configuration it compiled in; all ten must
    agree, and that value (not the sealing job's) goes into the manifest."""
    digests = set()
    for artifact in ARTIFACTS:
        path = config_dir / f"{CONFIG_ARTIFACT_PREFIX}{artifact}" / f"build-config-{artifact}.sha256"
        value = _regular(path).read_text(encoding="utf-8").strip()
        _require(DIGEST.fullmatch(value) is not None, f"{artifact}: malformed build-config digest")
        digests.add(value)
    _require(len(digests) == 1, "build jobs compiled different build configurations")
    return digests.pop()


def _artifact_files(directory: Path, artifact: str, version: str) -> dict[str, dict]:
    """The exact payload of one build artifact: its archive and its wheel."""
    files = sorted(p for p in directory.rglob("*") if p.is_symlink() or not p.is_dir())
    names = {p.relative_to(directory).as_posix(): p for p in files}
    wheels = [n for n in names if n.startswith("dist-engine-wheels/")]
    _require(set(names) == {archive_name(artifact), *wheels} and len(wheels) == 1,
             f"lean-ctx-{artifact}: unexpected payload {sorted(names)}")
    match = WHEEL.fullmatch(wheels[0].split("/", 1)[1])
    _require(match is not None and match["version"] == version, f"lean-ctx-{artifact}: wheel version mismatch")
    return {name: {"sha256": sha256_file(_regular(path)), "size": path.stat().st_size}
            for name, path in sorted(names.items())}


def seal(artifacts_dir: Path, repo: str, sha: str, tree: str, version: str,
         run_id: int, run_attempt: int, build_config: str) -> dict:
    present = sorted(p.name for p in artifacts_dir.iterdir() if p.is_dir())
    expected = sorted(f"lean-ctx-{a}" for a in ARTIFACTS)
    _require(present == expected, f"artifact set {present} != {expected}")
    manifest = {
        "schema": SCHEMA, "repository": repo, "workflow": WORKFLOW_PATH, "ref": MAIN_REF,
        "commit": sha, "tree": tree, "version": version, "run_id": run_id,
        "run_attempt": run_attempt, "build_config_sha256": build_config,
        "artifacts": {f"lean-ctx-{a}": _artifact_files(artifacts_dir / f"lean-ctx-{a}", a, version)
                      for a in ARTIFACTS},
    }
    verify_manifest(manifest, repo, sha, tree, version, run_id, build_config)
    return manifest


def verify_manifest(manifest: object, repo: str, sha: str, tree: str, version: str,
                    run_id: int, build_config: str) -> None:
    _require(isinstance(manifest, dict) and manifest.get("schema") == SCHEMA, "unknown manifest schema")
    _require(SHA.fullmatch(sha) is not None and SHA.fullmatch(tree) is not None, "invalid commit or tree")
    _require(is_stable(version), "only a stable X.Y.Z release reuses a prebuild")
    for key, value in (("repository", repo), ("workflow", WORKFLOW_PATH), ("ref", MAIN_REF),
                       ("commit", sha), ("tree", tree), ("version", version), ("run_id", run_id),
                       ("build_config_sha256", build_config)):
        _require(manifest.get(key) == value, f"manifest {key} {manifest.get(key)!r} != {value!r}")
    artifacts = manifest.get("artifacts")
    _require(isinstance(artifacts, dict) and sorted(artifacts) == sorted(f"lean-ctx-{a}" for a in ARTIFACTS),
             "manifest artifact set drifted")
    seen: set[str] = set()
    for artifact in ARTIFACTS:
        files = artifacts[f"lean-ctx-{artifact}"]
        _require(isinstance(files, dict) and archive_name(artifact) in files and len(files) == 2,
                 f"lean-ctx-{artifact}: manifest payload drifted")
        for name, record in files.items():
            _require(name not in seen, f"{name}: listed twice")
            seen.add(name)
            wheel = WHEEL.fullmatch(name.split("/", 1)[1]) if name.startswith("dist-engine-wheels/") else None
            _require(name == archive_name(artifact) or (wheel is not None and wheel["version"] == version),
                     f"{name}: unexpected file")
            _require(isinstance(record, dict) and isinstance(record.get("sha256"), str)
                     and DIGEST.fullmatch(record["sha256"]) is not None
                     and isinstance(record.get("size"), int) and record["size"] > 0, f"{name}: bad record")


def verify_attestation(results: object, manifest_sha256: str, repo: str, sha: str, run_id: int) -> None:
    """`gh attestation verify --format json` output for the manifest: at least
    one verified result must name this repository's release.yml on main, this
    commit, this run and a GitHub-hosted runner."""
    _require(isinstance(results, list) and results, "no verified attestation")
    workflow_uri = f"https://github.com/{repo}/{WORKFLOW_PATH}@{MAIN_REF}"
    run_prefix = f"https://github.com/{repo}/actions/runs/{run_id}/attempts/"
    for result in results:
        verified = result.get("verificationResult") if isinstance(result, dict) else None
        if not isinstance(verified, dict):
            continue
        signature, statement = verified.get("signature"), verified.get("statement")
        cert = signature.get("certificate") if isinstance(signature, dict) else None
        subjects = statement.get("subject") if isinstance(statement, dict) else None
        if not isinstance(cert, dict) or not isinstance(subjects, list):
            continue
        if (
            cert.get("issuer") == OIDC_ISSUER
            and cert.get("buildSignerURI") == workflow_uri
            and cert.get("buildConfigURI") == workflow_uri
            and cert.get("sourceRepositoryURI") == f"https://github.com/{repo}"
            and cert.get("sourceRepositoryDigest") == sha
            and cert.get("sourceRepositoryRef") == MAIN_REF
            and cert.get("buildTrigger") in ("push", "workflow_dispatch")
            and cert.get("runnerEnvironment") == "github-hosted"
            and isinstance(cert.get("runInvocationURI"), str)
            and cert["runInvocationURI"].startswith(run_prefix)
            and any(isinstance(s, dict) and isinstance(s.get("digest"), dict)
                    and s["digest"].get("sha256") == manifest_sha256 for s in subjects)
        ):
            return
    raise PrebuildError("no attestation binds the manifest to release.yml on main at this commit and run")


def verify_files(manifest: dict, directory: Path) -> None:
    """The merged download (archives at the root, wheels in dist-engine-wheels/)
    holds exactly the sealed files, byte for byte, none of them a link."""
    expected = {name: record for files in manifest["artifacts"].values() for name, record in files.items()}
    present = {p.name for p in directory.glob("lean-ctx-*") if p.name.endswith((".tar.gz", ".zip"))}
    wheels = directory / "dist-engine-wheels"
    _require(not wheels.is_symlink(), "dist-engine-wheels must not be a link")
    if wheels.exists():
        present |= {f"dist-engine-wheels/{p.relative_to(wheels).as_posix()}" for p in wheels.rglob("*")}
    _require(present == set(expected), f"downloaded files differ from the manifest: {sorted(present ^ set(expected))}")
    for name, record in sorted(expected.items()):
        path = _regular(directory / name)
        _require(path.stat().st_size == record["size"] and sha256_file(path) == record["sha256"],
                 f"{name}: digest differs from the sealed manifest")


# ---- CLI --------------------------------------------------------------------

def _output(values: dict[str, str]) -> None:
    target = os.environ.get("GITHUB_OUTPUT")
    lines = "".join(f"{key}={value}\n" for key, value in values.items())
    if target:
        with open(target, "a", encoding="utf-8") as handle:
            handle.write(lines)
    sys.stdout.write(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    sub = parser.add_subparsers(dest="command", required=True)
    planner = sub.add_parser("plan")
    planner.add_argument("--event", required=True)
    planner.add_argument("--ref", required=True)
    planner.add_argument("--repo", required=True)
    planner.add_argument("--before", default="")
    planner.add_argument("--root", type=Path, default=Path("."))
    found = sub.add_parser("find")
    found.add_argument("--repo", required=True)
    found.add_argument("--sha", required=True)
    found.add_argument("--wait-seconds", type=int, default=0)
    found.add_argument("--poll-seconds", type=int, default=60)
    for name in ("seal", "verify"):
        command = sub.add_parser(name)
        command.add_argument("--repo", required=True)
        command.add_argument("--sha", required=True)
        command.add_argument("--tree", required=True)
        command.add_argument("--version", required=True)
        command.add_argument("--run-id", type=int, required=True)
    sealer = sub.choices["seal"]
    sealer.add_argument("--artifacts-dir", type=Path, required=True)
    sealer.add_argument("--config-dir", type=Path, required=True)
    sealer.add_argument("--run-attempt", type=int, required=True)
    sealer.add_argument("--output", type=Path, required=True)
    verifier = sub.choices["verify"]
    verifier.add_argument("--manifest", type=Path, required=True)
    verifier.add_argument("--attestation", type=Path, required=True)
    verifier.add_argument("--dir", type=Path)
    sub.add_parser("config-digest")
    args = parser.parse_args(argv)
    token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")

    try:
        if getattr(args, "repo", None) is not None:
            _require(REPO.fullmatch(args.repo) is not None, "invalid repository")
        if args.command == "plan":
            _output(plan(args.event, args.ref, args.repo, args.before, args.root, token))
        elif args.command == "find":
            _require(SHA.fullmatch(args.sha) is not None, "invalid commit")
            run_id = find(args.repo, args.sha, token, args.wait_seconds, args.poll_seconds)
            print(run_id or "")
        elif args.command == "config-digest":
            print(config_digest())
        elif args.command == "seal":
            manifest = seal(args.artifacts_dir, args.repo, args.sha, args.tree, args.version,
                            args.run_id, args.run_attempt, common_build_config(args.config_dir))
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        else:
            # Hash exactly the bytes that are parsed.
            raw = _regular(args.manifest).read_bytes()
            manifest = json.loads(raw)
            verify_manifest(manifest, args.repo, args.sha, args.tree, args.version, args.run_id, config_digest())
            verify_attestation(json.loads(_regular(args.attestation).read_bytes()),
                               sha256_bytes(raw), args.repo, args.sha, args.run_id)
            if args.dir is not None:
                verify_files(manifest, args.dir)
            print(f"verified prebuild run {args.run_id} for {args.sha}")
    except (PrebuildError, OSError, ValueError, KeyError, AttributeError, TypeError) as error:
        print(f"release-prebuild: refused: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
