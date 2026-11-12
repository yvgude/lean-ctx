// SPDX-License-Identifier: Apache-2.0
//! The built-in `leanctx` provider: the Hive subscription models (mz6,
//! `docs/model-access-subscription.md` §4).
//!
//! `/providers/leanctx/v1/chat/completions` (or `/v1/responses`) forwards,
//! OpenAI-shaped, to the Hive models gateway. The credential is the
//! instance's short-lived `hive_i_` token from the registered
//! [`HiveTokenSource`]; every caller credential header is dropped, so the
//! caller's lean-ctx token or a provider key never reaches the gateway, and
//! the token only ever goes to the origin the source pinned to the Hive host.
//!
//! The gateway's refusals hold the route in the throttle style: a `402`,
//! `403` or `429 quota_exceeded` is recorded by the source, and until it
//! ends every request is answered here (same status and code, with
//! `Retry-After`) without a network call. A `401` makes the next admission
//! mint a fresh token.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, Request, StatusCode},
    response::Response,
};

use lean_ctx::core::hive_models::{
    HiveTokenSource, ModelsAdmission, ModelsRefusal, PROVIDER_ID, TOKEN_UNAVAILABLE_CODE,
    UNAVAILABLE_CODE, hive_token_source,
};

use super::{ProxyState, forward, providers::RegistryProviderId};

/// The longest wait for an admission (a token mint is one Hive call).
const ADMIT_TIMEOUT: Duration = Duration::from_secs(5);
/// The most of a refusal body that is read to record its hold.
const REFUSAL_BODY_LIMIT: usize = 64 * 1024;

/// The gateway refused the cached token: the next admission mints one.
static FRESH_TOKEN: AtomicBool = AtomicBool::new(false);

/// Every header a caller may authenticate with; none reaches the gateway.
const CALLER_CREDENTIAL_HEADERS: [&str; 4] =
    ["authorization", "x-api-key", "api-key", "x-goog-api-key"];

fn json_answer(status: StatusCode, body: &serde_json::Value, retry_after: Option<u64>) -> Response {
    let mut response = Response::new(Body::from(body.to_string()));
    *response.status_mut() = status;
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    if let Some(seconds) = retry_after
        && let Ok(value) = HeaderValue::from_str(&seconds.to_string())
    {
        response
            .headers_mut()
            .insert(axum::http::header::RETRY_AFTER, value);
    }
    response
}

/// The local answer to a refused admission. The body follows the Hive
/// error envelope, so a client reads it like the gateway's own answer.
pub(super) fn refusal_response(refusal: &ModelsRefusal) -> Response {
    let error = |code: &str, message: &str| serde_json::json!({"error": {"code": code, "message": message}});
    match refusal {
        ModelsRefusal::Unavailable => json_answer(
            StatusCode::FORBIDDEN,
            &error(
                UNAVAILABLE_CODE,
                "the subscription models are not available to this instance \
                 (connect it to Hive on a plan that includes them)",
            ),
            None,
        ),
        ModelsRefusal::OriginRefused(why) => {
            tracing::warn!("lean-ctx proxy: [hive] models_url refused: {why}");
            json_answer(
                StatusCode::FORBIDDEN,
                &error(
                    UNAVAILABLE_CODE,
                    "[hive] models_url is not on the Hive host; the instance token is not sent",
                ),
                None,
            )
        }
        ModelsRefusal::Held {
            status,
            code,
            retry_after_s,
            line,
        } => json_answer(
            StatusCode::from_u16(*status).unwrap_or(StatusCode::TOO_MANY_REQUESTS),
            &serde_json::json!({"error": {
                "code": code,
                "message": line,
                "retry_after_s": retry_after_s,
            }}),
            Some(*retry_after_s),
        ),
        ModelsRefusal::TokenUnavailable => json_answer(
            StatusCode::SERVICE_UNAVAILABLE,
            &error(
                TOKEN_UNAVAILABLE_CODE,
                "no Hive instance token could be had; try again shortly",
            ),
            Some(30),
        ),
    }
}

/// Drop every caller credential and send only the instance token.
pub(super) fn inject_instance_token(
    headers: &mut HeaderMap,
    token: &str,
) -> Result<(), StatusCode> {
    for h in CALLER_CREDENTIAL_HEADERS {
        headers.remove(h);
    }
    let mut value =
        HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| StatusCode::BAD_GATEWAY)?;
    value.set_sensitive(true);
    headers.insert(axum::http::header::AUTHORIZATION, value);
    Ok(())
}

