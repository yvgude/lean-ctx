#!/usr/bin/env python3
"""Check the public Rust tree against the documented open-core boundary.

The classification document is optional during staged rollout. Import,
protocol-freeze, source-injection, and strategic-data checks still run when it
is absent.
"""

from __future__ import annotations

import argparse
import fnmatch
import hashlib
import json
import os
import re
import stat
import sys
from pathlib import Path
from typing import Iterable, List, Optional, Sequence, Tuple


ROOT = Path(__file__).resolve().parents[1]
CLASSIFICATION_PATH = ROOT / "docs/internal/architecture/MODULE_CLASSIFICATION.md"
RUST_SOURCE_ROOT = ROOT / "rust/src"
RUST_CRATES_ROOT = ROOT / "rust/crates"
MAX_RUST_FILES = 10_000
MAX_RUST_ENTRIES = 100_000
MAX_RUST_SOURCE_BYTES = 4 * 1024 * 1024
MAX_RUST_TOTAL_BYTES = 128 * 1024 * 1024
MAX_RUST_MACROS = 2_048
MAX_MACRO_ARGUMENT_BYTES = 8_192
MAX_RUST_USE_BYTES = 8_192
# Treat every non-ASCII code point as a possible Rust XID continuation. This
# deliberately over-approximates Unicode identifier syntax so boundaries can
# never split a valid identifier merely because Python and Rust use different
# Unicode table versions.
RUST_IDENTIFIER_CONTINUE = r"\w\u0080-\U0010ffff"
RUST_IDENTIFIER_LEFT = r"(?<![" + RUST_IDENTIFIER_CONTINUE + r"])"
RUST_IDENTIFIER_RIGHT = r"(?![" + RUST_IDENTIFIER_CONTINUE + r"])"
RUST_PATH_LEFT = r"(?<![" + RUST_IDENTIFIER_CONTINUE + r":])"
RUST_IDENTIFIER = (
    r"(?:r#)?[^\W\d]"
    r"(?:\w|[\u0300-\u036f\u1ab0-\u1aff\u1dc0-\u1dff\u20d0-\u20ff\ufe20-\ufe2f])*"
)

CLASS_TOKEN = re.compile(
    r"\b(?:class|classification)\s*[:=\-]?\s*([A-E])\b", re.IGNORECASE
)
INLINE_PATH = re.compile(r"(?:^|\s)(rust/(?:src|crates)/[^\s|)`]+)")
PRIVATE_IMPORT = re.compile(
    RUST_IDENTIFIER_LEFT
    + r"(?:"
    r"(?:r#)?lean[_-]?ctx[_-]?(?:cloud|enterprise|private)|"
    r"(?:r#)?leanctx[_-]?(?:cloud|enterprise|private)|"
    r"(?:r#)?(?:private|enterprise|proprietary|commercial|strategic_data)"
    r")(?:$|::|\s+as\b)",
    re.IGNORECASE,
)
PRIVATE_QUALIFIED = re.compile(
    RUST_IDENTIFIER_LEFT
    + r"(?:::)?(?:"
    r"(?:r#)?lean[_-]?ctx[_-]?(?:cloud|enterprise|private)|"
    r"(?:r#)?leanctx[_-]?(?:cloud|enterprise|private)"
    r")(?:::|" + RUST_IDENTIFIER_RIGHT + r")",
    re.IGNORECASE,
)
PRIVATE_LOCAL_QUALIFIED = re.compile(
    RUST_IDENTIFIER_LEFT
    + r"(?:r#)?(?:private|enterprise|proprietary|commercial|strategic_data)\s*::",
    re.IGNORECASE,
)
PRIVATE_EXTERN = re.compile(
    RUST_IDENTIFIER_LEFT
    + r"extern\s+crate\s+(?:"
    r"(?:r#)?lean[_-]?ctx[_-]?(?:cloud|enterprise|private)|"
    r"(?:r#)?leanctx[_-]?(?:cloud|enterprise|private)|"
    r"(?:r#)?(?:private|enterprise|proprietary|commercial|strategic_data)"
    r")"
    + RUST_IDENTIFIER_RIGHT
    + (r"(?:\s+as\s+%s)?" % RUST_IDENTIFIER),
    re.IGNORECASE,
)
PRIVATE_USE_ALIAS = re.compile(
    RUST_IDENTIFIER_LEFT
    + r"use\s+(?:r#)?"
    r"(?:private|enterprise|proprietary|commercial|strategic_data)"
    r"(?:\s*::|\s+as\b)",
    re.IGNORECASE,
)
APPROVED_OSS_ENTERPRISE_IMPORTS = {
    "rust/src/cli/mod.rs": {"pub(crate) use enterprise::cmd_enterprise;"},
    "rust/src/core/config/mod.rs": {"pub use enterprise::EnterpriseConfig;"},
}

BENCHMARK_CORPUS_PATH = re.compile(
    r"(?:^|/)(?:benchmark[_-]?(?:corpus|dataset)|"
    r"private[_-]?benchmark|customer[_-]?benchmark)(?:[._/-]|$)",
    re.IGNORECASE,
)
PROVIDER_RATE_PATH = re.compile(
    r"(?:^|/)(?:provider[_-]?(?:rate|rates|pricing|prices|costs)|"
    r"model[_-]?rates|rate[_-]?card|pricing[_-]?table)(?:[._/-]|$)",
    re.IGNORECASE,
)
BENCHMARK_CORPUS_DATA = re.compile(
    r"\b(?:private|customer|proprietary)\s+benchmark\s+(?:corpus|dataset)\b|"
    r"\bbenchmark[_-]?(?:corpus|dataset)\b",
    re.IGNORECASE,
)
PROVIDER_RATE_DATA = re.compile(
    r"(?:\"(?:provider|model)_(?:rate|rates|pricing|price|prices)\"\s*:|"
    r"\b(?:provider|model)_(?:rate|rates|pricing|price|prices)_micros\b|"
    r"\b(?:provider|model)_(?:rate|rates|pricing|price|prices)\s*=|"
    r"\b(?:input|output|cached|reasoning)_rate_micros\b)",
    re.IGNORECASE,
)


class ClassificationRule:
    __slots__ = ("pattern", "class_name", "line_number")

    def __init__(self, pattern: str, class_name: str, line_number: int):
        self.pattern = pattern
        self.class_name = class_name
        self.line_number = line_number


def relative_path(path: Path) -> str:
    """Return a stable POSIX path relative to the repository root."""

    return path.relative_to(ROOT).as_posix()


def rust_modules() -> List[Path]:
    """Enumerate every Rust module in rust/src and rust/crates."""

    modules: List[Path] = []
    for root in (RUST_SOURCE_ROOT, RUST_CRATES_ROOT):
        if root.is_dir():
            modules.extend(path for path in root.rglob("*.rs") if path.is_file())
    return sorted(modules, key=relative_path)


def public_path(path: Path) -> bool:
    """Return whether a path is in the public Rust surface checked here."""

    name = relative_path(path)
    return name.startswith("rust/src/") or name.startswith(("rust/crates/lean-ctx-", "rust/crates/lean-ctx/"))


