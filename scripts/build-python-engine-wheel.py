#!/usr/bin/env python3
"""Build deterministic platform wheels containing one lean-ctx Engine binary."""

from __future__ import annotations

import argparse
import base64
import csv
import hashlib
import io
import os
from pathlib import Path
import re
import stat
import tempfile
import zipfile


ALLOWED_DISTRIBUTIONS = {
    "thinkery-leanctx-engine",
    "thinkery-leanctx-engine-cuda",
    "thinkery-leanctx-engine-windows-gnu",
}
MAX_BINARY_BYTES = 512 * 1024 * 1024
VERSION_RE = re.compile(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\Z")
PLATFORM_RE = re.compile(r"[A-Za-z0-9_]+\Z")
ZIP_TIME = (1980, 1, 1, 0, 0, 0)


def _digest(data: bytes) -> str:
    encoded = base64.urlsafe_b64encode(hashlib.sha256(data).digest())
    return "sha256=" + encoded.rstrip(b"=").decode("ascii")


def _read_regular(path: Path, *, maximum: int, label: str) -> bytes:
    initial = os.lstat(path)
    if stat.S_ISLNK(initial.st_mode) or not stat.S_ISREG(initial.st_mode):
        raise ValueError(f"{label} must be a regular non-symlink file")
    if initial.st_size <= 0 or initial.st_size > maximum:
        raise ValueError(f"{label} size is outside the release bounds")
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(path, flags)
    try:
        opened = os.fstat(descriptor)
        if not os.path.samestat(initial, opened):
            raise ValueError(f"{label} changed while it was opened")
        chunks = []
        remaining = opened.st_size
        while remaining:
            chunk = os.read(descriptor, min(1024 * 1024, remaining))
            if not chunk:
                raise ValueError(f"{label} was truncated while it was read")
            chunks.append(chunk)
            remaining -= len(chunk)
        if os.read(descriptor, 1):
            raise ValueError(f"{label} grew while it was read")
        final = os.fstat(descriptor)
        if final.st_size != opened.st_size or final.st_mtime_ns != opened.st_mtime_ns:
            raise ValueError(f"{label} changed while it was read")
        return b"".join(chunks)
    finally:
        os.close(descriptor)


def _entry(name: str, data: bytes, mode: int = 0o644) -> tuple[zipfile.ZipInfo, bytes]:
    info = zipfile.ZipInfo(name, ZIP_TIME)
    info.compress_type = zipfile.ZIP_DEFLATED
    info.create_system = 3
    info.external_attr = (stat.S_IFREG | mode) << 16
    return info, data


def _launcher(binary_name: str, expected_sha256: str) -> bytes:
    source = f'''"""Verified launcher for the bundled LeanCTX Engine."""
from __future__ import annotations

import hashlib
from importlib.resources import files
import os
import stat
import sys

EXPECTED_SHA256 = "{expected_sha256}"


def main() -> None:
    binary = files(__package__).joinpath("bin", "{binary_name}")
    path = os.fspath(binary)
    metadata = os.stat(path, follow_symlinks=False)
    if not stat.S_ISREG(metadata.st_mode):
        raise RuntimeError("bundled LeanCTX Engine is not a regular file")
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    if digest.hexdigest() != EXPECTED_SHA256:
        raise RuntimeError("bundled LeanCTX Engine failed integrity verification")
    if os.name != "nt" and not os.access(path, os.X_OK):
        raise RuntimeError("bundled LeanCTX Engine is not executable")
    os.execv(path, [path, *sys.argv[1:]])
'''
    return source.encode("utf-8")


def build_wheel(
    *,
    binary: Path,
    distribution: str,
    version: str,
    platform_tag: str,
    output_dir: Path,
    license_file: Path,
) -> Path:
    if distribution not in ALLOWED_DISTRIBUTIONS:
        raise ValueError("unsupported distribution")
    if not VERSION_RE.fullmatch(version):
        raise ValueError("version must be an exact release version")
    if not PLATFORM_RE.fullmatch(platform_tag) or platform_tag == "any":
        raise ValueError("platform tag must identify a concrete platform")
    binary_bytes = _read_regular(binary, maximum=MAX_BINARY_BYTES, label="binary")
    license_bytes = _read_regular(license_file, maximum=1024 * 1024, label="license")
    binary_sha256 = hashlib.sha256(binary_bytes).hexdigest()
    binary_name = "lean-ctx.exe" if platform_tag.startswith("win") else "lean-ctx"
    normalized = distribution.replace("-", "_")
    package = normalized
    dist_info = f"{normalized}-{version}.dist-info"
    wheel_name = f"{normalized}-{version}-py3-none-{platform_tag}.whl"
    if output_dir.exists() and (output_dir.is_symlink() or not output_dir.is_dir()):
        raise ValueError("output directory is unsafe")
    output_dir.mkdir(parents=True, exist_ok=True)
    output = output_dir / wheel_name

    metadata = f"""Metadata-Version: 2.4
Name: {distribution}
Version: {version}
Summary: Platform-specific LeanCTX Engine binary for the Thinkery LeanCTX SDK
Description-Content-Type: text/plain
License-Expression: Apache-2.0
License-File: LICENSE
Requires-Python: >=3.9
Classifier: Programming Language :: Python :: 3

This package contains the LeanCTX Engine binary required by AgentContext.
""".encode("utf-8")
    wheel = f"""Wheel-Version: 1.0
Generator: lean-ctx-engine-wheel/1
Root-Is-Purelib: false
Tag: py3-none-{platform_tag}
""".encode("utf-8")
    init = (
        f'__version__ = "{version}"\n'
        f'BINARY_SHA256 = "{binary_sha256}"\n'
    ).encode("utf-8")
    files = {
        f"{package}/__init__.py": init,
        f"{package}/launcher.py": _launcher(binary_name, binary_sha256),
        f"{package}/bin/{binary_name}": binary_bytes,
        f"{package}/BINARY.sha256": f"{binary_sha256}  {binary_name}\n".encode("ascii"),
        f"{dist_info}/METADATA": metadata,
        f"{dist_info}/WHEEL": wheel,
        f"{dist_info}/entry_points.txt": (
            f"[console_scripts]\nlean-ctx = {package}.launcher:main\n"
        ).encode("utf-8"),
        f"{dist_info}/top_level.txt": f"{package}\n".encode("ascii"),
        f"{dist_info}/licenses/LICENSE": license_bytes,
    }

    record_buffer = io.StringIO(newline="")
    writer = csv.writer(record_buffer, lineterminator="\n")
    for name in sorted(files):
        data = files[name]
        writer.writerow((name, _digest(data), str(len(data))))
    record_name = f"{dist_info}/RECORD"
    writer.writerow((record_name, "", ""))
    files[record_name] = record_buffer.getvalue().encode("utf-8")

    descriptor, temporary_name = tempfile.mkstemp(
        dir=output_dir, prefix=f".{wheel_name}.", suffix=".tmp"
    )
    os.close(descriptor)
    temporary = Path(temporary_name)
    try:
        with zipfile.ZipFile(temporary, "w", allowZip64=True) as archive:
            for name in sorted(files):
                mode = 0o755 if name == f"{package}/bin/{binary_name}" else 0o644
                info, data = _entry(name, files[name], mode)
                archive.writestr(info, data, compresslevel=9)
        os.replace(temporary, output)
    finally:
        temporary.unlink(missing_ok=True)
    return output


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--distribution", required=True, choices=sorted(ALLOWED_DISTRIBUTIONS))
    parser.add_argument("--version", required=True)
    parser.add_argument("--platform-tag", required=True)
    parser.add_argument("--output-dir", type=Path, default=Path("dist"))
    parser.add_argument(
        "--license-file",
        type=Path,
        default=Path(__file__).resolve().parents[1] / "LICENSE",
    )
    args = parser.parse_args()
    output = build_wheel(
        binary=args.binary,
        distribution=args.distribution,
        version=args.version,
        platform_tag=args.platform_tag,
        output_dir=args.output_dir,
        license_file=args.license_file,
    )
    print(output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
