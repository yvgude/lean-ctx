#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Vendor and bundle the Hive subscription model catalog (mz6/mz14 T4).

The engine ships `rust/data/model_catalog.json` for offline labels (origin
and host country of every model) and to recognise a vendor's own API host
(`[[direct]]`). It is generated, never edited by hand, from
`rust/data/model_catalog.hive.toml`: an exact copy of the Hive catalog
`crates/hive-server/data/model_catalog.toml`, preceded by one header line
that names the Hive commit and the sha256 of the copied bytes.

    scripts/sync-model-catalog.py --from <catalog.toml> --commit <hive sha>
        refresh the vendored copy from a local leanctx-hive checkout (or a
        file fetched with a personal token, locally only) and regenerate
        the JSON bundle
    scripts/sync-model-catalog.py --check
        exit 1 unless the vendored body matches its recorded sha256 and the
        bundled JSON equals the JSON regenerated from it (the CI diff check)

Canonical JSON: the TOML parsed by tomllib, dumped with sorted keys,
indent 2, ASCII-safe, one trailing newline. Python >= 3.11.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
BUNDLE = REPO / "rust" / "data" / "model_catalog.json"
VENDORED = REPO / "rust" / "data" / "model_catalog.hive.toml"
HIVE_PROJECT = "leanctx/leanctx-hive"
HIVE_PATH = "crates/hive-server/data/model_catalog.toml"
SCHEMA = "leanctx.hive.model-catalog/v1"
HEADER = re.compile(
    r"# vendored from (?P<project>\S+)@(?P<commit>[0-9a-f]{40}):(?P<path>\S+) "
    r"sha256=(?P<sha256>[0-9a-f]{64}); regenerate with scripts/sync-model-catalog.py\n"
)


def canonical(value: object) -> str:
    return json.dumps(value, sort_keys=True, indent=2, ensure_ascii=True) + "\n"


def bundle_from(body: bytes) -> str:
    catalog = tomllib.loads(body.decode("utf-8"))
    if catalog.get("schema") != SCHEMA:
        raise SystemExit(f"catalog schema is {catalog.get('schema')!r}, expected {SCHEMA!r}")
    return canonical(catalog)


def header_for(commit: str, body: bytes) -> str:
    return (
        f"# vendored from {HIVE_PROJECT}@{commit}:{HIVE_PATH} "
        f"sha256={hashlib.sha256(body).hexdigest()}; regenerate with scripts/sync-model-catalog.py\n"
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--from", dest="source", type=Path, help="Hive model_catalog.toml")
    parser.add_argument("--commit", help="Hive commit the --from file was taken at")
    parser.add_argument("--check", action="store_true", help="verify, write nothing")
    args = parser.parse_args()

    if not args.check:
        if args.source is None or not args.commit or not re.fullmatch(r"[0-9a-f]{40}", args.commit):
            parser.error("refreshing needs --from and a full 40-hex --commit")
        body = args.source.read_bytes()
        bundle = bundle_from(body)
        VENDORED.write_bytes(header_for(args.commit, body).encode("ascii") + body)
        BUNDLE.write_text(bundle, encoding="utf-8")
        print(f"model catalog vendored from {HIVE_PROJECT}@{args.commit}:{HIVE_PATH}")
        return 0

    raw = VENDORED.read_bytes()
    first, sep, body = raw.partition(b"\n")
    match = HEADER.fullmatch(first.decode("utf-8", "replace") + sep.decode())
    if match is None:
        print(f"{VENDORED.relative_to(REPO)}: missing or malformed header line", file=sys.stderr)
        return 1
    digest = hashlib.sha256(body).hexdigest()
    if digest != match["sha256"]:
        print(
            f"{VENDORED.relative_to(REPO)}: body sha256 {digest} differs from the recorded "
            f"{match['sha256']}; refresh it from {match['project']}, never edit it by hand",
            file=sys.stderr,
        )
        return 1
    if bundle_from(body) != BUNDLE.read_text(encoding="utf-8"):
        print(f"{BUNDLE.relative_to(REPO)} differs from the vendored catalog; regenerate it", file=sys.stderr)
        return 1
    print(f"model catalog bundle equals {match['project']}@{match['commit']}:{match['path']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
