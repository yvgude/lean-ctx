#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Validate the product's Cargo source closure without network access or builds."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]
# Layout keys naming a component every product part must take from one place.
SHARED_KEYS = ("protocol_manifest", "ocla_manifest", "native_storage_manifest",
               "commercial_core_manifest", "team_company_contracts_manifest")
# Build output and installed packages are not source of this checkout.
SKIPPED_DIRECTORIES = {"target", "node_modules"}


def dependency_tables(manifest):
    for key in ("dependencies", "dev-dependencies", "build-dependencies"):
        yield manifest.get(key, {})
    yield manifest.get("workspace", {}).get("dependencies", {})
    yield manifest.get("replace", {})
    yield from manifest.get("patch", {}).values()
    for target in manifest.get("target", {}).values():
        for key in ("dependencies", "dev-dependencies", "build-dependencies"):
            yield target.get(key, {})


def load_manifest(path: Path) -> dict:
    raw = path.read_bytes()
    if len(raw) > 1024 * 1024:
        raise ValueError("Cargo manifest is too large")
    return tomllib.loads(raw.decode("utf-8"))


def crate_names(manifest: dict):
    """The package name and the crate identifier dependents import."""
    name = manifest.get("package", {}).get("name")
    if not isinstance(name, str):
        return None, None
    return name, manifest.get("lib", {}).get("name") or name.replace("-", "_")


def shared_components(root: Path, layout: dict):
    """Canonical manifest of each shared component, by package name and by crate identifier.

    The names are read from the canonical manifests, so the rule follows the
    layout instead of a second list. A component absent from this tree (the
    commercial core in a Community checkout) claims no name here; the closure
    check below still requires every component its profile needs.
    """
    by_name, by_identifier = {}, {}
    for key in SHARED_KEYS:
        path = layout.get(key)
        if not path or not (root / path).is_file():
            continue
        name, identifier = crate_names(load_manifest(root / path))
        if name is None:
            raise ValueError(f"{path}: shared component has no package name")
        by_name[name] = by_identifier[identifier] = (root / path).resolve(strict=True)
    return by_name, by_identifier


def duplicate_of(resolved: Path, manifest: dict, shared):
    """Name under which a manifest outside the canonical path claims a shared component.

    Version and location do not matter: an older, vendored or renamed-package
    copy that still builds the canonical crate identifier is a second core.
    """
    for table, key in zip(shared, crate_names(manifest)):
        if key in table and table[key] != resolved:
            return key
    return None


def stray_copies(root: Path, shared):
    """Shared-component copies anywhere in the tree, referenced by a manifest or not."""
    found = []
    for directory, names, files in os.walk(root):
        names[:] = sorted(name for name in names
                          if name not in SKIPPED_DIRECTORIES and not name.startswith("."))
        if "Cargo.toml" not in files:
            continue
        path = Path(directory, "Cargo.toml").resolve()
        try:
            manifest = load_manifest(path)
        except (tomllib.TOMLDecodeError, UnicodeDecodeError):
            continue  # not a manifest Cargo could build
        if duplicate_of(path, manifest, shared):
            found.append(path.relative_to(root).as_posix())
    return found


# What the public tree may learn from the layout: the Community closure reads the
# public manifests only. Commercial, Team/Company and private operations paths
# stay in the private layout (`--project`).
PUBLIC_LAYOUT_KEYS = ("schema", "core_manifest", "protocol_manifest", "ocla_manifest",
                      "native_storage_manifest", "publication", "license_changes")


def project(layout: dict) -> dict:
    """The public projection of the layout; deterministic for the same input."""
    if layout.get("projection") is not None:
        raise ValueError("the layout is already a projection")
    public = {key: layout[key] for key in PUBLIC_LAYOUT_KEYS if key in layout}
    public["cargo_entrypoints"] = [path for path in layout["cargo_entrypoints"]
                                   if path.split("/", 1)[0] not in ("ee", "sdk")]
    public["projection"] = "public"
    return public


SKILL = "skills/leanctx-gateway-delivery/SKILL.md"
SKILL_ALIASES = (".agents/skills/leanctx-gateway-delivery/SKILL.md",
                 ".claude/skills/leanctx-gateway-delivery/SKILL.md")


