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

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DRY = "--apply" not in sys.argv
NOTE = " (internal, not in this repository)"

# [anything](…internal/whatever.md) — any depth of ../ and an optional docs/ prefix.
LINK = re.compile(r"\[([^\]]*)\]\((?:\.\./)*(?:docs/)?internal/([A-Za-z0-9/_.-]+\.md)\)")

changed = 0
touched = []

for path in sorted(ROOT.rglob("*.md")):
    rel = path.relative_to(ROOT)
    if rel.parts[0] not in ("docs", "README.md", "CONTRACTS.md"):
        continue
    if any(p in rel.parts for p in (".worktrees", "node_modules", "target")):
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
for t in touched[:5]:
    print(f"  {t}")
if len(touched) > 5:
    print(f"  … and {len(touched) - 5} more")
