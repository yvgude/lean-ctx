#!/usr/bin/env python3
"""Addon registry version-staleness check (GL #addon-hardening).

The curated addon registry (`rust/data/addon_registry.json`) pins every
upstream package to an exact version (uv/pip/npm/dotnet/cargo installs and
`npx`/`uvx` runners alike). Exact pins are a supply-chain feature — but they go
stale silently, so a user who runs `addon add <x>` gets an outdated tool.

This script resolves every pinned upstream against its registry (PyPI, npm,
NuGet, crates.io) and reports the ones that have fallen behind. It is
informational by default (exit 0, GitHub `::warning::` annotations) so an
upstream release never blocks our own release; pass `--strict` to exit non-zero
when anything is behind (useful for a scheduled maintenance run).

No third-party dependencies — standard library only.
"""

from __future__ import annotations

import json
import pathlib
import re
import sys
import urllib.error
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
REGISTRY = ROOT / "rust" / "data" / "addon_registry.json"
UA = "lean-ctx-addon-version-check"
TIMEOUT = 15

OUTDATED: list[str] = []
UNRESOLVED: list[str] = []


def http_json(url: str) -> dict:
    req = urllib.request.Request(url, headers={"User-Agent": UA, "Accept": "application/json"})
    with urllib.request.urlopen(req, timeout=TIMEOUT) as resp:  # noqa: S310 (trusted registries)
        return json.load(resp)


def pypi_latest(pkg: str) -> str:
    return http_json(f"https://pypi.org/pypi/{pkg}/json")["info"]["version"]


def npm_latest(pkg: str) -> str:
    # Scoped names (@scope/pkg) must have the slash percent-encoded.
    encoded = pkg.replace("/", "%2F")
    return http_json(f"https://registry.npmjs.org/{encoded}")["dist-tags"]["latest"]


def nuget_latest(pkg: str) -> str:
    data = http_json(f"https://api.nuget.org/v3-flatcontainer/{pkg.lower()}/index.json")
    versions = [v for v in data.get("versions", []) if not _is_prerelease(v)]
    if not versions:
        raise ValueError("no stable versions")
    return versions[-1]


def crates_latest(pkg: str) -> str:
    return http_json(f"https://crates.io/api/v1/crates/{pkg}")["crate"]["max_stable_version"]


RESOLVERS = {
    "pypi": pypi_latest,
    "npm": npm_latest,
    "nuget": nuget_latest,
    "crates": crates_latest,
}

# Which registry each install-block manager resolves against.
MANAGER_REGISTRY = {
    "uv": "pypi",
    "pip": "pypi",
    "pipx": "pypi",
    "npm": "npm",
    "pnpm": "npm",
    "yarn": "npm",
    "bun": "npm",
    "dotnet": "nuget",
    "cargo": "crates",
}


def _is_prerelease(v: str) -> bool:
    return bool(re.search(r"[A-Za-z]", v.split("+")[0]))


def _version_tuple(v: str) -> tuple[int, ...]:
    parts = re.split(r"[.\-+]", v.split("+")[0])
    out: list[int] = []
    for p in parts:
        m = re.match(r"\d+", p)
        out.append(int(m.group()) if m else 0)
    return tuple(out)


def is_behind(pinned: str, latest: str) -> bool:
    a, b = _version_tuple(pinned), _version_tuple(latest)
    width = max(len(a), len(b))
    return a + (0,) * (width - len(a)) < b + (0,) * (width - len(b))


def strip_extras(pkg: str) -> str:
    """`headroom-ai[mcp]` -> `headroom-ai` (extras are not part of the name)."""
    return pkg.split("[", 1)[0].strip()


def parse_runner_arg(command: str, args: list[str]) -> tuple[str, str, str] | None:
    """A pinned `npx pkg@ver` / `uvx --from pkg==ver` invocation → (registry, pkg, ver)."""
    base = command.rsplit("/", 1)[-1]
    registry = {"npx": "npm", "bunx": "npm", "pnpx": "npm", "uvx": "pypi", "pipx": "pypi"}.get(base)
    if registry is None:
        return None
    sep = "==" if registry == "pypi" else "@"
    for arg in args:
        if arg.startswith("-"):
            continue
        # Scoped npm name: @scope/pkg@ver — split on the LAST '@'.
        if registry == "npm" and arg.startswith("@"):
            head, _, ver = arg[1:].rpartition("@")
            if head and ver:
                return (registry, "@" + head, ver)
        elif sep in arg:
            pkg, _, ver = arg.partition(sep)
            if pkg and ver:
                return (registry, pkg, ver)
    return None


def targets() -> list[tuple[str, str, str, str]]:
    """(addon, registry, package, pinned_version) for every resolvable pin."""
    registry = json.loads(REGISTRY.read_text(encoding="utf-8"))
    out: list[tuple[str, str, str, str]] = []
    for entry in registry["addons"]:
        name = entry["addon"]["name"]
        install = entry.get("install") or {}
        if install.get("manager") and install.get("package") and install.get("version"):
            reg = MANAGER_REGISTRY.get(install["manager"].lower())
            if reg:
                out.append((name, reg, strip_extras(install["package"]), install["version"]))
            continue
        mcp = entry.get("mcp") or {}
        if mcp.get("command"):
            parsed = parse_runner_arg(mcp["command"], mcp.get("args", []))
            if parsed:
                out.append((name, parsed[0], parsed[1], parsed[2]))
    return out


def main() -> int:
    strict = "--strict" in sys.argv[1:]
    checked = 0

    for name, registry, pkg, pinned in targets():
        try:
            latest = RESOLVERS[registry](pkg)
        except (urllib.error.URLError, KeyError, ValueError, TimeoutError) as e:
            UNRESOLVED.append(f"{name}: {registry}:{pkg} could not be resolved ({e})")
            continue
        checked += 1
        status = "behind" if is_behind(pinned, latest) else "ok"
        print(f"  {name:<24} {registry:<7} {pkg:<40} pinned={pinned:<12} latest={latest:<12} {status}")
        if status == "behind":
            OUTDATED.append(f"{name}: {pkg} pinned {pinned} but {registry} has {latest}")

    for u in UNRESOLVED:
        print(f"::warning title=Addon version unresolved::{u}")
    for o in OUTDATED:
        print(f"::warning title=Addon pin outdated::{o}")

    print(f"\nchecked {checked} pinned addon(s): {len(OUTDATED)} outdated, {len(UNRESOLVED)} unresolved")

    if OUTDATED and strict:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
