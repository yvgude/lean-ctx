#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Write the public projection of the canonical capability registry.

The private registry (`product/capabilities.toml`) names, for every capability, where its
source lives and how it is rated internally. The public tree carries a projection: the same
entries in the same bytes, except that a capability with `source_visibility = "private_source"`
discloses neither its source path, nor its internal criticality and moat rating, nor its
private maturity. The projection states `projection = "public"` so the capability checker
treats it as such.

    project-capabilities.py --registry product/capabilities.toml --out <file>
    project-capabilities.py --registry product/capabilities.toml --check <published file>

Deterministic: the same registry bytes always give the same projection bytes.
"""
from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

WITHHELD = {
    "source_path": '"undisclosed"',
    "aha_criticality": '"undisclosed"',
    "moat_sensitivity": '"undisclosed"',
    "public_maturity": '"private"',
}
KEY = re.compile(r'^(?P<key>[a-z_]+)\s*=\s*(?P<value>.*)$')


def project(text: str) -> str:
    if re.search(r'(?m)^projection\s*=', text):
        raise ValueError("the input is already a projection")
    schema = re.search(r'(?m)^schema_version = 3\n', text)
    if not schema:
        raise ValueError("registry schema_version 3 line not found")
    text = text[:schema.end()] + 'projection = "public"\n' + text[schema.end():]
    blocks = re.split(r'(?m)^(?=\[\[capability\]\]\s*$)', text)
    out = [blocks[0]]
    for block in blocks[1:]:
        private = re.search(r'(?m)^source_visibility\s*=\s*"private_source"\s*$', block)
        if private:
            lines = []
            for line in block.splitlines(keepends=True):
                match = KEY.match(line.rstrip("\n"))
                if match and match.group("key") in WITHHELD:
                    line = f'{match.group("key")} = {WITHHELD[match.group("key")]}\n'
                lines.append(line)
            block = "".join(lines)
        out.append(block)
    return "".join(out)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--registry", required=True)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--out")
    group.add_argument("--check")
    args = parser.parse_args()
    projected = project(Path(args.registry).read_text(encoding="utf-8"))
    if args.out:
        Path(args.out).write_text(projected, encoding="utf-8")
        return 0
    if Path(args.check).read_text(encoding="utf-8") != projected:
        print(f"project-capabilities: {args.check} is not the projection of {args.registry}",
              file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
