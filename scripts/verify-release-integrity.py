#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Verify the checksums, SBOM, and manifest of a downloaded release."""

import argparse
import hashlib
import json
import os
import re
import stat
import tarfile
import tempfile
import urllib.parse
import urllib.request
import zipfile
from pathlib import Path

from release_inventory import SUPPLEMENTAL_SOURCES, VERIFIABLE_SUPPLEMENTAL_SOURCES, artifact_kind, upload_paths
from release_inventory import safe_filename as inventory_filename


class GateError(RuntimeError):
    """Raised when release-integrity evidence is malformed or does not match."""


SHA256_RE = re.compile(r"[0-9a-f]{64}")
COMMIT_RE = re.compile(r"[0-9a-f]{40}")
RELEASE_FILES = ("SHA256SUMS", "SBOM.cdx.json", "release-manifest.json")


def sha256_file(path):
    """Return the SHA-256 digest of a regular file."""
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def safe_filename(value):
    """Reject paths that could make an archive write outside its download directory."""
    try:
        return inventory_filename(value)
    except ValueError as exc:
        raise GateError(str(exc)) from exc


def parse_checksums(data):
    """Parse GNU sha256sum output into a filename-to-digest mapping."""
    checksums = {}
    for line in data.decode("utf-8", errors="strict").splitlines():
        match = re.fullmatch(r"([0-9a-f]{64}) [ *](.+)", line)
        if not match:
            raise GateError("invalid SHA256SUMS entry")
        digest, name = match.groups()
        name = safe_filename(name)
        if name in checksums:
            raise GateError("duplicate SHA256SUMS entry")
        checksums[name] = digest
    if not checksums:
        raise GateError("SHA256SUMS contains no artifacts")
    return checksums


def parse_sbom(data):
    """Parse the CycloneDX JSON SBOM emitted by the release workflow."""
    try:
        value = json.loads(data.decode("utf-8", errors="strict"))
    except (json.JSONDecodeError, UnicodeError) as exc:
        raise GateError("invalid CycloneDX SBOM") from exc
    if (not isinstance(value, dict) or value.get("bomFormat") != "CycloneDX"
            or not isinstance(value.get("components"), list)
            or not value["components"]):
        raise GateError("invalid CycloneDX SBOM")
    return value


def validate_manifest(value):
    """Validate and return the v1 release manifest."""
    expected = {
        "schema_version", "tag", "commit", "artifacts",
        "sbom_sha256", "checksums_sha256",
    }
    if not isinstance(value, dict) or set(value) != expected:
        raise GateError("invalid release manifest schema")
    if value["schema_version"] != "leanctx.release-manifest/v1":
        raise GateError("unsupported release manifest schema")
    if not isinstance(value["tag"], str) or not value["tag"]:
        raise GateError("invalid manifest tag")
    if not isinstance(value["commit"], str) or not COMMIT_RE.fullmatch(value["commit"]):
        raise GateError("invalid manifest commit")
    if not all(isinstance(value[key], str) and SHA256_RE.fullmatch(value[key])
               for key in ("sbom_sha256", "checksums_sha256")):
        raise GateError("invalid manifest digest")
    artifacts = value["artifacts"]
    if not isinstance(artifacts, dict) or not artifacts:
        raise GateError("invalid manifest artifacts")
    typed = any(isinstance(details, dict) and "kind" in details for details in artifacts.values())
    for name, details in artifacts.items():
        safe_filename(name)
        try:
            kind = artifact_kind(name, value["tag"]) if typed else None
        except ValueError as exc:
            raise GateError(str(exc)) from exc
        fields = {"sha256", "size"}
        if typed:
            fields.add("kind")
            if kind == "binary":
                fields.add("payload_sha256")
            elif kind == "supplemental":
                fields.add("source_path")
        if (not isinstance(details, dict)
                or (set(details) != fields if typed else
                    set(details) not in (fields, fields | {"payload_sha256"}))
                or not isinstance(details["sha256"], str)
                or not SHA256_RE.fullmatch(details["sha256"])
                or type(details["size"]) is not int or details["size"] < 0):
            raise GateError("invalid manifest artifact")
        if typed and (details["kind"] != kind or
                      (kind == "supplemental" and details["source_path"] != VERIFIABLE_SUPPLEMENTAL_SOURCES[name])):
            raise GateError("manifest kind/source does not match release inventory")
        payload = details.get("payload_sha256")
        if payload is not None and (not isinstance(payload, str) or not SHA256_RE.fullmatch(payload)):
            raise GateError("invalid manifest payload digest")
        source_archive = f"lean-ctx-{value['tag'].removeprefix('v')}-source.tar.gz"
        if name != source_archive and kind != "supplemental" and payload is None:
            raise GateError("binary manifest artifact omits payload digest")
    if typed:
        if not SUPPLEMENTAL_SOURCES.keys() <= artifacts.keys():
            raise GateError("manifest omits required supplemental assets")
        if artifacts["SBOM.cdx.json"]["sha256"] != value["sbom_sha256"]:
            raise GateError("SBOM artifact digest disagrees with manifest scalar")
    return value


