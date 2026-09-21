#!/usr/bin/env python3
"""Verify ZIP and wheel embed exactly the already verified signed engine."""
import argparse
import hashlib
from pathlib import Path
import zipfile


def verify(binary: Path, archive: Path) -> None:
    expected = binary.read_bytes()
    with zipfile.ZipFile(archive) as package:
        engines = [name for name in package.namelist() if name.split("/")[-1] == "lean-ctx.exe"]
        if len(engines) != 1:
            raise ValueError(f"{archive}: expected exactly one lean-ctx.exe")
        if package.read(engines[0]) != expected:
            raise ValueError(f"{archive}: packaged engine differs from signed binary")
    print(f"{archive}: signed engine SHA256 {hashlib.sha256(expected).hexdigest()}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("archives", type=Path, nargs="+")
    args = parser.parse_args()
    for archive in args.archives:
        verify(args.binary, archive)
