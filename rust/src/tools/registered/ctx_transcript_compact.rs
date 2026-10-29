use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};
use std::time::Duration;

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str, get_usize};
use crate::tool_defs::tool_def;
use crate::tools::ctx_transcript_compact::{compact_messages, render_result, serialize_transcript};

const DEFAULT_FRESH_TAIL_TOKENS: usize = 4_000;
const DEFAULT_PROTECT_MIN_MESSAGES: usize = 6;
const OFFLOAD_MAX_CHARS: usize = 8_000;
const SESSION_LOCK_BUDGET: Duration = Duration::from_millis(250);

fn session_matches_snapshot(
    live: &crate::core::session::SessionState,
    snapshot: &crate::core::session::SessionState,
) -> bool {
    live.id == snapshot.id
        && live.version == snapshot.version
        && live.project_root == snapshot.project_root
        && live.shell_cwd == snapshot.shell_cwd
        && live.extra_roots == snapshot.extra_roots
}

pub struct CtxTranscriptCompactTool;

impl McpTool for CtxTranscriptCompactTool {
    fn name(&self) -> &'static str {
        "ctx_transcript_compact"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_transcript_compact",
            "Compact an OpenAI-format message array deterministically:\n\
             keep system + fresh tail verbatim, replace older turns with a recoverable\n\
             summary, offload raw turns into session memory (indexed for recall).\n\
             Returns JSON {messages, stats}. tool_call/tool_result pairs never split.",
            json!({
                "type": "object",
                "properties": {
                    "messages": {
                        "type": "array",
                        "items": { "type": "object" },
                        "description": "OpenAI message array to compact"
                    },
                    "fresh_tail_tokens": {
                        "type": "integer",
                        "description": "Recent tokens to keep verbatim"
                    },
                    "protect_min_messages": {
                        "type": "integer",
                        "description": "Minimum recent messages to keep"
                    },
                    "focus_topic": {
                        "type": "string",
                        "description": "Topic to prioritise in summary"
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

        // Offload only through the same detached admission and optimistic
        // commit path used by direct session actions.
        let offload_target = if result.did_compact && !result.summarized.is_empty() {
            ctx.session.as_ref()
        } else {
            None
        };
        if let Some(session_handle) = offload_target {
            let digest = serialize_transcript(&result.summarized, OFFLOAD_MAX_CHARS);
            if !digest.is_empty() {
                if let Some(snapshot) = crate::server::bounded_lock::read_for(
                    session_handle,
                    "transcript compact session snapshot",
                    SESSION_LOCK_BUDGET,
                )
                .map(|session| session.clone())
                {
                    if let Ok(prepared) = crate::tools::ctx_session::prepare_admitted_action(
                        &snapshot,
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
                            agent_id: None,
                        },
                    ) {
                        if let Some(mut live) = crate::server::bounded_lock::write_for(
                            session_handle,
                            "transcript compact session commit",
                            SESSION_LOCK_BUDGET,
                        ) {
                            if session_matches_snapshot(&live, &snapshot)
                                && prepared.store_admission.is_current()
                            {
                                *live = prepared.candidate;
                            }
                        }
                    }
                }
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
            content_blocks: None,
        })
    }
}
