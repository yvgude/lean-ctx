// SPDX-License-Identifier: Apache-2.0
//! Model origin and host-country gate (mz14, `docs/model-access-subscription.md`
//! §7): decided on the destination before any network call, on every route.
//!
//! Classification comes from the bundled model catalog
//! (`lean_ctx::core::model_catalog`), never from what a provider entry
//! declares about itself (its `region` is not trusted):
//!
//! - **Vendor-direct host** (a catalog `[[direct]]` host, e.g. a vendor's own
//!   China-hosted API): refused when its host country or its vendor's model
//!   origin is blocked by policy; a CN host additionally needs the local
//!   opt-in `[proxy] allow_vendor_direct_cn = true`, which an org policy with
//!   `vendor_direct_cn = false` voids (403 `cn_direct_opt_in_required`).
//! - **Catalog model** (`leanctx/<id>`, or the bare id on the `leanctx`
//!   route): refused when its origin is blocked; on the subscription route
//!   also when every one of its hosts is in a blocked country.
//!
//! Default (no policy, no opt-in): subscription models pass, vendor-direct
//! CN is refused. A policy that could not be established fails closed.

use axum::body::Body;
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderValue, StatusCode};
use axum::response::Response;

use lean_ctx::core::model_catalog::{self, CatalogModel, MODEL_ID_PREFIX, ModelCatalog};
use lean_ctx::core::policy::RoutingPolicyRules;

use super::{PreparedProxyRequest, ProxyPrimitive, admission};
use crate::admitted::{GatewayRefusal, REASON_CODES_HEADER, REFUSAL_HEADER};

/// Reason code of a vendor-direct CN request without a valid opt-in (403).
pub(super) const CN_DIRECT_OPT_IN_REQUIRED: &str = "cn_direct_opt_in_required";
/// The model's weights come from a country the policy blocks.
const MODEL_ORIGIN_BLOCKED: &str = "destination.model_origin_blocked";
/// The request would be processed in a country the policy blocks.
const HOST_COUNTRY_BLOCKED: &str = "destination.host_country_blocked";
/// The destination URL has no host to classify.
const HOST_UNKNOWN: &str = "destination.host_unknown";
/// The bundled catalog failed its checks: nothing can be classified.
const CATALOG_UNAVAILABLE: &str = "destination.model_catalog_unavailable";
/// The policy could not be established (deny-all).
const POLICY_UNAVAILABLE: &str = "policy.origin_rules_unavailable";
/// Lifecycle stage reason of a refusal by this gate.
const ORIGIN_GATE_REFUSED: &str = "model origin gate refused request";
/// Registry id of the subscription route (T3, `core::hive_models::PROVIDER_ID`).
const SUBSCRIPTION_PROVIDER_ID: &str = "leanctx";

/// Where a request is about to go.
#[derive(Debug, Clone, Copy)]
pub(super) struct Destination<'a> {
    /// Final upstream base URL.
    pub base: &'a str,
    /// Model named in the outgoing body, if any.
    pub model: Option<&'a str>,
    /// Registry provider id of the route (`/providers/{id}`), if any.
    pub registry_id: Option<&'a str>,
}

/// Why the gate refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Refusal {
    /// Vendor-direct CN without the opt-in; `voided` when the org policy
    /// overrode a local opt-in.
    CnDirectOptIn {
        host: String,
        vendor: String,
        voided: bool,
    },
    /// Withheld with this reason code (400).
    Withheld(&'static str),
    /// The rules could not be established (503).
    Unavailable(&'static str),
}

/// The decision, pure: catalog, rules and the local opt-in in, verdict out.
pub(super) fn decide(
    catalog: &ModelCatalog,
    rules: &RoutingPolicyRules,
    local_opt_in: bool,
    dest: Destination<'_>,
) -> Result<(), Refusal> {
    let host = host_of(dest.base).ok_or(Refusal::Withheld(HOST_UNKNOWN))?;
    if let Some(direct) = catalog.direct_vendor(&host) {
        if rules.host_blocked(&direct.host_country) {
            return Err(Refusal::Withheld(HOST_COUNTRY_BLOCKED));
        }
        // The vendor's own API serves the vendor's own weights. A vendor
        // without a catalog model is taken to be of its host country.
        let mut origins = catalog
            .models
            .iter()
            .filter(|m| m.vendor == direct.vendor)
            .map(|m| m.origin_country.as_str())
            .peekable();
        let origin_blocked = if origins.peek().is_none() {
            rules.origin_blocked(&direct.host_country)
        } else {
            origins.any(|c| rules.origin_blocked(c))
        };
        if origin_blocked {
            return Err(Refusal::Withheld(MODEL_ORIGIN_BLOCKED));
        }
        if direct.host_country == "CN" {
            let voided = rules.vendor_direct_cn == Some(false);
            if !local_opt_in || voided {
                return Err(Refusal::CnDirectOptIn {
                    host,
                    vendor: direct.vendor.clone(),
                    voided: local_opt_in && voided,
                });
            }
        }
    }
    let subscription_route = dest.registry_id == Some(SUBSCRIPTION_PROVIDER_ID);
    if let Some(model) = dest
        .model
        .and_then(|m| catalog_model(catalog, m, subscription_route))
    {
        if rules.origin_blocked(&model.origin_country) {
            return Err(Refusal::Withheld(MODEL_ORIGIN_BLOCKED));
        }
        if subscription_route
            && model
                .hosts
                .iter()
                .all(|h| rules.host_blocked(&h.host_country))
        {
            return Err(Refusal::Withheld(HOST_COUNTRY_BLOCKED));
        }
    }
    Ok(())
}

/// The catalog model a request names: the full id on any route, the bare
/// id only on the subscription route (elsewhere it is the provider's own).
fn catalog_model<'c>(
    catalog: &'c ModelCatalog,
    model: &str,
    subscription_route: bool,
) -> Option<&'c CatalogModel> {
    catalog.model(model).or_else(|| {
        (subscription_route && !model.starts_with(MODEL_ID_PREFIX))
            .then(|| catalog.model(&format!("{MODEL_ID_PREFIX}{model}")))
            .flatten()
    })
}

