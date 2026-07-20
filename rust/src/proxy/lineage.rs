//! Trusted, payload-free OCLA lineage admitted at the HTTP proxy boundary.

use axum::http::{HeaderMap, request::Parts};

use crate::core::ocla::OclaRequestContext;

pub(super) const REQUEST_ID_HEADER: &str = "x-leanctx-request-id";
pub(super) const SESSION_ID_HEADER: &str = "x-leanctx-session-id";
pub(super) const AGENT_ID_HEADER: &str = "x-leanctx-agent-id";

const MAX_LINEAGE_ID_BYTES: usize = 256;

/// OCLA proxy `content_ref` v1 domain: only the exact, bounded raw HTTP body
/// bytes received by the proxy. Headers, decoded/normalized JSON, routing and
/// compressed/transformed output bytes are deliberately outside the digest.
/// The stable wire representation is `blake3:<64 lowercase hex characters>`.
fn content_ref_v1(exact_bounded_raw_body: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(exact_bounded_raw_body).to_hex())
}

/// Projects trusted request headers into the canonical OCLA context.
///
/// The auth middleware establishes trust before provider handlers run. Legacy,
/// partial, duplicated or malformed header sets remain unmanaged; the proxy
/// never invents lineage and never rejects otherwise valid provider traffic.
pub(super) fn from_trusted_headers(
    headers: &HeaderMap,
    exact_bounded_body: &[u8],
) -> Option<OclaRequestContext> {
    let request_id = exact_id(headers, REQUEST_ID_HEADER)?;
    let session_id = exact_id(headers, SESSION_ID_HEADER)?;
    let agent_id = exact_id(headers, AGENT_ID_HEADER)?;

    Some(OclaRequestContext {
        request_id,
        session_id,
        agent_id,
        content_ref: content_ref_v1(exact_bounded_body),
        tenant_id: None,
    })
}

/// Admits caller-supplied lineage only after the proxy auth guard has marked
/// the request as gateway-trusted. Provider-key fallback remains unmanaged;
/// its upstream credential authenticates the provider, not OCLA metadata.
pub(super) fn from_trusted_request(
    parts: &Parts,
    exact_bounded_body: &[u8],
) -> Option<OclaRequestContext> {
    parts
        .extensions
        .get::<super::gateway_identity::TrustedGatewayRequest>()
        .and_then(|_| from_trusted_headers(&parts.headers, exact_bounded_body))
}

fn exact_id(headers: &HeaderMap, name: &'static str) -> Option<String> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    let value = value.to_str().ok()?;
    if value.is_empty()
        || value.len() > MAX_LINEAGE_ID_BYTES
        || value.trim() != value
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return None;
    }
    Some(value.to_owned())
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, Request};

    use super::*;

    fn complete() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(REQUEST_ID_HEADER, HeaderValue::from_static("req-1"));
        headers.insert(SESSION_ID_HEADER, HeaderValue::from_static("session:1"));
        headers.insert(AGENT_ID_HEADER, HeaderValue::from_static("agent@example"));
        headers
    }

    #[test]
    fn complete_headers_hash_exact_body_without_payload() {
        let body = br#"{"model":"gpt-5","input":"secret"}"#;
        let context = from_trusted_headers(&complete(), body).expect("managed lineage");
        assert_eq!(context.request_id, "req-1");
        assert_eq!(context.session_id, "session:1");
        assert_eq!(context.agent_id, "agent@example");
        assert_eq!(
            context.content_ref,
            format!("blake3:{}", blake3::hash(body).to_hex())
        );
        assert!(!context.content_ref.contains("secret"));
        assert_eq!(context.tenant_id, None);
    }

    #[test]
    fn missing_or_partial_headers_are_unmanaged() {
        assert!(from_trusted_headers(&HeaderMap::new(), b"body").is_none());
        let mut headers = complete();
        headers.remove(AGENT_ID_HEADER);
        assert!(from_trusted_headers(&headers, b"body").is_none());
    }

    #[test]
    fn duplicate_header_is_unmanaged() {
        let mut headers = complete();
        headers.append(REQUEST_ID_HEADER, HeaderValue::from_static("req-2"));
        assert!(from_trusted_headers(&headers, b"body").is_none());
    }

    #[test]
    fn malformed_headers_are_unmanaged() {
        for value in ["", " req", "req ", "contains space"] {
            let mut headers = complete();
            headers.insert(REQUEST_ID_HEADER, HeaderValue::from_str(value).unwrap());
            assert!(
                from_trusted_headers(&headers, b"body").is_none(),
                "{value:?}"
            );
        }

        let mut oversized = complete();
        oversized.insert(
            REQUEST_ID_HEADER,
            HeaderValue::from_str(&"a".repeat(MAX_LINEAGE_ID_BYTES + 1)).unwrap(),
        );
        assert!(from_trusted_headers(&oversized, b"body").is_none());

        let mut opaque = complete();
        opaque.insert(
            REQUEST_ID_HEADER,
            HeaderValue::from_bytes(&[0x80]).expect("opaque header value"),
        );
        assert!(from_trusted_headers(&opaque, b"body").is_none());

        let mut control = complete();
        control.insert(
            REQUEST_ID_HEADER,
            HeaderValue::from_bytes(b"req\tid").expect("HTTP permits horizontal tab"),
        );
        assert!(from_trusted_headers(&control, b"body").is_none());

        let mut boundary = complete();
        boundary.insert(
            REQUEST_ID_HEADER,
            HeaderValue::from_str(&"a".repeat(MAX_LINEAGE_ID_BYTES)).unwrap(),
        );
        assert!(from_trusted_headers(&boundary, b"body").is_some());
    }

    #[test]
    fn digest_domain_is_exact_pre_transform_bytes() {
        let raw = br#"{ "input": "same meaning" }"#;
        let normalized = br#"{"input":"same meaning"}"#;
        let raw_ref = from_trusted_headers(&complete(), raw).unwrap().content_ref;
        let normalized_ref = from_trusted_headers(&complete(), normalized)
            .unwrap()
            .content_ref;
        assert_ne!(raw_ref, normalized_ref);
    }

    #[test]
    fn content_ref_v1_empty_body_regression_vector() {
        assert_eq!(
            content_ref_v1(b""),
            "blake3:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
    }

    #[test]
    fn lineage_headers_require_gateway_auth_marker() {
        let request = Request::builder()
            .header(REQUEST_ID_HEADER, "req-1")
            .header(SESSION_ID_HEADER, "session-1")
            .header(AGENT_ID_HEADER, "agent-1")
            .body(())
            .unwrap();
        let (mut parts, ()) = request.into_parts();

        // Provider-key fallback can carry these headers, but must remain
        // unmanaged because it has no gateway trust marker.
        assert!(from_trusted_request(&parts, b"raw-body").is_none());

        parts
            .extensions
            .insert(super::super::gateway_identity::TrustedGatewayRequest);
        let context = from_trusted_request(&parts, b"raw-body").expect("managed lineage");
        assert_eq!(context.request_id, "req-1");
        assert_eq!(context.session_id, "session-1");
        assert_eq!(context.agent_id, "agent-1");
        assert_eq!(context.tenant_id, None);
        assert_eq!(
            context.content_ref,
            format!("blake3:{}", blake3::hash(b"raw-body").to_hex())
        );
    }
}
