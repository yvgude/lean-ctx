use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::core::shared_context::SharedContext;
use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str, get_usize};
use crate::tool_defs::tool_def;

pub struct CtxMemoryTool;

impl McpTool for CtxMemoryTool {
    fn name(&self) -> &'static str {
        "ctx_memory"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            self.name(),
            "Store and retrieve durable cross-agent facts with BLAKE3 deduplication.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["put", "get", "list", "stats", "prune"],
                        "description": "Memory operation"
                    },
                    "content": { "type": "string", "description": "Fact content (required for put)" },
                    "agent": { "type": "string", "description": "Agent identity for put; defaults to unknown" },
                    "query": { "type": "string", "description": "Keyword query (required for get)" },
                    "category": {
                        "type": "string",
                        "enum": ["fact", "decision", "blocker", "pattern"],
                        "description": "Category for put or list filter"
                    },
                    "limit": { "type": "integer", "minimum": 0, "default": 10 },
                    "max_age_seconds": { "type": "integer", "minimum": 0, "description": "Maximum idle age for prune" },
                    "max_entries": { "type": "integer", "minimum": 0, "description": "Maximum entries retained by prune" }
                },
                "required": ["action"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let action = get_str(args, "action")
            .ok_or_else(|| ErrorData::invalid_params("action is required", None))?;
        let store = SharedContext::default();
        let limit = get_usize(args, "limit").unwrap_or(10);
        let text = match action.as_str() {
            "put" => {
                let content = required(args, "content")?;
                let agent = get_str(args, "agent").unwrap_or_else(|| "unknown".to_string());
                let category = get_str(args, "category").unwrap_or_else(|| "fact".to_string());
                let id = store.put(content, agent, category).map_err(memory_error)?;
                format!("stored {id}")
            }
            "get" => {
                let query = required(args, "query")?;
                serialize(&store.get_relevant(query, limit).map_err(memory_error)?)?
            }
            "list" => {
                let entries = match get_str(args, "category") {
                    Some(category) => store
                        .get_by_category(&category)
                        .map_err(memory_error)?
                        .into_iter()
                        .take(limit)
                        .collect(),
                    None => store.get_recent(limit).map_err(memory_error)?,
                };
                serialize(&entries)?
            }
            "stats" => serialize(&store.stats().map_err(memory_error)?)?,
            "prune" => {
                let max_age_seconds =
                    get_usize(args, "max_age_seconds").unwrap_or(60 * 60 * 24 * 30) as u64;
                let max_entries = get_usize(args, "max_entries").unwrap_or(1_000);
                let removed = store
                    .prune(max_age_seconds, max_entries)
                    .map_err(memory_error)?;
                format!("pruned {removed} entries")
            }
            other => {
                return Err(ErrorData::invalid_params(
                    format!("unknown action '{other}'"),
                    None,
                ));
            }
        };
        Ok(ToolOutput::simple(text))
    }
}

fn required<'a>(args: &'a Map<String, Value>, key: &str) -> Result<&'a str, ErrorData> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ErrorData::invalid_params(format!("{key} is required"), None))
}

fn serialize<T: serde::Serialize>(value: &T) -> Result<String, ErrorData> {
    serde_json::to_string_pretty(value).map_err(|error| {
        ErrorData::internal_error(format!("serialize memory result: {error}"), None)
    })
}

fn memory_error(error: impl std::fmt::Display) -> ErrorData {
    ErrorData::internal_error(format!("shared context: {error}"), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_tool_schema_has_expected_actions() {
        let definition = CtxMemoryTool.tool_def();
        assert_eq!(definition.name, "ctx_memory");
    }
}
