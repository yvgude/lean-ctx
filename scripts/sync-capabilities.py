#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Bring everything derived from ``product/capabilities.toml`` in step with it.

    python3 scripts/sync-capabilities.py            # --check: report drift, change nothing
    python3 scripts/sync-capabilities.py --write    # rewrite the derived files
    python3 scripts/sync-capabilities.py --tree DIR # another checkout than this script's

The registry is the source; nothing here edits it. The chain, in order:

1. the byte mirrors named in ``product/tier-map.json`` ``registry_mirrors`` get
   the registry's bytes (or its public projection);
2. the legacy hosted services stay on their frozen schema-2 registry
   (``legacy_hosted.registry`` in the tier map; Hive replaces them in strands
   F2/F3): its ``mirrors`` get its bytes, the tree's own generator
   ``ee/services/cloud/scripts/gen_product_capabilities.py`` (run as a
   subprocess, bound to that registry's digest) writes or checks the catalog
   draft ``ee/services/cloud/contracts/product-capabilities-v1.json``;
3. the digest constants in the billing service and the Edge are set to the
   digests of those frozen bytes. No digest is typed by hand.

Exit 0: in step. Exit 1: drift (``--check``) or a step that could not be done.
A missing file, a pin whose pattern no longer matches and a failing generator
are failures in both modes; ``--write`` stops at a failing generator before any
pin is touched.

Not done here: the generated matrix ``docs/reference/generated/product-capabilities.md``
(``cargo run --example gen_product_capability_matrix``, needs a build); the
Engine and Edge tests that pin hosted lists (they change only when
``catalog_lists_changed`` is true). Nothing live: no catalog is signed or
published and no deployed service is contacted. Needs the complete checkout;
``scripts/check-capabilities.py`` is the read-only gate for both profiles.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
REGISTRY = "product/capabilities.toml"
MAP_PATH = "product/tier-map.json"
CATALOG = "ee/services/cloud/contracts/product-capabilities-v1.json"
GENERATOR = "ee/services/cloud/scripts/gen_product_capabilities.py"
# (file, constant, pattern with the digest as group 2, which bytes it pins)
PINS = (
    ("ee/services/cloud/src/entitlement_issuer.rs", "CANONICAL_REGISTRY_DIGEST",
     r'(const CANONICAL_REGISTRY_DIGEST: &str =\s*"sha256:)([0-9a-f]{64})(";)', "registry"),
    ("ee/services/cloud/src/entitlement_issuer.rs", "CATALOG_SHA256",
     r'(const CATALOG_SHA256: &str = ")([0-9a-f]{64})(";)', "catalog"),
    ("ee/services/edge/src/cloud_server/capability_entitlement.rs", "PRODUCT_CAPABILITIES_SHA256",
     r'(const PRODUCT_CAPABILITIES_SHA256: &str =\s*")([0-9a-f]{64})(";)', "registry"),
)
NOT_DONE_HERE = [
    "docs/reference/generated/product-capabilities.md (cargo example gen_product_capability_matrix)",
    "Engine/Edge tests that pin hosted lists (only if catalog_lists_changed)",
    "anything live: no publication, no signing, no deployed service",
]


def _projection_tool():
    """`scripts/project-capabilities.py`, the one definition of the public projection."""
    import importlib.util
    path = Path(__file__).with_name("project-capabilities.py")
    spec = importlib.util.spec_from_file_location("project_capabilities", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def lists(text: str) -> dict:
    """The catalog without the field that only restates the registry digest."""
    return {key: value for key, value in (json.loads(text) if text else {}).items()
            if key != "source_revision"}


def run(tree: Path, write: bool) -> tuple[dict, list[str]]:
    """The report and the failures; an empty failure list means in step."""
    failures: list[str] = []
    report: dict = {"tree": str(tree), "mode": "write" if write else "check",
                    "mirrors": {}, "pins": {}, "not_done_here": NOT_DONE_HERE}
    missing = [name for name in (REGISTRY, MAP_PATH, GENERATOR, *(pin[0] for pin in PINS))
               if not (tree / name).is_file()]
    if missing:
        return report, ["MISSING (complete checkout needed): " + ", ".join(sorted(set(missing)))]

    registry_path = tree / REGISTRY
    registry = registry_path.read_bytes()
    registry_digest = sha(registry)
    report["registry_sha256"] = registry_digest
    tier_map = json.loads((tree / MAP_PATH).read_text(encoding="utf-8"))
    mirrors = tier_map.get("registry_mirrors")
    if not isinstance(mirrors, list) or not mirrors:
        return report, [MAP_PATH + " names no registry_mirrors"]
    legacy = tier_map.get("legacy_hosted") or {}
    if not legacy.get("registry") or not (tree / legacy["registry"]).is_file():
        return report, [MAP_PATH + " names no existing legacy_hosted.registry"]

    for mirror in mirrors:
        path = tree / mirror
        current = path.read_bytes() if path.is_file() else b""
        # A mirror that declares itself the public projection (owner decision
        # E2) carries `project-capabilities.py`'s output, not the raw bytes.
        projected = re.search(rb'(?m)^projection\s*=\s*"public"', current) is not None
        expected = (_projection_tool().project(registry.decode("utf-8")).encode("utf-8")
                    if projected else registry)
        if current == expected:
            report["mirrors"][mirror] = "in step" + (" (public projection)" if projected else "")
        elif write:
            path.write_bytes(expected)
            report["mirrors"][mirror] = "written" + (" (public projection)" if projected else "")
        else:
            report["mirrors"][mirror] = "OUT OF STEP"
            failures.append("mirror " + mirror + " differs from the registry"
                            + (" projection" if projected else ""))

    # The legacy hosted services decide from their frozen schema-2 registry.
    registry_path = tree / legacy["registry"]
    registry = registry_path.read_bytes()
    registry_digest = sha(registry)
    report["legacy_registry_sha256"] = registry_digest
    for mirror in legacy.get("mirrors", []):
        path = tree / mirror
        current = path.read_bytes() if path.is_file() else b""
        if current == registry:
            report["mirrors"][mirror] = "in step (legacy frozen)"
        elif write:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(registry)
            report["mirrors"][mirror] = "written (legacy frozen)"
        else:
            report["mirrors"][mirror] = "OUT OF STEP"
            failures.append("legacy mirror " + mirror + " differs from " + legacy["registry"])

    catalog_path = tree / CATALOG
    before = catalog_path.read_text(encoding="utf-8") if catalog_path.is_file() else ""
    command = [sys.executable, "-B", str(tree / GENERATOR), "--registry", str(registry_path),
               "--expect-sha256", registry_digest] + ([] if write else ["--check"])
    result = subprocess.run(command, capture_output=True, text=True, check=False)
    report["catalog_generator"] = {"exit": result.returncode,
                                   "output": (result.stdout + result.stderr).strip().splitlines()}
    if result.returncode != 0:
        failures.append("GENERATOR FAILED (exit %d): %s" % (
            result.returncode, GENERATOR + (" --check" if not write else "")))
        if write:  # no pin is moved to a catalog the generator did not produce
            return report, failures
    if not catalog_path.is_file():
        return report, failures + ["MISSING: " + CATALOG]
    catalog = catalog_path.read_bytes()
    catalog_digest = sha(catalog)
    report["catalog_sha256"] = catalog_digest
    try:
        report["catalog_lists_changed"] = lists(before) != lists(catalog.decode("utf-8"))
        report["catalog_source_revision"] = json.loads(catalog)["source_revision"]
    except (ValueError, KeyError) as error:
        return report, failures + ["catalog is not the generator's JSON: " + str(error)]

    digests = {"registry": registry_digest, "catalog": catalog_digest}
    for relative, name, pattern, which in PINS:
        path = tree / relative
        text = path.read_text(encoding="utf-8")
        matches = list(re.finditer(pattern, text))
        if len(matches) != 1:
            report["pins"][name] = "PATTERN NOT FOUND in " + relative
            failures.append("PATTERN NOT FOUND: %s in %s (%d matches)" % (name, relative, len(matches)))
            continue
        match, wanted = matches[0], digests[which]
        if match.group(2) == wanted:
            report["pins"][name] = "in step"
        elif write:
            path.write_text(text[:match.start(2)] + wanted + text[match.end(2):], encoding="utf-8")
            report["pins"][name] = "written (" + match.group(2)[:12] + "… → " + wanted[:12] + "…)"
        else:
            report["pins"][name] = ("OUT OF STEP (pins " + match.group(2)[:12] + "…, bytes are "
                                    + wanted[:12] + "…)")
            failures.append("pin " + name + " in " + relative + " differs from the " + which + " bytes")
    return report, failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--tree", type=Path, default=ROOT,
                        help="checkout to work on (default: the one this script is in)")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true", help="report drift, change nothing (default)")
    mode.add_argument("--write", action="store_true", help="rewrite mirrors, catalog draft and pins")
    arguments = parser.parse_args()
    report, failures = run(arguments.tree.resolve(), arguments.write)
    report["in_step"] = not failures
    report["failures"] = failures
    print(json.dumps(report, indent=1, ensure_ascii=False))
    for failure in failures:
        print("sync-capabilities: " + failure, file=sys.stderr)
    return 0 if not failures else 1


if __name__ == "__main__":
    sys.exit(main())
