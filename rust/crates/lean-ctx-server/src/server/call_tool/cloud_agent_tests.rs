// SPDX-License-Identifier: Apache-2.0
//! ent2-2 at the enforcement point: the Cloud `agents.allowed` member refuses
//! every tool of a client whose agent is not listed, `ctx_call` included;
//! N1: `security.require_proxy` refuses a client that is not routed.

use lean_ctx::core::policy::cloud_floor;

fn install_agents_allowed(data: &std::path::Path, allowed: &[&str]) {
    let floor_dir = data.join("cloud").join("policy").join("effective");
    std::fs::create_dir_all(&floor_dir).unwrap();
    let floor = serde_json::json!({
        "schema": cloud_floor::FLOOR_SCHEMA, "version": 3,
        "digest": format!("sha256:{}", "a".repeat(64)),
        "not_after": "2099-01-01T00:00:00Z",
        "policy": {"effective": {
            "schema": "leanctx.cloud.policy/v1",
            "security": {}, "context": {}, "sharing": {},
            "agents": {"allowed": allowed}
        }, "vaults": []}
    });
    std::fs::write(floor_dir.join(cloud_floor::FLOOR_FILE), floor.to_string()).unwrap();
    std::fs::write(
        floor_dir.join(cloud_floor::REQUIRED_FILE),
        r#"{"version":3}"#,
    )
    .unwrap();
}

fn visible_text(result: &rmcp::model::CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| block.as_text())
        .map(|block| block.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_agent_gets_policy_blocked_through_ctx_call() {
    let data = lean_ctx::core::data_dir::isolated_data_dir();
    install_agents_allowed(data.path(), &["codex"]);
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("probe.txt"), "hello").unwrap();
    let server = crate::tools::LeanCtxServer::new_with_project_root(Some(
        root.path().to_str().expect("temporary root is UTF-8"),
    ));
    let request = || {
        let args = serde_json::Map::from_iter([
            ("name".to_owned(), serde_json::json!("ctx_read")),
            (
                "arguments".to_owned(),
                serde_json::json!({"path": "probe.txt", "mode": "full"}),
            ),
        ]);
        rmcp::model::CallToolRequestParams::new("ctx_call").with_arguments(args)
    };

    *server.client_name.write().await = "Cursor".to_owned();
    let refused = server
        .call_tool_guarded(request())
        .await
        .expect("a policy refusal is a tool result, not a transport error");
    let text = visible_text(&refused);
    assert_eq!(refused.is_error, Some(true), "{text}");
    assert!(
        text.starts_with("[POLICY BLOCKED] agent \"cursor\""),
        "{text}"
    );
    assert!(text.contains("agents.allowed"), "{text}");
    assert!(!text.contains("hello"), "{text}");

    *server.client_name.write().await = "codex-mcp-client".to_owned();
    let allowed = server
        .call_tool_guarded(request())
        .await
        .expect("an allowed agent's call succeeds");
    let text = visible_text(&allowed);
    assert!(!text.contains("[POLICY BLOCKED]"), "{text}");
}

/// N1 at the enforcement point: with `security.require_proxy` an agent whose
/// routing cannot be confirmed (no probe in this binary) is refused.
#[tokio::test(flavor = "multi_thread")]
async fn require_proxy_refuses_an_unconfirmed_agent() {
    let data = lean_ctx::core::data_dir::isolated_data_dir();
    let floor_dir = data.path().join("cloud").join("policy").join("effective");
    std::fs::create_dir_all(&floor_dir).unwrap();
    let floor = serde_json::json!({
        "schema": cloud_floor::FLOOR_SCHEMA, "version": 3,
        "digest": format!("sha256:{}", "a".repeat(64)),
        "not_after": "2099-01-01T00:00:00Z",
        "policy": {"effective": {
            "schema": "leanctx.cloud.policy/v1",
            "security": {"require_proxy": true}, "context": {}, "sharing": {}
        }, "vaults": []}
    });
    std::fs::write(floor_dir.join(cloud_floor::FLOOR_FILE), floor.to_string()).unwrap();
    std::fs::write(
        floor_dir.join(cloud_floor::REQUIRED_FILE),
        r#"{"version":3}"#,
    )
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("probe.txt"), "hello").unwrap();
    let server = crate::tools::LeanCtxServer::new_with_project_root(Some(
        root.path().to_str().expect("temporary root is UTF-8"),
    ));
    *server.client_name.write().await = "claude-code".to_owned();
    let args = serde_json::Map::from_iter([
        ("path".to_owned(), serde_json::json!("probe.txt")),
        ("mode".to_owned(), serde_json::json!("full")),
    ]);
    let refused = server
        .call_tool_guarded(rmcp::model::CallToolRequestParams::new("ctx_read").with_arguments(args))
        .await
        .expect("a policy refusal is a tool result, not a transport error");
    let text = visible_text(&refused);
    assert_eq!(refused.is_error, Some(true), "{text}");
    assert!(text.starts_with("[POLICY BLOCKED]"), "{text}");
    assert!(text.contains("security.require_proxy"), "{text}");
    assert!(text.contains("lean-ctx proxy enable"), "{text}");
    assert!(!text.contains("hello"), "{text}");
}
