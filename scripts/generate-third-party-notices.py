#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Generate deterministic LeanCTX third-party notices without network access.

The generator is intentionally hermetic.  It derives the Cargo inventory from
cargo metadata --locked --offline --all-features, verifies every manifest
digest locally, and never writes legal approval evidence.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path, PurePosixPath
from typing import Any

try:
    import tomllib
except ModuleNotFoundError:  # Python 3.9 uses the already-installed stdlib-compatible tomli.
    try:
        import tomli as tomllib
    except ModuleNotFoundError:
        tomllib = None

SCHEMA = "leanctx.third-party-notices/v1"
ASSET_SCHEMA = "leanctx.third-party-assets/v1"
NOTICE_PATH = "THIRD_PARTY_NOTICES"
ASSET_MANIFEST_PATH = "legal/third-party-assets.toml"
CARGO_LOCK_PATH = "rust/Cargo.lock"
CARGO_MANIFEST_PATH = "rust/Cargo.toml"
SBOM_PATH = "SBOM.cdx.json"
NPM_LOCK_PATH = "packages/pi-lean-ctx/package-lock.json"
MAX_FILE_BYTES = 16 * 1024 * 1024
MAX_METADATA_BYTES = 32 * 1024 * 1024
SHA256_RE = re.compile(r"[0-9a-f]{64}\Z")
SAFE_PATH_RE = re.compile(r"[A-Za-z0-9._/@+-]+\Z")
KNOWN_LICENSES = {
    "0BSD",
    "Apache-2.0",
    "BSD-1-Clause",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "BSL-1.0",
    "CC0-1.0",
    "CDLA-Permissive-2.0",
    "ISC",
    "LGPL-2.1-or-later",
    "LLVM-exception",
    "MIT",
    "MIT-0",
    "MPL-2.0",
    "OFL-1.1",
    "Unicode-3.0",
    "Unlicense",
    "Zlib",
    "bzip2-1.0.6",
}
ZERO_DIGEST = "0" * 64


class GenerationError(ValueError):
    """Expected fail-closed generator error."""


def _safe_relpath(value: Any) -> bool:
    if not isinstance(value, str) or not value or len(value) > 512:
        return False
    if (
        value.startswith(("/", "\\"))
        or "\x00" in value
        or any(ord(char) < 0x20 or ord(char) == 0x7F for char in value)
        or "\\" in value
        or "//" in value
    ):
        return False
    path = PurePosixPath(value)
    return (
        str(path) == value
        and "." not in path.parts
        and ".." not in path.parts
        and all(part and part not in {".", ".."} for part in path.parts)
        and bool(SAFE_PATH_RE.fullmatch(value))
    )


def _sha256(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def _read_file(path: Path, label: str, limit: int = MAX_FILE_BYTES) -> bytes:
    if path.is_symlink():
        raise GenerationError(f"{label}: symlink is not allowed")
    if not path.is_file():
        raise GenerationError(f"{label}: file is missing")
    try:
        size = path.stat().st_size
        if size > limit:
            raise GenerationError(f"{label}: file exceeds size limit")
        return path.read_bytes()
    except OSError as exc:
        raise GenerationError(f"{label}: cannot read file") from exc


def _repo_path(root: Path, value: Any, label: str, allow_missing: bool = False) -> Path:
    if not _safe_relpath(value):
        raise GenerationError(f"{label}: unsafe relative path")
    target = root.joinpath(*PurePosixPath(value).parts)
    current = root
    for part in PurePosixPath(value).parts:
        current = current / part
        if current.is_symlink():
            raise GenerationError(f"{label}: symlink is not allowed")
    if not allow_missing and not target.exists():
        raise GenerationError(f"{label}: file is missing")
    try:
        resolved = target.resolve(strict=False)
        resolved.relative_to(root.resolve())
    except (OSError, ValueError) as exc:
        raise GenerationError(f"{label}: path escapes repository root") from exc
    return target


def _tree_digest(path: Path, label: str) -> str:
    if path.is_symlink():
        raise GenerationError(f"{label}: symlink is not allowed")
    if not path.is_dir():
        raise GenerationError(f"{label}: directory is missing")
    rows: list[bytes] = []
    for current, dirs, files in os.walk(path, topdown=True, followlinks=False):
        dirs[:] = sorted(dirs)
        files[:] = sorted(files)
        for name in (*dirs, *files):
            candidate = Path(current) / name
            if candidate.is_symlink():
                raise GenerationError(f"{label}: symlink is not allowed")
        for name in files:
            candidate = Path(current) / name
            rel = candidate.relative_to(path).as_posix()
            raw = _read_file(candidate, f"{label}/{rel}")
            rows.append(f"{rel}\0{_sha256(raw)}\0{len(raw)}\n".encode())
    return _sha256(b"".join(rows))


def _path_digest(path: Path, label: str) -> tuple[str, str]:
    if path.is_dir():
        return _tree_digest(path, label), "directory"
    return _sha256(_read_file(path, label)), "file"


def _load_toml(path: Path, label: str) -> dict[str, Any]:
    raw = _read_file(path, label, MAX_FILE_BYTES)
    if tomllib is None:
        raise GenerationError("Python 3.11+ tomllib is required")
    try:
        value = tomllib.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, tomllib.TOMLDecodeError) as exc:
        raise GenerationError(f"{label}: invalid TOML") from exc
    if not isinstance(value, dict):
        raise GenerationError(f"{label}: TOML root must be a table")
    return value