def payload_sha256(path):
    """Hash the single regular root-level binary without extracting it."""
    try:
        if path.suffix in (".zip", ".whl"):
            names = {"lean-ctx.exe"}
            if path.suffix == ".whl":
                distribution = path.name.split("-", 1)[0]
                if distribution not in {"thinkery_leanctx_engine", "thinkery_leanctx_engine_cuda", "thinkery_leanctx_engine_windows_gnu"}:
                    raise GateError("unsupported companion wheel distribution")
                names = {f"{distribution}/bin/lean-ctx", f"{distribution}/bin/lean-ctx.exe"}
            with zipfile.ZipFile(path) as bundle:
                members = [
                    member for member in bundle.infolist()
                    if not member.is_dir() and member.filename in names
                    and stat.S_IFMT(member.external_attr >> 16) != stat.S_IFLNK
                ]
                if len(members) != 1:
                    raise GateError("archive must contain one regular root-level payload")
                return hashlib.sha256(bundle.read(members[0])).hexdigest()
        if path.name.endswith(".tar.gz"):
            with tarfile.open(path, "r:gz") as bundle:
                members = [member for member in bundle.getmembers()
                           if member.isfile() and member.name == "lean-ctx"]
                if len(members) != 1:
                    raise GateError("archive must contain one regular root-level payload")
                handle = bundle.extractfile(members[0])
                if handle is None:
                    raise GateError("archive payload is unreadable")
                return hashlib.sha256(handle.read()).hexdigest()
    except (tarfile.TarError, zipfile.BadZipFile, OSError) as exc:
        raise GateError("invalid release archive") from exc
    raise GateError("unsupported binary release archive")


def read_release_file(directory, name):
    path = directory / safe_filename(name)
    if path.is_symlink() or not path.is_file():
        raise GateError(f"missing release file: {name}")
    return path


def verify_release(tag, directory):
    """Return a deterministic verification report for a local release directory."""
    report = {"schema_version": "leanctx.release-integrity-report/v1", "tag": tag,
              "directory": str(directory), "verified": False, "checks": [], "errors": []}
    try:
        manifest_path = read_release_file(directory, "release-manifest.json")
        manifest = validate_manifest(json.loads(manifest_path.read_text(encoding="utf-8")))
        report["manifest_tag"] = manifest["tag"]
        if manifest["tag"] != tag:
            raise GateError("manifest tag does not match expected tag")
        report["checks"].append("manifest-tag")

        sbom_path = read_release_file(directory, "SBOM.cdx.json")
        if sha256_file(sbom_path) != manifest["sbom_sha256"]:
            raise GateError("SBOM digest does not match manifest")
        report["checks"].append("sbom-sha256")
        parse_sbom(sbom_path.read_bytes())
        report["checks"].append("sbom-format")

        sums_path = read_release_file(directory, "SHA256SUMS")
        if sha256_file(sums_path) != manifest["checksums_sha256"]:
            raise GateError("SHA256SUMS digest does not match manifest")
        report["checks"].append("checksums-sha256")

        checksums = parse_checksums(sums_path.read_bytes())
        if set(checksums) != set(manifest["artifacts"]):
            raise GateError("manifest artifacts do not match SHA256SUMS")
        for name in sorted(checksums):
            path = read_release_file(directory, name)
            details = manifest["artifacts"][name]
            actual = sha256_file(path)
            if actual != checksums[name] or actual != details["sha256"]:
                raise GateError(f"artifact digest mismatch: {name}")
            if path.stat().st_size != details["size"]:
                raise GateError(f"artifact size mismatch: {name}")
            expected_payload = details.get("payload_sha256")
            if expected_payload is not None and payload_sha256(path) != expected_payload:
                raise GateError(f"artifact payload digest mismatch: {name}")
            report["checks"].append(f"artifact:{name}")
        report["verified"] = True
    except (GateError, OSError, UnicodeError, json.JSONDecodeError) as exc:
        report["errors"].append(str(exc))
    return report


