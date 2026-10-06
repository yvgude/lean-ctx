#!/usr/bin/env python3
"""Fail closed when public LeanCTX entry points drift from product claims."""

from __future__ import annotations

import json
import os
from pathlib import Path, PurePosixPath
import posixpath
import re
import shlex
import sys
from typing import Any
from urllib.parse import unquote, urlsplit

try:
    import tomllib
except ModuleNotFoundError:  # Python 3.10, still used by some local workflows.
    tomllib = None  # type: ignore[assignment]


ROOT = Path(__file__).resolve().parents[1]
CONTRACT = "docs/contracts/public-product-claims-v1.md"
CONTRACT_BLOCK = re.compile(
    r"```json narrative-governance-contract\n(?P<data>.*?)\n```", re.DOTALL
)
ALLOWED_STATUSES = {
    "Available",
    "Experimental",
    "Historical",
    "Local runtime",
    "Preview",
    "Research",
    "Retired",
    "Target",
}
PRIVATE_PARTS = {".git", "internal", "private"}
IGNORED_DIRECTORY_PARTS = {".git", "target", "node_modules", ".venv", "__pycache__"}
METADATA_TEXT_KEYS = {
    "description",
    "displayname",
    "productdescription",
    "shortdescription",
    "summary",
    "tagline",
    "title",
}


def is_public_path(relative_path: str) -> bool:
    """Reject unsafe or private contract paths before opening referenced files."""
    if not isinstance(relative_path, str) or not relative_path or "\\" in relative_path:
        return False
    path = PurePosixPath(relative_path)
    return (
        bool(path.parts)
        and not path.is_absolute()
        and ".." not in path.parts
        and not any(ord(char) < 32 or char == ":" for char in relative_path)
        and not any(part.casefold() in PRIVATE_PARTS for part in path.parts)
    )


def is_safe_pattern(pattern: str) -> bool:
    if not is_public_path(pattern):
        return False
    return not any(part in {".", ""} for part in PurePosixPath(pattern).parts)


def load_contract(root: Path) -> dict[str, Any]:
    path = root / CONTRACT
    if path.is_symlink() or not path.is_file():
        raise ValueError(f"required public claims contract is missing or unsafe: {CONTRACT}")

    content = path.read_text(encoding="utf-8")
    match = CONTRACT_BLOCK.search(content)
    if match is None:
        raise ValueError(f"{CONTRACT}: missing narrative-governance JSON block")

    try:
        contract = json.loads(match.group("data"))
    except json.JSONDecodeError as error:
        raise ValueError(f"{CONTRACT}: invalid JSON: {error.msg}") from error

    if not isinstance(contract, dict) or contract.get("schema_version") != 1:
        raise ValueError(f"{CONTRACT}: expected schema_version 1")
    return contract


def _public_path_list(value: Any) -> bool:
    return isinstance(value, list) and all(is_public_path(item) for item in value)