def _validate_spdx(expression: Any, label: str) -> str:
    if not isinstance(expression, str) or not expression.strip() or len(expression) > 512:
        raise GenerationError(f"{label}: missing SPDX expression")
    normalized = expression.replace("/", " OR ").strip()
    token_re = re.compile(
        r"\s*(\(|\)|AND\b|OR\b|WITH\b|[A-Za-z0-9][A-Za-z0-9.+-]*)"
    )
    tokens: list[str] = []
    position = 0
    while position < len(normalized):
        match = token_re.match(normalized, position)
        if match is None:
            raise GenerationError(f"{label}: malformed SPDX expression")
        tokens.append(match.group(1))
        position = match.end()
    index = 0

    def validate_atom(atom: str) -> None:
        if atom not in KNOWN_LICENSES and not atom.startswith("LicenseRef-"):
            raise GenerationError(f"{label}: unknown license {atom}")

    def parse_atom() -> None:
        nonlocal index
        if index >= len(tokens):
            raise GenerationError(f"{label}: malformed SPDX expression")
        if tokens[index] == "(":
            index += 1
            parse_or()
            if index >= len(tokens) or tokens[index] != ")":
                raise GenerationError(f"{label}: malformed SPDX expression")
            index += 1
            return
        atom = tokens[index]
        if atom in {"AND", "OR", "WITH", ")", "("}:
            raise GenerationError(f"{label}: malformed SPDX expression")
        validate_atom(atom)
        index += 1
        if index < len(tokens) and tokens[index] == "WITH":
            index += 1
            if index >= len(tokens) or tokens[index] in {"AND", "OR", "WITH", ")", "("}:
                raise GenerationError(f"{label}: malformed SPDX expression")
            validate_atom(tokens[index])
            index += 1

    def parse_and() -> None:
        nonlocal index
        parse_atom()
        while index < len(tokens) and tokens[index] == "AND":
            index += 1
            parse_atom()

    def parse_or() -> None:
        nonlocal index
        parse_and()
        while index < len(tokens) and tokens[index] == "OR":
            index += 1
            parse_and()

    parse_or()
    if index != len(tokens):
        raise GenerationError(f"{label}: malformed SPDX expression")
    return expression


def _dotted_value(document: dict[str, Any], key: str) -> Any:
    value: Any = document
    for part in key.split("."):
        if not isinstance(value, dict) or part not in value:
            raise GenerationError(f"license text key missing: {key}")
        value = value[part]
    return value


