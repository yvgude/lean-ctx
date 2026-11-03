// SPDX-License-Identifier: Apache-2.0
//! Attribution and summary helpers for `ctx_read`.
//!
//! Split out of `ctx_read.rs` to keep that file under the #660 LOC gate. These
//! are leaf helpers with no access to the read pipeline's local state.

use std::sync::atomic::Ordering;

use crate::server::tool_trait::{ToolContext, ToolOutput};

pub(super) fn record_attribution_result(ctx: &ToolContext, source: String, output: &ToolOutput) {
    let session_id = lean_ctx::core::task_spine::TaskSpine::task_id()
        .or_else(|| {
            ctx.session
                .as_ref()
                .and_then(|session| session.try_read().ok().map(|state| state.id.clone()))
        })
        .unwrap_or_else(|| "mcp-session".to_string());
    let turn_provided = ctx
        .call_count
        .as_ref()
        .map_or(0, |count| count.load(Ordering::Relaxed) as u64);
    let token_cost = lean_ctx::core::tokens::count_tokens(&output.text);
    let chunk = lean_ctx::core::causal_attribution::ContextChunkRecord::new(
        &output.text,
        source,
        token_cost,
        turn_provided,
    );
    if let Err(error) = lean_ctx::core::causal_attribution::record_chunk(&session_id, chunk) {
        tracing::debug!(%error, "causal attribution ctx_read recording failed");
    }
}

pub(super) fn extract_file_summary(output: &str, path: &str) -> String {
    let hint = lean_ctx::core::auto_findings::extract_content_hint(output);
    if !hint.is_empty() {
        return hint;
    }
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let line_count = output.lines().count();
    if line_count > 5 {
        format!("{ext} file, {line_count} lines")
    } else {
        String::new()
    }
}

/// May a session task with this `intent` steer how a file is read?
///
/// Only a task the caller actually stated may. `auto_infer_task` marks the
/// descriptions it fabricates from touched-file patterns — "Working on
/// /repo/src/printer (explore)" — with `intent = "inferred"`. Those words are a
/// telemetry label, not a statement about any file's contents, and letting them
/// drive the information-bottleneck filter or the intent-target override
/// silently answered a question the caller never asked (#1590).
///
/// The same holds for `intent = "git"`: a summary of the working tree's dirty
/// files describes *state*, often left over from unrelated work, not what the
/// caller is asking about — filtering reads by "files changed, insertions"
/// dropped the very lines the caller wanted.
pub(crate) fn task_intent_steers_read(intent: Option<&str>) -> bool {
    !lean_ctx::core::session::is_auto_task_intent(intent)
}
