use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str, get_usize};
use crate::tool_defs::tool_def;
use crate::tools::ctx_transcript_compact::{compact_messages, render_result, serialize_transcript};

const DEFAULT_FRESH_TAIL_TOKENS: usize = 4_000;
const DEFAULT_PROTECT_MIN_MESSAGES: usize = 6;
const OFFLOAD_MAX_CHARS: usize = 8_000;

pub struct CtxTranscriptCompactTool;

impl McpTool for CtxTranscriptCompactTool {
    fn name(&self) -> &'static str {
        "ctx_transcript_compact"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_transcript_compact",
            "Compact an OpenAI-format message array deterministically: keep system + a fresh tail verbatim, replace older turns with a recoverable summary, and offload the raw turns into lean-ctx session memory (indexed for ctx_search/ctx_knowledge recall). Built for the Hermes context-engine plugin. Returns JSON {messages, stats}; tool_call/tool_result pairs are never split.",
            json!({
                "type": "object",
                "properties": {
                    "messages": {
                        "type": "array",
                        "items": { "type": "object" },
                        "description": "OpenAI-format message array to compact"
                    },
                    "fresh_tail_tokens": {
                        "type": "integer",
                        "description": "Recent tokens to keep verbatim (default 4000)"
                    },
                    "protect_min_messages": {
                        "type": "integer",
                        "description": "Minimum recent messages to keep verbatim (default 6)"
                    },
                    "focus_topic": {
                        "type": "string",
                        "description": "Optional topic to prioritise in the summary"
                    }
                },
                "required": ["messages"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let messages = args
            .get("messages")
            .and_then(Value::as_array)
            .ok_or_else(|| ErrorData::invalid_params("messages (array) is required", None))?;
        let fresh_tail = get_usize(args, "fresh_tail_tokens").unwrap_or(DEFAULT_FRESH_TAIL_TOKENS);
        let protect_min =
            get_usize(args, "protect_min_messages").unwrap_or(DEFAULT_PROTECT_MIN_MESSAGES);
        let focus = get_str(args, "focus_topic");

        let result = compact_messages(messages.clone(), fresh_tail, protect_min, focus.as_deref());

        // Best-effort offload of the raw older turns into session memory so the
        // recall tools (and the autonomy consolidation pipeline) can recover
        // them. Skipped when no session is bound (e.g. one-shot CLI).
        let offload_target = if result.did_compact && !result.summarized.is_empty() {
            ctx.session.as_ref()
        } else {
            None
        };
        if let Some(session_handle) = offload_target {
            let digest = serialize_transcript(&result.summarized, OFFLOAD_MAX_CHARS);
            if !digest.is_empty() {
                let mut session = session_handle.blocking_write();
                let _ = crate::tools::ctx_session::handle(
                    &mut session,
                    &[],
                    "finding",
                    Some(&digest),
                    None,
                    crate::tools::ctx_session::SessionToolOptions {
                        format: None,
                        path: None,
                        write: false,
                        privacy: None,
                        terse: Some(true),
                    },
                );
            }
        }

        let saved = result
            .original_tokens
            .saturating_sub(result.compacted_tokens);
        Ok(ToolOutput {
            text: render_result(&result),
            original_tokens: result.original_tokens,
            saved_tokens: saved,
            mode: Some("transcript_compact".to_string()),
            path: None,
            changed: false,
            shell_outcome: None,
        })
    }
}
