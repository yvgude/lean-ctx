//! End-to-end proof for manifest-declared tools (EPIC 12.11): a plugin that
//! ships a `[[tools]]` entry is discovered, registered as a native MCP tool
//! **without forking** `build_registry()`, and invocable as a sandboxed
//! subprocess. Own integration-test binary so the global plugin registry
//! (`OnceLock`) is fresh. Unix-only: the tool is the `cat` echo binary.

#![cfg(unix)]

use std::fs;

use lean_ctx::core::plugins::tools::invoke;
use lean_ctx::core::plugins::PluginManager;
use lean_ctx::server::registry::build_registry;

#[test]
fn manifest_tool_is_discovered_registered_and_invocable() {
    let root = tempfile::tempdir().expect("tempdir");
    let plugin_dir = root.path().join("weather-plugin");
    fs::create_dir_all(&plugin_dir).expect("plugin dir");

    // `cat` echoes the JSON args we pass on stdin straight back as the result —
    // a real subprocess round-trip with no mock layer.
    fs::write(
        plugin_dir.join("plugin.toml"),
        "[plugin]\nname = \"weather-plugin\"\nversion = \"1.0.0\"\n\n\
         [[tools]]\nname = \"weather_lookup\"\n\
         description = \"Look up the weather\"\ncommand = \"cat\"\ntimeout_ms = 5000\n\
         input_schema = { type = \"object\", properties = { city = { type = \"string\" } } }\n",
    )
    .expect("manifest");

    std::env::set_var("LEAN_CTX_PLUGINS_DIR", root.path());
    PluginManager::init();

    // 1) Discovered from the manifest (no fork).
    let specs = PluginManager::tool_specs();
    let spec = specs
        .iter()
        .find(|s| s.name == "weather_lookup")
        .expect("manifest tool should be discovered");
    assert_eq!(spec.plugin_name, "weather-plugin");

    // 2) Appears in the native tool surface (gateway / list_tools).
    let registry = build_registry();
    assert!(
        registry.contains("weather_lookup"),
        "manifest tool must register as a native MCP tool without a code edit"
    );

    // 3) Runs sandboxed: the subprocess echoes our arguments back.
    let out = invoke(spec, "{\"city\":\"Bern\"}").expect("tool invocation");
    assert!(out.contains("Bern"), "tool output should echo args: {out}");
}