def validate_contract(contract: dict[str, Any]) -> list[str]:
    """Validate every contract path and regex before scanning repository content."""
    failures: list[str] = []

    product = contract.get("product")
    if not isinstance(product, dict):
        failures.append(f"{CONTRACT}: product must be an object")
    else:
        for field in ("name", "category", "promise"):
            if not isinstance(product.get(field), str) or not product[field].strip():
                failures.append(f"{CONTRACT}: product.{field} must be a non-empty string")
        components = product.get("components")
        if not isinstance(components, list) or not components or not all(
            isinstance(value, str) and value.strip() for value in components
        ):
            failures.append(f"{CONTRACT}: product.components must be non-empty strings")
        story = product.get("primary_story")
        if not isinstance(story, list) or len(story) < 2 or not all(
            isinstance(value, str) and value.strip() for value in story
        ):
            failures.append(f"{CONTRACT}: product.primary_story must be an ordered string list")

    primary_entrypoints = contract.get("primary_entrypoints")
    if not _public_path_list(primary_entrypoints) or not primary_entrypoints:
        failures.append(f"{CONTRACT}: primary_entrypoints must contain public paths")
    elif len(set(primary_entrypoints)) != len(primary_entrypoints):
        failures.append(f"{CONTRACT}: primary_entrypoints must not contain duplicates")

    component_story_surfaces = contract.get("component_story_surfaces")
    if not _public_path_list(component_story_surfaces) or not component_story_surfaces:
        failures.append(f"{CONTRACT}: component_story_surfaces must contain public paths")

    required_text = contract.get("required_text")
    if not isinstance(required_text, dict):
        failures.append(f"{CONTRACT}: required_text must be an object")
    else:
        for relative_path, fragments in required_text.items():
            if not is_public_path(relative_path):
                failures.append(
                    f"{CONTRACT}: required_text path must be public: {relative_path!r}"
                )
            if not isinstance(fragments, list) or not all(
                isinstance(fragment, str) and fragment.strip() for fragment in fragments
            ):
                failures.append(
                    f"{CONTRACT}: required_text fragments must be non-empty strings: {relative_path!r}"
                )

    forbidden_text = contract.get("forbidden_text")
    if not isinstance(forbidden_text, dict):
        failures.append(f"{CONTRACT}: forbidden_text must be an object")
    else:
        for relative_path, fragments in forbidden_text.items():
            if not isinstance(relative_path, str) or not is_public_path(relative_path):
                failures.append(
                    f"{CONTRACT}: forbidden_text path must be public: {relative_path!r}"
                )
            if not isinstance(fragments, list) or not all(
                isinstance(fragment, str) and fragment for fragment in fragments
            ):
                failures.append(
                    f"{CONTRACT}: forbidden_text fragments must be non-empty strings: {relative_path!r}"
                )

    status_records = contract.get("status_guarded_records")
    if not _public_path_list(status_records):
        failures.append(f"{CONTRACT}: status_guarded_records must contain public paths")

    feature_statuses = contract.get("feature_statuses")
    if not isinstance(feature_statuses, dict) or not feature_statuses:
        failures.append(f"{CONTRACT}: feature_statuses must be a non-empty object")
    elif not all(
        isinstance(feature, str)
        and feature.strip()
        and isinstance(status, str)
        and status in ALLOWED_STATUSES
        for feature, status in feature_statuses.items()
    ):
        failures.append(f"{CONTRACT}: feature_statuses contains an invalid feature or status")

    canonical_reference = contract.get("canonical_reference")
    if not is_public_path(canonical_reference):
        failures.append(f"{CONTRACT}: canonical_reference must be a public path")

    if not _public_path_list(contract.get("historical_release_logs")):
        failures.append(f"{CONTRACT}: historical_release_logs must contain public paths")

    discovery = contract.get("discovery")
    if not isinstance(discovery, dict):
        failures.append(f"{CONTRACT}: discovery must be an object")
    else:
        for field in ("entrypoint_indexes", "excluded_prefixes"):
            if not _public_path_list(discovery.get(field)):
                failures.append(f"{CONTRACT}: discovery.{field} must contain public paths")
        for field in ("readme_globs", "metadata_globs"):
            patterns = discovery.get(field)
            if not isinstance(patterns, list) or not patterns or not all(
                isinstance(pattern, str) and is_safe_pattern(pattern)
                for pattern in patterns
            ):
                failures.append(f"{CONTRACT}: discovery.{field} must contain safe patterns")

    legacy_definitions = contract.get("legacy_definitions")
    if not isinstance(legacy_definitions, list) or not legacy_definitions or not all(
        isinstance(term, str) and term.strip() for term in legacy_definitions
    ):
        failures.append(f"{CONTRACT}: legacy_definitions must contain non-empty strings")

    scoped_exceptions = contract.get("scoped_technical_heading_exceptions")
    if not isinstance(scoped_exceptions, list):
        failures.append(f"{CONTRACT}: scoped_technical_heading_exceptions must be a list")
    else:
        for index, exception in enumerate(scoped_exceptions):
            if (
                not isinstance(exception, dict)
                or not is_public_path(exception.get("path"))
                or not isinstance(exception.get("term"), str)
                or not isinstance(exception.get("heading"), str)
                or not exception["term"].strip()
                or not exception["heading"].strip()
                or exception["term"].casefold() not in exception["heading"].casefold()
            ):
                failures.append(
                    f"{CONTRACT}: scoped_technical_heading_exceptions[{index}] needs a public path, term, and exact heading"
                )

    evidence_terms = contract.get("claim_evidence_terms")
    if not isinstance(evidence_terms, list) or not evidence_terms or not all(
        isinstance(term, str) and term.strip() for term in evidence_terms
    ):
        failures.append(f"{CONTRACT}: claim_evidence_terms must contain non-empty strings")

    unsupported_claims = contract.get("unsupported_claims")
    if not isinstance(unsupported_claims, list) or not unsupported_claims:
        failures.append(f"{CONTRACT}: unsupported_claims must be a non-empty list")
    else:
        for index, claim in enumerate(unsupported_claims):
            if (
                not isinstance(claim, dict)
                or not isinstance(claim.get("name"), str)
                or not claim["name"].strip()
                or not isinstance(claim.get("pattern"), str)
                or not claim["pattern"].strip()
            ):
                failures.append(f"{CONTRACT}: unsupported_claims[{index}] needs a name and pattern")
                continue
            try:
                re.compile(claim.get("pattern", ""), re.IGNORECASE)
            except (re.error, TypeError):
                failures.append(f"{CONTRACT}: unsupported_claims[{index}] has an invalid pattern")

    return failures


