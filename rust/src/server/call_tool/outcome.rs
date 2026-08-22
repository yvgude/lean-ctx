#[allow(clippy::wildcard_imports)]
use super::super::*;

/// Classifies a failed `roots/list` call (GH #694). `-32601 Method not found`
/// means the client declared the roots capability but does not implement the
/// request (Cursor's documented behavior, #699) — retrying can never succeed.
/// Everything else (timeout, transport hiccup while an IDE window is still
/// starting up) is transient and worth a bounded retry on a later tool call.
pub(in crate::server) fn roots_list_failure_is_permanent(e: &rmcp::ServiceError) -> bool {
    matches!(
        e,
        rmcp::ServiceError::McpError(mcp)
            if mcp.code == rmcp::model::ErrorCode::METHOD_NOT_FOUND
    )
}

/// Build the final `CallToolResult`, surfacing shell failures in MCP metadata
/// (GitHub #389): a failing or blocked command sets `isError: true` and a
/// `structuredContent` payload (`{"exitCode": N}` / `{"blocked": true}`), so
/// clients no longer have to regex-parse the `[exit:N]` text footer. The text
/// content is identical in both cases — only the metadata changes. Managed
/// background jobs always carry structured state, code and a short summary;
/// ordinary non-zero exits that are *not* failures (#1090/#1086) still carry
/// neither, because some clients render `structuredContent` in place of text
/// (#1127).
pub(in crate::server) fn finalize_call_result(
    result_text: &str,
    shell_outcome: Option<crate::server::tool_trait::ShellOutcome>,
) -> CallToolResult {
    let mut result = CallToolResult::success(vec![ContentBlock::text(result_text.to_owned())]);
    if let Some(outcome) = shell_outcome {
        if matches!(
            outcome,
            crate::server::tool_trait::ShellOutcome::Background(_)
                | crate::server::tool_trait::ShellOutcome::BackgroundLookupError(_)
        ) {
            result.structured_content = outcome.structured();
            if outcome.is_error() {
                result.is_error = Some(true);
            }
        } else if is_shell_error(&outcome, result_text) {
            // #1127: clients that understand `structuredContent` render it *instead
            // of* the text block on a non-error result, so attaching it to a benign
            // non-zero exit (exit 1 with output per #1090, exit 124 with partial
            // output per #1086) hid the command output completely — `ls /nope` came
            // back as a bare `{"exitCode":1}`. Only errors, whose text the client
            // surfaces separately, carry the structured payload.
            result.is_error = Some(true);
            result.structured_content = outcome.structured();
        }
    }
    result
}

/// #1090: exit 1 with non-empty stdout is not an error for grep/diff/test.
/// #1086/#1089: exit 124 (timeout) with captured partial output is returned
/// as a success with a timeout marker, not as an error. The partial output is
/// often enough to answer the question; treating it as a failure causes the
/// client to retry the entire pipeline.
fn is_shell_error(outcome: &crate::server::tool_trait::ShellOutcome, output: &str) -> bool {
    match outcome {
        crate::server::tool_trait::ShellOutcome::Exit(0) => false,
        crate::server::tool_trait::ShellOutcome::Exit(1) => {
            output.trim().is_empty() || output.trim().starts_with("[exit:")
        }
        crate::server::tool_trait::ShellOutcome::Exit(124) => {
            // Timeout with partial output: return as success so the client
            // sees the captured data. The ERROR marker in the text body still
            // signals the timeout to the agent.
            crate::server::execute::output_before_timeout_marker(output).is_none_or(str::is_empty)
        }
        crate::server::tool_trait::ShellOutcome::Exit(_)
        | crate::server::tool_trait::ShellOutcome::Blocked
        | crate::server::tool_trait::ShellOutcome::BackgroundLookupError(_) => true,
        crate::server::tool_trait::ShellOutcome::Background(outcome) => outcome.is_error,
    }
}
