#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Build the Homebrew bottles and the tap formula for a LeanCTX release.

The tap formula installs the signed release binaries. Without a bottle,
Homebrew treats every formula as a source build and refuses to install it on
macOS unless current Command Line Tools are present, even when `install` only
copies a prebuilt file (F57). Each release therefore repacks the verified
release archives into bottles, one per platform, and the formula lists them.

Subcommands:

  bottles  Repack the engine and Cockpit archives into bottle archives and
           write HOMEBREW_BOTTLES_SHA256SUMS next to them.
  formula  Print Formula/lean-ctx.rb for the tap.

Both are deterministic: the same inputs always give the same bytes.
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import re
import sys
import tarfile
from pathlib import Path

FORMULA_NAME = "lean-ctx"
BOTTLE_SUMS = "HOMEBREW_BOTTLES_SHA256SUMS"
COCKPIT_FILES = (
    "archive.tar.gz",
    "channel.json",
    "channel.sig",
    "manifest.json",
    "manifest.sig",
)

# Release target -> Homebrew bottle tag. On macOS Homebrew pours a bottle
# built for an older macOS on any newer one of the same architecture, so one
# bottle per architecture covers every macOS from Monterey on. The binaries
# target macOS 11, but Big Sur is already scheduled for removal from
# Homebrew's version table and an unknown tag would no longer match anywhere.
BOTTLE_TAGS = {
    "aarch64-apple-darwin": "arm64_monterey",
    "x86_64-apple-darwin": "monterey",
    "aarch64-unknown-linux-gnu": "arm64_linux",
    "x86_64-unknown-linux-gnu": "x86_64_linux",
}

VERSION_RE = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.]+)?$")
SHA_RE = re.compile(r"^[0-9a-f]{64}$")


def fail(message: str) -> None:
    raise SystemExit(f"error: {message}")


def check_version(version: str) -> str:
    if not VERSION_RE.match(version):
        fail(f"invalid version {version!r}")
    return version


def bottle_name(version: str, tag: str) -> str:
    """The name Homebrew downloads from root_url (Bottle::Filename#url_encode).

    Homebrew caches the file locally as `lean-ctx--<version>...`; the URL
    uses a single dash.
    """
    return f"{FORMULA_NAME}-{version}.{tag}.bottle.tar.gz"


def read_sums(path: Path) -> dict[str, str]:
    """Parse a sha256sum file; every file name must be unique."""
    sums: dict[str, str] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        parts = line.split()
        if len(parts) != 2 or not SHA_RE.match(parts[0]):
            fail(f"{path.name}: malformed line {line!r}")
        name = parts[1].lstrip("*")
        if name in sums:
            fail(f"{path.name}: duplicate entry for {name}")
        sums[name] = parts[0]
    return sums


def digest(sums: dict[str, str], name: str, source: str) -> str:
    value = sums.get(name)
    if value is None:
        fail(f"{source} has no entry for {name}")
    return value


def _regular_members(name: str, data: bytes) -> dict[str, bytes]:
    """Parse the archive from the bytes whose digest was checked."""
    files: dict[str, bytes] = {}
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as tar:
        for member in tar.getmembers():
            if member.isdir():
                continue
            if not member.isfile():
                fail(f"{name}: {member.name} is not a regular file")
            handle = tar.extractfile(member)
            assert handle is not None
            files[member.name] = handle.read()
    return files


def read_engine(name: str, data: bytes) -> bytes:
    files = _regular_members(name, data)
    if set(files) != {"lean-ctx"}:
        fail(f"{name}: expected exactly one file lean-ctx, found {sorted(files)}")
    return files["lean-ctx"]


def read_cockpit(name: str, data: bytes) -> dict[str, bytes]:
    files = _regular_members(name, data)
    expected = {f"lean-ctx-cockpit/{n}" for n in COCKPIT_FILES}
    if set(files) != expected:
        fail(f"{name}: unexpected Cockpit bundle layout {sorted(files)}")
    return {n: files[f"lean-ctx-cockpit/{n}"] for n in COCKPIT_FILES}


