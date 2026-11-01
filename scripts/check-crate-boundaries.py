#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Keep the engine crate split moving in one direction ("Split before you grow").

Reads rust/CRATE_MAP.toml and enforces three rules:

1. A module scheduled for extraction may not gain a new outbound `crate::`
   edge beyond its recorded baseline (rust/crate_boundary_baseline.json).
   Every such edge is coupling that the extraction would have to cut first.
2. An extracted crate never depends on the engine (`lean-ctx` / `lean_ctx`),
   and depends on other workspace crates only where the map allows it.
3. An extracted module's old engine directory keeps only its facade files, so
   new code lands in the crate, not next to the facade.

`--update` rewrites the baseline from the current tree. Shrinking it is always
welcome; growing it needs the crate-split lead's sign-off in the handover
("monolith: <reason>"). Output is deterministic: sorted, no timestamps.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
RUST = REPO / "rust"
MAP = RUST / "CRATE_MAP.toml"
BASELINE = RUST / "crate_boundary_baseline.json"

IDENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
LINE_COMMENT = re.compile(r"(?<!:)//(?![/!]).*$", re.M)


def module_of(rel: str) -> str:
    """`core/foo/bar.rs` -> `core::foo`, `cli/x.rs` -> `cli`, `lib.rs` -> `lib`."""
    parts = rel.split("/")
    if parts[0] == "core" and len(parts) > 1:
        name = parts[1][:-3] if parts[1].endswith(".rs") else parts[1]
        return f"core::{name}"
    return parts[0][:-3] if parts[0].endswith(".rs") else parts[0]


def _group_items(text: str, pos: int) -> tuple[list[str], int]:
    """Parse a `{a, b::c, d::{e}}` group starting at `pos` (the `{`)."""
    depth, start, items = 0, pos + 1, []
    i = pos
    while i < len(text):
        ch = text[i]
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                items.append(text[start:i])
                return [it.strip() for it in items if it.strip()], i + 1
        elif ch == "," and depth == 1:
            items.append(text[start:i])
            start = i + 1
        i += 1
    return [], len(text)


def _targets(path: str) -> list[str]:
    """Module targets named by a path relative to `crate::`."""
    path = path.strip()
    if path.startswith("{"):
        items, _ = _group_items(path, 0)
        return [t for item in items for t in _targets(item)]
    head = IDENT.match(path)
    if not head:
        return []
    name = head.group(0)
    if name in {"self", "super"}:
        return []
    rest = path[head.end():].lstrip()
    if name != "core":
        return [name]
    if not rest.startswith("::"):
        return []
    rest = rest[2:].lstrip()
    if rest.startswith("{"):
        items, _ = _group_items(rest, 0)
        out = []
        for item in items:
            m = IDENT.match(item)
            if m and m.group(0) not in {"self", "super"}:
                out.append(f"core::{m.group(0)}")
        return out
    m = IDENT.match(rest)
    return [f"core::{m.group(0)}"] if m else []


def edges_of_text(text: str) -> set[str]:
    text = LINE_COMMENT.sub("", text)
    found: set[str] = set()
    for m in re.finditer(r"\bcrate::", text):
        tail = text[m.end():]
        if tail.startswith("{"):
            _, end = _group_items(tail, 0)
            found.update(_targets(tail[:end]))
        else:
            seg = re.match(r"[A-Za-z_][A-Za-z0-9_]*(\s*::\s*(\{|[A-Za-z_][A-Za-z0-9_]*))?", tail)
            if not seg:
                continue
            if seg.group(2) == "{":
                brace = tail.index("{", seg.start(2))
                _, end = _group_items(tail, brace)
                found.update(_targets(tail[:end]))
            else:
                found.update(_targets(seg.group(0)))
    return found


