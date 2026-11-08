#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Print the `--features` list the Test job builds with.

Every feature of every default workspace member, except those that enable
`no-jail` directly or through other features. `no-jail` compiles PathJail
and its tests out, so `--all-features` never tested the jail at all. This is
the same rule as GitLab's `scripts/ci/mr_quick.py --print-test-features`.

Run from `rust/`.
"""

import json
import subprocess

UNTESTED_FEATURES = {"no-jail"}


def main() -> None:
    meta = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--no-deps"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )
    packages = meta["packages"]
    names = {p["name"] for p in packages}
    edges: dict[str, set[str]] = {}
    for p in packages:
        for feature, values in p["features"].items():
            out = edges.setdefault(f"{p['name']}/{feature}", set())
            for value in values:
                if value.startswith("dep:"):
                    continue
                if "/" in value:
                    dep, sub = value.split("/", 1)
                    dep = dep.rstrip("?")
                    if dep in names:
                        out.add(f"{dep}/{sub}")
                else:
                    out.add(f"{p['name']}/{value}")
    bad = {n for n in edges if n.split("/", 1)[1] in UNTESTED_FEATURES}
    changed = True
    while changed:
        changed = False
        for node, outs in edges.items():
            if node not in bad and outs & bad:
                bad.add(node)
                changed = True
    selected = set(meta["workspace_default_members"])
    print(
        ",".join(
            f"{p['name']}/{f}"
            for p in sorted(packages, key=lambda p: p["name"])
            if p["id"] in selected
            for f in sorted(p["features"])
            if f != "default" and f"{p['name']}/{f}" not in bad
        )
    )


if __name__ == "__main__":
    main()
