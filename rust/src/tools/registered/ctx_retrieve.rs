use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use crate::tool_defs::tool_def;

pub struct CtxRetrieveTool;

impl McpTool for CtxRetrieveTool {
    fn name(&self) -> &'static str {
        "ctx_retrieve"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_retrieve",
            "Retrieve original uncompressed content from the session cache (CCR). \
             Use when a compressed ctx_read output is insufficient.",
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path whose original content to retrieve"
                    },
                    "query": {
                        "type": "string",
                        "description": "Optional: search within cached content"
                    }
                },
                "required": ["path"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let path_raw = get_str(args, "path")
            .ok_or_else(|| ErrorData::invalid_params("path is required", None))?;
        let resolved = if let Some(p) = ctx.resolved_path("path") {
            p.to_string()
        } else if let Some(err) = ctx.path_error("path") {
            return Err(ErrorData::invalid_params(format!("path: {err}"), None));
        } else {
            path_raw.clone()
        };
        let query = get_str(args, "query");

        let cache = ctx
            .cache
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("cache not available", None))?;
        let Some(guard) = crate::server::bounded_lock::read(cache, "ctx_retrieve") else {
            return Ok(ToolOutput::simple(
                "[retrieve unavailable — cache busy, retry]".to_string(),
            ));
        };
        let result = match guard.get_full_content(&resolved) {
            Some(full) => {
                if let Some(ref q) = query {
                    ccr_search_within(&full, q)
                } else {
                    full
                }
            }
            None => {
                format!("No cached content for \"{path_raw}\". Use ctx_read(\"{path_raw}\") first.")
            }
        };

        Ok(ToolOutput::simple(result))
    }
}

fn ccr_search_within(content: &str, query: &str) -> String {
    let query_lower = query.to_lowercase();
    let terms: Vec<&str> = query_lower.split_whitespace().collect();
    if terms.is_empty() {
        return content.to_string();
    }

    let mut matches: Vec<(usize, &str)> = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let lower = line.to_lowercase();
        if terms.iter().any(|t| lower.contains(t)) {
            matches.push((i + 1, line));
        }
    }

    if matches.is_empty() {
        return format!("No lines matching \"{query}\" in cached content.");
    }

    let total = content.lines().count();
    let mut out = format!("# {}/{total} lines match \"{query}\"\n", matches.len());
    for (lineno, line) in matches.iter().take(200) {
        out.push_str(&format!("{lineno:>6}| {line}\n"));
    }
    if matches.len() > 200 {
        out.push_str(&format!("... and {} more matches\n", matches.len() - 200));
    }
    out
}
