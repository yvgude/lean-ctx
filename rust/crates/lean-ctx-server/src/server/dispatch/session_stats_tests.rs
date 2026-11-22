// SPDX-License-Identifier: Apache-2.0
//! rel12 WP2: `session.stats` `tools.compose` counts every `ctx_compose`
//! call of the session, the direct ones and those routed through
//! `ctx_call`, and no other tool. `knowledge.written` counts a `remember`
//! routed through `ctx_call` once.

use serde_json::{Map, Value, json};

use crate::tools::LeanCtxServer;

const MODULE: &str = "server::dispatch::session_stats_tests::";

/// The session's persisted counter `field` as the stats JSON holds it.
async fn counted(server: &LeanCtxServer, field: &str) -> Value {
    let stats = serde_json::to_value(&server.session.read().await.stats).unwrap();
    stats[field].clone()
}

/// `ctx_call` enforces the process-global role policy, which sibling tests
/// switch; each test here runs under a fixed role in a process of its own.
/// Returns true in the parent, after the child passed; the caller returns.
fn ran_in_subprocess(test: &str) -> bool {
    if std::env::var_os("LEAN_CTX_SESSION_STATS_SUBPROCESS").is_some() {
        return false;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &format!("{MODULE}{test}"), "--nocapture"])
        .env("LEAN_CTX_SESSION_STATS_SUBPROCESS", "1")
        .env("LEAN_CTX_ROLE", "coder")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

/// `ctx_call` arguments routing `tool` with `arguments`.
fn routed(tool: &str, arguments: Value) -> Map<String, Value> {
    let mut routed = Map::new();
    routed.insert("name".into(), Value::String(tool.into()));
    routed.insert("arguments".into(), arguments);
    routed
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compose_calls_count_direct_and_through_ctx_call() {
    if ran_in_subprocess("compose_calls_count_direct_and_through_ctx_call") {
        return;
    }
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    std::fs::write(root.join("quasar.rs"), "fn quasar() {}\n").unwrap();
    let server = LeanCtxServer::new_with_project_root(root.to_str());
    let compose = json!({"task": "quasar", "task_aware": false});

    server
        .dispatch_tool("ctx_compose", compose.as_object(), false)
        .await
        .expect("direct ctx_compose");
    assert_eq!(
        counted(&server, "compose_calls").await,
        json!(1),
        "a direct ctx_compose counts"
    );

    server
        .dispatch_tool("ctx_call", Some(&routed("ctx_compose", compose)), false)
        .await
        .expect("ctx_compose through ctx_call");
    assert_eq!(
        counted(&server, "compose_calls").await,
        json!(2),
        "a ctx_compose routed through ctx_call counts"
    );

    let mut read = Map::new();
    read.insert("path".into(), Value::String("quasar.rs".into()));
    let _ = server.dispatch_tool("ctx_read", Some(&read), false).await;
    assert_eq!(
        counted(&server, "compose_calls").await,
        json!(2),
        "another tool is not a compose call"
    );

    // `tools.compose` is a subset of `tokens.tool_calls`: every counted
    // compose call is also one of the session's tool calls.
    let stats = serde_json::to_value(&server.session.read().await.stats).unwrap();
    let compose = stats["compose_calls"].as_u64().unwrap_or_default();
    let tool_calls = stats["total_tool_calls"].as_u64().unwrap_or_default();
    assert!(
        compose <= tool_calls && tool_calls >= 3,
        "compose {compose} must lie within tool_calls {tool_calls}: {stats}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_remember_through_ctx_call_counts_once() {
    if ran_in_subprocess("a_remember_through_ctx_call_counts_once") {
        return;
    }
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let server = LeanCtxServer::new_with_project_root(root.to_str());
    let remember = json!({"action": "remember", "category": "architecture",
                          "key": "retry", "value": "payments retry three times"});

    server
        .dispatch_tool("ctx_call", Some(&routed("ctx_knowledge", remember)), false)
        .await
        .expect("remember through ctx_call");
    assert_eq!(
        counted(&server, "knowledge_written").await,
        json!(1),
        "a remember routed through ctx_call counts exactly once"
    );
}