def _check_no_absolute_strings(value: Any, root: Path, label: str) -> None:
    if isinstance(value, str):
        root_text = root.as_posix()
        if root_text in value or re.search(r"(?:path\+)?file:///(?!\.)|(?:path\+)?file://[A-Za-z]:", value):
            raise GenerationError(f"{label}: absolute worktree path")
    elif isinstance(value, list):
        for item in value:
            _check_no_absolute_strings(item, root, label)
    elif isinstance(value, dict):
        for item in value.values():
            _check_no_absolute_strings(item, root, label)


def _load_manifest(root: Path) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    path = _repo_path(root, ASSET_MANIFEST_PATH, ASSET_MANIFEST_PATH)
    document = _load_toml(path, ASSET_MANIFEST_PATH)
    if document.get("schema") != ASSET_SCHEMA or document.get("version") != 1:
        raise GenerationError(f"{ASSET_MANIFEST_PATH}: unsupported schema")
    _check_no_absolute_strings(document, root, ASSET_MANIFEST_PATH)
    inputs = document.get("inputs")
    if not isinstance(inputs, dict):
        raise GenerationError(f"{ASSET_MANIFEST_PATH}: missing inputs table")
    for name, digest_key in (
        ("cargo_lock", "cargo_lock_sha256"),
        ("sbom", "sbom_sha256"),
        ("npm_lock", "npm_lock_sha256"),
    ):
        input_path = inputs.get(name)
        expected = inputs.get(digest_key)
        target = _repo_path(root, input_path, f"{ASSET_MANIFEST_PATH}:{name}")
        actual = _sha256(_read_file(target, name))
        if not isinstance(expected, str) or not SHA256_RE.fullmatch(expected):
            raise GenerationError(f"{ASSET_MANIFEST_PATH}: invalid {digest_key}")
        if actual != expected:
            raise GenerationError(f"{name}: digest drift")
    manifest_path = str(path.relative_to(root).as_posix())
    assets = document.get("assets")
    if not isinstance(assets, list) or not assets:
        raise GenerationError(f"{ASSET_MANIFEST_PATH}: assets must be non-empty")
    seen_ids: set[str] = set()
    seen_paths: set[str] = set()
    for index, item in enumerate(assets):
        label = f"{ASSET_MANIFEST_PATH}:assets[{index}]"
        if not isinstance(item, dict):
            raise GenerationError(f"{label}: entry must be a table")
        ident = item.get("id")
        asset_path = item.get("path")
        if not isinstance(ident, str) or not ident.strip() or ident in seen_ids:
            raise GenerationError(f"{label}: missing or duplicate id")
        if not isinstance(asset_path, str) or asset_path in seen_paths:
            raise GenerationError(f"{label}: missing or duplicate path")
        seen_ids.add(ident)
        seen_paths.add(asset_path)
        generated = item.get("generated", False)
        if not isinstance(generated, bool):
            raise GenerationError(f"{label}: generated must be boolean")
        target = _repo_path(
            root,
            asset_path,
            f"{label}:path",
            allow_missing=generated and bool(item.get("allow_missing_in_checkout")),
        )
        expected = item.get("sha256")
        if not isinstance(expected, str) or not SHA256_RE.fullmatch(expected):
            raise GenerationError(f"{label}: invalid sha256")
        if target.exists():
            actual, kind = _path_digest(target, f"{label}:path")
            if actual != expected:
                raise GenerationError(f"{asset_path}: digest drift")
            item["_kind"] = kind
        elif not generated or not item.get("allow_missing_in_checkout"):
            raise GenerationError(f"{asset_path}: asset is missing")
        else:
            item["_kind"] = "generated-file"
        version = item.get("version")
        source = item.get("source")
        if not isinstance(version, str) or not version.strip():
            raise GenerationError(f"{label}: missing version")
        if not isinstance(source, str) or not source.strip():
            raise GenerationError(f"{label}: missing source")
        _validate_spdx(item.get("spdx"), f"{label}:spdx")
        license_path = item.get("license_text_path")
        license_target = _repo_path(root, license_path, f"{label}:license_text_path")
        license_key = item.get("license_text_key")
        if license_path == manifest_path:
            if not isinstance(license_key, str) or not license_key:
                raise GenerationError(f"{label}: missing license_text_key")
            license_text = _dotted_value(document, license_key)
        else:
            license_text = _read_file(license_target, f"{label}:license_text_path").decode(
                "utf-8"
            )
        if not isinstance(license_text, str) or not license_text.strip():
            raise GenerationError(f"{label}: empty license text")
        item["_license_text"] = license_text
        evidence = item.get("evidence_paths")
        if not isinstance(evidence, list) or not evidence:
            raise GenerationError(f"{label}: missing evidence_paths")
        for evidence_path in evidence:
            _repo_path(root, evidence_path, f"{label}:evidence")
        modified = item.get("modified", False)
        if not isinstance(modified, bool):
            raise GenerationError(f"{label}: modified must be boolean")
        if modified and not str(item.get("modification_summary", "")).strip():
            raise GenerationError(f"{label}: modified asset lacks modification_summary")
    return document, sorted(assets, key=lambda item: (str(item["id"]), str(item["path"])))


