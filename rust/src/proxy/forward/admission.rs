// SPDX-License-Identifier: Apache-2.0
//! Gateway admission stages of the shared forward path.
//!
//! The request is admitted twice, around everything that transforms it:
//! [`admit_caller`] decides on what the caller supplied before any
//! optimisation, compression or store reads it, and [`admit_final`] holds the
//! bytes that leave — after compression, routing, translation and every
//! revert — to the destination they actually leave for. The decision rules
//! live in [`crate::proxy::admitted`]; this module adds what only the forward
//! path knows: the trusted destination metadata and the wire encoding.

use axum::http::request::Parts;

use super::{PreparedProxyRequest, ProxyPrimitive, prepare};
use crate::core::context_admission::egress::{self, EgressTarget};
use crate::proxy::admitted::{self, Admission, CallerAdmission, GatewayRefusal, Payload};
use crate::proxy::routing::RouteDecision;

pub(super) fn trusted_destination_binding<'a>(
    parts: &'a Parts,
    route: Option<&'a RouteDecision>,
) -> egress::TrustedDestinationBinding<'a> {
    let caller_scope_override = [
        "openai-organization",
        "openai-project",
        "chatgpt-account-id",
    ]
    .iter()
    .any(|header| parts.headers.contains_key(*header))
        || crate::proxy::providers::has_untrusted_key_query(&parts.uri);
    let signed_region = parts
        .extensions
        .get::<crate::proxy::bedrock::SigningContext>()
        .map(|context| context.region.as_str());

    // A provider-target alias carries its own binding. A model-only alias
    // leaves the serving route unchanged, so a direct `/providers/{id}` route
    // must retain the registry metadata already attached by its handler. An
    // explicit built-in target has an upstream override but no registry ID;
    // its URL heuristic and shape label intentionally replace the source entry.
    if let Some(route) =
        route.filter(|route| route.provider_id.is_some() || route.upstream_base.is_some())
    {
        return egress::TrustedDestinationBinding {
            provider_id: route.provider_id.as_deref(),
            local: route.local,
            account_ref: (route.gateway_managed_credential && !caller_scope_override)
                .then_some(route.account_ref.as_deref())
                .flatten(),
            region: signed_region.or(route.region.as_deref()),
        };
    }

    parts
        .extensions
        .get::<crate::proxy::providers::RegistryProviderId>()
        .map_or_else(
            || egress::TrustedDestinationBinding {
                region: signed_region,
                ..Default::default()
            },
            |provider| egress::TrustedDestinationBinding {
                provider_id: Some(&provider.id),
                local: Some(provider.local),
                account_ref: (provider.gateway_managed_credential && !caller_scope_override)
                    .then_some(provider.account_ref.as_deref())
                    .flatten(),
                region: signed_region.or(provider.region.as_deref()),
            },
        )
}

fn payload(parsed: Option<&serde_json::Value>, opaque_bytes: usize) -> Payload<'_> {
    parsed.map_or(Payload::Opaque(opaque_bytes), Payload::Document)
}

fn model_of(parsed: Option<&serde_json::Value>) -> Option<&str> {
    parsed
        .and_then(|doc| doc.get("model"))
        .and_then(serde_json::Value::as_str)
}

/// Where a request is headed: the provider label of the serving handler, the
/// upstream it addresses by default and the alias decision, if any.
#[derive(Clone, Copy)]
pub(super) struct Leaving<'a> {
    pub(super) provider: &'a str,
    pub(super) upstream_base: &'a str,
    pub(super) route: Option<&'a RouteDecision>,
}

/// The content-free identity of the caller of this request, as one dimension
/// provider-sealed items are bound to. Two sources, both read before any
/// gateway-held credential replaces the caller's headers:
///
/// - the person a per-person gateway key resolved to (enterprise builds; set
///   by the auth guard, trusted);
/// - the upstream credential the caller presented. The gateway cannot verify
///   whose it is, but the destination does: an item produced for one
///   credential is not handed to a request presenting another.
///
/// Not used: agent or session ids from request headers or bodies — caller
/// claims the proxy cannot check. `None`: the request carries neither, so
/// the route has one context for all its callers.
pub(crate) fn sealed_principal(parts: &Parts) -> Option<String> {
    #[cfg(feature = "enterprise")]
    let person = parts
        .extensions
        .get::<crate::proxy::gateway_identity::GatewayTags>()
        .and_then(|tags| tags.person.as_deref());
    #[cfg(not(feature = "enterprise"))]
    let person = None;
    principal_of(&parts.headers, person)
}

/// [`sealed_principal`] from the headers the caller presented and the
/// gateway person, where a rail admits after the request parts are gone: the
/// turns and frames of a WebSocket, whose credential came with the upgrade.
pub(crate) fn principal_of(
    headers: &axum::http::HeaderMap,
    person: Option<&str>,
) -> Option<String> {
    let mut identity = blake3::Hasher::new();
    let mut known = false;
    if let Some(person) = person {
        identity.update(b"person\0");
        identity.update(person.as_bytes());
        known = true;
    }
    for header in ["authorization", "x-api-key", "x-goog-api-key"] {
        if let Some(value) = headers.get(header) {
            identity.update(header.as_bytes());
            identity.update(b"\0");
            identity.update(value.as_bytes());
            known = true;
        }
    }
    known.then(|| identity.finalize().to_hex().to_string())
}

