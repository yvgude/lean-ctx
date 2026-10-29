# SPDX-License-Identifier: Apache-2.0
"""The capability check must fail when registry, tier map and gating code drift apart."""
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("check_capabilities", ROOT / "scripts/check-capabilities.py")
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)

RECORD = ('[[capability]]\nid = "{id}"\nprice_class = "{price}"\n{plan}source_visibility = "{visibility}"\n'
          'runtime_delivery = "{delivery}"\naccount_required = {hosted}\nmanaged_cloud_available = {hosted}\n'
          'source_path = "{source}"\n\n')


def record(identifier, price, visibility, delivery, hosted, source, plan=None):
    return RECORD.format(id=identifier, price=price, visibility=visibility, delivery=delivery, hosted=hosted,
                         source=source, plan=f'hive_plan = "{plan}"\n' if plan else "")


# The schema-3 product registry: local free, Hive plans, retired Personal Cloud.
REGISTRY = 'schema_version = 3\n\n[aliases]\n"sync" = "pro.cloud_sync"\n\n' + "".join([
    record("trust.runtime.cache", "free", "public_apache", "source_build", "false", "rust/src/core/cache.rs"),
    record("pro.cloud_sync", "retired", "private_source", "managed_cloud_service", "true", "cloud"),
    record("pro.runtime.adaptive_context", "free", "private_source", "signed_binary", "false", "runtime"),
    record("pro.runtime.code_security", "hive", "private_source", "hive_service", "true", "hive", "pro"),
])
# The frozen schema-2 registry the legacy hosted services keep deciding from.
LEGACY = 'schema_version = 2\n\n[aliases]\n"sync" = "pro.cloud_sync"\n\n' + "".join([
    record("trust.runtime.cache", "free", "public_apache", "source_build", "false", "rust/src/core/cache.rs"),
    record("pro.cloud_sync", "cloud_paid", "private_source", "managed_cloud_service", "true", "cloud"),
    record("pro.runtime.adaptive_context", "pro_paid", "private_source", "signed_binary", "false", "runtime"),
])
ENGINE = ('pub enum UserPriceClass {\n    Free,\n    Hive,\n    Retired,\n    CloudPaid,\n    ProPaid,\n}\n'
          'pub enum RuntimeDelivery {\n    SourceBuild,\n    SignedBinary,\n    ManagedCloudService,\n'
          '    HiveService,\n}\n'
          'UserPriceClass::Free => Plan::Community,\n')
EDGE = 'const RETIRED: [&str; 1] = ["pro.runtime.adaptive_routing"];\nfn allows() {}\nconst PIN: &str = "{digest}";\n'
LEGACY_PATH = "ee/services/cloud/legacy-registry.toml"


def needle(path, contains):
    return {"path": path, "contains": contains}


def sha(text):
    return hashlib.sha256(text.encode()).hexdigest()


class CapabilityCheckTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        engine = [needle("rust/src/core/product_capabilities.rs", "UserPriceClass::Free => Plan::Community,")]
        self.edge = edge = "ee/services/edge/src/entitlement.rs"
        price_classes = {name: {"tier": tier, "grant": "g", "enforced_by": engine}
                         for name, tier in [("free", "community"), ("cloud_paid", "pro"), ("pro_paid", "pro")]}
        for legacy in ("cloud_paid", "pro_paid"):
            price_classes[legacy]["legacy"] = True
        price_classes["hive"] = {"tier_from": "hive_plan", "plans": {"pro": "pro"}, "grant": "g",
                                  "enforced_by": engine}
        price_classes["retired"] = {"retired": True, "grant": "none", "enforced_by": engine}
        self.map = {
            "schema": "leanctx.tier-map/v1", "registry": "product/capabilities.toml",
            "registry_mirrors": ["rust/data/product-capabilities.toml"], "registry_digest_pins": [],
            "legacy_hosted": {"registry": LEGACY_PATH, "schema_version": 2,
                              "mirrors": ["ee/services/edge/legacy-registry.toml"], "digest_pins": [edge]},
            "code_enums": {"path": "rust/src/core/product_capabilities.rs", "price_class": "UserPriceClass",
                           "runtime_delivery": "RuntimeDelivery"},
            "tiers": ["community", "pro"],
            "instance_limits": {"pro": {"instances": 2, "per": "account"}},
            "price_classes": price_classes,
            "hosted_catalog": {"path": "ee/services/cloud/catalog.json", "generator": "gen", "price_classes": ["cloud_paid"]},
            "deliveries": {name: {"ships_in": name, "source_roots": []}
                           for name in ["source_build", "signed_binary", "managed_cloud_service", "hive_service"]},
            "runtime": {"engine_gate_dir": "rust/src/core/intelligence_runtime",
                        "description": "ee/intelligence/src/lib.rs", "not_advertised": {}},
            "retired": {"enforced_by": needle(edge, "fn allows() {}"), "ids": {"pro.runtime.adaptive_routing": "r"}},
            "unregistered": {}, "not_capability_ids": {},
            "id_scan": {"roots": ["rust/src", "ee/services/edge/src", "ee/intelligence/src"], "prefixes": ["trust", "pro"]},
            "license_features": {"path": "ee/intelligence/src/features.rs",
                                 "features": {"adaptive_context": {"gates": ["pro.runtime.adaptive_context"]}}},
        }
        self.write_registry(REGISTRY)
        self.write_legacy(LEGACY)
        self.write("rust/src/core/cache.rs", "")
        self.write("rust/src/core/product_capabilities.rs", ENGINE)
        self.write("rust/src/core/intelligence_runtime/health.rs", 'const ID: &str = "pro.runtime.adaptive_context";\n')
        self.write("ee/intelligence/src/lib.rs", 'pub const ADAPTIVE_CONTEXT_CAPABILITY: &str = "pro.runtime.adaptive_context";\n')
        self.write("ee/intelligence/src/features.rs", 'Self::AdaptiveContext => "adaptive_context",\n')
        self.write("ee/services/cloud/catalog.json", json.dumps({"pro": ["pro.cloud_sync"]}))
        self.save_map()

    def write(self, path, text):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")

    def write_registry(self, text):
        self.write("product/capabilities.toml", text)
        # The Engine embeds the public projection, never the canonical registry.
        self.write("rust/data/product-capabilities.toml", check._projection_tool().project(text))

    def write_legacy(self, text):
        self.write(LEGACY_PATH, text)
        self.write("ee/services/edge/legacy-registry.toml", text)
        self.write(self.edge, EDGE.replace("{digest}", sha(text)))

    def save_map(self):
        self.write("product/tier-map.json", json.dumps(self.map))

    def errors(self, profile="product"):
        return "\n".join(check.verify(self.root, profile)["errors"])

    def test_consistent_tree_passes_with_and_without_commercial_sources(self):
        self.assertEqual(self.errors(), "")
        shutil.rmtree(self.root / "ee")
        self.assertEqual(self.errors("community"), "")
        self.assertIn("ee/intelligence/src/lib.rs: missing", self.errors("product"))

    def test_capability_id_used_in_code_but_not_registered_fails(self):
        self.write("rust/src/gate.rs", 'fn gate() { allows("pro.shadow_feature"); }\n')
        self.assertIn("pro.shadow_feature: used in rust/src/gate.rs", self.errors("community"))

    def test_registered_runtime_capability_without_engine_admission_fails(self):
        self.write("rust/src/core/intelligence_runtime/health.rs", "")
        self.assertIn("pro.runtime.adaptive_context: Runtime capability is not admitted", self.errors())

    def test_engine_copy_must_follow_the_registry(self):
        self.write("product/capabilities.toml", REGISTRY.replace('"sync" = ', '"cloud_sync" = '))
        self.assertIn("rust/data/product-capabilities.toml: not the public projection", self.errors())

    def test_legacy_copies_pins_and_signed_catalog_must_follow_the_frozen_registry(self):
        self.write(LEGACY_PATH, LEGACY.replace('"cloud_paid"', '"free"'))
        found = self.errors()
        self.assertIn("ee/services/edge/legacy-registry.toml: not byte-identical", found)
        self.assertIn("does not pin legacy registry sha256", found)
        self.assertIn("ee/services/cloud/catalog.json: list pro differs", found)

    def test_schema_3_refuses_legacy_classes_planless_hive_and_accounted_free(self):
        self.write_registry(REGISTRY.replace('price_class = "retired"', 'price_class = "cloud_paid"'))
        self.assertIn("pro.cloud_sync: legacy price class cloud_paid is not valid in schema 3", self.errors())
        self.write_registry(REGISTRY.replace('hive_plan = "pro"\n', ""))
        self.assertIn("pro.runtime.code_security: hive_plan must be set exactly on hive", self.errors())
        self.write_registry(REGISTRY.replace('hive_plan = "pro"', 'hive_plan = "galaxy"'))
        self.assertIn("pro.runtime.code_security: unknown hive_plan galaxy", self.errors())
        self.write_registry(REGISTRY + record("trust.runtime.x", "free", "private_source", "signed_binary",
                                              "true", "runtime"))
        self.assertIn("trust.runtime.x: a free capability must not require an account", self.errors())

    def test_every_paid_tier_needs_an_instance_limit(self):
        self.map["instance_limits"] = {}
        self.save_map()
        self.assertIn("tier pro has no positive instance limit", self.errors())

    def test_retired_id_cannot_return_to_the_registry(self):
        self.write_registry(REGISTRY + record("pro.runtime.adaptive_routing", "free", "private_source",
                                              "signed_binary", "false", "runtime"))
        self.assertIn("pro.runtime.adaptive_routing: listed as retired but present", self.errors())

    def test_price_class_or_license_feature_added_in_code_needs_a_map_entry(self):
        self.write("rust/src/core/product_capabilities.rs", ENGINE.replace("ProPaid,", "ProPaid,\n    TeamPaid,"))
        self.write("ee/intelligence/src/features.rs", 'Self::A => "adaptive_context",\nSelf::B => "model_routing",\n')
        found = self.errors()
        self.assertIn("price class team_paid: code knows it, the tier map does not", found)
        self.assertIn("license feature model_routing: issued by code, missing in the tier map", found)

    def test_team_capability_is_either_served_by_named_code_or_listed_as_not_served(self):
        team = ("team.bus", "team.graph", "team.leases")
        self.write_registry(REGISTRY + "".join(
            record(i, "hive", "private_source", "hive_service", "true", "hive", "team") for i in team))
        self.write_legacy(LEGACY + "".join(
            record(i, "team_paid", "private_source", "managed_cloud_service", "true", "cloud") for i in team))
        self.write("rust/src/core/product_capabilities.rs", ENGINE.replace("ProPaid,", "ProPaid,\n    TeamPaid,"))
        self.write("ee/services/cloud/catalog.json", json.dumps({"pro": ["pro.cloud_sync"], "team": list(team)}))
        declared, routes = "ee/services/cloud/src/availability.rs", "ee/services/cloud/src/routes.rs"
        self.write(declared, 'pub const BUS: &str = "team.bus";\npub const LEASES: &str = "team.leases";\n'
                             'pub const DAYS: &str = "RETENTION_DAYS";\n'
                             "pub const TEAM_SERVICE: [(&str, &[&str]); 2] = [\n"
                             "    (LEASES, &[]),\n    (BUS, &[DAYS]),\n];\n")
        self.write(routes, 'route("/leases")\nroute("/bus")\n')
        self.map["tiers"].append("team")
        self.map["instance_limits"]["team"] = {"instances": 25, "per": "org"}
        self.map["price_classes"]["team_paid"] = dict(self.map["price_classes"]["cloud_paid"], tier="team")
        self.map["price_classes"]["hive"]["plans"]["team"] = "team"
        self.map["hosted_catalog"]["price_classes"].append("team_paid")
        self.map["id_scan"]["roots"].append("ee/services/cloud/src")
        self.map["id_scan"]["prefixes"].append("team")
        self.save_map()
        self.assertIn("team_service is missing", self.errors())

        def listed(served, conditional, unserved):
            self.map["team_service"] = {
                "declared_in": declared,
                "served": {i: [needle(routes, f'route("/{i[5:]}")')] for i in served},
                "served_when_configured": {i: {"requires": ["RETENTION_DAYS"],
                                               "handlers": [needle(routes, f'route("/{i[5:]}")')]}
                                           for i in conditional},
                "entitled_not_served": dict.fromkeys(unserved, "no implementation"),
            }
            self.save_map()
            return self.errors()

        self.assertEqual(listed(["team.leases"], ["team.bus"], ["team.graph"]), "")
        # Code serves it, the map says it does not: the list has gone stale.
        self.assertIn("team.leases: listed as entitled_not_served but ee/services/cloud/src/availability.rs serves it",
                      listed([], ["team.bus"], ["team.graph", "team.leases"]))
        # The map claims a capability is served that no code serves.
        found = listed(["team.leases", "team.graph"], ["team.bus"], [])
        self.assertIn("team.graph: ee/services/cloud/src/availability.rs and team_service disagree", found)
        self.assertIn('no longer contains `route("/graph")`', found)
        # A capability needing an operator input is not "served" outright.
        self.assertIn("team.bus: ee/services/cloud/src/availability.rs and team_service disagree",
                      listed(["team.leases", "team.bus"], [], ["team.graph"]))
        # A Team capability nobody classified, and a handler that names an unserved one.
        self.assertIn("team.graph: Team capability must be in exactly one team_service list, found in none",
                      listed(["team.leases"], ["team.bus"], []))
        self.write("ee/services/cloud/src/graph.rs", 'fn gate() { require("team.graph"); }\n')
        self.assertIn("team.graph: listed as entitled_not_served but named in ee/services/cloud/src/graph.rs",
                      listed(["team.leases"], ["team.bus"], ["team.graph"]))

    @unittest.skipIf(check.tomllib is None, "tomllib is the reference parser")
    def test_fallback_reader_matches_tomllib_on_the_real_registry(self):
        for path in ("product/capabilities.toml", "ee/services/cloud/contracts/product-capabilities-registry.toml"):
            source = ROOT / path
            if not source.is_file():
                continue
            text = source.read_text(encoding="utf-8")
            reference, check.tomllib = check.tomllib, None
            try:
                self.assertEqual(check.parse_registry(text), reference.loads(text), path)
            finally:
                check.tomllib = reference


if __name__ == "__main__":
    unittest.main()