def normalize_pattern(candidate: str) -> Optional[str]:
    """Normalize a classification path/glob into a repository-relative form."""

    value = candidate.strip().strip("`\"'")
    value = re.sub(r"^\./", "", value).replace("\\", "/")
    value = value.rstrip(",;:)")
    if not value:
        return None
    if value.startswith(str(ROOT).replace("\\", "/") + "/"):
        value = value[len(str(ROOT).replace("\\", "/")) + 1 :]
    if value.startswith("src/") or value.startswith("crates/"):
        value = "rust/" + value
    if not value.startswith("rust/"):
        return None
    return value


def path_candidates(line: str) -> List[str]:
    """Extract path/glob cells from a Markdown classification line."""

    candidates: List[str] = []
    candidates.extend(re.findall(r"`([^`]+)`", line))
    candidates.extend(INLINE_PATH.findall(line))
    if "|" in line:
        candidates.extend(cell.strip() for cell in line.split("|") if cell.strip())
    normalized: List[str] = []
    for candidate in candidates:
        match = re.search(r"(?:^|\s)(rust/(?:src|crates)/[^\s|)`]+)", candidate)
        value = match.group(1) if match else candidate
        normalized_value = normalize_pattern(value)
        if normalized_value and normalized_value not in normalized:
            normalized.append(normalized_value)
    return normalized


def class_from_line(line: str) -> Optional[str]:
    """Extract an explicit A-E class from a heading, list item, or table row."""

    match = CLASS_TOKEN.search(line)
    if match:
        return match.group(1).upper()
    heading = re.match(r"^\s*#{1,6}\s*(?:class\s*)?([A-E])(?:\s|[-—:]|$)", line, re.I)
    if heading:
        return heading.group(1).upper()
    bold = re.search(r"\*\*([A-E])\*\*", line, re.I)
    if bold:
        return bold.group(1).upper()
    if "|" in line:
        for cell in line.split("|"):
            if re.fullmatch(r"\s*([A-E])\s*", cell, re.IGNORECASE):
                return cell.strip().upper()
    return None


def parse_classification_document(
    path: Path,
) -> Tuple[List[ClassificationRule], List[str]]:
    """Parse both explicit rows and class-scoped path lists."""

    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeError) as error:
        return [], ["cannot read classification document: %s" % error]

    rules: List[ClassificationRule] = []
    errors: List[str] = []
    current_class: Optional[str] = None
    for line_number, line in enumerate(lines, start=1):
        heading = re.match(r"^\s*#{1,6}\s+.*", line)
        explicit_class = class_from_line(line)
        if heading and explicit_class:
            current_class = explicit_class
        candidates = path_candidates(line)
        if not candidates:
            continue
        selected_class = explicit_class or current_class
        if selected_class is None:
            errors.append(
                "classification path without class at line %d: %s"
                % (line_number, line.strip())
            )
            continue
        for pattern in candidates:
            rules.append(ClassificationRule(pattern, selected_class, line_number))
    return rules, errors


def pattern_matches(pattern: str, path_name: str) -> bool:
    """Match exact paths, directory rules, and ordinary Markdown globs."""

    alternatives = (path_name, path_name.removeprefix("rust/"))
    for candidate in alternatives:
        if fnmatch.fnmatchcase(candidate, pattern):
            return True
        if candidate == pattern or candidate.startswith(pattern.rstrip("/") + "/"):
            return True
    return False


def best_classification(path: Path, rules: Sequence[ClassificationRule]) -> Optional[str]:
    """Choose the most-specific matching rule, preserving deterministic output."""

    name = relative_path(path)
    matches = [rule for rule in rules if pattern_matches(rule.pattern, name)]
    if not matches:
        return None
    matches.sort(key=lambda rule: (len(rule.pattern.replace("*", "")), rule.line_number), reverse=True)
    return matches[0].class_name


def check_classifications(modules: Sequence[Path]) -> List[str]:
    """Check module coverage and reject D/E assignments in public Rust paths."""

    if not CLASSIFICATION_PATH.exists():
        return []

    rules, parse_errors = parse_classification_document(CLASSIFICATION_PATH)
    violations = ["[classification] %s" % error for error in parse_errors]
    if not rules:
        violations.append(
            "[classification] no usable path/class rules found in %s"
            % relative_path(CLASSIFICATION_PATH)
        )
    for module in modules:
        class_name = best_classification(module, rules)
        name = relative_path(module)
        if class_name is None:
            violations.append("[classification] %s has no A-E classification" % name)
        elif class_name in ("D", "E") and public_path(module):
            violations.append(
                "[public-private-boundary] %s is Class %s in a public path"
                % (name, class_name)
            )
    return violations


def check_private_imports(modules: Sequence[Path]) -> List[str]:
    """Reject imports from recognizable private namespaces in public modules."""

    violations: List[str] = []
    for module in modules:
        if not public_path(module):
            continue
        try:
            lines = module.read_text(encoding="utf-8").splitlines()
        except (OSError, UnicodeError) as error:
            violations.append("[read] %s: %s" % (relative_path(module), error))
            continue
        for line_number, line in enumerate(lines, start=1):
            match = re.match(r"^\s*(?:pub(?:\([^)]*\))?\s+)?use\s+([^;]+);", line)
            if match and PRIVATE_IMPORT.search(match.group(1)):
                violations.append(
                    "[private-import] %s:%d imports private namespace: %s"
                    % (relative_path(module), line_number, match.group(1).strip())
                )
    return violations


def public_files() -> Iterable[Path]:
    """Yield files under the public source and public OCLA crate roots."""

    roots = [RUST_SOURCE_ROOT]
    if RUST_CRATES_ROOT.is_dir():
        roots.extend(sorted(RUST_CRATES_ROOT.glob("lean-ctx-*")))
    for root in roots:
        if not root.is_dir():
            continue
        for path in sorted(root.rglob("*"), key=relative_path):
            if path.is_file():
                yield path


def check_strategic_data() -> List[str]:
    """Reject obvious private benchmark corpora and provider-rate data."""

    violations: List[str] = []
    for path in public_files():
        name = relative_path(path)
        reason: Optional[str] = None
        if BENCHMARK_CORPUS_PATH.search(name):
            reason = "benchmark corpus/dataset path"
        elif PROVIDER_RATE_PATH.search(name):
            reason = "provider rate/pricing path"
        else:
            try:
                if path.stat().st_size > 4 * 1024 * 1024:
                    continue
                lines = path.read_text(encoding="utf-8").splitlines()
            except (OSError, UnicodeError):
                continue
            for line in lines:
                if BENCHMARK_CORPUS_DATA.search(line):
                    reason = "benchmark corpus/dataset content"
                    break
                if PROVIDER_RATE_DATA.search(line):
                    reason = "provider rate/pricing data"
                    break
        if reason:
            violations.append("[strategic-data] %s contains %s" % (name, reason))
    return violations


