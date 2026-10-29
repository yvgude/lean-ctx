// SPDX-License-Identifier: Apache-2.0

/// Record real compression savings after every response rewrite is complete.
///
/// Registered tools report the raw-to-tool-output delta in `tool_saved_tokens`.
/// The final output count includes dispatcher-level terse compression and
/// decorations. This is deliberately best-effort: tracker failures cannot
/// affect a completed tool response.
pub(super) fn record_compression_savings(
    name: &str,
    tool_saved_tokens: usize,
    output_tokens: usize,
) {
    if let Some((raw_tokens, compressed_tokens)) =
        compression_tracker_tokens(name, tool_saved_tokens, output_tokens)
    {
        crate::core::savings_tracker::record_compression(raw_tokens, compressed_tokens, name);
    }
}

pub(super) fn compression_tracker_tokens(
    name: &str,
    tool_saved_tokens: usize,
    output_tokens: usize,
) -> Option<(u64, u64)> {
    matches!(
        name,
        "ctx_read" | "ctx_shell" | "ctx_search" | "ctx_compose"
    )
    .then(|| {
        let raw_tokens = tool_saved_tokens.saturating_add(output_tokens) as u64;
        (raw_tokens, output_tokens as u64)
    })
}
