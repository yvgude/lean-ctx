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
        ShellOutcome::Exit(_) | ShellOutcome::Background(_) => Some(ErrorCategory::Internal),
        ShellOutcome::BackgroundLookupError(_) => None,
    }
}

pub(super) fn record_error_category(category: crate::core::telemetry_v2::ErrorCategory) {
    if let Err(error) = crate::core::telemetry_aggregate::record_error_category(category) {
        tracing::debug!(%error, "error telemetry aggregation failed");
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
