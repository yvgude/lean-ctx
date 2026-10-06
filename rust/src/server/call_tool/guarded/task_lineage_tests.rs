// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::core::task_spine::TaskSpine;
use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput};
use lean_ctx_protocol::TaskEnvelopeV1;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

type Captures = Arc<Mutex<Vec<(String, Option<TaskEnvelopeV1>)>>>;

/// Observe the real blocking dispatch, then execute the actual file-read tool.
struct ObservedRead {
    captures: Captures,
    rendezvous: Arc<(Mutex<usize>, Condvar)>,
}

impl McpTool for ObservedRead {
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
        self.captures
            .lock()
            .unwrap()
            .push((args["path"].as_str().unwrap().into(), TaskSpine::current()));
        let (count, ready) = &*self.rendezvous;
        let mut count = count.lock().unwrap();
        *count += 1;
        ready.notify_all();
        let (count, timeout) = ready
            .wait_timeout_while(count, Duration::from_secs(5), |count| *count < 2)
            .unwrap();
        if timeout.timed_out() && *count < 2 {
            return Err(ErrorData::internal_error(
                "fixture rendezvous timed out",
                None,
            ));
        }
        drop(count);
        crate::tools::registered::ctx_read::CtxReadTool.handle(args, ctx)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_mcp_reads_preserve_worker_and_receipt_task_identity() {
    // Other tests change process-global role/config state; preserve real guards
    // and exercise the full MCP route in an isolated test process.
    if std::env::var_os("LEAN_CTX_MCP_LINEAGE_SUBPROCESS").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "server::call_tool::guarded::task_lineage_tests::concurrent_mcp_reads_preserve_worker_and_receipt_task_identity", "--nocapture"])
            .env("LEAN_CTX_MCP_LINEAGE_SUBPROCESS", "1")
            .env("LEAN_CTX_ROLE", "coder")
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let _data = crate::core::data_dir::isolated_data_dir();
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let mut server = LeanCtxServer::new_with_project_root(root.to_str());
    *server.agent_id.write().await = Some("lineage-reader".into());
    let captures = Captures::default();
    let mut registry = crate::server::registry::build_registry();
    registry.register(Box::new(ObservedRead {
        captures: captures.clone(),
        rendezvous: Arc::new((Mutex::new(0), Condvar::new())),
    }));
    server.registry = Some(Arc::new(registry));
    let requests: Vec<CallToolRequestParams> = ["first", "second"]
        .into_iter()
        .map(|name| {
            let path = root.join(format!("{name}.rs"));
            std::fs::write(&path, format!("pub fn {name}() {{}}\n")).unwrap();
            serde_json::from_value(serde_json::json!({
                "name":"ctx_read", "arguments":{"path":path,"mode":"full"},
                "_meta":{"idempotencyKey":name}
            }))
            .unwrap()
        })
        .collect();
    let (first, second) = tokio::join!(
        server.call_tool_guarded(requests[0].clone()),
        server.call_tool_guarded(requests[1].clone()),
    );
    for (response, expected) in [
        (first.unwrap(), "pub fn first"),
        (second.unwrap(), "pub fn second"),
    ] {
        assert_ne!(response.is_error, Some(true), "{response:?}");
        assert!(
            serde_json::to_string(&response).unwrap().contains(expected),
            "{response:?}"
        );
    }
    let captured = captures.lock().unwrap().clone();
    assert_eq!(captured.len(), 2);
    let first = captured[0]
        .1
        .as_ref()
        .expect("first blocking handler must inherit canonical task");
    let second = captured[1]
        .1
        .as_ref()
        .expect("second blocking handler must inherit canonical task");
    assert_ne!(first.task_id, second.task_id);
    let session = server.session.read().await;
    for request in &requests {
        let path = request.arguments.as_ref().unwrap()["path"]
            .as_str()
            .unwrap();
        let envelope = captured
            .iter()
            .find(|(captured_path, _)| captured_path == path)
            .unwrap()
            .1
            .as_ref()
            .unwrap();
        let hash = crate::server::helpers::hash_fast(
            &crate::server::helpers::canonical_args_string(request.arguments.as_ref()),
        );
        let receipt = session
            .evidence
            .iter()
            .find(|entry| {
                entry.tool.as_deref() == Some("ctx_read")
                    && entry.input_md5.as_deref() == Some(hash.as_str())
            })
            .expect("real read receipt");
        assert_eq!(receipt.task_id.as_deref(), Some(envelope.task_id.as_str()));
        assert_eq!(
            receipt.agent_id.as_deref(),
            Some(envelope.agent_id.as_str())
        );
    }
    let receipt_count = session
        .evidence
        .iter()
        .filter(|entry| entry.tool.as_deref() == Some("ctx_read"))
        .count();
    drop(session);
    let replay = server.call_tool_guarded(requests[0].clone()).await.unwrap();
    assert_ne!(replay.is_error, Some(true));
    assert_eq!(
        captures.lock().unwrap().len(),
        2,
        "replay must not dispatch another worker"
    );
    assert_eq!(
        server
            .session
            .read()
            .await
            .evidence
            .iter()
            .filter(|entry| entry.tool.as_deref() == Some("ctx_read"))
            .count(),
        receipt_count
    );
}