def _asset_covers(asset_path: str, candidate: str) -> bool:
    return candidate == asset_path or candidate.startswith(asset_path.rstrip("/") + "/")


def _validate_coverage(root: Path, assets: list[dict[str, Any]]) -> None:
    required: list[str] = []
    for directory in (
        "rust/src/dashboard/static/vendor",
        "rust/crates/vendor",
        "packages/pi-lean-ctx/extensions/vendor",
    ):
        base = _repo_path(root, directory, directory)
        for current, dirs, files in os.walk(base, topdown=True, followlinks=False):
            dirs[:] = sorted(dirs)
            files[:] = sorted(files)
            for name in (*dirs, *files):
                candidate = Path(current) / name
                if candidate.is_symlink():
                    raise GenerationError(f"{candidate.relative_to(root)}: symlink is not allowed")
            for name in files:
                required.append((Path(current) / name).relative_to(root).as_posix())
    fonts = _repo_path(root, "rust/src/dashboard/static/fonts", "fonts")
    for current, _, files in os.walk(fonts, topdown=True, followlinks=False):
        for name in sorted(files):
            if name.endswith(".woff2"):
                required.append((Path(current) / name).relative_to(root).as_posix())
    for candidate in sorted(set(required)):
        if not any(_asset_covers(str(asset["path"]), candidate) for asset in assets):
            raise GenerationError(f"{candidate}: missing asset manifest entry")


