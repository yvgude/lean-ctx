#!/usr/bin/env python3
"""Stop rendering references to `docs/internal/` as clickable links.

`docs/internal/` was removed from the public repository in #1577, but the
references to it are **not** accidental dead links: `check-narrative-governance.py`
*requires* the fragment `docs/internal/README.md` in public entry points, so
that a reader can see product scope is governed by material that is
deliberately not published. Retargeting them at a public document would
misstate what governs what, and breaks the contract outright — the first
attempt at this did exactly that, and CI said so.

What is genuinely wrong is only the markdown link syntax: it renders as
clickable and resolves to nothing inside the repo. So the path stays, as prose,
and the link goes:

    [`docs/internal/README.md`](../internal/README.md)
 -> `docs/internal/README.md` (internal, not in this repository)

Run with --apply; default is a dry run.
"""

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

DRY = "--apply" not in sys.argv
NOTE = " (internal, not in this repository)"

# Left alone on purpose, and derived rather than hand-listed — a copied list
# drifts, and this script has already been wrong twice by guessing what it was
# safe to touch:
#
#  1. OCLA contract-pack artifacts. `docs/contracts/ocla-contract-pack-v1.json`
#     records a SHA-256 per file under a fixed pack `version`. Editing the text
#     invalidates a digest consumers verify; rewriting the digests would let two
#     different contents claim the same version, which is the drift the pack
#     exists to prevent.
#  2. Frozen contracts. `rust/tests/suite/contracts_frozen.rs` hashes every
#     contract classified `Frozen` against `docs/contracts/frozen-hashes.json`
#     and fails on any edit; CONTRACTS.md § Contract file rule says a change
#     lands as a new `-v2.md` file, never as an edit.
#
# In both cases a dead link inside the document is the smaller problem than an
# attestation that no longer means anything.
def _excluded() -> frozenset[str]:
    out: set[str] = set()

    frozen = ROOT / "docs/contracts/frozen-hashes.json"
    if frozen.exists():
        out |= {f"docs/contracts/{name}" for name in json.loads(frozen.read_text())}

    suite = ROOT / "scripts/verify-ocla-contract-suite.py"
    if suite.exists():
        block = re.search(
            r"CONTRACT_PACK_ARTIFACTS\s*=\s*frozenset\(\s*\{(.*?)\}\s*\)",
            suite.read_text(),
            re.S,
        )
        if block:
            out |= set(re.findall(r'"([^"]+\.md)"', block.group(1)))

    return frozenset(out)


EXCLUDED = _excluded()

# [anything](…internal/whatever.md) — any depth of ../ and an optional docs/ prefix.
LINK = re.compile(r"\[([^\]]*)\]\((?:\.\./)*(?:docs/)?internal/([A-Za-z0-9/_.-]+\.md)\)")

changed = 0
touched = []
skipped = []

for path in sorted(ROOT.rglob("*.md")):
    rel = path.relative_to(ROOT)
    if rel.parts[0] not in ("docs", "README.md", "CONTRACTS.md"):
        continue
    if any(p in rel.parts for p in (".worktrees", "node_modules", "target")):
        continue
    if str(rel) in EXCLUDED:
        skipped.append(str(rel))
        continue

    text = original = path.read_text(encoding="utf-8")
    if "internal/" not in text:
        continue

    def repair(m: re.Match) -> str:
        label, target = m.group(1), m.group(2)
        path = f"docs/internal/{target}"
        # The path itself must survive: `check-narrative-governance.py` looks
        # for the literal fragment (e.g. `internal/README.md`) in guarded
        # entry points. Dropping it in favour of a prettier label is what broke
        # the first attempt at this cleanup.
        if label.strip() and target.rsplit("/", 1)[-1] not in label:
            return f"{label} (`{path}`, internal — not in this repository)"
        return f"`{path}`{NOTE}"

    text = LINK.sub(repair, text)

    # Avoid stacking the note when a file is processed twice.
    text = re.sub(re.escape(NOTE) + r"(?:" + re.escape(NOTE) + r")+", NOTE, text)

    if text != original:
        changed += len(LINK.findall(original))
        touched.append(str(rel))
        if not DRY:
            path.write_text(text, encoding="utf-8")

print(f"{'DRY RUN — ' if DRY else ''}{len(touched)} files, {changed} links de-linked")
if skipped:
    print(f"  skipped {len(skipped)} attested file(s) — frozen or digest-pinned:")
    for s_ in skipped:
        print(f"    {s_}")
for t in touched[:5]:
    print(f"  {t}")
if len(touched) > 5:
    print(f"  … and {len(touched) - 5} more")
