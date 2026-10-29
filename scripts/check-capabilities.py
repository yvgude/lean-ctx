#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Check that the capability registry, the tier map and the gating code agree.

``product/capabilities.toml`` says what each capability costs and how it is
delivered. ``product/tier-map.json`` says which tier a price class is, which
code enforces it and which artifact ships it. This check reads both and the
named source files; it compiles nothing and uses no network.

Profile ``product`` needs the complete checkout. Profile ``community`` skips
every statement about ``ee/`` and so holds on a checkout without it. On the
host export, which also withholds the tier map, it checks the registry against
the Engine's copy and enums alone.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import sys

try:
    import tomllib
except ModuleNotFoundError:  # Python < 3.11: the registry's narrow layout is read directly.
    tomllib = None

ROOT = Path(__file__).resolve().parents[1]
MAP_PATH = "product/tier-map.json"
# What the Apache host export carries when the tier map is withheld from it.
EXPORT_REGISTRY = "product/capabilities.toml"
EXPORT_MIRROR = "rust/data/product-capabilities.toml"
EXPORT_ENUMS = "rust/src/core/product_capabilities.rs"
PRIVATE_ROOTS = ("ee", "sdk")
TEST_FILE = re.compile(r"(^|/)(tests?|fixtures|vendor|archive)(/|$)|(^|[_/])tests?\.rs$")


