#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Capture and verify a file-level source export candidate.

This tool records immutable source bytes and checks that a candidate target tree
matches them. It checks supplied approval references for exact row binding; it
does not determine legal authority, grant rights, copy source, or approve a
release. Stable origin identifiers are supplied by the plan; remote URLs and
local repository paths are never inferred or written to the manifest.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import stat
import sys
from typing import Any

SCHEMA = "leanctx.source-export-manifest/v1"
APPROVAL_SCHEMA = "leanctx.source-export-approvals/v1"
REPORT_SCHEMA = "leanctx.source-export-validation/v1"
MAX_MANIFEST_BYTES = 16 * 1024 * 1024
MAX_ENTRIES = 20_000
MAX_FILE_BYTES = 8 * 1024 * 1024
MAX_TOTAL_BYTES = 128 * 1024 * 1024
HEX_RE = re.compile(r"(?:[0-9a-f]{40}|[0-9a-f]{64})\Z")
SHA256_RE = re.compile(r"[0-9a-f]{64}\Z")
ID_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,79}\Z")
ORIGIN_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._:-]{0,127}\Z")
REF_ID_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._:/#-]{0,255}\Z")
GROUP_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._/-]{0,127}\Z")


class ExportError(Exception):
    """Expected, sanitized candidate-manifest failure."""


def load_audit_module():
    """Load the existing immutable Git-tree reader; tests may point at a fixture copy."""
    configured = os.environ.get("LEANCTX_IP_PROVENANCE_AUDIT")
    path = Path(configured) if configured else Path(__file__).resolve().with_name("ip-provenance-audit.py")
    if path.is_symlink() or not path.is_file():
        raise ExportError("ip-provenance-audit.py is unavailable beside this tool")
    spec = importlib.util.spec_from_file_location("leanctx_ip_provenance_audit", path)
    if spec is None or spec.loader is None:
        raise ExportError("ip-provenance-audit.py cannot be loaded")
    module = importlib.util.module_from_spec(spec)
    try:
        spec.loader.exec_module(module)
    except Exception as exc:
        raise ExportError("ip-provenance-audit.py is invalid") from exc
    return module


