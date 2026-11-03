// SPDX-License-Identifier: Apache-2.0
//! Real loopback HTTP requests: redirects must never receive provider authority.
pub(crate) use super::redirect_support::{CANARY, Server};
use super::{HardenedClient, HttpOutcome};
use crate::core::providers::config_provider::{http, schema::ResourceConfig};
use std::collections::HashMap;

#[test]
fn builtin_provider_redirects_never_forward_headers_or_bodies() {
    let target = Server::new(200, None, "[]");
    for status in [301, 302, 303, 307, 308] {
        let origin = Server::new(status, Some(&target.url), CANARY);
        let client = HardenedClient::new("gitlab");
        for outcome in [
            client.get_with_headers(&origin.url, &[("PRIVATE-TOKEN", CANARY)]),
            client.post_with_headers(&origin.url, CANARY, &[("X-Api-Key", CANARY)]),
        ] {
            assert!(
                matches!(&outcome, HttpOutcome::HttpError { status: actual, body, .. } if *actual == status && body.is_empty())
            );
            assert!(!outcome.into_body().unwrap_err().contains(CANARY));
        }
        assert_eq!(origin.count(), 2);
        assert!(origin.received_canary());
        assert!(origin.received_body(CANARY));
        assert_eq!(
            target.count(),
            0,
            "redirect authority reached another origin"
        );
    }
}

#[test]
fn config_provider_redirects_never_forward_any_supported_method() {
    let target = Server::new(200, None, "[]");
    let auth = http::ResolvedAuth::CustomHeader {
        header: "PRIVATE-TOKEN".into(),
        value: CANARY.into(),
    };
    for status in [301, 302, 303, 307, 308] {
        let origin = Server::new(status, Some(&target.url), CANARY);
        for method in ["GET", "POST"] {
            let resource: ResourceConfig = serde_json::from_value(serde_json::json!({
                "method": method, "path": "/issues", "response": {"mapping": {"id":"id", "title":"title"}}
            })).unwrap();
            let error =
                http::execute_request(&origin.url, &resource, &auth, &HashMap::new()).unwrap_err();
            assert!(error.contains(&status.to_string()), "{method}: {error}");
            assert!(!error.contains(CANARY));
        }
        assert_eq!(origin.count(), 2);
        assert!(origin.received_canary());
        assert_eq!(target.count(), 0);
    }
}

#[test]
fn direct_config_provider_requests_still_authenticate_and_return_json() {
    let origin = Server::new(200, None, "[{\"id\":1,\"title\":\"Login bug\"}]");
    let auth = http::ResolvedAuth::ApiKeyQuery {
        param: "key".into(),
        value: CANARY.into(),
    };
    for method in ["GET", "POST"] {
        let resource: ResourceConfig = serde_json::from_value(serde_json::json!({
            "method": method, "path": "/issues", "response": {"mapping": {"id":"id", "title":"title"}}
        })).unwrap();
        let value = http::execute_request(&origin.url, &resource, &auth, &HashMap::new()).unwrap();
        assert_eq!(value[0]["id"], 1);
    }
    // Connectors are read-only: a write method never reaches the source.
    for method in ["DELETE", "PUT", "PATCH"] {
        let resource: ResourceConfig = serde_json::from_value(serde_json::json!({
            "method": method, "path": "/issues", "response": {"mapping": {"id":"id", "title":"title"}}
        })).unwrap();
        let error =
            http::execute_request(&origin.url, &resource, &auth, &HashMap::new()).unwrap_err();
        assert!(error.contains("read-only"), "{method}: {error}");
    }
    assert_eq!(origin.count(), 2);
    assert!(origin.received_canary());
}
