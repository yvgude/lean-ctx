#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Deterministic license, provenance, and release-artifact audit.

The checker is deliberately self-contained: it reads a local git tree, the
versioned matrix, and bounded local files only.  It never resolves a URL or
uses a network-capable dependency.
"""

from __future__ import annotations

import argparse
from datetime import date
import fnmatch
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path, PurePosixPath
from typing import Any

try:
    import tomllib  # Python 3.11+
except ModuleNotFoundError:  # Python 3.9/3.10: keep the checker stdlib-only.
    tomllib = None


TOOL_SCHEMA = "leanctx.ip-provenance-audit/v1"
CLA_V1_SIGNATURE_REF = "f09ea6d8a28066007678342056ac8be95df756d7"
CLA_V1_SIGNATURE_PATH = "signatures/v1/cla.json"
CLA_V1_SIGNATURE_SHA256 = "831846e0db4c8d1d097eb8988e23685bb5990030641c8b6c9f5cae83ced6a56e"
CLA_V1_SIGNATURE_COUNT = 19
MAX_MATRIX_BYTES = 512 * 1024
MAX_GIT_OUTPUT_BYTES = 8 * 1024 * 1024
MAX_TRACKED_FILES = 20_000
MAX_FILE_BYTES = 8 * 1024 * 1024
MAX_TOTAL_BYTES = 128 * 1024 * 1024
GIT_TIMEOUT_SECONDS = 15

SOURCE_SUFFIXES = {
    ".c",
    ".cc",
    ".cpp",
    ".css",
    ".go",
    ".h",
    ".hpp",
    ".html",
    ".java",
    ".js",
    ".jsx",
    ".kt",
    ".kts",
    ".lean",
    ".lua",
    ".mjs",
    ".php",
    ".proto",
    ".ps1",
    ".py",
    ".rb",
    ".rs",
    ".sh",
    ".swift",
    ".ts",
    ".tsx",
    ".yml",
    ".yaml",
}

KNOWN_SPDX_IDS = {
    "0BSD",
    "Apache-2.0",
    "BSD-1-Clause",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "CC0-1.0",
    "ISC",
    "MIT",
    "MIT-0",
    "NOASSERTION",
    "Unicode-3.0",
    "Unlicense",
    "Zlib",
}
SPDX_ID_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9.+-]*\Z")
REF_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._/@-]*\Z")
SHA256_RE = re.compile(r"[0-9a-f]{64}\Z")
SPDX_HEADER_RE = re.compile(r"SPDX-License-Identifier:\s*(.*?)(?:\s*(?:\*/|-->)\s*)?\Z")
LEGAL_MARKER_RE = re.compile(
    r"\b(?:NON-OPERATIVE|LEGAL\s+REVIEW\s+REQUIRED|PENDING\s+COUNSEL|"
    r"PENDING\s+LEGAL\s+APPROVAL|LEGAL\s+DRAFT)\b",
    re.IGNORECASE,
)

REQUIRED_CLASSES = {
    "public_trust_core",
    "sdk_protocol",
    "private_service",
    "commercial",
    "free_runtime",
    "generated",
    "third_party",
    "docs_assets",
    "tests_examples",
    "historical",
}
RULE_KEYS = {
    "order",
    "name",
    "class",
    "license",
    "include",
    "exclude",
    "root_only",
    "forbidden_public",
    "provenance_required",
    "generated",
    "vendor",
    "fixture",
    "artifact_kind",
    "owner",
    "provenance",
    "notice",
}
LEGAL_DRAFT_PATHS = (
    "LICENSES/LeanCTX-Commercial-Source-License-2.0.txt",
    "TRADEMARKS.md",
    "LEGAL_REVIEW_REQUIRED.md",
    "legal/cla/CLA-v2.md",
)
PRESERVED_PATHS = ("LICENSE", "NOTICE", "CLA.md")
DECISION_FIELDS = (
    "commercial_license_status:",
    "cla_v2_status:",
    "trademarks_status:",
    "approving_counsel:",
    "approval_evidence:",
    "effective_date:",
)
NOTICE_APPROVAL_PATH = "legal/third-party-notices-approval.toml"
NOTICE_DIGEST_RE = re.compile(r"^Content-Digest:\s*sha256:([0-9a-f]{64})$", re.MULTILINE)
NOTICE_APPROVAL_SCHEMA = "leanctx.third-party-notices-approval/v1"
LEGAL_APPROVAL_PATH = "legal/release-approval.toml"
LEGAL_APPROVAL_SCHEMA = "leanctx.release-legal-approval/v1"
APACHE_HOST_PRIVATE_CLASSES = {"commercial", "free_runtime", "private_service"}
APACHE_HOST_PRIVATE_ARTIFACTS = (*LEGAL_DRAFT_PATHS,
    "LICENSES/LicenseRef-LeanCTX-Commercial-Source-2.0.txt",
    "LICENSES/Thinkery-Free-Runtime-License-1.0-DRAFT.txt",
    "LICENSES-LeanCTX-Commercial-Source-License-2.0.txt", "ee/LICENSE", LEGAL_APPROVAL_PATH,
)
APACHE_HOST_REQUIRED_ARTIFACTS = {
    *PRESERVED_PATHS, "LICENSE.md", "LICENSE_MATRIX.toml", "LICENSES/Apache-2.0.txt",
    "THIRD_PARTY_NOTICES", "legal/cla/CLA-v1-legacy.md", "legal/third-party-assets.toml",
}


class AuditError(Exception):
    """Expected, sanitized audit failure."""


class MatrixTomlError(ValueError):
    """Strict fallback TOML parser error for Python versions without tomllib."""


def _toml_value(value: str) -> Any:
    if value in {"true", "false"}:
        return value == "true"
    if re.fullmatch(r"-?[0-9]+", value):
        return int(value)
    if value.startswith('"') and value.endswith('"'):
        try:
            parsed = json.loads(value)
        except json.JSONDecodeError as exc:
            raise MatrixTomlError("invalid string") from exc
        if not isinstance(parsed, str):
            raise MatrixTomlError("invalid string")
        return parsed
    if value.startswith("[") and value.endswith("]"):
        value = re.sub(r",\s*]", "]", value)
        try:
            parsed = json.loads(value)
        except json.JSONDecodeError as exc:
            raise MatrixTomlError("invalid array") from exc
        if not isinstance(parsed, list):
            raise MatrixTomlError("invalid array")
        return parsed
    raise MatrixTomlError("unsupported TOML value")


def _fallback_toml(text: str) -> dict[str, Any]:
    """Parse the deliberately narrow TOML subset used by LICENSE_MATRIX."""

    result: dict[str, Any] = {}
    current: dict[str, Any] = result
    pending = ""
    for raw_line in text.splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        pending = f"{pending} {line}".strip()
        if pending.count("[") != pending.count("]"):
            continue
        line = pending
        pending = ""
        if line.startswith("[[") and line.endswith("]]"):
            dotted = line[2:-2].strip()
            if not dotted or not re.fullmatch(r"[A-Za-z0-9_.-]+", dotted):
                raise MatrixTomlError("invalid array table")
            parts = dotted.split(".")
            parent: dict[str, Any] = result
            for part in parts[:-1]:
                child = parent.get(part)
                if not isinstance(child, dict):
                    raise MatrixTomlError("invalid table nesting")
                parent = child
            key = parts[-1]
            values = parent.setdefault(key, [])
            if not isinstance(values, list):
                raise MatrixTomlError("duplicate table")
            current = {}
            values.append(current)
            continue
        if line.startswith("[") and line.endswith("]"):
            dotted = line[1:-1].strip()
            if not dotted or not re.fullmatch(r"[A-Za-z0-9_.-]+", dotted):
                raise MatrixTomlError("invalid table")
            current = result
            for part in dotted.split("."):
                child = current.setdefault(part, {})
                if not isinstance(child, dict):
                    raise MatrixTomlError("invalid table nesting")
                current = child
            continue
        if "=" not in line:
            raise MatrixTomlError("missing assignment")
        key, value = (part.strip() for part in line.split("=", 1))
        if not re.fullmatch(r"[A-Za-z0-9_-]+", key) or key in current:
            raise MatrixTomlError("invalid or duplicate key")
        current[key] = _toml_value(value)
    if pending:
        raise MatrixTomlError("unterminated TOML value")
    return result


def load_toml(raw: bytes) -> dict[str, Any]:
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise MatrixTomlError("matrix is not UTF-8") from exc
    if tomllib is not None:
        return tomllib.loads(text)
    return _fallback_toml(text)


def safe_relpath(value: str) -> bool:
    """Return whether value is a safe repository-relative POSIX path."""

    if (
        not value
        or value.startswith("/")
        or "\\" in value
        or any(ord(char) < 0x20 or ord(char) == 0x7F for char in value)
    ):
        return False
    parts = value.split("/")
    return all(part not in {"", ".", ".."} for part in parts)


def safe_ref(value: str) -> bool:
    return bool(value) and len(value) <= 200 and bool(REF_RE.fullmatch(value)) and ".." not in value


def read_bounded(path: Path, limit: int = MAX_FILE_BYTES) -> bytes:
    try:
        size = path.stat().st_size
    except OSError as exc:
        raise AuditError("file cannot be inspected") from exc
    if size > limit:
        raise AuditError("file exceeds configured size limit")
    try:
        raw = path.read_bytes()
    except OSError as exc:
        raise AuditError("file cannot be read") from exc
    if len(raw) > limit:
        raise AuditError("file exceeds configured size limit")
    return raw


def git(root: Path, *args: str) -> bytes:
    try:
        result = subprocess.run(
            ["git", "-C", str(root), *args],
            check=False,
            capture_output=True,
            timeout=GIT_TIMEOUT_SECONDS,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise AuditError("git command unavailable or timed out") from exc
    if len(result.stdout) > MAX_GIT_OUTPUT_BYTES:
        raise AuditError("git output exceeds configured size limit")
    if result.returncode != 0:
        raise AuditError("git command failed")
    return result.stdout


def decode_git(raw: bytes) -> str:
    try:
        return raw.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise AuditError("git path is not valid UTF-8") from exc


def git_blob(root: Path, ref: str, path: str) -> bytes:
    if not safe_ref(ref) or not safe_relpath(path):
        raise AuditError("unsafe git object reference")
    return git(root, "show", f"{ref}:{path}")


EXPORT_MANIFEST_PATH = "legal/provenance-manifest.json"
EXPORT_MANIFEST_SCHEMA = "leanctx.provenance-export-manifest/v1"


class SignatureSummary:
    """What the audit checks about the CLA-v1 signature inventory."""

    def __init__(self, sha256: str, valid_json: bool, count: int, unique: bool, invalid_signer: bool):
        self.sha256 = sha256
        self.valid_json = valid_json
        self.count = count
        self.unique = unique
        self.invalid_signer = invalid_signer

    def to_dict(self) -> dict[str, Any]:
        return {
            "sha256": self.sha256,
            "valid_json": self.valid_json,
            "signer_count": self.count,
            "signers_unique": self.unique,
            "invalid_signer": self.invalid_signer,
        }


def summarize_signatures(raw: bytes) -> SignatureSummary:
    digest = hashlib.sha256(raw).hexdigest()
    try:
        payload = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        return SignatureSummary(digest, False, 0, True, False)
    signers = payload.get("signedContributors") if isinstance(payload, dict) else None
    if not isinstance(signers, list):
        return SignatureSummary(digest, False, 0, True, False)
    names, invalid = [], False
    for signer in signers:
        if not isinstance(signer, dict) or not isinstance(signer.get("name"), str) or not signer["name"].strip():
            invalid = True
            continue
        names.append(signer["name"].casefold())
    return SignatureSummary(digest, True, len(names), len(names) == len(set(names)), invalid)


class GitHistory:
    """Historical provenance read from the repository's own git history."""

    def __init__(self, root: Path):
        self.root = root

    def paths(self, ref: str) -> set[str]:
        return set(enumerate_paths(self.root, ref, MAX_TRACKED_FILES))

    def digest(self, ref: str, path: str) -> str:
        return hashlib.sha256(git_blob(self.root, ref, path)).hexdigest()

    def signatures(self, ref: str, path: str) -> SignatureSummary:
        return summarize_signatures(git_blob(self.root, ref, path))


