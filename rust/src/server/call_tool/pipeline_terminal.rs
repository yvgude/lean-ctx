// SPDX-License-Identifier: Apache-2.0

//! Terminal results under an active policy: a tool error is still model-visible
//! output, so it passes the same result guard as a successful reply.

use super::{CallToolResult, ContentBlock, McpProcessed, policy_guard};

pub(super) fn policy_blocked_output(reason: &'static str) -> McpProcessed {
    McpProcessed {
        result: CallToolResult::error(vec![ContentBlock::text(reason)]),
        ir: None,
        ledger: None,
        receipt: None,
        checkpoint: None,
    }
}

pub(super) fn protect_terminal_result(name: &str, result: CallToolResult) -> CallToolResult {
    if !crate::core::policy::runtime::is_active() {
        return result;
    }
    let mut blocks = Vec::with_capacity(result.content.len());
    for block in result.content {
        let Some(text) = block.as_text() else {
            return policy_blocked_output("[POLICY BLOCKED] Uninspectable tool error.").result;
        };
        match policy_guard::protect_result(name, &text.text) {
            Ok(text) => blocks.push(ContentBlock::text(text)),
            Err(reason) => return policy_blocked_output(reason).result,
        }
    }
    // Terminal errors have no supported structured payload; do not echo an
    // unchecked parallel representation through structuredContent or metadata.
    CallToolResult::error(blocks)
}
