#!/usr/bin/env python3
"""Merge Arabic locale patches into ar.json (preserves key order, inserts new sections)."""
from __future__ import annotations

import json
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
LOCALES = SCRIPT_DIR.parent / "src" / "i18n" / "locales"
AR_PATH = LOCALES / "ar.json"

PROTOCOL_KEYS = ["protocolsIndex", "protocolsCcp", "protocolsCep", "protocolsTdd"]
DOCS_TOOLS_KEYS = [
    "docsToolsCore",
    "docsToolsIntelligence",
    "docsToolsMemory",
    "docsToolsSession",
]


def load_patch() -> dict:
    patch: dict = {}
    for p in sorted(SCRIPT_DIR.glob("ar_patch_*.json")):
        chunk = json.loads(p.read_text(encoding="utf-8"))
        overlap = set(patch) & set(chunk)
        if overlap:
            raise SystemExit(f"Duplicate keys in patch files: {overlap}")
        patch.update(chunk)
    return patch


def merge_flat(base: dict, updates: dict) -> dict:
    out = dict(base)
    out.update(updates)
    return out


def rebuild(ar: dict, patch: dict) -> dict:
    out: dict = {}
    inserted_docs = False
    inserted_pro = False

    for k, v in ar.items():
        if k == "docs.treeSitter" and not inserted_docs:
            for dk in DOCS_TOOLS_KEYS:
                if dk in patch:
                    out[dk] = merge_flat(ar.get(dk) or {}, patch[dk])
            inserted_docs = True

        if k == "pro" and not inserted_pro:
            for pk in PROTOCOL_KEYS:
                if pk in patch:
                    out[pk] = merge_flat(ar.get(pk) or {}, patch[pk])
            inserted_pro = True

        if k in PROTOCOL_KEYS or k in DOCS_TOOLS_KEYS:
            continue

        if k in patch and isinstance(v, dict):
            out[k] = merge_flat(v, patch[k])
        else:
            out[k] = v

    return out


def main() -> None:
    patch = load_patch()
    if not patch:
        print("No ar_patch_*.json files found.", file=sys.stderr)
        sys.exit(1)

    ar = json.loads(AR_PATH.read_text(encoding="utf-8"))
    merged = rebuild(ar, patch)
    out_text = json.dumps(merged, ensure_ascii=False, indent=2) + "\n"
    AR_PATH.write_text(out_text, encoding="utf-8")
    json.loads(out_text)
    print(f"OK: wrote {AR_PATH} ({len(out_text.splitlines())} lines)")


if __name__ == "__main__":
    main()
