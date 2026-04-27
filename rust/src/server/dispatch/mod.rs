mod read_tools;
mod session_tools;
mod shell_tools;
mod utility_tools;

use rmcp::ErrorData;
use serde_json::Value;

use crate::tools::LeanCtxServer;

impl LeanCtxServer {
    pub(super) async fn dispatch_tool(
        &self,
        name: &str,
        args: Option<&serde_json::Map<String, Value>>,
        minimal: bool,
    ) -> Result<String, ErrorData> {
        match name {
            "ctx_read" | "ctx_multi_read" | "ctx_smart_read" | "ctx_delta" | "ctx_edit"
            | "ctx_fill" => self.dispatch_read_tools(name, args, minimal).await,

            "ctx_shell" | "ctx_search" | "ctx_execute" => {
                self.dispatch_shell_tools(name, args, minimal).await
            }

            "ctx_session" | "ctx_knowledge" | "ctx_agent" | "ctx_share" | "ctx_task"
            | "ctx_handoff" | "ctx_workflow" => {
                self.dispatch_session_tools(name, args, minimal).await
            }

            _ => self.dispatch_utility_tools(name, args, minimal).await,
        }
    }
}