def _is_excluded(relative_path: str, prefixes: list[str]) -> bool:
    parts = PurePosixPath(relative_path).parts
    if any(part.casefold() in PRIVATE_PARTS for part in parts):
        return True
    return any(
        relative_path == prefix.rstrip("/")
        or relative_path.startswith(prefix.rstrip("/") + "/")
        for prefix in prefixes
    )


def _safe_repo_path(root: Path, relative_path: str, failures: list[str]) -> Path | None:
    if not is_public_path(relative_path):
        failures.append(f"unsafe or private governance path: {relative_path!r}")
        return None

    path = root
    for part in PurePosixPath(relative_path).parts:
        path = path / part
        if path.is_symlink():
            failures.append(f"governance path must not traverse a symlink: {relative_path}")
            return None

    try:
        path.resolve(strict=False).relative_to(root.resolve())
    except ValueError:
        failures.append(f"governance path escapes repository root: {relative_path}")
        return None
    return path


def read(relative_path: str, root: Path, failures: list[str]) -> str | None:
    path = _safe_repo_path(root, relative_path, failures)
    if path is None:
        return None
    if not path.is_file():
        failures.append(f"required governance file is missing: {relative_path}")
        return None
    try:
        return path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as error:
        failures.append(f"cannot read governance file {relative_path}: {error}")
        return None


def _walk_public_files(root: Path, excluded_prefixes: list[str]):
    """Walk public directories only; private directories are pruned before descent."""
    for current, directory_names, file_names in os.walk(root, topdown=True, followlinks=False):
        current_path = Path(current)
        relative_directory = current_path.relative_to(root).as_posix()
        if relative_directory == ".":
            relative_directory = ""
        kept_directories: list[str] = []
        for name in directory_names:
            child = f"{relative_directory}/{name}".strip("/")
            if _is_excluded(child, excluded_prefixes) or name.casefold() in IGNORED_DIRECTORY_PARTS:
                continue
            if (current_path / name).is_symlink():
                continue
            kept_directories.append(name)
        directory_names[:] = kept_directories
        for name in file_names:
            relative_file = f"{relative_directory}/{name}".strip("/")
            if not _is_excluded(relative_file, excluded_prefixes):
                yield relative_file


def _glob_matches(relative_path: str, pattern: str) -> bool:
    # fnmatch treats * as spanning '/', which is the useful behavior for these
    # small, contract-owned recursive patterns. The root-level form is explicit.
    return (
        PurePosixPath(relative_path).match(pattern)
        or (pattern.startswith("**/") and relative_path == pattern[3:])
    )


def _discover_globbed_paths(
    root: Path, patterns: list[str], excluded_prefixes: list[str]
) -> set[str]:
    files = set(_walk_public_files(root, excluded_prefixes))
    return {
        relative_path
        for relative_path in files
        if any(_glob_matches(relative_path, pattern) for pattern in patterns)
    }


