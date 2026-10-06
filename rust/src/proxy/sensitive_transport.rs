// SPDX-License-Identifier: Apache-2.0

//! Edge-local owner for provider transport authority.

use std::fmt;

use axum::http::request::Parts;

/// Provider request metadata that may contain authorization headers or cookies.
///
/// This type intentionally implements neither `Serialize` nor `Clone`. Only
/// Edge-local code can access the wrapped request parts, and the public Via wire
/// encoder accepts the closed `ViaWireMessageV1` enum instead.
///
/// Provider transport cannot satisfy a serialization bound:
///
/// ```compile_fail
/// use lean_ctx::proxy::sensitive_transport::SensitiveProviderTransport;
/// fn assert_serialize<T: serde::Serialize>() {}
/// assert_serialize::<SensitiveProviderTransport>();
/// ```
///
/// The existing provider cookie store cannot satisfy it either:
///
/// ```compile_fail
/// use lean_ctx::proxy::chatgpt_cookies::ChatGptCloudflareCookieStore;
/// fn assert_serialize<T: serde::Serialize>() {}
/// assert_serialize::<ChatGptCloudflareCookieStore>();
/// ```
///
/// Raw proxy bridge data containing headers is also non-serializable:
///
/// ```compile_fail
/// use lean_ctx::core::context_kernel::proxy_bridge::ProxyRequestData;
/// fn assert_serialize<T: serde::Serialize>() {}
/// assert_serialize::<ProxyRequestData>();
/// ```
///
/// Provider authority cannot become a Via wire message:
///
/// ```compile_fail
/// use lean_ctx::proxy::sensitive_transport::SensitiveProviderTransport;
/// use lean_ctx_protocol::edge_via::ViaWireMessageV1;
/// fn assert_into_via<T: Into<ViaWireMessageV1>>() {}
/// assert_into_via::<SensitiveProviderTransport>();
/// ```
///
/// Raw HTTP headers and arbitrary JSON are also excluded from the closed
/// encoder input:
///
/// ```compile_fail
/// use axum::http::HeaderMap;
/// use lean_ctx_protocol::edge_via::ViaWireMessageV1;
/// fn assert_into_via<T: Into<ViaWireMessageV1>>() {}
/// assert_into_via::<HeaderMap>();
/// ```
///
/// ```compile_fail
/// use lean_ctx_protocol::edge_via::ViaWireMessageV1;
/// fn assert_into_via<T: Into<ViaWireMessageV1>>() {}
/// assert_into_via::<serde_json::Value>();
/// ```
pub struct SensitiveProviderTransport {
    parts: Parts,
}

impl SensitiveProviderTransport {
    pub(crate) fn new(parts: Parts) -> Self {
        Self { parts }
    }

    pub(crate) fn parts(&self) -> &Parts {
        &self.parts
    }

    pub(crate) fn parts_mut(&mut self) -> &mut Parts {
        &mut self.parts
    }
}

impl fmt::Debug for SensitiveProviderTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SensitiveProviderTransport")
            .field("method", &self.parts.method)
            .field("uri", &"[REDACTED]")
            .field("headers", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{Request, header};

    use super::*;

    #[test]
    fn debug_redacts_provider_authority_while_edge_retains_it() {
        const AUTH_CANARY: &str = "Bearer via-provider-auth-canary";
        const COOKIE_CANARY: &str = "cf_clearance=via-provider-cookie-canary";
        const URI_CANARY: &str = "via-provider-uri-key-canary";
        let request = Request::builder()
            .uri(format!(
                "https://api.example.test/v1/messages?key={URI_CANARY}"
            ))
            .header(header::AUTHORIZATION, AUTH_CANARY)
            .header(header::COOKIE, COOKIE_CANARY)
            .body(())
            .unwrap();
        let (parts, ()) = request.into_parts();
        let transport = SensitiveProviderTransport::new(parts);

        let debug = format!("{transport:?}");
        assert!(!debug.contains(AUTH_CANARY));
        assert!(!debug.contains(COOKIE_CANARY));
        assert!(!debug.contains(URI_CANARY));
        assert!(debug.contains("[REDACTED]"));
        assert_eq!(
            transport.parts().headers[header::AUTHORIZATION],
            AUTH_CANARY
        );
        assert_eq!(transport.parts().headers[header::COOKIE], COOKIE_CANARY);
    }
}
