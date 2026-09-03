use crate::server::background_shell::JobState;
use crate::server::tool_trait::{
    BackgroundDisplay, BackgroundJobState, BackgroundLookupError, BackgroundShellOutcome,
    ShellOutcome,
};

/// Render a `background_action` result.
///
/// #1246: a caller-requested cancel is a success, not a tool failure. The
/// process's own SIGINT exit (130) used to be reported as the tool's exit code,
/// which tripped the client's failure hook and told the agent to fix something
/// it had deliberately done. A cancel therefore never reports a non-zero exit,
/// and is idempotent: cancelling an already-cancelled, already-finished or
/// already-pruned job is equally benign. The first cancel also gets its own
/// wording so it cannot be mistaken for a status poll that did nothing.
pub(super) fn format_background_state(
    id: &str,
    is_cancel: bool,
    state: Option<JobState>,
) -> (String, ShellOutcome) {
    let Some(state) = state else {
        return if is_cancel {
            (
                format!("[background:{id} not found — already finished or cancelled]"),
                ShellOutcome::Exit(0),
            )
        } else {
            (
                format!("[background:{id} not found or expired]"),
                ShellOutcome::BackgroundLookupError(BackgroundLookupError {
                    job_id: id.to_string(),
                    code: "background_job_not_found_or_expired".to_string(),
                    reason: "job not found or retained terminal verdict expired".to_string(),
                }),
            )
        };
    };
    match state {
        JobState::Running { output } => {
            // #1217: show the captured-so-far output so a poll of a
            // long-running job reflects progress instead of a bare
            // "running" with no signal of whether it is advancing.
            let output = redact_shell_output_secrets(&output);
            let captured = summarize_background_output(&output);
            // #1674: the structured verdict has to differ too. The header
            // already said "cancel requested", but `state` stayed `running` —
            // and `state` is what the caller reads. A cancel reply that is
            // byte-indistinguishable from a status poll reads as a no-op, so
            // the caller works around a job that is already gone. `cancel` only
            // sets the flag on a job that is Running, so this arm is exactly
            // the accepted-cancel case; every later observation of this job
            // reports `cancelled`, and so does this one.
            let (head, job_state, summary) = if is_cancel {
                (
                    format!(
                        "[background:{id} cancel requested — job is stopping; poll status for the final output]"
                    ),
                    BackgroundJobState::Cancelled,
                    format!("cancel accepted — job is stopping ({captured})"),
                )
            } else {
                (
                    format!("[background:{id} running]"),
                    BackgroundJobState::Running,
                    captured,
                )
            };
            (
                output.clone(),
                ShellOutcome::Background(BackgroundShellOutcome {
                    state: job_state,
                    exit_code: None,
                    job_id: id.to_string(),
                    archive_id: None,
                    archive_truncated: None,
                    captured_chars: None,
                    archived_chars: None,
                    summary,
                    is_error: false,
                    display: Some(BackgroundDisplay {
                        header: head,
                        footer: None,
                    }),
                }),
            )
        }
        JobState::Completed { output, exit_code } => {
            let output = redact_shell_output_secrets(&output);
            let state = if exit_code == 0 {
                BackgroundJobState::Completed
            } else {
                BackgroundJobState::Failed
            };
            let head = format!("[background:{id} {}, exit {exit_code}]", state.as_str());
            let footer = (exit_code != 0).then(|| format!("[exit:{exit_code}]"));
            (
                output.clone(),
                ShellOutcome::Background(BackgroundShellOutcome {
                    state,
                    exit_code: Some(exit_code),
                    job_id: id.to_string(),
                    archive_id: None,
                    archive_truncated: None,
                    captured_chars: None,
                    archived_chars: None,
                    summary: summarize_background_output(&output),
                    is_error: !is_cancel && exit_code != 0,
                    display: Some(BackgroundDisplay {
                        header: head,
                        footer,
                    }),
                }),
            )
        }
        JobState::Cancelled { output } => {
            let output = redact_shell_output_secrets(&output);
            (
                output.clone(),
                ShellOutcome::Background(BackgroundShellOutcome {
                    state: BackgroundJobState::Cancelled,
                    exit_code: Some(130),
                    job_id: id.to_string(),
                    archive_id: None,
                    archive_truncated: None,
                    captured_chars: None,
                    archived_chars: None,
                    summary: summarize_background_output(&output),
                    is_error: false,
                    display: Some(BackgroundDisplay {
                        header: format!("[background:{id} cancelled, exit 130]"),
                        footer: Some(format!("[cancelled: {id}, exit 130]")),
                    }),
                }),
            )
        }
    }
}

