# SPDX-License-Identifier: Apache-2.0
"""Canonical release asset names, independent of manifest claims."""

import re
from pathlib import Path


# Explicit, injective names: GitHub assets cannot retain the LICENSES directory.
SUPPLEMENTAL_SOURCES = {
    "SBOM.cdx.json": "SBOM.cdx.json",
    "THIRD_PARTY_NOTICES": "THIRD_PARTY_NOTICES",
    "LICENSE.md": "LICENSE.md",
    "LICENSES-Apache-2.0.txt": "LICENSES/Apache-2.0.txt",
}
# Previously signed mixed-source inventories remain verifiable. New Apache
# inventories never collect or generate this legacy supplemental artifact.
VERIFIABLE_SUPPLEMENTAL_SOURCES = {
    **SUPPLEMENTAL_SOURCES,
    "LICENSES-LeanCTX-Commercial-Source-License-2.0.txt":
        "LICENSES/LeanCTX-Commercial-Source-License-2.0.txt",
}
SIGNED_ENVELOPE = (
    "SHA256SUMS", "SHA256SUMS.sig", "SHA256SUMS.pem",
    "release-manifest.json", "release-manifest.json.sig", "release-manifest.json.pem",
)


def safe_filename(value):
    if (not isinstance(value, str)
            or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+-]*", value)
            or ".." in value):
        raise ValueError("release file name is unsafe")
    return value


def source_name(tag):
    return safe_filename(f"lean-ctx-{safe_filename(tag).removeprefix('v')}-source.tar.gz")


def artifact_kind(name, tag):
    safe_filename(name)
    if name in VERIFIABLE_SUPPLEMENTAL_SOURCES:
        return "supplemental"
    if name == source_name(tag):
        return "source"
    if ((name.startswith("lean-ctx-") and name.endswith((".tar.gz", ".zip")))
            or (name.startswith("thinkery_leanctx_engine") and name.endswith(".whl"))):
        return "binary"
    raise ValueError(f"unsupported release artifact: {name}")


def read_regular(path):
    if path.is_symlink():
        raise ValueError(f"{path.name}: release artifact must not be a symlink")
    if not path.is_file():
        raise ValueError(f"{path.name}: release artifact must be a regular file")
    return path.read_bytes()


def collect(root, tag):
    """Validate the full input set before materializing flat license assets."""
    root = Path(root)
    paths = {}
    for path in sorted([*root.glob("lean-ctx-*"), *root.glob("thinkery_leanctx_engine*.whl")]):
        if not path.is_symlink() and path.is_dir():
            continue
        artifact_kind(path.name, tag)
        read_regular(path)
        if path.name in paths or path.name in SUPPLEMENTAL_SOURCES:
            raise ValueError(f"release inventory collision: {path.name}")
        paths[path.name] = path
    if not any(artifact_kind(name, tag) == "binary" for name in paths):
        raise ValueError("release inventory contains no binary artifacts")
    copies = []
    for name, source in SUPPLEMENTAL_SOURCES.items():
        destination = root / safe_filename(name)
        original = root / source
        if any((root / parent).is_symlink() for parent in Path(source).parents):
            raise ValueError("release metadata parent must not be a symlink")
        data = read_regular(original)
        if source != name:
            if destination.exists() or destination.is_symlink():
                if read_regular(destination) != data:
                    raise ValueError(f"release inventory collision: {name}")
            else:
                copies.append((destination, data))
        paths[name] = destination
    for destination, data in copies:
        with destination.open("xb") as output:
            output.write(data)
    return dict(sorted(paths.items()))


def upload_paths(directory, artifacts):
    if set(artifacts) & (VERIFIABLE_SUPPLEMENTAL_SOURCES.keys() - SUPPLEMENTAL_SOURCES.keys()):
        raise ValueError("legacy supplemental assets are verification-only, not publishable")
    names = [*sorted(artifacts), *SIGNED_ENVELOPE]
    if len(names) != len(set(names)):
        raise ValueError("release upload inventory collision")
    for name in names:
        if not read_regular(directory / safe_filename(name)):
            raise ValueError(f"empty release upload asset: {name}")
    return names
