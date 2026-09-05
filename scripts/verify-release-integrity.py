#!/usr/bin/env python3
"""Verify the checksums, SBOM, and manifest of a downloaded release."""

import argparse
import hashlib
import json
import re
import urllib.parse
import urllib.request
from pathlib import Path


class GateError(RuntimeError):
    """Raised when release-integrity evidence is malformed or does not match."""


SHA256_RE = re.compile(r"[0-9a-f]{64}")
COMMIT_RE = re.compile(r"[0-9a-f]{40}")
RELEASE_FILES = ("SHA256SUMS", "SBOM.txt", "release-manifest.json")


def sha256_file(path):
    """Return the SHA-256 digest of a regular file."""
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def safe_filename(value):
    """Reject paths that could make an archive write outside its download directory."""
    if not isinstance(value, str) or not value:
        raise GateError("release file name is unsafe")
    path = Path(value)
    if path.is_absolute() or len(path.parts) != 1:
        raise GateError("release file name is unsafe")
    return value


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
    """Parse Cargo's one-package-and-license-per-line SBOM representation."""
    entries = []
    for line in data.decode("utf-8", errors="strict").splitlines():
        package_and_version, separator, license_name = line.rpartition(" ")
        if not separator or not package_and_version or not license_name:
            raise GateError("invalid SBOM entry")
        entries.append({"package": package_and_version, "license": license_name})
    if not entries:
        raise GateError("SBOM contains no entries")
    return entries


def validate_manifest(value):
    """Validate and return the v1 release manifest."""
    expected = {
        "schema_version", "tag", "commit", "timestamp", "artifacts",
        "sbom_sha256", "checksums_sha256",
    }
    if not isinstance(value, dict) or set(value) != expected:
        raise GateError("invalid release manifest schema")
    if value["schema_version"] != "leanctx.release-manifest/v1":
        raise GateError("unsupported release manifest schema")
    if not isinstance(value["tag"], str) or not value["tag"]:
        raise GateError("invalid manifest tag")
    if not isinstance(value["timestamp"], str) or not value["timestamp"].endswith("Z"):
        raise GateError("invalid manifest timestamp")
    if not isinstance(value["commit"], str) or not COMMIT_RE.fullmatch(value["commit"]):
        raise GateError("invalid manifest commit")
    if not all(isinstance(value[key], str) and SHA256_RE.fullmatch(value[key])
               for key in ("sbom_sha256", "checksums_sha256")):
        raise GateError("invalid manifest digest")
    artifacts = value["artifacts"]
    if not isinstance(artifacts, dict) or not artifacts:
        raise GateError("invalid manifest artifacts")
    for name, details in artifacts.items():
        safe_filename(name)
        if (not isinstance(details, dict) or set(details) != {"sha256", "size"}
                or not isinstance(details["sha256"], str)
                or not SHA256_RE.fullmatch(details["sha256"])
                or not isinstance(details["size"], int) or details["size"] < 0):
            raise GateError("invalid manifest artifact")
    return value


def read_release_file(directory, name):
    path = directory / safe_filename(name)
    if not path.is_file():
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

        sbom_path = read_release_file(directory, "SBOM.txt")
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
            report["checks"].append(f"artifact:{name}")
        report["verified"] = True
    except (GateError, OSError, UnicodeError, json.JSONDecodeError) as exc:
        report["errors"].append(str(exc))
    return report


def download_file(url, destination):
    """Download one release asset without interpreting its contents."""
    request = urllib.request.Request(url, headers={"User-Agent": "lean-ctx-release-integrity"})
    with urllib.request.urlopen(request, timeout=30) as response:
        destination.write_bytes(response.read())


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
    for action in ("verify", "download"):
        command = commands.add_parser(action)
        command.add_argument("--tag", required=True)
        command.add_argument("--dir", "--download-dir", dest="dir", type=Path, required=True)
    commands.choices["download"].add_argument("--repository", default="yvgude/lean-ctx")
    args = parser.parse_args(argv)
    try:
        report = (verify_release(args.tag, args.dir) if args.action == "verify"
                  else download_release(args.tag, args.dir, args.repository))
    except (GateError, OSError, UnicodeError) as exc:
        report = {"schema_version": "leanctx.release-integrity-report/v1", "tag": args.tag,
                  "verified": False, "checks": [], "errors": [str(exc)]}
    print(json.dumps(report, sort_keys=True))
    return 0 if report.get("verified", args.action == "download") else 1


if __name__ == "__main__":
    raise SystemExit(main())