MANIFEST_PATH = ROOT / "security/public-protocol-surface-freeze-v1.json"
MANIFEST_SCHEMA = "leanctx.public-protocol-surface-freeze/v1"
FROZEN_SURFACES = ("auto_routing", "control_plane", "fleet_control", "rollout", "value_share")
MANIFEST_BYTES = 256 * 1024
RELEASE_RE = re.compile(r"^v[0-9]+(?:\.[0-9]+)*$")
RUST_USE = re.compile(
    r"(?s)" + RUST_IDENTIFIER_LEFT + r"(?:pub(?:\([^)]*\))?\s+)?use\s+[^;]+;"
)
RUST_RAW_STRING = re.compile(r'(?:br|cr|r)(#+)?"')
RUST_PUBLIC_ITEM = re.compile(
    r"^pub\s+(?:(?:async|unsafe)\s+)?(struct|enum|trait|fn|type|const|static|mod)\s+"
    r"([A-Za-z_][A-Za-z0-9_]*)\b"
)


class ManifestError(ValueError):
    """A freeze manifest is unsafe to use as an allowlist."""


def canonical_json(value: object) -> bytes:
    """Serialize reports without filesystem, hash, or insertion-order noise."""

    return (json.dumps(value, ensure_ascii=True, sort_keys=True, separators=(",", ":")) + "\n").encode(
        "utf-8"
    )


def _unique_object(pairs: List[Tuple[str, object]]) -> dict:
    result = {}
    for key, value in pairs:
        if key in result:
            raise ManifestError("duplicate JSON key: %s" % key)
        result[key] = value
    return result


def _exact_keys(value: object, keys: Sequence[str], label: str) -> None:
    if not isinstance(value, dict) or set(value) != set(keys):
        raise ManifestError("%s must contain exactly %s" % (label, sorted(keys)))


