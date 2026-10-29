#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Generate and verify the positive public export of a pinned private commit.

The public tree is *selected*, never filtered: a path is exported only when the
publish manifest includes it. Everything else — unknown, new or private — stays
out because nothing names it. A second, independent list of forbidden prefixes
is checked against the selection and against the generated tree, so a manifest
mistake fails instead of exporting.

  export   read a commit's objects (never the working tree), write the selected
           files into an empty directory and a receipt beside it
  verify   check that a directory is exactly the receipt's tree — the whole
           tree, every path — and that the receipt follows from the manifest

This tool selects paths. It grants no right: licence classification stays with
`ip-provenance-audit.py`, dependency closure with `check-source-layout.py
--profile community`, and both run on the generated tree.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import subprocess
import sys
import tomllib

SCHEMA = 1
RECEIPT_NAME = "PUBLIC-EXPORT-RECEIPT.json"
COMMIT = re.compile(r"[0-9a-f]{40}")
REGULAR_MODES = {"100644", "100755"}


class ExportError(Exception):
    """A refusal; the message names the first offending path or field."""


def _pattern(glob: str) -> re.Pattern[str]:
    """`**` crosses directories, `*` and `?` stay inside one path component."""
    if not glob or glob.startswith("/") or "\\" in glob or ".." in PurePosixPath(glob).parts:
        raise ExportError(f"invalid pattern: {glob!r}")
    out, i = [], 0
    while i < len(glob):
        if glob.startswith("**/", i):
            out.append("(?:.*/)?")
            i += 3
        elif glob.startswith("**", i):
            out.append(".*")
            i += 2
        elif glob[i] == "*":
            out.append("[^/]*")
            i += 1
        elif glob[i] == "?":
            out.append("[^/]")
            i += 1
        else:
            out.append(re.escape(glob[i]))
            i += 1
    return re.compile("".join(out))


class Manifest:
    def __init__(self, path: Path):
        self.sha256 = hashlib.sha256(path.read_bytes()).hexdigest()
        data = tomllib.loads(path.read_text(encoding="utf-8"))
        unknown = set(data) - {"schema", "include", "exclude", "forbidden_prefixes", "generated",
                               "baseline_commit", "new_paths", "removed_paths"}
        if unknown:
            raise ExportError(f"unknown manifest keys: {sorted(unknown)}")
        if data.get("schema") != SCHEMA:
            raise ExportError(f"manifest schema must be {SCHEMA}")
        self.include = [_pattern(p) for p in self._strings(data, "include")]
        self.exclude = [_pattern(p) for p in self._strings(data, "exclude")]
        self.forbidden = self._strings(data, "forbidden_prefixes")
        if not self.include or not self.forbidden:
            raise ExportError("manifest needs `include` and `forbidden_prefixes`")
        for prefix in self.forbidden:
            if not prefix.endswith("/") and "/" in prefix:
                raise ExportError(f"forbidden prefix must be a directory (end with /): {prefix!r}")
        # The tree that is already public. A selected path that is not in it is
        # new to the public and must be named one by one in `new_paths`: a
        # directory include then cannot publish a file nobody looked at.
        self.baseline_commit = data.get("baseline_commit")
        self.new_paths = self._strings(data, "new_paths")
        # The reverse: a public file the export would no longer carry is a
        # feature sunset and needs its own approval, path by path.
        self.removed_paths = self._strings(data, "removed_paths")
        if self.baseline_commit is None:
            if self.new_paths or self.removed_paths:
                raise ExportError("`new_paths` and `removed_paths` need `baseline_commit`")
        elif not isinstance(self.baseline_commit, str) or not COMMIT.fullmatch(self.baseline_commit):
            raise ExportError("`baseline_commit` must be a full 40-hex commit id")
        # Files a release step writes into the export; each one is named and
        # digest-bound, never a wildcard. `required = false` is a staging state
        # (the step has not run yet); a release verification sets it to true.
        self.generated: dict[str, dict] = {}
        for row in data.get("generated", []):
            if (
                not isinstance(row, dict)
                or set(row) != {"path", "sha256", "required"}
                or not isinstance(row["path"], str)
                or not isinstance(row["sha256"], str)
                or not re.fullmatch(r"[0-9a-f]{64}", row["sha256"])
                or not isinstance(row["required"], bool)
            ):
                raise ExportError(f"generated entry needs exactly path, sha256 and required: {row!r}")
            path = row["path"]
            _safe_relative(path)
            if path in self.generated:
                raise ExportError(f"generated path is listed twice: {path!r}")
            if self.forbidden_prefix(path):
                raise ExportError(f"generated path is under a forbidden prefix: {path!r}")
            self.generated[path] = {"sha256": row["sha256"], "required": row["required"]}

    def generated_rows(self) -> list[dict]:
        return [{"path": p, **self.generated[p]} for p in sorted(self.generated)]

    @staticmethod
    def _strings(data: dict, key: str) -> list[str]:
        value = data.get(key, [])
        if not isinstance(value, list) or not all(isinstance(v, str) and v for v in value):
            raise ExportError(f"`{key}` must be a list of non-empty strings")
        if len(set(value)) != len(value):
            raise ExportError(f"`{key}` repeats an entry")
        return value

    def selects(self, path: str) -> bool:
        return any(p.fullmatch(path) for p in self.include) and not any(
            p.fullmatch(path) for p in self.exclude
        )

    def forbidden_prefix(self, path: str) -> str | None:
        for prefix in self.forbidden:
            if path == prefix.rstrip("/") or path.startswith(prefix if prefix.endswith("/") else prefix + "/"):
                return prefix
        return None


