// SPDX-License-Identifier: Apache-2.0
//! Measured-cost plumbing: request-side opt-in (#1179) and response-header
//! extraction (#1189).
//!
//! OpenRouter only reports the billed charge (`usage.cost`, and
//! `cost_details.upstream_inference_cost` for BYOK) when the request opts in
//! with `"usage": {"include": true}`. This module decides *when* the proxy may
//! inject that opt-in and performs the injection.
//!
//! The gate is deliberately narrow: `usage` is not an OpenAI Chat Completions
//! parameter — api.openai.com rejects unknown top-level fields with a 400, and
//! other OpenAI-compatible upstreams (Azure, Groq, vLLM…) are not guaranteed
//! to tolerate it either. Injection therefore only happens when the *effective*
//! upstream host (post-routing) is openrouter.ai.
//!
//! Gateways that report the bill out-of-band do it via response headers:
//! LiteLLM sends `x-litellm-response-cost` (USD) on every proxied turn, and
//! corporate gateways often expose an equivalent under their own name
//! (`[proxy] cost_response_header`). `cost_from_headers` turns those into
//! the same measured figure the OpenRouter body path produces.

use axum::http::HeaderMap;
use serde_json::Value;

/// Response header LiteLLM proxies attach to every turn: the billed USD.
const LITELLM_COST_HEADER: &str = "x-litellm-response-cost";
/// Response header of the Hive models gateway (mz6): the credit charged for
/// the turn, integer micro-USD (`docs/model-access-subscription.md` §4.5).
const LEANCTX_COST_MICROS_HEADER: &str = "x-leanctx-cost-usd-micros";

/// True when `base_url` points at OpenRouter — the only upstream documented to
/// accept (and reward) the `usage.include` opt-in.
pub(super) fn upstream_is_openrouter(base_url: &str) -> bool {
    let rest = base_url
        .strip_prefix("https://")
        .or_else(|| base_url.strip_prefix("http://"))
        .unwrap_or(base_url);
    let host_port = rest.split(['/', '?']).next().unwrap_or(rest);
    let host = host_port.split(':').next().unwrap_or(host_port);
    host.eq_ignore_ascii_case("openrouter.ai")
        || host.to_ascii_lowercase().ends_with(".openrouter.ai")
}

/// Billed USD reported by the upstream gateway via response headers (#1189):
/// LiteLLM's standard header first, then the operator-configured extra header
/// (already lowercase). Unparseable, negative or non-finite values are
/// ignored — a broken gateway header must never poison the ledger.
pub(super) fn cost_from_headers(headers: &HeaderMap, extra_header: Option<&str>) -> Option<f64> {
    let mut names = vec![LITELLM_COST_HEADER];
    if let Some(h) = extra_header {
        names.push(h);
    }
    // mz6: the Hive models gateway charges the month's credit in integer
    // micro-USD and names that figure; it is the authoritative one.
    if let Some(micros) = headers
        .get(LEANCTX_COST_MICROS_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
    {
        return Some(micros as f64 / 1_000_000.0);
    }
    for name in names {
        let cost = headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<f64>().ok())
            .filter(|c| c.is_finite() && *c >= 0.0);
        if cost.is_some() {
            return cost;
        }
    }
    None
}

/// Injects `"usage": {"include": true}` into an OpenAI-shaped Chat Completions
/// body so OpenRouter's final usage payload carries the billed `cost`.
///
/// Respects the caller: an existing `usage.include` (even `false`) is never
/// overwritten, and a non-object `usage` value is left untouched.
pub(super) fn inject_usage_include(doc: &mut Value) {
    let Some(obj) = doc.as_object_mut() else {
        return;
    };
    let usage = obj
        .entry("usage")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if let Some(usage_obj) = usage.as_object_mut() {
        usage_obj.entry("include").or_insert(Value::Bool(true));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openrouter_hosts_are_recognized() {
        for url in [
            "https://openrouter.ai/api",
            "https://openrouter.ai",
            "http://openrouter.ai:443/api",
            "https://gateway.openrouter.ai/api",
            "https://OPENROUTER.AI/api",
        ] {
            assert!(
                upstream_is_openrouter(url),
                "{url} must count as OpenRouter"
            );
        }
    }

    #[test]
    fn other_upstreams_are_not_openrouter() {
        for url in [
            "https://api.openai.com",
            "https://my-resource.services.ai.azure.com",
            "https://api.groq.com/openai",
            "http://127.0.0.1:11434",
            "https://evil-openrouter.ai.example.com",
            "https://notopenrouter.ai",
        ] {
            assert!(
                !upstream_is_openrouter(url),
                "{url} must NOT count as OpenRouter"
            );
        }
    }

    #[test]
    fn litellm_cost_header_is_measured() {
        let mut h = HeaderMap::new();
        h.insert("x-litellm-response-cost", "0.00042".parse().unwrap());
        assert_eq!(cost_from_headers(&h, None), Some(0.00042));
        // The Hive models gateway's integer micro-USD wins (mz6).
        h.insert("x-leanctx-cost-usd-micros", "1250".parse().unwrap());
        assert_eq!(cost_from_headers(&h, None), Some(0.00125));
    }

    #[test]
    fn operator_header_is_recognized_and_junk_ignored() {
        let mut h = HeaderMap::new();
        h.insert("x-corp-billed-usd", "1.25".parse().unwrap());
        assert_eq!(
            cost_from_headers(&h, Some("x-corp-billed-usd")),
            Some(1.25),
            "configured gateway header must be read"
        );
        assert_eq!(
            cost_from_headers(&h, None),
            None,
            "unconfigured extra header is not consulted"
        );

        let mut junk = HeaderMap::new();
        junk.insert("x-litellm-response-cost", "not-a-number".parse().unwrap());
        junk.insert("x-corp-billed-usd", "-4".parse().unwrap());
        assert_eq!(
            cost_from_headers(&junk, Some("x-corp-billed-usd")),
            None,
            "unparseable and negative figures never enter the ledger"
        );
    }

    #[test]
    fn litellm_header_beats_extra_header_order() {
        let mut h = HeaderMap::new();
        h.insert("x-litellm-response-cost", "0.10".parse().unwrap());
        h.insert("x-corp-billed-usd", "9.99".parse().unwrap());
        assert_eq!(
            cost_from_headers(&h, Some("x-corp-billed-usd")),
            Some(0.10),
            "the standard header wins when both are present"
        );
    }

    #[test]
    fn injects_usage_include_when_absent() {
        let mut doc = serde_json::json!({"model": "deepseek/deepseek-v4-flash", "messages": []});
        inject_usage_include(&mut doc);
        assert_eq!(doc["usage"]["include"], Value::Bool(true));
    }

    #[test]
    fn existing_opt_out_is_respected() {
        let mut doc = serde_json::json!({"model": "m", "usage": {"include": false}});
        inject_usage_include(&mut doc);
        assert_eq!(doc["usage"]["include"], Value::Bool(false));
    }

    #[test]
    fn non_object_usage_is_left_untouched() {
        let mut doc = serde_json::json!({"model": "m", "usage": true});
        inject_usage_include(&mut doc);
        assert_eq!(doc["usage"], Value::Bool(true));
    }
}
