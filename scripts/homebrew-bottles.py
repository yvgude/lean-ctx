#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Build the macOS Homebrew bottles of a release from its published archives.

Without a bottle for the running macOS, `brew install` takes the build-from-
source path, and Homebrew then refuses outdated Command Line Tools or Xcode
before the formula's install step runs, although that step only copies the
prebuilt binary (Air test F57: macOS 26 asked for CLT 16.4). A bottle is
poured without those checks.

Each bottle holds the keg the formula's install step would create, nothing
else: `<name>/<version>/bin/lean-ctx` from lean-ctx-<target>.tar.gz and the
five signed Cockpit files under `share/lean-ctx/cockpit` from
lean-ctx-cockpit-<target>.tar.gz. The engine is built for macOS 11, so the
bottles carry the Big Sur tags; Homebrew pours a bottle for an older macOS on
every newer one of the same architecture. The archives are byte-for-byte the
release's, entries are sorted and their times fixed, so the same inputs give
the same bottle digests.

Usage: homebrew-bottles.py --version V --dist DIR --out DIR --block FILE
  --dist   holds the four lean-ctx[-cockpit]-<target>.tar.gz release assets,
           already checked against SHA256SUMS / COCKPIT_SHA256SUMS
  --block  receives the formula's `bottle do ... end` block
"""

import argparse
import gzip
import hashlib
import io
from pathlib import Path
import re
import sys
import tarfile

NAME = "lean-ctx"
ROOT_URL = "https://github.com/yvgude/lean-ctx/releases/download/v{version}"
# Release target -> Homebrew bottle tag (MACOSX_DEPLOYMENT_TARGET 11.0).
TARGETS = (
    ("aarch64-apple-darwin", "arm64_big_sur"),
    ("x86_64-apple-darwin", "big_sur"),
)
COCKPIT_FILES = ("archive.tar.gz", "channel.json", "channel.sig", "manifest.json", "manifest.sig")
VERSION_RE = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.]+)?$")


def regular_members(path):
    """Map member name -> bytes for the regular files of a release archive."""
    files = {}
    with tarfile.open(path, "r:gz") as archive:
        for member in archive.getmembers():
            if member.isdir():
                continue
            if not member.isfile():
                raise ValueError(f"{path.name}: {member.name} is not a regular file")
            if member.name in files:
                raise ValueError(f"{path.name}: {member.name} appears twice")
            files[member.name] = archive.extractfile(member).read()
    return files


def keg_files(dist, target):
    """The files of the installed keg, relative to <name>/<version>/."""
    engine = regular_members(dist / f"{NAME}-{target}.tar.gz")
    if set(engine) != {NAME}:
        raise ValueError(f"{NAME}-{target}.tar.gz must hold exactly the {NAME} binary")
    cockpit = regular_members(dist / f"{NAME}-cockpit-{target}.tar.gz")
    expected = {f"lean-ctx-cockpit/{name}" for name in COCKPIT_FILES}
    if set(cockpit) != expected:
        raise ValueError(f"{NAME}-cockpit-{target}.tar.gz must hold exactly {sorted(expected)}")
    files = {f"bin/{NAME}": (engine[NAME], 0o755)}
    for name in COCKPIT_FILES:
        files[f"share/lean-ctx/cockpit/{name}"] = (cockpit[f"lean-ctx-cockpit/{name}"], 0o644)
    return files


def bottle_bytes(version, files):
    """A gzip tarball of <name>/<version>/..., deterministic for the same files."""
    prefix = f"{NAME}/{version}"
    dirs = {prefix.split("/")[0], prefix}
    for path in files:
        parts = path.split("/")[:-1]
        for depth in range(1, len(parts) + 1):
            dirs.add(f"{prefix}/{'/'.join(parts[:depth])}")
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w", format=tarfile.PAX_FORMAT) as archive:
        for path in sorted(dirs):
            info = tarfile.TarInfo(path)
            info.type, info.mode, info.mtime = tarfile.DIRTYPE, 0o755, 0
            archive.addfile(info)
        for path in sorted(files):
            data, mode = files[path]
            info = tarfile.TarInfo(f"{prefix}/{path}")
            info.size, info.mode, info.mtime = len(data), mode, 0
            archive.addfile(info, io.BytesIO(data))
    out = io.BytesIO()
    with gzip.GzipFile(fileobj=out, mode="wb", mtime=0, filename="") as compressed:
        compressed.write(raw.getvalue())
    return out.getvalue()


def build(version, dist, out):
    """Write the bottles; return [(tag, filename, sha256)]."""
    if not VERSION_RE.match(version):
        raise ValueError(f"not a release version: {version!r}")
    out.mkdir(parents=True, exist_ok=True)
    bottles = []
    for target, tag in TARGETS:
        data = bottle_bytes(version, keg_files(dist, target))
        # The name Homebrew downloads from a root_url (Bottle::Filename#url_encode:
        # one hyphen; the double hyphen is only its local cache and GHCR name).
        filename = f"{NAME}-{version}.{tag}.bottle.tar.gz"
        (out / filename).write_bytes(data)
        bottles.append((tag, filename, hashlib.sha256(data).hexdigest()))
    return bottles


def bottle_block(version, bottles):
    """The formula's bottle block, indented for the top level of the class."""
    lines = ["  bottle do", f'    root_url "{ROOT_URL.format(version=version)}"']
    for tag, _, sha in bottles:
        lines.append(f'    sha256 cellar: :any_skip_relocation, {tag}: "{sha}"')
    lines.append("  end")
    return "\n".join(lines) + "\n"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--version", required=True)
    parser.add_argument("--dist", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--block", type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        bottles = build(args.version, args.dist, args.out)
    except (OSError, ValueError, tarfile.TarError) as error:
        print(f"::error::Homebrew bottles not built: {error}", file=sys.stderr)
        return 1
    args.block.write_text(bottle_block(args.version, bottles))
    for tag, filename, sha in bottles:
        print(f"{sha}  {filename}  ({tag})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