def download_file(url, destination):
    """Download one release asset without interpreting its contents."""
    request = urllib.request.Request(url, headers={"User-Agent": "lean-ctx-release-integrity"})
    with urllib.request.urlopen(request, timeout=30) as response:
        data = response.read()
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary_name = None
    try:
        with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as handle:
            temporary_name = handle.name
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary_name, destination)
        temporary_name = None
    finally:
        if temporary_name is not None:
            Path(temporary_name).unlink(missing_ok=True)


def download_release(tag, directory, repository):
    """Download integrity metadata and every artifact listed by SHA256SUMS."""
    if not tag or "/" in tag or "\\" in tag:
        raise GateError("release tag is unsafe")
    directory.mkdir(parents=True, exist_ok=True)
    base = "https://github.com/{}/releases/download/{}".format(
        repository.strip("/"), urllib.parse.quote(tag, safe=""))
    download_file(f"{base}/SHA256SUMS", directory / "SHA256SUMS")
    checksums = parse_checksums((directory / "SHA256SUMS").read_bytes())
    for name in RELEASE_FILES[1:]:
        download_file(f"{base}/{urllib.parse.quote(name)}", directory / name)
    for name in sorted(checksums):
        download_file(f"{base}/{urllib.parse.quote(name)}", directory / name)
    return {"schema_version": "leanctx.release-download-report/v1", "tag": tag,
            "directory": str(directory), "downloaded": [*RELEASE_FILES, *sorted(checksums)]}


def main(argv=None):
    parser = argparse.ArgumentParser(description="Verify a lean-ctx release integrity chain")
    commands = parser.add_subparsers(dest="action", required=True)
    for action in ("verify", "download", "upload-list"):
        command = commands.add_parser(action)
        command.add_argument("--tag", required=True)
        command.add_argument("--dir", "--download-dir", dest="dir", type=Path, required=True)
    commands.choices["download"].add_argument("--repository", default="yvgude/lean-ctx")
    args = parser.parse_args(argv)
    try:
        if args.action == "upload-list":
            report = verify_release(args.tag, args.dir)
            if not report["verified"]:
                raise GateError("; ".join(report["errors"]))
            manifest = json.loads((args.dir / "release-manifest.json").read_text())
            if not all("kind" in entry for entry in manifest["artifacts"].values()):
                raise GateError("publication requires the complete typed inventory")
            print("\n".join(str(args.dir / name) for name in upload_paths(args.dir, manifest["artifacts"])))
            return 0
        report = (verify_release(args.tag, args.dir) if args.action == "verify"
                  else download_release(args.tag, args.dir, args.repository))
    except (GateError, ValueError, OSError, UnicodeError) as exc:
        report = {"schema_version": "leanctx.release-integrity-report/v1", "tag": args.tag,
                  "verified": False, "checks": [], "errors": [str(exc)]}
    print(json.dumps(report, sort_keys=True))
    return 0 if report.get("verified", args.action == "download") else 1


if __name__ == "__main__":
    raise SystemExit(main())
