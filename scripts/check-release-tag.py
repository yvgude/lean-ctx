#!/usr/bin/env python3
"""Fail closed on release tags that drift from every shipped engine package."""

import argparse
import json
import re
import sys
from pathlib import Path


COUPLED_PACKAGES = (
    "packages/pi-lean-ctx/package.json",
    "packages/lean-ctx-bin/package.json",
    # The Claude Code mod manifest: the Claude directory needs a new version
    # per release, and the binary materializes the same file.
    "rust/src/templates/claude_mod/plugin.json",
    "integrations/claude-code-mod/.claude-plugin/plugin.json",
)
SEMVER_RE = re.compile(
    r"^(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
    r"(?:-(?:0|[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$"
)


class InvalidReleaseTag(ValueError):
    """Tag is not an exact version of every coupled release artifact."""


def _fail(message):
    raise InvalidReleaseTag(message)


def _read(path):
    try:
        return path.read_text(encoding="utf-8")
    except OSError as error:
        _fail(f"cannot read {path}: {error}")


def _cargo_version(root):
    content = _read(root / "rust/Cargo.toml")
    package = re.search(r"^\[package\]\s*$([\s\S]*?)(?=^\[[^\]]+\]|\Z)", content, re.MULTILINE)
    if package is None:
        _fail("rust/Cargo.toml has no [package] section")
    version = re.search(r'^version\s*=\s*"([^"]+)"\s*$', package.group(1), re.MULTILINE)
    if version is None:
        _fail("rust/Cargo.toml has no package version")
    return version.group(1)


def _package_version(root, relative):
    try:
        value = json.loads(_read(root / relative))
    except (TypeError, ValueError) as error:
        _fail(f"{relative} is not valid JSON: {error}")
    if not isinstance(value, dict) or not isinstance(value.get("version"), str):
        _fail(f"{relative} has no package version")
    return value["version"]


def _strict_version(value, label):
    if not isinstance(value, str) or not SEMVER_RE.fullmatch(value):
        _fail(f"{label} is not strict SemVer")
    return value


def verify_tag(tag: str, root: Path) -> str:
    if not isinstance(tag, str) or not tag.startswith("v"):
        _fail("release tag must start with v")
    version = _strict_version(tag[1:], "release tag")
    root = Path(root)
    engine = _strict_version(_cargo_version(root), "engine version")
    if engine != version:
        _fail(f"release tag {tag} does not equal engine version {engine}")
    for relative in COUPLED_PACKAGES:
        package_version = _strict_version(_package_version(root, relative), relative)
        if package_version != version:
            _fail(f"release tag {tag} does not equal {relative} version {package_version}")
    return version


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    parser.add_argument("--root", default=".")
    arguments = parser.parse_args(argv)
    try:
        verify_tag(arguments.tag, Path(arguments.root))
    except InvalidReleaseTag as error:
        print(f"release tag rejected: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
