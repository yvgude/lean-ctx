#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Engine <-> Agent-Tools-SDK version-coupling gate.

`release.yml` checks out `Thinkery-AG/leanctx-sdk` at a pinned commit and runs
that SDK's `verify_agent_context_e2e.py` against every freshly built engine
binary. Line one of that script is:

    if expected_engine_version != SUPPORTED_AGENT_TOOLS_ENGINE_VERSION:
        raise RuntimeError("SDK Engine version constant does not match the release gate")

`expected_engine_version` is the release tag with its leading `v` stripped. So
if the pinned SDK commit's constant still names the *previous* engine version,
**every** build-matrix leg fails — nine jobs, after each has already compiled a
full release binary. That is what happened to v3.10.2 on 2026-09-16: ~15 minutes
of CI, nine red legs, one-line cause, and nothing in the local gate could have
caught it because the coupling lives in another repository.

This gate closes that hole. It reads the pin out of `release.yml`, fetches that
exact commit's constant from GitHub, and compares it with the engine version in
`rust/Cargo.toml`. Runs in about a second, needs no checkout of the SDK, and
fails *before* a tag is ever pushed.

Deliberately network-dependent: the pinned commit is not in this repository, so
there is nothing local to compare against. When GitHub is unreachable the gate
reports a skip rather than a pass — an unverifiable coupling must never read as
a verified one.

No third-party dependencies — standard library only.
"""

from __future__ import annotations

import json
import os
import pathlib
import re
import sys
import urllib.error
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
RELEASE_WORKFLOW = ROOT / ".github/workflows/release.yml"
CARGO_MANIFEST = ROOT / "rust/Cargo.toml"

# Where the constant lives inside the SDK repository.
SDK_CONSTANT_PATH = "src/leanctx_sdk/agent.py"
SDK_CONSTANT_NAME = "SUPPORTED_AGENT_TOOLS_ENGINE_VERSION"

# `repository:` / `ref:` pair of the SDK checkout step in release.yml.
SDK_CHECKOUT = re.compile(
    r"repository:\s*(?P<repo>[\w.-]+/[\w.-]+)\s*\n\s*ref:\s*(?P<ref>[0-9a-f]{40})",
    re.MULTILINE,
)
ENGINE_VERSION = re.compile(r'^version\s*=\s*"([^"]+)"', re.MULTILINE)
CONSTANT = re.compile(
    rf'^{SDK_CONSTANT_NAME}\s*=\s*"([^"]+)"',
    re.MULTILINE,
)


def fail(message: str) -> None:
    print(f"FAIL: {message}")
    raise SystemExit(1)


def skip(message: str) -> None:
    print(f"SKIP: {message}")
    raise SystemExit(0)


def engine_version() -> str:
    """The `[package] version` the release will ship."""
    text = CARGO_MANIFEST.read_text(encoding="utf-8")
    package = re.search(r"^\[package\]\s*$(.*?)^\[", text, re.MULTILINE | re.DOTALL)
    if package is None:
        fail("rust/Cargo.toml has no [package] section")
    match = ENGINE_VERSION.search(package.group(1))
    if match is None:
        fail("rust/Cargo.toml has no package version")
    return match.group(1)


def pinned_sdk() -> tuple[str, str]:
    """The `(repository, commit)` release.yml pins the Agent Tools SDK to."""
    text = RELEASE_WORKFLOW.read_text(encoding="utf-8")
    match = SDK_CHECKOUT.search(text)
    if match is None:
        fail(
            "release.yml has no `repository: … / ref: <40-hex>` SDK checkout — "
            "if that step moved or now uses a tag, update this gate with it"
        )
    return match.group("repo"), match.group("ref")


def fetch_constant(repository: str, ref: str) -> str:
    """Read the SDK's engine-version constant at one exact commit."""
    url = (
        f"https://api.github.com/repos/{repository}/contents/"
        f"{SDK_CONSTANT_PATH}?ref={ref}"
    )
    request = urllib.request.Request(
        url,
        headers={
            "Accept": "application/vnd.github.raw+json",
            "User-Agent": "lean-ctx-sdk-coupling-gate",
        },
    )
    # Unauthenticated GitHub API calls are capped at 60/hour per IP, which CI
    # runners share. Use the workflow token when one is available.
    token = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN")
    if token:
        request.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            source = response.read().decode("utf-8")
    except urllib.error.HTTPError as error:
        if error.code == 404:
            fail(
                f"{repository}@{ref[:12]} has no {SDK_CONSTANT_PATH} — "
                "the pin may point at a commit from before the SDK was restructured"
            )
        skip(f"GitHub returned HTTP {error.code}; coupling not verified")
    except (urllib.error.URLError, TimeoutError, OSError) as error:
        skip(f"GitHub unreachable ({error}); coupling not verified")

    match = CONSTANT.search(source)
    if match is None:
        fail(
            f"{SDK_CONSTANT_NAME} not found in {repository}@{ref[:12]}:"
            f"{SDK_CONSTANT_PATH} — this gate can no longer verify the coupling"
        )
    return match.group(1)


def main(argv: list[str] | None = None) -> int:
    argv = sys.argv[1:] if argv is None else argv
    as_json = "--json" in argv

    engine = engine_version()
    repository, ref = pinned_sdk()
    supported = fetch_constant(repository, ref)

    report = {
        "schema_version": "leanctx.sdk-engine-coupling/v1",
        "engine_version": engine,
        "sdk_repository": repository,
        "sdk_commit": ref,
        "sdk_supported_engine_version": supported,
        "coupled": supported == engine,
    }

    if as_json:
        print(json.dumps(report, sort_keys=True))
    else:
        print(f"engine version (rust/Cargo.toml):        {engine}")
        print(f"pinned SDK ({repository}@{ref[:12]}):")
        print(f"  {SDK_CONSTANT_NAME}: {supported}")

    if supported != engine:
        fail(
            f"the pinned Agent Tools SDK declares support for engine {supported}, "
            f"but this release ships {engine}.\n"
            f"  Every build-matrix leg of release.yml will fail on\n"
            f"  'SDK Engine version constant does not match the release gate'.\n"
            f"  Fix: bump {SDK_CONSTANT_NAME} in {repository} to {engine}, then\n"
            f"  move the `ref:` in .github/workflows/release.yml to that commit."
        )

    print(f"OK: pinned SDK and engine agree on {engine}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