class ManifestHistory:
    """Historical provenance for a history-free export.

    The manifest is derived from history by ``--write-export-manifest`` and
    re-derived by every non-export audit wherever the history exists; in
    the export it stands in for history that is not there. A lookup the
    manifest does not cover fails exactly like a missing git object.
    """

    def __init__(self, manifest: dict[str, Any]):
        self.manifest = manifest

    def _require(self, ref: str) -> None:
        if ref not in (self.manifest.get("cut"), self.manifest.get("cla_v1_source_ref"),
                       self.manifest.get("cla_v1_signature_ref")):
            raise AuditError("provenance manifest does not cover this reference")

    def paths(self, ref: str) -> set[str]:
        if ref != self.manifest.get("cut"):
            raise AuditError("provenance manifest does not cover this reference")
        paths = self.manifest.get("cut_paths")
        if not isinstance(paths, list) or not all(isinstance(p, str) and safe_relpath(p) for p in paths):
            raise AuditError("provenance manifest cut paths are invalid")
        listed = "\n".join(sorted(set(paths))).encode("utf-8")
        if hashlib.sha256(listed).hexdigest() != self.manifest.get("cut_paths_sha256"):
            raise AuditError("provenance manifest cut paths do not match their digest")
        return set(paths)

    def digest(self, ref: str, path: str) -> str:
        self._require(ref)
        value = self.manifest.get("blobs", {}).get(f"{ref}:{path}")
        if not isinstance(value, str) or not SHA256_RE.fullmatch(value):
            raise AuditError("provenance manifest does not cover this object")
        return value

    def signatures(self, ref: str, path: str) -> SignatureSummary:
        self._require(ref)
        value = self.manifest.get("cla_v1_signatures")
        if not isinstance(value, dict) or value.get("ref") != ref or value.get("path") != path:
            raise AuditError("provenance manifest does not cover the signature inventory")
        try:
            return SignatureSummary(
                str(value["sha256"]), bool(value["valid_json"]), int(value["signer_count"]),
                bool(value["signers_unique"]), bool(value["invalid_signer"]),
            )
        except (KeyError, TypeError, ValueError) as exc:
            raise AuditError("provenance manifest signature summary is invalid") from exc


