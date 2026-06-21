use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::core::protocol::append_savings;
use crate::core::tokens::count_tokens;
use crate::core::web::{self, ReadMode, ReadOptions};
use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_int, get_str};
use crate::tool_defs::tool_def;

/// `ctx_url_read` — fetch a web page, PDF, or YouTube video and return
/// compressed, citation-backed context (HTML/PDF→text, transcript flattening,
/// extractive research-compression modes).
pub struct CtxUrlReadTool;

impl McpTool for CtxUrlReadTool {
    fn name(&self) -> &'static str {
        "ctx_url_read"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_url_read",
            "Fetch URL: pages→Markdown; PDF→text; YouTube→transcript; mode=auto best per type\n\
             mode=facts|quotes for research (claims+confidence). query='topic' to focus extraction.\n\
             GitHub blob/raw URLs auto-resolve to raw file. SSRF-guarded (no private IPs). max_tokens=6000.",
            json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "http(s) URL (page, PDF, YouTube)" },
                    "mode": {
                        "type": "string",
                        "enum": ["auto", "markdown", "text", "links", "facts", "quotes", "transcript"],
                        "description": "auto|markdown|text|links|facts|quotes|transcript (default auto)"
                    },
                    "query": { "type": "string", "description": "Focus query; boosts facts/quotes relevance" },
                    "max_tokens": { "type": "integer", "description": "Token budget (default 6000)" },
                    "max_items": { "type": "integer", "description": "Max items for facts/quotes (default 12)" },
                    "timeout_secs": { "type": "integer", "description": "Timeout seconds (default 20, max 60)" }
                },
                "required": ["url"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let url = get_str(args, "url")
            .ok_or_else(|| ErrorData::invalid_params("url is required", None))?;

        let mode = match get_str(args, "mode") {
            Some(m) => ReadMode::parse(&m).ok_or_else(|| {
                ErrorData::invalid_params(
                    format!("invalid mode '{m}' (use: auto, markdown, text, links, facts, quotes, transcript)"),
                    None,
                )
            })?,
            None => ReadMode::Auto,
        };

        let query = get_str(args, "query");
        let max_tokens = get_int(args, "max_tokens")
            .map_or(web::DEFAULT_MAX_TOKENS, |n| n.clamp(200, 50_000) as usize);
        let max_items =
            get_int(args, "max_items").map_or(web::DEFAULT_MAX_ITEMS, |n| n.clamp(1, 100) as usize);
        let timeout_secs = get_int(args, "timeout_secs")
            .map_or(web::fetch::DEFAULT_TIMEOUT_SECS, |n| n.clamp(1, 60) as u64);

        let opts = ReadOptions {
            url: &url,
            mode,
            query: query.as_deref(),
            max_tokens,
            max_items,
            timeout_secs,
        };

        let result = tokio::task::block_in_place(|| web::read_url(&opts));

        match result {
            Ok(read) => {
                let sent = count_tokens(&read.content);
                let saved = read.original_tokens.saturating_sub(sent);
                let text = append_savings(&read.content, read.original_tokens, sent);
                Ok(ToolOutput {
                    text,
                    original_tokens: read.original_tokens,
                    saved_tokens: saved,
                    mode: Some(read.mode.label().to_string()),
                    path: Some(read.final_url),
                    changed: false,
                    shell_outcome: None,
                })
            }
            Err(e) => Err(ErrorData::invalid_params(
                format!("ctx_url_read failed: {e}"),
                None,
            )),
        }
    }
}
