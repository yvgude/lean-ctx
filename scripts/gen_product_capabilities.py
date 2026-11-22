#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Generate the legacy hosted catalog product-capabilities-v1.json from its registry.

The input is the frozen schema-2 registry
`ee/services/cloud/contracts/product-capabilities-registry.toml` (the Engine's
`product/capabilities.toml` moved to schema 3 for LeanCTX Cloud; the legacy
hosted catalog keeps its released state until it is retired, strands F2/F3).
This script is the only writer of the catalog
`ee/services/cloud/contracts/product-capabilities-v1.json`; a hand edit is
caught by `--check` (run by `scripts/sync-capabilities.py`).

The tier names below (`pro`, `team`, `enterprise`) are internal ids, not prices
or plan names.

Selection:

  a capability is signed into a hosted entitlement iff
      account_required and managed_cloud_available
      and price_class in {cloud_paid, team_paid, enterprise_paid}

  its tier is the registry's own plan projection of price_class:
      cloud_paid -> pro, team_paid -> team, enterprise_paid -> enterprise

  additionally, an account-licensed local capability is signed into the
  `pro` list iff
      price_class == pro_paid and account_required
      and its id is outside the Runtime namespace `pro.runtime.`

`pro.runtime.*` capabilities are licensed by the commercial device lease and
verified by the signed local Runtime (which admits only that namespace), so
they are never named here. Every other `pro_paid` capability (for example the
local dashboard's knowledge workflows) has no Runtime package to carry a lease:
the account's signed entitlement is its only licence.
`free` capabilities are never issued. `sdk_oem_commercial` grants no online right.

Usage:
  scripts/gen_product_capabilities.py --registry PATH --expect-sha256 HEX            # write
  scripts/gen_product_capabilities.py --registry PATH --expect-sha256 HEX --check    # compare only
"""
import argparse
import hashlib
import json
import sys
from pathlib import Path

if sys.version_info < (3, 11):
    sys.exit("gen_product_capabilities.py needs Python 3.11 or newer (tomllib)")
import tomllib  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / "ee" / "services" / "cloud" / "contracts" / "product-capabilities-v1.json"
TIER_OF_PRICE_CLASS = {"cloud_paid": "pro", "team_paid": "team", "enterprise_paid": "enterprise"}
KNOWN_PRICE_CLASSES = {"free", "pro_paid", "sdk_oem_commercial", *TIER_OF_PRICE_CLASS}
# Capabilities the commercial device lease licenses through the signed Runtime.
RUNTIME_LEASE_NAMESPACE = "pro.runtime."


def render(registry_bytes: bytes) -> str:
    registry = tomllib.loads(registry_bytes.decode("utf-8"))
    if registry.get("schema_version") != 2:
        raise SystemExit("registry schema_version must be 2")
    tiers = {"pro": [], "team": [], "enterprise": []}
    seen = set()
    for capability in registry["capability"]:
        identifier = capability["id"]
        if identifier in seen:
            raise SystemExit(f"duplicate capability id: {identifier}")
        seen.add(identifier)
        price_class = capability["price_class"]
        if price_class not in KNOWN_PRICE_CLASSES:
            raise SystemExit(f"unknown price_class {price_class!r} on {identifier}")
        if not isinstance(capability["account_required"], bool) or not isinstance(
            capability["managed_cloud_available"], bool
        ):
            raise SystemExit(f"non-boolean availability on {identifier}")
        tier = TIER_OF_PRICE_CLASS.get(price_class)
        if tier and capability["account_required"] and capability["managed_cloud_available"]:
            tiers[tier].append(identifier)
        elif (
            price_class == "pro_paid"
            and capability["account_required"]
            and not identifier.startswith(RUNTIME_LEASE_NAMESPACE)
        ):
            tiers["pro"].append(identifier)
    catalog = {
        "source_revision": "sha256:" + hashlib.sha256(registry_bytes).hexdigest(),
        "pro": sorted(tiers["pro"]),
        "team": sorted(tiers["team"]),
        "enterprise": sorted(tiers["enterprise"]),
    }
    return json.dumps(catalog, indent=2) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--registry", required=True, type=Path)
    parser.add_argument("--expect-sha256", required=True)
    parser.add_argument("--check", action="store_true")
    arguments = parser.parse_args()

    registry_bytes = arguments.registry.read_bytes()
    actual = hashlib.sha256(registry_bytes).hexdigest()
    if actual != arguments.expect_sha256:
        print(f"registry digest {actual} is not the expected {arguments.expect_sha256}", file=sys.stderr)
        return 2
    rendered = render(registry_bytes)
    if arguments.check:
        if OUTPUT.read_text() != rendered:
            print(f"{OUTPUT} is not what the registry generates", file=sys.stderr)
            return 1
        print(f"{OUTPUT.name} matches registry sha256 {actual}")
        return 0
    OUTPUT.write_text(rendered)
    print(f"wrote {OUTPUT.name} from registry sha256 {actual}")
    # No source constant pins these bytes since the billing service source was
    # removed (owner decision 4); `--check` guards them.
    print(f"catalog sha256 {hashlib.sha256(rendered.encode('utf-8')).hexdigest()}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
