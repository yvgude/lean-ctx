#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Generate the release manifest consumed by the secure updater."""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import stat
import tarfile
import zipfile

from release_inventory import artifact_kind, collect, read_regular


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def payload_digest(archive: pathlib.Path) -> str | None:
    if archive.suffix in (".zip", ".whl"):
        names = {"lean-ctx.exe"}
        if archive.suffix == ".whl":
            distribution = archive.name.split("-", 1)[0]
            if distribution not in {"thinkery_leanctx_engine", "thinkery_leanctx_engine_cuda", "thinkery_leanctx_engine_windows_gnu"}:
                raise ValueError("unsupported companion wheel distribution")
            names = {f"{distribution}/bin/lean-ctx", f"{distribution}/bin/lean-ctx.exe"}
        with zipfile.ZipFile(archive) as bundle:
            members = [m for m in bundle.infolist() if not m.is_dir() and m.filename in names
                       and stat.S_IFMT(m.external_attr >> 16) != stat.S_IFLNK]
            if len(members) != 1:
                raise ValueError(f"{archive.name}: expected exactly one lean-ctx.exe")
            return digest(bundle.read(members[0]))
    if archive.name.endswith(".tar.gz"):
        with tarfile.open(archive, "r:gz") as bundle:
            members = [m for m in bundle.getmembers() if m.isfile() and m.name == "lean-ctx"]
            if len(members) != 1:
                raise ValueError(f"{archive.name}: expected exactly one lean-ctx")
            payload = bundle.extractfile(members[0])
            if payload is None:
                raise ValueError(f"{archive.name}: unreadable payload")
            return digest(payload.read())
    raise ValueError(f"{archive.name}: unsupported archive")


def generate(root: pathlib.Path, tag: str, commit: str) -> dict[str, object]:
    artifacts: dict[str, dict[str, object]] = {}
    from release_inventory import SUPPLEMENTAL_SOURCES

    for name, archive in collect(root, tag).items():
        kind = artifact_kind(name, tag)
        record: dict[str, object] = {
            "sha256": digest(read_regular(archive)), "size": archive.stat().st_size, "kind": kind,
        }
        if kind == "binary":
            record["payload_sha256"] = payload_digest(archive)
        elif kind == "supplemental":
            record["source_path"] = SUPPLEMENTAL_SOURCES[name]
        artifacts[archive.name] = record
    sums = "".join(f"{record['sha256']}  {name}\n" for name, record in artifacts.items())
    if (root / "SHA256SUMS").is_symlink():
        raise ValueError("SHA256SUMS must not be a symlink")
    (root / "SHA256SUMS").write_text(sums, encoding="utf-8")
    return {
        "schema_version": "leanctx.release-manifest/v1",
        "tag": tag,
        "commit": commit,
        "artifacts": artifacts,
        "sbom_sha256": digest(read_regular(root / "SBOM.cdx.json")),
        "checksums_sha256": digest(read_regular(root / "SHA256SUMS")),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=pathlib.Path, default=pathlib.Path("."))
    parser.add_argument("--tag", required=True)
    parser.add_argument("--commit", required=True)
    args = parser.parse_args()
    manifest = generate(args.root, args.tag, args.commit)
    if (args.root / "release-manifest.json").is_symlink():
        raise ValueError("release-manifest.json must not be a symlink")
    (args.root / "release-manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