def _string(value: object, label: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ManifestError("%s must be a non-empty string" % label)
    return value


def _safe_relative(value: object, label: str) -> str:
    path = _string(value, label)
    if "\\" in path or path.startswith("/") or "//" in path:
        raise ManifestError("%s must be a repository-relative POSIX path" % label)
    parts = path.split("/")
    if any(part in {"", ".", ".."} for part in parts):
        raise ManifestError("%s must not escape the repository" % label)
    return "/".join(parts)


def _bool(value: object, label: str) -> bool:
    if type(value) is not bool:
        raise ManifestError("%s must be boolean" % label)
    return value


def _sorted_unique_strings(value: object, label: str, allow_empty: bool = False) -> List[str]:
    if not isinstance(value, list) or (not allow_empty and not value):
        raise ManifestError("%s must be a non-empty list" % label)
    strings = [_string(item, "%s entry" % label) for item in value]
    if len(set(strings)) != len(strings):
        raise ManifestError("%s contains duplicate entries" % label)
    if strings != sorted(strings):
        raise ManifestError("%s must be sorted" % label)
    return strings


def _validate_manifest(value: object) -> dict:
    top_keys = (
        "schema_version",
        "policy_source_path",
        "owner",
        "status",
        "experimental",
        "v1",
        "review_release",
        "removal_not_before_release",
        "surfaces",
    )
    _exact_keys(value, top_keys, "manifest")
    if value["schema_version"] != MANIFEST_SCHEMA:
        raise ManifestError("unsupported schema_version")
    if value["policy_source_path"] != "security/public-protocol-surface-freeze-v1.json":
        raise ManifestError("policy_source_path must name the committed freeze manifest")
    metadata = {
        "owner": _string(value["owner"], "manifest.owner"),
        "status": _string(value["status"], "manifest.status"),
        "experimental": _bool(value["experimental"], "manifest.experimental"),
        "v1": _bool(value["v1"], "manifest.v1"),
        "review_release": _string(value["review_release"], "manifest.review_release"),
        "removal_not_before_release": _string(
            value["removal_not_before_release"], "manifest.removal_not_before_release"
        ),
    }
    if metadata["status"] != "experimental-non-v1" or not metadata["experimental"] or metadata["v1"]:
        raise ManifestError("manifest must mark every surface experimental and non-v1")
    for key in ("review_release", "removal_not_before_release"):
        if not RELEASE_RE.fullmatch(metadata[key]):
            raise ManifestError("manifest.%s is not a release identifier" % key)

    surfaces = value["surfaces"]
    if not isinstance(surfaces, dict) or set(surfaces) != set(FROZEN_SURFACES):
        raise ManifestError("surfaces must contain exactly %s" % list(FROZEN_SURFACES))

    surface_keys = (
        "owner",
        "status",
        "experimental",
        "v1",
        "review_release",
        "removal_not_before_release",
        "module_path",
        "module_sha256",
        "module_roots",
        "root_reexports",
        "exported_symbols",
        "approved_consumers",
    )
    for surface_name in FROZEN_SURFACES:
        surface = surfaces[surface_name]
        _exact_keys(surface, surface_keys, "surfaces.%s" % surface_name)
        for key, expected in metadata.items():
            if surface[key] != expected:
                raise ManifestError("surfaces.%s.%s disagrees with manifest metadata" % (surface_name, key))
        module_path = _safe_relative(surface["module_path"], "surfaces.%s.module_path" % surface_name)
        expected_module = "rust/crates/lean-ctx-protocol/src/%s.rs" % surface_name
        if module_path != expected_module:
            raise ManifestError("surfaces.%s.module_path is not the protocol module" % surface_name)
        module_sha256 = _string(
            surface["module_sha256"], "surfaces.%s.module_sha256" % surface_name
        )
        if not re.fullmatch(r"[0-9a-f]{64}", module_sha256):
            raise ManifestError(
                "surfaces.%s.module_sha256 must be lowercase SHA-256" % surface_name
            )
        for collection_name, allow_empty in (("module_roots", False), ("root_reexports", False), ("approved_consumers", True)):
            collection = surface[collection_name]
            if not isinstance(collection, list) or (not collection and not allow_empty):
                raise ManifestError("surfaces.%s.%s must be a list" % (surface_name, collection_name))
            seen_paths = set()
            for index, item in enumerate(collection):
                _exact_keys(item, ("path", "declarations") if collection_name == "module_roots" else ("path", "statements"), "%s.%s[%d]" % (surface_name, collection_name, index))
                path = _safe_relative(item["path"], "%s.%s[%d].path" % (surface_name, collection_name, index))
                if path in seen_paths:
                    raise ManifestError("surfaces.%s.%s repeats %s" % (surface_name, collection_name, path))
                seen_paths.add(path)
                field = "declarations" if collection_name == "module_roots" else "statements"
                _sorted_unique_strings(item[field], "%s.%s[%d].%s" % (surface_name, collection_name, index, field))
            if [item["path"] for item in collection] != sorted(item["path"] for item in collection):
                raise ManifestError("surfaces.%s.%s must be sorted by path" % (surface_name, collection_name))
        symbols = surface["exported_symbols"]
        if not isinstance(symbols, list) or not symbols:
            raise ManifestError("surfaces.%s.exported_symbols must be a non-empty list" % surface_name)
        parsed_symbols = []
        for index, item in enumerate(symbols):
            _exact_keys(item, ("kind", "name"), "surfaces.%s.exported_symbols[%d]" % (surface_name, index))
            kind = _string(item["kind"], "exported symbol kind")
            name = _string(item["name"], "exported symbol name")
            if kind not in {"const", "enum", "fn", "mod", "static", "struct", "trait", "type", "use"}:
                raise ManifestError("unsupported exported symbol kind: %s" % kind)
            parsed_symbols.append((kind, name))
        if len(set(parsed_symbols)) != len(parsed_symbols) or parsed_symbols != sorted(parsed_symbols):
            raise ManifestError("surfaces.%s.exported_symbols must be sorted and unique" % surface_name)
    return value


def load_manifest(path: Path) -> dict:
    """Read and validate a freeze manifest, rejecting ambiguity and truncation."""

    try:
        metadata = os.lstat(path)
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
            raise ManifestError("manifest is not a regular file")
        if metadata.st_size > MANIFEST_BYTES:
            raise ManifestError("manifest exceeds byte bound")
        raw = path.read_bytes()
    except ManifestError:
        raise
    except (OSError, UnicodeError) as error:
        raise ManifestError("cannot read manifest") from error
    if len(raw) > MANIFEST_BYTES:
        raise ManifestError("manifest exceeds byte bound")
    try:
        value = json.loads(raw.decode("utf-8"), object_pairs_hook=_unique_object)
    except ManifestError:
        raise
    except (UnicodeDecodeError, TypeError, ValueError, RecursionError) as error:
        raise ManifestError("manifest is not valid UTF-8 JSON: %s" % error) from error
    return _validate_manifest(value)


def _root_path(root: Path, relative: str, label: str) -> Path:
    candidate = root / relative
    current = root
    try:
        for part in Path(relative).parts:
            current /= part
            metadata = os.lstat(current)
            if stat.S_ISLNK(metadata.st_mode):
                raise ManifestError("%s uses a symlink path" % label)
        metadata = os.lstat(candidate)
    except ManifestError:
        raise
    except (OSError, ValueError) as error:
        raise ManifestError("%s is unavailable" % label) from error
    if not stat.S_ISREG(metadata.st_mode):
        raise ManifestError("%s is not a regular file" % label)
    return candidate


def _relative(root: Path, path: Path) -> str:
    return path.relative_to(root).as_posix()


def _rust_files_unchecked(root: Path) -> List[Path]:
    paths = []
    source_roots = []
    entry_count = 0
    source_root = root / "rust/src"
    crates_root = root / "rust/crates"
    for candidate in (root / "rust", source_root, crates_root):
        if os.path.lexists(candidate):
            metadata = os.lstat(candidate)
            if stat.S_ISLNK(metadata.st_mode):
                raise ManifestError("Rust source root uses a symlink path: %s" % _relative(root, candidate))
            if not stat.S_ISDIR(metadata.st_mode):
                raise ManifestError("Rust source root is not a directory: %s" % _relative(root, candidate))
    if os.path.lexists(source_root):
        source_roots.append(source_root)
    if os.path.lexists(crates_root):
        with os.scandir(crates_root) as entries:
            crate_entries = sorted(entries, key=lambda entry: entry.name)
        for entry in crate_entries:
            entry_count += 1
            if entry_count > MAX_RUST_ENTRIES:
                raise ManifestError("Rust source entry count exceeds limit")
            if not entry.name.startswith("lean-ctx-"):
                continue
            candidate = Path(entry.path)
            metadata = entry.stat(follow_symlinks=False)
            if stat.S_ISLNK(metadata.st_mode):
                raise ManifestError("Rust source uses a symlink path: %s" % _relative(root, candidate))
            if stat.S_ISDIR(metadata.st_mode):
                source_roots.append(candidate)
    for source_root in source_roots:
        pending = [source_root]
        while pending:
            directory = pending.pop()
            with os.scandir(directory) as entries:
                children = sorted(entries, key=lambda entry: entry.name)
            for entry in children:
                entry_count += 1
                if entry_count > MAX_RUST_ENTRIES:
                    raise ManifestError("Rust source entry count exceeds limit")
                path = Path(entry.path)
                metadata = entry.stat(follow_symlinks=False)
                if stat.S_ISLNK(metadata.st_mode):
                    raise ManifestError("Rust source uses a symlink path: %s" % _relative(root, path))
                if stat.S_ISDIR(metadata.st_mode):
                    pending.append(path)
                elif path.suffix == ".rs" and stat.S_ISREG(metadata.st_mode):
                    paths.append(path)
                    if len(paths) > MAX_RUST_FILES:
                        raise ManifestError("Rust source file count exceeds limit")
    return sorted(paths, key=lambda path: _relative(root, path))


def _rust_files(root: Path) -> List[Path]:
    try:
        return _rust_files_unchecked(root)
    except ManifestError:
        raise
    except OSError as error:
        raise ManifestError("cannot enumerate Rust source tree") from error


def _read_source(path: Path) -> str:
    try:
        metadata = os.lstat(path)
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
            raise ManifestError("Rust source is not a regular file: %s" % path.name)
        if metadata.st_size > MAX_RUST_SOURCE_BYTES:
            raise ManifestError("Rust source exceeds size limit: %s" % path.name)
        raw = path.read_bytes()
        if len(raw) > MAX_RUST_SOURCE_BYTES:
            raise ManifestError("Rust source exceeds size limit: %s" % path.name)
        return raw.decode("utf-8")
    except ManifestError:
        raise
    except (OSError, UnicodeError) as error:
        raise ManifestError("cannot read Rust source: %s" % path.name) from error


def _normalize_statement(statement: str) -> str:
    return " ".join(statement.strip().split())


def _mask_rust_non_code(source: str) -> str:
    """Mask comments and literals while preserving source offsets/newlines."""

    masked = list(source)
    length = len(source)

    def blank(start: int, end: int) -> None:
        for index in range(start, end):
            if source[index] not in "\r\n":
                masked[index] = " "

    def quoted_end(start: int, quote: str) -> int:
        index = start + 1
        while index < length:
            if source[index] == "\\":
                index += 2
            elif source[index] == quote:
                return index + 1
            else:
                index += 1
        return length

    index = 0
    while index < length:
        if source.startswith("//", index):
            end = source.find("\n", index)
            end = length if end < 0 else end
            blank(index, end)
            index = end
            continue
        if source.startswith("/*", index):
            end = index + 2
            depth = 1
            while end < length and depth:
                if source.startswith("/*", end):
                    depth += 1
                    end += 2
                elif source.startswith("*/", end):
                    depth -= 1
                    end += 2
                else:
                    end += 1
            blank(index, end)
            index = end
            continue

        raw = RUST_RAW_STRING.match(source, index)
        if raw:
            terminator = '"%s' % (raw.group(1) or "")
            end = source.find(terminator, raw.end())
            end = length if end < 0 else end + len(terminator)
            blank(index, end)
            index = end
            continue

        if source[index] == '"' or source[index : index + 2] in {"b\"", "c\""}:
            start = index
            quote = index if source[index] == '"' else index + 1
            end = quoted_end(quote, '"')
            blank(start, end)
            index = end
            continue

        if source[index] == "'":
            end = index + 1
            if end < length and source[end] == "\\":
                end += 2
            elif end < length:
                end += 1
            if end < length and source[end] == "'":
                blank(index, end + 1)
                index = end + 1
                continue

        index += 1
    return "".join(masked)


def _rust_views(source: str) -> Tuple[List[Tuple[int, str]], str]:
    """Parse imports and executable code from one lexical masking pass."""

    masked = _mask_rust_non_code(source)
    matches = list(RUST_USE.finditer(masked))
    if any(
        len(source[match.start() : match.end()].encode("utf-8")) > MAX_RUST_USE_BYTES
        for match in matches
    ):
        raise ManifestError("Rust use declaration exceeds size limit")
    statements = [
        (
            source.count("\n", 0, match.start()) + 1,
            _normalize_statement(source[match.start() : match.end()]),
        )
        for match in matches
    ]
    code = list(masked)
    for match in matches:
        for index in range(match.start(), match.end()):
            if code[index] not in "\r\n":
                code[index] = " "
    return statements, "".join(code)


def _use_statements(source: str) -> List[Tuple[int, str]]:
    return _rust_views(source)[0]


def _code_without_use_statements(source: str) -> str:
    """Return code with comments, literals, and use declarations removed."""

    return _rust_views(source)[1]


def _surface_reexport(statement: str, surface: str) -> bool:
    if not statement.startswith("pub use "):
        return False
    target = statement[len("pub use ") :].removesuffix(";").strip()
    return (
        target in {surface, "lean_ctx_protocol::" + surface}
        or target.startswith(surface + "::")
        or target.startswith("lean_ctx_protocol::" + surface + "::")
    )


def _top_level_use_items(value: str) -> List[str]:
    """Split a braced use list without descending into nested groups."""

    items = []
    start = 0
    depth = 0
    for index, character in enumerate(value):
        if character in "{[(":
            depth += 1
        elif character in "}])":
            depth = max(0, depth - 1)
        elif character == "," and depth == 0:
            items.append(value[start:index].strip())
            start = index + 1
    items.append(value[start:].strip())
    return [item for item in items if item]


def _use_target(statement: str) -> str:
    target = statement.split("use ", 1)[1].removesuffix(";").strip()
    return _normalize_statement(_mask_rust_non_code(target))


def _expand_use_tree(value: str, prefix: str = "") -> List[str]:
    """Expand Rust use-tree groups into independent paths."""

    value = value.strip()
    if value.startswith("{") and value.endswith("}"):
        return [
            leaf
            for item in _top_level_use_items(value[1:-1])
            for leaf in _expand_use_tree(item, prefix)
        ]

    depth = 0
    group_start = None
    for index, character in enumerate(value):
        if character == "{" and depth == 0:
            group_start = index
            break
        if character in "([":
            depth += 1
        elif character in ")]":
            depth = max(0, depth - 1)
    if group_start is not None:
        depth = 0
        group_end = None
        for index in range(group_start, len(value)):
            if value[index] == "{":
                depth += 1
            elif value[index] == "}":
                depth -= 1
                if depth == 0:
                    group_end = index
                    break
        if group_end is not None and not value[group_end + 1 :].strip():
            base = value[:group_start].strip().removesuffix("::").strip()
            joined = "::".join(part for part in (prefix, base) if part)
            return [
                leaf
                for item in _top_level_use_items(value[group_start + 1 : group_end])
                for leaf in _expand_use_tree(item, joined)
            ]
    return ["::".join(part for part in (prefix, value) if part)]


def _use_leaves(statement: str) -> List[str]:
    return _expand_use_tree(_use_target(statement))


def _identifier(value: str) -> Optional[str]:
    candidate = value.strip().removeprefix("r#")
    if not candidate or candidate.startswith("$"):
        return None
    return candidate


def _use_leaf(leaf: str) -> Tuple[List[str], Optional[str], bool]:
    alias = None
    match = re.search(r"\s+as\s+([^\s,;{}]+)\s*$", leaf)
    if match:
        alias = _identifier(match.group(1))
        leaf = leaf[: match.start()].strip()
    compact = re.sub(r"\s+", "", leaf).removeprefix("::")
    raw_components = compact.split("::") if compact else []
    glob = bool(raw_components and raw_components[-1] == "*")
    components = [
        identifier
        for component in raw_components
        if component != "*"
        for identifier in [_identifier(component)]
        if identifier is not None
    ]
    return components, alias, glob


def _exact_name(value: str, name: str) -> bool:
    return bool(
        re.search(
            RUST_IDENTIFIER_LEFT
            + r"(?:r#)?%s" % re.escape(name)
            + RUST_IDENTIFIER_RIGHT,
            value,
        )
    )


def _surface_import(
    statement: str,
    surface: str,
    root_symbols: Sequence[str] = (),
) -> bool:
    if "use " not in statement:
        return False
    roots = {"crate", "self", "super", "lean_ctx_protocol"}
    for leaf in _use_leaves(statement):
        components, _, glob = _use_leaf(leaf)
        if not components or components[0] not in roots:
            continue
        if components == ["lean_ctx_protocol"]:
            return True
        if surface in components or (len(components) > 1 and components[1] in root_symbols):
            return True
        if glob and len(components) == 1 and components[0] in {"crate", "lean_ctx_protocol"}:
            return True
    return False


def _root_reexport_symbols(surface: str, spec: dict) -> List[str]:
    """Return symbols reachable from the protocol crate root for a surface."""

    root_path = "rust/crates/lean-ctx-protocol/src/lib.rs"
    symbols = sorted(item["name"] for item in spec["exported_symbols"])
    selected = set()
    for item in spec["root_reexports"]:
        if item["path"] != root_path:
            continue
        for statement in item["statements"]:
            target = statement[len("pub use ") :].removesuffix(";").strip()
            if target in {surface + "::*", "lean_ctx_protocol::" + surface + "::*"}:
                return symbols
            if target.startswith(surface + "::{") or target.startswith("lean_ctx_protocol::" + surface + "::{"):
                selected.update(
                    symbol
                    for symbol in symbols
                    if re.search(r"(?:^|[,{\s])%s(?:\s|[,}])" % re.escape(symbol), target)
                )
    return sorted(selected)


def _surface_references(
    source: str,
    surface: str,
    root_symbols: Sequence[str],
    code: Optional[str] = None,
    same_protocol_crate: bool = False,
) -> List[Tuple[int, str]]:
    """Find qualified frozen paths outside import declarations."""

    if code is None:
        code = _code_without_use_statements(source)
    findings = []
    qualified = re.compile(
        RUST_IDENTIFIER_LEFT
        + r"(?:::)?(?:crate|self|super|(?:r#)?lean_ctx_protocol)"
        r"(?P<tail>(?:\s*::\s*(?:r#)?[^:\s<>,;(){}\[\]=!]+)+)"
    )
    for match in qualified.finditer(code):
        components, _, _ = _use_leaf(match.group("tail").lstrip(": "))
        if surface in components or (components and components[0] in root_symbols):
            findings.append((source.count("\n", 0, match.start()) + 1, match.group(0)))
    external_target = r"(?:r#)?lean_ctx_protocol"
    if same_protocol_crate:
        external_target = r"(?:%s|self)" % external_target
    external = re.compile(
        (
            RUST_IDENTIFIER_LEFT
            + r"extern\s+crate\s+%s"
            + RUST_IDENTIFIER_RIGHT
            + r"(?:\s+as\s+%s)?"
        )
        % (external_target, RUST_IDENTIFIER)
    )
    findings.extend(
        (source.count("\n", 0, match.start()) + 1, match.group(0))
        for match in external.finditer(code)
    )
    return sorted(findings, key=lambda item: (item[0], item[1]))


def _alias_surface_references(
    source: str,
    statements: Sequence[Tuple[int, str]],
    surface: str,
    root_symbols: Sequence[str],
    code: str,
    same_protocol_crate: bool = False,
    broad_context: bool = True,
) -> List[Tuple[int, str]]:
    roots = {"crate", "self", "super", "lean_ctx_protocol"}
    parsed = [
        (line, statement, _use_leaf(leaf))
        for line, statement in statements
        for leaf in _use_leaves(statement)
    ]
    aliases = {}
    rooted_globs = set()
    for _ in range(len(parsed) + 1):
        changed = False
        for line, statement, (components, alias, glob) in parsed:
            if not components:
                continue
            if components[0] in roots:
                canonicals = {tuple(components)}
            elif components[0] in aliases:
                canonicals = {
                    prefix + tuple(components[1:])
                    for prefix in aliases[components[0]]
                }
            else:
                continue
            if glob and broad_context:
                rooted_globs.update(
                    (line, statement, canonical) for canonical in canonicals
                )
            binding = alias
            if binding is None and not glob and len(components) > 1:
                binding = components[-1]
            if binding and binding != "_":
                existing = aliases.setdefault(binding, set())
                additions = canonicals - existing
                if additions:
                    existing.update(additions)
                    changed = True
        if not changed:
            break
    else:
        raise ManifestError("Rust use alias resolution exceeds limit")

    findings = []
    for alias in sorted(aliases):
        pattern = re.compile(
            RUST_IDENTIFIER_LEFT
            + r"(?:r#)?%s"
            r"(?P<tail>(?:\s*::\s*(?:r#)?[^:\s<>,;(){}\[\]=!]+)+)"
            % re.escape(alias)
        )
        for match in pattern.finditer(code):
            components, _, _ = _use_leaf(match.group("tail").lstrip(": "))
            if surface in components or (components and components[0] in root_symbols):
                findings.append((source.count("\n", 0, match.start()) + 1, match.group(0)))

    for line, statement, (components, _, _) in parsed:
        if not components or components[0] not in aliases:
            continue
        if surface in components[1:] or (len(components) > 1 and components[1] in root_symbols):
            findings.append((line, statement))

    for _, statement, canonical in rooted_globs:
        external_glob = canonical == ("lean_ctx_protocol",)
        protocol_glob = same_protocol_crate and canonical in {
            ("crate",),
            ("self",),
            ("super",),
        }
        if not (external_glob or protocol_glob or len(canonical) > 1):
            continue
        surface_pattern = (
            RUST_PATH_LEFT
            + r"(?:r#)?%s" % re.escape(surface)
            + RUST_IDENTIFIER_RIGHT
            + r"\s*::"
        )
        for match in re.finditer(surface_pattern, code):
            findings.append((source.count("\n", 0, match.start()) + 1, match.group(0)))
        if external_glob or protocol_glob:
            for name in root_symbols:
                pattern = (
                    RUST_PATH_LEFT
                    + r"(?:r#)?%s" % re.escape(name)
                    + RUST_IDENTIFIER_RIGHT
                )
                for match in re.finditer(pattern, code):
                    findings.append((source.count("\n", 0, match.start()) + 1, match.group(0)))

    return findings


def _matching_delimiter(
    value: str,
    start: int,
    opening: str,
    closing: str,
    max_length: Optional[int] = None,
) -> Optional[int]:
    depth = 0
    stop = len(value) if max_length is None else min(len(value), start + max_length + 1)
    for index in range(start, stop):
        if value[index] == opening:
            depth += 1
        elif value[index] == closing:
            depth -= 1
            if depth == 0:
                return index
    return None


def _macro_surface_references(
    source: str,
    surface: str,
    root_symbols: Sequence[str],
    masked: Optional[str] = None,
) -> List[Tuple[int, str]]:
    """Resolve local macro metavariables only at matching invocations."""

    if masked is None:
        masked = _mask_rust_non_code(source)
    findings = []
    header = re.compile(
        RUST_IDENTIFIER_LEFT + r"macro_rules!\s*(?:r#)?([^\s{]+)\s*\{"
    )
    variable = re.compile(
        r"(?:\b(?:crate|self|super|(?:r#)?lean_ctx_protocol)\s*::\s*"
        r"\$([^\s:;,(){}\[\]]+)|"
        r"\$([^\s:;,(){}\[\]]+)\s*::)"
    )
    definitions = list(header.finditer(masked))
    if len(definitions) > MAX_RUST_MACROS:
        raise ManifestError("Rust macro definition count exceeds limit")
    resolved = {}
    for definition in definitions:
        body_end = _matching_delimiter(masked, definition.end() - 1, "{", "}")
        if body_end is None:
            continue
        body = masked[definition.end() : body_end]
        variables = {
            name
            for match in variable.finditer(body)
            for name in match.groups()
            if name
        }
        if not variables:
            continue
        resolved.setdefault(definition.group(1), []).append(body_end)
    if not resolved:
        return []

    invocation = re.compile(
        RUST_IDENTIFIER_LEFT + r"(?:r#)?([^\s!({\[]+)\s*!\s*([({\[])"
    )
    calls = list(invocation.finditer(masked))
    if len(calls) > MAX_RUST_MACROS:
        raise ManifestError("Rust macro invocation count exceeds limit")
    for call in calls:
        candidates = [end for end in resolved.get(call.group(1), []) if end < call.start()]
        if not candidates:
            continue
        opening = call.group(2)
        closing = {"(": ")", "{": "}", "[": "]"}[opening]
        arguments_end = _matching_delimiter(
            masked,
            call.end() - 1,
            opening,
            closing,
            MAX_MACRO_ARGUMENT_BYTES,
        )
        if arguments_end is None:
            raise ManifestError("Rust macro invocation exceeds size limit")
        arguments = masked[call.end() : arguments_end]
        if any(_exact_name(arguments, name) for name in [surface, *root_symbols]):
            findings.append(
                (
                    source.count("\n", 0, call.start()) + 1,
                    source[call.start() : arguments_end + 1],
                )
            )
    return findings


def _glob_root_symbol_references(
    source: str,
    statements: Sequence[Tuple[int, str]],
    root_symbols: Sequence[str],
    code: str,
) -> List[Tuple[int, str]]:
    if not any(
        re.fullmatch(
            r"(?:pub(?:\s*\([^)]*\))?\s+)?use\s+super\s*::\s*"
            r"(?:\*|\{\s*\*\s*\})\s*;",
            statement,
        )
        for _, statement in statements
    ):
        return []
    findings = []
    for symbol in root_symbols:
        pattern = re.compile(
            RUST_PATH_LEFT + re.escape(symbol) + RUST_IDENTIFIER_RIGHT
        )
        findings.extend(
            (source.count("\n", 0, match.start()) + 1, match.group(0))
            for match in pattern.finditer(code)
        )
    return findings


def _source_injection_findings(
    root: Path,
    files: Sequence[Path],
    source_cache: dict,
    masked_cache: Optional[dict] = None,
) -> List[str]:
    """Reject Rust source injection that is not already in the audited file set."""

    audited = {path.resolve() for path in files}
    findings = []
    token_patterns = (
        ("include", re.compile(r"\binclude\s*!")),
        ("path", re.compile(r"#\s*\[\s*path\s*=")),
    )
    literal_patterns = {
        "include": re.compile(r'\binclude\s*!\s*\(\s*"([^"\\]*)"\s*\)'),
        "path": re.compile(r'#\s*\[\s*path\s*=\s*"([^"\\]*)"\s*\]'),
    }
    for path in files:
        source = source_cache[path]
        if not re.search(r"\binclude\s*!|#\s*\[\s*path\s*=", source):
            continue
        masked = masked_cache[path] if masked_cache is not None else _mask_rust_non_code(source)
        for kind, token_pattern in token_patterns:
            for token in token_pattern.finditer(masked):
                snippet = source[token.start() : token.start() + 1024]
                literal = literal_patterns[kind].match(snippet)
                line = source.count("\n", 0, token.start()) + 1
                if literal is None:
                    findings.append(
                        "[source-injection] %s:%d uses non-literal %s source"
                        % (_relative(root, path), line, kind)
                    )
                    continue
                target = path.parent / literal.group(1)
                try:
                    relative = target.resolve().relative_to(root.resolve()).as_posix()
                    resolved = _root_path(root, relative, "%s source" % kind).resolve()
                except (ManifestError, OSError, ValueError):
                    findings.append(
                        "[source-injection] %s:%d has unsafe %s source"
                        % (_relative(root, path), line, kind)
                    )
                    continue
                if resolved not in audited:
                    findings.append(
                        "[source-injection] %s:%d %s source is outside audited Rust files: %s"
                        % (_relative(root, path), line, kind, relative)
                    )
    return findings


def _shape_mapping(
    root: Path,
    files: Sequence[Path],
    surface: str,
    kind: str,
    use_cache: Optional[dict] = None,
    source_cache: Optional[dict] = None,
) -> dict:
    mapping = {}
    for path in files:
        source = source_cache.get(path) if source_cache is not None else None
        if source is None:
            source = _read_source(path)
        if kind == "module_roots":
            statements = [
                _normalize_statement(match.group(0))
                for match in re.finditer(r"(?m)^[ \t]*pub\s+mod\s+%s\s*;" % re.escape(surface), source)
            ]
        else:
            use_statements = use_cache[path] if use_cache is not None else _use_statements(source)
            statements = [statement for _, statement in use_statements if _surface_reexport(statement, surface)]
        if statements:
            mapping[_relative(root, path)] = sorted(statements)
    return mapping


def _expected_mapping(root: Path, entries: Sequence[dict], field: str, label: str) -> dict:
    mapping = {}
    for entry in entries:
        path = _safe_relative(entry["path"], "%s.path" % label)
        _root_path(root, path, "%s/%s" % (label, path))
        mapping[path] = sorted(_normalize_statement(statement) for statement in entry[field])
    return mapping


def _shape_findings(
    root: Path,
    files: Sequence[Path],
    surface: str,
    spec: dict,
    use_cache: Optional[dict] = None,
    source_cache: Optional[dict] = None,
) -> List[str]:
    findings = []
    for kind, field in (("module_roots", "declarations"), ("root_reexports", "statements")):
        expected = _expected_mapping(root, spec[kind], field, "%s.%s" % (surface, kind))
        actual = _shape_mapping(root, files, surface, kind, use_cache, source_cache)
        if actual != expected:
            findings.append(
                "[shape] %s %s drift: expected %s; found %s"
                % (surface, kind, canonical_json(expected).decode().strip(), canonical_json(actual).decode().strip())
            )
    module_path = _safe_relative(spec["module_path"], "%s.module_path" % surface)
    module = _root_path(root, module_path, "%s module" % surface)
    module_source = source_cache.get(module) if source_cache is not None else None
    if module_source is None:
        module_source = _read_source(module)
    actual_digest = hashlib.sha256(module_source.encode("utf-8")).hexdigest()
    if actual_digest != spec["module_sha256"]:
        findings.append(
            "[shape] %s module digest drift: expected %s; found %s"
            % (surface, spec["module_sha256"], actual_digest)
        )
    actual_symbols = []
    for line in module_source.splitlines():
        match = RUST_PUBLIC_ITEM.match(line)
        if match:
            actual_symbols.append({"kind": match.group(1), "name": match.group(2)})
    actual_symbols.sort(key=lambda item: (item["kind"], item["name"]))
    if actual_symbols != spec["exported_symbols"]:
        findings.append(
            "[shape] %s exported symbols drift: expected %s; found %s"
            % (
                surface,
                canonical_json(spec["exported_symbols"]).decode().strip(),
                canonical_json(actual_symbols).decode().strip(),
            )
        )
    return findings


def _consumer_findings(
    root: Path,
    files: Sequence[Path],
    surface: str,
    spec: dict,
    source_cache: Optional[dict] = None,
    use_cache: Optional[dict] = None,
    code_cache: Optional[dict] = None,
    masked_cache: Optional[dict] = None,
) -> List[str]:
    approved = {}
    for item in spec["approved_consumers"]:
        path = _safe_relative(item["path"], "%s.approved_consumer.path" % surface)
        _root_path(root, path, "%s approved consumer" % surface)
        approved[path] = {_normalize_statement(statement) for statement in item["statements"]}
    module_path = _safe_relative(spec["module_path"], "%s.module_path" % surface)
    root_symbols = _root_reexport_symbols(surface, spec)
    lexical_allowlist = set(approved)
    lexical_allowlist.add(module_path)
    lexical_allowlist.update(item["path"] for item in spec["module_roots"])
    lexical_allowlist.update(item["path"] for item in spec["root_reexports"])
    actual = {}
    findings = []
    for path in files:
        relative = _relative(root, path)
        if not (relative.startswith("rust/src/") or relative.startswith(("rust/crates/lean-ctx-", "rust/crates/lean-ctx/"))):
            continue
        if relative == module_path:
            continue
        source = source_cache[path] if source_cache is not None else _read_source(path)
        statements = use_cache[path] if use_cache is not None else _use_statements(source)
        for line, statement in statements:
            if _surface_reexport(statement, surface):
                if statement in approved.get(relative, set()):
                    actual.setdefault(relative, []).append((line, statement))
                continue
            if _surface_import(
                statement,
                surface,
                root_symbols,
            ):
                actual.setdefault(relative, []).append((line, statement))
        code = code_cache[path] if code_cache is not None else None
        same_protocol_crate = relative.startswith("rust/crates/lean-ctx-protocol/")
        references = _surface_references(
            source, surface, root_symbols, code, same_protocol_crate
        )
        references.extend(
            _alias_surface_references(
                source,
                statements,
                surface,
                root_symbols,
                code or "",
                same_protocol_crate,
                relative not in lexical_allowlist,
            )
        )
        if same_protocol_crate and relative not in approved:
            references.extend(
                _glob_root_symbol_references(source, statements, root_symbols, code or "")
            )
        if relative not in lexical_allowlist:
            masked = masked_cache[path] if masked_cache is not None else None
            references.extend(
                _macro_surface_references(source, surface, root_symbols, masked)
            )
        for line, reference in references:
            findings.append(
                "[new-consumer] %s %s:%d references frozen surface: %s"
                % (surface, relative, line, reference)
            )
    for path in sorted(actual):
        allowed = approved.get(path, set())
        for line, statement in sorted(actual[path], key=lambda item: (item[0], item[1])):
            if statement not in allowed:
                findings.append(
                    "[new-consumer] %s %s:%d imports frozen surface: %s"
                    % (surface, path, line, statement)
                )
    for path in sorted(approved):
        current = {statement for _, statement in actual.get(path, [])}
        for statement in sorted(approved[path] - current):
            findings.append(
                "[consumer] %s approved consumer missing: %s: %s" % (surface, path, statement)
            )
    return findings


def _private_import_findings(
    root: Path,
    files: Sequence[Path],
    use_cache: Optional[dict] = None,
    code_cache: Optional[dict] = None,
) -> List[str]:
    findings = []
    for path in files:
        statements = use_cache[path] if use_cache is not None else _use_statements(_read_source(path))
        for line, statement in statements:
            target = _mask_rust_non_code(
                statement.split("use ", 1)[1].removesuffix(";").strip()
            )
            if PRIVATE_IMPORT.search(target):
                if statement in APPROVED_OSS_ENTERPRISE_IMPORTS.get(_relative(root, path), set()):
                    continue
                findings.append(
                    "[private-import] %s:%d imports private namespace: %s"
                    % (_relative(root, path), line, target)
                )
        source = _read_source(path)
        code = code_cache.get(path) if code_cache is not None else _code_without_use_statements(source)
        for pattern in (
            PRIVATE_QUALIFIED,
            PRIVATE_LOCAL_QUALIFIED,
            PRIVATE_EXTERN,
            PRIVATE_USE_ALIAS,
        ):
            for match in pattern.finditer(code):
                line = source.count("\n", 0, match.start()) + 1
                findings.append(
                    "[private-import] %s:%d references private namespace: %s"
                    % (_relative(root, path), line, match.group(0))
                )
    return findings


def _needs_consumer_scan(source: str, manifest: dict) -> bool:
    """Skip lexical masking when source cannot mention a frozen/private path."""

    symbols = {
        item["name"]
        for surface in manifest["surfaces"].values()
        for item in surface["exported_symbols"]
    }
    if any(symbol in source for symbol in symbols):
        return True
    if re.search(
        r"\buse\s+(?:::)?(?:crate|self|super|(?:r#)?lean_ctx_protocol)"
        r"(?:\s|/\*[\s\S]{0,256}?\*/)+as\b",
        source,
    ):
        return True
    lowered = source.lower()
    needles = FROZEN_SURFACES + (
        "lean_ctx_protocol",
        "extern crate",
        "lean_ctx_cloud",
        "leanctx_cloud",
        "lean_ctx_enterprise",
        "leanctx_enterprise",
        "lean_ctx_private",
        "leanctx_private",
        "enterprise",
        "private",
        "proprietary",
        "commercial",
        "strategic_data",
    )
    return any(needle in lowered for needle in needles)


def _manifest_path(root: Path, manifest_path: Optional[Path]) -> Path:
    candidate = (root / "security/public-protocol-surface-freeze-v1.json") if manifest_path is None else Path(manifest_path)
    if not candidate.is_absolute():
        candidate = root / candidate
    try:
        candidate = Path(os.path.abspath(candidate))
        relative = candidate.relative_to(root).as_posix()
        return _root_path(root, relative, "manifest")
    except ManifestError:
        raise
    except (OSError, ValueError) as error:
        raise ManifestError("manifest path escapes repository root") from error


def check_repo(root: Path = ROOT, manifest_path: Optional[Path] = None) -> List[str]:
    """Return stable findings; any unreadable or incomplete input fails closed."""

    root = Path(root).resolve()
    if not root.is_dir():
        return ["[root] repository root is unavailable"]
    try:
        policy = _manifest_path(root, manifest_path)
        manifest = load_manifest(policy)
        files = [
            path
            for path in _rust_files(root)
            if _relative(root, path).startswith("rust/src/")
            or _relative(root, path).startswith(("rust/crates/lean-ctx-", "rust/crates/lean-ctx/"))
        ]
        if not files:
            raise ManifestError("no Rust source files found")
        if sum(os.lstat(path).st_size for path in files) > MAX_RUST_TOTAL_BYTES:
            raise ManifestError("Rust source total size exceeds limit")
        source_cache = {}
        use_cache = {}
        code_cache = {}
        masked_cache = {}
        actual_source_bytes = 0
        for path in files:
            relative = _relative(root, path)
            source = _read_source(path)
            actual_source_bytes += len(source.encode("utf-8"))
            if actual_source_bytes > MAX_RUST_TOTAL_BYTES:
                raise ManifestError("Rust source total size exceeds limit")
            source_cache[path] = source
            needs_scan = _needs_consumer_scan(source, manifest)
            needs_injection_scan = bool(
                re.search(r"\binclude\s*!|#\s*\[\s*path\s*=", source)
            )
            masked_cache[path] = (
                _mask_rust_non_code(source)
                if needs_scan or needs_injection_scan
                else ""
            )
            if needs_scan:
                statements, code = _rust_views(source)
            else:
                statements, code = [], ""
            use_cache[path] = statements
            if relative.startswith("rust/src/") or relative.startswith(("rust/crates/lean-ctx-", "rust/crates/lean-ctx/")):
                code_cache[path] = code
        findings = []
        findings.extend(_source_injection_findings(root, files, source_cache, masked_cache))
        for surface in FROZEN_SURFACES:
            spec = manifest["surfaces"][surface]
            findings.extend(
                _shape_findings(root, files, surface, spec, use_cache, source_cache)
            )
            findings.extend(
                _consumer_findings(
                    root,
                    files,
                    surface,
                    spec,
                    source_cache,
                    use_cache,
                    code_cache,
                    masked_cache,
                )
            )
        findings.extend(_private_import_findings(root, files, use_cache, code_cache))
        if root == ROOT:
            findings.extend(check_classifications(rust_modules()))
            findings.extend(check_strategic_data())
        return sorted(set(findings))
    except ManifestError as error:
        return ["[manifest] %s" % error]


check = check_repo
check_open_core_boundary = check_repo


def main(argv: Optional[Sequence[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--manifest", type=Path)
    parser.add_argument("--json", action="store_true", dest="as_json")
    args = parser.parse_args(argv)
    root = args.root.resolve()
    findings = check_repo(root, args.manifest)
    if args.as_json:
        report = {
            "manifest": "security/public-protocol-surface-freeze-v1.json",
            "status": "fail" if findings else "pass",
            "surfaces": list(FROZEN_SURFACES),
            "violations": findings,
        }
        sys.stdout.buffer.write(canonical_json(report))
        return 1 if findings else 0
    if findings:
        print("Open-core boundary: FAIL")
        for finding in findings:
            print("- %s" % finding)
        return 1
    print("Open-core boundary: PASS (protocol freeze: %d surfaces)" % len(FROZEN_SURFACES))
    return 0


if __name__ == "__main__":
    sys.exit(main())
