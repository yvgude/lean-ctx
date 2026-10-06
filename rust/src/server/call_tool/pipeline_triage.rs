// SPDX-License-Identifier: Apache-2.0

//! Post-dispatch task triage and its verbatim/bypass escape hatches.

#[allow(clippy::wildcard_imports)]
use super::*;

/// `ctx_read` already owns mode selection and edit-safety guarantees. Running a
/// second lossy pass after it resolved `auto` to `full` would hide content the
/// caller must see (#1511), so every read result bypasses post-dispatch triage.
pub(in crate::server::call_tool) fn triage_bypass_requested(
    name: &str,
    args: Option<&serde_json::Map<String, serde_json::Value>>,
) -> bool {
    name == "ctx_read"
        || super::super::context_gate::protected_path_requested(args)
        || args.is_some_and(|args| {
            args.get("raw")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
                || args
                    .get("mode")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|mode| {
                        mode == "raw"
                            || mode == "full"
                            || mode == "full-compact"
                            || mode.starts_with("lines:")
                            || mode.starts_with("anchored:")
                            || mode == "diff"
                    })
                || args
                    .get("aggressiveness")
                    .and_then(serde_json::Value::as_f64)
                    .is_some_and(|value| value == 0.0)
                || args
                    .get("fresh")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
        })
}

/// Whether the caller explicitly asked for the original bytes (#1582).
///
/// Deliberately narrower than [`triage_bypass_requested`]: only `raw = true`,
/// `mode = "raw"` and `inline = true` count. Those are the escape hatches every
/// compression annotation points at, so they earn the larger verbatim turn
/// budget. `full`, `lines:`, `anchored` and friends stay on the ordinary budget
/// — they are routine reads, not a request to defeat compression, and exempting
/// them would turn the backstop off for most traffic.
///
/// #1812: `inline` belongs here. It is the same request as `raw` — "return the
/// command's own output, uncompressed" — and holding it to the smaller backstop
/// truncated it at ~4k tokens while the identical command with `raw=true`
/// returned in full. Worse, the compressed path is what produces the archive
/// line, so a truncated `inline` response had no recovery route at all and its
/// notice pointed at `ctx_read(lines=)`, which needs a path that command output
/// does not have.
pub(in crate::server::call_tool) fn verbatim_requested(
    name: &str,
    args: Option<&serde_json::Map<String, serde_json::Value>>,
) -> bool {
    if !matches!(name, "ctx_read" | "ctx_shell") {
        return false;
    }
    args.is_some_and(|args| {
        args.get("raw")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
            || args
                .get("inline")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            || args
                .get("mode")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|mode| mode == "raw")
    })
}

/// Applies task triage at the native dispatch chokepoint. If no profile is
/// available or filtering panics, preserve the raw tool response unchanged.
pub(super) fn apply_task_triage_filter(
    result_text: String,
    profile: Option<&crate::core::triage::profile::TaskProfileLocal>,
    decision_context: &mut Option<crate::core::decision_loop_runtime::TaskContext>,
    max_filter_level: u8,
) -> String {
    if max_filter_level == 0 {
        return result_text;
    }
    let Some(profile) = profile else {
        return result_text;
    };

    let filtered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let level = context_gate::triage_filter_level(profile).min(max_filter_level);
        (level > 0).then(|| context_gate::apply_triage_filter(&result_text, profile, level))
    }));
    let Ok(Some((filtered_text, filtered_lines))) = filtered else {
        return result_text;
    };

    if let Some(context) = decision_context {
        context.filtered_lines = filtered_lines;
    }
    filtered_text
}