def sha256(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def _object(value: Any, required: set[str], allowed: set[str], label: str) -> dict[str, Any]:
    if not isinstance(value, dict) or not required.issubset(value) or not set(value).issubset(allowed):
        raise ExportError(f"{label} has missing or unknown fields")
    return value


def _unique_json_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ExportError("input JSON contains duplicate object keys")
        result[key] = value
    return result


def _read_json(path: Path, limit: int = MAX_MANIFEST_BYTES) -> dict[str, Any]:
    try:
        info = path.lstat()
        if not stat.S_ISREG(info.st_mode):
            raise ExportError("input JSON is missing or is not a regular file")
        if info.st_size > limit:
            raise ExportError("input JSON exceeds its size limit")
        flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
        descriptor = os.open(path, flags)
        with os.fdopen(descriptor, "rb") as stream:
            opened = os.fstat(stream.fileno())
            if not stat.S_ISREG(opened.st_mode):
                raise ExportError("input JSON is not a regular file")
            if opened.st_size > limit:
                raise ExportError("input JSON exceeds its size limit")
            raw = stream.read(limit + 1)
    except ExportError:
        raise
    except OSError as exc:
        raise ExportError("input JSON cannot be read") from exc
    if len(raw) > limit:
        raise ExportError("input JSON exceeds its size limit")
    try:
        value = json.loads(raw.decode("utf-8"), object_pairs_hook=_unique_json_object)
    except ExportError:
        raise
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ExportError("input JSON is invalid") from exc
    if not isinstance(value, dict):
        raise ExportError("input JSON must be an object")
    return value


def _write_json(path: Path, value: dict[str, Any]) -> None:
    rendered = json.dumps(value, indent=2, sort_keys=True) + "\n"
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
        descriptor = os.open(path, flags, 0o600)
        with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
            stream.write(rendered)
            stream.flush()
    except FileExistsError as exc:
        raise ExportError("output path already exists") from exc
    except OSError as exc:
        raise ExportError("output JSON cannot be written") from exc


def _repo_roots(items: list[str], expected_ids: set[str]) -> dict[str, Path]:
    roots: dict[str, Path] = {}
    for item in items:
        repo_id, separator, raw_path = item.partition("=")
        if not separator or not ID_RE.fullmatch(repo_id) or not raw_path or repo_id in roots:
            raise ExportError("repository mapping is invalid or duplicated")
        path = Path(raw_path).resolve()
        if not path.is_dir():
            raise ExportError("repository root is not a directory")
        roots[repo_id] = path
    if set(roots) != expected_ids:
        raise ExportError("repository mappings do not match the manifest")
    return roots


def _check_root(audit: Any, root: Path) -> None:
    try:
        top = audit.decode_git(audit.git(root, "rev-parse", "--show-toplevel")).strip()
    except Exception as exc:
        raise ExportError("repository root is not a readable Git worktree") from exc
    if Path(top).resolve() != root.resolve():
        raise ExportError("repository root must be the Git worktree root")


def _read_tree_file(audit: Any, tree: Any, path: str) -> bytes:
    if not audit.safe_relpath(path):
        raise ExportError("repository path is unsafe")
    try:
        return tree.read_bytes(path, MAX_FILE_BYTES)
    except Exception as exc:
        raise ExportError(f"tracked regular file cannot be read: {path}") from exc


def _safe_origin(value: Any) -> str:
    if not isinstance(value, str) or not ORIGIN_RE.fullmatch(value):
        raise ExportError("origin must be a supplied stable identifier without URL credentials")
    return value


def _safe_group(value: Any) -> str:
    if (
        not isinstance(value, str)
        or not GROUP_RE.fullmatch(value)
        or not all(part not in {"", ".", ".."} for part in value.split("/"))
    ):
        raise ExportError("source group is invalid")
    return value


def _under_managed_root(path: str, roots: list[str]) -> bool:
    return any(path == root or path.startswith(root + "/") for root in roots)


def _capture(plan: dict[str, Any], repo_items: list[str]) -> dict[str, Any]:
    audit = load_audit_module()
    plan = _object(
        plan,
        {"schema", "sources", "files", "managed_target_roots", "target_origin"},
        {"schema", "sources", "files", "managed_target_roots", "target_origin"},
        "plan",
    )
    if plan.get("schema") != "leanctx.source-export-plan/v1":
        raise ExportError("unsupported source export plan schema")
    target_origin = _safe_origin(plan["target_origin"])
    source_specs = plan.get("sources")
    file_specs = plan.get("files")
    roots_spec = plan.get("managed_target_roots")
    if not isinstance(source_specs, list) or not source_specs or len(source_specs) > MAX_ENTRIES:
        raise ExportError("plan sources are missing or exceed their limit")
    if not isinstance(file_specs, list) or not file_specs or len(file_specs) > MAX_ENTRIES:
        raise ExportError("plan files are missing or exceed their limit")
    if not isinstance(roots_spec, list) or not roots_spec:
        raise ExportError("managed target roots are required")
    managed_roots: list[str] = []
    for item in roots_spec:
        if not isinstance(item, str) or not audit.safe_relpath(item):
            raise ExportError("managed target root is unsafe")
        if item in managed_roots:
            raise ExportError("managed target roots are duplicated")
        managed_roots.append(item)
    managed_roots.sort()

    source_ids: set[str] = set()
    source_rows: list[dict[str, str]] = []
    source_trees: dict[str, Any] = {}
    input_roots = _repo_roots(repo_items, {
        row.get("id") for row in source_specs if isinstance(row, dict) and isinstance(row.get("id"), str)
    })
    for item in source_specs:
        row = _object(item, {"id", "origin", "ref"}, {"id", "origin", "ref"}, "source")
        source_id, origin, ref = row["id"], _safe_origin(row["origin"]), row["ref"]
        if not isinstance(source_id, str) or not ID_RE.fullmatch(source_id) or source_id in source_ids:
            raise ExportError("source identifier is invalid or duplicated")
        if not isinstance(ref, str) or not audit.safe_ref(ref):
            raise ExportError(f"source ref is invalid: {source_id}")
        root = input_roots[source_id]
        _check_root(audit, root)
        try:
            commit = audit.resolve_commit(root, ref)
        except Exception as exc:
            raise ExportError(f"source ref cannot be resolved: {source_id}") from exc
        if not HEX_RE.fullmatch(commit):
            raise ExportError(f"source ref did not resolve to a full commit: {source_id}")
        source_ids.add(source_id)
        source_trees[source_id] = audit.AuditTree(root, commit)
        source_rows.append({"id": source_id, "origin": origin, "ref": ref, "commit": commit})
    if set(input_roots) != source_ids:
        raise ExportError("source identifiers are duplicated or inconsistent")

    entries: list[dict[str, Any]] = []
    source_paths: set[tuple[str, str]] = set()
    targets: set[str] = set()
    folded_targets: set[str] = set()
    total_bytes = 0
    for item in file_specs:
        row = _object(
            item,
            {"source_id", "source_path", "target_path", "source_group", "spdx"},
            {"source_id", "source_path", "target_path", "source_group", "spdx",
             "license_evidence_path", "approval_ref"},
            "file entry",
        )
        source_id, source_path, target_path = row["source_id"], row["source_path"], row["target_path"]
        group = _safe_group(row["source_group"])
        spdx = row["spdx"]
        if not isinstance(source_id, str) or source_id not in source_trees:
            raise ExportError("file entry names an unknown source repository")
        if not isinstance(source_path, str) or not audit.safe_relpath(source_path):
            raise ExportError("source path is unsafe")
        if not isinstance(target_path, str) or not audit.safe_relpath(target_path):
            raise ExportError("target path is unsafe")
        if not _under_managed_root(target_path, managed_roots):
            raise ExportError("target path is outside every managed target root")
        if not isinstance(spdx, str) or len(spdx) > 256 or not audit.parse_spdx(spdx):
            raise ExportError("SPDX expression is invalid")
        identity = (source_id, source_path)
        folded_target = target_path.casefold()
        if identity in source_paths:
            raise ExportError(f"source file is mapped more than once: {source_path}")
        if target_path in targets or folded_target in folded_targets:
            raise ExportError(f"target path collision: {target_path}")
        source_paths.add(identity)
        targets.add(target_path)
        folded_targets.add(folded_target)

        raw = _read_tree_file(audit, source_trees[source_id], source_path)
        total_bytes += len(raw)
        if total_bytes > MAX_TOTAL_BYTES:
            raise ExportError("source content exceeds total size limit")
        header, header_values = audit.spdx_header(raw)
        if len(set(header_values)) > 1:
            raise ExportError(f"conflicting SPDX headers: {source_path}")
        evidence_path: str
        evidence_bytes: bytes
        if header is not None:
            if header != spdx:
                raise ExportError(f"declared SPDX does not match source header: {source_path}")
            evidence_path, evidence_bytes = source_path, raw
            basis = "spdx-header"
        else:
            license_path = row.get("license_evidence_path")
            if (
                not isinstance(license_path, str)
                or not audit.safe_relpath(license_path)
                or license_path == source_path
            ):
                raise ExportError(f"missing license evidence for headerless source: {source_path}")
            evidence_path = license_path
            evidence_bytes = _read_tree_file(audit, source_trees[source_id], evidence_path)
            basis = "referenced-source-file"
        approval_ref = row.get("approval_ref")
        if approval_ref is not None and (
            not isinstance(approval_ref, str) or not REF_ID_RE.fullmatch(approval_ref)
        ):
            raise ExportError(f"approval reference is invalid: {source_path}")
        entries.append({
            "source_id": source_id,
            "source_path": source_path,
            "source_sha256": sha256(raw),
            "spdx": spdx,
            "spdx_basis": basis,
            "spdx_claim": "observed_header" if header is not None else "plan_declared",
            "spdx_evidence_path": evidence_path,
            "spdx_evidence_sha256": sha256(evidence_bytes),
            "source_group": group,
            "target_path": target_path,
            "approval_ref": approval_ref,
        })

    entries.sort(key=lambda row: (row["source_id"], row["source_path"], row["target_path"]))
    source_rows.sort(key=lambda row: row["id"])
    return {
        "schema": SCHEMA,
        "state": "candidate",
        "target_origin": target_origin,
        "managed_target_roots": managed_roots,
        "sources": source_rows,
        "files": entries,
    }


def _verify_manifest_shape(manifest: dict[str, Any], audit: Any) -> None:
    _object(manifest, {"schema", "state", "target_origin", "managed_target_roots", "sources", "files"},
            {"schema", "state", "target_origin", "managed_target_roots", "sources", "files"}, "manifest")
    if manifest.get("schema") != SCHEMA or manifest.get("state") != "candidate":
        raise ExportError("unsupported or non-candidate source manifest")
    _safe_origin(manifest["target_origin"])
    sources, files, roots = manifest["sources"], manifest["files"], manifest["managed_target_roots"]
    if not isinstance(sources, list) or not sources or len(sources) > MAX_ENTRIES:
        raise ExportError("manifest sources are missing or exceed their limit")
    if not isinstance(files, list) or not files or len(files) > MAX_ENTRIES:
        raise ExportError("manifest files are missing or exceed their limit")
    if (
        not isinstance(roots, list)
        or not roots
        or any(not isinstance(r, str) or not audit.safe_relpath(r) for r in roots)
        or len(set(roots)) != len(roots)
    ):
        raise ExportError("manifest managed roots are invalid")
    _object_sources: set[str] = set()
    for source in sources:
        row = _object(source, {"id", "origin", "ref", "commit"}, {"id", "origin", "ref", "commit"}, "manifest source")
        if not isinstance(row["id"], str) or not ID_RE.fullmatch(row["id"]) or row["id"] in _object_sources:
            raise ExportError("manifest source identifier is invalid or duplicated")
        if not _safe_origin(row["origin"]):
            raise ExportError("manifest source origin is invalid")
        if (
            not isinstance(row["ref"], str)
            or not audit.safe_ref(row["ref"])
            or not isinstance(row["commit"], str)
            or not HEX_RE.fullmatch(row["commit"])
        ):
            raise ExportError("manifest source ref/commit is invalid")
        _object_sources.add(row["id"])
    seen_files: set[tuple[str, str]] = set()
    seen_targets: set[str] = set()
    folded_targets: set[str] = set()
    for entry in files:
        row = _object(
            entry,
            {"source_id", "source_path", "source_sha256", "spdx", "spdx_basis", "spdx_claim",
             "spdx_evidence_path", "spdx_evidence_sha256", "source_group", "target_path", "approval_ref"},
            {"source_id", "source_path", "source_sha256", "spdx", "spdx_basis", "spdx_claim",
             "spdx_evidence_path", "spdx_evidence_sha256", "source_group", "target_path", "approval_ref"},
            "manifest file",
        )
        if not isinstance(row["source_id"], str) or row["source_id"] not in _object_sources:
            raise ExportError("manifest file names an unknown source")
        if not isinstance(row["source_path"], str) or not audit.safe_relpath(row["source_path"]):
            raise ExportError("manifest source path is invalid")
        if not SHA256_RE.fullmatch(str(row["source_sha256"])):
            raise ExportError("manifest source digest is invalid")
        if (
            not isinstance(row["spdx"], str)
            or not audit.parse_spdx(row["spdx"])
            or not isinstance(row["spdx_basis"], str)
        ):
            raise ExportError("manifest license metadata is invalid")
        if row["spdx_basis"] not in {"spdx-header", "referenced-source-file"}:
            raise ExportError("manifest SPDX basis is unsupported")
        if (
            not isinstance(row["spdx_claim"], str)
            or row["spdx_claim"] not in {"observed_header", "plan_declared"}
            or (row["spdx_basis"] == "spdx-header") != (row["spdx_claim"] == "observed_header")
        ):
            raise ExportError("manifest SPDX basis is unsupported")
        if (
            not isinstance(row["spdx_evidence_path"], str)
            or not audit.safe_relpath(row["spdx_evidence_path"])
            or not isinstance(row["spdx_evidence_sha256"], str)
            or not SHA256_RE.fullmatch(row["spdx_evidence_sha256"])
        ):
            raise ExportError("manifest SPDX evidence is invalid")
        if (
            not isinstance(row["source_group"], str)
            or _safe_group(row["source_group"]) != row["source_group"]
            or not isinstance(row["target_path"], str)
            or not audit.safe_relpath(row["target_path"])
            or not _under_managed_root(row["target_path"], roots)
        ):
            raise ExportError("manifest mapping is invalid")
        key = (row["source_id"], row["source_path"])
        target = row["target_path"]
        if key in seen_files or target in seen_targets or target.casefold() in folded_targets:
            raise ExportError("manifest contains duplicate source or target paths")
        seen_files.add(key)
        seen_targets.add(target)
        folded_targets.add(target.casefold())
        if row["approval_ref"] is not None and (
            not isinstance(row["approval_ref"], str) or not REF_ID_RE.fullmatch(row["approval_ref"])
        ):
            raise ExportError("manifest approval reference is invalid")


def _load_approvals(path: Path) -> dict[str, dict[str, Any]]:
    document = _read_json(path)
    _object(document, {"schema", "approvals"}, {"schema", "approvals"}, "approval ledger")
    if document["schema"] != APPROVAL_SCHEMA or not isinstance(document["approvals"], list):
        raise ExportError("approval ledger schema is invalid")
    if len(document["approvals"]) > MAX_ENTRIES:
        raise ExportError("approval ledger exceeds its entry limit")
    rows: dict[str, dict[str, Any]] = {}
    fields = {
        "ref", "status", "source_id", "source_origin", "source_commit", "target_origin",
        "source_path", "source_sha256", "spdx", "spdx_basis", "spdx_claim",
        "spdx_evidence_path", "spdx_evidence_sha256", "source_group", "target_path",
    }
    for item in document["approvals"]:
        row = _object(item, fields, fields, "approval record")
        ref = row["ref"]
        if not isinstance(ref, str) or not REF_ID_RE.fullmatch(ref) or ref in rows:
            raise ExportError("approval references are invalid or duplicated")
        if not isinstance(row["status"], str) or row["status"] not in {"approved", "pending", "rejected"}:
            raise ExportError("approval status is invalid")
        rows[ref] = row
    return rows


def _check_approval(
    entry: dict[str, Any], source: dict[str, Any], target_origin: str,
    approvals: dict[str, dict[str, Any]],
) -> str | None:
    ref = entry["approval_ref"]
    if ref is None:
        return "file has no approval reference"
    approval = approvals.get(ref)
    if approval is None:
        return "approval reference is missing from the supplied ledger"
    expected = {
        "source_id": entry["source_id"],
        "source_origin": source["origin"],
        "source_commit": source["commit"],
        "target_origin": target_origin,
        "source_path": entry["source_path"],
        "source_sha256": entry["source_sha256"],
        "spdx": entry["spdx"],
        "spdx_basis": entry["spdx_basis"],
        "spdx_claim": entry["spdx_claim"],
        "spdx_evidence_path": entry["spdx_evidence_path"],
        "spdx_evidence_sha256": entry["spdx_evidence_sha256"],
        "source_group": entry["source_group"],
        "target_path": entry["target_path"],
    }
    if any(approval.get(key) != value for key, value in expected.items()):
        return "approval record does not bind the exact source and target row"
    if approval["status"] != "approved":
        return "approval record is not approved"
    return None


def verify_manifest(
    manifest: dict[str, Any],
    approval_path: Path,
    source_items: list[str],
    target_root: Path,
    target_origin: str,
    target_ref: str,
) -> dict[str, Any]:
    audit = load_audit_module()
    _verify_manifest_shape(manifest, audit)
    target_origin = _safe_origin(target_origin)
    if manifest["target_origin"] != target_origin:
        raise ExportError("target origin does not match the candidate manifest")
    source_rows = {row["id"]: row for row in manifest["sources"]}
    roots = _repo_roots(source_items, set(source_rows))
    trees: dict[str, Any] = {}
    for source_id, source in source_rows.items():
        root = roots[source_id]
        _check_root(audit, root)
        try:
            resolved = audit.resolve_commit(root, source["ref"])
        except Exception as exc:
            raise ExportError(f"source ref cannot be resolved: {source_id}") from exc
        if resolved != source["commit"]:
            raise ExportError(f"source ref no longer resolves to the recorded commit: {source_id}")
        trees[source_id] = audit.AuditTree(root, source["commit"])

    try:
        approvals = _load_approvals(approval_path)
    except ExportError:
        raise
    referenced_approvals = {row["approval_ref"] for row in manifest["files"] if row["approval_ref"] is not None}
    unused = set(approvals) - referenced_approvals
    if unused:
        raise ExportError("approval ledger contains unreferenced records")

    checked_sources: set[tuple[str, str]] = set()
    total_bytes = 0
    for entry in manifest["files"]:
        source = source_rows[entry["source_id"]]
        raw = _read_tree_file(audit, trees[entry["source_id"]], entry["source_path"])
        total_bytes += len(raw)
        if total_bytes > MAX_TOTAL_BYTES:
            raise ExportError("source content exceeds total size limit")
        if sha256(raw) != entry["source_sha256"]:
            raise ExportError(f"source digest mismatch: {entry['source_path']}")
        evidence = _read_tree_file(audit, trees[entry["source_id"]], entry["spdx_evidence_path"])
        if sha256(evidence) != entry["spdx_evidence_sha256"]:
            raise ExportError(f"SPDX evidence digest mismatch: {entry['source_path']}")
        header, header_values = audit.spdx_header(raw)
        if len(set(header_values)) > 1:
            raise ExportError(f"conflicting SPDX headers: {entry['source_path']}")
        if entry["spdx_basis"] == "spdx-header":
            if header != entry["spdx"] or entry["spdx_evidence_path"] != entry["source_path"]:
                raise ExportError(f"SPDX header mismatch: {entry['source_path']}")
        elif header is not None or entry["spdx_evidence_path"] == entry["source_path"]:
            raise ExportError(f"headerless source lacks separate license evidence: {entry['source_path']}")
        approval_error = _check_approval(entry, source, target_origin, approvals)
        if approval_error:
            raise ExportError(f"{entry['source_path']}: {approval_error}")
        checked_sources.add((entry["source_id"], entry["source_path"]))

    _check_root(audit, target_root.resolve())
    if not audit.safe_ref(target_ref):
        raise ExportError("target ref is invalid")
    try:
        target_commit = audit.resolve_commit(target_root.resolve(), target_ref)
    except Exception as exc:
        raise ExportError("target ref cannot be resolved") from exc
    target_tree = audit.AuditTree(target_root.resolve(), target_commit)
    target_paths = target_tree.paths(MAX_ENTRIES)
    mapped_targets = {entry["target_path"] for entry in manifest["files"]}
    for entry in manifest["files"]:
        raw = _read_tree_file(audit, target_tree, entry["target_path"])
        if sha256(raw) != entry["source_sha256"]:
            raise ExportError(f"target digest does not match source: {entry['target_path']}")
    for managed_root in manifest["managed_target_roots"]:
        if not audit.safe_relpath(managed_root):
            raise ExportError("managed target root is unsafe")
        actual = {path for path in target_paths if path == managed_root or path.startswith(managed_root + "/")}
        expected = {path for path in mapped_targets if path == managed_root or path.startswith(managed_root + "/")}
        if actual != expected:
            raise ExportError(f"managed target root is not exactly covered by manifest: {managed_root}")

    report = {
        "schema": REPORT_SCHEMA,
        "result": "PASS",
        "manifest_sha256": sha256(json.dumps(manifest, indent=2, sort_keys=True).encode("utf-8") + b"\n"),
        "target": {"origin": target_origin, "ref": target_ref, "commit": target_commit},
        "source_files_verified": len(checked_sources),
        "approval_records_verified": len(referenced_approvals),
        "note": "Approval references were matched to supplied records; this tool does not determine legal authority or grant rights.",
    }
    return report


def _inventory(args: argparse.Namespace) -> int:
    plan = _read_json(args.plan)
    manifest = _capture(plan, args.source)
    _write_json(args.output, manifest)
    rendered = json.dumps(manifest, indent=2, sort_keys=True).encode("utf-8") + b"\n"
    print(f"source-export-manifest: candidate inventory {sha256(rendered)}")
    return 0


def _verify(args: argparse.Namespace) -> int:
    manifest = _read_json(args.manifest)
    report = verify_manifest(
        manifest,
        args.approvals,
        args.source,
        args.target_root.resolve(),
        args.target_origin,
        args.target_ref,
    )
    rendered = json.dumps(report, indent=2, sort_keys=True) + "\n"
    if args.report:
        _write_json(args.report, report)
    print(rendered, end="")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    inventory = subparsers.add_parser("inventory", help="capture a candidate from immutable source refs; does not approve")
    inventory.add_argument("--plan", type=Path, required=True)
    inventory.add_argument("--source", action="append", default=[], metavar="ID=GIT_ROOT")
    inventory.add_argument("--output", type=Path, required=True)
    verify = subparsers.add_parser("verify", help="validate exact source approvals and candidate target commit")
    verify.add_argument("--manifest", type=Path, required=True)
    verify.add_argument("--approvals", type=Path, required=True)
    verify.add_argument("--source", action="append", default=[], metavar="ID=GIT_ROOT")
    verify.add_argument("--target-root", type=Path, required=True)
    verify.add_argument("--target-origin", required=True, help="stable non-secret identifier, not a remote URL")
    verify.add_argument("--target-ref", required=True)
    verify.add_argument("--report", type=Path)
    args = parser.parse_args(argv)
    try:
        return _inventory(args) if args.command == "inventory" else _verify(args)
    except ExportError as exc:
        print(f"source-export-manifest: {exc}", file=sys.stderr)
        return 1
    except Exception:
        print("source-export-manifest: unexpected validation failure", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