def _projection_tool():
    """`scripts/project-capabilities.py`, the one definition of the public projection."""
    path = Path(__file__).with_name("project-capabilities.py")
    spec = importlib.util.spec_from_file_location("project_capabilities", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def parse_registry(text: str) -> dict:
    """The registry is flat: one optional key table, array tables, scalar values."""
    if tomllib is not None:
        return tomllib.loads(text)
    result: dict = {}
    current = result
    for number, raw in enumerate(text.splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if re.fullmatch(r"\[\[[a-z_]+\]\]", line):
            current = {}
            result.setdefault(line[2:-2], []).append(current)
        elif re.fullmatch(r"\[[a-z_]+\]", line):
            current = result.setdefault(line[1:-1], {})
        else:
            match = re.fullmatch(r'("[^"\\]+"|[A-Za-z0-9_]+) = ("(?:[^"\\]|\\.)*"|true|false|[0-9]+)', line)
            if not match:
                raise ValueError(f"registry line {number}: unsupported TOML")
            key = match.group(1).strip('"')
            if key in current:
                raise ValueError(f"registry line {number}: duplicate key {key}")
            current[key] = json.loads(match.group(2))
    return result


def host_file(root: Path, source: str) -> bool:
    """Whether `source` names a regular file of this checkout's Apache host.

    `root` is canonical. The path must be relative, without `..`, outside the private
    roots as written AND as resolved: a link that leaves the checkout or lands in a
    private root is not a file of the host, however the path is spelled.
    """
    path = Path(source)
    if (not source or not path.parts or path.is_absolute() or ".." in path.parts
            or path.parts[0] in PRIVATE_ROOTS):
        return False
    try:
        resolved = (root / path).resolve(strict=True)
    except (OSError, RuntimeError):
        return False
    if not resolved.is_relative_to(root) or not resolved.is_file():
        return False
    inside = resolved.relative_to(root).parts
    return bool(inside) and inside[0] not in PRIVATE_ROOTS


# The commercial device lease licenses this namespace through the signed Runtime.
RUNTIME_LEASE_NAMESPACE = "pro.runtime."


def account_licensed_pro(capability: dict) -> bool:
    """A `pro_paid` capability outside the Runtime namespace has no Runtime
    package to carry a lease: the account's signed entitlement (hosted catalog
    list `pro`) is its licence. Same rule as gen_product_capabilities.py."""
    return (capability["price_class"] == "pro_paid" and capability["account_required"]
            and not capability["id"].startswith(RUNTIME_LEASE_NAMESPACE))


REGISTRY_SCHEMA = 3
LEGACY_SCHEMA = 2


def tier_of(capability: dict, price_classes: dict):
    """The tier a capability's price class projects to: fixed per class, by
    `hive_plan` for the `hive` class, None for a retired class."""
    entry = price_classes.get(capability["price_class"], {})
    if entry.get("retired"):
        return None
    if "tier_from" in entry:
        return entry["plans"].get(capability.get(entry["tier_from"]))
    return entry.get("tier")


def check_classes(capabilities: list, price_classes: dict, errors: list) -> None:
    """Schema-3 rules: no legacy class, `hive_plan` exactly on hive records,
    every free capability accountless."""
    for capability in capabilities:
        identifier, price = capability["id"], capability["price_class"]
        entry = price_classes.get(price, {})
        if entry.get("legacy"):
            errors.append(f"{identifier}: legacy price class {price} is not valid in schema {REGISTRY_SCHEMA}")
        planned = "hive_plan" in capability
        if planned != ("tier_from" in entry):
            errors.append(f"{identifier}: hive_plan must be set exactly on hive capabilities")
        elif planned and tier_of(capability, price_classes) is None:
            errors.append(f"{identifier}: unknown hive_plan {capability['hive_plan']}")
        if price == "free" and capability["account_required"]:
            errors.append(f"{identifier}: a free capability must not require an account")


def snake(name: str) -> str:
    return re.sub(r"(?<!^)(?=[A-Z])", "_", name).lower()


def rust_enum(text: str, name: str) -> set:
    body = re.search(r"pub enum " + re.escape(name) + r" \{(.*?)\n\}", text, re.S)
    if not body:
        raise ValueError(f"enum {name} not found")
    return {snake(v) for v in re.findall(r"^\s*([A-Z][A-Za-z0-9]*),", body.group(1), re.M)}


def squash(text: str) -> str:
    return " ".join(text.split())


def team_service_table(text: str) -> dict:
    """Rows of the service's `TEAM_SERVICE` table: capability id -> required operator inputs."""
    constants = dict(re.findall(r'pub const ([A-Z_]+): &str = "([^"]+)";', text))
    body = re.search(r"pub const TEAM_SERVICE: [^=]+= \[(.*?)\n\];", text, re.S)
    if not body:
        raise ValueError("TEAM_SERVICE table not found")
    rows = {}
    for name, inputs in re.findall(r"\(\s*([A-Z_]+),\s*&\[([A-Z_,\s]*)\],?\s*\)", body.group(1)):
        names = [name, *re.findall(r"[A-Z_]+", inputs)]
        unknown = [item for item in names if item not in constants]
        if unknown:
            raise ValueError(f"TEAM_SERVICE names undefined constants: {', '.join(unknown)}")
        rows[constants[name]] = sorted(constants[item] for item in names[1:])
    return rows


def check_team_service(tree, service: dict, team_ids: set, found: dict) -> None:
    """Every Team capability is served by named code or listed as entitled and not served — never both.

    `served` and `served_when_configured` must name handler text that exists and must equal the
    service's own table (which the handlers and the capability listing decide from). An id listed as
    `entitled_not_served` has no row there and is named by no scanned source besides the declared ones.
    """
    errors = tree.errors
    served, conditional = service["served"], service["served_when_configured"]
    unserved, declared = service["entitled_not_served"], service["declared_in"]
    states = (("served", served), ("served_when_configured", conditional), ("entitled_not_served", unserved))
    for identifier in sorted(team_ids):
        listed = [label for label, group in states if identifier in group]
        if len(listed) != 1:
            errors.append(f"{identifier}: Team capability must be in exactly one team_service list, "
                          f"found in {', '.join(listed) or 'none'}")
    for label, group in states:
        for identifier in sorted(set(group) - team_ids):
            errors.append(f"{identifier}: listed as {label} but not a Team capability of the registry")
    handlers = dict(served)
    for identifier, entry in conditional.items():
        handlers[identifier] = entry["handlers"]
        if not entry["requires"]:
            errors.append(f"{identifier}: served_when_configured without a required operator input")
    for identifier, needles in handlers.items():
        if not needles:
            errors.append(f"{identifier}: listed as served without a serving handler")
        for where in needles:
            tree.needle(where, f"team capability {identifier}")
    text = tree.read(declared)
    if text is not None:
        rows = team_service_table(text)
        expected = {identifier: [] for identifier in served}
        expected.update((identifier, sorted(entry["requires"])) for identifier, entry in conditional.items())
        for identifier in sorted(set(rows) | set(expected)):
            if identifier in unserved and identifier in rows:
                errors.append(f"{identifier}: listed as entitled_not_served but {declared} serves it")
            elif rows.get(identifier) != expected.get(identifier):
                errors.append(f"{identifier}: {declared} and team_service disagree "
                              f"(code {rows.get(identifier)}, map {expected.get(identifier)})")
    allowed = {declared, *service.get("catalog_pin_files", [])}
    for identifier in sorted(unserved):
        for path in sorted(set(found.get(identifier, ())) - allowed):
            errors.append(f"{identifier}: listed as entitled_not_served but named in {path}")


class Tree:
    def __init__(self, root: Path, profile: str):
        self.root, self.profile, self.errors, self.skipped = root, profile, [], []

    def private(self, path: str) -> bool:
        return path.split("/", 1)[0] in PRIVATE_ROOTS

    def read(self, path: str):
        """Text of a file, or None when the community profile leaves it out."""
        if self.private(path) and self.profile == "community":
            self.skipped.append(path)
            return None
        try:
            return (self.root / path).read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            self.errors.append(f"{path}: missing or unreadable")
            return None

    def needle(self, where: dict, label: str) -> None:
        text = self.read(where["path"])
        if text is not None and squash(where["contains"]) not in squash(text):
            self.errors.append(f"{label}: {where['path']} no longer contains `{where['contains']}`")

    def literals(self, roots: list, prefixes: list) -> dict:
        pattern = re.compile(r'"((?:%s)\.[a-z0-9_]+(?:\.[a-z0-9_]+)*)"' % "|".join(map(re.escape, prefixes)))
        found: dict = {}
        for base in roots:
            if self.private(base) and self.profile == "community":
                self.skipped.append(base)
                continue
            directory = self.root / base
            if not directory.is_dir():
                self.errors.append(f"{base}: scan root is missing")
                continue
            for path in sorted(directory.rglob("*.rs")):
                relative = path.relative_to(self.root).as_posix()
                if TEST_FILE.search(relative):
                    continue
                for match in pattern.finditer(path.read_text(encoding="utf-8", errors="replace")):
                    found.setdefault(match.group(1), set()).add(relative)
        return found


def verify_export(root: Path) -> dict:
    """The Apache host export: it ships the registry and the Engine's copy, not the tier map.

    What the export can state about itself is checked: the registry parses, ids and aliases
    are consistent, the Engine's embedded copy is byte-identical, every price class and
    delivery is one the Engine's code knows, and every source path the registry publishes
    is a file of this checkout. Tiers, enforcement points and pins need the tier map.
    """
    errors: list = []
    root = root.resolve(strict=True)
    registry_bytes = (root / EXPORT_REGISTRY).read_bytes()
    registry = parse_registry(registry_bytes.decode("utf-8"))
    if registry.get("schema_version") != REGISTRY_SCHEMA:
        raise ValueError(f"registry schema_version must be {REGISTRY_SCHEMA}")
    capabilities = registry["capability"]
    by_id = {capability["id"] for capability in capabilities}
    if len(by_id) != len(capabilities):
        errors.append("registry: duplicate capability id")
    for alias, target in registry.get("aliases", {}).items():
        if target not in by_id:
            errors.append(f"registry: alias {alias} names unknown capability {target}")
    mirror = root / EXPORT_MIRROR
    if not mirror.is_file() or mirror.read_bytes() != registry_bytes:
        errors.append(f"{EXPORT_MIRROR}: not byte-identical to {EXPORT_REGISTRY}")
    enum_source = (root / EXPORT_ENUMS).read_text(encoding="utf-8")
    price_classes = rust_enum(enum_source, "UserPriceClass")
    deliveries = rust_enum(enum_source, "RuntimeDelivery")
    rows = []
    for capability in capabilities:
        identifier = capability["id"]
        if capability["price_class"] not in price_classes:
            errors.append(f"{identifier}: price class {capability['price_class']} is unknown to {EXPORT_ENUMS}")
        if capability["runtime_delivery"] not in deliveries:
            errors.append(f"{identifier}: delivery {capability['runtime_delivery']} is unknown to {EXPORT_ENUMS}")
        if capability.get("source_visibility") == "public_apache":
            for source in (part.strip() for part in capability.get("source_path", "").split(";")):
                if not host_file(root, source):
                    errors.append(f"{identifier}: public source path {source} is not a file of the Apache host")
        rows.append({"id": identifier, "price_class": capability["price_class"],
                     "delivery": capability["runtime_delivery"]})
    return {"schema": "leanctx.capability-check/v1", "passed": not errors, "profile": "community",
            "registry_sha256": hashlib.sha256(registry_bytes).hexdigest(), "capabilities": rows,
            "tiers": {}, "errors": errors, "skipped": [MAP_PATH],
            "scope": "registry and Engine copy of the host export; the tier map is not part of it"}


def verify(root: Path, profile: str = "product") -> dict:
    if profile not in ("product", "community"):
        raise ValueError("unsupported profile")
    root = root.resolve(strict=True)
    if profile == "community" and not (root / MAP_PATH).exists():
        return verify_export(root)
    tier_map = json.loads((root / MAP_PATH).read_text(encoding="utf-8"))
    if tier_map.get("schema") != "leanctx.tier-map/v1":
        raise ValueError("unsupported tier map")
    tree = Tree(root, profile)
    errors = tree.errors
    registry_bytes = (root / tier_map["registry"]).read_bytes()
    registry = parse_registry(registry_bytes.decode("utf-8"))
    if registry.get("schema_version") != REGISTRY_SCHEMA:
        raise ValueError(f"registry schema_version must be {REGISTRY_SCHEMA}")
    capabilities = registry["capability"]
    by_id = {capability["id"]: capability for capability in capabilities}
    if len(by_id) != len(capabilities):
        errors.append("registry: duplicate capability id")
    aliases = registry.get("aliases", {})
    for alias, target in aliases.items():
        if target not in by_id:
            errors.append(f"registry: alias {alias} names unknown capability {target}")
    digest = hashlib.sha256(registry_bytes).hexdigest()
    # A public projection withholds private source paths and has its own bytes.
    projected = registry.get("projection") == "public"

    # A public mirror of the full private registry is its public projection: the
    # Engine embeds it, so private source paths and ratings never reach a public build.
    public_projection = None
    if not projected:
        public_projection = _projection_tool().project(registry_bytes.decode("utf-8")).encode("utf-8")
    for mirror in tier_map["registry_mirrors"]:
        if tree.private(mirror) and (profile == "community" or projected):
            tree.skipped.append(mirror)
        elif not (root / mirror).is_file():
            errors.append(f"{mirror}: missing")
        elif tree.private(mirror) or projected:
            if (root / mirror).read_bytes() != registry_bytes:
                errors.append(f"{mirror}: not byte-identical to {tier_map['registry']}")
        elif (root / mirror).read_bytes() != public_projection:
            errors.append(f"{mirror}: not the public projection of {tier_map['registry']} "
                          "(scripts/project-capabilities.py)")
    if not projected:
        for pin in tier_map["registry_digest_pins"]:
            text = tree.read(pin)
            if text is not None and digest not in text:
                errors.append(f"{pin}: does not pin registry sha256 {digest}")
        # The export writes the projection in place of the canonical registry;
        # its manifest pins that file's digest, which follows every registry edit.
        manifest = root / "PUBLISH_MANIFEST.toml"
        if manifest.is_file():
            generated = {
                block.group("path"): block.group("sha256")
                for block in re.finditer(
                    r'(?m)^\[\[generated\]\]\s*\npath = "(?P<path>[^"]+)"\s*\n'
                    r'sha256 = "(?P<sha256>[0-9a-f]{64})"',
                    manifest.read_text(encoding="utf-8"),
                )
            }
            expected = hashlib.sha256(public_projection).hexdigest()
            if generated.get(tier_map["registry"]) != expected:
                errors.append(f"PUBLISH_MANIFEST.toml: generated {tier_map['registry']} must pin "
                              f"the projection sha256 {expected}")

    enums = tier_map["code_enums"]
    enum_source = tree.read(enums["path"]) or ""
    price_classes, deliveries, tiers = tier_map["price_classes"], tier_map["deliveries"], tier_map["tiers"]
    for label, mapped, enum in (("price class", price_classes, enums["price_class"]),
                                ("runtime delivery", deliveries, enums["runtime_delivery"])):
        in_code = rust_enum(enum_source, enum)
        for name in sorted(in_code ^ set(mapped)):
            errors.append(f"{label} {name}: " + ("code knows it, the tier map does not"
                                                  if name in in_code else "tier map entry unknown to the code"))
    for name, entry in price_classes.items():
        named = (list(entry["plans"].values()) if "tier_from" in entry
                 else [] if entry.get("retired") else [entry.get("tier")])
        for tier in named:
            if tier not in tiers:
                errors.append(f"price class {name}: unknown tier {tier}")
        if not entry["enforced_by"]:
            errors.append(f"price class {name}: no enforcement point")
        for where in entry["enforced_by"]:
            tree.needle(where, f"price class {name}")

    rows = []
    for capability in capabilities:
        identifier, price, delivery = capability["id"], capability["price_class"], capability["runtime_delivery"]
        if price not in price_classes or delivery not in deliveries:
            errors.append(f"{identifier}: price class {price} or delivery {delivery} has no tier map entry")
            continue
        public = capability["source_visibility"] == "public_apache"
        for source in (part.strip() for part in capability["source_path"].split(";")):
            if public and not host_file(root, source):
                errors.append(f"{identifier}: public source path {source} is not a file of the Apache host")
        rows.append({"id": identifier, "price_class": price,
                     "tier": tier_of(capability, price_classes) or "retired",
                     "grant": price_classes[price]["grant"], "delivery": delivery,
                     "ships_in": deliveries[delivery]["ships_in"],
                     "enforced_by": [where["path"] for where in price_classes[price]["enforced_by"]]})
    check_classes(capabilities, price_classes, errors)
    limits = tier_map.get("instance_limits", {})
    for tier in tiers[1:]:
        limit = limits.get(tier, {})
        if not any(isinstance(limit.get(key), int) and limit[key] > 0
                   for key in ("instances", "instances_per_seat")):
            errors.append(f"{MAP_PATH}: tier {tier} has no positive instance limit")

    # The hosted catalog and the Team service belong to the legacy hosted
    # services, which stay on their frozen schema-2 registry until retired.
    legacy = tier_map["legacy_hosted"]
    legacy_capabilities = []
    legacy_text = None if projected else tree.read(legacy["registry"])
    if legacy_text is not None:
        legacy_bytes = (root / legacy["registry"]).read_bytes()
        legacy_registry = parse_registry(legacy_text)
        if legacy_registry.get("schema_version") != legacy["schema_version"]:
            errors.append(f"{legacy['registry']}: schema_version must stay {legacy['schema_version']}")
        legacy_capabilities = legacy_registry["capability"]
        legacy_digest = hashlib.sha256(legacy_bytes).hexdigest()
        for mirror in legacy["mirrors"]:
            text = tree.read(mirror)
            if text is not None and (root / mirror).read_bytes() != legacy_bytes:
                errors.append(f"{mirror}: not byte-identical to {legacy['registry']}")
        for pin in legacy["digest_pins"]:
            text = tree.read(pin)
            if text is not None and legacy_digest not in text:
                errors.append(f"{pin}: does not pin legacy registry sha256 {legacy_digest}")
        legacy_ids = {c["id"] for c in legacy_capabilities}
        for name in sorted(legacy_ids - set(by_id)):
            errors.append(f"{name}: in the frozen legacy registry but not a product capability")

    hosted = tier_map["hosted_catalog"]
    catalog_text = tree.read(hosted["path"])
    if catalog_text is not None and legacy_capabilities:
        catalog = json.loads(catalog_text)
        for tier in tiers[1:]:
            expected = sorted(c["id"] for c in legacy_capabilities
                              if (c["price_class"] in hosted["price_classes"]
                                  and price_classes[c["price_class"]]["tier"] == tier
                                  and c["account_required"] and c["managed_cloud_available"])
                              or (tier == "pro" and account_licensed_pro(c)))
            if catalog.get(tier) != expected:
                errors.append(f"{hosted['path']}: list {tier} differs from the registry (run {hosted['generator']})")

    retired, unregistered = tier_map["retired"]["ids"], tier_map["unregistered"]
    not_ids, runtime = tier_map["not_capability_ids"], tier_map["runtime"]
    for label, names in (("retired", retired), ("unregistered", unregistered), ("not a capability", not_ids)):
        for name in names:
            if name in by_id or name in aliases:
                errors.append(f"{name}: listed as {label} but present in the registry")
    retired_gate = tree.read(tier_map["retired"]["enforced_by"]["path"])
    if retired_gate is not None:
        tree.needle(tier_map["retired"]["enforced_by"], "retired ids")
        for name in retired:
            if f'"{name}"' not in retired_gate:
                errors.append(f"{name}: retired id is not refused in {tier_map['retired']['enforced_by']['path']}")

    scan = tier_map["id_scan"]
    found = tree.literals(scan["roots"], scan["prefixes"])
    known = set(by_id) | set(retired) | set(unregistered) | set(not_ids)
    for name in sorted(set(found) - known):
        errors.append(f"{name}: used in {', '.join(sorted(found[name]))} but neither registered nor declared in {MAP_PATH}")
    if profile == "product":
        for name in sorted((set(retired) | set(unregistered) | set(not_ids)) - set(found)):
            errors.append(f"{name}: declared in {MAP_PATH} but no longer used in the scanned code")

    team_ids = {c["id"] for c in legacy_capabilities
                if price_classes.get(c["price_class"], {}).get("tier") == "team"
                and c["account_required"] and c["managed_cloud_available"]}
    if "team_service" in tier_map:
        if legacy_capabilities:
            check_team_service(tree, tier_map["team_service"], team_ids, found)
    elif team_ids:
        errors.append(f"{MAP_PATH}: team_service is missing, so nothing says which Team capabilities are served")

    gate_dir = runtime["engine_gate_dir"] + "/"
    # A Runtime capability is one the signed local Runtime gates by a lease
    # feature: every `pro_paid` id, and any id of another price class that a
    # lease feature of the tier map names in its `gates` (an enterprise
    # operation of the Runtime is not a hosted entitlement). The same set is
    # held to both rules below: admitted by the Engine, and advertised.
    lease_gated = {gated for entry in tier_map["license_features"]["features"].values()
                   for gated in entry["gates"]}
    registered = ({c["id"] for c in capabilities
                   if c["price_class"] == "pro_paid" and not account_licensed_pro(c)}
                  | (lease_gated & set(by_id)))
    for name in sorted(registered):
        if not any(path.startswith(gate_dir) for path in found.get(name, ())):
            errors.append(f"{name}: Runtime capability is not admitted anywhere under {gate_dir}")
    description = tree.read(runtime["description"])
    if description is not None:
        advertised = set(re.findall(r'pub const [A-Z_]+_CAPABILITY: &str = "([a-z0-9_.]+)";', description))
        for name in sorted(advertised - registered - set(unregistered)):
            errors.append(f"{name}: advertised by {runtime['description']} but not a registered Runtime capability")
        for name in sorted(registered - advertised - set(runtime["not_advertised"])):
            errors.append(f"{name}: registered Runtime capability is not advertised by {runtime['description']}")
        for name in sorted(set(runtime["not_advertised"]) & advertised):
            errors.append(f"{name}: declared not advertised but {runtime['description']} advertises it")

    features = tier_map["license_features"]
    feature_source = tree.read(features["path"])
    if feature_source is not None:
        in_code = set(re.findall(r'Self::[A-Za-z]+ => "([a-z_]+)",', feature_source))
        for name in sorted(in_code ^ set(features["features"])):
            errors.append(f"license feature {name}: " + ("issued by code, missing in the tier map"
                                                         if name in in_code else "tier map entry unknown to the code"))
        for name, entry in features["features"].items():
            for gated in entry["gates"]:
                if gated not in by_id:
                    errors.append(f"license feature {name}: gates unknown capability {gated}")

    counts = {tier: sum(1 for row in rows if row["tier"] == tier) for tier in tiers}
    return {"schema": "leanctx.capability-check/v1", "passed": not errors, "profile": profile,
            "registry_sha256": digest, "capabilities": rows, "tiers": counts, "errors": errors,
            "skipped": sorted(set(tree.skipped)),
            "scope": "registry, tier map and source text; not compilation, issuance or release approval"}


def table(result: dict) -> str:
    lines = ["| Capability | Price class | Tier | Grant | Ships in | Enforced by |", "|---|---|---|---|---|---|"]
    for row in sorted(result["capabilities"], key=lambda r: r["id"]):
        lines.append("| {id} | {price_class} | {tier} | {grant} | {ships_in} | {where} |".format(
            where="<br>".join(row["enforced_by"]), **row))
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n", 1)[0])
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--profile", choices=("product", "community"), default="product")
    parser.add_argument("--check", action="store_true", help="verify only (the default action)")
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--table", action="store_true", help="print the joined capability table")
    args = parser.parse_args()
    try:
        result = verify(args.root, args.profile)
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"capabilities: {error}", file=sys.stderr)
        return 1
    for error in result["errors"]:
        print(f"capabilities: {error}", file=sys.stderr)
    if args.json:
        print(json.dumps(result, sort_keys=True))
    elif args.table and not args.check and result["tiers"]:
        print(table(result))
    elif result["passed"]:
        tiers = ", ".join(f"{tier}={count}" for tier, count in result["tiers"].items()) \
            or "tiers not joined: no tier map in this checkout"
        print(f"capabilities: PASS ({len(result['capabilities'])} capabilities; {tiers}; "
              f"profile {result['profile']}; registry sha256 {result['registry_sha256'][:12]})")
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
