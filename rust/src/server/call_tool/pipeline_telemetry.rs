// SPDX-License-Identifier: Apache-2.0

pub(super) fn record_shell_error_category(
    outcome: Option<&crate::server::tool_trait::ShellOutcome>,
    output: &str,
) {
    if let Some(category) = shell_error_category(outcome, output) {
        if let Some(crate::server::tool_trait::ShellOutcome::Background(background)) = outcome {
            if let Err(error) = crate::server::background_shell::record_error_telemetry_once(
                &background.job_id,
                category,
            ) {
                tracing::debug!(%error, "error telemetry aggregation failed");
            }
        } else {
            record_error_category(category);
        }
    }
}

pub(super) fn shell_error_category(
    outcome: Option<&crate::server::tool_trait::ShellOutcome>,
    output: &str,
) -> Option<crate::core::telemetry_v2::ErrorCategory> {
    use crate::core::telemetry_v2::ErrorCategory;
    use crate::server::tool_trait::ShellOutcome;

    let outcome =
        outcome.filter(|outcome| super::super::outcome::is_shell_error(outcome, output))?;
    match outcome {
        ShellOutcome::Blocked => Some(ErrorCategory::Authorization),
        ShellOutcome::Exit(124) => Some(ErrorCategory::Timeout),
        ShellOutcome::Background(background) if background.exit_code == Some(124) => {
            Some(ErrorCategory::Timeout)
        }
        ShellOutcome::Exit(_) | ShellOutcome::Background(_) => Some(ErrorCategory::Command),
        ShellOutcome::BackgroundLookupError(_) => None,
    }
}

pub(super) fn record_error_category(category: crate::core::telemetry_v2::ErrorCategory) {
    if let Err(error) = crate::core::telemetry_aggregate::record_error_category(category) {
        tracing::debug!(%error, "error telemetry aggregation failed");
    }
}

/// Failure of a finished result, or `None` when it succeeded. The class sees
/// the reason and the text; the template comes from the text the tool wrote.
pub(in crate::server) fn result_failure(
    result: &rmcp::model::CallToolResult,
    reason: &str,
) -> Option<crate::core::telemetry_failure::Failure> {
    (result.is_error == Some(true)).then(|| {
        let text: Vec<&str> = result
            .content
            .iter()
            .filter_map(|block| block.as_text().map(|text| text.text.as_str()))
            .collect();
        let text = text.join("\n");
        crate::core::telemetry_failure::Failure {
            kind: crate::core::telemetry_failure::classify(&format!("{reason} {text}")),
            template: crate::core::failure_template::message_template(if text.trim().is_empty() {
                reason
            } else {
                &text
            }),
        }
    })
}

/// Failure of a protocol error that ended the call.
pub(in crate::server) fn error_failure(
    error: &rmcp::model::ErrorData,
) -> crate::core::telemetry_failure::Failure {
    let mut failure = crate::core::telemetry_failure::Failure::from_message(&error.message);
    if error.code == rmcp::model::ErrorCode::INVALID_PARAMS {
        failure.kind = crate::core::telemetry_failure::FailureKind::InvalidInput;
    }
    failure
}

/// Category of an operation a tool ran and refused (`TOOL_EXECUTION_ERROR`),
/// from the failure class of its message. A refusal of the request (stale
/// edit, bad argument, missing file, size limit, jail or policy) is the
/// caller's input being rejected, not a LeanCTX fault; a timeout keeps its
/// own category; only an unexplained failure or a missing dependency stays
/// `Internal`.
pub(super) fn tool_execution_error_category(
    kind: crate::core::telemetry_failure::FailureKind,
) -> crate::core::telemetry_v2::ErrorCategory {
    use crate::core::telemetry_failure::FailureKind;
    use crate::core::telemetry_v2::ErrorCategory;

    match kind {
        FailureKind::Conflict
        | FailureKind::InvalidInput
        | FailureKind::NotFound
        | FailureKind::Permission
        | FailureKind::PolicyBlocked
        | FailureKind::TooLarge => ErrorCategory::Validation,
        FailureKind::Timeout => ErrorCategory::Timeout,
        FailureKind::Unavailable | FailureKind::Other => ErrorCategory::Internal,
    }
}

pub(super) fn mcp_error_category(
    code: rmcp::model::ErrorCode,
) -> crate::core::telemetry_v2::ErrorCategory {
    if code == rmcp::model::ErrorCode::INVALID_PARAMS {
        crate::core::telemetry_v2::ErrorCategory::Validation
    } else {
        crate::core::telemetry_v2::ErrorCategory::Internal
    }
}
