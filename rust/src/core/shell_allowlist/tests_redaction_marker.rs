// SPDX-License-Identifier: Apache-2.0
use super::{check_shell_allowlist, refuse_redaction_marker};

const COPIED: &str = "sed -i 's#url=.*#url=postgres://[REDACTED:Database credentials]@db:5432/hive#' \
                      /opt/hive/.env";

/// 24.10.: an agent copied a marker out of its redacted view into a `sed`;
/// the marker landed in a remote config. It never runs, in any mode.
#[test]
fn a_command_with_a_redaction_marker_never_runs() {
    let _lock = crate::core::data_dir::test_env_lock();
    let mut outcomes = Vec::new();
    for mode in ["enforce", "warn", "off"] {
        crate::test_env::set_var("LEAN_CTX_SHELL_SECURITY", mode);
        outcomes.push((mode, check_shell_allowlist(COPIED)));
    }
    crate::test_env::remove_var("LEAN_CTX_SHELL_SECURITY");
    for (mode, outcome) in outcomes {
        let refused = outcome.expect_err(mode);
        assert!(
            refused.contains("[REDACTED:Database credentials]") && refused.contains("[BLOCKED]"),
            "{mode}: {}",
            refused.message()
        );
    }
}

#[test]
fn only_a_complete_marker_is_refused() {
    assert!(refuse_redaction_marker("grep -c REDACTED app.log").is_ok());
    assert!(refuse_redaction_marker("grep -c '\\[REDACTED' app.log").is_ok());
    assert!(refuse_redaction_marker("echo [REDACTED:]").is_ok());
    assert!(refuse_redaction_marker("curl -H 'x: [REDACTED:GitHub token]' u").is_err());
}

#[test]
fn ctx_execute_refuses_a_marker_in_every_language() {
    let code = "print('postgres://[REDACTED:Database credentials]@db/hive')";
    let (text, outcome) = crate::tools::ctx_execute::handle_in("python", code, None, None, None);
    assert!(
        matches!(outcome, crate::server::tool_trait::ShellOutcome::Blocked),
        "{text}"
    );
    let batch = [("python".to_string(), code.to_string())];
    let (text, outcome) = crate::tools::ctx_execute::handle_batch(&batch);
    assert!(
        matches!(outcome, crate::server::tool_trait::ShellOutcome::Blocked),
        "{text}"
    );
}
