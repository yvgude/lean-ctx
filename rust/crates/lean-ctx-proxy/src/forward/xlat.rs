// SPDX-License-Identifier: Apache-2.0
#![cfg(feature = "shape-xlat")]

use axum::http::StatusCode;

/// The translated OpenAI body for a cross-shape route, or `None` when the
/// route is within-shape / absent / the body is untranslatable.
pub(super) fn translated_openai_body(
    route: Option<&super::super::routing::RouteDecision>,
    parsed: &serde_json::Value,
) -> Option<serde_json::Value> {
    route
        .filter(|r| r.xlat)
        .and_then(|_| super::super::shape_xlat::messages_to_chat(parsed))
}

/// Non-streaming translated response: chat.completion → Anthropic message on
/// success, error envelope on failure. Unrecognizable bodies pass unchanged
/// (better a shape-mismatched body than a dropped one).
pub(super) fn xlat_response_bytes(resp_bytes: &[u8], status: StatusCode) -> Vec<u8> {
    let translated = serde_json::from_slice::<serde_json::Value>(resp_bytes)
        .ok()
        .and_then(|v| {
            if status.is_success() {
                super::super::shape_xlat::chat_to_messages(&v)
            } else {
                super::super::shape_xlat::error_to_anthropic(&v)
            }
        });
    if let Some(v) = translated {
        serde_json::to_vec(&v).unwrap_or_else(|_| resp_bytes.to_vec())
    } else {
        tracing::warn!(
            "lean-ctx proxy: cross-shape response not translatable (status {status}) — \
             forwarding raw body"
        );
        resp_bytes.to_vec()
    }
}