def build_export_manifest(root: Path, matrix: dict[str, Any]) -> dict[str, Any]:
    """Derive the provenance manifest from history. Deterministic: no commit
    ids beyond the fixed historical references, no timestamps."""
    history = GitHistory(root)
    cut = str(matrix.get("pre_v4_apache_cut", ""))
    if not safe_ref(cut):
        raise AuditError("matrix has no pre-v4 Apache cut")
    cut_paths = sorted(history.paths(cut))
    # PRESERVED_PATHS includes LICENSE, which also pins the Apache text.
    blobs = {f"{cut}:{path}": history.digest(cut, path) for path in PRESERVED_PATHS}
    provenance = matrix.get("provenance", {})
    manifest: dict[str, Any] = {
        "schema": EXPORT_MANIFEST_SCHEMA,
        "cut": cut,
        "cut_paths": cut_paths,
        "cut_paths_sha256": hashlib.sha256("\n".join(cut_paths).encode("utf-8")).hexdigest(),
    }
    source_ref, source_path = provenance.get("cla_v1_source_ref"), provenance.get("cla_v1_source_path")
    if isinstance(source_ref, str) and isinstance(source_path, str):
        manifest["cla_v1_source_ref"] = source_ref
        blobs[f"{source_ref}:{source_path}"] = history.digest(source_ref, source_path)
    signature_ref = provenance.get("cla_v1_signature_ref")
    signature_path = provenance.get("cla_v1_signature_path")
    if isinstance(signature_ref, str) and isinstance(signature_path, str):
        manifest["cla_v1_signature_ref"] = signature_ref
        manifest["cla_v1_signatures"] = {
            "ref": signature_ref,
            "path": signature_path,
            **history.signatures(signature_ref, signature_path).to_dict(),
        }
    manifest["blobs"] = dict(sorted(blobs.items()))
    return manifest


def render_export_manifest(manifest: dict[str, Any]) -> str:
    return json.dumps(manifest, indent=2, sort_keys=True) + "\n"


def load_export_manifest(root: Path) -> dict[str, Any]:
    raw = read_bounded(root / EXPORT_MANIFEST_PATH, MAX_FILE_BYTES)
    try:
        manifest = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise AuditError("provenance manifest is not valid JSON") from exc
    if not isinstance(manifest, dict) or manifest.get("schema") != EXPORT_MANIFEST_SCHEMA:
        raise AuditError("provenance manifest has an unknown schema")
    return manifest


def enumerate_paths(root: Path, ref: str | None, max_files: int) -> list[str]:
    if ref is None:
        raw = git(root, "ls-files", "-co", "--exclude-standard", "-z")
    else:
        if not safe_ref(ref):
            raise AuditError("invalid git ref")
        raw = git(root, "ls-tree", "-r", "--name-only", "-z", ref)
    fields = [field for field in raw.split(b"\0") if field]
    if len(fields) > max_files:
        raise AuditError("tracked file count exceeds configured limit")
    paths = []
    for field in fields:
        path = decode_git(field)
        if not safe_relpath(path):
            raise AuditError("unsafe tracked path")
        paths.append(path)
    return sorted(set(paths))


def parse_spdx(expression: str) -> bool:
    """Parse the SPDX expression subset needed by the matrix and headers."""

    tokens = re.findall(r"\(|\)|\bAND\b|\bOR\b|\bWITH\b|[^\s()]+", expression)
    if not tokens or "".join(tokens) == "":
        return False
    index = 0

    def identifier(token: str) -> bool:
        if not SPDX_ID_RE.fullmatch(token):
            return False
        return token in KNOWN_SPDX_IDS or token.startswith("LicenseRef-")

    def parse_atom() -> bool:
        nonlocal index
        if index >= len(tokens):
            return False
        token = tokens[index]
        if token == "(":
            index += 1
            if not parse_or() or index >= len(tokens) or tokens[index] != ")":
                return False
            index += 1
            return True
        index += 1
        return identifier(token)

    def parse_with() -> bool:
        nonlocal index
        if not parse_atom():
            return False
        if index < len(tokens) and tokens[index].upper() == "WITH":
            index += 1
            if index >= len(tokens) or not identifier(tokens[index]):
                return False
            index += 1
        return True

    def parse_and() -> bool:
        nonlocal index
        if not parse_with():
            return False
        while index < len(tokens) and tokens[index].upper() == "AND":
            index += 1
            if not parse_with():
                return False
        return True

    def parse_or() -> bool:
        nonlocal index
        if not parse_and():
            return False
        while index < len(tokens) and tokens[index].upper() == "OR":
            index += 1
            if not parse_and():
                return False
        return True

    return parse_or() and index == len(tokens)


def spdx_header(raw: bytes) -> tuple[str | None, list[str]]:
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError:
        return None, ["invalid UTF-8"]
    values = []
    for line in text.splitlines()[:40]:
        if "SPDX-License-Identifier:" not in line:
            continue
        match = SPDX_HEADER_RE.search(line.strip())
        if not match:
            values.append("")
        else:
            values.append(match.group(1).strip().rstrip("#/"))
    if not values:
        return None, []
    return values[0], values


def matches(path: str, rule: dict[str, Any]) -> bool:
    if rule.get("root_only") and "/" in path:
        return False
    includes = rule.get("include", [])
    excludes = rule.get("exclude", [])
    return any(fnmatch.fnmatchcase(path, pattern) for pattern in includes) and not any(
        fnmatch.fnmatchcase(path, pattern) for pattern in excludes
    )


def relative_target(root: Path, path: str) -> Path:
    target = root / PurePosixPath(path)
    if target.is_symlink():
        raise AuditError("tracked symlink is not allowed")
    try:
        target.resolve().relative_to(root.resolve())
    except ValueError as exc:
        raise AuditError("path escapes repository") from exc
    return target


def is_gitlink(root: Path, path: str) -> bool:
    try:
        record = decode_git(git(root, "ls-files", "-s", "--", path))
    except AuditError:
        return False
    return record.startswith("160000 ")


