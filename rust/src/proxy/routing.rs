// SPDX-License-Identifier: Apache-2.0
//! Explicit model targets — operator-written aliases in the forward path.
//!
//! Runs between body parse and body compression: an exact alias match
//! (`[proxy.routing.aliases]`, [`RoutingRules`]) may replace the `model` field
//! and re-target the request to another upstream of the **same wire shape** —
//! or, with the `shape-xlat` feature (enterprise#16), route an Anthropic
//! `/v1/messages` request onto an OpenAI-shape upstream with the translation
//! flag set. `"acme/fast" = "foundry:gpt-4o-mini"` gives clients a stable org
//! name for an approved endpoint; a `local` target keeps traffic on-device.
//!
//! LeanCTX never picks a model on its own. The rewrite is recorded as
//! `routed_from` on the usage record so the operator can see it happened.
//! Missing rules/model and unavailable target shapes leave the body unchanged.

use crate::core::config::{
    ResolvedProvider, RoutingRules, Upstreams, WireShape, parse_route_target,
};

/// What an alias resolved to for one request. Applied by the forward path:
/// `model` already swapped in the body by [`route_request`]; the caller
/// re-targets the upstream and injects the registry credential if set.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteDecision {
    /// Model now in the body.
    pub model: String,
    /// Originally requested model (usage record `routed_from`).
    pub routed_from: String,
    /// Registry/builtin provider id serving the request after routing
    /// (usage attribution); `None` = upstream unchanged.
    pub provider_id: Option<String>,
    /// Override for the upstream base URL; `None` = keep the handler's.
    pub upstream_base: Option<String>,
    /// Registry entry whose `api_key_env` credential must be injected before
    /// the request leaves (gateway-held keys, enterprise#7).
    pub credential: Option<ResolvedProvider>,
    /// Target's local-inference flag (shadow-rate billing): `Some` for
    /// registry targets, `None` for built-ins (URL heuristic applies).
    pub local: Option<bool>,
    /// Cross-shape route (enterprise#16, feature `shape-xlat`): the Anthropic
    /// request body must be translated to OpenAI Chat Completions before it
    /// leaves, and the response translated back. Always `false` within-shape.
    pub xlat: bool,
}

/// Applies the alias rules to a parsed request body. On a match the body's
/// `model` field is rewritten in place and the decision is returned; without a
/// match or reachable target the body is untouched and `None` is returned.
///
/// `xlat_ok` — the caller vouches that this request may be shape-translated
/// (exact messages-create path, `shape-xlat` compiled in). Subpaths like
/// `count_tokens`/`batches` have no OpenAI equivalent and must stay
/// within-shape.
pub fn route_request(
    parsed: &mut serde_json::Value,
    provider_label: &str,
    upstreams: &Upstreams,
    rules: &RoutingRules,
    xlat_ok: bool,
) -> Option<RouteDecision> {
    if !rules.is_active() {
        return None;
    }
    // Body-addressed model dialects route. Gemini keys the model in the URL
    // path and ChatGPT-backend is OAuth'd Codex traffic — both passthrough.
    let request_shape = match provider_label {
        "Anthropic" => WireShape::Anthropic,
        "OpenAI" => WireShape::OpenAi,
        _ => return None,
    };
    let requested = parsed
        .get("model")
        .and_then(serde_json::Value::as_str)?
        .trim()
        .to_string();
    let target = rules.aliases.get(&requested)?;
    let (provider, new_model) = parse_route_target(target)?;
    let new_model = new_model.to_string();

    let resolved = match provider {
        None => ResolvedTarget::default(),
        Some(p) => resolve_provider(p, request_shape, upstreams, xlat_ok)?,
    };

    if new_model == requested && resolved.upstream_base.is_none() {
        return None; // no-op rule
    }

    parsed["model"] = serde_json::Value::String(new_model.clone());
    Some(RouteDecision {
        model: new_model,
        routed_from: requested,
        provider_id: resolved.provider_id,
        upstream_base: resolved.upstream_base,
        credential: resolved.credential,
        local: resolved.local,
        xlat: resolved.xlat,
    })
}

