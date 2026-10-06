// SPDX-License-Identifier: Apache-2.0

use super::{AppState, RateLimiter, v1_tool_call};
use axum::{Router, body::Body, http::Request, routing::post};
use rmcp::model::ErrorData;
use serde_json::{Map, Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration as StdDuration;
use tokio::sync::Notify;
use tower::ServiceExt;

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput};
use crate::tools::LeanCtxServer;

#[derive(Clone)]
struct Probe {
    gate: Arc<(Mutex<bool>, Condvar)>,
    started: Arc<AtomicBool>,
    completed: Arc<AtomicBool>,
    read_succeeded: Arc<AtomicBool>,
    started_notify: Arc<Notify>,
    completed_notify: Arc<Notify>,
}

impl Probe {
    fn new() -> Self {
        Self {
            gate: Arc::new((Mutex::new(false), Condvar::new())),
            started: Arc::new(AtomicBool::new(false)),
            completed: Arc::new(AtomicBool::new(false)),
            read_succeeded: Arc::new(AtomicBool::new(false)),
            started_notify: Arc::new(Notify::new()),
            completed_notify: Arc::new(Notify::new()),
        }
    }

    fn release(&self) {
        let (released, wake) = &*self.gate;
        *released.lock().expect("probe gate") = true;
        wake.notify_all();
    }
}

struct ReleaseOnDrop(Probe);

impl ReleaseOnDrop {
    fn new(probe: Probe) -> Self {
        Self(probe)
    }

    fn release(&self) {
        self.0.release();
    }
}

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// Wrap the real blocking `ctx_read` tool so the MCP request can be observed
/// across the outer REST deadline without replacing the MCP lifecycle.
struct DelayedRead {
    probe: Probe,
}

impl McpTool for DelayedRead {
    fn name(&self) -> &'static str {
        "ctx_read"
    }

    fn tool_def(&self) -> rmcp::model::Tool {
        crate::tools::registered::ctx_read::CtxReadTool.tool_def()
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        self.probe.started.store(true, Ordering::Release);
        self.probe.started_notify.notify_one();

        let (released, wake) = &*self.probe.gate;
        let mut released = released.lock().expect("probe gate");
        let deadline = std::time::Instant::now() + StdDuration::from_secs(15);
        while !*released {
            let (next, result) = wake
                .wait_timeout(
                    released,
                    deadline.saturating_duration_since(std::time::Instant::now()),
                )
                .expect("probe gate wait");
            released = next;
            if result.timed_out() && !*released {
                return Err(ErrorData::internal_error(
                    "cancellation regression gate timed out",
                    None,
                ));
            }
        }
        drop(released);

        let result = crate::tools::registered::ctx_read::CtxReadTool.handle(args, ctx);
        self.probe.read_succeeded.store(
            result
                .as_ref()
                .is_ok_and(|output| output.text.contains("after_timeout")),
            Ordering::Release,
        );
        self.probe.completed.store(true, Ordering::Release);
        self.probe.completed_notify.notify_one();
        result
    }
}

