mod read_tools;
mod session_tools;
mod shell_tools;
mod utility_tools;

use rmcp::ErrorData;
use serde_json::Value;

use crate::server::helpers::get_str;
use crate::tools::LeanCtxServer;

impl LeanCtxServer {
    pub(super) async fn dispatch_tool(
        &self,
        name: &str,
        args: Option<&serde_json::Map<String, Value>>,
        minimal: bool,
    ) -> Result<String, ErrorData> {
        match name {
            "ctx_call" => {
                let inner = get_str(args, "name")
                    .ok_or_else(|| ErrorData::invalid_params("name is required", None))?;
                if inner == "ctx_call" {
                    return Err(ErrorData::invalid_params(
                        "ctx_call cannot invoke itself",
                        None,
                    ));
                }

                let arg_map = match args.and_then(|m| m.get("arguments")) {
                    None | Some(Value::Null) => None,
                    Some(Value::Object(map)) => Some(map.clone()),
                    Some(_) => {
                        return Err(ErrorData::invalid_params(
                            "arguments must be an object",
                            None,
                        ))
                    }
                };

                // Dispatch without recursive async calls.
                let result = match inner.as_str() {
                    "ctx_read" | "ctx_multi_read" | "ctx_smart_read" | "ctx_delta" | "ctx_edit"
                    | "ctx_fill" => {
                        self.dispatch_read_tools(&inner, arg_map.as_ref(), minimal)
                            .await?
                    }

                    "ctx_shell" | "ctx_search" | "ctx_execute" => {
                        self.dispatch_shell_tools(&inner, arg_map.as_ref(), minimal)
                            .await?
                    }

                    "ctx_session" | "ctx_knowledge" | "ctx_agent" | "ctx_share" | "ctx_task"
                    | "ctx_handoff" | "ctx_workflow" => {
                        self.dispatch_session_tools(&inner, arg_map.as_ref(), minimal)
                            .await?
                    }

                    _ => {
                        self.dispatch_utility_tools(&inner, arg_map.as_ref(), minimal)
                            .await?
                    }
                };

                self.record_call("ctx_call", 0, 0, Some(inner)).await;
                Ok(result)
            }
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
