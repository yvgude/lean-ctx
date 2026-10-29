// SPDX-License-Identifier: Apache-2.0
//! `GET /api/gateway/connect` — the Connect stage of the cockpit: which
//! sources are connected (each connector's declared capabilities, doc 17)
//! and which hosts reach the gateway at which coverage level (G7).
//!
//! Content: provider ids, availability, capability levels and the coverage
//! classification `lean-ctx doctor` prints. No credential, no URL, no item.

use super::helpers::detect_project_root_for_dashboard;

fn connect_document(
    connectors: &[crate::core::providers::registry::ProviderInfo],
    hosts: &[crate::core::host_coverage::HostCoverage],
) -> serde_json::Value {
    let connectors: Vec<serde_json::Value> = connectors
        .iter()
        .map(|info| {
            let mut entry = serde_json::to_value(info).unwrap_or_default();
            entry["shared_reuse"] = serde_json::Value::from(info.connector.allows_shared_reuse());
            entry
        })
        .collect();
    serde_json::json!({
        "connectors": connectors,
        "hosts": hosts,
        "capability_matrix": crate::core::providers::registry::capability_matrix_markdown(),
    })
}

pub(super) fn handle(
    path: &str,
    _query: &str,
    method: &str,
    _body: &str,
) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/gateway/connect" if method.eq_ignore_ascii_case("GET") => {
            let root = detect_project_root_for_dashboard();
            crate::core::providers::init::init_with_project_root(Some(std::path::Path::new(&root)));
            let connectors = crate::core::providers::registry::global_registry().discover();
            let hosts = dirs::home_dir()
                .map(|home| crate::doctor::configured_host_coverage(&home))
                .unwrap_or_default();
            Some((
                "200 OK",
                "application/json",
                connect_document(&connectors, &hosts).to_string(),
            ))
        }
        "/api/gateway/connect" => Some((
            "405 Method Not Allowed",
            "application/json",
            r#"{"error":"method not allowed"}"#.to_string(),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A configured connector is listed with its declared capabilities and
    /// whether shared reuse is allowed — and never with its credential.
    #[test]
    fn connectors_are_listed_with_capabilities_and_without_credentials() {
        let token = "canary-credential-0001";
        let provider = crate::core::providers::gitlab::GitLabProvider::with_config(
            crate::core::providers::config::GitLabConfig {
                host: "gitlab.example.com".into(),
                token: token.into(),
                project_path: Some("group/project".into()),
            },
        );
        let registry = crate::core::providers::registry::ProviderRegistry::new();
        registry.register(std::sync::Arc::new(provider));
        let hosts = [crate::core::host_coverage::classify("codex", None, false)];
        let document = connect_document(&registry.discover(), &hosts);
        let gitlab = &document["connectors"][0];
        assert_eq!(gitlab["id"], "gitlab");
        assert_eq!(gitlab["connector"]["revalidate_acl"], "supported");
        assert_eq!(gitlab["shared_reuse"], true);
        assert_eq!(document["hosts"][0]["host"], "codex");
        assert!(!document.to_string().contains(token), "{document}");
    }
}
