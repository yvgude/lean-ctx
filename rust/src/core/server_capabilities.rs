//! `GET /v1/capabilities` — runtime discovery of what this lean-ctx instance
//! supports, so any client (any language) can branch on real features instead
//! of trial calls. The HTTP route lives in `http_server`; the payload builder
//! lives here so it stays compiled (and drift-tested) without the
//! `http-server` feature.
//!
//! Contract: `docs/contracts/capabilities-contract-v1.md`. The set of
//! [`TOP_LEVEL_KEYS`] is the stable contract surface and is bound to that doc
//! by `tests/capabilities_contract_up_to_date.rs`.
//!
//! Not to be confused with [`crate::core::capabilities`], which models RBAC
//! permissions (`fs:read`, …). This module describes *server* capabilities.

use serde_json::{Value, json};

use crate::core::contracts::{CAPABILITIES_CONTRACT_VERSION, status_kv, versions_kv};

/// Stable, documented top-level keys of the capabilities document.
pub const TOP_LEVEL_KEYS: [&str; 11] = [
    "contract_version",
    "server",
    "plane",
    "transports",
    "presets",
    "read_modes",
    "tools",
    "features",
    "extensions",
    "contracts",
    "contract_status",
];

/// Build the capabilities document for this running instance.
#[must_use]
pub fn capabilities_value() -> Value {
    let manifest = crate::core::mcp_manifest::manifest_value();
    let tool_names = tool_names(&manifest);
    let read_modes = manifest.get("read_modes").cloned().unwrap_or(Value::Null);
    let active_persona =
        crate::core::persona::Persona::resolve(&crate::core::config::Config::load());

    json!({
        "contract_version": CAPABILITIES_CONTRACT_VERSION,
        "server": {
            "name": "lean-ctx",
            "version": env!("CARGO_PKG_VERSION"),
            "persona": active_persona.name,
        },
        "plane": "personal",
        "transports": ["stdio-mcp", "http-mcp", "rest", "sse"],
        "presets": crate::core::persona::Persona::builtin_names(),
        "read_modes": read_modes,
        "tools": {
            "total": tool_names.len(),
            "names": tool_names,
        },
        "features": features(),
        "extensions": extensions(),
        "contracts": versions_kv(),
        // Stability per contract document (frozen|stable|experimental) so
        // clients can check compatibility before building against a surface
        // (GL #394). Additive: existing consumers are unaffected.
        "contract_status": status_kv(),
    })
}