def validate_matrix(root: Path, matrix_path: Path) -> tuple[dict[str, Any], list[str]]:
    errors: list[str] = []
    try:
        matrix_raw = read_bounded(matrix_path, MAX_MATRIX_BYTES)
        data = load_toml(matrix_raw)
    except (AuditError, UnicodeDecodeError, ValueError):
        return {}, ["LICENSE_MATRIX.toml: invalid TOML or unreadable matrix"]
    if not isinstance(data, dict):
        return {}, ["LICENSE_MATRIX.toml: top-level value must be a table"]

    expected_top = {
        "schema",
        "version",
        "distribution",
        "authority",
        "pre_v4_apache_cut",
        "new_source_requires_spdx",
        "limits",
        "licenses",
        "provenance",
        "required_artifacts",
        "rules",
    }
    unknown_top = sorted(set(data) - expected_top)
    if unknown_top:
        errors.append("LICENSE_MATRIX.toml: unknown top-level key")
    if data.get("schema") != "leanctx.license-matrix/v1" or data.get("version") != 1:
        errors.append("LICENSE_MATRIX.toml: unsupported schema version")
    if data.get("authority") != "LICENSE.md":
        errors.append("LICENSE_MATRIX.toml: authority must be LICENSE.md")
    distribution = data.get("distribution", "mixed-source")
    if distribution not in ("mixed-source", "apache-host"):
        errors.append("LICENSE_MATRIX.toml: unsupported distribution")
    cut = data.get("pre_v4_apache_cut")
    if not isinstance(cut, str) or not safe_ref(cut):
        errors.append("LICENSE_MATRIX.toml: invalid pre-v4 git ref")
    if not isinstance(data.get("new_source_requires_spdx"), bool):
        errors.append("LICENSE_MATRIX.toml: new_source_requires_spdx must be boolean")

    limits = data.get("limits")
    if not isinstance(limits, dict):
        errors.append("LICENSE_MATRIX.toml: limits table missing")
        limits = {}
    for key, ceiling in {
        "max_tracked_files": MAX_TRACKED_FILES,
        "max_file_bytes": MAX_FILE_BYTES,
        "max_total_bytes": MAX_TOTAL_BYTES,
        "max_git_output_bytes": MAX_GIT_OUTPUT_BYTES,
    }.items():
        value = limits.get(key)
        if not isinstance(value, int) or value <= 0 or value > ceiling:
            errors.append(f"LICENSE_MATRIX.toml: invalid limit {key}")

    licenses = data.get("licenses")
    if not isinstance(licenses, dict) or not licenses:
        errors.append("LICENSE_MATRIX.toml: licenses table missing")
        licenses = {}
    for name, spec in sorted(licenses.items()):
        if not isinstance(name, str) or not isinstance(spec, dict):
            errors.append("LICENSE_MATRIX.toml: invalid license entry")
            continue
        identifier = spec.get("spdx")
        text_path = spec.get("text")
        if not isinstance(identifier, str) or not parse_spdx(identifier):
            errors.append(f"license {name}: invalid SPDX expression")
        if not isinstance(text_path, str) or not safe_relpath(text_path):
            errors.append(f"license {name}: unsafe license text path")
        else:
            try:
                text_target = relative_target(root, text_path)
                if not text_target.is_file() and not (
                    name == "free_runtime" and text_path.endswith("-DRAFT.txt")
                ):
                    errors.append(f"license {name}: missing license text")
            except AuditError:
                errors.append(f"license {name}: license text escapes repository")
        for boolean in ("redistributable", "notice_required"):
            if not isinstance(spec.get(boolean), bool):
                errors.append(f"license {name}: {boolean} must be boolean")
        if isinstance(identifier, str) and identifier.startswith("LicenseRef-") and not isinstance(
            text_path, str
        ):
            errors.append(f"license {name}: custom license text required")

    provenance = data.get("provenance")
    if not isinstance(provenance, dict):
        errors.append("LICENSE_MATRIX.toml: provenance table missing")
        provenance = {}
    for key in (
        "cla_v1_legacy_path",
        "cla_v1_source_path",
        "cla_v1_source_ref",
        "cla_v1_sha256",
    ):
        if not isinstance(provenance.get(key), str) or not provenance[key]:
            errors.append(f"provenance: missing {key}")
    if not isinstance(provenance.get("cla_v1_sha256"), str) or not SHA256_RE.fullmatch(
        str(provenance.get("cla_v1_sha256", ""))
    ):
        errors.append("provenance: invalid CLA-v1 SHA-256")
    signature_ref = provenance.get("cla_v1_signature_ref")
    signature_path = provenance.get("cla_v1_signature_path")
    signature_sha = provenance.get("cla_v1_signature_sha256")
    signature_count = provenance.get("cla_v1_signature_count")
    if not isinstance(signature_ref, str) or not safe_ref(signature_ref):
        errors.append("provenance: invalid CLA-v1 signature ref")
    if not isinstance(signature_path, str) or not safe_relpath(signature_path):
        errors.append("provenance: invalid CLA-v1 signature path")
    if signature_path != CLA_V1_SIGNATURE_PATH:
        errors.append("provenance: CLA-v1 signature path must be v1 cla.json")
    if signature_ref != CLA_V1_SIGNATURE_REF:
        errors.append("provenance: CLA-v1 signature ref is not the immutable source")
    if not isinstance(signature_sha, str) or not SHA256_RE.fullmatch(signature_sha):
        errors.append("provenance: invalid CLA-v1 signature SHA-256")
    elif signature_sha != CLA_V1_SIGNATURE_SHA256:
        errors.append("provenance: CLA-v1 signature digest is stale")
    if signature_count != CLA_V1_SIGNATURE_COUNT:
        errors.append("provenance: CLA-v1 signer count is not 19")
    for key in ("cla_v1_legacy_path", "cla_v1_source_path"):
        value = provenance.get(key)
        if isinstance(value, str) and not safe_relpath(value):
            errors.append(f"provenance: unsafe {key}")
    if isinstance(cut, str) and provenance.get("cla_v1_source_ref") != cut:
        errors.append("provenance: CLA-v1 source ref differs from Apache cut")

    required = data.get("required_artifacts")
    if not isinstance(required, dict):
        errors.append("LICENSE_MATRIX.toml: required_artifacts table missing")
        required = {}
    for key in ("paths", "release_paths"):
        values = required.get(key)
        if not isinstance(values, list) or not values or not all(
            isinstance(value, str) and safe_relpath(value) for value in values
        ):
            errors.append(f"required_artifacts: invalid {key}")
    if distribution == "apache-host":
        paths = required.get("paths", [])
        releases = required.get("release_paths", [])
        if not isinstance(paths, list) or not all(isinstance(path, str) for path in paths) or not APACHE_HOST_REQUIRED_ARTIFACTS.issubset(paths):
            errors.append("apache-host: public provenance artifacts must remain required")
        if not isinstance(releases, list) or "SBOM.cdx.json" not in releases:
            errors.append("apache-host: release SBOM must remain required")
        if set(licenses) != {"apache", "third_party"}:
            errors.append("apache-host: only Apache and upstream license catalogs are allowed")
        apache_license = licenses.get("apache")
        if not isinstance(apache_license, dict) or apache_license.get("spdx") != "Apache-2.0":
            errors.append("apache-host: first-party source must retain Apache-2.0")

    rules = data.get("rules")
    if not isinstance(rules, list) or not rules:
        errors.append("LICENSE_MATRIX.toml: ordered rules missing")
        rules = []
    seen_names: set[str] = set()
    seen_orders: list[int] = []
    seen_classes: set[str] = set()
    for expected_order, rule in enumerate(rules, 1):
        if not isinstance(rule, dict):
            errors.append("rule: entry must be a table")
            continue
        unknown = sorted(set(rule) - RULE_KEYS)
        if unknown:
            errors.append("rule: unknown key")
        order = rule.get("order")
        name = rule.get("name")
        klass = rule.get("class")
        license_name = rule.get("license")
        if order != expected_order:
            errors.append("rule: order is not contiguous")
        if not isinstance(name, str) or not name or name in seen_names:
            errors.append("rule: names must be unique and non-empty")
        else:
            seen_names.add(name)
        if not isinstance(klass, str) or klass not in REQUIRED_CLASSES:
            errors.append("rule: unknown classification")
        else:
            seen_classes.add(klass)
        if klass != "private_service" and not rule.get("forbidden_public") and license_name not in licenses:
            errors.append("rule: non-private class references unknown license")
        if distribution == "apache-host" and isinstance(klass, str) and klass in APACHE_HOST_PRIVATE_CLASSES and rule.get("forbidden_public") is not True:
            errors.append("apache-host: private and commercial classes must be forbidden_public")
        if klass == "private_service" and not rule.get("forbidden_public"):
            errors.append("rule: private_service must be forbidden_public")
        if klass == "commercial" and rule.get("artifact_kind") != "license_text" and not rule.get(
            "provenance_required"
        ):
            errors.append("rule: commercial source requires provenance")
        if klass == "generated":
            if not rule.get("generated") or not isinstance(rule.get("owner"), str) or not isinstance(
                rule.get("provenance"), str
            ):
                errors.append("rule: generated paths require owner and provenance")
        if klass == "third_party":
            if not rule.get("vendor") or not isinstance(rule.get("owner"), str) or not isinstance(
                rule.get("provenance"), str
            ) or not isinstance(rule.get("notice"), str):
                errors.append("rule: vendor paths require owner, provenance, and notice")
        includes = rule.get("include")
        excludes = rule.get("exclude", [])
        if not isinstance(includes, list) or not includes or not all(
            isinstance(pattern, str) and safe_relpath(pattern.replace("*", "x"))
            for pattern in includes
        ):
            errors.append("rule: include patterns must be safe non-empty strings")
        if not isinstance(excludes, list) or not all(
            isinstance(pattern, str) and safe_relpath(pattern.replace("*", "x"))
            for pattern in excludes
        ):
            errors.append("rule: exclude patterns must be safe strings")
        seen_orders.append(order if isinstance(order, int) else -1)
    if seen_orders != list(range(1, len(rules) + 1)):
        errors.append("rule: ordered classification is not deterministic")
    if seen_classes != REQUIRED_CLASSES:
        errors.append("rule: matrix must cover every required path category")
    return data, sorted(set(errors))


