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

/// Always-on Community capabilities. They are explicit Trust Core product
/// records rather than capabilities inferred to be free because they are local.
pub const COMMUNITY_ALWAYS_ON_FEATURES: &[&str] = &[
    "compression",
    "caching",
    "knowledge",
    "session",
    "gateway",
    "sensitivity_floor",
    "savings_ledger",
    "audit_trail",
    "routing",
    "team_seat_value_v1",
];

/// Community capabilities gated only by compilation.
pub const COMMUNITY_OPTIONAL_FEATURES: &[&str] = &[
    "ast_compression",
    "semantic_search",
    "http_server",
    "wasm_runtime",
    // Cross-shape routing Anthropic→OpenAI (enterprise#16, `shape-xlat`).
    "shape_translation",
];

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
        // The advertised primitive is deterministic Community routing.
        // Personalized adaptive routing has its own Pro capability ID.
        "routing": true,
        "team_seat_value_v1": true,
        "ast_compression": cfg!(feature = "tree-sitter"),
        "semantic_search": cfg!(feature = "embeddings"),
        "http_server": cfg!(feature = "http-server"),
        "wasm_runtime": cfg!(feature = "wasm"),
        "shape_translation": cfg!(feature = "shape-xlat"),
    })
}

/// Runtime-discovered extensions: installed plugins plus the registered
/// read-modes / compressors / chunkers (EPIC 12.9). The sandboxed extension
/// runtime (EPIC 12.8) expands what registers here.
fn extensions() -> Value {
    let (read_modes, compressors, chunkers) = crate::core::extension_registry::global()
        .read()
        .map(|r| (r.read_mode_names(), r.compressor_names(), r.chunker_names()))
        .unwrap_or_default();

    json!({
        "plugins": [],
        "tools": [],
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
    fn every_advertised_feature_has_an_explicit_product_record() {
        let v = capabilities_value();
        for key in v["features"].as_object().expect("features object").keys() {
            assert!(
                crate::core::product_capabilities::registry()
                    .find(key)
                    .is_some(),
                "server feature '{key}' lacks product classification"
            );
        }
    }

    #[test]
    fn community_always_on_features_are_available_without_an_account() {
        let v = capabilities_value();
        for key in COMMUNITY_ALWAYS_ON_FEATURES {
            assert_eq!(
                v["features"][key],
                json!(true),
                "Community capability '{key}' must be available"
            );
            let capability = crate::core::product_capabilities::registry()
                .find(key)
                .expect("feature classified");
            assert_eq!(
                capability.minimum_plan(),
                crate::core::billing::Plan::Community
            );
            assert!(!capability.account_required);
        }
    }

    #[test]
    fn reports_compiled_features() {
        let v = capabilities_value();
        // Always-on capabilities are unconditionally true.
        assert_eq!(v["features"]["compression"], json!(true));
        assert_eq!(v["features"]["savings_ledger"], json!(true));
        assert_eq!(v["features"]["team_seat_value_v1"], json!(true));
        assert_eq!(
            v["contracts"]["leanctx.contract.team_seat_value_v1.schema_version"],
            json!(1)
        );
        // Feature-gated flags mirror the compile-time cfg.
        assert_eq!(
            v["features"]["semantic_search"],
            json!(cfg!(feature = "embeddings"))
        );
    }
}
