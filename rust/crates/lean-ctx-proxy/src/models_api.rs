// SPDX-License-Identifier: Apache-2.0
//! `GET /v1/models` — the org's model catalog, served on the proxy port
//! (enterprise#63).
//!
//! IDE clients (Cursor, OpenCode, Codex, Claude Code) call this endpoint to
//! discover selectable models and to verify a configured API key. The catalog
//! is the org's *curated* namespace: every `[proxy.routing]` alias (e.g.
//! `"zuehlke/fast" = "foundry:deepseek-v4-flash"`) is listed under its alias
//! name — the routing engine rewrites it to the real provider/model on the
//! forward path. Tier rules are intent-based and transparent, so they are
//! deliberately not listed as selectable names.
//!
//! Auth: the route sits behind the standard proxy auth guard — a personal
//! gateway key (or the org Bearer token / a provider key on loopback)
//! authenticates. A `200` from this endpoint therefore doubles as the
//! "is my key valid?" check clients run at setup time.
//!
//! Shape: OpenAI list format by default; the Anthropic variant is returned
//! when the request carries Anthropic client headers (`anthropic-version` /
//! `x-api-key`). Output is deterministic (#498): alias order comes from the
//! config's `BTreeMap`, the `created` stamp is a fixed constant — never a
//! live clock.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};

use lean_ctx::core::config::{RoutingRules, parse_route_target};

/// Fixed `created` stamp for catalog entries (#498: no live clocks in bodies).
/// 2025-01-01T00:00:00Z — clients only need a stable integer, not a real date.
const CATALOG_CREATED_UNIX: i64 = 1_735_689_600;
const CATALOG_CREATED_RFC3339: &str = "2025-01-01T00:00:00Z";

/// `GET /v1/models` (and bare `/models`, normalized before routing).
pub async fn handler(req: axum::extract::Request) -> Response {
    let rules = lean_ctx::core::config::Config::load().proxy.routing.clone();
    let body = if wants_anthropic_shape(&req) {
        anthropic_model_list(&rules)
    } else {
        openai_model_list(&rules)
    };
    (StatusCode::OK, Json(body)).into_response()
}

/// Anthropic clients identify themselves by their credential/version headers;
/// everything else (Cursor, OpenAI SDKs, plain curl) gets the OpenAI shape.
fn wants_anthropic_shape(req: &axum::extract::Request) -> bool {
    req.headers().contains_key("anthropic-version") || req.headers().contains_key("x-api-key")
}

/// mz14: where the model behind `target` comes from and where it is served,
/// for an alias naming a subscription catalog model (`leanctx/<id>`, or the
/// bare id on the `leanctx` provider). From the bundled catalog.
fn origin_host(target: &str) -> Option<serde_json::Value> {
    let (provider, model) = parse_route_target(target)?;
    let model = if provider == Some("leanctx") {
        lean_ctx::core::model_catalog::model_for_request(model)
    } else {
        lean_ctx::core::model_catalog::bundled()?.model(model)
    }?;
    Some(serde_json::json!({
        "catalog_model": model.id,
        "origin_country": model.origin_country,
        "host_countries": model.hosts.iter().map(|h| h.host_country.as_str()).collect::<Vec<_>>(),
        "label": model.origin_host_label(),
    }))
}

/// The catalog rows: `(alias, owned_by, origin/host label)`. Empty when
/// routing is inactive — an honest empty list, never an invented model set.
fn catalog(rules: &RoutingRules) -> Vec<(String, String, Option<serde_json::Value>)> {
    if !rules.is_active() {
        return Vec::new();
    }
    rules
        .aliases
        .iter()
        .map(|(alias, target)| {
            let owned_by = parse_route_target(target)
                .and_then(|(provider, _)| provider.map(str::to_string))
                // Model-only alias (upstream unchanged) → owned by the gateway.
                .unwrap_or_else(|| "gateway".to_string());
            (alias.clone(), owned_by, origin_host(target))
        })
        .collect()
}

/// OpenAI `GET /v1/models` list shape; a catalog model carries its origin
/// and host countries under `lean_ctx` (clients ignore unknown fields).
fn openai_model_list(rules: &RoutingRules) -> serde_json::Value {
    let data: Vec<serde_json::Value> = catalog(rules)
        .into_iter()
        .map(|(id, owned_by, origin)| {
            let mut entry = serde_json::json!({
                "id": id,
                "object": "model",
                "created": CATALOG_CREATED_UNIX,
                "owned_by": owned_by,
            });
            if let Some(origin) = origin {
                entry["lean_ctx"] = origin;
            }
            entry
        })
        .collect();
    serde_json::json!({ "object": "list", "data": data })
}