def target_bytes(root: Path, path: str, limit: int) -> tuple[bytes | None, str | None]:
    try:
        target = relative_target(root, path)
        if not target.is_file():
            return None, "file is missing"
        return read_bounded(target, limit), None
    except AuditError as exc:
        return None, str(exc)


def validate_sbom(root: Path, path: str) -> str | None:
    raw, problem = target_bytes(root, path, MAX_FILE_BYTES)
    if problem:
        return f"{path}: {problem}"
    try:
        document = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        return f"{path}: invalid JSON"
    if not isinstance(document, dict):
        return f"{path}: SBOM must be an object"
    if document.get("bomFormat") == "CycloneDX":
        if not isinstance(document.get("specVersion"), str) or not isinstance(
            document.get("components"), list
        ) or not document["components"]:
            return f"{path}: incomplete CycloneDX SBOM"
        for component in document["components"]:
            if not isinstance(component, dict) or not all(
                isinstance(component.get(key), str) and component[key]
                for key in ("type", "name", "version")
            ):
                return f"{path}: invalid CycloneDX component"
        if re.search(r"path\+file://(?:/|[A-Za-z]:)", raw.decode("utf-8")):
            return f"{path}: contains absolute local file reference"
        metadata = document.get("metadata")
        properties = metadata.get("properties") if isinstance(metadata, dict) else None
        if not isinstance(properties, list):
            return f"{path}: missing Cargo.lock digest"
        lock_digests = [
            item.get("value")
            for item in properties
            if isinstance(item, dict)
            and item.get("name") == "leanctx:cargo-lock-sha256"
            and isinstance(item.get("value"), str)
        ]
        if len(lock_digests) != 1:
            return f"{path}: expected exactly one Cargo.lock digest"
        lock_raw, lock_problem = target_bytes(root, "rust/Cargo.lock", MAX_FILE_BYTES)
        if lock_problem:
            return f"{path}: cannot validate Cargo.lock digest: {lock_problem}"
        expected_lock_digest = hashlib.sha256(lock_raw).hexdigest()
        if lock_digests[0] != expected_lock_digest:
            return f"{path}: Cargo.lock digest is stale"
        return None
    if isinstance(document.get("spdxVersion"), str) and isinstance(document.get("packages"), list):
        if not document["packages"]:
            return f"{path}: incomplete SPDX SBOM"
        for package in document["packages"]:
            if not isinstance(package, dict) or not isinstance(package.get("name"), str) or not isinstance(
                package.get("versionInfo"), str
            ):
                return f"{path}: invalid SPDX package"
        return None
    return f"{path}: expected CycloneDX or SPDX SBOM"


def _validate_legal_approval(root: Path, artifacts: dict[str, bytes], errors: list[str]) -> None:
    """Validate a recorded external decision; the checker never grants approval."""
    raw, problem = target_bytes(root, LEGAL_APPROVAL_PATH, MAX_MATRIX_BYTES)
    if problem:
        errors.append(f"legal approval: evidence missing or unreadable ({LEGAL_APPROVAL_PATH})")
        return
    try:
        approval = load_toml(raw)
    except (MatrixTomlError, ValueError, TypeError):
        errors.append("legal approval: malformed evidence")
        return
    if set(approval) != {"schema", "approving_counsel", "approval_evidence", "approved_at", "artifacts"} or approval.get("schema") != LEGAL_APPROVAL_SCHEMA:
        errors.append("legal approval: invalid schema or fields")
        return
    placeholder = re.compile(r"\b(?:NOT\s+RECORDED|NOT\s+SET|UNKNOWN|TBD|TODO|PENDING)\b", re.I)
    for field in ("approving_counsel", "approval_evidence", "approved_at"):
        value = approval.get(field)
        if not isinstance(value, str) or not value.strip() or placeholder.search(value) or LEGAL_MARKER_RE.search(value):
            errors.append(f"legal approval: missing or unresolved {field}")
    try:
        date.fromisoformat(approval["approved_at"])
    except (ValueError, TypeError):
        errors.append("legal approval: invalid approved_at date")
    record = artifacts["LEGAL_REVIEW_REQUIRED.md"].decode("utf-8")
    for field in DECISION_FIELDS:
        values = re.findall(r"^\s*(?:-\s*)?" + re.escape(field) + r"[ \t]*([^\r\n]*)$", record, re.M)
        if len(values) != 1:
            errors.append(f"legal approval: missing or duplicate decision {field[:-1]}")
            continue
        value = values[0].strip()
        if field.endswith("_status:"):
            valid = value == "APPROVED"
        elif field == "effective_date:":
            try:
                date.fromisoformat(value)
                valid = True
            except ValueError:
                valid = False
        else:
            valid = bool(value) and value == approval.get(field[:-1])
        if not valid:
            errors.append(f"legal approval: unresolved or inconsistent decision {field[:-1]}")
    entries = approval.get("artifacts")
    if not isinstance(entries, list) or len(entries) != len(LEGAL_DRAFT_PATHS):
        errors.append("legal approval: exact artifact inventory required")
        return
    seen: set[str] = set()
    for entry in entries:
        if not isinstance(entry, dict) or set(entry) != {"path", "sha256"}:
            errors.append("legal approval: malformed artifact")
            continue
        path, digest = entry["path"], entry["sha256"]
        if not isinstance(path, str) or path not in artifacts or path in seen:
            errors.append("legal approval: unknown or duplicate artifact")
            continue
        seen.add(path)
        if not isinstance(digest, str) or not SHA256_RE.fullmatch(digest) or digest != hashlib.sha256(artifacts[path]).hexdigest():
            errors.append(f"legal approval: missing, invalid or stale digest for {path}")
    if seen != set(LEGAL_DRAFT_PATHS):
        errors.append("legal approval: incomplete artifact inventory")


