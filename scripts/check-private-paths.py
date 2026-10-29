#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Fail when a path that must stay off the public mirror is tracked.

`.github-ignore` is the single list of paths that never belong in this
repository. Two ways it can be defeated are checked here:

1. A denied path is already tracked. The pre-push hook only inspects files a
   push adds, so a file committed before its path was denied stays public
   forever unless the tracked tree itself is checked.
2. A path in the internal section of `.gitignore` is missing from `.github-ignore`.
   `.gitignore` only stops `git add`; a forced add or an older commit still
   publishes the file, and nothing else would notice.
3. Paid detection content in a public path: model files, the names of the
   commercial detection runtime, or a built-in policy pack that is not on the
   community list. Domain filters (legal, health, finance, company-specific)
   and model packs are commercial; the public tree keeps the deterministic
   floor and a neutral, license-gated seam only.

Usage: check-private-paths.py [repo-root]
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path
from typing import Callable, Iterable, List, Optional

# `.gitignore` sections whose entries must never be published, by header line.
INTERNAL_GITIGNORE_SECTIONS = ("Internal strategy docs (confidential)",)

MODEL_ARTIFACT_SUFFIXES = (".onnx", ".safetensors", ".gguf", ".pt", ".pth", ".ckpt")
MODEL_ARTIFACT_NAMES = ("tokenizer.json", "tokenizer_config.json")
# Model families, sources and methods of the commercial detection runtime.
PAID_DETECTION_TERMS = re.compile(
    r"(?i)\b(?:gliner|deberta|quantize_dynamic|span_extraction)\b"
    r"|(?i:protectai|urchade|knowledgator)/"
)
# Evaluation corpus name; case-sensitive so ordinary words do not match.
PAID_DETECTION_CORPUS = re.compile(r"\bMAPA\b")
TEXT_SUFFIXES = (".rs", ".toml", ".md", ".json", ".py", ".ts", ".tsx", ".js", ".yml", ".yaml", ".txt", ".sh")
POLICY_BUILTIN_DIR = "rust/src/core/policy/builtin/"
COMMUNITY_POLICY_PACKS = {"baseline", "strict-redaction", "open-source"}
# Compliance-framework mappings and their per-control assessment are Enterprise
# (the licensed runtime, ee/intelligence). The engine keeps only the facts it
# sends, the signed report format and its verification (core/compliance_evidence).
PAID_PUBLIC_PREFIXES = ("rust/data/compliance/", "rust/src/core/compliance_report/")
PAID_PUBLIC_FILES = {"rust/src/core/compliance.rs", "rust/src/cli/compliance_cmd.rs"}
# Control ids of the framework mappings (EU AI Act, ISO 42001, SOC 2).
PAID_COMPLIANCE_CONTROLS = re.compile(r"\b(?:AIA-[0-9]|ISO-A\.[0-9]|SOC2-C{1,2}[0-9])")
# Files that name the terms in order to forbid them.
PAID_TERM_EXEMPT = {"scripts/check-private-paths.py", "tests/security/test_private_paths.py"}


def _entries(lines: Iterable[str]) -> List[str]:
    out = []
    for raw in lines:
        line = raw.strip()
        if line and not line.startswith("#"):
            out.append(line)
    return out


def denied_paths(github_ignore: str) -> List[str]:
    return [line.lstrip("/") for line in _entries(github_ignore.splitlines())]


def internal_gitignore_entries(gitignore: str) -> List[str]:
    """Entries of the internal sections, without glob-only patterns."""
    entries: List[str] = []
    active = False
    for raw in gitignore.splitlines():
        line = raw.strip()
        if line.startswith("# ──"):
            active = any(name in line for name in INTERNAL_GITIGNORE_SECTIONS)
            continue
        if not active or not line or line.startswith("#"):
            continue
        if "*" in line:
            continue
        entries.append(line.lstrip("/"))
    return entries