def module_files(src: Path, module: str) -> list[Path]:
    rel = module.replace("::", "/")
    files = []
    single = src / f"{rel}.rs"
    if single.is_file():
        files.append(single)
    folder = src / rel
    if folder.is_dir():
        files.extend(sorted(folder.rglob("*.rs")))
    return files


def current_edges(src: Path, module: str) -> set[str]:
    edges: set[str] = set()
    for path in module_files(src, module):
        edges |= edges_of_text(path.read_text(encoding="utf-8", errors="replace"))
    edges.discard(module)
    return edges


def load_map() -> dict:
    with MAP.open("rb") as fh:
        return tomllib.load(fh)


def check_crates(cmap: dict, errors: list[str]) -> None:
    allowed_names = {c["name"] for c in cmap.get("crate", [])}
    for crate in cmap.get("crate", []):
        root = RUST / crate["path"]
        manifest = (root / "Cargo.toml").read_text(encoding="utf-8")
        deps = set(re.findall(r'^\s*([A-Za-z0-9_-]+)\s*=\s*\{[^}\n]*\bpath\s*=', manifest, re.M))
        for dep in sorted(deps):
            if dep == "lean-ctx":
                errors.append(f"{crate['name']}: depends on the engine crate lean-ctx")
            elif dep in allowed_names and dep not in crate.get("may_depend_on", []):
                errors.append(
                    f"{crate['name']}: depends on {dep}, which CRATE_MAP.toml does not allow"
                )
        for path in sorted((root / "src").rglob("*.rs")):
            text = LINE_COMMENT.sub("", path.read_text(encoding="utf-8", errors="replace"))
            if re.search(r"\blean_ctx::", text):
                errors.append(f"{path.relative_to(REPO)}: names lean_ctx:: (crates never depend on the engine)")
        for facade in crate.get("facade", []):
            folder = RUST / "src" / facade["dir"]
            keep = set(facade.get("files", []))
            if folder.is_dir():
                for path in sorted(folder.rglob("*")):
                    rel = path.relative_to(folder).as_posix()
                    if path.is_file() and rel not in keep:
                        errors.append(
                            f"rust/src/{facade['dir']}/{rel}: new code beside the "
                            f"{crate['name']} facade; put it into {crate['path']}"
                        )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--update", action="store_true", help="rewrite the edge baseline")
    args = parser.parse_args(argv)

    cmap = load_map()
    src = RUST / "src"
    scheduled = sorted(
        {m for entry in cmap.get("scheduled", []) for m in entry["modules"]}
    )
    now = {m: sorted(current_edges(src, m)) for m in scheduled}

    if args.update:
        BASELINE.write_text(json.dumps(now, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        print(f"crate boundaries: baseline rewritten for {len(now)} scheduled modules")
        return 0

    errors: list[str] = []
    try:
        baseline = json.loads(BASELINE.read_text(encoding="utf-8"))
    except FileNotFoundError:
        baseline = {}
        errors.append(f"{BASELINE.relative_to(REPO)} missing; run with --update")

    shrunk = 0
    for module in scheduled:
        if not module_files(src, module):
            errors.append(f"{module}: scheduled in CRATE_MAP.toml but not found under rust/src")
            continue
        base = set(baseline.get(module, []))
        if module not in baseline:
            errors.append(f"{module}: no baseline entry; run with --update")
            continue
        for target in sorted(set(now[module]) - base):
            errors.append(
                f"{module} -> {target}: new edge out of a module scheduled for a crate. "
                "Avoid it, or record 'monolith: <reason>' with the crate-split lead and run --update."
            )
        if base - set(now[module]):
            shrunk += 1

    check_crates(cmap, errors)

    if errors:
        print("crate boundaries: FAILED")
        for line in errors:
            print(f"  {line}")
        return 1
    note = f"; {shrunk} module(s) shed edges, run --update to lock that in" if shrunk else ""
    print(f"crate boundaries: OK ({len(scheduled)} scheduled modules, "
          f"{len(cmap.get('crate', []))} extracted crates{note})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
