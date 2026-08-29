//! Mode-degradation and attribution helpers for `ctx_read`.
//!
//! Split out of `ctx_read.rs` to keep that file under the #660 LOC gate. These
//! are leaf helpers with no access to the read pipeline's local state.

use std::sync::atomic::Ordering;

use crate::server::tool_trait::{ToolContext, ToolOutput};

pub(super) fn record_attribution_result(ctx: &ToolContext, source: String, output: &ToolOutput) {
    let session_id = crate::core::task_spine::TaskSpine::task_id()
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
    let token_cost = crate::core::tokens::count_tokens(&output.text);
    let chunk = crate::core::causal_attribution::ContextChunkRecord::new(
        &output.text,
        source,
        token_cost,
        turn_provided,
    );
    if let Err(error) = crate::core::causal_attribution::record_chunk(&session_id, chunk) {
        tracing::debug!(%error, "causal attribution ctx_read recording failed");
    }
}

pub(super) fn apply_verdict(
    mode: &str,
    verdict: crate::core::degradation_policy::DegradationVerdictV1,
) -> (String, bool) {
    use crate::core::degradation_policy::DegradationVerdictV1;
    match verdict {
        DegradationVerdictV1::Ok => (mode.to_string(), false),
        DegradationVerdictV1::Warn => match mode {
            "full" => ("map".to_string(), true),
            other => (other.to_string(), false),
        },
        DegradationVerdictV1::Throttle => match mode {
            "full" | "map" => ("signatures".to_string(), true),
            other => (other.to_string(), false),
        },
        DegradationVerdictV1::Block => {
            if mode == "signatures" {
                ("signatures".to_string(), false)
            } else {
                ("signatures".to_string(), true)
            }
        }
    }
}

pub(super) fn auto_degrade_read_mode(mode: &str) -> (String, Option<String>) {
    if crate::core::config::Config::load().no_degrade_effective() {
        return (mode.to_string(), None);
    }
    let profile = crate::core::profiles::active_profile();
    if !profile.degradation.enforce_effective() {
        return (mode.to_string(), None);
    }
    let policy = crate::core::degradation_policy::evaluate_v1_for_tool("ctx_read", None);
    let (new_mode, degraded) = apply_verdict(mode, policy.decision.verdict);
    let warning = if degraded {
        Some(format!(
            "⚠ Context pressure: mode={mode} was downgraded to mode={new_mode} \
             (verdict: {:?}). Use start_line=1 to bypass, or run ctx_compress to free budget.",
            policy.decision.verdict
        ))
    } else {
        None
    };
    (new_mode, warning)
}

pub(super) fn extract_file_summary(output: &str, path: &str) -> String {
    let hint = crate::core::auto_findings::extract_content_hint(output);
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
pub(crate) fn task_intent_steers_read(intent: Option<&str>) -> bool {
    intent != Some("inferred")
}