def validate_legal_files(root: Path, strict: bool, warnings: list[str], errors: list[str]) -> None:
    artifacts: dict[str, bytes] = {}
    operative = []
    for path in LEGAL_DRAFT_PATHS:
        raw, problem = target_bytes(root, path, MAX_FILE_BYTES)
        if problem:
            errors.append(f"{path}: {problem}")
            continue
        try:
            text = raw.decode("utf-8")
        except UnicodeDecodeError:
            errors.append(f"{path}: invalid UTF-8")
            continue
        artifacts[path] = raw
        if LEGAL_MARKER_RE.search(text):
            if "NON-OPERATIVE DRAFT" not in text and path != "LEGAL_REVIEW_REQUIRED.md":
                errors.append(f"{path}: missing NON-OPERATIVE DRAFT marker")
            message = f"{path}: unresolved legal marker"
            (errors if strict else warnings).append(message)
        else:
            operative.append(path)
    legal_review, problem = target_bytes(root, "LEGAL_REVIEW_REQUIRED.md", MAX_FILE_BYTES)
    if problem:
        errors.append(f"LEGAL_REVIEW_REQUIRED.md: {problem}")
    else:
        text = legal_review.decode("utf-8", "replace")
        for field in DECISION_FIELDS:
            if field not in text:
                errors.append(f"LEGAL_REVIEW_REQUIRED.md: missing decision field {field[:-1]}")
    if operative:
        if len(operative) != len(LEGAL_DRAFT_PATHS):
            errors.append("legal approval: mixed or incomplete draft/operative artifacts")
        else:
            _validate_legal_approval(root, artifacts, errors)


def _validate_notice_approval(
    root: Path,
    notice_raw: bytes,
    errors: list[str],
) -> None:
    approval_raw, problem = target_bytes(root, NOTICE_APPROVAL_PATH, MAX_FILE_BYTES)
    if problem:
        errors.append(f"THIRD_PARTY_NOTICES: approval evidence missing ({NOTICE_APPROVAL_PATH})")
        return
    try:
        document = load_toml(approval_raw)
    except (MatrixTomlError, ValueError, TypeError):
        errors.append(f"THIRD_PARTY_NOTICES: malformed approval evidence ({NOTICE_APPROVAL_PATH})")
        return
    if document.get("schema") != NOTICE_APPROVAL_SCHEMA:
        errors.append(f"THIRD_PARTY_NOTICES: malformed approval schema ({NOTICE_APPROVAL_PATH})")
    digest = document.get("approved_content_sha256")
    if not isinstance(digest, str) or not SHA256_RE.fullmatch(digest):
        errors.append("THIRD_PARTY_NOTICES: approval digest is missing or malformed")
    elif digest != hashlib.sha256(notice_raw).hexdigest():
        errors.append("THIRD_PARTY_NOTICES: approval digest is stale for current content")
    # Policy (2026-09-28): the copyright owner may approve the generated
    # attribution notices in place of external counsel. Exactly one approver
    # field must be present so the record names who carries the decision.
    approvers = [
        field
        for field in ("approving_counsel", "approving_owner")
        if isinstance(document.get(field), str) and document[field].strip()
    ]
    if not approvers:
        errors.append("THIRD_PARTY_NOTICES: approval field approving_counsel or approving_owner is missing")
    elif len(approvers) > 1:
        errors.append("THIRD_PARTY_NOTICES: approval must name exactly one approver")
    for field in ("approval_evidence", "approved_at"):
        value = document.get(field)
        if not isinstance(value, str) or not value.strip():
            errors.append(f"THIRD_PARTY_NOTICES: approval field {field} is missing")
    if LEGAL_MARKER_RE.search(approval_raw.decode("utf-8", "replace")):
        errors.append("THIRD_PARTY_NOTICES: approval evidence contains an unresolved legal marker")


def validate_notice_generation(root: Path, errors: list[str]) -> None:
    script = root / "scripts" / "generate-third-party-notices.py"
    if script.is_symlink() or not script.is_file():
        errors.append("THIRD_PARTY_NOTICES: generator script is missing")
        return
    try:
        result = subprocess.run(
            [sys.executable, "scripts/generate-third-party-notices.py", "--root", ".", "--check"],
            cwd=root,
            check=False,
            capture_output=True,
            timeout=180,
        )
    except (OSError, subprocess.TimeoutExpired):
        errors.append("THIRD_PARTY_NOTICES: generator --check unavailable")
        return
    if result.returncode != 0:
        errors.append("THIRD_PARTY_NOTICES: generator --check failed")


def validate_notice(root: Path, strict: bool, warnings: list[str], errors: list[str]) -> str | None:
    raw, problem = target_bytes(root, "THIRD_PARTY_NOTICES", MAX_FILE_BYTES)
    if problem:
        errors.append(f"THIRD_PARTY_NOTICES: {problem}")
        return None
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError:
        errors.append("THIRD_PARTY_NOTICES: invalid UTF-8")
        return None
    for marker in (
        "Schema:",
        "Status:",
        "Generation command:",
        "Source inventory:",
        "Content-Digest:",
        "Approval contract:",
    ):
        if marker not in text:
            errors.append(f"THIRD_PARTY_NOTICES: missing {marker[:-1]}")
    if str(root) in text or f"file:///{root.as_posix().lstrip('/')}" in text:
        errors.append("THIRD_PARTY_NOTICES: contains absolute worktree path")
    digest_matches = NOTICE_DIGEST_RE.findall(text)
    if len(digest_matches) != 1:
        errors.append("THIRD_PARTY_NOTICES: expected exactly one content digest")
    else:
        canonical = re.sub(
            rb"^Content-Digest:\s*sha256:[0-9a-f]{64}$",
            b"Content-Digest: sha256:" + (b"0" * 64),
            raw,
            count=1,
            flags=re.MULTILINE,
        )
        if hashlib.sha256(canonical).hexdigest() != digest_matches[0]:
            errors.append("THIRD_PARTY_NOTICES: content digest is invalid")
    status_match = re.search(r"^Status:\s*(.+)$", text, re.MULTILINE)
    status = status_match.group(1).strip() if status_match else None
    if status == "PENDING GENERATION":
        message = "THIRD_PARTY_NOTICES: dependency notices are not generated"
        (errors if strict else warnings).append(message)
    elif status == "GENERATED - PENDING APPROVAL":
        message = "THIRD_PARTY_NOTICES: generated notices await exact-digest legal approval"
        (errors if strict else warnings).append(message)
    elif status == "GENERATED AND APPROVED":
        _validate_notice_approval(root, raw, errors)
    else:
        errors.append("THIRD_PARTY_NOTICES: unknown status")
    return status