def bottle_bytes(version: str, engine: bytes, cockpit: dict[str, bytes]) -> bytes:
    """A bottle unpacks to <cellar>/lean-ctx/<version>/ like the formula install."""
    prefix = f"{FORMULA_NAME}/{version}"
    entries: list[tuple[str, bytes | None, int]] = [
        (FORMULA_NAME, None, 0o755),
        (prefix, None, 0o755),
        (f"{prefix}/bin", None, 0o755),
        (f"{prefix}/bin/lean-ctx", engine, 0o755),
        (f"{prefix}/share", None, 0o755),
        (f"{prefix}/share/lean-ctx", None, 0o755),
        (f"{prefix}/share/lean-ctx/cockpit", None, 0o755),
    ]
    entries += [
        (f"{prefix}/share/lean-ctx/cockpit/{name}", cockpit[name], 0o644)
        for name in COCKPIT_FILES
    ]

    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w", format=tarfile.PAX_FORMAT) as tar:
        for name, data, mode in entries:
            info = tarfile.TarInfo(name)
            info.mode = mode
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            info.mtime = 0
            if data is None:
                info.type = tarfile.DIRTYPE
                tar.addfile(info)
            else:
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))
    out = io.BytesIO()
    with gzip.GzipFile(fileobj=out, mode="wb", mtime=0, filename="") as gz:
        gz.write(raw.getvalue())
    return out.getvalue()


def cmd_bottles(args: argparse.Namespace) -> int:
    version = check_version(args.version)
    engine_dir, cockpit_dir, out_dir = Path(args.engine_dir), Path(args.cockpit_dir), Path(args.out)
    engine_sums = read_sums(Path(args.sha256sums))
    cockpit_sums = read_sums(Path(args.cockpit_sha256sums))
    out_dir.mkdir(parents=True, exist_ok=True)

    lines = []
    for target, tag in BOTTLE_TAGS.items():
        engine_archive = engine_dir / f"lean-ctx-{target}.tar.gz"
        cockpit_archive = cockpit_dir / f"lean-ctx-cockpit-{target}.tar.gz"
        # Read each archive once: the bytes that match the signed checksum
        # are the bytes that are unpacked.
        verified = {}
        for archive, sums, source in (
            (engine_archive, engine_sums, "SHA256SUMS"),
            (cockpit_archive, cockpit_sums, "COCKPIT_SHA256SUMS"),
        ):
            expected = digest(sums, archive.name, source)
            raw = archive.read_bytes()
            if hashlib.sha256(raw).hexdigest() != expected:
                fail(f"{archive.name} does not match {source}")
            verified[source] = raw
        data = bottle_bytes(
            version,
            read_engine(engine_archive.name, verified["SHA256SUMS"]),
            read_cockpit(cockpit_archive.name, verified["COCKPIT_SHA256SUMS"]),
        )
        name = bottle_name(version, tag)
        (out_dir / name).write_bytes(data)
        lines.append(f"{hashlib.sha256(data).hexdigest()}  {name}\n")
    (out_dir / BOTTLE_SUMS).write_text("".join(lines), encoding="utf-8")
    return 0