/// Stage 1 (G01): admit the caller's body for the destination the request is
/// expected to leave for. `leaving.route` is the alias decision the router
/// will take for this body's model; it reads no content.
pub(super) fn admit_caller(
    parts: &Parts,
    parsed: Option<&serde_json::Value>,
    raw_bytes: usize,
    leaving: Leaving<'_>,
) -> Result<CallerAdmission, GatewayRefusal> {
    let target = EgressTarget {
        provider: leaving.provider,
        model: leaving
            .route
            .map(|route| route.model.as_str())
            .or_else(|| model_of(parsed)),
        upstream_base: leaving
            .route
            .and_then(|route| route.upstream_base.as_deref())
            .unwrap_or(leaving.upstream_base),
    };
    admitted::admit_caller(
        payload(parsed, raw_bytes),
        &target,
        trusted_destination_binding(parts, leaving.route),
        sealed_principal(parts),
    )
}

/// The body that leaves and the admission its receipt is built from.
pub(super) struct FinalAdmission {
    pub(super) body: Vec<u8>,
    pub(super) admission: Admission,
}

/// Stage 2 (G05): final egress control on exactly the body that leaves, for
/// the upstream `leaving.upstream_base` it is addressed to after routing. A
/// parsed body is re-encoded only when this pass changed something, so an
/// admitted request keeps its exact bytes (#1912); a body the proxy cannot
/// parse is handled as an opaque payload. The returned outcome may be a
/// refusal.
///
/// `restore` is the request's answer to a decision that would alter or
/// withhold a recovery stub it issued (`admitted::admit_final_with`): the
/// body with those blocks back to their originals, which is then the one
/// admitted and sent.
pub(super) fn admit_final(
    parts: &Parts,
    body: Vec<u8>,
    parsed: Option<&serde_json::Value>,
    leaving: Leaving<'_>,
    caller: CallerAdmission,
    untouched: bool,
    restore: impl FnOnce(Option<&serde_json::Value>) -> Option<serde_json::Value>,
) -> Result<FinalAdmission, GatewayRefusal> {
    let target = EgressTarget {
        provider: leaving.provider,
        model: model_of(parsed),
        upstream_base: leaving.upstream_base,
    };
    let (admission, rewritten) = admitted::admit_final_with(
        payload(parsed, body.len()),
        &target,
        trusted_destination_binding(parts, leaving.route),
        caller,
        untouched,
        restore,
    )?;
    let body = match rewritten {
        Some(doc) => serde_json::to_vec(&doc)
            .ok()
            .and_then(|logical| prepare::encode_request_body(parts, logical).ok())
            .ok_or_else(|| GatewayRefusal::unavailable("egress.encode_failed"))?,
        None => body,
    };
    Ok(FinalAdmission { body, admission })
}

/// Stage 0 (AP10-C2): the caller's path and query and the values of the
/// caller headers the forward relays (its allow-list), admitted for the
/// destination the body is expected to leave for — before the body. They
/// leave with it; a finding refuses the request.
pub(super) async fn admit_target(
    parts: &Parts,
    parsed: Option<&serde_json::Value>,
    leaving: Leaving<'_>,
) -> Result<(), GatewayRefusal> {
    let target = EgressTarget {
        provider: leaving.provider,
        model: leaving
            .route
            .map(|route| route.model.as_str())
            .or_else(|| model_of(parsed)),
        upstream_base: leaving
            .route
            .and_then(|route| route.upstream_base.as_deref())
            .unwrap_or(leaving.upstream_base),
    };
    admitted::admit_request_target(
        parts
            .uri
            .path_and_query()
            .map_or("/", axum::http::uri::PathAndQuery::as_str),
        parts
            .headers
            .iter()
            .filter(|(name, _)| super::headers::should_forward_request_header(name.as_str(), true)),
        &target,
        trusted_destination_binding(parts, leaving.route),
        sealed_principal(parts).as_deref(),
    )
    .await
}

/// Answer the caller with the typed refusal instead of dispatching.
pub(super) fn refused(
    refusal: &GatewayRefusal,
    provider_label: &str,
    original_bytes: usize,
    trace_id: &str,
) -> PreparedProxyRequest {
    PreparedProxyRequest::Terminal(Box::new(ProxyPrimitive::Policy {
        response: refusal.response(provider_label),
        original_tokens: original_bytes / 4,
        trace_id: trace_id.to_owned(),
        reason: GATEWAY_REFUSED,
    }))
}

/// Lifecycle stage reason of a gateway refusal.
pub(super) const GATEWAY_REFUSED: &str = "context gateway refused request";