def verify(root: Path, layout: dict, profile: str = "product", tree: bool = False) -> dict:
    root = root.resolve(strict=True)
    if layout.get("schema") != "leanctx.source-layout/v1":
        raise ValueError("unsupported source layout")
    if profile not in ("product", "community"):
        raise ValueError("unsupported source profile")
    if layout.get("projection") == "public" and profile != "community":
        raise ValueError("the public layout projection checks the community profile only")
    pending = ([root / layout["core_manifest"]] if profile == "community"
               else [root / path for path in layout["cargo_entrypoints"]])
    visited = set()
    shared = shared_components(root, layout)
    while pending:
        path = pending.pop()
        resolved = path.resolve(strict=True)
        if not resolved.is_relative_to(root):
            raise ValueError("local Cargo dependency escapes the source checkout")
        if resolved in visited:
            continue
        visited.add(resolved)
        if len(visited) > 500:
            raise ValueError("source closure exceeds 500 manifests")
        relative = resolved.relative_to(root).as_posix()
        if profile == "community" and relative.split("/", 1)[0] in ("ee", "sdk"):
            raise ValueError(f"{relative}: Community depends on separately licensed product source")
        manifest = load_manifest(resolved)
        duplicate = duplicate_of(resolved, manifest, shared)
        if duplicate:
            raise ValueError(f"{relative}: duplicate shared implementation for {duplicate}")
        for dependencies in dependency_tables(manifest):
            for name, dependency in dependencies.items():
                if not isinstance(dependency, dict):
                    continue
                if "git" in dependency:
                    raise ValueError(f"{relative}: Git dependency {name} is not portable")
                if "path" in dependency:
                    pending.append(resolved.parent / dependency["path"] / "Cargo.toml")
        for member in manifest.get("workspace", {}).get("members", []):
            if Path(member).is_absolute() or ".." in Path(member).parts:
                raise ValueError(f"{relative}: workspace member escapes its workspace")
            matches = ([resolved.parent] if Path(member) == Path(".")
                       else list(resolved.parent.glob(member)))
            if not matches:
                raise ValueError(f"{relative}: workspace member does not exist: {member}")
            pending.extend(member / "Cargo.toml" for member in matches)
        lock = resolved.with_name("Cargo.lock")
        if lock.exists():
            for package in tomllib.loads(lock.read_text(encoding="utf-8")).get("package", []):
                if str(package.get("source", "")).startswith("git+"):
                    raise ValueError(f"{relative}: lockfile retains a Git source")
    required_manifests = [layout["core_manifest"], layout["protocol_manifest"],
                          layout["native_storage_manifest"]]
    if layout.get("ocla_manifest"):
        required_manifests.append(layout["ocla_manifest"])
    if profile == "product":
        required_manifests.append(layout["commercial_core_manifest"])
    for required in required_manifests:
        if (root / required).resolve(strict=True) not in visited:
            raise ValueError(f"shared component is outside the source closure: {required}")
    if tree:
        strays = stray_copies(root, shared)
        if strays:
            raise ValueError(f"{strays[0]}: duplicate shared implementation in the tree "
                             f"({len(strays)} in total)")
    # The delivery skill has one canonical file. Its two agent aliases are tracked
    # REGULAR copies (the provenance audit admits no tracked symbolic link), so a
    # stale copy is caught here instead of being read as a second truth.
    canonical = root / SKILL
    if canonical.is_file():
        for alias in SKILL_ALIASES:
            copy = root / alias
            if copy.is_symlink() or (copy.exists() and copy.read_bytes() != canonical.read_bytes()):
                raise ValueError(f"{alias}: not a byte-identical regular copy of {SKILL}")
    return {"schema": "leanctx.source-layout-check/v1", "passed": True, "profile": profile,
            "manifests": sorted(p.relative_to(root).as_posix() for p in visited),
            "shared_components": sorted(shared[0]), "tree_scanned": tree,
            "git_sources": 0, "external_local_paths": 0,
            "scope": "Cargo manifests and locks, not compilation or release approval"}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--profile", choices=("product", "community"), default="product")
    parser.add_argument("--tree", action="store_true",
                        help="also refuse a shared-component copy no manifest references")
    parser.add_argument("--project", type=Path, metavar="OUT",
                        help="write the public projection of the layout (for the export) and exit")
    args = parser.parse_args()
    try:
        layout = json.loads((args.root / "product/source-layout.json").read_text())
        if args.project:
            args.project.write_text(json.dumps(project(layout), indent=2, sort_keys=True) + "\n",
                                    encoding="utf-8")
            return 0
        result = verify(args.root, layout, args.profile, args.tree)
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"source-layout: {error}", file=sys.stderr)
        return 1
    if args.json:
        print(json.dumps(result, sort_keys=True))
    else:
        print(f"source-layout: {len(result['manifests'])} manifests; no Git sources or escaping paths; "
              f"one implementation each of {', '.join(result['shared_components'])}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