/// Anthropic `GET /v1/models` list shape (single page, `has_more: false`).
fn anthropic_model_list(rules: &RoutingRules) -> serde_json::Value {
    let entries = catalog(rules);
    let first_id = entries.first().map(|(id, ..)| id.clone());
    let last_id = entries.last().map(|(id, ..)| id.clone());
    let data: Vec<serde_json::Value> = entries
        .into_iter()
        .map(|(id, _, origin)| {
            // The label is part of what a person sees when choosing.
            let display_name = match origin.as_ref().and_then(|o| o["label"].as_str()) {
                Some(label) => format!("{id} — {label}"),
                None => id.clone(),
            };
            let mut entry = serde_json::json!({
                "type": "model",
                "id": id,
                "display_name": display_name,
                "created_at": CATALOG_CREATED_RFC3339,
            });
            if let Some(origin) = origin {
                entry["lean_ctx"] = origin;
            }
            entry
        })
        .collect();
    serde_json::json!({
        "data": data,
        "has_more": false,
        "first_id": first_id,
        "last_id": last_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(aliases: &[(&str, &str)]) -> RoutingRules {
        RoutingRules {
            enabled: Some(true),
            aliases: aliases
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            tiers: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn openai_list_exposes_aliases_with_provider_ownership() {
        let body = openai_model_list(&rules(&[
            ("zuehlke/fast", "foundry:deepseek-v4-flash"),
            ("zuehlke/premium", "anthropic:claude-opus-4-5"),
            ("claude-opus-4-5", "claude-sonnet-4-5"), // model-only downgrade
        ]));
        assert_eq!(body["object"], "list");
        let data = body["data"].as_array().expect("data array");
        assert_eq!(data.len(), 3);
        // BTreeMap order: deterministic, alphabetical by alias (#498).
        assert_eq!(data[0]["id"], "claude-opus-4-5");
        assert_eq!(data[0]["owned_by"], "gateway", "model-only alias");
        assert_eq!(data[1]["id"], "zuehlke/fast");
        assert_eq!(data[1]["owned_by"], "foundry");
        assert_eq!(data[2]["id"], "zuehlke/premium");
        assert_eq!(data[2]["owned_by"], "anthropic");
        for m in data {
            assert_eq!(m["object"], "model");
            assert_eq!(m["created"], CATALOG_CREATED_UNIX);
        }
    }

    #[test]
    fn anthropic_list_mirrors_the_same_catalog() {
        let body = anthropic_model_list(&rules(&[
            ("zuehlke/fast", "foundry:deepseek-v4-flash"),
            ("zuehlke/premium", "anthropic:claude-opus-4-5"),
        ]));
        let data = body["data"].as_array().expect("data array");
        assert_eq!(data.len(), 2);
        assert_eq!(data[0]["type"], "model");
        assert_eq!(data[0]["id"], "zuehlke/fast");
        assert_eq!(data[0]["display_name"], "zuehlke/fast");
        assert_eq!(body["has_more"], false);
        assert_eq!(body["first_id"], "zuehlke/fast");
        assert_eq!(body["last_id"], "zuehlke/premium");
        assert!(
            data[0].get("lean_ctx").is_none(),
            "no catalog model, no label"
        );
    }

    #[test]
    fn catalog_models_are_listed_with_origin_and_host() {
        let aliases = rules(&[("fast", "leanctx:glm-5.3-flash")]);
        let openai = openai_model_list(&aliases);
        let origin = &openai["data"][0]["lean_ctx"];
        assert_eq!(origin["catalog_model"], "leanctx/glm-5.3-flash");
        assert_eq!(origin["origin_country"], "CN");
        assert_eq!(origin["host_countries"], serde_json::json!(["US"]));
        let anthropic = anthropic_model_list(&aliases);
        let shown = anthropic["data"][0]["display_name"].as_str().expect("name");
        assert!(shown.starts_with("fast — origin CN ("), "{shown}");
        assert!(shown.contains("hosted US ("), "{shown}");
    }

    #[test]
    fn inactive_routing_yields_an_honest_empty_list() {
        // enabled but no rules → inactive; disabled with rules → inactive.
        let empty = rules(&[]);
        assert_eq!(openai_model_list(&empty)["data"], serde_json::json!([]));
        let mut off = rules(&[("a", "b:c")]);
        off.enabled = Some(false);
        assert_eq!(openai_model_list(&off)["data"], serde_json::json!([]));
        let anth = anthropic_model_list(&off);
        assert_eq!(anth["data"], serde_json::json!([]));
        assert_eq!(anth["first_id"], serde_json::Value::Null);
    }

    #[test]
    fn output_is_deterministic_across_calls() {
        // Same rules → byte-identical JSON (#498: catalog responses must be
        // stable for provider-side prompt caching and cheap client polling).
        let r = rules(&[("z/fast", "foundry:m1"), ("a/slow", "local:m2")]);
        let a = serde_json::to_string(&openai_model_list(&r)).unwrap();
        let b = serde_json::to_string(&openai_model_list(&r)).unwrap();
        assert_eq!(a, b);
        // Alphabetical alias order regardless of insertion order.
        let data = openai_model_list(&r);
        assert_eq!(data["data"][0]["id"], "a/slow");
        assert_eq!(data["data"][1]["id"], "z/fast");
    }

    #[test]
    fn shape_detection_keys_on_anthropic_headers() {
        let openai_req = axum::http::Request::builder()
            .uri("/v1/models")
            .header("authorization", "Bearer gk-alice-abc")
            .body(axum::body::Body::empty())
            .unwrap();
        assert!(!wants_anthropic_shape(&openai_req));

        let claude_req = axum::http::Request::builder()
            .uri("/v1/models")
            .header("x-api-key", "gk-alice-abc")
            .header("anthropic-version", "2023-06-01")
            .body(axum::body::Body::empty())
            .unwrap();
        assert!(wants_anthropic_shape(&claude_req));
    }
}