async fn wait_for(flag: &AtomicBool, notify: &Notify, label: &str) {
    let deadline = tokio::time::Instant::now() + StdDuration::from_secs(10);
    while !flag.load(Ordering::Acquire) {
        let notified = notify.notified();
        if flag.load(Ordering::Acquire) {
            break;
        }
        tokio::time::timeout_at(deadline, notified)
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {label}"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outer_rest_timeout_preserves_mcp_execution() {
    // Keep process-global role/data state isolated, matching the existing MCP
    // lifecycle regression harness in server/call_tool/guarded/task_lineage_tests.rs.
    if std::env::var_os("LEAN_CTX_MCP_CANCELLATION_SUBPROCESS").is_none() {
        let environment = tempfile::tempdir().unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "http_server::cancellation_tests::outer_rest_timeout_preserves_mcp_execution",
                "--nocapture",
            ])
            .env("LEAN_CTX_MCP_CANCELLATION_SUBPROCESS", "1")
            .env("LEAN_CTX_ROLE", "coder")
            .env_remove("LEAN_CTX_RECEIPT_HOST_CONFIG")
            .env_remove("LEAN_CTX_PROJECT_ROOT")
            .env_remove("CLAUDE_PROJECT_DIR")
            .env_remove("WORKSPACE_FOLDER_PATHS");
        for name in [
            "LEAN_CTX_DATA_DIR",
            "LEAN_CTX_CONFIG_DIR",
            "LEAN_CTX_STATE_DIR",
            "LEAN_CTX_CACHE_DIR",
        ] {
            command.env(name, environment.path());
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let _isolated_data_dir = crate::core::data_dir::isolated_data_dir();
    let directory = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(directory.path()).expect("canonical root");
    let root_str = root.to_string_lossy().to_string();
    let path = root.join("after-timeout.rs");
    std::fs::write(&path, "pub fn after_timeout() {}\n").expect("fixture file");

    let probe = Probe::new();
    let _release_on_panic = ReleaseOnDrop::new(probe.clone());
    let mut server = LeanCtxServer::new_shared_with_context(&root_str, "default", "default");
    let mut registry = crate::server::registry::build_registry();
    registry.register(Box::new(DelayedRead {
        probe: probe.clone(),
    }));
    let registry = Arc::new(registry);
    let service_lifetime = Arc::downgrade(&registry);
    server.registry = Some(registry);
    // Do not include cold tokenizer initialization in the response deadline.
    let _ = crate::core::tokens::count_tokens("warm cancellation regression");

    let app = Router::new()
        .route("/v1/tools/call", post(v1_tool_call))
        .with_state(AppState {
            token: None,
            a2a_signing_key: None,
            a2a_recipient_id: None,
            a2a_tenant_id: None,
            a2a_project_id: None,
            a2a_peers: Arc::new(crate::core::a2a::relay::RelayPeerTableV1::default()),
            relay_quotas: None,
            a2a_task_authority: Arc::new(crate::core::a2a::task::TaskAuthorityConfigV1::default()),
            remote_replays: Arc::new(super::remote_replay::RemoteReplayGuard::new(
                &root_str,
                super::handlers::REMOTE_REPLAY_RETENTION_SECONDS,
            )),
            concurrency: Arc::new(tokio::sync::Semaphore::new(1)),
            rate: Arc::new(RateLimiter::new(10, 10)),
            project_root: root_str,
            timeout: StdDuration::from_secs(3),
            server,
        });
    let request = Request::builder()
        .method("POST")
        .uri("/v1/tools/call")
        .header("Host", "localhost")
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "name": "ctx_read",
                "arguments": {"path": path, "mode": "full"}
            })
            .to_string(),
        ))
        .expect("request");

    let response_task = tokio::spawn(app.oneshot(request));
    wait_for(&probe.started, &probe.started_notify, "tool start").await;
    assert!(
        !response_task.is_finished(),
        "tool must start before the response deadline"
    );
    let response = tokio::time::timeout(StdDuration::from_secs(5), response_task)
        .await
        .expect("handler deadline")
        .expect("handler task")
        .expect("handler response");
    assert_eq!(response.status(), axum::http::StatusCode::GATEWAY_TIMEOUT);
    assert!(
        !probe.completed.load(Ordering::Acquire),
        "tool must still be blocked when REST returns 504"
    );

    // This is the critical distinction: response timeout has happened, then
    // the still-owned MCP service is allowed to finish its real tool call.
    _release_on_panic.release();
    wait_for(&probe.completed, &probe.completed_notify, "tool completion").await;
    assert!(probe.completed.load(Ordering::Acquire));
    assert!(probe.read_succeeded.load(Ordering::Acquire));
    // The consumed router has no local strong server owner; observe the
    // detached per-call service releasing its server after real completion.
    tokio::time::timeout(StdDuration::from_secs(5), async {
        while service_lifetime.strong_count() != 0 {
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("per-call MCP service must release its server");
}
