# SPDX-License-Identifier: Apache-2.0
"""The Windows import guard must reject engines that need the VC++ or MinGW runtime (#2178)."""
import contextlib
import importlib.util
import io
from pathlib import Path
import struct
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location(
    "windows_imports", Path(__file__).resolve().parents[2] / "scripts/check-windows-imports.py"
)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)

SECTION_RVA = 0x1000
SECTION_RAW = 0x200
SECTION_SIZE = 0x400


def make_pe(load_time=(), delayed=(), pe32_plus=True) -> bytes:
    """A minimal PE image with one section holding the import directories."""
    body = bytearray(SECTION_SIZE)
    names_at = 0x200
    cursor = names_at

    def name_rva(name: str) -> int:
        nonlocal cursor
        encoded = name.encode("ascii") + b"\0"
        body[cursor : cursor + len(encoded)] = encoded
        rva = SECTION_RVA + cursor
        cursor += len(encoded)
        return rva

    import_offset = 0
    for index, name in enumerate(load_time):
        struct.pack_into("<IIIII", body, import_offset + 20 * index, 0, 0, 0, name_rva(name), 0)
    delay_offset = 0x100
    for index, name in enumerate(delayed):
        struct.pack_into("<8I", body, delay_offset + 32 * index, 1, name_rva(name), 0, 0, 0, 0, 0, 0)

    optional_size = 240 if pe32_plus else 224
    image = bytearray(SECTION_RAW)
    image[0:2] = b"MZ"
    struct.pack_into("<I", image, 0x3C, 0x40)
    image[0x40:0x44] = b"PE\0\0"
    struct.pack_into("<HHIIIHH", image, 0x44, 0x8664, 1, 0, 0, 0, optional_size, 0x22)
    optional = 0x58
    struct.pack_into("<H", image, optional, 0x20B if pe32_plus else 0x10B)
    count_offset = optional + (108 if pe32_plus else 92)
    directories = optional + (112 if pe32_plus else 96)
    struct.pack_into("<I", image, count_offset, 16)
    if load_time:
        struct.pack_into("<II", image, directories + 8 * 1, SECTION_RVA + import_offset, 20 * (len(load_time) + 1))
    if delayed:
        struct.pack_into("<II", image, directories + 8 * 13, SECTION_RVA + delay_offset, 32 * (len(delayed) + 1))
    section = optional + optional_size
    struct.pack_into("<8sIIII", image, section, b".idata", SECTION_SIZE, SECTION_RVA, SECTION_SIZE, SECTION_RAW)
    return bytes(image) + bytes(body)


class WindowsImportsTest(unittest.TestCase):
    def test_parses_load_time_and_delay_imports(self):
        for pe32_plus in (True, False):
            with self.subTest(pe32_plus=pe32_plus):
                image = make_pe(
                    ["KERNEL32.dll", "api-ms-win-crt-runtime-l1-1-0.dll"],
                    ["onnxruntime.dll"],
                    pe32_plus=pe32_plus,
                )
                self.assertEqual(
                    MODULE.imports(image),
                    (["KERNEL32.dll", "api-ms-win-crt-runtime-l1-1-0.dll"], ["onnxruntime.dll"]),
                )

    def test_forbidden_runtime_dlls(self):
        cases = {
            "VCRUNTIME140.dll": True,
            "vcruntime140_1.dll": True,
            "MSVCP140.dll": True,
            "msvcp140_atomic_wait.dll": True,
            "MSVCR120.dll": True,
            "CONCRT140.dll": True,
            "VCOMP140.DLL": True,
            "ucrtbased.dll": True,
            "libgcc_s_seh-1.dll": True,
            "libstdc++-6.dll": True,
            "libwinpthread-1.dll": True,
            # Shipped with every Windows 10+ install.
            "msvcrt.dll": False,
            "msvcp_win.dll": False,
            "ucrtbase.dll": False,
            "api-ms-win-crt-runtime-l1-1-0.dll": False,
            "KERNEL32.dll": False,
            "bcryptprimitives.dll": False,
        }
        for name, bad in cases.items():
            with self.subTest(name=name):
                self.assertEqual(MODULE.forbidden([name]), [name] if bad else [])

    def test_check_fails_only_on_load_time_runtime_import(self):
        cases = (
            (["KERNEL32.dll", "VCRUNTIME140.dll"], [], False),
            (["KERNEL32.dll", "ntdll.dll"], [], True),
            # A delay-loaded DLL is resolved on first call, not at start-up.
            (["KERNEL32.dll"], ["VCRUNTIME140.dll"], True),
        )
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "lean-ctx.exe"
            for load_time, delayed, ok in cases:
                with self.subTest(load_time=load_time, delayed=delayed):
                    binary.write_bytes(make_pe(load_time, delayed))
                    with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                        self.assertEqual(MODULE.check(binary), ok)

    def test_rejects_non_pe_and_truncated_images(self):
        image = make_pe(["KERNEL32.dll"])
        for data in (b"", b"#!/bin/sh\n", image[:0x60], image[:0x50] + b"\0" * 0x10):
            with self.subTest(size=len(data)):
                with self.assertRaises(MODULE.PEError):
                    MODULE.imports(data)


if __name__ == "__main__":
    unittest.main()