def _git(repo: Path, *args: str, data: bytes | None = None) -> bytes:
    done = subprocess.run(["git", "-C", str(repo), *args], input=data, capture_output=True, check=False)
    if done.returncode != 0:
        raise ExportError(f"git {args[0]} failed: {done.stderr.decode(errors='replace').strip()}")
    return done.stdout


def _safe_relative(path: str) -> None:
    parts = PurePosixPath(path).parts
    if not parts or path.startswith("/") or ".." in parts or "\\" in path or "\x00" in path:
        raise ExportError(f"unsafe path: {path!r}")
    if parts[0] == ".git" or path == RECEIPT_NAME:
        raise ExportError(f"reserved path: {path!r}")


def select(repo: Path, commit: str, manifest: Manifest) -> list[dict]:
    """Rows of the selected tree at `commit`; refuses what cannot be exported exactly."""
    if not COMMIT.fullmatch(commit):
        raise ExportError("--source-commit must be a full 40-hex commit id, not a moving ref")
    if _git(repo, "cat-file", "-t", commit).strip() != b"commit":
        raise ExportError(f"{commit} is not a commit")
    rows = []
    for record in _git(repo, "ls-tree", "-r", "-z", "--full-tree", commit).split(b"\x00"):
        if not record:
            continue
        meta, name = record.split(b"\t", 1)
        mode, kind, blob = meta.decode().split()
        path = name.decode("utf-8")
        if not manifest.selects(path):
            continue
        _safe_relative(path)
        hit = manifest.forbidden_prefix(path)
        if hit:
            raise ExportError(f"manifest selects {path!r} under forbidden prefix {hit!r}")
        if kind != "blob" or mode not in REGULAR_MODES:
            # Symlinks and submodules can point outside the selection.
            raise ExportError(f"{path!r} is not a regular file (mode {mode}); list it in `exclude`")
        rows.append({"path": path, "mode": mode, "blob": blob})
    if not rows:
        raise ExportError("manifest selects nothing at this commit")
    selected = {row["path"] for row in rows}
    if manifest.baseline_commit is not None:
        if _git(repo, "cat-file", "-t", manifest.baseline_commit).strip() != b"commit":
            raise ExportError(f"baseline {manifest.baseline_commit} is not a commit")
        published = {
            name.decode("utf-8")
            for name in _git(repo, "ls-tree", "-r", "-z", "--name-only", "--full-tree",
                             manifest.baseline_commit).split(b"\x00")
            if name
        }
        named = set(manifest.new_paths)
        unnamed = sorted(selected - published - named)
        if unnamed:
            raise ExportError(
                f"{len(unnamed)} selected path(s) are not in the public baseline and not named "
                f"in `new_paths`; first: {unnamed[0]!r}"
            )
        stale = sorted(named - (selected - published))
        if stale:
            raise ExportError(f"`new_paths` names a path that is not a new selected file: {stale[0]!r}")
        dropped = {p for p in published if manifest.selects(p)} - selected - set(manifest.generated)
        removed = set(manifest.removed_paths)
        unapproved = sorted(dropped - removed)
        if unapproved:
            raise ExportError(
                f"{len(unapproved)} public path(s) would be dropped without an approved sunset in "
                f"`removed_paths`; first: {unapproved[0]!r}"
            )
        stale = sorted(removed - dropped)
        if stale:
            raise ExportError(f"`removed_paths` names a path the export does not drop: {stale[0]!r}")
    for path in manifest.generated:
        if path in selected:
            raise ExportError(f"generated path {path!r} collides with a source file")
    return sorted(rows, key=lambda r: r["path"])


