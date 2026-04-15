use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};

use anyhow::{anyhow, Context, Result};
use rmcp::{
    model::{
        CallToolRequest, CallToolRequestParams, CallToolResult, ClientJsonRpcMessage,
        ClientRequest, JsonRpcRequest, NumberOrString, ServerJsonRpcMessage, ServerResult,
    },
    service::serve_directly,
    service::RoleServer,
    transport::OneshotTransport,
};
use serde_json::{Map, Value};

use crate::tools::LeanCtxServer;

pub struct ContextEngine {
    server: LeanCtxServer,
    next_id: AtomicI64,
}

impl ContextEngine {
    pub fn new() -> Self {
        Self {
            server: LeanCtxServer::new(),
            next_id: AtomicI64::new(1),
        }
    }

    pub fn with_project_root(project_root: impl Into<PathBuf>) -> Self {
        Self {
            server: LeanCtxServer::new_with_project_root(Some(
                project_root.into().to_string_lossy().to_string(),
            )),
            next_id: AtomicI64::new(1),
        }
    }

    pub fn from_server(server: LeanCtxServer) -> Self {
        Self {
            server,
            next_id: AtomicI64::new(1),
        }
    }

    pub fn server(&self) -> &LeanCtxServer {
        &self.server
    }

    pub fn manifest(&self) -> Value {
        crate::core::mcp_manifest::manifest_value()
    }

    pub async fn call_tool_value(&self, name: &str, arguments: Option<Value>) -> Result<Value> {
        let result = self.call_tool_result(name, arguments).await?;
        serde_json::to_value(result).map_err(|e| anyhow!("serialize CallToolResult: {e}"))
    }

    pub async fn call_tool_result(
        &self,
        name: &str,
        arguments: Option<Value>,
    ) -> Result<CallToolResult> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req_id = NumberOrString::Number(id);

        let args_obj: Map<String, Value> = match arguments {
            None => Map::new(),
            Some(Value::Object(m)) => m,
            Some(other) => {
                return Err(anyhow!(
                    "tool arguments must be a JSON object (got {})",
                    other
                ))
            }
        };

        let params = CallToolRequestParams::new(name.to_string()).with_arguments(args_obj);
        let call: CallToolRequest = CallToolRequest::new(params);
        let client_req = ClientRequest::CallToolRequest(call);
        let msg = ClientJsonRpcMessage::Request(JsonRpcRequest::new(req_id, client_req));

        let (transport, mut rx) = OneshotTransport::<RoleServer>::new(msg);
        let service = serve_directly(self.server.clone(), transport, None);
        tokio::spawn(async move {
            let _ = service.waiting().await;
        });

        let Some(server_msg) = rx.recv().await else {
            return Err(anyhow!("no response from tool call"));
        };

        match server_msg {
            ServerJsonRpcMessage::Response(r) => match r.result {
                ServerResult::CallToolResult(result) => Ok(result),
                other => Err(anyhow!("unexpected server result: {:?}", other)),
            },
            ServerJsonRpcMessage::Error(e) => Err(anyhow!("{e:?}")).context("tool call error"),
            ServerJsonRpcMessage::Notification(_) => Err(anyhow!("unexpected notification")),
            ServerJsonRpcMessage::Request(_) => Err(anyhow!("unexpected request")),
        }
    }

    pub async fn call_tool_text(&self, name: &str, arguments: Option<Value>) -> Result<String> {
        let result = self.call_tool_result(name, arguments).await?;
        let mut out = String::new();
        for c in result.content {
            if let Some(t) = c.as_text() {
                out.push_str(&t.text);
            }
        }
        if out.is_empty() {
            if let Some(v) = result.structured_content {
                out = v.to_string();
            }
        }
        Ok(out)
    }
}