FORMULA_TEMPLATE = """\
class LeanCtx < Formula
  desc "Local LeanCTX Context Gateway for AI systems"
  homepage "https://leanctx.com"
  version "{version}"
  # lean-ctx: Apache-2.0. Bundled LeanCTX Cockpit binary: LeanCTX Cockpit
  # EULA (LicenseRef-LeanCTX-Cockpit-EULA), which SPDX cannot express.
  license all_of: ["Apache-2.0", :cannot_represent]

  # Prebuilt bottles of the signed release binaries. Without them Homebrew
  # handles this formula as a source build and asks for current Command Line
  # Tools on macOS, although nothing is compiled.
  bottle do
    root_url "{release_url}"
{bottle_lines}
  end

  # Semantic search (ctx_semantic_search / embeddings) loads
  # libonnxruntime at runtime; lean-ctx resolves it from the
  # Homebrew prefix lib dir. Without this dependency the dylib is
  # absent and ORT init fails. See issue #544.
  depends_on "onnxruntime"

  on_macos do
    if Hardware::CPU.arm?
{macos_arm}
    else
{macos_intel}
    end
  end

  on_linux do
    if Hardware::CPU.arm?
{linux_arm}
    else
{linux_intel}
    end
  end

  def install
    bin.install "lean-ctx"
    # The signed Cockpit bundle; lean-ctx verifies and imports it from
    # share/lean-ctx/cockpit on the first `lean-ctx cockpit`.
    resource("cockpit").stage do
      (share/"lean-ctx/cockpit").install "channel.json", "channel.sig",
                                         "archive.tar.gz", "manifest.json", "manifest.sig"
    end
  end

  def caveats
    <<~TEXT
      lean-ctx is licensed under Apache-2.0.
      The bundled LeanCTX Cockpit (lean-ctx cockpit) is free to use, including
      commercially, under the LeanCTX Cockpit EULA; it is not open source.
    TEXT
  end

  test do
    assert_match "lean-ctx {version}", shell_output("#{{bin}}/lean-ctx --version")
    assert_path_exists share/"lean-ctx/cockpit/manifest.json"
  end
end
"""


def _platform_block(release_url: str, target: str, sha: str, cockpit_sha: str) -> str:
    return (
        f'      url "{release_url}/lean-ctx-{target}.tar.gz"\n'
        f'      sha256 "{sha}"\n'
        "\n"
        '      resource "cockpit" do\n'
        f'        url "{release_url}/lean-ctx-cockpit-{target}.tar.gz"\n'
        f'        sha256 "{cockpit_sha}"\n'
        "      end"
    )


def render_formula(
    version: str,
    release_url: str,
    engine_sums: dict[str, str],
    cockpit_sums: dict[str, str],
    bottle_sums: dict[str, str],
) -> str:
    check_version(version)
    if not release_url.startswith("https://") or release_url.endswith("/"):
        fail(f"invalid release url {release_url!r}")
    bottle_lines = "\n".join(
        f"    sha256 cellar: :any_skip_relocation, {tag}: "
        f'"{digest(bottle_sums, bottle_name(version, tag), BOTTLE_SUMS)}"'
        for tag in BOTTLE_TAGS.values()
    )
    blocks = {
        key: _platform_block(
            release_url,
            target,
            digest(engine_sums, f"lean-ctx-{target}.tar.gz", "SHA256SUMS"),
            digest(cockpit_sums, f"lean-ctx-cockpit-{target}.tar.gz", "COCKPIT_SHA256SUMS"),
        )
        for key, target in (
            ("macos_arm", "aarch64-apple-darwin"),
            ("macos_intel", "x86_64-apple-darwin"),
            ("linux_arm", "aarch64-unknown-linux-gnu"),
            ("linux_intel", "x86_64-unknown-linux-gnu"),
        )
    }
    return FORMULA_TEMPLATE.format(
        version=version, release_url=release_url, bottle_lines=bottle_lines, **blocks
    )


def cmd_formula(args: argparse.Namespace) -> int:
    sys.stdout.write(
        render_formula(
            args.version,
            args.release_url,
            read_sums(Path(args.sha256sums)),
            read_sums(Path(args.cockpit_sha256sums)),
            read_sums(Path(args.bottle_sha256sums)),
        )
    )
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)

    bottles = sub.add_parser("bottles", help="repack release archives into bottles")
    bottles.add_argument("--version", required=True)
    bottles.add_argument("--engine-dir", required=True)
    bottles.add_argument("--cockpit-dir", required=True)
    bottles.add_argument("--sha256sums", required=True)
    bottles.add_argument("--cockpit-sha256sums", required=True)
    bottles.add_argument("--out", required=True)
    bottles.set_defaults(func=cmd_bottles)

    formula = sub.add_parser("formula", help="print the tap formula")
    formula.add_argument("--version", required=True)
    formula.add_argument("--release-url", required=True)
    formula.add_argument("--sha256sums", required=True)
    formula.add_argument("--cockpit-sha256sums", required=True)
    formula.add_argument("--bottle-sha256sums", required=True)
    formula.set_defaults(func=cmd_formula)

    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