def _tree_digest(rows: list[dict]) -> str:
    text = "".join(f"{r['mode']} {r['sha256']} {r['path']}\n" for r in rows)
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def _tool_sha256() -> str:
    return hashlib.sha256(Path(__file__).read_bytes()).hexdigest()


def receipt_text(receipt: dict) -> str:
    """The one serialization of a receipt; its SHA-256 is what a release pins."""
    return json.dumps(receipt, indent=1, sort_keys=True) + "\n"


def receipt_sha256(receipt: dict) -> str:
    return hashlib.sha256(receipt_text(receipt).encode("utf-8")).hexdigest()


def _git_blob_id(content: bytes) -> str:
    return hashlib.sha1(b"blob %d\x00" % len(content) + content).hexdigest()  # noqa: S324 - git object id


def export(repo: Path, commit: str, manifest_path: Path, out: Path) -> dict:
    manifest = Manifest(manifest_path)
    rows = select(repo, commit, manifest)
    if out.exists() and any(out.iterdir()):
        raise ExportError(f"output directory is not empty: {out}")
    out.mkdir(parents=True, exist_ok=True)
    for row in rows:
        content = _git(repo, "cat-file", "blob", row["blob"])
        target = out / row["path"]
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(content)
        target.chmod(0o755 if row["mode"] == "100755" else 0o644)
        row["sha256"] = hashlib.sha256(content).hexdigest()
        row["size"] = len(content)
    receipt = {
        "schema": SCHEMA,
        "source_commit": commit,
        "baseline_commit": manifest.baseline_commit,
        "manifest_sha256": manifest.sha256,
        "tool_sha256": _tool_sha256(),
        "tree_sha256": _tree_digest(rows),
        "files": rows,
        "generated": manifest.generated_rows(),
    }
    return receipt


def _walk(root: Path) -> dict[str, Path]:
    found = {}
    for directory, names, files in os.walk(root, followlinks=False):
        here = Path(directory)
        if here == root and ".git" in names:
            # The publication clone has its own history; its object store is not tree content.
            names.remove(".git")
        for name in list(names):
            if (here / name).is_symlink():
                raise ExportError(f"symlinked directory in export: {(here / name).relative_to(root)}")
        for name in files:
            path = here / name
            found[path.relative_to(root).as_posix()] = path
    return found


