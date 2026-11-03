// SPDX-License-Identifier: Apache-2.0
//! The ctx_read disk-streaming short-circuit (#811): formats a streamed
//! `anchored:N-M` line window from `core::rooted_read` as a tool output.

use super::{
    ReadOutput, count_tokens, format_anchored_output_window, parse_disk_anchor_range,
    read_line_window,
};

/// #811: attempt the disk-streaming short-circuit for a fresh `anchored:N-M`
/// read. `None` when the request isn't eligible (not a windowed anchored
/// read, or a preread is already in hand — nothing to short-circuit) or the
/// fast path can't run cleanly (binary file, invalid UTF-8, I/O error); the
/// caller falls through to the normal full-read path in that case.
pub(crate) fn try_disk_anchored_window(
    path: &str,
    mode: &str,
    fresh: bool,
    preread_is_none: bool,
    file_ref: &str,
    short: &str,
) -> Option<ReadOutput> {
    if !fresh || !preread_is_none {
        return None;
    }
    let range = mode.strip_prefix("anchored:")?;
    let (start, end) = parse_disk_anchor_range(range)?;
    let window = read_line_window(path, start, end)?;
    let (out, _) = format_anchored_output_window(
        file_ref,
        short,
        &window.body,
        window.total_lines,
        Some((window.start, window.end)),
    );
    let out = lean_ctx::core::redaction::redact_text_if_enabled(&out);
    let sent = count_tokens(&out);
    Some(ReadOutput {
        content: out,
        resolved_mode: mode.to_string(),
        output_tokens: sent,
        is_cache_hit: false,
    })
}
