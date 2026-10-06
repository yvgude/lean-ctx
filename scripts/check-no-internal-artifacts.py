#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Fail closed when Git tracks internal artifacts or public-boundary paths.

Default mode keeps the internal-documentation guard used by private CI.
``--public`` enforces the canonical ``.github-ignore`` boundary against the Git
index and rejects every tracked entry the policy blocks, without exceptions.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path


INTERNAL_ARCHIVE = re.compile(r"^docs/internal[^/]*\.zip$", re.IGNORECASE)
# Private-plane delivery receipts and design notes. They cite private services,
# hosts and commercial rollout detail, so no name under these families may be
# tracked, whatever a future receipt is called.
PRIVATE_RECEIPT = re.compile(
    r"^docs/contracts/v4-[^/]+\.md$|^docs/architecture/v4_[^/]+$", re.IGNORECASE
)

POLICY_FILENAME = ".github-ignore"
MAX_POLICY_BYTES = 64 * 1024
MAX_POLICY_ENTRIES = 1000
MAX_TRACKED_PATHS = 10000
MAX_GIT_OUTPUT_BYTES = 8 * 1024 * 1024
GIT_TIMEOUT_SECONDS = 120

# Policy entries use printable ASCII; invisible Unicode must not weaken a rule.
UNSAFE_ENTRY = re.compile(r"[*?\[\]!\\]|[^\x21-\x7e]")


class GuardError(Exception):
    """A fail-closed guard condition with a deterministic diagnostic."""


def escape(value: str) -> str:
    """Render a path as printable ASCII so unusual bytes cannot forge output."""
    return "".join(
        chr(byte) if 0x20 <= byte < 0x7F and byte != 0x5C else f"\\x{byte:02x}"
        for byte in value.encode("utf-8", "surrogateescape")
    )


def is_forbidden(path: str) -> bool:
    normalized = path.replace("\\", "/").lstrip("./")
    lowered = normalized.casefold()
    return (
        lowered == "docs/internal"
        or lowered.startswith("docs/internal/")
        or lowered == "docs/archive"
        or lowered.startswith("docs/archive/")
        or INTERNAL_ARCHIVE.fullmatch(normalized) is not None
        or PRIVATE_RECEIPT.fullmatch(normalized) is not None
    )


def entry_problem(entry: str) -> str | None:
    """Return why a policy entry is unsupported, or None when it is safe."""
    if UNSAFE_ENTRY.search(entry):
        return "unsupported pattern or unsafe character"
    if entry.startswith(("/", "~")):
        return "entry must be repository-relative"
    path = entry[:-1] if entry.endswith("/") else entry
    if any(segment in ("", ".", "..") for segment in path.split("/")):
        return "entry must not contain empty or relative segments"
    return None


def policy_entries(text: str) -> list[str]:
    """Parse ``.github-ignore`` the way .githooks/pre-push reads it."""
    entries: list[str] = []
    for number, line in enumerate(text.split("\n"), start=1):
        entry = line.split("#", 1)[0].replace(" ", "")
        if not entry:
            continue
        problem = entry_problem(entry)
        if problem is not None:
            raise GuardError(
                f"{POLICY_FILENAME} line {number}: {problem}: {escape(entry)}"
            )
        if len(entries) >= MAX_POLICY_ENTRIES:
            raise GuardError(
                f"{POLICY_FILENAME} declares more than {MAX_POLICY_ENTRIES} entries"
            )
        entries.append(entry)
    if not entries:
        raise GuardError(f"{POLICY_FILENAME} declares no blocked entries")
    return entries


def load_policy(root: Path) -> list[str]:
    path = root / POLICY_FILENAME
    if path.is_symlink():
        raise GuardError(f"{POLICY_FILENAME} must be a regular file, not a symlink")
    if not path.is_file():
        raise GuardError(f"{POLICY_FILENAME} is missing under {escape(str(root))}")
    size = path.stat().st_size
    if size == 0:
        raise GuardError(f"{POLICY_FILENAME} is empty")
    if size > MAX_POLICY_BYTES:
        raise GuardError(f"{POLICY_FILENAME} exceeds {MAX_POLICY_BYTES} bytes")
    try:
        with path.open("rb") as stream:
            raw = stream.read(MAX_POLICY_BYTES + 1)
        if not raw or len(raw) > MAX_POLICY_BYTES:
            raise GuardError(f"{POLICY_FILENAME} is empty or exceeds {MAX_POLICY_BYTES} bytes")
        text = raw.decode("utf-8")
    except UnicodeDecodeError:
        raise GuardError(f"{POLICY_FILENAME} is not valid UTF-8") from None
    return policy_entries(text)