def _markdown_targets(content: str, source_path: str) -> set[str]:
    targets: set[str] = set()
    raw_targets = re.findall(r"\[[^\]]*\]\(<?([^\s)>]+)>?(?:\s+[^)]*)?\)", content)
    raw_targets.extend(
        re.findall(r"(?im)^\s*\[[^\]]+\]:\s*<?([^\s>]+)>?", content)
    )
    for raw_target in raw_targets:
        target = unquote(raw_target)
        parsed = urlsplit(target)
        if parsed.scheme or parsed.netloc or not parsed.path:
            continue
        suffix = PurePosixPath(parsed.path).suffix.casefold()
        if suffix not in {".md", ".mdx", ".rst", ".txt"}:
            continue
        parent = PurePosixPath(source_path).parent.as_posix()
        normalized = posixpath.normpath(posixpath.join(parent, parsed.path))
        if normalized == ".." or normalized.startswith("../"):
            continue
        if is_public_path(normalized):
            targets.add(normalized)
    return targets


def _readme_candidates(root: Path, contract: dict[str, Any], failures: list[str]) -> set[str]:
    discovery = contract["discovery"]
    excluded = discovery["excluded_prefixes"]
    candidates = set(contract["primary_entrypoints"])
    candidates.update(_discover_globbed_paths(root, discovery["readme_globs"], excluded))
    candidates.add("docs/reference/generated/mcp-tools.md")

    index_contents: dict[str, str] = {}
    for index_path in discovery["entrypoint_indexes"]:
        index_content = read(index_path, root, failures)
        if index_content is not None:
            index_contents[index_path] = index_content
            candidates.add(index_path)

    for index_path, content in index_contents.items():
        candidates.update(
            target
            for target in _markdown_targets(content, index_path)
            if not _is_excluded(target, excluded)
            and (root / target).is_file()
            and not (root / target).is_symlink()
        )

    for relative_path in contract["required_text"]:
        candidates.add(relative_path)
    for relative_path in contract["status_guarded_records"]:
        candidates.add(relative_path)
    return candidates


def _metadata_values(path: Path, relative_path: str, failures: list[str]) -> list[str]:
    try:
        raw = path.read_text(encoding="utf-8")
        if path.name == "PKGBUILD":
            # Inspect the static package description; never source packaging code.
            description = re.search(r"(?m)^\s*pkgdesc\s*=\s*(.+)$", raw)
            if description is None:
                raise ValueError("PKGBUILD must declare a static pkgdesc")
            literal = description.group(1)
            if not re.fullmatch(r'''(?:"(?:[^"\\]|\\.)*"|'[^']*')\s*(?:#.*)?''', literal):
                raise ValueError("PKGBUILD pkgdesc must be a quoted static string")
            if literal.startswith('"') and re.search(r"\$(?:[({]|[A-Za-z_])|`", literal):
                raise ValueError("PKGBUILD pkgdesc must not contain shell expansion")
            values = shlex.split(literal, comments=True)
            if len(values) != 1:
                raise ValueError("PKGBUILD pkgdesc must be one static string")
            return values
        if path.suffix.casefold() == ".toml":
            if tomllib is not None:
                document: Any = tomllib.loads(raw)
            else:
                return _toml_description_values(raw)
        else:
            document = json.loads(raw)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
        failures.append(f"public metadata cannot be parsed ({relative_path}): {error}")
        return []

    values: list[str] = []

    def visit(value: Any) -> None:
        if isinstance(value, dict):
            for key, child in value.items():
                if isinstance(key, str) and key.casefold() in METADATA_TEXT_KEYS:
                    if isinstance(child, str) and child.strip():
                        values.append(child)
                    elif isinstance(child, list):
                        values.extend(
                            item for item in child if isinstance(item, str) and item.strip()
                        )
                visit(child)
        elif isinstance(value, list):
            for child in value:
                visit(child)

    visit(document)
    return values


