# SPDX-License-Identifier: Apache-2.0
"""scripts/scan-release-artifacts.py: private content in a release artifact is found, at any
nesting depth, and a clean artifact passes."""
import importlib.util
import io
import struct
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "scan-release-artifacts.py"
spec = importlib.util.spec_from_file_location("scan_release_artifacts", SCRIPT)
scan_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(scan_module)


def zipped(members: dict[str, bytes]) -> bytes:
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as archive:
        for name, data in members.items():
            archive.writestr(name, data)
    return buffer.getvalue()


def tarred(members: dict[str, bytes]) -> bytes:
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as archive:
        for name, data in members.items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            archive.addfile(info, io.BytesIO(data))
    return buffer.getvalue()


def elf64(sections: list[str], payload: bytes = b"") -> bytes:
    """A little-endian ELF64 image whose section table names `sections`."""
    names = ["", *sections, ".shstrtab"]
    strtab, offsets = b"", []
    for name in names:
        offsets.append(len(strtab))
        strtab += name.encode() + b"\0"
    data_at = 64
    shoff = data_at + len(strtab) + len(payload)
    header = bytearray(b"\x7fELF\x02\x01\x01" + b"\0" * 57)
    struct.pack_into("<Q", header, 0x28, shoff)
    struct.pack_into("<HHH", header, 0x3A, 64, len(names), len(names) - 1)
    table = b""
    for index, offset in enumerate(offsets):
        entry = bytearray(64)
        struct.pack_into("<I", entry, 0, offset)
        if index == len(names) - 1:
            struct.pack_into("<Q", entry, 0x18, data_at)
        table += bytes(entry)
    return bytes(header) + strtab + payload + table


def macho_universal(segments: list[str]) -> bytes:
    """A universal (fat) Mach-O holding one 64-bit image with these segments."""
    commands = b"".join(struct.pack("<II", 0x19, 72) + name.encode().ljust(16, b"\0") + b"\0" * 48
                        for name in segments)
    image = struct.pack("<IIIIIIII", 0xFEEDFACF, 0x0100000C, 0, 2, len(segments), len(commands), 0, 0)
    offset = 4096
    fat = struct.pack(">II", 0xCAFEBABE, 1) + struct.pack(">IIIII", 0x0100000C, 0, offset, len(image) + len(commands), 12)
    return fat.ljust(offset, b"\0") + image + commands


def pe(sections: list[str]) -> bytes:
    """A PE image; a name longer than 8 bytes goes to the COFF string table as MinGW writes it."""
    strings, raw = b"", []
    for name in sections:
        if len(name) > 8:
            raw.append(f"/{4 + len(strings)}".encode())
            strings += name.encode() + b"\0"
        else:
            raw.append(name.encode())
    table_at = 0x40 + 24
    string_table_at = table_at + 40 * len(sections)
    header = bytearray(b"MZ".ljust(0x40, b"\0"))
    struct.pack_into("<I", header, 0x3C, 0x40)
    coff = b"PE\0\0" + struct.pack("<HHIIIHH", 0x8664, len(sections), 0, string_table_at, 0, 0, 0)
    table = b"".join(name.ljust(8, b"\0") + b"\0" * 32 for name in raw)
    return bytes(header) + coff + table + struct.pack("<I", 4 + len(strings)) + strings


class ScanTests(unittest.TestCase):
    def scan(self, name: str, data: bytes):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / name
            path.write_bytes(data)
            return scan_module.scan([path])

    def test_a_clean_package_passes(self):
        wheel = zipped({"leanctx/__init__.py": b"VERSION = '1'\n", "leanctx-1.dist-info/METADATA": b"Name: x"})
        self.assertEqual(self.scan("leanctx-1-py3-none-any.whl", wheel), [])

    def test_a_private_member_path_in_a_wheel_is_found(self):
        wheel = zipped({"leanctx/ee/team/hub.py": b"pass\n"})
        findings = self.scan("pkg.whl", wheel)
        self.assertTrue(any("private path" in f["finding"] for f in findings))

    def test_a_private_source_path_inside_a_binary_is_found(self):
        binary = b"\x7fELF" + b"\x00" * 64 + b"ee/services/cloud/src/team_features/leases.rs" + b"\x00"
        findings = self.scan("lean-ctx", binary)
        self.assertEqual([f["finding"] for f in findings], ["private marker `ee/services/`"])

    def test_nested_archives_are_opened(self):
        inner = zipped({"src/commercial.rs": b"// SPDX-License-" b"Identifier: LicenseRef-Proprietary\n"})
        nupkg = zipped({"content/inner.zip": inner})
        crate = tarred({"lean-ctx-1/pkg.nupkg": nupkg})
        findings = self.scan("lean-ctx-1.crate", crate)
        self.assertTrue(any(f["artifact"].endswith("inner.zip!src/commercial.rs")
                            and "LicenseRef-Proprietary" in f["finding"] for f in findings))

    def test_the_bare_license_identifier_of_runtime_verification_is_no_finding(self):
        # The public Engine checks a commercial Runtime's manifest for this identifier.
        binary = b"\x7fELF" + b"\x00" * 64 + b"LicenseRef-Proprietary" + b"\x00"
        self.assertEqual(self.scan("lean-ctx", binary), [])

    def test_debug_information_in_an_executable_is_found_by_its_section_table(self):
        cases = {
            "lean-ctx": (elf64([".text", ".debug_info", ".zdebug_line"]),
                         ["debug information `.debug_info`", "debug information `.zdebug_line`"]),
            "lean-ctx-macos": (macho_universal(["__TEXT", "__DWARF"]), ["debug information `__DWARF`"]),
            "lean-ctx.exe": (pe([".text", ".debug_info"]), ["debug information `.debug_info`"]),
        }
        for name, (image, expected) in cases.items():
            with self.subTest(name):
                self.assertEqual([f["finding"] for f in self.scan(name, image)], expected)

    def test_a_stripped_executable_naming_debug_sections_in_its_data_passes(self):
        # Backtrace code carries ".debug_info" as a string; only a section is debug information.
        self.assertEqual(self.scan("lean-ctx", elf64([".text", ".rodata"], b".debug_info\0__DWARF\0")), [])
        self.assertEqual(self.scan("lean-ctx-macos", macho_universal(["__TEXT", "__LINKEDIT"])), [])
        self.assertEqual(self.scan("lean-ctx.exe", pe([".text", ".rdata"])), [])

    def test_a_truncated_section_table_fails_closed(self):
        image = elf64([".text", ".debug_info"])
        findings = self.scan("lean-ctx", image[: len(image) - 40])
        self.assertEqual([f["finding"] for f in findings],
                         ["debug information `unreadable section table (ValueError)`"])

    def test_an_unreadable_archive_fails_closed(self):
        findings = self.scan("broken.tgz", b"not a tar at all")
        self.assertTrue(any("unreadable archive" in f["finding"] for f in findings))


if __name__ == "__main__":
    unittest.main()