def tracked_paths(root: Path) -> list[str]:
    result = subprocess.run(
        ["git", "-C", str(root), "ls-files", "-z"],
        check=True,
        capture_output=True,
        timeout=GIT_TIMEOUT_SECONDS,
    )
    if len(result.stdout) > MAX_GIT_OUTPUT_BYTES:
        raise GuardError("git ls-files output exceeds the bounded size")
    paths = [path.decode("utf-8", "surrogateescape") for path in result.stdout.split(b"\0") if path]
    if len(paths) > MAX_TRACKED_PATHS:
        raise GuardError(f"more than {MAX_TRACKED_PATHS} tracked paths")
    return paths


def find_forbidden_tracked_paths(root: Path) -> list[str]:
    return sorted(path for path in tracked_paths(root) if is_forbidden(path))


def find_forbidden_listed_paths(stream) -> list[str]:
    """Forbidden entries of a newline- or NUL-separated path list (pre-push)."""
    raw = stream.read(MAX_GIT_OUTPUT_BYTES + 1)
    if len(raw) > MAX_GIT_OUTPUT_BYTES:
        raise GuardError("path list exceeds the bounded size")
    paths = [
        path.decode("utf-8", "surrogateescape")
        for path in re.split(rb"[\0\n]", raw)
        if path
    ]
    return sorted({path for path in paths if is_forbidden(path)})


def public_violations(entries: list[str], tracked: list[str]) -> list[tuple[str, str]]:
    """Every tracked path blocked by the policy, as (path, entry) pairs."""
    exact = {entry for entry in entries if not entry.endswith("/")}
    prefixes = sorted(entry for entry in entries if entry.endswith("/"))
    violations: set[tuple[str, str]] = set()
    for path in tracked:
        if path in exact:
            violations.add((path, path))
            continue
        for prefix in prefixes:
            if path.startswith(prefix) or path == prefix.rstrip("/"):
                violations.add((path, prefix))
                break
    return sorted(violations)


def find_public_violations(root: Path) -> list[tuple[str, str]]:
    top = subprocess.run(
        ["git", "-C", str(root), "rev-parse", "--show-toplevel"],
        check=True,
        capture_output=True,
        timeout=GIT_TIMEOUT_SECONDS,
    ).stdout.decode("utf-8", "surrogateescape").rstrip("\n")
    if Path(top).resolve() != root.resolve():
        raise GuardError("--root must be the Git worktree root")
    entries = load_policy(root)
    tracked = tracked_paths(root)
    violations = set(public_violations(entries, tracked))
    blocked = {path for path, _ in violations}
    violations.update(
        (path, "internal-artifact") for path in tracked if path not in blocked and is_forbidden(path)
    )
    return sorted(violations)


def main() -> int:
    parser = argparse.ArgumentParser(description="Tracked-artifact boundary guard.")
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument(
        "--public",
        action="store_true",
        help=f"enforce the canonical {POLICY_FILENAME} boundary (no exceptions)",
    )
    parser.add_argument(
        "--stdin-paths",
        action="store_true",
        help="check the newline/NUL-separated paths on stdin instead of the index",
    )
    args = parser.parse_args()
    label = "public-boundary" if args.public else "internal-artifact"

    try:
        root = args.root.resolve()
        if not root.is_dir():
            raise GuardError(f"--root is not a directory: {escape(str(args.root))}")
        if args.public:
            violations = find_public_violations(root)
        elif args.stdin_paths:
            forbidden = find_forbidden_listed_paths(sys.stdin.buffer)
        else:
            forbidden = find_forbidden_tracked_paths(root)
    except GuardError as error:
        print(f"{label} guard failed closed: {error}", file=sys.stderr)
        return 2
    except (OSError, subprocess.SubprocessError) as error:
        print(f"{label} guard failed closed: {type(error).__name__}", file=sys.stderr)
        return 2

    if args.public:
        if violations:
            print(f"tracked paths blocked by {POLICY_FILENAME}:", file=sys.stderr)
            for path, entry in violations:
                print(f"- {escape(path)} [{escape(entry)}]", file=sys.stderr)
            return 1
        print(f"No {POLICY_FILENAME} blocked paths are tracked.")
        return 0

    if forbidden:
        print("forbidden tracked internal artifacts:", file=sys.stderr)
        for path in forbidden:
            print(f"- {escape(path)}", file=sys.stderr)
        return 1

    print("No forbidden internal artifacts are tracked.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