/// A resolved route target. `Default` = model-only rewrite (upstream unchanged).
#[derive(Default)]
struct ResolvedTarget {
    provider_id: Option<String>,
    upstream_base: Option<String>,
    credential: Option<ResolvedProvider>,
    local: Option<bool>,
    xlat: bool,
}

/// Resolves a route-target provider name, enforcing the shape rules: same
/// shape always routes; Anthropic→OpenAI routes with the translation flag when
/// the `shape-xlat` feature is compiled in and the caller allowed it. Unknown
/// ids and untranslatable shape pairs are logged and route nothing.
fn resolve_provider(
    name: &str,
    request_shape: WireShape,
    upstreams: &Upstreams,
    xlat_ok: bool,
) -> Option<ResolvedTarget> {
    let (target_shape, base_url, credential, local) = match name {
        "anthropic" => (
            WireShape::Anthropic,
            upstreams.anthropic.clone(),
            None,
            None,
        ),
        "openai" => (WireShape::OpenAi, upstreams.openai.clone(), None, None),
        "gemini" => (WireShape::Gemini, upstreams.gemini.clone(), None, None),
        id => {
            let Some(p) = upstreams.provider_by_id(id) else {
                tracing::warn!(
                    "[proxy.routing] target provider '{id}' not in [[proxy.providers]] — passthrough"
                );
                return None;
            };
            (
                p.shape,
                p.base_url.clone(),
                p.api_key_env.is_some().then(|| p.clone()),
                Some(p.local),
            )
        }
    };
    let xlat = if target_shape == request_shape {
        false
    } else if can_translate(
        request_shape,
        target_shape,
        xlat_ok,
        credential.as_ref(),
        local,
    ) {
        true
    } else {
        tracing::warn!(
            "[proxy.routing] target '{name}' speaks {} but the request is {} — \
             not translatable here, passthrough",
            target_shape.as_str(),
            request_shape.as_str()
        );
        return None;
    };
    Some(ResolvedTarget {
        provider_id: Some(name.to_string()),
        upstream_base: Some(base_url),
        credential,
        local,
        xlat,
    })
}

/// Anthropic→OpenAI is the supported translation pair (enterprise#16). The
/// upstream must be gateway-authenticated (`api_key_env`) or a local endpoint
/// (no auth) — the caller's Anthropic credentials mean nothing to an
/// OpenAI-shape provider.
#[cfg(feature = "shape-xlat")]
fn can_translate(
    request_shape: WireShape,
    target_shape: WireShape,
    xlat_ok: bool,
    credential: Option<&ResolvedProvider>,
    local: Option<bool>,
) -> bool {
    xlat_ok
        && request_shape == WireShape::Anthropic
        && target_shape == WireShape::OpenAi
        && (credential.is_some() || local == Some(true))
}