def verify(
    tree: Path,
    manifest_path: Path,
    receipt: dict,
    *,
    repo: Path | None = None,
    source_commit: str | None = None,
    pinned_receipt_sha256: str | None = None,
    staging: bool = False,
) -> None:
    """Check `tree` against something the caller already trusts.

    A receipt describes itself, so it proves nothing alone: whoever can change
    a file can recompute its rows. The caller must supply the expectation —
    either the source (`repo` + `source_commit`: the selection is derived again
    from that commit and every row must equal it) or the digest of the receipt
    that a release pinned (`pinned_receipt_sha256`).

    `staging` is the state before the release steps ran: a required generated
    file may still be absent. Present, it is checked either way.
    """
    manifest = Manifest(manifest_path)
    if receipt.get("schema") != SCHEMA or not COMMIT.fullmatch(str(receipt.get("source_commit", ""))):
        raise ExportError("receipt schema or source commit is invalid")
    if receipt.get("manifest_sha256") != manifest.sha256:
        raise ExportError("receipt was produced with a different manifest")
    if receipt.get("tool_sha256") != _tool_sha256():
        raise ExportError("receipt was produced by a different version of this tool")
    # The baseline a consumer may rely on is the manifest's, never a receipt field of its own.
    if receipt.get("baseline_commit") != manifest.baseline_commit:
        raise ExportError("receipt names a different public baseline than the manifest")
    if repo is not None and source_commit is not None:
        if receipt["source_commit"] != source_commit:
            raise ExportError("receipt names a different source commit than expected")
        derived = {r["path"]: r for r in select(repo, source_commit, manifest)}
        claimed = receipt.get("files") if isinstance(receipt.get("files"), list) else []
        if [r.get("path") for r in claimed] != sorted(derived):
            raise ExportError("receipt's paths differ from the selection at the expected commit")
        for row in claimed:
            source = derived[row["path"]]
            if row.get("mode") != source["mode"] or row.get("blob") != source["blob"]:
                raise ExportError(f"receipt row differs from the expected commit: {row['path']!r}")
    elif pinned_receipt_sha256 is not None:
        if receipt_sha256(receipt) != pinned_receipt_sha256:
            raise ExportError("receipt differs from the pinned receipt digest")
    else:
        raise ExportError(
            "verification needs an expectation: the source repository and commit, "
            "or the pinned receipt digest"
        )
    from_source = repo is not None and source_commit is not None
    rows = receipt.get("files")
    if not isinstance(rows, list) or not rows:
        raise ExportError("receipt lists no files")
    expected = {}
    for row in rows:
        path = row.get("path", "")
        _safe_relative(path)
        if path in expected or not manifest.selects(path) or manifest.forbidden_prefix(path):
            raise ExportError(f"receipt path is repeated, unselected or forbidden: {path!r}")
        if row.get("mode") not in REGULAR_MODES:
            raise ExportError(f"receipt mode is invalid for {path!r}")
        expected[path] = row
    if receipt.get("tree_sha256") != _tree_digest(rows) or [r["path"] for r in rows] != sorted(expected):
        raise ExportError("receipt tree digest or ordering does not match its rows")
    if receipt.get("generated") != manifest.generated_rows():
        raise ExportError("receipt's generated entries differ from the manifest")
    generated = manifest.generated

    actual = _walk(tree)
    actual.pop(RECEIPT_NAME, None)
    # The whole tree, not selected roots: one unlisted file anywhere fails.
    for path in sorted(actual):
        hit = manifest.forbidden_prefix(path)
        if hit:
            raise ExportError(f"export contains {path!r} under forbidden prefix {hit!r}")
        if path not in expected and path not in generated:
            raise ExportError(f"export contains an unlisted path: {path!r}")
    for path, row in expected.items():
        file = actual.get(path)
        if file is None:
            raise ExportError(f"export is missing {path!r}")
        info = file.lstat()
        if not stat.S_ISREG(info.st_mode):
            raise ExportError(f"{path!r} is not a regular file")
        content = file.read_bytes()
        if hashlib.sha256(content).hexdigest() != row["sha256"]:
            raise ExportError(f"content differs from the receipt: {path!r}")
        if from_source and _git_blob_id(content) != row["blob"]:
            raise ExportError(f"content differs from the expected commit: {path!r}")
        if bool(info.st_mode & 0o100) != (row["mode"] == "100755"):
            raise ExportError(f"executable bit differs from the receipt: {path!r}")
    for path, entry in generated.items():
        file = actual.get(path)
        if file is None:
            if entry["required"] and not staging:
                raise ExportError(f"required generated file is missing: {path!r}")
            continue
        if not stat.S_ISREG(file.lstat().st_mode):
            raise ExportError(f"generated path is not a regular file: {path!r}")
        if hashlib.sha256(file.read_bytes()).hexdigest() != entry["sha256"]:
            raise ExportError(f"generated file differs from the manifest digest: {path!r}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    commands = parser.add_subparsers(dest="command", required=True)
    make = commands.add_parser("export")
    make.add_argument("--repo", type=Path, required=True)
    make.add_argument("--source-commit", required=True)
    make.add_argument("--manifest", type=Path, required=True)
    make.add_argument("--out", type=Path, required=True)
    check = commands.add_parser("verify")
    check.add_argument("--tree", type=Path, required=True)
    check.add_argument("--manifest", type=Path, required=True)
    check.add_argument("--receipt", type=Path)
    check.add_argument("--repo", type=Path, help="with --source-commit: derive the expectation again")
    check.add_argument("--source-commit")
    check.add_argument("--receipt-sha256", help="digest of the receipt a release pinned")
    check.add_argument("--staging", action="store_true",
                       help="before the release steps ran: required generated files may be absent")
    args = parser.parse_args()
    try:
        if args.command == "export":
            receipt = export(args.repo, args.source_commit, args.manifest, args.out)
            (args.out / RECEIPT_NAME).write_text(receipt_text(receipt), encoding="utf-8")
            verify(args.out, args.manifest, receipt, repo=args.repo,
                   source_commit=args.source_commit, staging=True)
            print(
                f"exported {len(receipt['files'])} files, tree {receipt['tree_sha256']}, "
                f"receipt {receipt_sha256(receipt)}"
            )
        else:
            if bool(args.repo) != bool(args.source_commit):
                raise ExportError("--repo and --source-commit belong together")
            receipt_path = args.receipt or args.tree / RECEIPT_NAME
            verify(
                args.tree,
                args.manifest,
                json.loads(receipt_path.read_text(encoding="utf-8")),
                repo=args.repo,
                source_commit=args.source_commit,
                pinned_receipt_sha256=args.receipt_sha256,
                staging=args.staging,
            )
            print("export tree matches the expected source, receipt and manifest")
    except (ExportError, OSError, ValueError, KeyError, tomllib.TOMLDecodeError) as error:
        print(f"public-export: refused: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
