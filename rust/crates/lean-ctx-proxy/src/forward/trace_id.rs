// SPDX-License-Identifier: Apache-2.0
use axum::http::{HeaderMap, HeaderValue};
use axum::response::Response;

use lean_ctx::core::ocla::types::OclaRequestContext;

const TRACE_ID_HEADER: &str = "x-trace-id";

pub fn extract_or_generate_trace_id(headers: &HeaderMap) -> String {
    headers
        .get(TRACE_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .map_or_else(
            || {
                OclaRequestContext::new(
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    None,
                    None,
                )
                .trace_id
            },
            str::to_owned,
        )
}

pub fn inject_trace_id(response: &mut Response, trace_id: &str) {
    response.headers_mut().insert(
        TRACE_ID_HEADER,
        HeaderValue::from_str(trace_id).expect("generated trace ID is a valid header"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provided_trace_id_is_preserved() {
        let mut headers = HeaderMap::new();
        headers.insert(TRACE_ID_HEADER, HeaderValue::from_static("tr-provided"));
        assert_eq!(extract_or_generate_trace_id(&headers), "tr-provided");
    }

    #[test]
    fn missing_trace_id_is_generated() {
        assert!(extract_or_generate_trace_id(&HeaderMap::new()).starts_with("tr-"));
    }

    #[test]
    fn trace_id_is_injected_into_response() {
        let mut response = Response::new(axum::body::Body::empty());
        inject_trace_id(&mut response, "tr-test");
        assert_eq!(response.headers()[TRACE_ID_HEADER], "tr-test");
    }
}