#[cfg(not(feature = "shape-xlat"))]
fn can_translate(
    _request_shape: WireShape,
    _target_shape: WireShape,
    _xlat_ok: bool,
    _credential: Option<&ResolvedProvider>,
    _local: Option<bool>,
) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn upstreams_with_foundry() -> Upstreams {
        Upstreams {
            anthropic: "https://api.anthropic.com".into(),
            openai: "https://api.openai.com".into(),
            chatgpt: "https://chatgpt.com".into(),
            gemini: "https://generativelanguage.googleapis.com".into(),
            providers: vec![
                ResolvedProvider {
                    id: "foundry".into(),
                    shape: WireShape::OpenAi,
                    base_url: "https://acme.services.ai.azure.com/openai".into(),
                    api_key_env: Some("FOUNDRY_API_KEY".into()),
                    aws_region: None,
                    local: false,
                },
                ResolvedProvider {
                    id: "claudeish".into(),
                    shape: WireShape::Anthropic,
                    base_url: "https://anthropic-gw.example.com".into(),
                    api_key_env: None,
                    aws_region: None,
                    local: false,
                },
            ],
        }
    }

    fn rules(aliases: &[(&str, &str)]) -> RoutingRules {
        RoutingRules {
            enabled: Some(true),
            aliases: aliases
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..RoutingRules::default()
        }
    }

    #[test]
    fn alias_routes_to_registry_provider_and_rewrites_model() {
        let mut body = json!({"model": "acme/fast", "messages": [{"role":"user","content":"hi"}]});
        let d = route_request(
            &mut body,
            "OpenAI",
            &upstreams_with_foundry(),
            &rules(&[("acme/fast", "foundry:gpt-4o-mini")]),
            false,
        )
        .expect("routed");
        assert_eq!(body["model"], "gpt-4o-mini");
        assert_eq!(d.routed_from, "acme/fast");
        assert_eq!(d.provider_id.as_deref(), Some("foundry"));
        assert_eq!(
            d.upstream_base.as_deref(),
            Some("https://acme.services.ai.azure.com/openai")
        );
        assert!(
            d.credential.is_some(),
            "foundry has api_key_env — credential must be injected"
        );
    }

    #[test]
    fn alias_model_only_keeps_upstream() {
        let mut body =
            json!({"model": "claude-opus-4-5", "messages": [{"role":"user","content":"hi"}]});
        let d = route_request(
            &mut body,
            "Anthropic",
            &upstreams_with_foundry(),
            &rules(&[("claude-opus-4-5", "claude-sonnet-4-5")]),
            false,
        )
        .expect("routed");
        assert_eq!(body["model"], "claude-sonnet-4-5");
        assert_eq!(d.upstream_base, None);
        assert_eq!(d.provider_id, None);
        assert_eq!(d.credential, None);
    }

    #[test]
    fn cross_shape_target_is_passthrough_when_xlat_not_allowed() {
        // Anthropic request → OpenAI-shape foundry with xlat_ok=false (wrong
        // path, e.g. count_tokens): must stay passthrough.
        let mut body =
            json!({"model": "claude-opus-4-5", "messages": [{"role":"user","content":"hi"}]});
        let before = body.clone();
        let d = route_request(
            &mut body,
            "Anthropic",
            &upstreams_with_foundry(),
            &rules(&[("claude-opus-4-5", "foundry:gpt-4o-mini")]),
            false,
        );
        assert_eq!(d, None);
        assert_eq!(body, before, "fail-open must leave the body untouched");
    }

    #[cfg(feature = "shape-xlat")]
    #[test]
    fn cross_shape_target_routes_with_translation_flag() {
        // enterprise#16: with the feature compiled in and the caller vouching
        // for the path, Anthropic → OpenAI-shape routes and marks xlat.
        let mut body =
            json!({"model": "claude-opus-4-5", "messages": [{"role":"user","content":"hi"}]});
        let d = route_request(
            &mut body,
            "Anthropic",
            &upstreams_with_foundry(),
            &rules(&[("claude-opus-4-5", "foundry:gpt-4o-mini")]),
            true,
        )
        .expect("cross-shape route with translation");
        assert!(d.xlat, "decision must carry the translation flag");
        assert_eq!(body["model"], "gpt-4o-mini");
        assert_eq!(d.provider_id.as_deref(), Some("foundry"));
        assert!(d.credential.is_some());

        // Within-shape decisions never set xlat.
        let mut body2 = json!({"model": "acme/fast", "messages": [{"role":"user","content":"hi"}]});
        let d2 = route_request(
            &mut body2,
            "OpenAI",
            &upstreams_with_foundry(),
            &rules(&[("acme/fast", "foundry:gpt-4o-mini")]),
            true,
        )
        .expect("within-shape route");
        assert!(!d2.xlat);
    }

    #[cfg(feature = "shape-xlat")]
    #[test]
    fn cross_shape_needs_gateway_credential_or_local_target() {
        // An OpenAI-shape target without api_key_env and not local cannot be
        // reached with the caller's Anthropic credentials → passthrough.
        let mut upstreams = upstreams_with_foundry();
        upstreams.providers.push(ResolvedProvider {
            id: "openaiish".into(),
            shape: WireShape::OpenAi,
            base_url: "https://oai-compat.example.com".into(),
            api_key_env: None,
            aws_region: None,
            local: false,
        });
        let mut body =
            json!({"model": "claude-opus-4-5", "messages": [{"role":"user","content":"hi"}]});
        let before = body.clone();
        let d = route_request(
            &mut body,
            "Anthropic",
            &upstreams,
            &rules(&[("claude-opus-4-5", "openaiish:gpt-4o-mini")]),
            true,
        );
        assert_eq!(d, None);
        assert_eq!(body, before);

        // The same target declared local (e.g. Ollama) needs no credential.
        upstreams.providers.last_mut().unwrap().local = true;
        let d = route_request(
            &mut body,
            "Anthropic",
            &upstreams,
            &rules(&[("claude-opus-4-5", "openaiish:llama3.3")]),
            true,
        )
        .expect("local cross-shape target routes");
        assert!(d.xlat);
        assert_eq!(d.local, Some(true));
    }

    #[cfg(feature = "shape-xlat")]
    #[test]
    fn openai_to_anthropic_direction_stays_passthrough() {
        // Only Anthropic→OpenAI is translated; the reverse pair passes through.
        let mut body = json!({"model": "gpt-5.2", "messages": [{"role":"user","content":"hi"}]});
        let d = route_request(
            &mut body,
            "OpenAI",
            &upstreams_with_foundry(),
            &rules(&[("gpt-5.2", "claudeish:claude-sonnet-4-5")]),
            true,
        );
        assert_eq!(d, None);
    }

    #[test]
    fn unknown_provider_and_disabled_rules_are_passthrough() {
        let mut body = json!({"model": "m", "messages": [{"role":"user","content":"hi"}]});
        let before = body.clone();
        assert_eq!(
            route_request(
                &mut body,
                "OpenAI",
                &upstreams_with_foundry(),
                &rules(&[("m", "nope:x")]),
                false,
            ),
            None
        );
        // enabled=false → inactive even with rules present.
        let mut off = rules(&[("m", "foundry:x")]);
        off.enabled = Some(false);
        assert_eq!(
            route_request(&mut body, "OpenAI", &upstreams_with_foundry(), &off, false),
            None
        );
        assert_eq!(body, before);
    }

    #[test]
    fn gemini_and_chatgpt_labels_are_passthrough() {
        let mut body = json!({"model":"m","messages":[{"role":"user","content":"hi"}]});
        for label in ["Gemini", "ChatGPT"] {
            assert_eq!(
                route_request(
                    &mut body,
                    label,
                    &upstreams_with_foundry(),
                    &rules(&[("m", "x")]),
                    false,
                ),
                None,
                "{label} must not route in M1"
            );
        }
    }

    #[test]
    fn a_removed_tier_table_never_rewrites_the_model() {
        // v4 removed automatic model selection: a legacy `[proxy.routing.tiers]`
        // table must neither activate the router nor change the request.
        let mut legacy = rules(&[]);
        legacy
            .tiers
            .insert("fast".to_string(), "foundry:phi-4".to_string());
        assert!(!legacy.is_active());
        let mut body = json!({"model":"gpt-5.2","messages":[{"role":"user","content":"where is the config?"}]});
        let before = body.clone();
        assert_eq!(
            route_request(
                &mut body,
                "OpenAI",
                &upstreams_with_foundry(),
                &legacy,
                false
            ),
            None
        );
        assert_eq!(body, before);
    }
}