def _toml_description_values(raw: str) -> list[str]:
    """Read package descriptions on Python 3.10 without adding a TOML dependency."""
    sections: dict[str, list[str]] = {}
    current_section = ""
    for line in raw.splitlines():
        section = re.match(r"^\s*\[([^]]+)\]\s*(?:#.*)?$", line)
        if section:
            current_section = section.group(1).strip()
            sections.setdefault(current_section, [])
        else:
            sections.setdefault(current_section, []).append(line)

    values: list[str] = []
    for section_name in ("package", "workspace.package"):
        for line in sections.get(section_name, []):
            field = re.match(
                r"^\s*description\s*=\s*(\"(?:\\.|[^\"\\])*\"|'[^']*')\s*(?:#.*)?$",
                line,
            )
            if not field:
                continue
            literal = field.group(1)
            if literal.startswith('"') and literal.endswith('"'):
                values.append(json.loads(literal))
            elif literal.startswith("'") and literal.endswith("'"):
                values.append(literal[1:-1])
            else:
                raise ValueError("TOML description must be a single-line string")
    return values


def _visible_lines(content: str) -> list[str]:
    source_lines = re.sub(r"<!--.*?-->", "", content, flags=re.DOTALL).splitlines()
    visible_lines: list[str] = []
    fence_char: str | None = None
    fence_length = 0
    for line in source_lines:
        fence = re.match(r"^\s{0,3}(`{3,}|~{3,})", line)
        if fence:
            marker = fence.group(1)
            if fence_char is None:
                fence_char, fence_length = marker[0], len(marker)
            elif marker[0] == fence_char and len(marker) >= fence_length:
                fence_char, fence_length = None, 0
            continue
        if fence_char is None:
            visible_lines.append(line)
    return visible_lines


def _intro_and_headings(content: str) -> str:
    visible_lines = _visible_lines(content)

    heading_positions: list[int] = []
    for index, line in enumerate(visible_lines):
        if re.match(r"^\s{0,3}#{1,6}\s+", line):
            heading_positions.append(index)
        elif (
            index + 1 < len(visible_lines)
            and line.strip()
            and re.match(r"^\s{0,3}(?:=+|-+)\s*$", visible_lines[index + 1])
        ):
            heading_positions.append(index)

    headings = [visible_lines[index] for index in heading_positions]
    if not headings:
        intro_text = "\n".join(visible_lines)[:2_000]
    else:
        # Keep every heading so later section titles cannot establish a second
        # product definition, while limiting prose checks to the opening section.
        # A tagline may be a subheading (README uses H3). Keep its following
        # definition in the introduction until the next real H1/H2 section.
        intro_end = next(
            (index for index in heading_positions[1:]
             if re.match(r"^\s{0,3}#{1,2}\s+", visible_lines[index])),
            len(visible_lines),
        )
        intro_text = "\n".join(visible_lines[:intro_end])[:1_500]
    excerpt = intro_text + "\n" + "\n".join(headings)
    return re.sub(r"(?m)^\s*>\s?", "", excerpt)


def _phrase_pattern(phrase: str) -> re.Pattern[str]:
    escaped = re.escape(phrase).replace(r"\ ", r"\s+")
    return re.compile(escaped, re.IGNORECASE)


def _has_canonical_link(content: str, source_path: str, canonical_reference: str) -> bool:
    links = re.findall(r"\[[^\]]+\]\(<?([^\s)>]+)>?(?:\s+[^)]*)?\)", content)
    links.extend(re.findall(r"(?im)^\s*\[[^\]]+\]:\s*<?([^\s>]+)>?", content))
    for target in links:
        parsed = urlsplit(unquote(target))
        if parsed.scheme or parsed.netloc or not parsed.path:
            continue
        resolved = posixpath.normpath(
            posixpath.join(PurePosixPath(source_path).parent.as_posix(), parsed.path)
        )
        if resolved == canonical_reference:
            return True
    return False


def _has_prominent_historical_status(content: str) -> bool:
    opening = "\n".join(content.splitlines()[:20])[:1_500]
    opening = re.sub(r"(?m)^\s*>\s?", "", opening).replace("**", "")
    return bool(
        re.search(r"(?im)^\s*(?:\*\*)?status(?:\*\*)?\s*:\s*historical\b", opening)
        or re.search(r"(?im)^\s*#{1,3}\s+historical\b", opening)
    )


