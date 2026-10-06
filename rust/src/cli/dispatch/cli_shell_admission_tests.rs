// SPDX-License-Identifier: Apache-2.0

use super::{cli_shell_completion, execute_cli_shell};
use crate::core::execution_lifecycle::{LIFECYCLE_STAGE_ORDER, LifecycleStage, StageDisposition};
use crate::{core, shell};
use lean_ctx_protocol::AcceptanceState;

struct RestoreEnvironment(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl Drop for RestoreEnvironment {
    fn drop(&mut self) {
        for (key, value) in &self.0 {
            if let Some(value) = value {
                crate::test_env::set_var(key, value);
            } else {
                crate::test_env::remove_var(key);
            }
        }
    }
}

fn with_shell_environment(test: impl FnOnce()) {
    let _lock = core::data_dir::test_env_lock();
    let keys = [
        "LEAN_CTX_ACTIVE",
        "LEAN_CTX_WRAPPED",
        "LEAN_CTX_COMPRESS",
        "LEAN_CTX_SHELL",
        "LEAN_CTX_DISABLED",
        "LEAN_CTX_ALLOWLIST_WARN_ONLY",
        "LEAN_CTX_HOOK_CHILD",
        "LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE",
    ];
    let _restore = RestoreEnvironment(
        keys.into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect(),
    );
    for key in keys {
        crate::test_env::remove_var(key);
    }
    // Exercise this adapter, not an inherited agent shell wrapper or profile.
    crate::test_env::set_var("LEAN_CTX_SHELL", "/bin/sh");
    crate::test_env::set_var("LEAN_CTX_HOOK_CHILD", "1");
    crate::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", "git");
    test();
}

#[test]
fn denied_or_empty_argv_never_creates_a_lifecycle() {
    with_shell_environment(|| {
        let before = core::task_spine::TaskSpine::current().map(|task| task.task_id);
        assert!(matches!(
            execute_cli_shell("exec", "xxd", None, false),
            Err(126)
        ));
        assert!(matches!(
            execute_cli_shell("track", "", Some(&[]), false),
            Err(127)
        ));
        assert!(matches!(
            execute_cli_shell("track", "", None, false),
            Err(126)
        ));
        assert_eq!(
            core::task_spine::TaskSpine::current().map(|task| task.task_id),
            before
        );
    });
}

#[test]
fn actual_child_statuses_finish_once_without_acceptance_evidence() {
    with_shell_environment(|| {
        for (binary, expected) in [("true", 0), ("false", 1), ("__cli_missing_binary__", 127)] {
            crate::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", binary);
            let args = vec![binary.to_owned()];
            let (observation, guard) = execute_cli_shell("track", binary, Some(&args), false)
                .unwrap_or_else(|code| panic!("unexpected admission rejection: {code}"));
            assert_eq!(observation.exit_code, expected);
            assert!(observation.child_dispatch_attempted);
            let guard = guard.expect("admitted child owns lifecycle");
            let context = guard.context();
            let outcome = context.outcome().expect("completed lifecycle");
            assert_eq!(outcome.accepted_outcome.accepted, AcceptanceState::Unknown);
            assert!(outcome.assessment.is_none());
            assert_eq!(context.completed_stages(), LIFECYCLE_STAGE_ORDER);
            let stages = context.stage_executions();
            for stage in [
                LifecycleStage::ReversiblePostProcess,
                LifecycleStage::RecordContextIr,
                LifecycleStage::RecordLedger,
                LifecycleStage::RecordEvidence,
            ] {
                assert!(stages.iter().any(|entry| entry.stage == stage
                    && matches!(entry.disposition, StageDisposition::Skipped(_))));
            }
            for stage in [
                LifecycleStage::DispatchPrimitive,
                LifecycleStage::FlushState,
            ] {
                assert!(
                    stages.iter().any(|entry| entry.stage == stage
                        && entry.disposition == StageDisposition::Applied)
                );
            }
            let duplicate = guard.complete(cli_shell_completion(expected));
            assert_eq!(duplicate.accepted_outcome, outcome.accepted_outcome);
            assert_eq!(context.stage_executions(), stages);
            let completion = cli_shell_completion(expected);
            assert_eq!(completion.provider, "local");
            assert_eq!(completion.success, expected == 0);
            assert!(completion.outcome_signals.is_empty());
        }
    });
}

#[test]
fn direct_arguments_and_reentry_keep_their_execution_semantics() {
    with_shell_environment(|| {
        let value = "a b; '$HOME' \"quoted\"";
        let args = vec![
            "test".to_owned(),
            value.to_owned(),
            "=".to_owned(),
            value.to_owned(),
        ];
        crate::test_env::set_var("LEAN_CTX_ACTIVE", "1");
        let before = core::task_spine::TaskSpine::current().map(|task| task.task_id);
        let (observation, guard) =
            execute_cli_shell("track", &shell::join_command(&args), Some(&args), true)
                .unwrap_or_else(|code| panic!("unexpected admission rejection: {code}"));
        assert_eq!(observation.exit_code, 0);
        assert!(guard.is_none());
        assert_eq!(
            core::task_spine::TaskSpine::current().map(|task| task.task_id),
            before
        );
    });
}

#[test]
fn single_shell_input_executes_through_the_shared_adapter() {
    with_shell_environment(|| {
        assert!(!crate::shell::reentry::is_wrapped());
        assert_eq!(crate::shell::platform::shell_and_flag().0, "/bin/sh");
        let (observation, guard) = execute_cli_shell("track", "true", None, false)
            .unwrap_or_else(|code| panic!("unexpected admission rejection: {code}"));
        assert_eq!(observation.exit_code, 0);
        assert!(guard.is_some());
    });
}
