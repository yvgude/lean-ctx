//! Intent-based model routing (P8 / DIM 3 — Leistungsstufe).
//!
//! Classifies each request's last user message via [`crate::core::intent_engine`]
//! and resolves the resulting [`ModelTier`] to a concrete routing target using
//! `[proxy.routing.tiers]`. The forward path then rewrites the request body's
//! `model` field (and optionally re-targets the upstream provider).
//!
//! **Fail-open by construction:** any classification miss, absent tier key, or
//! empty target string leaves the request untouched. Premium work is never
//! silently downgraded unless the operator explicitly configures a tier target.
//!
//! **Opt-in only:** requires `[proxy.routing] enabled = true` AND at least one
//! tier entry. Without both, this module is a no-op.
//!
//! ## Interaction with other routing mechanisms
//!
//! - **Aliases** (exact model-name swap) run first — if the requested model
//!   matches an alias, the aliased target is used and tier routing is skipped.
//! - **Policy gate** (model ceiling, budgets) runs after routing — it sees the
//!   *post-routing* model and can veto it.
//! - **Effort routing** (thinking budget) is orthogonal — it adjusts the
//!   `reasoning_effort` / `thinking` parameter, not the model identity.
//!
//! ## Cost-quality awareness
//!
//! When live model prices are available (loaded by the proxy at startup from
//! `~/.config/lean-ctx/model-prices.json`), the router annotates its decision
//! with cost savings estimates. This is observability only — the tier lookup
//! is the authoritative routing decision, not a dynamic cost optimizer.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::config::{parse_route_target, RoutingRules};
use crate::core::intent_engine::{self, ModelTier, TaskClassification};

/// A routing decision record — emitted for observability and future OCLA bus
/// integration (P2). Deterministic: same input → same decision (#498).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoutingDecision {
    /// The model the client originally requested.
    pub requested_model: String,
    /// The model after routing (may be identical if no tier matched).
    pub routed_model: String,
    /// The provider the request is re-targeted to (None = keep upstream).
    pub routed_provider: Option<String>,
    /// The classified intent tier that drove the decision.
    pub tier: String,
    /// Classification confidence (0.0–1.0).
    pub confidence: f64,
    /// Why this tier was chosen (human-readable, deterministic).
    pub reasoning: String,
    /// Whether the model was actually changed.
    pub model_changed: bool,
    /// Estimated cost ratio (routed / original) when prices are known.
    pub estimated_cost_ratio: Option<f64>,
}

/// Applies intent-based tier routing to a request body.
///
/// Returns `Some(decision)` when routing is active and classification
/// succeeded. The caller must apply the model swap from the decision.
/// Returns `None` when routing is inactive or the request is exempt.
pub fn route(body: &Value, rules: &RoutingRules) -> Option<RoutingDecision> {
    if !rules.is_active() || rules.tiers.is_empty() {
        return None;
    }

    let requested_model = extract_model(body)?;

    // Aliases take priority — if the model matches an alias, tier routing
    // is skipped (the alias already resolved a specific target).
    if rules.aliases.contains_key(&requested_model) {
        return None;
    }

    let messages = body.get("messages")?;
    let last_user_content = extract_last_user_content(messages)?;

    let classification = intent_engine::classify(&last_user_content);
    let route = intent_engine::route_intent(&last_user_content, &classification);

    let tier_key = route.model_tier.as_str();
    let target = rules.tiers.get(tier_key)?;

    // Empty target = "keep the requested model for this tier".
    if target.is_empty() {
        return Some(RoutingDecision {
            requested_model: requested_model.clone(),
            routed_model: requested_model,
            routed_provider: None,
            tier: tier_key.to_string(),
            confidence: route.confidence,
            reasoning: route.reasoning,
            model_changed: false,
            estimated_cost_ratio: None,
        });
    }

    let (provider, model) = parse_route_target(target)?;

    let cost_ratio = estimate_cost_ratio(&requested_model, model);

    Some(RoutingDecision {
        requested_model: requested_model.clone(),
        routed_model: model.to_string(),
        routed_provider: provider.map(str::to_string),
        tier: tier_key.to_string(),
        confidence: route.confidence,
        reasoning: route.reasoning,
        model_changed: requested_model != model,
        estimated_cost_ratio: cost_ratio,
    })
}

