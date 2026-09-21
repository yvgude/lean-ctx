"""Archive integrity checks must reject missing, duplicate and changed engines."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
import warnings
import zipfile

SPEC = importlib.util.spec_from_file_location(
    "windows_packages", Path(__file__).resolve().parents[2] / "scripts/verify-windows-packages.py"
)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class WindowsPackagesTest(unittest.TestCase):
    def test_archive_integrity(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "lean-ctx.exe"
            binary.write_bytes(b"signed engine bytes")
            cases = (
                ([("lean-ctx.exe", binary.read_bytes())], True),
                ([("thinkery_leanctx_engine/bin/lean-ctx.exe", binary.read_bytes())], True),
                ([("lean-ctx.exe", b"changed")], False),
                ([("README", b"no executable")], False),
                ([("lean-ctx.exe", binary.read_bytes())] * 2, False),
            )
            for entries, valid in cases:
                with self.subTest(entries=entries):
                    archive = Path(directory) / "package.zip"
                    with warnings.catch_warnings():
                        warnings.simplefilter("ignore", UserWarning)
                        with zipfile.ZipFile(archive, "w") as package:
                            for name, content in entries:
                                package.writestr(name, content)
                    if valid:
                        MODULE.verify(binary, archive)
                    else:
                        with self.assertRaises(ValueError):
                            MODULE.verify(binary, archive)


if __name__ == "__main__":
    unittest.main()