fn host_of(base: &str) -> Option<String> {
    let uri = base.parse::<axum::http::Uri>().ok()?;
    let host = uri
        .host()?
        .trim_matches(|c| c == '[' || c == ']')
        .trim_end_matches('.');
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// Admit `dest` under the active policy and `config.toml`; `Some` is the
/// terminal answer when refused. No network call is made either way.
pub(super) fn admit(
    dest: Destination<'_>,
    provider_label: &str,
    original_bytes: usize,
    trace_id: &str,
) -> Option<PreparedProxyRequest> {
    let verdict = match model_catalog::bundled() {
        None => Err(Refusal::Unavailable(CATALOG_UNAVAILABLE)),
        Some(catalog) => match lean_ctx::core::policy::runtime::active() {
            Some(policy) if !policy.content_valid => Err(Refusal::Unavailable(POLICY_UNAVAILABLE)),
            policy => {
                let rules = policy
                    .as_ref()
                    .map(|p| p.resolved.routing.clone())
                    .unwrap_or_default();
                let opt_in = lean_ctx::core::config::Config::load()
                    .proxy
                    .allows_vendor_direct_cn();
                decide(catalog, &rules, opt_in, dest)
            }
        },
    };
    let refusal = verdict.err()?;
    let response = match &refusal {
        Refusal::CnDirectOptIn {
            host,
            vendor,
            voided,
        } => {
            tracing::warn!(
                "lean-ctx proxy: {host} is {vendor}'s own CN-hosted API (403 {CN_DIRECT_OPT_IN_REQUIRED})"
            );
            cn_direct_response(provider_label, host, vendor, *voided)
        }
        Refusal::Withheld(code) => {
            let refusal = GatewayRefusal::withheld(code);
            tracing::warn!("lean-ctx proxy: {}", refusal.message());
            return Some(admission::refused(
                &refusal,
                provider_label,
                original_bytes,
                trace_id,
            ));
        }
        Refusal::Unavailable(code) => {
            let refusal = GatewayRefusal::unavailable(code);
            tracing::warn!("lean-ctx proxy: {}", refusal.message());
            return Some(admission::refused(
                &refusal,
                provider_label,
                original_bytes,
                trace_id,
            ));
        }
    };
    Some(PreparedProxyRequest::Terminal(Box::new(
        ProxyPrimitive::Policy {
            response,
            original_tokens: original_bytes / 4,
            trace_id: trace_id.to_owned(),
            reason: ORIGIN_GATE_REFUSED,
        },
    )))
}

/// 403 `cn_direct_opt_in_required` in the error shape of the caller's SDK.
/// Deterministic and content-free: host and vendor come from the catalog.
fn cn_direct_response(provider_label: &str, host: &str, vendor: &str, voided: bool) -> Response {
    let how = if voided {
        "your organization's policy forbids vendor-direct CN APIs (vendor_direct_cn = false), \
         so the local opt-in does not apply"
    } else {
        "allow it with `[proxy] allow_vendor_direct_cn = true` in config.toml"
    };
    let message = format!(
        "request not forwarded: {host} is {vendor}'s own China-hosted API; {how} \
         ({CN_DIRECT_OPT_IN_REQUIRED}). This is not a login problem; logging in again does not help"
    );
    let detail = serde_json::json!({
        "refusal": "context_gateway",
        "reason_codes": [CN_DIRECT_OPT_IN_REQUIRED],
        "vendor": vendor,
        "host": host,
        "voided_by_policy": voided,
    });
    let body = match provider_label {
        "Anthropic" | "Bedrock" | "Copilot" => serde_json::json!({
            "type": "error",
            "error": { "type": "permission_error", "message": message },
            "lean_ctx": detail,
        }),
        "Gemini" => serde_json::json!({
            "error": { "code": 403, "message": message, "status": "PERMISSION_DENIED" },
            "lean_ctx": detail,
        }),
        _ => serde_json::json!({
            "error": {
                "message": message,
                "type": "permission_error",
                "param": null,
                "code": CN_DIRECT_OPT_IN_REQUIRED,
            },
            "lean_ctx": detail,
        }),
    };
    let mut response = Response::new(Body::from(body.to_string()));
    *response.status_mut() = StatusCode::FORBIDDEN;
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(REFUSAL_HEADER, HeaderValue::from_static("context_gateway"));
    headers.insert(
        REASON_CODES_HEADER,
        HeaderValue::from_static(CN_DIRECT_OPT_IN_REQUIRED),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(
        origins: &[&str],
        hosts: &[&str],
        vendor_direct_cn: Option<bool>,
    ) -> RoutingPolicyRules {
        RoutingPolicyRules {
            blocked_model_origins: origins.iter().map(|s| (*s).to_owned()).collect(),
            blocked_host_countries: hosts.iter().map(|s| (*s).to_owned()).collect(),
            vendor_direct_cn,
            ..RoutingPolicyRules::default()
        }
    }

    fn dest<'a>(base: &'a str, model: &'a str, registry_id: Option<&'a str>) -> Destination<'a> {
        Destination {
            base,
            model: Some(model),
            registry_id,
        }
    }

    #[test]
    fn vendor_direct_cn_needs_the_local_opt_in_and_org_false_voids_it() {
        let catalog = model_catalog::bundled().expect("bundled catalog");
        // The declared region of the entry plays no part: only the host does.
        let direct = dest("https://API.Z.ai./api/paas/v4", "glm-5", Some("zai"));
        let refused = |voided| {
            Err(Refusal::CnDirectOptIn {
                host: "api.z.ai".to_owned(),
                vendor: "Z.ai (Zhipu)".to_owned(),
                voided,
            })
        };
        let none = RoutingPolicyRules::default();
        assert_eq!(decide(catalog, &none, false, direct), refused(false));
        assert_eq!(decide(catalog, &none, true, direct), Ok(()));
        assert_eq!(
            decide(catalog, &rules(&[], &[], Some(true)), false, direct),
            refused(false),
            "an org true never replaces the local opt-in"
        );
        assert_eq!(
            decide(catalog, &rules(&[], &[], Some(false)), true, direct),
            refused(true)
        );
    }

    #[test]
    fn origin_and_host_rules_cover_every_route() {
        let catalog = model_catalog::bundled().expect("bundled catalog");
        let none = RoutingPolicyRules::default();
        let subscription = dest(
            "https://models.hive.example",
            "glm-5.3-flash",
            Some("leanctx"),
        );
        let own = dest(
            "https://openrouter.ai/api",
            "leanctx/glm-5.3-flash",
            Some("openrouter"),
        );

        // Default: subscription models pass, labelled CN-origin or not.
        assert_eq!(decide(catalog, &none, false, subscription), Ok(()));
        // Origin CN refuses the catalog model on the subscription route and
        // under its full id on an own provider.
        let cn_origin = rules(&["CN"], &[], None);
        for d in [subscription, own] {
            assert_eq!(
                decide(catalog, &cn_origin, false, d),
                Err(Refusal::Withheld(MODEL_ORIGIN_BLOCKED))
            );
        }
        // A bare id elsewhere is the provider's own model, not the catalog's.
        let bare_elsewhere = dest(
            "https://openrouter.ai/api",
            "glm-5.3-flash",
            Some("openrouter"),
        );
        assert_eq!(decide(catalog, &cn_origin, false, bare_elsewhere), Ok(()));
        // Host country: every catalog host blocked refuses the subscription.
        assert_eq!(
            decide(catalog, &rules(&[], &["US"], None), false, subscription),
            Err(Refusal::Withheld(HOST_COUNTRY_BLOCKED))
        );
        // ...and an own provider at a CN vendor host, opt-in or not.
        let direct = dest(
            "https://api.deepseek.com/v1",
            "deepseek-chat",
            Some("deepseek"),
        );
        assert_eq!(
            decide(catalog, &rules(&[], &["CN"], None), true, direct),
            Err(Refusal::Withheld(HOST_COUNTRY_BLOCKED))
        );
        // Origin blocks vendor-direct even with the opt-in.
        assert_eq!(
            decide(catalog, &cn_origin, true, direct),
            Err(Refusal::Withheld(MODEL_ORIGIN_BLOCKED))
        );
        // An unclassifiable destination fails closed.
        assert_eq!(
            decide(catalog, &none, true, dest("not a url", "m", None)),
            Err(Refusal::Withheld(HOST_UNKNOWN))
        );
    }
}