def _claim_is_negated(paragraph: str, start: int) -> bool:
    prefix = paragraph[:start]
    return bool(
        re.search(
            r"\b(?:avoid|cannot|can't|do not|don't|does not|never|not promise|should not|must not)\b"
            r"[^.!?]{0,100}$",
            prefix,
            re.IGNORECASE,
        )
    )


def _claim_has_evidence(paragraph: str, evidence_terms: list[str]) -> bool:
    for term in evidence_terms:
        if term.casefold() == "version/date":
            if not re.search(r"\bversion\b|\b20\d{2}[-/]\d{1,2}(?:[-/]\d{1,2})?\b", paragraph, re.I):
                return False
        elif not re.search(_phrase_pattern(term), paragraph):
            return False
    return True


def _check_narrative(
    relative_path: str,
    content: str,
    contract: dict[str, Any],
    failures: list[str],
) -> None:
    if relative_path in contract["historical_release_logs"]:
        if not _has_canonical_link(content[:1_500], relative_path, contract["canonical_reference"]):
            failures.append(f"{relative_path}: historical release log needs a current canonical link")
        # Preserve published version history while checking the current intro
        # and Unreleased notes. A future release is not exempt until versioned.
        content = re.split(r"(?m)^##\s+\[?\d+\.\d+\.\d+\b", content, maxsplit=1)[0]
    excerpt = _intro_and_headings(content)
    historical_exception = _has_prominent_historical_status(
        content
    ) and _has_canonical_link(content[:1_500], relative_path, contract["canonical_reference"])

    scoped_excerpt = excerpt
    for exception in contract["scoped_technical_heading_exceptions"]:
        if exception["path"] != relative_path:
            continue
        heading = re.escape(exception["heading"])
        scoped_excerpt = re.sub(
            rf"(?im)^\s{{0,3}}#{{1,6}}\s+{heading}\s*$", "", scoped_excerpt
        )

    prose = re.sub(r"[*_`>]", "", "\n".join(_visible_lines(content)))
    for term in contract["legacy_definitions"]:
        if _phrase_pattern(term).search(scoped_excerpt) and not historical_exception:
            failures.append(
                f"{relative_path}: competing current product definition appears in the introduction or a heading: {term!r}"
            )
        # A later roadmap/product paragraph can also redefine the whole product.
        # Match explicit identity assertions, not technical mentions or negations.
        definition = re.compile(
            r"\bLeanCTX\s+(?:(?:is|becomes|will become)\s+"
            r"|is\s+growing\b[^.!?]{0,150}?\binto\s+)"
            r"(?:an?|the)\s+(?:full\s+)?" + _phrase_pattern(term).pattern,
            re.IGNORECASE,
        )
        if definition.search(prose) and not historical_exception:
            failures.append(
                f"{relative_path}: explicit current product redefinition in public prose: {term!r}"
            )

    if not historical_exception:
        paragraphs = [part for part in re.split(r"\n\s*\n", prose) if part.strip()]
        for claim in contract["unsupported_claims"]:
            pattern = re.compile(claim["pattern"], re.IGNORECASE)
            for paragraph in paragraphs:
                for match in pattern.finditer(paragraph):
                    if _claim_is_negated(paragraph, match.start()):
                        continue
                    if not _claim_has_evidence(paragraph, contract["claim_evidence_terms"]):
                        failures.append(
                            f"{relative_path}: unsupported blanket claim in public prose: {claim['name']}"
                        )