async fn admit_route(source: Arc<dyn HiveTokenSource>) -> Result<ModelsAdmission, ModelsRefusal> {
    let fresh = FRESH_TOKEN.swap(false, Ordering::AcqRel);
    let call = tokio::task::spawn_blocking(move || source.admit_route(fresh));
    match tokio::time::timeout(ADMIT_TIMEOUT, call).await {
        Ok(Ok(answer)) => answer,
        Ok(Err(_)) | Err(_) => Err(ModelsRefusal::TokenUnavailable),
    }
}

/// Record what the gateway's answer says about the route.
async fn after_answer(
    source: Arc<dyn HiveTokenSource>,
    response: Response,
    probe: bool,
) -> Response {
    let status = response.status().as_u16();
    if status == 401 {
        FRESH_TOKEN.store(true, Ordering::Release);
        return response;
    }
    if response.status().is_success() {
        if probe {
            // Detached: clearing the hold must not delay the answer.
            drop(tokio::task::spawn_blocking(move || source.note_passed()));
        }
        return response;
    }
    if !matches!(status, 402 | 403 | 409 | 429) {
        return response;
    }
    let (parts, body) = response.into_parts();
    let bytes = axum::body::to_bytes(body, REFUSAL_BODY_LIMIT)
        .await
        .unwrap_or_default();
    let retry_after = parts
        .headers
        .get(axum::http::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let noted = bytes.clone();
    let _ = tokio::task::spawn_blocking(move || {
        source.note_answer(status, retry_after.as_deref(), &noted);
    })
    .await;
    Response::from_parts(parts, Body::from(bytes))
}

/// `/providers/leanctx/{rest}`.
pub(super) async fn handle(
    state: ProxyState,
    rest: &str,
    mut req: Request<Body>,
) -> Result<Response, StatusCode> {
    let Some(source) = hive_token_source() else {
        return Ok(refusal_response(&ModelsRefusal::Unavailable));
    };
    serve(state, rest, &mut req, source).await
}

async fn serve(
    state: ProxyState,
    rest: &str,
    req: &mut Request<Body>,
    source: Arc<dyn HiveTokenSource>,
) -> Result<Response, StatusCode> {
    let admission = match admit_route(source.clone()).await {
        Ok(admission) => admission,
        Err(refusal) => return Ok(refusal_response(&refusal)),
    };
    req.extensions_mut().insert(RegistryProviderId {
        id: PROVIDER_ID.to_string(),
        local: false,
        account_ref: None,
        region: Some("hive".to_string()),
        gateway_managed_credential: true,
    });
    let path = format!("/{rest}");
    let uri = match req.uri().query() {
        Some(q) => format!("{path}?{q}").parse::<axum::http::Uri>(),
        None => path.parse::<axum::http::Uri>(),
    }
    .map_err(|_| StatusCode::BAD_REQUEST)?;
    *req.uri_mut() = uri;
    inject_instance_token(req.headers_mut(), &admission.token)?;
    let request = std::mem::take(req);
    let origin = admission.origin.clone();
    let probe = admission.probe;
    drop(admission);
    let response = if super::providers::is_openai_responses_path(request.uri().path()) {
        forward::forward_request(
            axum::extract::State(state),
            request,
            &origin,
            "/v1/responses",
            super::openai_responses::compress_request_body,
            "OpenAI",
            &[],
        )
        .await
    } else {
        forward::forward_request(
            axum::extract::State(state),
            request,
            &origin,
            "/v1/chat/completions",
            super::openai::compress_request_body,
            "OpenAI",
            &[],
        )
        .await
    }?;
    Ok(after_answer(source, response, probe).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_instance_token_reaches_the_gateway() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer lean-ctx-token".parse().unwrap());
        headers.insert("x-api-key", "caller-key".parse().unwrap());
        headers.insert("api-key", "caller-azure-key".parse().unwrap());
        headers.insert("x-goog-api-key", "caller-goog-key".parse().unwrap());
        inject_instance_token(&mut headers, "hive_i_fixture").unwrap();
        assert_eq!(
            headers.get("authorization").unwrap().to_str().unwrap(),
            "Bearer hive_i_fixture"
        );
        assert_eq!(headers.len(), 1, "every caller credential is dropped");
        assert!(headers.get("authorization").unwrap().is_sensitive());
    }

    #[tokio::test]
    async fn without_the_right_the_route_answers_403_unavailable() {
        let response = refusal_response(&ModelsRefusal::Unavailable);
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["code"], "subscription_models_unavailable");
    }

    #[tokio::test]
    async fn a_held_route_answers_with_retry_after_and_no_network() {
        let response = refusal_response(&ModelsRefusal::Held {
            status: 429,
            code: "quota_exceeded".into(),
            retry_after_s: 120,
            line: "waiting for Hive until 2026-11-10T12:02:00Z: credit".into(),
        });
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers().get("retry-after").unwrap(), "120");
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["code"], "quota_exceeded");
    }
}
