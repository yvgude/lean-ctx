#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Fail when a Windows engine imports a runtime DLL that a clean Windows lacks.

A clean Windows install ships the Universal CRT (api-ms-win-crt-*, ucrtbase)
and the system DLLs, but not the Visual C++ Redistributable (vcruntime,
msvcp, concrt, vcomp) nor the MinGW runtime (libgcc_s, libstdc++,
libwinpthread). An engine that imports one of those exits before main with
STATUS_DLL_NOT_FOUND (0xC0000135) and no message (#2178). Only the regular
import directory is load-time; delay-load imports are listed for information.
"""
import argparse
from pathlib import Path
import re
import struct
import sys

FORBIDDEN = tuple(
    re.compile(pattern, re.IGNORECASE)
    for pattern in (
        r"vcruntime\d+.*\.dll",
        r"msvcp\d+.*\.dll",
        r"msvcr\d+.*\.dll",
        r"concrt\d+\.dll",
        r"vccorlib\d+\.dll",
        r"vcomp\d+.*\.dll",
        r"ucrtbased\.dll",
        r"libgcc_s_.*\.dll",
        r"libstdc\+\+-\d+\.dll",
        r"libwinpthread-\d+\.dll",
        r"libgomp-\d+\.dll",
    )
)

IMPORT_DIRECTORY = 1
DELAY_IMPORT_DIRECTORY = 13


class PEError(ValueError):
    pass


def _unpack(fmt: str, data: bytes, offset: int):
    size = struct.calcsize(fmt)
    if offset < 0 or offset + size > len(data):
        raise PEError(f"truncated PE at offset {offset:#x}")
    return struct.unpack_from(fmt, data, offset)


def _sections(data: bytes, offset: int, count: int):
    sections = []
    for index in range(count):
        _, virtual_size, virtual_address, raw_size, raw_pointer = _unpack(
            "<8sIIII", data, offset + 40 * index
        )
        sections.append((virtual_address, max(virtual_size, raw_size), raw_pointer, raw_size))
    return sections


def _rva_to_offset(sections, rva: int) -> int:
    for virtual_address, span, raw_pointer, raw_size in sections:
        if virtual_address <= rva < virtual_address + span:
            delta = rva - virtual_address
            if delta >= raw_size:
                raise PEError(f"RVA {rva:#x} lies outside the section's file data")
            return raw_pointer + delta
    raise PEError(f"RVA {rva:#x} is not in any section")


def _c_string(data: bytes, offset: int) -> str:
    end = data.find(b"\0", offset)
    if end < 0:
        raise PEError(f"unterminated name at offset {offset:#x}")
    return data[offset:end].decode("ascii", errors="replace")


def imports(data: bytes):
    """Return (load-time DLL names, delay-load DLL names) of a PE image."""
    if data[:2] != b"MZ":
        raise PEError("not a PE image (no MZ header)")
    (pe_offset,) = _unpack("<I", data, 0x3C)
    if data[pe_offset : pe_offset + 4] != b"PE\0\0":
        raise PEError("not a PE image (no PE signature)")
    coff = pe_offset + 4
    _, section_count, _, _, _, optional_size, _ = _unpack("<HHIIIHH", data, coff)
    optional = coff + 20
    (magic,) = _unpack("<H", data, optional)
    if magic == 0x10B:
        count_offset, directories = optional + 92, optional + 96
    elif magic == 0x20B:
        count_offset, directories = optional + 108, optional + 112
    else:
        raise PEError(f"unknown optional header magic {magic:#x}")
    (directory_count,) = _unpack("<I", data, count_offset)
    sections = _sections(data, optional + optional_size, section_count)

    def directory(index: int):
        if index >= directory_count:
            return 0
        rva, size = _unpack("<II", data, directories + 8 * index)
        return rva if size else 0

    load_time = []
    rva = directory(IMPORT_DIRECTORY)
    if rva:
        offset = _rva_to_offset(sections, rva)
        while True:
            descriptor = _unpack("<IIIII", data, offset)
            if not any(descriptor):
                break
            load_time.append(_c_string(data, _rva_to_offset(sections, descriptor[3])))
            offset += 20

    delayed = []
    rva = directory(DELAY_IMPORT_DIRECTORY)
    if rva:
        offset = _rva_to_offset(sections, rva)
        while True:
            descriptor = _unpack("<8I", data, offset)
            if not any(descriptor):
                break
            if descriptor[0] & 1:  # RVA-based descriptor (every modern linker)
                delayed.append(_c_string(data, _rva_to_offset(sections, descriptor[1])))
            offset += 32
    return load_time, delayed


def forbidden(names):
    return [name for name in names if any(pattern.fullmatch(name) for pattern in FORBIDDEN)]


def _listing(names) -> str:
    unique = {name.lower(): name for name in names}
    return ", ".join(unique[key] for key in sorted(unique)) or "(none)"


def check(path: Path) -> bool:
    load_time, delayed = imports(path.read_bytes())
    print(f"{path}: load-time imports: {_listing(load_time)}")
    if delayed:
        print(f"{path}: delay-load imports: {_listing(delayed)}")
    bad = forbidden(load_time)
    if bad:
        print(
            f"{path}: FAIL: imports runtime DLLs absent from a clean Windows: {', '.join(bad)}",
            file=sys.stderr,
        )
        return False
    print(f"{path}: OK: no Visual C++ or MinGW runtime DLL is imported")
    return True


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("binaries", type=Path, nargs="+")
    args = parser.parse_args()
    results = [check(binary) for binary in args.binaries]
    sys.exit(0 if all(results) else 1)
