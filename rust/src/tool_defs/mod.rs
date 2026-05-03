use std::sync::Arc;

use rmcp::model::Tool;
use serde_json::{json, Map, Value};

mod granular;
pub use granular::{granular_tool_defs, list_all_tool_defs, unified_tool_defs};

pub fn tool_def(name: &'static str, description: &'static str, schema_value: Value) -> Tool {
    let schema: Map<String, Value> = match schema_value {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    Tool::new(name, description, Arc::new(schema))
}

const CORE_TOOL_NAMES: &[&str] = &[
    "ctx_read",
    "ctx_multi_read",
    "ctx_call",
    "ctx_shell",
    "ctx_search",
    "ctx_tree",
    "ctx_edit",
    "ctx_session",
    "ctx_knowledge",
];

pub fn lazy_tool_defs() -> Vec<Tool> {
    let all = granular_tool_defs();
    let mut core: Vec<Tool> = all
        .into_iter()
        .filter(|t| CORE_TOOL_NAMES.contains(&t.name.as_ref()))
        .collect();

    core.push(tool_def(
        "ctx_discover_tools",
        "Search available lean-ctx tools by keyword. Returns matching tool names + descriptions for on-demand loading.",
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search keyword (e.g. 'graph', 'cost', 'workflow', 'dedup')"
                }
            },
            "required": ["query"]
        }),
    ));

    core
}

pub fn discover_tools(query: &str) -> String {
    let all = list_all_tool_defs();
    let query_lower = query.to_lowercase();
    let matches: Vec<(&str, &str)> = all
        .iter()
        .filter(|(name, desc, _)| {
            name.to_lowercase().contains(&query_lower) || desc.to_lowercase().contains(&query_lower)
        })
        .map(|(name, desc, _)| (*name, *desc))
        .collect();

    if matches.is_empty() {
        return format!("No tools found matching '{query}'. Try broader terms like: graph, cost, session, search, compress, agent, workflow, gain.");
    }

    let mut out = format!("{} tools matching '{query}':\n", matches.len());
    for (name, desc) in &matches {
        let short = if desc.len() > 80 { &desc[..80] } else { desc };
        out.push_str(&format!("  {name} — {short}\n"));
    }
    out.push_str(
        "\nIf your MCP client registers tools only once at startup (static tools/list), \
use ctx_call (available in lazy mode) to invoke discovered tools:\n\
  ctx_call {\"name\":\"ctx_graph\",\"arguments\":{\"action\":\"status\"}}\n",
    );
    out
}

pub fn is_full_mode() -> bool {
    std::env::var("LEAN_CTX_FULL_TOOLS").is_ok()
        || std::env::var("LEAN_CTX_LAZY_TOOLS")
            .is_ok_and(|v| v == "0" || v.eq_ignore_ascii_case("false"))
}