def _check_primary_content(
    contract: dict[str, Any],
    contents: dict[str, str],
    failures: list[str],
) -> None:
    product = contract["product"]
    for relative_path in contract["primary_entrypoints"]:
        content = contents.get(relative_path)
        if content is None:
            continue
        opening = "\n".join(_visible_lines(content))[:1_500].casefold()
        for field in ("category", "promise"):
            expected = product[field]
            if expected.casefold() not in opening:
                failures.append(
                    f"{relative_path}: missing required current product {field} in opening copy: {expected!r}"
                )

    story = " → ".join(product["primary_story"])
    for relative_path in contract["component_story_surfaces"]:
        content = contents.get(relative_path)
        if content is None:
            continue
        for component in product["components"]:
            if component.casefold() not in content.casefold():
                failures.append(f"{relative_path}: missing product component {component!r}")
        if story.casefold() not in content.casefold():
            failures.append(f"{relative_path}: missing ordered primary story {story!r}")

    for relative_path, fragments in contract["required_text"].items():
        content = contents.get(relative_path)
        if content is None:
            continue
        for fragment in fragments:
            if fragment.casefold() not in content.casefold():
                failures.append(
                    f"{relative_path}: missing required public entry-point text {fragment!r}"
                )


def check_repository(root: Path) -> list[str]:
    try:
        contract = load_contract(root)
    except (OSError, UnicodeDecodeError, ValueError) as error:
        return [str(error)]

    failures = validate_contract(contract)
    if failures:
        # No contract-declared path is opened or traversed until the full schema
        # passes, so a malformed contract cannot redirect the scan.
        return sorted(set(failures))

    discovery = contract["discovery"]
    candidates = _readme_candidates(root, contract, failures)
    metadata_paths = _discover_globbed_paths(
        root, discovery["metadata_globs"], discovery["excluded_prefixes"]
    )

    contents: dict[str, str] = {}
    for relative_path in sorted(candidates):
        if _is_excluded(relative_path, discovery["excluded_prefixes"]):
            continue
        content = read(relative_path, root, failures)
        if content is not None:
            contents[relative_path] = content
            _check_narrative(relative_path, content, contract, failures)

    for relative_path in sorted(metadata_paths):
        if _is_excluded(relative_path, discovery["excluded_prefixes"]):
            continue
        path = _safe_repo_path(root, relative_path, failures)
        if path is None or not path.is_file():
            continue
        values = _metadata_values(path, relative_path, failures)
        if not values:
            continue
        _check_narrative(relative_path, "\n\n".join(values), contract, failures)

    _check_primary_content(contract, contents, failures)

    forbidden_text = contract.get("forbidden_text", {})
    if isinstance(forbidden_text, dict):
        for relative_path, forbidden_fragments in forbidden_text.items():
            if not isinstance(relative_path, str) or not isinstance(forbidden_fragments, list):
                continue
            content = read(relative_path, root, failures)
            if content is None:
                continue
            for fragment in forbidden_fragments:
                if isinstance(fragment, str) and fragment in content:
                    failures.append(
                        f"{relative_path}: forbidden public claim {fragment!r}"
                    )

    status_pattern = re.compile(
        r"(?im)^.{0,3}(?:\*\*)?status(?:\*\*)?\s*:\s*"
        r"(?:available|preview|research|historical|retired|local runtime|local implementation|experimental|target)\b"
    )
    status_heading_pattern = re.compile(
        r"(?im)^#\s+(?:historical|research|preview|retired|local runtime|target)\b"
    )
    for relative_path in contract["status_guarded_records"]:
        content = contents.get(relative_path)
        if content is None:
            continue
        opening = content[:1_500]
        if not status_pattern.search(opening) and not status_heading_pattern.search(opening):
            failures.append(
                f"{relative_path}: retained or non-current surface needs a prominent status header"
            )

    generated_tools_path = "docs/reference/generated/mcp-tools.md"
    if generated_tools_path in contents:
        generated_tools = contents[generated_tools_path]
        if "avoids resending unchanged content" not in generated_tools:
            failures.append(
                f"{generated_tools_path}: ctx_delta needs its bounded behavior description"
            )
        if "saves 90%+ tokens" in generated_tools:
            failures.append(
                f"{generated_tools_path}: ctx_delta must not make an unqualified percentage claim"
            )

    return sorted(set(failures))


def main() -> int:
    failures = check_repository(ROOT)
    if failures:
        print("Narrative governance failed:", file=sys.stderr)
        print("\n".join(f"- {failure}" for failure in failures), file=sys.stderr)
        return 1

    print("Narrative governance passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
