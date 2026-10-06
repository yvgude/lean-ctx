// SPDX-License-Identifier: Apache-2.0
//! `/.well-known/mcp-server.json`: the MCP server card (#1913).

use axum::Json;
use axum::response::IntoResponse;
use serde_json::Value;

pub(super) async fn mcp_server_card() -> impl IntoResponse {
    Json(mcp_server_card_value())
}

/// Category tools pass the same public-surface filter as `tools/list`, so the
/// card cannot advertise hidden or compiled-out tools (#1913).
fn mcp_server_card_value() -> Value {
    let categories: Vec<Value> = [
        (
            "file_operations",
            &["ctx_read", "ctx_search", "ctx_tree", "ctx_edit"][..],
            150,
        ),
        (
            "session_management",
            &["ctx_session", "ctx_compress", "ctx_dedup", "ctx_preload"][..],
            80,
        ),
        (
            "intelligence",
            &[
                "ctx_knowledge",
                "ctx_semantic_search",
                "ctx_graph",
                "ctx_overview",
            ][..],
            200,
        ),
    ]
    .into_iter()
    .filter_map(|(name, tools, avg_token_cost)| {
        let tools = crate::core::a2a::agent_card::advertised_tools(tools);
        (!tools.is_empty()).then(
            || serde_json::json!({"name": name, "tools": tools, "avg_token_cost": avg_token_cost}),
        )
    })
    .collect();

    serde_json::json!({
        "name": "lean-ctx",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "Context Infrastructure Layer — compression, caching, governance for AI agents",
        "capabilities": {
            "tools": true,
            "resources": false,
            "prompts": false,
            "sampling": false
        },
        "tool_categories": categories,
        "features": {
            "compression": "deterministic AST-based, 40-70% token reduction",
            "caching": "session-scoped with zstd, unchanged full/auto re-reads ~13 tokens",
            "audit_trail": "SHA-256 chained JSONL",
            "rbac": "5 built-in roles with capability-based access",
            "sandboxing": "Level 0 (subprocess) + Level 1 (OS-level)",
            "secret_detection": "8 regex patterns + custom"
        },
        "security": {
            "path_jail": true,
            "rate_limiting": true,
            "budget_tracking": true,
            "timing_safe_auth": true
        }
    })
}

#[cfg(test)]
mod tests {
    use super::super::{HttpServerConfig, build_app_router_with_auth};
    use super::mcp_server_card_value;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    // #1913: the agent card's auth scheme matches what the router enforces,
    // and both discovery documents list only tools `tools/list` publishes.
    #[tokio::test]
    async fn discovery_documents_match_the_served_surface() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = HttpServerConfig {
            project_root: dir.path().to_path_buf(),
            auth_token: Some("secret".to_string()),
            ..HttpServerConfig::default()
        };

        for (require_auth, scheme) in [(true, "bearer"), (false, "none")] {
            let app = build_app_router_with_auth(&cfg, require_auth, None);
            let req = Request::builder()
                .method("GET")
                .uri("/.well-known/agent.json")
                .header("Host", "localhost")
                .header("Authorization", "Bearer secret")
                .body(Body::empty())
                .expect("request");
            let resp = app.oneshot(req).await.expect("resp");
            assert_eq!(resp.status(), StatusCode::OK);
            let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
                .await
                .expect("body");
            let card: Value = serde_json::from_slice(&body).expect("json");
            assert_eq!(card["authentication"]["schemes"], json!([scheme]));
        }

        let published: Vec<String> = crate::server::registry::build_registry()
            .tool_defs()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        let card = mcp_server_card_value();
        let categories = card["tool_categories"].as_array().expect("categories");
        assert!(!categories.is_empty());
        for category in categories {
            assert_ne!(category["name"], "agent_ops");
            for tool in category["tools"].as_array().expect("tools") {
                let name = tool.as_str().expect("name");
                assert!(
                    published.iter().any(|p| p == name),
                    "{name} is not published"
                );
            }
        }
        assert!(card["security"].get("signed_handoffs").is_none());
    }
}