def validate_paths(
    root: Path,
    paths: list[str],
    matrix: dict[str, Any],
    rules: list[dict[str, Any]],
    cut_paths: set[str],
    errors: list[str],
) -> tuple[list[dict[str, Any]], int, int]:
    rows: list[dict[str, Any]] = []
    total_bytes = 0
    vendor_count = 0
    generated_count = 0
    for path in paths:
        selected = [rule for rule in rules if matches(path, rule)]
        if len(selected) != 1:
            names = ",".join(str(rule.get("name")) for rule in selected) or "none"
            errors.append(f"{path}: expected exactly one matrix rule ({names})")
            continue
        rule = selected[0]
        klass = str(rule.get("class"))
        if klass == "third_party":
            vendor_count += 1
        if klass == "generated":
            generated_count += 1
        try:
            target = relative_target(root, path)
            if target.is_dir() and is_gitlink(root, path):
                raw = b""
            else:
                raw = read_bounded(target, int(matrix["limits"]["max_file_bytes"]))
        except (AuditError, KeyError, TypeError, ValueError) as exc:
            errors.append(f"{path}: {str(exc)}")
            continue
        total_bytes += len(raw)
        if total_bytes > int(matrix["limits"]["max_total_bytes"]):
            errors.append("tracked content exceeds configured size limit")
            break
        forbidden = bool(rule.get("forbidden_public"))
        if matrix.get("distribution") == "apache-host" and (klass in APACHE_HOST_PRIVATE_CLASSES or path in APACHE_HOST_PRIVATE_ARTIFACTS):
            forbidden = True
        if forbidden:
            errors.append(f"{path}: private_service paths are forbidden in public tree")
        if ("/generated/" in f"/{path}" or path.startswith("generated/")) and klass not in {
            "generated",
            "tests_examples",
        }:
            errors.append(f"{path}: generated path lacks generated provenance rule")
        if ("/vendor/" in f"/{path}" or path.startswith("vendor/")) and klass != "third_party":
            errors.append(f"{path}: vendor path lacks third-party provenance rule")

        is_source = PurePosixPath(path).suffix.lower() in SOURCE_SUFFIXES
        header, header_values = spdx_header(raw) if is_source else (None, [])
        if not rule.get("fixture"):
            if len(header_values) > 1 and len(set(header_values)) != 1:
                errors.append(f"{path}: conflicting SPDX headers")
            for value in header_values:
                if not value or not parse_spdx(value):
                    errors.append(f"{path}: invalid SPDX expression")
        expected = None
        license_name = rule.get("license")
        license_spec = matrix.get("licenses", {}).get(license_name, {})
        if isinstance(license_spec, dict):
            expected = license_spec.get("spdx")
        is_new_source = path not in cut_paths
        requires_header = bool(is_source and not forbidden and not rule.get("fixture") and (
            (klass == "commercial" and rule.get("artifact_kind") != "license_text")
            or (matrix.get("new_source_requires_spdx") and is_new_source)
        ))
        if requires_header and header != expected:
            errors.append(f"{path}: missing or mismatched required SPDX header")
        if (
            header is not None
            and not forbidden
            and not rule.get("fixture")
            and klass != "third_party"
            and rule.get("artifact_kind") != "license_text"
        ):
            if header != expected:
                errors.append(f"{path}: SPDX header conflicts with matrix")
        rows.append({
            "path": path,
            "rule": rule.get("name"),
            "class": klass,
            "new_since_cut": is_new_source,
            "spdx_header": header,
        })
    if total_bytes > int(matrix["limits"]["max_total_bytes"]):
        errors.append("tracked content exceeds configured size limit")
    return rows, vendor_count, generated_count


def validate_artifacts(
    root: Path,
    matrix: dict[str, Any],
    ref: str,
    strict: bool,
    errors: list[str],
    warnings: list[str],
    history: "GitHistory | ManifestHistory | None" = None,
) -> None:
    history = history or GitHistory(root)
    required = matrix.get("required_artifacts", {})
    for path in required.get("paths", []):
        raw, problem = target_bytes(root, path, MAX_FILE_BYTES)
        if problem or raw == b"":
            errors.append(f"{path}: missing required artifact" if not raw else f"{path}: {problem}")
    if strict:
        for path in required.get("release_paths", []):
            problem = validate_sbom(root, path) if path.endswith((".cdx.json", ".spdx.json")) else None
            if path.endswith((".cdx.json", ".spdx.json")) and problem:
                errors.append(problem)
            elif not path.endswith((".cdx.json", ".spdx.json")):
                _, missing = target_bytes(root, path, MAX_FILE_BYTES)
                if missing:
                    errors.append(f"{path}: missing release artifact")

    cut = str(matrix.get("pre_v4_apache_cut", ""))
    if not safe_ref(cut):
        return
    for path in PRESERVED_PATHS:
        current, problem = target_bytes(root, path, MAX_FILE_BYTES)
        if problem:
            errors.append(f"{path}: {problem}")
            continue
        try:
            historical = history.digest(cut, path)
        except AuditError:
            errors.append(f"{path}: historical source cannot be proven")
            continue
        if hashlib.sha256(current).hexdigest() != historical:
            errors.append(f"{path}: historical bytes changed")

    apache_path = "LICENSES/Apache-2.0.txt"
    apache, problem = target_bytes(root, apache_path, MAX_FILE_BYTES)
    if problem:
        errors.append(f"{apache_path}: {problem}")
    else:
        try:
            if hashlib.sha256(apache).hexdigest() != history.digest(cut, "LICENSE"):
                errors.append(f"{apache_path}: does not equal pre-v4 Apache bytes")
        except AuditError:
            errors.append(f"{apache_path}: Apache source bytes cannot be proven")

    provenance = matrix.get("provenance", {})
    legacy_path = provenance.get("cla_v1_legacy_path")
    source_path = provenance.get("cla_v1_source_path")
    source_ref = provenance.get("cla_v1_source_ref")
    expected_sha = provenance.get("cla_v1_sha256")
    if all(isinstance(value, str) for value in (legacy_path, source_path, source_ref, expected_sha)):
        legacy, legacy_problem = target_bytes(root, legacy_path, MAX_FILE_BYTES)
        if legacy_problem:
            errors.append(f"{legacy_path}: {legacy_problem}")
        elif hashlib.sha256(legacy).hexdigest() != expected_sha:
            errors.append(f"{legacy_path}: CLA-v1 digest drift")
        try:
            if legacy_problem is None and hashlib.sha256(legacy).hexdigest() != history.digest(source_ref, source_path):
                errors.append(f"{legacy_path}: CLA-v1 source provenance drift")
        except AuditError:
            errors.append(f"{legacy_path}: CLA-v1 source cannot be proven")

    signature_ref = provenance.get("cla_v1_signature_ref")
    signature_path = provenance.get("cla_v1_signature_path")
    expected_signature_sha = provenance.get("cla_v1_signature_sha256")
    expected_signature_count = provenance.get("cla_v1_signature_count")
    if all(isinstance(value, str) for value in (signature_ref, signature_path, expected_signature_sha)):
        try:
            summary = history.signatures(signature_ref, signature_path)
        except AuditError:
            errors.append("CLA-v1 signature source cannot be proven")
            return
        if summary.sha256 != expected_signature_sha:
            errors.append("CLA-v1 signature inventory digest drift")
        if not summary.valid_json:
            errors.append("CLA-v1 signature inventory is invalid JSON or lacks signedContributors")
            return
        if summary.invalid_signer:
            errors.append("CLA-v1 signature inventory contains an invalid signer")
        if isinstance(expected_signature_count, int) and summary.count != expected_signature_count:
            errors.append("CLA-v1 signature inventory signer count drift")
        if not summary.unique:
            errors.append("CLA-v1 signature inventory contains duplicate signers")