def _run_metadata(root: Path) -> tuple[dict[str, Any], str, int]:
    lock = _repo_path(root, CARGO_LOCK_PATH, CARGO_LOCK_PATH)
    lock_raw = _read_file(lock, CARGO_LOCK_PATH)
    lock_digest = _sha256(lock_raw)
    try:
        result = subprocess.run(
            [
                "cargo",
                "metadata",
                "--locked",
                "--offline",
                "--all-features",
                "--manifest-path",
                CARGO_MANIFEST_PATH,
                "--format-version",
                "1",
            ],
            cwd=root,
            env={**os.environ, "CARGO_NET_OFFLINE": "true", "CARGO_TERM_COLOR": "never"},
            check=False,
            capture_output=True,
            timeout=180,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise GenerationError("cargo metadata unavailable or timed out") from exc
    if result.returncode != 0 or len(result.stdout) > MAX_METADATA_BYTES:
        raise GenerationError("cargo metadata --locked --offline failed")
    try:
        document = json.loads(result.stdout.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise GenerationError("cargo metadata returned invalid JSON") from exc
    packages = document.get("packages")
    nodes = (document.get("resolve") or {}).get("nodes")
    if not isinstance(packages, list) or not isinstance(nodes, list):
        raise GenerationError("cargo metadata lacks the resolved dependency graph")
    if lock_raw.count(b"[[package]]") != len(packages):
        raise GenerationError("Cargo.lock and cargo metadata package counts differ")
    by_id = {package.get("id"): package for package in packages if isinstance(package, dict)}
    node_by_id = {node.get("id"): node for node in nodes if isinstance(node, dict)}
    rows: list[dict[str, Any]] = []
    for package in packages:
        if not isinstance(package, dict) or not package.get("source"):
            continue
        name = package.get("name")
        version = package.get("version")
        license_expression = package.get("license")
        if not isinstance(name, str) or not isinstance(version, str):
            raise GenerationError("cargo metadata package lacks name/version")
        _validate_spdx(license_expression, f"Cargo package {name}@{version}")
        node = node_by_id.get(package.get("id"), {})
        dep_names: set[str] = set()
        for dependency in node.get("deps", []) if isinstance(node, dict) else []:
            if not isinstance(dependency, dict):
                continue
            dep_id = dependency.get("pkg")
            dep = by_id.get(dep_id)
            dep_name = dep.get("name") if isinstance(dep, dict) else dependency.get("name")
            if isinstance(dep_name, str):
                dep_names.add(dep_name)
        targets = []
        for target in package.get("targets", []):
            if isinstance(target, dict) and isinstance(target.get("name"), str):
                kinds = target.get("kind", [])
                if isinstance(kinds, list):
                    targets.append(f"{target['name']}[{','.join(sorted(str(k) for k in kinds))}]")
        features = package.get("features", {})
        rows.append({
            "name": name,
            "version": version,
            "source": str(package["source"]),
            "package_source": str(
                package.get("repository") or package.get("homepage") or package["source"]
            ),
            "spdx": license_expression,
            "targets": sorted(set(targets)),
            "features": sorted(features) if isinstance(features, dict) else [],
            "dependencies": sorted(dep_names),
        })
    rows.sort(key=lambda item: (item["name"], item["version"], item["source"]))
    canonical = json.dumps(
        {"package_count": len(packages), "dependencies": rows},
        ensure_ascii=True,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")
    return {
        "all_package_count": len(packages),
        "third_party_package_count": len(rows),
        "packages": rows,
        "digest": _sha256(canonical),
    }, lock_digest, len(packages)


def _validate_sbom(root: Path, lock_digest: str) -> tuple[str, int]:
    path = _repo_path(root, SBOM_PATH, SBOM_PATH)
    raw = _read_file(path, SBOM_PATH)
    try:
        document = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise GenerationError("SBOM.cdx.json: invalid JSON") from exc
    _check_no_absolute_strings(document, root, SBOM_PATH)
    if document.get("bomFormat") != "CycloneDX" or not isinstance(
        document.get("components"), list
    ) or not document["components"]:
        raise GenerationError("SBOM.cdx.json: incomplete CycloneDX document")
    properties = (document.get("metadata") or {}).get("properties")
    digests = [
        item.get("value")
        for item in properties or []
        if isinstance(item, dict) and item.get("name") == "leanctx:cargo-lock-sha256"
    ]
    if len(digests) != 1 or digests[0] != lock_digest:
        raise GenerationError("SBOM.cdx.json: Cargo.lock digest is stale")
    lock_path = _repo_path(root, "rust/Cargo.lock", "rust/Cargo.lock")
    packages = _load_toml(lock_path, "rust/Cargo.lock").get("package")
    if not isinstance(packages, list) or not packages:
        raise GenerationError("rust/Cargo.lock: missing package inventory")
    locked_versions = set()
    for package in packages:
        if not isinstance(package, dict) or not all(
            isinstance(package.get(key), str) and package[key]
            for key in ("name", "version")
        ):
            raise GenerationError("rust/Cargo.lock: invalid package inventory")
        locked_versions.add((package["name"], package["version"]))
    for component in document["components"]:
        if not isinstance(component, dict) or not all(
            isinstance(component.get(key), str) and component[key]
            for key in ("name", "version", "type")
        ):
            raise GenerationError("SBOM.cdx.json: invalid component")
        if (component["name"], component["version"]) not in locked_versions:
            raise GenerationError("SBOM.cdx.json: component absent from Cargo.lock")
    return _sha256(raw), len(document["components"])


def _validate_npm_lock(root: Path, assets: list[dict[str, Any]]) -> dict[str, str]:
    path = _repo_path(root, NPM_LOCK_PATH, NPM_LOCK_PATH)
    raw = _read_file(path, NPM_LOCK_PATH)
    try:
        document = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise GenerationError("package-lock.json: invalid JSON") from exc
    entry = (document.get("packages") or {}).get("node_modules/@modelcontextprotocol/sdk")
    if not isinstance(entry, dict):
        raise GenerationError("package-lock.json: bundled MCP SDK entry is missing")
    for key in ("version", "resolved", "integrity"):
        if not isinstance(entry.get(key), str) or not entry[key]:
            raise GenerationError(f"package-lock.json: SDK {key} is missing")
    sdk_assets = [
        item for item in assets if str(item.get("id", "")).startswith("mcp-sdk-")
    ]
    if not sdk_assets:
        raise GenerationError("asset manifest: bundled MCP SDK is not declared")
    for item in sdk_assets:
        if item.get("version") != entry["version"]:
            raise GenerationError("asset manifest: MCP SDK version drift")
        if item.get("source") != entry["resolved"]:
            raise GenerationError("asset manifest: MCP SDK source drift")
    return {
        "name": "@modelcontextprotocol/sdk",
        "version": entry["version"],
        "source": entry["resolved"],
        "integrity": entry["integrity"],
    }




def _render_with_inputs(
    metadata: dict[str, Any],
    lock_digest: str,
    sbom_digest: str,
    sbom_count: int,
    npm: dict[str, str],
    manifest: dict[str, Any],
    manifest_raw: bytes,
    assets: list[dict[str, Any]],
    status: str,
) -> bytes:
    npm_digest = str(manifest["inputs"]["npm_lock_sha256"])
    # _render_notice keeps the public rendering function small while avoiding
    # any path-derived digest in its output.
    lines = [
        "# LeanCTX third-party notices",
        f"Schema: {SCHEMA}",
        f"Status: {status}",
        "Content-Digest-Scope: SHA-256 of this file with the Content-Digest value zeroed",
        f"Content-Digest: sha256:{ZERO_DIGEST}",
        "Source inventory: rust/Cargo.lock + cargo metadata --locked --offline --all-features + SBOM.cdx.json + legal/third-party-assets.toml + packages/pi-lean-ctx/package-lock.json",
        "Generation command: cargo metadata --locked --offline --all-features --manifest-path rust/Cargo.toml --format-version 1",
        "Approval contract: legal/third-party-notices-approval.toml binds approved_content_sha256 to this file's exact SHA-256",
        f"Cargo.lock SHA-256: {lock_digest}",
        f"Cargo metadata inventory SHA-256: {metadata['digest']}",
        f"SBOM.cdx.json SHA-256: {sbom_digest}",
        f"SBOM components: {sbom_count}",
        f"Asset manifest SHA-256: {_sha256(manifest_raw)}",
        f"npm package-lock.json SHA-256: {npm_digest}",
        f"Bundled MCP SDK: {npm['name']} {npm['version']} ({npm['integrity']})",
        f"Cargo graph packages: {metadata['all_package_count']}",
        f"Third-party Cargo packages: {metadata['third_party_package_count']}",
        "",
        "## Cargo dependency graph",
    ]
    for package in metadata["packages"]:
        lines.extend([
            f"### {package['name']} {package['version']}",
            f"Source: {package['source']}",
            f"Package source: {package['package_source']}",
            f"SPDX-License-Identifier: {package['spdx']}",
            f"Targets: {', '.join(package['targets']) or '(none)'}",
            f"Features: {', '.join(package['features']) or '(none)'}",
            f"Dependencies: {', '.join(package['dependencies']) or '(none)'}",
            "",
        ])
    lines.append("## Non-Cargo and vendored assets")
    text_records: dict[str, dict[str, Any]] = {}
    for asset in assets:
        lines.extend([
            f"### {asset['id']}",
            f"Path: {asset['path']}",
            f"Digest: sha256:{asset['sha256']}",
            f"Version: {asset['version']}",
            f"Source: {asset['source']}",
            f"SPDX-License-Identifier: {asset['spdx']}",
            f"License text path: {asset['license_text_path']}",
            f"Evidence paths: {', '.join(sorted(asset['evidence_paths']))}",
            f"Copyright: {asset.get('copyright', 'as declared by upstream')}",
            f"Modified: {'yes' if asset.get('modified', False) else 'no'}",
        ])
        if asset.get("modified", False):
            lines.append(f"Modification summary: {asset['modification_summary']}")
        if asset.get("generated", False) and asset.get("allow_missing_in_checkout"):
            lines.append("Build artifact: generated by npm prepack; digest is checked when present")
        lines.append("")
        text = str(asset["_license_text"])
        key = _sha256(text.encode("utf-8"))
        record = text_records.setdefault(
            key,
            {"spdx": asset["spdx"], "paths": [], "text": text},
        )
        record["paths"].append(str(asset["path"]))
    lines.append("## Deduplicated license texts")
    for record in sorted(text_records.values(), key=lambda item: (item["spdx"], item["text"])):
        lines.extend([
            f"### {record['spdx']}",
            f"Source paths: {', '.join(sorted(record['paths']))}",
            "",
        ])
        lines.extend(str(record["text"]).rstrip("\n").split("\n"))
        lines.append("")
    rendered = ("\n".join(lines).rstrip("\n") + "\n").encode("utf-8")
    digest = _sha256(rendered)
    canonical = rendered.replace(
        f"Content-Digest: sha256:{digest}".encode("utf-8"),
        f"Content-Digest: sha256:{ZERO_DIGEST}".encode("utf-8"),
        1,
    )
    digest = _sha256(canonical)
    return rendered.replace(
        f"Content-Digest: sha256:{ZERO_DIGEST}".encode("utf-8"),
        f"Content-Digest: sha256:{digest}".encode("utf-8"),
        1,
    )


def _generate(root: Path) -> tuple[bytes, bytes]:
    root = root.resolve()
    if not root.is_dir():
        raise GenerationError("repository root is not a directory")
    manifest_path = _repo_path(root, ASSET_MANIFEST_PATH, ASSET_MANIFEST_PATH)
    manifest_raw = _read_file(manifest_path, ASSET_MANIFEST_PATH)
    manifest, assets = _load_manifest(root)
    _validate_coverage(root, assets)
    metadata, lock_digest, _ = _run_metadata(root)
    sbom_digest, sbom_count = _validate_sbom(root, lock_digest)
    npm = _validate_npm_lock(root, assets)
    pending = _render_with_inputs(
        metadata, lock_digest, sbom_digest, sbom_count, npm,
        manifest, manifest_raw, assets, "GENERATED - PENDING APPROVAL",
    )
    approved = _render_with_inputs(
        metadata, lock_digest, sbom_digest, sbom_count, npm,
        manifest, manifest_raw, assets, "GENERATED AND APPROVED",
    )
    return pending, approved


def _write_atomic(path: Path, data: bytes) -> None:
    if path.is_symlink():
        raise GenerationError(f"{NOTICE_PATH}: symlink is not allowed")
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temp_name = tempfile.mkstemp(prefix=".third-party-notices.", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temp_name, path)
    except OSError as exc:
        try:
            os.unlink(temp_name)
        except OSError:
            pass
        raise GenerationError(f"{NOTICE_PATH}: cannot write") from exc


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--write", action="store_true")
    mode.add_argument("--check", action="store_true")
    mode.add_argument("--print-digest", action="store_true")
    args = parser.parse_args(argv)
    try:
        pending, approved = _generate(args.root)
        root = args.root.resolve()
        target = _repo_path(root, NOTICE_PATH, NOTICE_PATH, allow_missing=True)
        if args.write:
            _write_atomic(target, pending)
            print(f"generate-third-party-notices: wrote {NOTICE_PATH}")
            return 0
        if args.print_digest:
            print(_sha256(pending))
            return 0
        if not target.exists():
            raise GenerationError(f"{NOTICE_PATH}: file is missing")
        current = _read_file(target, NOTICE_PATH)
        if args.check and current not in {pending, approved}:
            raise GenerationError(f"{NOTICE_PATH}: generated bytes drifted")
        if args.check:
            print(f"generate-third-party-notices: check PASS ({_sha256(current)})")
        return 0
    except GenerationError as exc:
        print(f"generate-third-party-notices: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
