#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Scan release artifacts for private content before anything is published.

    scan-release-artifacts.py ARTIFACT... [--json REPORT]

Every artifact — a binary, a crate, a wheel, a NuGet or npm package, a tar or zip — is
read whole. Archives are opened recursively (nested archives up to depth 3) and every
member's path and bytes are checked; a plain file's bytes are checked as they are. A finding
is a private source path or name in a member path, or a private marker in the bytes:
source paths below `ee/`, private crate identifiers, a proprietary SPDX source header.

The markers are the private tree's own names: they never occur in a correct public build,
so a hit is a leak, not a heuristic. An archive that cannot be read is a finding too (fail
closed). Exit 0 only when no artifact has a finding. The report names artifact, member and
marker — never surrounding bytes.

An executable (ELF, Mach-O including universal binaries, PE) must also carry no debug
information: its section tables are read and a debug section (`.debug_*`, `.zdebug_*`,
the Mach-O `__DWARF` segment) is a finding. Debug information holds source paths and
names that a marker list cannot enumerate; release builds strip it (`strip = true`), so
no DWARF reaches a public artifact and none has to be parsed. Only section headers are
read — a debug-looking string elsewhere in the bytes is not a section.
"""
from __future__ import annotations

import argparse
import io
import json
import struct
import sys
import tarfile
import zipfile
from pathlib import Path, PurePosixPath

PRIVATE_PATH_PARTS = ("ee",)
PRIVATE_MARKERS = (
    b"ee/services/", b"ee/governance/", b"ee/intelligence/", b"ee/contracts/", b"ee/tooling/",
    b"ee\\services\\", b"ee\\governance\\", b"ee\\intelligence\\",
    b"leanctx_team_company_contracts", b"leanctx-team-company-contracts",
    b"commercial_license::", b"commercial_core::",
    # A proprietary source header shipped inside an artifact. The bare identifier is
    # not a marker: the public Engine names it when it verifies a commercial Runtime.
    # Split so this file itself carries no second, header-shaped SPDX line.
    b"SPDX-License-" b"Identifier: LicenseRef-Proprietary",
)
ZIP_SUFFIXES = (".zip", ".whl", ".nupkg", ".jar", ".vsix")
TAR_SUFFIXES = (".tar", ".tgz", ".tar.gz", ".crate", ".tar.xz", ".tar.bz2")
MAX_DEPTH = 3
MAX_MEMBERS = 20_000
MAX_MEMBER_BYTES = 512 * 1024 * 1024


def is_archive(name: str) -> str | None:
    folded = name.lower()
    if folded.endswith(ZIP_SUFFIXES):
        return "zip"
    if folded.endswith(TAR_SUFFIXES):
        return "tar"
    return None


def path_finding(path: str) -> str | None:
    parts = PurePosixPath(path.replace("\\", "/")).parts
    if any(part in PRIVATE_PATH_PARTS for part in parts):
        return "private path component `ee`"
    return None


def byte_findings(data: bytes) -> list[str]:
    return [marker.decode() for marker in PRIVATE_MARKERS if marker in data]


def _is_debug(name: str) -> bool:
    return name.startswith((".debug", ".zdebug")) or name == "__DWARF"


def _cstr(data: bytes, offset: int) -> str:
    end = data.find(b"\0", offset)
    if offset < 0 or offset >= len(data) or end < 0:
        raise ValueError("name outside the file")
    return data[offset:end].decode("latin-1")


def _elf_sections(data: bytes) -> list[str]:
    wide, order = data[4] == 2, "<" if data[5] == 1 else ">"
    if wide:
        shoff, = struct.unpack_from(order + "Q", data, 0x28)
        shentsize, shnum, shstrndx = struct.unpack_from(order + "HHH", data, 0x3A)
        name_at, offset_at, offset_fmt = 0, 0x18, "Q"
    else:
        shoff, = struct.unpack_from(order + "I", data, 0x20)
        shentsize, shnum, shstrndx = struct.unpack_from(order + "HHH", data, 0x2E)
        name_at, offset_at, offset_fmt = 0, 0x10, "I"
    if shoff == 0 or shnum == 0:
        return []
    if shstrndx >= shnum or shoff + shnum * shentsize > len(data):
        raise ValueError("section table outside the file")
    strtab, = struct.unpack_from(order + offset_fmt, data, shoff + shstrndx * shentsize + offset_at)
    names = []
    for index in range(shnum):
        name, = struct.unpack_from(order + "I", data, shoff + index * shentsize + name_at)
        names.append(_cstr(data, strtab + name))
    return names


def _macho_sections(data: bytes, base: int = 0) -> list[str]:
    magic = data[base:base + 4]
    if magic in (b"\xca\xfe\xba\xbe", b"\xca\xfe\xba\xbf"):  # universal: one image per architecture
        count, = struct.unpack_from(">I", data, base + 4)
        wide = magic == b"\xca\xfe\xba\xbf"
        names = []
        for index in range(count):
            entry = base + 8 + index * (32 if wide else 20)
            offset, = struct.unpack_from(">Q" if wide else ">I", data, entry + 8)
            names += _macho_sections(data, offset)
        return names
    wide = magic == b"\xcf\xfa\xed\xfe"
    ncmds, = struct.unpack_from("<I", data, base + 16)
    at = base + (32 if wide else 28)
    names = []
    for _ in range(ncmds):
        cmd, size = struct.unpack_from("<II", data, at)
        if size < 8 or at + size > len(data):
            raise ValueError("load command outside the file")
        if cmd in (0x1, 0x19):  # LC_SEGMENT, LC_SEGMENT_64
            names.append(data[at + 8:at + 24].rstrip(b"\0").decode("latin-1"))
        at += size
    return names


def _pe_sections(data: bytes) -> list[str]:
    pe, = struct.unpack_from("<I", data, 0x3C)
    if data[pe:pe + 4] != b"PE\0\0":
        return []
    count, = struct.unpack_from("<H", data, pe + 6)
    symbols, nsymbols = struct.unpack_from("<II", data, pe + 12)
    optional, = struct.unpack_from("<H", data, pe + 20)
    table = pe + 24 + optional
    strings = symbols + nsymbols * 18  # COFF string table, holds long section names
    names = []
    for index in range(count):
        raw = data[table + index * 40:table + index * 40 + 8].rstrip(b"\0").decode("latin-1")
        # MinGW names DWARF sections "/<offset>" into the string table.
        names.append(_cstr(data, strings + int(raw[1:])) if raw.startswith("/") and raw[1:].isdigit()
                     else raw)
    return names


def debug_sections(data: bytes) -> list[str]:
    """Debug section names of an executable image; [] for anything that is not one."""
    if data[:4] == b"\x7fELF":
        reader = _elf_sections
    elif data[:4] in (b"\xcf\xfa\xed\xfe", b"\xce\xfa\xed\xfe", b"\xca\xfe\xba\xbe", b"\xca\xfe\xba\xbf"):
        reader = _macho_sections
    elif data[:2] == b"MZ" and len(data) > 0x40:
        reader = _pe_sections
    else:
        return []
    try:
        names = reader(data)
    except (struct.error, ValueError, IndexError) as error:
        return [f"unreadable section table ({type(error).__name__})"]
    return sorted({name for name in names if _is_debug(name)})


def scan_bytes(label: str, data: bytes, depth: int, findings: list[dict]) -> None:
    kind = is_archive(label)
    if kind and depth < MAX_DEPTH:
        try:
            members = list(iter_members(kind, data))
        except (OSError, ValueError, tarfile.TarError, zipfile.BadZipFile, EOFError) as error:
            findings.append({"artifact": label, "finding": f"unreadable archive ({type(error).__name__})"})
            return
        for name, member in members:
            where = f"{label}!{name}"
            reason = path_finding(name)
            if reason:
                findings.append({"artifact": where, "finding": reason})
            if member is not None:
                scan_bytes(where, member, depth + 1, findings)
        return
    for marker in byte_findings(data):
        findings.append({"artifact": label, "finding": f"private marker `{marker}`"})
    for section in debug_sections(data):
        findings.append({"artifact": label, "finding": f"debug information `{section}`"})


def iter_members(kind: str, data: bytes):
    count = 0
    if kind == "zip":
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            for info in archive.infolist():
                count += 1
                if count > MAX_MEMBERS:
                    raise ValueError("too many members")
                if info.is_dir():
                    yield info.filename, None
                    continue
                if info.file_size > MAX_MEMBER_BYTES:
                    raise ValueError("member too large")
                yield info.filename, archive.read(info)
        return
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:*") as archive:
        for info in archive:
            count += 1
            if count > MAX_MEMBERS:
                raise ValueError("too many members")
            if info.issym() or info.islnk():
                yield info.name, None
                continue
            if not info.isfile():
                yield info.name, None
                continue
            if info.size > MAX_MEMBER_BYTES:
                raise ValueError("member too large")
            handle = archive.extractfile(info)
            yield info.name, handle.read() if handle else b""


def scan(paths: list[Path]) -> list[dict]:
    findings: list[dict] = []
    for path in paths:
        if not path.is_file():
            findings.append({"artifact": str(path), "finding": "not a readable file"})
            continue
        scan_bytes(path.name, path.read_bytes(), 0, findings)
        for item in findings:
            item.setdefault("file", str(path))
    return findings


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("artifacts", nargs="+", type=Path)
    parser.add_argument("--json", type=Path)
    args = parser.parse_args()
    findings = scan(args.artifacts)
    report = {"schema": "leanctx.release-artifact-scan/v1", "artifacts": [str(p) for p in args.artifacts],
              "result": "FAIL" if findings else "PASS", "findings": findings}
    if args.json:
        args.json.write_text(json.dumps(report, indent=1) + "\n")
    for item in findings:
        print(f"scan-release-artifacts: {item['artifact']}: {item['finding']}", file=sys.stderr)
    print(f"scan-release-artifacts: {report['result']} ({len(args.artifacts)} artifact(s), "
          f"{len(findings)} finding(s))")
    return 1 if findings else 0


if __name__ == "__main__":
    sys.exit(main())