def run_audit(root: Path, ref: str | None, strict: bool, export: bool = False) -> dict[str, Any]:
    """Audit the tree. ``export``: a history-free public export, whose
    historical provenance comes from the committed provenance manifest."""
    errors: list[str] = []
    warnings: list[str] = []
    root = root.resolve()
    history: GitHistory | ManifestHistory = GitHistory(root)
    if export:
        try:
            history = ManifestHistory(load_export_manifest(root))
        except AuditError as exc:
            errors.append(f"{EXPORT_MANIFEST_PATH}: {exc}")
            return make_report(ref, strict, [], [], errors, warnings)
    if not root.is_dir():
        errors.append("repository root is not a directory")
        return make_report(ref, strict, [], [], errors, warnings)
    try:
        top = decode_git(git(root, "rev-parse", "--show-toplevel")).strip()
        if Path(top).resolve() != root:
            errors.append("repository root is not the git worktree root")
    except AuditError as exc:
        errors.append(str(exc))
    matrix_path = root / "LICENSE_MATRIX.toml"
    matrix, matrix_errors = validate_matrix(root, matrix_path)
    errors.extend(matrix_errors)
    if not matrix:
        return make_report(ref, strict, [], [], errors, warnings)
    try:
        paths = enumerate_paths(
            root,
            ref,
            min(int(matrix["limits"]["max_tracked_files"]), MAX_TRACKED_FILES),
        )
    except (AuditError, KeyError, TypeError, ValueError) as exc:
        errors.append(str(exc))
        paths = []
    rules = matrix.get("rules", [])
    if not isinstance(rules, list):
        rules = []
    cut = str(matrix.get("pre_v4_apache_cut", ""))
    try:
        cut_paths = history.paths(cut)
    except AuditError:
        errors.append("pre-v4 Apache cut is not readable")
        cut_paths = set()
    rows, vendor_count, generated_count = validate_paths(
        root, paths, matrix, rules, cut_paths, errors
    )
    validate_artifacts(root, matrix, ref or "WORKTREE", strict, errors, warnings, history)
    if not export:
        check_export_manifest(root, matrix, errors)
    notice_status = validate_notice(root, strict, warnings, errors)
    validate_notice_generation(root, errors)
    if matrix.get("distribution", "mixed-source") == "mixed-source":
        validate_legal_files(root, strict, warnings, errors)
    if vendor_count and notice_status != "GENERATED AND APPROVED":
        message = "third-party paths require exact-digest legal approval of generated notices"
        (errors if strict else warnings).append(message)
    if generated_count and not any(row["class"] == "generated" for row in rows):
        errors.append("generated paths lack provenance")
    return make_report(ref, strict, paths, rows, errors, warnings)


def check_export_manifest(root: Path, matrix: dict[str, Any], errors: list[str]) -> None:
    """Where history exists, the committed manifest must be exactly what the
    history derives: an export can then trust it without the history."""
    path = root / EXPORT_MANIFEST_PATH
    if not path.exists():
        errors.append(f"{EXPORT_MANIFEST_PATH}: missing (run --write-export-manifest)")
        return
    try:
        expected = render_export_manifest(build_export_manifest(root, matrix))
        actual = read_bounded(path, MAX_FILE_BYTES).decode("utf-8")
    except (AuditError, UnicodeDecodeError) as exc:
        errors.append(f"{EXPORT_MANIFEST_PATH}: cannot be re-derived from history ({exc})")
        return
    if actual != expected:
        errors.append(f"{EXPORT_MANIFEST_PATH}: differs from what history derives")


def make_report(
    ref: str | None,
    strict: bool,
    paths: list[str],
    rows: list[dict[str, Any]],
    errors: list[str],
    warnings: list[str],
) -> dict[str, Any]:
    return {
        "schema": TOOL_SCHEMA,
        "mode": "release-strict" if strict else "audit",
        "ref": ref or "WORKTREE",
        "tracked_paths": len(paths),
        "classified_paths": len(rows),
        "classification": rows,
        "errors": sorted(set(errors)),
        "warnings": sorted(set(warnings)),
        "result": "FAIL" if errors else "PASS",
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--ref", help="audit this exact git tree; omit for tracked working tree")
    parser.add_argument("--output", type=Path, help="write the deterministic JSON report")
    parser.add_argument("--release-strict", action="store_true")
    parser.add_argument(
        "--export",
        action="store_true",
        help=f"audit a history-free export against its {EXPORT_MANIFEST_PATH}",
    )
    parser.add_argument(
        "--write-export-manifest",
        action="store_true",
        help=f"derive {EXPORT_MANIFEST_PATH} from history and write it",
    )
    args = parser.parse_args(argv)
    if args.write_export_manifest:
        root = args.root.resolve()
        matrix, matrix_errors = validate_matrix(root, root / "LICENSE_MATRIX.toml")
        if matrix_errors or not matrix:
            for message in matrix_errors:
                print(f"ip-provenance-audit: {message}", file=sys.stderr)
            return 1
        try:
            rendered = render_export_manifest(build_export_manifest(root, matrix))
        except AuditError as exc:
            print(f"ip-provenance-audit: {exc}", file=sys.stderr)
            return 1
        (root / EXPORT_MANIFEST_PATH).write_text(rendered, encoding="utf-8")
        print(f"ip-provenance-audit: wrote {EXPORT_MANIFEST_PATH}")
        return 0
    if args.ref is not None and not safe_ref(args.ref):
        report = make_report(args.ref, args.release_strict, [], [], ["invalid git ref"], [])
    else:
        report = run_audit(args.root, args.ref, args.release_strict, args.export)
    rendered = json.dumps(report, indent=2, sort_keys=True) + "\n"
    if args.output:
        try:
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(rendered, encoding="utf-8")
        except OSError:
            print("ip-provenance-audit: cannot write report", file=sys.stderr)
            return 1
    print(f"ip-provenance-audit: {report['result']}")
    for message in report["errors"]:
        print(f"ip-provenance-audit: {message}", file=sys.stderr)
    return 0 if report["result"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
