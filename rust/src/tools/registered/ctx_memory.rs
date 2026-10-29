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
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let action = get_str(args, "action")
            .ok_or_else(|| ErrorData::invalid_params("action is required", None))?;
        // Facts this path stored under an earlier identity of the project.
        crate::core::legacy_adoption::auto_adopt_shared_context(&ctx.project_root);
        let store = SharedContext::for_project(&ctx.project_root).map_err(memory_error)?;
        let limit = get_usize(args, "limit").unwrap_or(10);
        let text = match action.as_str() {
            "put" => {
                let content = storable(required(args, "content")?)?;
                let agent = get_str(args, "agent").unwrap_or_else(|| "unknown".to_string());
                let category = get_str(args, "category").unwrap_or_else(|| "fact".to_string());
                let id = store.put(content, agent, category).map_err(memory_error)?;
                format!("stored {id}")
            }
            "get" => {
                let query = required(args, "query")?;
                served(&serialize(
                    &store.get_relevant(query, limit).map_err(memory_error)?,
                )?)
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
                served(&serialize(&entries)?)
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

/// The text a `put` may persist (E3): caller-supplied content admitted for a
/// derived store under the current policy. Stored bytes are the admitted
/// text — a value the gateway masks is stored masked; content that is
/// withheld or restricted is refused and nothing is written.
fn storable(content: &str) -> Result<String, ErrorData> {
    crate::core::context_admission::carrier::admit_for_store(content)
        .map_err(|withheld| ErrorData::invalid_params(withheld.message("memory content"), None))
}

/// Stored entries as they may be served NOW (G04). Shared memory carries no
/// source binding, so the rendered entries follow the originless rule:
/// withheld while protection is active in any layer, re-admitted under
/// today's content rules otherwise. A refusal is the content-free notice.
fn served(rendered: &str) -> String {
    crate::tools::admitted_sources::serve_originless(rendered, "shared memory")
        .unwrap_or_else(|reason| format!("[withheld: {reason}]"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// E3 / G04: `put` stored caller text raw and `get` / `list` printed the
    /// store raw. Both ends now pass admission.
    #[test]
    fn shared_memory_is_admitted_at_write_and_served_under_the_originless_rule() {
        const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");
        let _data = crate::core::data_dir::isolated_data_dir();
        let entries = "[\n  {\n    \"content\": \"retries live in net/retry.rs\"\n  }\n]";
        {
            let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
            let clean = storable("retries live in net/retry.rs");
            assert!(clean.is_ok_and(|text| text == "retries live in net/retry.rs"));
            let keyed = storable(&format!("deploy key is {AWS_KEY}"));
            assert!(keyed.is_ok_and(|text| !text.contains(AWS_KEY)));
            assert_eq!(served(entries), entries);
            let legacy = served(&format!("[{{\"content\": \"key {AWS_KEY}\"}}]"));
            assert!(!legacy.contains(AWS_KEY), "{legacy}");
        }
        let policy = crate::core::policy::load(
            "name = \"ctx-memory-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n\
             [filters]\nclassification = \"block\"\nblocked_labels = [\"CONFIDENTIAL\"]\n",
        )
        .unwrap();
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(policy));
        assert!(storable("CONFIDENTIAL\nroadmap-marker").is_err());
        let withheld = served(entries);
        assert!(withheld.starts_with("[withheld: "), "{withheld}");
        assert!(!withheld.contains("retry.rs"), "{withheld}");
    }

    #[test]
    fn memory_tool_schema_has_expected_actions() {
        let definition = CtxMemoryTool.tool_def();
        assert_eq!(definition.name, "ctx_memory");
    }
}
