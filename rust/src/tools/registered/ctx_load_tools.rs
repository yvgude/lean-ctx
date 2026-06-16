use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::dynamic_tools::{self, DynamicToolState, ToolCategory};
use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput};
use crate::tool_defs::tool_def;

pub struct CtxLoadToolsTool;

impl McpTool for CtxLoadToolsTool {
    fn name(&self) -> &'static str {
        "ctx_load_tools"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_load_tools",
            "Load/unload specialized tool categories on demand. Categories: arch, debug, memory, metrics, session. Core is always loaded.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["load", "unload", "list"],
                        "description": "load = activate category, unload = deactivate, list = show status"
                    },
                    "category": {
                        "type": "string",
                        "description": "Category name: arch|debug|memory|metrics|session"
                    }
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
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("list");
        let category_str = args.get("category").and_then(|v| v.as_str());

        match action {
            "list" => Ok(ToolOutput::simple(format_category_status())),
            "load" => {
                let cat_name = category_str.ok_or_else(|| {
                    ErrorData::invalid_params("'category' required for load action", None)
                })?;
                let cat = ToolCategory::parse(cat_name).ok_or_else(|| {
                    ErrorData::invalid_params(
                        format!(
                            "Unknown category '{cat_name}'. Available: {}",
                            DynamicToolState::all_categories().join(", ")
                        ),
                        None,
                    )
                })?;
                let changed = {
                    let Ok(mut state) = dynamic_tools::global().lock() else {
                        return Err(ErrorData::internal_error("dynamic_tools lock failed", None));
                    };
                    state.load_category(cat)
                };
                let text = if changed {
                    format!(
                        "Loaded category '{cat_name}'.\n{}",
                        format_category_status()
                    )
                } else {
                    format!("Category '{cat_name}' was already loaded.")
                };
                let mut out = ToolOutput::simple(text);
                out.changed = changed;
                Ok(out)
            }
            "unload" => {
                let cat_name = category_str.ok_or_else(|| {
                    ErrorData::invalid_params("'category' required for unload action", None)
                })?;
                let cat = ToolCategory::parse(cat_name).ok_or_else(|| {
                    ErrorData::invalid_params(
                        format!(
                            "Unknown category '{cat_name}'. Available: {}",
                            DynamicToolState::all_categories().join(", ")
                        ),
                        None,
                    )
                })?;
                let changed = {
                    let Ok(mut state) = dynamic_tools::global().lock() else {
                        return Err(ErrorData::internal_error("dynamic_tools lock failed", None));
                    };
                    state.unload_category(cat)
                };
                let text = if changed {
                    format!(
                        "Unloaded category '{cat_name}'.\n{}",
                        format_category_status()
                    )
                } else if cat == ToolCategory::Core {
                    "Cannot unload 'core' category.".to_string()
                } else {
                    format!("Category '{cat_name}' was not loaded.")
                };
                let mut out = ToolOutput::simple(text);
                out.changed = changed;
                Ok(out)
            }
            other => Err(ErrorData::invalid_params(
                format!("Unknown action '{other}'. Use load|unload|list."),
                None,
            )),
        }
    }
}

fn format_category_status() -> String {
    let Ok(state) = dynamic_tools::global().lock() else {
        return "dynamic_tools: lock unavailable".to_string();
    };
    let active = state.active_categories();
    let all = DynamicToolState::all_categories();
    let mut lines = vec![format!(
        "Dynamic tools: {} (list_changed={})",
        if state.supports_list_changed() {
            "active"
        } else {
            "all-visible"
        },
        state.supports_list_changed()
    )];
    for cat in &all {
        let status = if active.contains(cat) {
            "loaded"
        } else {
            "unloaded"
        };
        lines.push(format!("  {cat}: {status}"));
    }
    lines.join("\n")
}
