// SPDX-License-Identifier: Apache-2.0

//! Real HTTP cancellation across an executor-process boundary.
use crate::core::work_graph::{BoundedWorkGraph, WorkNodeBudget};
use crate::core::work_graph_store::{ClaimNodeExecution, WorkGraphStore};
use serde_json::{Value, json};
use std::process::{Child, Command};
use std::time::Duration;

struct Executor(Child);
impl Drop for Executor {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn executor_process() {
    let Ok(project) = std::env::var("LEANCTX_CONTROL_TEST_PROJECT") else {
        return;
    };
    let fence = std::env::var("LEANCTX_CONTROL_TEST_FENCE").unwrap();
    let watch = crate::core::agent_connector::timeout::durable::Guard::install(
        &project, "graph", "child", &fence,
    )
    .unwrap();
    let key = crate::core::work_graph_executor::execution_key(
        std::path::Path::new(&project),
        "graph",
        "child",
        &fence,
    )
    .unwrap();
    let mut command = Command::new("sh");
    command.args([
        "-c",
        "printf ready > \"$1/ready\"; sleep 20",
        "control-test",
        &project,
    ]);
    let result = crate::core::agent_connector::timeout::run_with_timeout_cancellable(
        &mut command,
        10_000,
        Some(&key),
    )
    .unwrap();
    assert!(result.cancelled);
    assert!(!result.timed_out);
    assert!(watch.captures().all_reaped());
    WorkGraphStore::mutate(&project, |store| {
        store
            .graph_mut("graph")
            .unwrap()
            .acknowledge_execution_exit("child", &fence)
            .map_err(|error| error.to_string())
    })
    .unwrap();
}

async fn request(body: Value) -> Value {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        super::handle_request(
            stream,
            Some(std::sync::Arc::new("lifecycle-test".into())),
            std::sync::Arc::new(String::new()),
            std::sync::Arc::new(super::allowed_loopback()),
        )
        .await;
    });
    let response = tokio::time::timeout(Duration::from_secs(5), async {
        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        let body = body.to_string();
        let auth = ["Bearer", "lifecycle-test"].join(" ");
        let bytes = format!(
            "POST /api/agents/work-graph HTTP/1.1\r\nHost: localhost\r\nAuthorization: {auth}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        client.write_all(bytes.as_bytes()).await.unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).await.unwrap();
        response
    }).await;
    if response.is_err() {
        server.abort();
    }
    let response = response.expect("bounded HTTP exchange");
    server.await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap()
}

#[tokio::test]
async fn http_stop_reaches_executor_and_persists_exit_acknowledgement() {
    let _isolation = crate::core::data_dir::isolated_data_dir();
    let project_dir = tempfile::tempdir().unwrap();
    let project = project_dir.path().to_str().unwrap();
    let mut registry = crate::core::agents::AgentRegistry::load_or_create();
    registry
        .register("codex", Some("control-test"), project, None)
        .unwrap();
    registry.save().unwrap();
    let budget = WorkNodeBudget {
        tokens_allocated: 100,
        tokens_consumed: 0,
        cost_micros_allocated: 100,
        cost_micros_consumed: 0,
    };
    let mut graph = BoundedWorkGraph::default();
    graph
        .add_root(
            "root".into(),
            "lead".into(),
            "capsule:root".into(),
            budget.clone(),
        )
        .unwrap();
    graph
        .queue_child(
            "root",
            "child".into(),
            "worker".into(),
            "capsule:child".into(),
            budget,
        )
        .unwrap();
    let fence = WorkGraphStore::mutate(project, |store| {
        store.create("graph", graph)?;
        let ClaimNodeExecution::Claimed(node) =
            store.claim_node_execution("graph", "child", 1, u64::MAX)?
        else {
            return Err("expected execution claim".into());
        };
        Ok(node.execution_fence.unwrap())
    })
    .unwrap();
    let mut executor = Executor(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "dashboard::tests::control_lifecycle::executor_process",
                "--nocapture",
            ])
            .env("LEANCTX_CONTROL_TEST_PROJECT", project)
            .env("LEANCTX_CONTROL_TEST_FENCE", &fence)
            .spawn()
            .unwrap(),
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while !project_dir.path().join("ready").exists() {
            assert!(
                executor.0.try_wait().unwrap().is_none(),
                "executor exited before readiness"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("executor readiness");
    let observed =
        request(json!({"action":"observe", "project_root":project, "graph_id":"graph"})).await;
    let cancelled = request(json!({"action":"cancel", "project_root":project, "graph_id":"graph", "node_id":"child", "expected_revision":observed["write_revision"]})).await;
    assert_eq!(cancelled["cancellation_requested"], true);
    let child = |value: &Value| {
        value["graph"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["node_id"] == "child")
            .unwrap()
            .clone()
    };
    assert_eq!(child(&cancelled)["process_exit"], "unconfirmed");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(status) = executor.0.try_wait().unwrap() {
                assert!(status.success(), "executor failed: {status}");
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("bounded executor shutdown");
    let final_state =
        request(json!({"action":"observe", "project_root":project, "graph_id":"graph"})).await;
    assert_eq!(child(&final_state)["status"], "stopped");
    assert_eq!(child(&final_state)["process_exit"], "confirmed");
    assert!(!final_state.to_string().contains(&fence));
}
