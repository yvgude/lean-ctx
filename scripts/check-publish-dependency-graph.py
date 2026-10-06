#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Reject local dependencies that make a publishable Cargo package unpublishable."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path
from typing import Any


def cargo_metadata(root: Path) -> dict[str, Any]:
    result = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        cwd=root,
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip() or "cargo metadata failed")
    return json.loads(result.stdout)


def is_publishable(package: dict[str, Any]) -> bool:
    if "publish" not in package:
        raise ValueError("cargo metadata package has no publish field")
    publish = package["publish"]
    if publish is None:
        return True
    if isinstance(publish, list) and all(isinstance(item, str) for item in publish):
        return bool(publish)
    raise ValueError("cargo metadata publish field must be null or a string list")


def validate(root: Path) -> list[str]:
    metadata = cargo_metadata(root.resolve())
    packages = metadata.get("packages")
    member_values = metadata.get("workspace_members")
    if not isinstance(packages, list) or not isinstance(member_values, list):
        raise ValueError("cargo metadata packages/workspace_members must be lists")
    if not packages or not member_values:
        raise ValueError("cargo metadata returned no workspace packages")
    if any(not isinstance(item, dict) for item in packages):
        raise ValueError("cargo metadata packages must contain objects")
    if any(not isinstance(item, str) or not item for item in member_values):
        raise ValueError("cargo metadata workspace members must be non-empty strings")
    for package in packages:
        if not isinstance(package.get("id"), str) or not package["id"]:
            raise ValueError("cargo metadata package has no id")
        if not isinstance(package.get("name"), str) or not package["name"]:
            raise ValueError("cargo metadata package has no name")
    members = set(member_values)
    package_ids = {item.get("id") for item in packages}
    missing_members = sorted(str(item) for item in members - package_ids)
    if missing_members:
        raise ValueError(
            "cargo metadata omitted workspace members: " + ", ".join(missing_members)
        )

    by_manifest: dict[Path, dict[str, Any]] = {}
    for package in packages:
        manifest_value = package.get("manifest_path")
        if not isinstance(manifest_value, str) or not manifest_value:
            raise ValueError("cargo metadata package has no manifest path")
        manifest_path = Path(manifest_value).resolve()
        if manifest_path in by_manifest:
            raise ValueError(f"cargo metadata has duplicate manifest path: {manifest_path}")
        by_manifest[manifest_path] = package
    errors: list[str] = []
    for package in sorted(
        (item for item in packages if item.get("id") in members),
        key=lambda item: item["name"],
    ):
        dependencies = package.get("dependencies")
        if not isinstance(dependencies, list):
            raise ValueError(
                f"cargo metadata package {package.get('name', '<unknown>')} "
                "has no dependency list"
            )
        if any(not isinstance(item, dict) for item in dependencies):
            raise ValueError("cargo metadata dependencies must contain objects")
        for dependency in dependencies:
            if not isinstance(dependency.get("name"), str) or not dependency["name"]:
                raise ValueError("cargo metadata dependency has no name")
            if "path" in dependency and dependency["path"] is not None and not isinstance(
                dependency["path"], str
            ):
                raise ValueError("cargo metadata dependency has invalid path")
            if not isinstance(dependency.get("req"), str) or not dependency["req"]:
                raise ValueError(
                    f"cargo metadata dependency {dependency['name']} has no requirement"
                )
        if not is_publishable(package):
            continue
        for dependency in sorted(
            dependencies,
            key=lambda item: (item.get("name", ""), item.get("kind") or ""),
        ):
            path_text = dependency.get("path")
            if path_text is None or dependency.get("kind") == "dev":
                continue
            location = f"{package['name']} -> {dependency['name']}"
            requirement = dependency["req"]
            if requirement == "*":
                errors.append(f"{location}: path dependency lacks a crates.io version")
                continue
            target_manifest = (Path(path_text) / "Cargo.toml").resolve()
            target = by_manifest.get(target_manifest)
            if target is None:
                errors.append(f"{location}: local dependency is absent from cargo metadata")
            elif not is_publishable(target):
                errors.append(
                    f"{location}: publishable package {package['name']} depends on "
                    f"non-publishable package {target['name']}"
                )
    return sorted(errors)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--workspace-root",
        type=Path,
        default=Path(__file__).resolve().parents[1] / "rust",
    )
    args = parser.parse_args()
    try:
        errors = validate(args.workspace_root)
    except (OSError, RuntimeError, ValueError, json.JSONDecodeError) as exc:
        print(f"publish dependency graph check failed: {exc}", file=sys.stderr)
        return 2
    if errors:
        print("publish dependency graph is invalid:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1
    print("publish dependency graph is valid")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
