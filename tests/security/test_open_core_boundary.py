import importlib.util
import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/check-open-core-boundary.py"
SPEC = importlib.util.spec_from_file_location("open_core_boundary", SCRIPT)
GATE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = GATE
SPEC.loader.exec_module(GATE)


class OpenCoreBoundaryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.repo = Path(self.temp.name)
        for relative in (
            "rust/crates/lean-ctx-protocol/src/lib.rs",
            "rust/crates/lean-ctx-protocol/src/auto_routing.rs",
            "rust/crates/lean-ctx-protocol/src/control_plane.rs",
            "rust/crates/lean-ctx-protocol/src/eligibility.rs",
            "rust/crates/lean-ctx-protocol/src/fleet_control.rs",
            "rust/crates/lean-ctx-protocol/src/outcome_engine.rs",
            "rust/crates/lean-ctx-protocol/src/rollout.rs",
            "rust/crates/lean-ctx-protocol/src/value_share.rs",
            "rust/crates/lean-ctx-proxy/src/lib.rs",
            "rust/crates/lean-ctx-proxy/src/auto_routing.rs",
            "rust/crates/lean-ctx-proxy/src/rollout.rs",
        ):
            destination = self.repo / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / relative, destination)
        # Consumer fixtures are engine sources; the proxy files above no longer
        # create rust/src as a side effect (the proxy is its own crate).
        (self.repo / "rust/src").mkdir(parents=True, exist_ok=True)
        self.manifest = self.repo / "security/public-protocol-surface-freeze-v1.json"
        self.manifest.parent.mkdir()
        shutil.copy2(ROOT / "security/public-protocol-surface-freeze-v1.json", self.manifest)

    def tearDown(self):
        self.temp.cleanup()

    def write_manifest(self, value):
        self.manifest.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")

    def read_manifest(self):
        return json.loads(self.manifest.read_text(encoding="utf-8"))

    def test_current_tree_passes(self):
        self.assertEqual(GATE.check_repo(ROOT), [])

    def test_fixture_tree_passes_and_approved_local_import_is_not_private(self):
        findings = GATE.check_repo(self.repo)
        self.assertEqual(findings, [])
        self.assertNotIn("private-import", "\n".join(findings))

    def test_missing_manifest_fails_closed(self):
        self.manifest.unlink()
        findings = GATE.check_repo(self.repo)
        self.assertTrue(findings)
        self.assertTrue(findings[0].startswith("[manifest]"))

    def test_malformed_manifest_fails_closed(self):
        self.manifest.write_text('{"schema_version":', encoding="utf-8")
        findings = GATE.check_repo(self.repo)
        self.assertTrue(findings[0].startswith("[manifest]"))

    def test_manifest_metadata_drift_fails_closed(self):
        manifest = self.read_manifest()
        manifest["surfaces"]["rollout"]["status"] = "stable"
        self.write_manifest(manifest)
        self.assertTrue(any("[manifest]" in finding for finding in GATE.check_repo(self.repo)))

    def test_manifest_rejects_invalid_module_digest(self):
        manifest = self.read_manifest()
        manifest["surfaces"]["rollout"]["module_sha256"] = "not-a-digest"
        self.write_manifest(manifest)
        self.assertTrue(any("[manifest]" in finding for finding in GATE.check_repo(self.repo)))

    def test_public_module_root_drift_is_rejected(self):
        path = self.repo / "rust/crates/lean-ctx-proxy/src/lib.rs"
        path.write_text(path.read_text(encoding="utf-8") + "\npub mod value_share;\n", encoding="utf-8")
        findings = GATE.check_repo(self.repo)
        self.assertTrue(any("value_share module_roots drift" in finding for finding in findings))

    def test_root_reexport_drift_is_rejected(self):
        path = self.repo / "rust/crates/lean-ctx-proxy/src/rollout.rs"
        source = path.read_text(encoding="utf-8").replace("RolloutConfig, assign_rollout", "RolloutConfig")
        path.write_text(source, encoding="utf-8")
        findings = GATE.check_repo(self.repo)
        self.assertTrue(any("rollout root_reexports drift" in finding for finding in findings))

    def test_new_export_is_rejected(self):
        path = self.repo / "rust/crates/lean-ctx-protocol/src/control_plane.rs"
        path.write_text(path.read_text(encoding="utf-8") + "\npub struct UnreviewedControlPlaneExport;\n", encoding="utf-8")
        findings = GATE.check_repo(self.repo)
        self.assertTrue(any("control_plane exported symbols drift" in finding for finding in findings))

    def test_existing_public_wire_shape_drift_is_rejected(self):
        path = self.repo / "rust/crates/lean-ctx-protocol/src/control_plane.rs"
        source = path.read_text(encoding="utf-8").replace(
            "    pub available_capabilities: Vec<CapabilityManifestV1>,\n",
            "    pub available_capabilities: Vec<CapabilityManifestV1>,\n"
            "    pub unreviewed_field: bool,\n",
        )
        path.write_text(source, encoding="utf-8")
        findings = GATE.check_repo(self.repo)
        self.assertTrue(any("control_plane module digest drift" in finding for finding in findings))

    def test_crlf_only_module_change_is_rejected(self):
        path = self.repo / "rust/crates/lean-ctx-protocol/src/control_plane.rs"
        path.write_bytes(path.read_bytes().replace(b"\n", b"\r\n"))
        findings = GATE.check_repo(self.repo)
        self.assertTrue(any("control_plane module digest drift" in finding for finding in findings))

    def test_new_consumer_is_rejected(self):
        consumer = self.repo / "rust/src/unreviewed_consumer.rs"
        consumer.write_text(
            "use lean_ctx_protocol::control_plane::ControlPlaneRequest;\n",
            encoding="utf-8",
        )
        findings = GATE.check_repo(self.repo)
        self.assertTrue(any("[new-consumer] control_plane" in finding for finding in findings))
        self.assertFalse(any("[private-import]" in finding for finding in findings))

    def assert_new_consumer(self, filename, source, surface):
        consumer = self.repo / "rust/src" / filename
        consumer.write_text(source, encoding="utf-8")
        findings = GATE.check_repo(self.repo)
        self.assertTrue(any("[new-consumer] %s" % surface in finding for finding in findings), findings)

    def test_same_crate_root_symbol_import_is_rejected(self):
        self.assert_new_consumer(
            "root_symbol_consumer.rs",
            "use crate::{ControlPlaneRequest};\n",
            "control_plane",
        )

    def test_same_crate_module_symbol_import_is_rejected(self):
        self.assert_new_consumer(
            "module_symbol_consumer.rs",
            "use crate::control_plane::ControlPlaneRequest;\n",
            "control_plane",
        )

    def test_fully_qualified_root_symbol_reference_is_rejected(self):
        self.assert_new_consumer(
            "qualified_consumer.rs",
            "fn consume(value: crate::ControlPlaneRequest) { let _ = value; }\n",
            "control_plane",
        )

    def test_external_fully_qualified_root_symbol_reference_is_rejected(self):
        self.assert_new_consumer(
            "external_qualified_consumer.rs",
            "fn consume(value: lean_ctx_protocol::ControlPlaneRequest) { let _ = value; }\n",
            "control_plane",
        )

    def test_external_root_symbol_import_is_rejected(self):
        self.assert_new_consumer(
            "external_root_consumer.rs",
            "use lean_ctx_protocol::ControlPlaneRequest;\n",
            "control_plane",
        )

    def test_relevant_glob_imports_are_rejected(self):
        consumer = self.repo / "rust/src/glob_consumer.rs"
        consumer.write_text(
            "use crate::*;\nuse crate::control_plane::*;\n"
            "use lean_ctx_protocol::control_plane::*;\n",
            encoding="utf-8",
        )
        findings = GATE.check_repo(self.repo)
        for surface in ("control_plane", "fleet_control", "value_share"):
            self.assertTrue(any("[new-consumer] %s" % surface in finding for finding in findings), findings)

    def test_external_crate_alias_is_rejected(self):
        consumer = self.repo / "rust/src/alias_consumer.rs"
        consumer.write_text("use lean_ctx_protocol as protocol;\n", encoding="utf-8")
        findings = GATE.check_repo(self.repo)
        for surface in GATE.FROZEN_SURFACES:
            self.assertTrue(any("[new-consumer] %s" % surface in finding for finding in findings), findings)

    def test_same_crate_alias_and_extern_crate_are_rejected(self):
        consumer = self.repo / "rust/crates/lean-ctx-protocol/src/alias_consumer.rs"
        consumer.write_text(
            "use crate as protocol;\n"
            "fn consume(_: protocol::ControlPlaneRequest) {}\n"
            "extern crate lean_ctx_protocol as external;\n",
            encoding="utf-8",
        )
        findings = GATE.check_repo(self.repo)
        for surface in GATE.FROZEN_SURFACES:
            self.assertTrue(any("[new-consumer] %s" % surface in finding for finding in findings), findings)

    def test_multiline_same_crate_alias_is_rejected(self):
        consumer = self.repo / "rust/crates/lean-ctx-protocol/src/multiline_alias.rs"
        consumer.write_text(
            "use\n    crate as protocol;\n"
            "fn consume(_: protocol::ControlPlaneRequest) {}\n",
            encoding="utf-8",
        )
        findings = GATE.check_repo(self.repo)
        self.assertTrue(any("[new-consumer] control_plane" in finding for finding in findings), findings)

    def test_self_and_super_same_crate_aliases_are_rejected(self):
        consumer = self.repo / "rust/crates/lean-ctx-protocol/src/relative_alias.rs"
        consumer.write_text(
            "use self as local;\n"
            "use super as parent;\n"
            "fn first(_: local::ControlPlaneRequest) {}\n"
            "fn second(_: parent::ControlPlaneRequest) {}\n",
            encoding="utf-8",
        )
        findings = GATE.check_repo(self.repo)
        self.assertTrue(any("[new-consumer] control_plane" in finding for finding in findings), findings)

    def test_extern_protocol_crate_alone_is_rejected(self):
        consumer = self.repo / "rust/src/extern_consumer.rs"
        consumer.write_text("extern crate lean_ctx_protocol;\n", encoding="utf-8")
        findings = GATE.check_repo(self.repo)
        for surface in GATE.FROZEN_SURFACES:
            self.assertTrue(any("[new-consumer] %s" % surface in finding for finding in findings), findings)

    def test_raw_identifier_external_protocol_paths_are_rejected(self):
        consumer = self.repo / "rust/src/raw_external_consumer.rs"
        consumer.write_text(
            "use r#lean_ctx_protocol::control_plane::ControlPlaneRequest;\n"
            "extern crate r#lean_ctx_protocol;\n",
            encoding="utf-8",
        )
        findings = GATE.check_repo(self.repo)
        for surface in GATE.FROZEN_SURFACES:
            self.assertTrue(any("[new-consumer] %s" % surface in finding for finding in findings), findings)

    def test_alias_and_grouped_root_consumers_are_rejected(self):
        consumer = self.repo / "rust/src/aliased_path_consumer.rs"
        consumer.write_text(
            "use crate::proxy as p;\n"
            "use {crate::ControlPlaneRequest as CP};\n"
            "fn consume(_: p::auto_routing::RoutingMode, _: CP) {}\n",
            encoding="utf-8",
        )
        findings = GATE.check_repo(self.repo)
        self.assertTrue(any("[new-consumer] auto_routing" in finding for finding in findings), findings)
        self.assertTrue(any("[new-consumer] control_plane" in finding for finding in findings), findings)

    def test_unicode_alias_consumer_is_rejected(self):
        consumer = self.repo / "rust/src/unicode_alias_consumer.rs"
        consumer.write_text(
            "use crate::proxy as π;\n"
            "fn consume(_: π::auto_routing::RoutingMode) {}\n",
            encoding="utf-8",
        )
        findings = GATE.check_repo(self.repo)
        self.assertTrue(any("[new-consumer] auto_routing" in finding for finding in findings), findings)

    def test_alias_chain_consumer_is_rejected(self):
        consumer = self.repo / "rust/src/alias_chain_consumer.rs"
        consumer.write_text(
            "use crate::proxy as p;\n"
            "use p::auto_routing::RoutingMode;\n",
            encoding="utf-8",
        )
        findings = GATE.check_repo(self.repo)
        self.assertTrue(any("[new-consumer] auto_routing" in finding for finding in findings), findings)

    def test_parent_module_and_glob_consumers_are_rejected(self):
        cases = {
            "parent_consumer.rs": (
                "use crate::proxy;\n"
                "fn consume(_: proxy::auto_routing::RoutingMode) {}\n"
            ),
            "parent_group_consumer.rs": (
                "use crate::{proxy};\n"
                "fn consume(_: proxy::auto_routing::RoutingMode) {}\n"
            ),
            "parent_glob_consumer.rs": (
                "use crate::proxy::*;\n"
                "fn consume(_: auto_routing::RoutingMode) {}\n"
            ),
        }
        for filename, source in cases.items():
            with self.subTest(filename=filename):
                consumer = self.repo / "rust/src" / filename
                consumer.write_text(source, encoding="utf-8")
                findings = GATE.check_repo(self.repo)
                self.assertTrue(any("[new-consumer] auto_routing" in finding for finding in findings), findings)
                consumer.unlink()

    def test_rooted_alias_glob_consumer_is_rejected(self):
        self.assert_new_consumer(
            "alias_glob_consumer.rs",
            "use crate::proxy as p;\n"
            "use p::*;\n"
            "fn consume(_: auto_routing::RoutingMode) {}\n",
            "auto_routing",
        )

    def test_unicode_intermediate_and_grouped_paths_are_rejected(self):
        consumer = self.repo / "rust/src/unicode_qualified_consumer.rs"
        consumer.write_text(
            "use {crate::π::auto_routing::RoutingMode};\n"
            "fn consume(_: crate::π::auto_routing::RoutingMode, _: RoutingMode) {}\n",
            encoding="utf-8",
        )
        findings = GATE.check_repo(self.repo)
        self.assertTrue(any("[new-consumer] auto_routing" in finding for finding in findings), findings)

    def test_combining_mark_alias_is_rejected(self):
        consumer = self.repo / "rust/src/combining_alias_consumer.rs"
        alias = "pi\u0301"
        consumer.write_text(
            "use crate::proxy as %s;\nfn consume(_: %s::auto_routing::RoutingMode) {}\n"
            % (alias, alias),
            encoding="utf-8",
        )
        findings = GATE.check_repo(self.repo)
        self.assertTrue(any("[new-consumer] auto_routing" in finding for finding in findings), findings)

    def test_arabic_combining_mark_alias_is_rejected(self):
        consumer = self.repo / "rust/src/arabic_combining_alias_consumer.rs"
        alias = "p\u064b"
        consumer.write_text(
            "use crate as %s;\nfn consume(_: %s::auto_routing::RoutingMode) {}\n"
            % (alias, alias),
            encoding="utf-8",
        )
        findings = GATE.check_repo(self.repo)
        self.assertTrue(any("[new-consumer] auto_routing" in finding for finding in findings), findings)

    def test_crate_alias_root_symbol_is_rejected(self):
        consumer = self.repo / "rust/src/crate_alias_root_consumer.rs"
        consumer.write_text(
            "use crate as protocol;\nfn consume(_: protocol::ControlPlaneRequest) {}\n",
            encoding="utf-8",
        )
        findings = GATE.check_repo(self.repo)
        self.assertTrue(any("[new-consumer] control_plane" in finding for finding in findings), findings)

    def test_super_glob_bare_root_symbol_is_rejected(self):
        consumer = self.repo / "rust/crates/lean-ctx-protocol/src/super_glob_consumer.rs"
        consumer.write_text(
            "use super::*;\nfn consume(_: ControlPlaneRequest) {}\n",
            encoding="utf-8",
        )
        findings = GATE.check_repo(self.repo)
        self.assertTrue(any("[new-consumer] control_plane" in finding for finding in findings), findings)

    def test_visible_super_glob_variants_are_rejected(self):
        variants = (
            "pub use super::*;",
            "pub use super::{*};",
            "pub(crate) use super::*;",
        )
        for index, variant in enumerate(variants):
            with self.subTest(variant=variant):
                consumer = self.repo / "rust/crates/lean-ctx-protocol/src" / ("visible_glob_%d.rs" % index)
                consumer.write_text(
                    variant + "\nfn consume(_: ControlPlaneRequest) {}\n",
                    encoding="utf-8",
                )
                findings = GATE.check_repo(self.repo)
                self.assertTrue(any("[new-consumer] control_plane" in finding for finding in findings), findings)
                consumer.unlink()

    def test_raw_root_symbol_imports_are_rejected(self):
        consumer = self.repo / "rust/src/raw_root_symbol_import.rs"
        consumer.write_text(
            "use crate::{r#ControlPlaneRequest as CP};\nfn consume(_: CP) {}\n",
            encoding="utf-8",
        )
        findings = GATE.check_repo(self.repo)
        self.assertTrue(any("[new-consumer] control_plane" in finding for finding in findings), findings)

    def test_attributed_and_macro_consumers_are_rejected(self):
        consumer = self.repo / "rust/src/contextual_aliases.rs"
        consumer.write_text(
            "#[allow(unused_imports)] use crate::{r#ControlPlaneRequest as CP};\n"
            "macro_rules! import_symbol { ($name:ident) => { use super::$name; }; }\n"
            "import_symbol!(ControlPlaneRequest);\n",
            encoding="utf-8",
        )
        findings = GATE.check_repo(self.repo)
        self.assertTrue(any("[new-consumer] control_plane" in finding for finding in findings), findings)

    def test_attributed_super_glob_and_same_line_macro_are_rejected(self):
        consumer = self.repo / "rust/crates/lean-ctx-protocol/src/attributed_glob.rs"
        consumer.write_text(
            "#[allow(unused_imports)] use super::*;\n"
            "macro_rules! import_parent { () => { use super::*; }; }\n"
            "fn consume(_: ControlPlaneRequest) {}\n",
            encoding="utf-8",
        )
        findings = GATE.check_repo(self.repo)
        self.assertTrue(any("[new-consumer] control_plane" in finding for finding in findings), findings)

    def test_unicode_xid_alias_is_rejected(self):
        consumer = self.repo / "rust/src/unicode_xid_alias.rs"
        consumer.write_text(
            "use crate::proxy as ℘;\n"
            "fn consume(_: ℘::auto_routing::RoutingMode) {}\n",
            encoding="utf-8",
        )
        findings = GATE.check_repo(self.repo)
        self.assertTrue(any("[new-consumer] auto_routing" in finding for finding in findings), findings)

    def test_grouped_raw_root_symbol_after_sibling_is_rejected(self):
        self.assert_new_consumer(
            "grouped_raw_root_consumer.rs",
            "use super::{Other, r#ControlPlaneRequest as CP};\n",
            "control_plane",
        )

    def test_macro_surface_metavariable_is_resolved_at_its_invocation(self):
        self.assert_new_consumer(
            "macro_surface_consumer.rs",
            "macro_rules! path { ($name:ident) => { $name::RoutingMode }; }\n"
            "path!(auto_routing);\n",
            "auto_routing",
        )

    def test_uninvoked_and_unrelated_macros_are_not_consumers(self):
        consumer = self.repo / "rust/src/unrelated_macros.rs"
        consumer.write_text(
            "macro_rules! import_symbol { ($name:ident) => { use super::$name; }; }\n"
            "macro_rules! log_name { ($name:ident) => {}; }\n"
            "log_name!(ControlPlaneRequest);\n"
            "log_name!(auto_routing);\n",
            encoding="utf-8",
        )
        self.assertEqual(GATE.check_repo(self.repo), [])

    def test_macro_resource_bounds_fail_closed(self):
        consumer = self.repo / "rust/src/bounded_macro.rs"
        consumer.write_text(
            "macro_rules! path { ($name:ident) => { $name::RoutingMode }; }\n"
            "path!(auto_routing);\n",
            encoding="utf-8",
        )
        previous = GATE.MAX_RUST_MACROS
        GATE.MAX_RUST_MACROS = 0
        try:
            findings = GATE.check_repo(self.repo)
        finally:
            GATE.MAX_RUST_MACROS = previous
        self.assertEqual(findings, ["[manifest] Rust macro definition count exceeds limit"])

    def test_oversized_use_declaration_fails_closed(self):
        consumer = self.repo / "rust/src/oversized_use.rs"
        consumer.write_text(
            "use crate::{ControlPlaneRequest as CP};\n",
            encoding="utf-8",
        )
        previous = GATE.MAX_RUST_USE_BYTES
        GATE.MAX_RUST_USE_BYTES = 32
        try:
            findings = GATE.check_repo(self.repo)
        finally:
            GATE.MAX_RUST_USE_BYTES = previous
        self.assertEqual(findings, ["[manifest] Rust use declaration exceeds size limit"])

    def test_extern_self_alias_in_protocol_crate_is_rejected(self):
        consumer = self.repo / "rust/crates/lean-ctx-protocol/src/extern_self.rs"
        consumer.write_text("extern crate self as protocol;\n", encoding="utf-8")
        findings = GATE.check_repo(self.repo)
        for surface in GATE.FROZEN_SURFACES:
            self.assertTrue(any("[new-consumer] %s" % surface in finding for finding in findings), findings)

    def test_self_and_super_root_symbol_references_are_rejected(self):
        self.assert_new_consumer(
            "relative_root_consumer.rs",
            "fn first(_: self::ControlPlaneRequest) {}\n"
            "fn second(_: super::ControlPlaneRequest) {}\n",
            "control_plane",
        )

    def test_nested_surface_import_is_rejected(self):
        self.assert_new_consumer(
            "nested_surface_consumer.rs",
            "use crate::proxy::auto_routing::{RoutingMode};\n",
            "auto_routing",
        )

    def test_nested_grouped_and_spaced_surface_paths_are_rejected(self):
        self.assert_new_consumer(
            "nested_grouped_consumer.rs",
            "use crate::proxy::{auto_routing::{RoutingMode}};\n"
            "fn consume(_: crate :: proxy :: auto_routing :: RoutingMode) {}\n",
            "auto_routing",
        )

    def test_raw_identifier_surface_reference_is_rejected(self):
        self.assert_new_consumer(
            "raw_identifier_consumer.rs",
            "fn consume(_: crate::r#auto_routing::RoutingMode) {}\n",
            "auto_routing",
        )

    def test_comments_and_literals_are_not_consumers(self):
        consumer = self.repo / "rust/src/non_consumer_text.rs"
        consumer.write_text(
            "// crate::ControlPlaneRequest\n"
            "const TEXT: &str = \"lean_ctx_protocol::ControlPlaneRequest\";\n"
            "const RAW: &str = r#\"use crate::control_plane::ControlPlaneRequest;\"#;\n",
            encoding="utf-8",
        )
        self.assertEqual(GATE.check_repo(self.repo), [])

    def test_unrelated_surface_names_and_unused_crate_alias_are_not_consumers(self):
        consumer = self.repo / "rust/src/unrelated_names.rs"
        consumer.write_text(
            "use unrelated::auto_routing;\n"
            "use vendor::fleet_control;\n"
            "use vendor::rollout;\n"
            "use crate as p;\n"
            "extern crate self as local;\n"
            "extern crate lean_ctx_protocol_unused;\n"
            "use crate::auto_routingé::Thing;\n"
            "fn first(_: vendor::control_plane::ControlPlaneRequesté) {}\n",
            encoding="utf-8",
        )
        self.assertEqual(GATE.check_repo(self.repo), [])

    def test_identifier_suffixes_do_not_match_private_namespaces(self):
        consumer = self.repo / "rust/src/unrelated_private_names.rs"
        consumer.write_text(
            "fn first(_: fooπprivate::Thing) {}\n"
            "fn second(_: fooπenterprise::Thing) {}\n"
            "fn third(_: fooπcommercial::Thing) {}\n",
            encoding="utf-8",
        )
        self.assertEqual(GATE.check_repo(self.repo), [])

    def test_private_and_protocol_prefixes_are_not_namespace_matches(self):
        consumer = self.repo / "rust/src/unrelated_prefixed_names.rs"
        consumer.write_text(
            "extern crate private_vendor;\n"
            "extern crate lean_ctx_cloudy;\n"
            "extern crate lean_ctx_protocol_unused;\n",
            encoding="utf-8",
        )
        self.assertEqual(GATE.check_repo(self.repo), [])

    def test_combining_mark_suffixes_preserve_identifier_boundaries(self):
        mark = "\u0301"
        consumer = self.repo / "rust/src/combining_suffixes.rs"
        consumer.write_text(
            "extern crate private%s_vendor;\n"
            "extern crate lean_ctx_protocol%s_unused;\n"
            "use crate::auto_routing%s::Thing;\n"
            "use crate::ControlPlaneRequest%s as CP;\n"
            "fn consume(_: foo%sprivate::Thing) {}\n"
            "fn unicode(_: foo℘private::Thing, _: crate::ControlPlaneRequest℘) {}\n"
            % (mark, mark, mark, mark, mark),
            encoding="utf-8",
        )
        self.assertEqual(GATE.check_repo(self.repo), [])

    def test_surface_module_references_are_not_consumers(self):
        module = self.repo / "rust/crates/lean-ctx-protocol/src/control_plane.rs"
        module.write_text(
            module.read_text(encoding="utf-8")
            + "\nfn local_check(value: crate::ControlPlaneRequest) { let _ = value; }\n",
            encoding="utf-8",
        )
        findings = GATE.check_repo(self.repo)
        self.assertFalse(any("[new-consumer] control_plane" in finding for finding in findings))
        self.assertTrue(any("control_plane module digest drift" in finding for finding in findings))

    def test_private_cloud_import_remains_distinct(self):
        consumer = self.repo / "rust/src/private_consumer.rs"
        consumer.write_text(
            "use lean_ctx_enterprise::control_plane::ControlPlaneRequest;\n",
            encoding="utf-8",
        )
        findings = GATE.check_repo(self.repo)
        self.assertTrue(any("[private-import]" in finding for finding in findings))

    def test_cloud_import_and_enterprise_extern_are_private(self):
        consumer = self.repo / "rust/src/private_qualified_consumer.rs"
        consumer.write_text(
            "use lean_ctx_cloud::secret::Thing;\n"
            "extern crate lean_ctx_enterprise;\n",
            encoding="utf-8",
        )
        findings = GATE.check_repo(self.repo)
        private = [finding for finding in findings if "[private-import]" in finding]
        self.assertGreaterEqual(len(private), 2, findings)

    def test_raw_identifier_private_paths_are_rejected(self):
        consumer = self.repo / "rust/src/raw_private_consumer.rs"
        consumer.write_text(
            "use r#lean_ctx_cloud::restricted_api::Thing;\n"
            "extern crate r#lean_ctx_enterprise;\n",
            encoding="utf-8",
        )
        findings = GATE.check_repo(self.repo)
        private = [finding for finding in findings if "[private-import]" in finding]
        self.assertGreaterEqual(len(private), 2, findings)

    def test_private_namespace_aliases_are_rejected(self):
        consumer = self.repo / "rust/src/private_alias_consumer.rs"
        consumer.write_text(
            "use lean_ctx_cloud as cloud;\n"
            "use lean_ctx_enterprise as enterprise;\n"
            "use private as restricted;\n",
            encoding="utf-8",
        )
        findings = GATE.check_repo(self.repo)
        private = [finding for finding in findings if "[private-import]" in finding]
        self.assertEqual(len(private), 3, findings)

    def test_plain_private_qualified_and_extern_paths_are_rejected(self):
        consumer = self.repo / "rust/src/plain_private_consumer.rs"
        consumer.write_text(
            "extern crate private;\n"
            "extern crate enterprise;\n"
            "fn consume(_: private::Thing) {}\n",
            encoding="utf-8",
        )
        findings = GATE.check_repo(self.repo)
        private = [finding for finding in findings if "[private-import]" in finding]
        self.assertGreaterEqual(len(private), 3, findings)

    def test_all_plain_private_namespaces_are_rejected(self):
        consumer = self.repo / "rust/src/plain_private_namespaces.rs"
        consumer.write_text(
            "fn first(_: proprietary::Thing) {}\n"
            "fn second(_: commercial::Thing) {}\n"
            "fn third(_: strategic_data::Thing) {}\n"
            "fn fourth(_: enterprise::control_plane::Thing) {}\n",
            encoding="utf-8",
        )
        findings = GATE.check_repo(self.repo)
        private = [finding for finding in findings if "[private-import]" in finding]
        self.assertEqual(len(private), 4, findings)

    def test_raw_private_extern_and_enterprise_misc_are_rejected(self):
        consumer = self.repo / "rust/src/raw_private_extern_consumer.rs"
        consumer.write_text(
            "extern crate r#private as restricted;\n"
            "use enterprise::misc::Thing;\n"
            "use r#enterprise::misc::Other;\n",
            encoding="utf-8",
        )
        findings = GATE.check_repo(self.repo)
        private = [finding for finding in findings if "[private-import]" in finding]
        self.assertGreaterEqual(len(private), 3, findings)

    def test_commented_private_alias_is_rejected(self):
        consumer = self.repo / "rust/src/commented_private_alias.rs"
        consumer.write_text("use private /* comment */ as restricted;\n", encoding="utf-8")
        findings = GATE.check_repo(self.repo)
        self.assertTrue(any("[private-import]" in finding for finding in findings), findings)

    def test_non_rs_include_and_path_sources_are_rejected(self):
        include_source = self.repo / "rust/src/include_consumer.rs"
        include_source.write_text('include!("../hidden.inc");\n', encoding="utf-8")
        path_source = self.repo / "rust/src/path_consumer.rs"
        path_source.write_text('#[path = "../hidden.inc"]\nmod hidden;\n', encoding="utf-8")
        (self.repo / "rust/hidden.inc").write_text(
            "use lean_ctx_protocol::control_plane::ControlPlaneRequest;\n",
            encoding="utf-8",
        )
        findings = GATE.check_repo(self.repo)
        injections = [finding for finding in findings if "[source-injection]" in finding]
        self.assertEqual(len(injections), 2, findings)

    def test_symlinked_source_directory_fails_closed(self):
        target = self.repo / "outside"
        target.mkdir()
        link = self.repo / "rust/src/symlinked"
        try:
            link.symlink_to(target, target_is_directory=True)
        except OSError as error:
            self.skipTest("symlinks unavailable: %s" % error)
        findings = GATE.check_repo(self.repo)
        self.assertTrue(any("symlink path" in finding for finding in findings), findings)

    def test_unreadable_source_directory_fails_closed(self):
        restricted = self.repo / "rust/src/restricted"
        restricted.mkdir()
        (restricted / "consumer.rs").write_text(
            "use lean_ctx_protocol::control_plane::ControlPlaneRequest;\n",
            encoding="utf-8",
        )
        restricted.chmod(0)
        try:
            findings = GATE.check_repo(self.repo)
        finally:
            restricted.chmod(0o700)
        if not findings:
            self.skipTest("runtime can traverse mode-000 directories")
        self.assertTrue(findings[0].startswith("[manifest]"), findings)

    def test_source_entry_bound_fails_closed(self):
        previous = GATE.MAX_RUST_ENTRIES
        GATE.MAX_RUST_ENTRIES = 1
        try:
            findings = GATE.check_repo(self.repo)
        finally:
            GATE.MAX_RUST_ENTRIES = previous
        self.assertTrue(any("entry count exceeds limit" in finding for finding in findings), findings)

    def test_oversized_source_fails_closed(self):
        source = self.repo / "rust/src/oversized.rs"
        source.write_bytes(b" " * (GATE.MAX_RUST_SOURCE_BYTES + 1))
        findings = GATE.check_repo(self.repo)
        self.assertTrue(any("exceeds size limit" in finding for finding in findings), findings)

    def test_duplicate_manifest_key_fails_closed(self):
        raw = self.manifest.read_text(encoding="utf-8")
        marker = '"schema_version": "leanctx.public-protocol-surface-freeze/v1",'
        raw = raw.replace(marker, marker + "\n  " + marker, 1)
        self.manifest.write_text(raw, encoding="utf-8")
        findings = GATE.check_repo(self.repo)
        self.assertTrue(findings[0].startswith("[manifest]"), findings)

    def test_deeply_nested_manifest_fails_closed(self):
        self.manifest.write_text("[" * 1000 + "0" + "]" * 1000, encoding="utf-8")
        findings = GATE.check_repo(self.repo)
        self.assertTrue(findings[0].startswith("[manifest]"), findings)

    def test_nul_manifest_path_fails_closed(self):
        findings = GATE.check_repo(self.repo, Path("bad\0manifest.json"))
        self.assertTrue(findings[0].startswith("[manifest]"), findings)

    def test_nul_manifest_content_path_fails_closed(self):
        manifest = self.read_manifest()
        manifest["surfaces"]["rollout"]["module_path"] = "rust/src/\0bad.rs"
        self.write_manifest(manifest)
        findings = GATE.check_repo(self.repo)
        self.assertTrue(findings[0].startswith("[manifest]"), findings)

    def test_symlinked_manifest_parent_fails_closed(self):
        security = self.repo / "security"
        relocated = self.repo / "security-real"
        security.rename(relocated)
        security.symlink_to(relocated, target_is_directory=True)
        try:
            findings = GATE.check_repo(self.repo)
        finally:
            security.unlink()
            relocated.rename(security)
        self.assertTrue(any("symlink path" in finding for finding in findings), findings)

    def test_symlinked_rust_parent_fails_closed(self):
        rust = self.repo / "rust"
        relocated = self.repo / "rust-real"
        rust.rename(relocated)
        rust.symlink_to(relocated, target_is_directory=True)
        try:
            findings = GATE.check_repo(self.repo)
        finally:
            rust.unlink()
            relocated.rename(rust)
        self.assertTrue(any("symlink path" in finding for finding in findings), findings)

    def test_cli_json_output_is_deterministic(self):
        command = [sys.executable, str(SCRIPT), "--root", str(self.repo), "--json"]
        first = subprocess.run(command, capture_output=True, check=False)
        second = subprocess.run(command, capture_output=True, check=False)
        self.assertEqual(first.returncode, 0)
        self.assertEqual(first.stdout, second.stdout)
        self.assertEqual(first.stderr, second.stderr)
        self.assertEqual(json.loads(first.stdout)["status"], "pass")


if __name__ == "__main__":
    unittest.main()