fn tool_names(manifest: &Value) -> Vec<String> {
    manifest
        .get("tools")
        .and_then(|t| t.get("granular"))
        .and_then(|g| g.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Always-on local capabilities — free, ungated, unconditionally available in
/// every build. The heart of the Local-Free Invariant (RFC §6): these must
/// never depend on an account, license, or plan.
pub const LOCAL_ALWAYS_ON_FEATURES: &[&str] = &[
    "compression",
    "caching",
    "knowledge",
    "session",
    "gateway",
    "sensitivity_floor",
    "savings_ledger",
    "audit_trail",
];

/// Local capabilities that are free but gated by *compilation* only (Cargo
/// features) — never by account/license/plan.
pub const LOCAL_OPTIONAL_FEATURES: &[&str] = &[
    "ast_compression",
    "semantic_search",
    "http_server",
    "wasm_runtime",
];

/// Commercial-plane capabilities — additive, opt-in, and never required for any
/// local feature. Compiled in via opt-in Cargo features.
pub const COMMERCIAL_PLANE_FEATURES: &[&str] = &["team_server", "cloud_server"];

/// Always-on capabilities plus compiled-in feature flags. Booleans reflect what
/// this binary can actually do.
fn features() -> Value {
    json!({
        "compression": true,
        "caching": true,
        "knowledge": true,
        "session": true,
        "gateway": true,
        "sensitivity_floor": true,
        "savings_ledger": true,
        "audit_trail": true,
        "ast_compression": cfg!(feature = "tree-sitter"),
        "semantic_search": cfg!(feature = "embeddings"),
        "http_server": cfg!(feature = "http-server"),
        "wasm_runtime": cfg!(feature = "wasm"),
        "team_server": cfg!(feature = "team-server"),
        "cloud_server": cfg!(feature = "cloud-server"),
    })
}

/// Runtime-discovered extensions: installed plugins plus the registered
/// read-modes / compressors / chunkers (EPIC 12.9). The sandboxed extension
/// runtime (EPIC 12.8) expands what registers here.
fn extensions() -> Value {
    let plugins = crate::core::plugins::PluginManager::with_registry(|reg| {
        reg.enabled_plugins()
            .iter()
            .map(|p| {
                json!({
                    "name": p.manifest.plugin.name,
                    "version": p.manifest.plugin.version,
                    "permissions": p.manifest.trust.policy().declared_permissions(),
                })
            })
            .collect::<Vec<_>>()
    })
    .unwrap_or_default();

    let (read_modes, compressors, chunkers) = crate::core::extension_registry::global()
        .read()
        .map(|r| (r.read_mode_names(), r.compressor_names(), r.chunker_names()))
        .unwrap_or_default();

    let tools: Vec<Value> = crate::core::plugins::PluginManager::tool_specs()
        .iter()
        .map(|t| json!({ "name": t.name, "plugin": t.plugin_name }))
        .collect();

    json!({
        "plugins": plugins,
        "tools": tools,
        "read_modes": read_modes,
        "compressors": compressors,
        "chunkers": chunkers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_has_exactly_documented_top_level_keys() {
        let v = capabilities_value();
        let obj = v.as_object().expect("capabilities is an object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        let mut expected: Vec<&str> = TOP_LEVEL_KEYS.to_vec();
        expected.sort_unstable();
        assert_eq!(keys, expected, "top-level keys drifted from TOP_LEVEL_KEYS");
    }

    #[test]
    fn contract_version_matches_constant() {
        let v = capabilities_value();
        assert_eq!(v["contract_version"], json!(CAPABILITIES_CONTRACT_VERSION));
    }

    #[test]
    fn lists_real_tools_and_read_modes() {
        let v = capabilities_value();
        assert!(
            v["tools"]["total"].as_u64().unwrap_or(0) > 0,
            "expected at least one tool"
        );
        assert!(v["read_modes"]["modes"].is_array());
    }

    #[test]
    fn extensions_expose_registry_builtins() {
        let v = capabilities_value();
        let ext = &v["extensions"];
        assert!(ext["plugins"].is_array());
        let compressors = ext["compressors"].as_array().expect("compressors array");
        assert!(compressors.iter().any(|c| c == "identity"));
        assert!(
            ext["read_modes"]
                .as_array()
                .is_some_and(|a| a.iter().any(|m| m == "full"))
        );
        assert!(
            ext["chunkers"]
                .as_array()
                .is_some_and(|a| a.iter().any(|c| c == "lines"))
        );
    }

    #[test]
    fn feature_keys_partition_into_local_and_commercial() {
        // Every advertised feature must be classified as local (always-on or
        // compile-optional) or commercial — no unclassified flag. This keeps the
        // Local-Free Invariant lists honest as features are added.
        let v = capabilities_value();
        let keys: std::collections::BTreeSet<String> = v["features"]
            .as_object()
            .expect("features object")
            .keys()
            .cloned()
            .collect();
        let mut classified: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for k in LOCAL_ALWAYS_ON_FEATURES
            .iter()
            .chain(LOCAL_OPTIONAL_FEATURES)
            .chain(COMMERCIAL_PLANE_FEATURES)
        {
            classified.insert((*k).to_string());
        }
        assert_eq!(
            keys, classified,
            "every feature must be classified local vs commercial (Local-Free Invariant)"
        );
    }

    #[test]
    fn local_always_on_features_are_unconditionally_true() {
        let v = capabilities_value();
        for key in LOCAL_ALWAYS_ON_FEATURES {
            assert_eq!(
                v["features"][key],
                json!(true),
                "local capability '{key}' must be free + always on"
            );
        }
    }

    #[test]
    fn reports_compiled_features() {
        let v = capabilities_value();
        // Always-on capabilities are unconditionally true.
        assert_eq!(v["features"]["compression"], json!(true));
        assert_eq!(v["features"]["savings_ledger"], json!(true));
        // Feature-gated flags mirror the compile-time cfg.
        assert_eq!(
            v["features"]["semantic_search"],
            json!(cfg!(feature = "embeddings"))
        );
    }
}
