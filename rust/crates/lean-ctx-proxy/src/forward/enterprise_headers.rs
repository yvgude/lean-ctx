// SPDX-License-Identifier: Apache-2.0
//! Inject `x-leanctx-*` metadata headers when the Enterprise Suite is configured.
//!
//! Called in the forward path between compression and `send_upstream`, so the
//! Suite Gateway receives context-engineering metadata alongside the (already
//! optimized) request body.

use axum::http::request::Parts;
use reqwest::header::HeaderValue;

use lean_ctx::core::config::EnterpriseConfig;

/// Metadata computed by the Runtime for this request, injected as headers.
pub(crate) struct RuntimeMetadata {
    pub original_tokens: usize,
    pub compressed_tokens: usize,
    pub agent_id: Option<String>,
    pub session_id: Option<String>,
    pub task_class: Option<String>,
}

/// Injects enterprise metadata headers into the request parts.
///
/// Headers injected:
/// - `x-leanctx-instance`: Runtime instance identifier
/// - `x-leanctx-context-saved`: tokens removed by context engineering
/// - `x-leanctx-compression-ratio`: ratio of compressed to original (0.0-1.0)
/// - `x-leanctx-agent`: agent identifier (from lineage or config)
/// - `x-leanctx-session`: session identifier
/// - `x-leanctx-task-class`: classified task type
///
/// The Suite Gateway reads these in `inject_leanctx_headers()` and persists
/// them to the economics ledger.
pub(crate) fn inject(parts: &mut Parts, enterprise: &EnterpriseConfig, meta: &RuntimeMetadata) {
    if !enterprise.should_inject_headers() {
        return;
    }

    let instance_id = enterprise.effective_instance_id();
    if let Ok(val) = HeaderValue::from_str(&instance_id) {
        parts.headers.insert("x-leanctx-instance", val);
    }

    let saved = meta.original_tokens.saturating_sub(meta.compressed_tokens);
    if let Ok(val) = HeaderValue::from_str(&saved.to_string()) {
        parts.headers.insert("x-leanctx-context-saved", val);
    }

    if meta.original_tokens > 0 {
        let ratio = meta.compressed_tokens as f64 / meta.original_tokens as f64;
        let clamped = ratio.clamp(0.0, 1.0);
        if let Ok(val) = HeaderValue::from_str(&format!("{clamped:.4}")) {
            parts.headers.insert("x-leanctx-compression-ratio", val);
        }
    }

    if let Some(agent) = &meta.agent_id {
        if let Ok(val) = HeaderValue::from_str(agent) {
            parts.headers.insert("x-leanctx-agent", val);
        }
    }

    if let Some(session) = &meta.session_id {
        if let Ok(val) = HeaderValue::from_str(session) {
            parts.headers.insert("x-leanctx-session", val);
        }
    }

    if let Some(task_class) = &meta.task_class {
        if let Ok(val) = HeaderValue::from_str(task_class) {
            parts.headers.insert("x-leanctx-task-class", val);
        }
    }

    // Inject enterprise auth token so the Suite authenticates this Runtime.
    if let Some(token) = enterprise.effective_token() {
        if let Ok(val) = HeaderValue::from_str(&format!("Bearer {token}")) {
            parts.headers.insert("x-leanctx-authorization", val);
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;

    use super::*;

    fn make_parts() -> axum::http::request::Parts {
        let (parts, _) = Request::builder()
            .uri("http://test/v1/chat/completions")
            .body(Body::empty())
            .unwrap()
            .into_parts();
        parts
    }

    #[test]
    fn injects_headers_when_active() {
        let cfg = EnterpriseConfig {
            gateway_url: Some("https://api.leanctx.com".to_owned()),
            instance_id: Some("test-runtime".to_owned()),
            instance_token: Some("tok_test".to_owned()),
            ..Default::default()
        };
        let meta = RuntimeMetadata {
            original_tokens: 10000,
            compressed_tokens: 3000,
            agent_id: Some("cursor-42".to_owned()),
            session_id: Some("sess-abc".to_owned()),
            task_class: Some("bugfix".to_owned()),
        };
        let mut parts = make_parts();
        inject(&mut parts, &cfg, &meta);

        assert_eq!(
            parts.headers.get("x-leanctx-instance").unwrap(),
            "test-runtime"
        );
        assert_eq!(
            parts.headers.get("x-leanctx-context-saved").unwrap(),
            "7000"
        );
        assert_eq!(
            parts.headers.get("x-leanctx-compression-ratio").unwrap(),
            "0.3000"
        );
        assert_eq!(parts.headers.get("x-leanctx-agent").unwrap(), "cursor-42");
        assert_eq!(parts.headers.get("x-leanctx-session").unwrap(), "sess-abc");
        assert_eq!(parts.headers.get("x-leanctx-task-class").unwrap(), "bugfix");
        assert_eq!(
            parts.headers.get("x-leanctx-authorization").unwrap(),
            "Bearer tok_test"
        );
    }

    #[test]
    fn skips_when_disabled() {
        let cfg = EnterpriseConfig {
            gateway_url: Some("https://api.leanctx.com".to_owned()),
            disabled: true,
            ..Default::default()
        };
        let meta = RuntimeMetadata {
            original_tokens: 10000,
            compressed_tokens: 3000,
            agent_id: None,
            session_id: None,
            task_class: None,
        };
        let mut parts = make_parts();
        inject(&mut parts, &cfg, &meta);

        assert!(parts.headers.get("x-leanctx-instance").is_none());
    }
}