def _covered(path: str, denied: Iterable[str]) -> bool:
    for entry in denied:
        if entry.endswith("/"):
            if path == entry.rstrip("/") or path.startswith(entry):
                return True
        elif path == entry:
            return True
    return False


def find_violations(github_ignore: str, gitignore: str, tracked: Iterable[str]) -> List[str]:
    denied = denied_paths(github_ignore)
    findings = []
    for entry in internal_gitignore_entries(gitignore):
        if not _covered(entry, denied):
            findings.append(
                f"[drift] internal .gitignore entry {entry!r} is missing from .github-ignore"
            )
    for path in sorted(tracked):
        if _covered(path, denied):
            findings.append(f"[tracked] {path} is listed in .github-ignore but tracked")
    return findings


def paid_detection_findings(
    public: Iterable[str], read_text: Callable[[str], Optional[str]]
) -> List[str]:
    """Public paths that carry commercial detection content."""
    findings = []
    for path in sorted(public):
        name = path.rsplit("/", 1)[-1]
        if name.endswith(MODEL_ARTIFACT_SUFFIXES) or name in MODEL_ARTIFACT_NAMES:
            findings.append(f"[paid] {path} is a model file; model packs ship signed and private")
            continue
        if path.startswith(POLICY_BUILTIN_DIR) and name.endswith(".toml"):
            pack = name[: -len(".toml")]
            if pack not in COMMUNITY_POLICY_PACKS:
                findings.append(
                    f"[paid] {path} is a built-in policy pack outside the community floor; "
                    "domain and compliance packs are commercial "
                    "(community keeps baseline, strict-redaction, open-source)"
                )
        if path.startswith(PAID_PUBLIC_PREFIXES) or path in PAID_PUBLIC_FILES:
            findings.append(
                f"[paid] {path} is compliance-framework content; it belongs to LeanCTX Enterprise"
            )
            continue
        if path in PAID_TERM_EXEMPT or not name.endswith(TEXT_SUFFIXES):
            continue
        text = read_text(path)
        if text is None:
            continue
        for number, line in enumerate(text.splitlines(), 1):
            match = PAID_DETECTION_TERMS.search(line) or PAID_DETECTION_CORPUS.search(line)
            if match:
                findings.append(
                    f"[paid] {path}:{number} names the commercial detection runtime "
                    f"({match.group(0)!r})"
                )
                break
            control = PAID_COMPLIANCE_CONTROLS.search(line)
            if control:
                findings.append(
                    f"[paid] {path}:{number} carries a compliance-framework mapping "
                    f"({control.group(0)!r}); mappings belong to the Enterprise runtime"
                )
                break
    return findings


def _read_text(root: Path) -> Callable[[str], Optional[str]]:
    def read(path: str) -> Optional[str]:
        try:
            return (root / path).read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            return None

    return read


def main(argv: List[str]) -> int:
    root = Path(argv[1] if len(argv) > 1 else Path(__file__).resolve().parents[1])
    try:
        github_ignore = (root / ".github-ignore").read_text(encoding="utf-8")
        gitignore = (root / ".gitignore").read_text(encoding="utf-8")
        tracked = subprocess.run(
            ["git", "-C", str(root), "ls-files", "-z"],
            check=True,
            capture_output=True,
        ).stdout.decode("utf-8").split("\0")
    except (OSError, subprocess.CalledProcessError) as error:
        print(f"check-private-paths: cannot read repository state: {error}", file=sys.stderr)
        return 2
    tracked = [t for t in tracked if t]
    findings = find_violations(github_ignore, gitignore, tracked)
    denied = denied_paths(github_ignore)
    public = [t for t in tracked if not _covered(t, denied)]
    findings += paid_detection_findings(public, _read_text(root))
    for finding in findings:
        print(finding)
    if findings:
        print(
            "Private paths and paid detection content must not be tracked in the public "
            "repository. See .github-ignore."
        )
        return 1
    print("check-private-paths: ok")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