/// Applies a routing decision to a mutable request body (in-place model swap).
pub fn apply_decision(body: &mut Value, decision: &RoutingDecision) {
    if !decision.model_changed {
        return;
    }
    if let Some(obj) = body.as_object_mut() {
        obj.insert(
            "model".to_string(),
            Value::String(decision.routed_model.clone()),
        );
    }
}

/// Classifies a request without applying routing — for dry-run / observability.
pub fn classify_only(body: &Value) -> Option<(TaskClassification, intent_engine::IntentRoute)> {
    let messages = body.get("messages")?;
    let content = extract_last_user_content(messages)?;
    let classification = intent_engine::classify(&content);
    let route = intent_engine::route_intent(&content, &classification);
    Some((classification, route))
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn extract_model(body: &Value) -> Option<String> {
    body.get("model")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Extracts the text content of the last user message from a messages array.
fn extract_last_user_content(messages: &Value) -> Option<String> {
    let arr = messages.as_array()?;
    for msg in arr.iter().rev() {
        let role = msg.get("role").and_then(Value::as_str)?;
        if role != "user" {
            continue;
        }
        // Content can be a string or an array of content blocks.
        match msg.get("content") {
            Some(Value::String(s)) => return Some(s.clone()),
            Some(Value::Array(blocks)) => {
                let text: String = blocks
                    .iter()
                    .filter_map(|b| {
                        if b.get("type").and_then(Value::as_str) == Some("text") {
                            b.get("text").and_then(Value::as_str)
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.is_empty() {
                    return Some(text);
                }
            }
            _ => continue,
        }
    }
    None
}

/// Rough cost ratio estimate based on known model price tiers.
/// Returns None when either model is unknown.
fn estimate_cost_ratio(original: &str, routed: &str) -> Option<f64> {
    let orig_cost = model_cost_tier(original)?;
    let routed_cost = model_cost_tier(routed)?;
    if orig_cost == 0.0 {
        return None;
    }
    Some(routed_cost / orig_cost)
}

/// Relative cost tier for well-known models (normalized to Sonnet = 1.0).
/// These are static approximations for estimation only — live prices from
/// the model-prices.json file are used for actual billing.
fn model_cost_tier(model: &str) -> Option<f64> {
    let m = model.to_lowercase();
    Some(match () {
        // Nano tier (~0.02x Sonnet) — must precede "mini" substring matches
        _ if m.contains("nano") || m.contains("gpt-4.1-nano") => 0.04,
        // Fast tier (~0.1-0.3x Sonnet) — must precede "gpt-4o" (catches "4o-mini")
        _ if m.contains("haiku")
            || m.contains("flash")
            || m.contains("4o-mini")
            || m.contains("4.1-mini")
            || m.contains("deepseek") =>
        {
            0.2
        }
        // Premium tier (~5x Sonnet)
        _ if m.contains("opus") || m.contains("o3-pro") || m.contains("o1-pro") => 5.0,
        // High tier (~2-3x Sonnet)
        _ if m.contains("o3") || m.contains("o1") || m.contains("gpt-5") => 2.5,
        // Standard tier (= Sonnet baseline)
        _ if m.contains("sonnet")
            || m.contains("gpt-4o")
            || m.contains("gemini-2.5-pro")
            || m.contains("gemini-2.0-pro") =>
        {
            1.0
        }
        _ => return None,
    })
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn active_rules(tiers: &[(&str, &str)]) -> RoutingRules {
        RoutingRules {
            enabled: Some(true),
            aliases: BTreeMap::new(),
            tiers: tiers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    fn request_body(model: &str, user_message: &str) -> Value {
        json!({
            "model": model,
            "messages": [
                {"role": "user", "content": user_message}
            ]
        })
    }

    // ─── Routing inactive / passthrough ──────────────────────────────────

    #[test]
    fn inactive_routing_returns_none() {
        let rules = RoutingRules::default();
        let body = request_body("claude-sonnet-4", "fix the bug");
        assert!(route(&body, &rules).is_none());
    }

    #[test]
    fn empty_tiers_returns_none() {
        let rules = RoutingRules {
            enabled: Some(true),
            aliases: BTreeMap::new(),
            tiers: BTreeMap::new(),
        };
        let body = request_body("claude-sonnet-4", "fix the bug");
        assert!(route(&body, &rules).is_none());
    }

    #[test]
    fn no_model_field_returns_none() {
        let body = json!({"messages": [{"role": "user", "content": "hi"}]});
        let rules = active_rules(&[("fast", "x")]);
        assert!(route(&body, &rules).is_none());
    }

    #[test]
    fn no_user_messages_returns_none() {
        let body = json!({
            "model": "claude-sonnet-4",
            "messages": [{"role": "assistant", "content": "hello"}]
        });
        let rules = active_rules(&[("fast", "x")]);
        assert!(route(&body, &rules).is_none());
    }

    #[test]
    fn alias_takes_priority_over_tier() {
        let mut rules = active_rules(&[("fast", "anthropic:claude-haiku-4-5")]);
        rules
            .aliases
            .insert("my-model".to_string(), "openai:gpt-4o".to_string());
        let body = request_body("my-model", "explain the code");
        assert!(route(&body, &rules).is_none(), "alias exempts from tiers");
    }

    // ─── Tier routing ────────────────────────────────────────────────────

    #[test]
    fn fast_tier_downgrades_explore_queries() {
        // "explain" + "how" → 2 Explore matches → confidence 0.85 → Fast
        let rules = active_rules(&[("fast", "anthropic:claude-haiku-4-5")]);
        let body = request_body("claude-sonnet-4", "explain how the cache works");
        let decision = route(&body, &rules).expect("should route");

        assert_eq!(decision.requested_model, "claude-sonnet-4");
        assert_eq!(decision.routed_model, "claude-haiku-4-5");
        assert_eq!(decision.routed_provider.as_deref(), Some("anthropic"));
        assert_eq!(decision.tier, "fast");
        assert!(decision.model_changed);
        assert!(decision.confidence > 0.5);
    }

    #[test]
    fn premium_tier_upgrades_generation_tasks() {
        let rules = active_rules(&[("premium", "anthropic:claude-opus-4")]);
        let body = request_body("claude-sonnet-4", "implement a new auth module with JWT");
        let decision = route(&body, &rules).expect("should route");

        assert_eq!(decision.routed_model, "claude-opus-4");
        assert_eq!(decision.tier, "premium");
        assert!(decision.model_changed);
    }

    #[test]
    fn standard_tier_for_fixbug() {
        // "fix" + "bug" → 2 FixBug matches → confidence 0.95 → Standard
        let rules = active_rules(&[
            ("fast", "claude-haiku-4-5"),
            ("standard", "claude-sonnet-4"),
            ("premium", "claude-opus-4"),
        ]);
        let body = request_body("claude-opus-4", "fix the bug in auth.rs");
        let decision = route(&body, &rules).expect("should route");

        assert_eq!(decision.tier, "standard");
        assert_eq!(decision.routed_model, "claude-sonnet-4");
    }

    #[test]
    fn missing_tier_key_is_passthrough() {
        // Only "fast" configured — standard/premium queries pass through.
        let rules = active_rules(&[("fast", "anthropic:claude-haiku-4-5")]);
        let body = request_body("claude-sonnet-4", "fix the null pointer bug in auth.rs");
        assert!(route(&body, &rules).is_none());
    }

    #[test]
    fn empty_tier_target_keeps_model() {
        // "explain" + "describe" → 2 Explore → Fast with confidence > 0.5
        let rules = active_rules(&[("fast", "")]);
        let body = request_body("claude-sonnet-4", "explain and describe this function");
        let decision = route(&body, &rules).expect("should route");

        assert_eq!(decision.routed_model, "claude-sonnet-4");
        assert!(!decision.model_changed);
        assert_eq!(decision.tier, "fast");
    }

    #[test]
    fn model_only_target_keeps_provider() {
        let rules = active_rules(&[("fast", "claude-haiku-4-5")]);
        let body = request_body("claude-sonnet-4", "explain what this function does");
        let decision = route(&body, &rules).expect("should route");

        assert_eq!(decision.routed_model, "claude-haiku-4-5");
        assert_eq!(decision.routed_provider, None, "no provider override");
        assert!(decision.model_changed);
    }

    // ─── Decision application ────────────────────────────────────────────

    #[test]
    fn apply_decision_rewrites_body() {
        let mut body = request_body("claude-sonnet-4", "explain");
        let decision = RoutingDecision {
            requested_model: "claude-sonnet-4".into(),
            routed_model: "claude-haiku-4-5".into(),
            routed_provider: Some("anthropic".into()),
            tier: "fast".into(),
            confidence: 0.85,
            reasoning: "explore(what) + low complexity -> fast".into(),
            model_changed: true,
            estimated_cost_ratio: Some(0.2),
        };
        apply_decision(&mut body, &decision);
        assert_eq!(body["model"], "claude-haiku-4-5");
    }

    #[test]
    fn apply_decision_noop_when_unchanged() {
        let mut body = request_body("claude-sonnet-4", "explain");
        let decision = RoutingDecision {
            requested_model: "claude-sonnet-4".into(),
            routed_model: "claude-sonnet-4".into(),
            routed_provider: None,
            tier: "standard".into(),
            confidence: 0.8,
            reasoning: "fix_bug(how) -> standard".into(),
            model_changed: false,
            estimated_cost_ratio: None,
        };
        apply_decision(&mut body, &decision);
        assert_eq!(body["model"], "claude-sonnet-4");
    }

    // ─── Cost estimation ─────────────────────────────────────────────────

    #[test]
    fn cost_ratio_estimates_downgrade_savings() {
        let rules = active_rules(&[("fast", "claude-haiku-4-5")]);
        let body = request_body("claude-sonnet-4", "explain what this module does");
        let decision = route(&body, &rules).expect("should route");
        let ratio = decision.estimated_cost_ratio.expect("known models");
        assert!(ratio < 1.0, "haiku cheaper than sonnet: {ratio}");
        assert!(ratio > 0.0);
    }

    #[test]
    fn cost_ratio_none_for_unknown_models() {
        let rules = active_rules(&[("fast", "custom-local-model")]);
        let body = request_body("claude-sonnet-4", "explain what this code does");
        let decision = route(&body, &rules).expect("should route");
        assert_eq!(decision.estimated_cost_ratio, None);
    }

    #[test]
    fn cost_tiers_are_ordered() {
        assert!(model_cost_tier("claude-opus-4").unwrap() > model_cost_tier("claude-sonnet-4").unwrap());
        assert!(model_cost_tier("claude-sonnet-4").unwrap() > model_cost_tier("claude-haiku-4-5").unwrap());
        assert!(model_cost_tier("gpt-4o").unwrap() > model_cost_tier("gpt-4o-mini").unwrap());
    }

    // ─── Content extraction ──────────────────────────────────────────────

    #[test]
    fn multipart_content_blocks_extracted() {
        let body = json!({
            "model": "claude-sonnet-4",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "explain how the session cache works"},
                    {"type": "image", "source": {"type": "base64"}}
                ]
            }]
        });
        let rules = active_rules(&[("fast", "claude-haiku-4-5")]);
        let decision = route(&body, &rules).expect("should extract text blocks");
        assert_eq!(decision.tier, "fast");
    }

    // ─── Determinism ─────────────────────────────────────────────────────

    #[test]
    fn decision_is_deterministic() {
        let rules = active_rules(&[
            ("fast", "claude-haiku-4-5"),
            ("standard", "claude-sonnet-4"),
            ("premium", "claude-opus-4"),
        ]);
        let body = request_body("claude-sonnet-4", "explain how the proxy routing works");
        let d1 = route(&body, &rules);
        let d2 = route(&body, &rules);
        assert_eq!(d1, d2, "routing must be deterministic (#498)");
    }
}