fn summarize_background_output(output: &str) -> String {
    let chars = output.chars().count();
    let lines = output.lines().count();
    let trimmed = output.trim();
    if trimmed.is_empty() {
        "no output".to_string()
    } else if chars <= 512 {
        trimmed.to_string()
    } else {
        format!("{chars} chars, {lines} lines")
    }
}

pub(super) fn redact_shell_output_secrets(output: &str) -> String {
    let output = crate::core::redaction::redact_text_if_enabled(output);
    let cfg = crate::core::config::Config::load();
    if !cfg.secret_detection.enabled {
        return output;
    }
    let (redacted, matches) =
        crate::core::secret_detection::scan_and_redact(&output, &cfg.secret_detection);
    if !matches.is_empty() {
        let names: Vec<&str> = matches.iter().map(|m| m.pattern_name).collect();
        tracing::warn!(
            "[SHELL SECRET REDACTION] {} secret(s) redacted from shell output: {}",
            matches.len(),
            names.join(", ")
        );
    }
    redacted
}

#[cfg(test)]
mod gh1674 {
    use super::*;

    fn running(output: &str) -> Option<JobState> {
        Some(JobState::Running {
            output: output.to_string(),
        })
    }

    fn background(outcome: &ShellOutcome) -> &BackgroundShellOutcome {
        match outcome {
            ShellOutcome::Background(b) => b,
            other => panic!("expected a background outcome, got {other:?}"),
        }
    }

    /// The reported failure: cancelling a running job replied `state: running`,
    /// byte-identical to a status poll, so the cancel read as a no-op and the
    /// caller worked around a job that was already gone.
    #[test]
    fn cancelling_a_running_job_reports_a_terminal_state() {
        let (_, outcome) = format_background_state("shell_abc", true, running(""));
        let b = background(&outcome);

        assert_eq!(b.state, BackgroundJobState::Cancelled);
        assert_ne!(
            b.state,
            BackgroundJobState::Running,
            "a cancel reply must not be indistinguishable from a poll"
        );
        assert!(!b.is_error, "a caller-requested cancel is not a failure");
        assert_eq!(b.exit_code, None);
    }

    /// It must also say more than a poll, not less: `no output` was strictly
    /// less informative than the status call it was mistaken for.
    #[test]
    fn the_cancel_summary_acknowledges_the_cancel() {
        let (_, outcome) = format_background_state("shell_abc", true, running(""));
        assert!(
            background(&outcome).summary.contains("cancel accepted"),
            "{}",
            background(&outcome).summary
        );

        let (_, with_output) =
            format_background_state("shell_abc", true, running("tick 1\ntick 2"));
        let summary = &background(&with_output).summary;
        assert!(summary.contains("cancel accepted"), "{summary}");
        assert!(
            summary.contains("tick 2"),
            "captured output is still reported: {summary}"
        );
    }

    /// A plain poll is untouched.
    #[test]
    fn a_status_poll_still_reports_running() {
        let (_, outcome) = format_background_state("shell_abc", false, running("tick 1"));
        let b = background(&outcome);
        assert_eq!(b.state, BackgroundJobState::Running);
        assert_eq!(b.summary, "tick 1");
    }

    /// Cancelling a job that already finished reports what actually happened,
    /// not a cancellation. `cancel` sets its flag only on a running job.
    #[test]
    fn cancelling_an_already_finished_job_reports_its_real_verdict() {
        let finished = Some(JobState::Completed {
            output: "done".to_string(),
            exit_code: 0,
        });
        let (_, outcome) = format_background_state("shell_abc", true, finished);
        assert_eq!(background(&outcome).state, BackgroundJobState::Completed);
    }
}
