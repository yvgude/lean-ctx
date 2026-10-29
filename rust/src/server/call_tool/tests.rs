#[allow(clippy::wildcard_imports)]
use super::super::*;
use super::{finalize_call_result, roots_list_failure_is_permanent};

/// Community regression (3.9.19): every documented lossless escape hatch was
/// ignored — triage ran after the compression pipeline, so per-call
/// parameters never reached it. These pins guarantee the bypass contract
/// stays wired at the dispatch chokepoint.
mod rules_self_heal_status_tests {
    use super::super::outcome::append_rules_self_heal_status;

    #[test]
    fn failed_rule_write_must_not_claim_auto_updated() {
        let failed = crate::rules_inject::InjectResult {
            errors: vec!["Cursor: simulated write failure".to_string()],
            ..Default::default()
        };

        let result = append_rules_self_heal_status("tool output".to_string(), Some(&failed));

        assert!(
            !result.contains("[RULES AUTO-UPDATED]"),
            "a failed rules write must never be reported as successfully refreshed, got: {result}"
        );
    }

    #[test]
    fn successful_rule_write_claims_auto_updated() {
        let updated = crate::rules_inject::InjectResult {
            updated: vec!["Cursor".to_string()],
            ..Default::default()
        };

        let result = append_rules_self_heal_status("tool output".to_string(), Some(&updated));

        assert!(
            result.contains("[RULES AUTO-UPDATED]"),
            "a successful rules rewrite must retain the existing success notice, got: {result}"
        );
    }

    #[test]
    fn already_current_rules_do_not_claim_auto_updated() {
        let already = crate::rules_inject::InjectResult {
            already: vec!["Cursor".to_string()],
            ..Default::default()
        };

        let result = append_rules_self_heal_status("tool output".to_string(), Some(&already));

        assert!(
            !result.contains("[RULES AUTO-UPDATED]"),
            "AlreadyPresent is not a write and must not be described as one, got: {result}"
        );
    }

    #[test]
    fn missing_heal_result_does_not_claim_auto_updated() {
        let result = append_rules_self_heal_status("tool output".to_string(), None);

        assert!(
            !result.contains("[RULES AUTO-UPDATED]"),
            "missing home or a failed blocking task must not be reported as a successful rewrite, got: {result}"
        );
    }

    #[test]
    fn partial_rule_write_failure_must_not_claim_auto_updated() {
        let heal_result = crate::rules_inject::InjectResult {
            updated: vec!["Cursor".to_string()],
            errors: vec!["OpenCode: simulated write failure".to_string()],
            ..Default::default()
        };

        let output =
            append_rules_self_heal_status("original result".to_string(), Some(&heal_result));

        assert_eq!(output, "original result");
        assert!(!output.contains("[RULES AUTO-UPDATED]"));
    }
}
mod triage_bypass_tests {
    use super::super::pipeline::triage_bypass_requested;

    fn args(json: &serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        json.as_object().expect("test args are an object").clone()
    }

    #[test]
    fn ctx_read_is_always_exempt() {
        // ctx_read delivers file content agents edit against — and its cache
        // marks content as fully delivered BEFORE the pipeline runs, so any
        // post-hoc filtering silently corrupts the dedup contract ("full
        // content already in this conversation" after a stripped delivery).
        assert!(triage_bypass_requested("ctx_read", None));
        assert!(triage_bypass_requested(
            "ctx_read",
            Some(&args(&serde_json::json!({"path": "src/identity.rs"})))
        ));
    }

    #[test]
    fn documented_lossless_escape_hatches_bypass_triage() {
        for a in [
            serde_json::json!({"raw": true}),
            serde_json::json!({"mode": "raw"}),
            serde_json::json!({"mode": "full"}),
            serde_json::json!({"mode": "full-compact"}),
            serde_json::json!({"mode": "lines:32-33"}),
            serde_json::json!({"mode": "anchored:1-10"}),
            serde_json::json!({"mode": "diff"}),
            serde_json::json!({"aggressiveness": 0.0}),
            serde_json::json!({"fresh": true}),
        ] {
            assert!(
                triage_bypass_requested("ctx_shell", Some(&args(&a))),
                "escape hatch must bypass triage: {a}"
            );
        }
    }

    #[test]
    fn plain_calls_remain_subject_to_the_level_gate() {
        assert!(!triage_bypass_requested(
            "ctx_shell",
            Some(&args(&serde_json::json!({"command": "git status"})))
        ));
        assert!(!triage_bypass_requested("ctx_search", None));
    }
}

mod shell_outcome_tests {
    use super::*;
    // GH #2005; lives in `call_tool/tests/shell_outcome_tests/`.
    #[cfg(not(windows))]
    mod expand_unpolled;
    #[cfg(not(windows))]
    use crate::server::call_tool::dispatch_and_post_process;
    use crate::server::tool_trait::{McpTool, ShellOutcome, ToolContext};

    #[cfg(not(windows))]
    struct ScopedEnvVar {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    #[cfg(not(windows))]
    impl ScopedEnvVar {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = std::env::var_os(key);
            crate::test_env::set_var(key, value);
            Self { key, previous }
        }
    }

    #[cfg(not(windows))]
    impl Drop for ScopedEnvVar {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                crate::test_env::set_var(self.key, previous);
            } else {
                crate::test_env::remove_var(self.key);
            }
        }
    }

    #[cfg(not(windows))]
    struct ScopedPolicy {
        _override: crate::core::policy::runtime::TestPolicyOverride,
    }

    #[cfg(not(windows))]
    impl ScopedPolicy {
        fn redacting(label: &str, pattern: &str) -> Self {
            let mut redaction = std::collections::BTreeMap::new();
            redaction.insert(label.to_string(), pattern.to_string());
            Self {
                _override: crate::core::policy::runtime::TestPolicyOverride::set(Some(
                    crate::core::policy::ResolvedPolicy {
                        name: "mes1609-test-policy".into(),
                        version: "1.0.0".into(),
                        description: "MES-1609 archive chokepoint regression".into(),
                        chain: vec![],
                        default_read_mode: None,
                        allow_tools: None,
                        deny_tools: vec![],
                        max_context_tokens: None,
                        audit_retention_days: None,
                        redaction,
                        filters: crate::core::policy::FilterRules {
                            pii: Some("redact".into()),
                            ..crate::core::policy::FilterRules::default()
                        },
                        egress: crate::core::policy::EgressRules::default(),
                        routing: crate::core::policy::RoutingPolicyRules::default(),
                        budgets: crate::core::policy::BudgetRules::default(),
                    },
                )),
            }
        }
    }

    /// A child that is still running whenever a test looks at it, however
    /// slow the runner. Tests asserting a `running` or `cancelled` state used
    /// `sleep 2`; under coverage instrumentation the status probe took longer
    /// and saw the job completed (PR #2100). The child never has to finish:
    /// [`BackgroundJobGuard`] cancels it — a process-group kill — when the
    /// test ends, and the timeout is only a backstop above the sleep.
    #[cfg(not(windows))]
    const HELD_OPEN_COMMAND: &str = "sleep 600";
    #[cfg(not(windows))]
    const HELD_OPEN_TIMEOUT_MS: u64 = 900_000;

    #[cfg(not(windows))]
    struct BackgroundJobGuard {
        job_id: String,
    }

    #[cfg(not(windows))]
    impl BackgroundJobGuard {
        fn new(job_id: String) -> Self {
            Self { job_id }
        }
    }

    #[cfg(not(windows))]
    impl Drop for BackgroundJobGuard {
        fn drop(&mut self) {
            let _ = crate::server::background_shell::cancel(&self.job_id);
            for _ in 0..120 {
                if !matches!(
                    crate::server::background_shell::status(&self.job_id),
                    Some(crate::server::background_shell::JobState::Running { .. })
                ) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            crate::server::background_shell::remove_for_test(&self.job_id);
        }
    }

    /// Wait until a background job has completed, and say why when it has not.
    ///
    /// The callers used to poll for a bare 6–10 s and then assert
    /// `matches!(status, Some(Completed))`. On a loaded ubuntu runner three
    /// trivial `printf | yes | head` jobs missed that window one after the
    /// other (PR #1995), and the assertion could not tell a slow job from one
    /// that was cancelled or pruned away. The ceiling only costs time when the
    /// job really is stuck; the panic names the state that was observed.
    #[cfg(not(windows))]
    fn wait_for_completed(job_id: &str) {
        use crate::server::background_shell::JobState;
        const CEILING: std::time::Duration = std::time::Duration::from_mins(1);
        let deadline = std::time::Instant::now() + CEILING;
        loop {
            let state = crate::server::background_shell::status(job_id);
            match &state {
                Some(JobState::Completed { .. }) => return,
                Some(JobState::Cancelled { output }) => {
                    panic!("job {job_id} was cancelled instead of completing: {output:?}")
                }
                _ if std::time::Instant::now() >= deadline => {
                    let observed = match state {
                        None => "gone from the job table (removed or pruned)".to_string(),
                        Some(JobState::Running { output }) => {
                            let tail: String = output.chars().rev().take(200).collect();
                            let tail: String = tail.chars().rev().collect();
                            format!(
                                "still running, {} bytes captured, tail {tail:?}",
                                output.len()
                            )
                        }
                        Some(other) => format!("{other:?}"),
                    };
                    panic!("job {job_id} did not complete within {CEILING:?}: {observed}");
                }
                _ => std::thread::sleep(std::time::Duration::from_millis(25)),
            }
        }
    }

    fn text_of(result: &CallToolResult) -> String {
        result
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.clone())
            .unwrap_or_default()
    }

    #[cfg(not(windows))]
    fn launched_job_guard(result: &CallToolResult) -> BackgroundJobGuard {
        let structured_id = result
            .structured_content
            .as_ref()
            .and_then(|value| value["jobId"].as_str());
        let text = text_of(result);
        let text_id = text
            .strip_prefix('[')
            .and_then(|value| value.split_once(':'))
            .and_then(|(_, value)| value.split_whitespace().next());
        BackgroundJobGuard::new(
            structured_id
                .or(text_id)
                .expect("background launch must expose a job id")
                .to_string(),
        )
    }

    /// Drive a command through the auto-detach path *deterministically*.
    ///
    /// The 10 ms foreground window used to be held open by the caller's own
    /// `sleep 0.1`, which assumes the child is spawned, scheduled and still
    /// running 10 ms later. On a loaded macOS runner that assumption broke and
    /// the helper panicked with "the reproduction must exercise the
    /// auto-detached path" — a flake in the harness, not a defect in the code
    /// under test. The child now blocks on a barrier file until the test
    /// releases it, exactly like `auto_detached_pipeline_result` below, so the
    /// detach is guaranteed no matter how the runner schedules us.
    #[cfg(not(windows))]
    fn auto_detached_result(command: &str) -> CallToolResult {
        let release_dir = tempfile::tempdir().expect("create auto-detach barrier directory");
        let release_path = release_dir.path().join("release");
        let mut extra_env = std::collections::HashMap::new();
        extra_env.insert(
            "LCTX_TEST_RELEASE_PATH".to_string(),
            release_path.to_string_lossy().into_owned(),
        );
        let guarded_command =
            format!("while [ ! -e \"$LCTX_TEST_RELEASE_PATH\" ]; do sleep 0.01; done; {command}");
        let detached = crate::server::background_shell::run_foreground_or_detach(
            guarded_command,
            ".".to_string(),
            extra_env,
            Some(10_000),
            std::time::Duration::from_millis(10),
            None,
            None,
        );
        let job_id = match detached {
            crate::server::background_shell::ForegroundResult::Detached { job_id } => job_id,
            // Diagnostic on purpose: if this ever fires on CI we need to see
            // *why* the child never reached the soft cap. A failed spawn or a
            // missing `sleep` shows up here as an exit code and a shell error
            // rather than as an unexplained timing story.
            crate::server::background_shell::ForegroundResult::Finished { output, exit_code } => {
                panic!(
                    "the reproduction must exercise the auto-detached path; the child \
                     finished inside the soft cap instead (exit {exit_code}): {output}"
                )
            }
        };
        let _job = BackgroundJobGuard::new(job_id.clone());
        std::fs::write(&release_path, b"release").expect("release auto-detached child");

        let mut terminal = false;
        for _ in 0..240 {
            if matches!(
                crate::server::background_shell::status(&job_id),
                Some(
                    crate::server::background_shell::JobState::Completed { .. }
                        | crate::server::background_shell::JobState::Cancelled { .. }
                )
            ) {
                terminal = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert!(
            terminal,
            "auto-detached child did not reach a terminal state"
        );

        let mut args = serde_json::Map::new();
        args.insert("background_action".to_string(), serde_json::json!("status"));
        args.insert("job_id".to_string(), serde_json::json!(job_id));
        let output = crate::tools::registered::ctx_shell::CtxShellTool
            .handle(&args, &ToolContext::default())
            .expect("status must return a tool result");
        finalize_call_result(&output.text, output.shell_outcome)
    }

    #[cfg(not(windows))]
    async fn auto_detached_pipeline_result(command: &str) -> (CallToolResult, BackgroundJobGuard) {
        let release_dir = tempfile::tempdir().expect("create auto-detach barrier directory");
        let release_path = release_dir.path().join("release");
        let mut extra_env = std::collections::HashMap::new();
        extra_env.insert(
            "LCTX_TEST_RELEASE_PATH".to_string(),
            release_path.to_string_lossy().into_owned(),
        );
        let guarded_command =
            format!("while [ ! -e \"$LCTX_TEST_RELEASE_PATH\" ]; do sleep 0.01; done; {command}");
        let detached = crate::server::background_shell::run_foreground_or_detach(
            guarded_command,
            ".".to_string(),
            extra_env,
            Some(30_000),
            std::time::Duration::from_millis(10),
            None,
            None,
        );
        let job_id = match detached {
            crate::server::background_shell::ForegroundResult::Detached { job_id } => job_id,
            // Diagnostic on purpose: if this ever fires on CI we need to see
            // *why* the child never reached the soft cap. A failed spawn or a
            // missing `sleep` shows up here as an exit code and a shell error
            // rather than as an unexplained timing story.
            crate::server::background_shell::ForegroundResult::Finished { output, exit_code } => {
                panic!(
                    "the reproduction must exercise the auto-detached path; the child \
                     finished inside the soft cap instead (exit {exit_code}): {output}"
                )
            }
        };
        let job = BackgroundJobGuard::new(job_id.clone());
        std::fs::write(release_path, b"release").expect("release auto-detached child");
        wait_for_completed(&job_id);
        (
            pipeline_background_status(&job_id, false, false, false).await,
            job,
        )
    }

    #[cfg(not(windows))]
    fn structured_of(result: &CallToolResult) -> &serde_json::Value {
        result
            .structured_content
            .as_ref()
            .expect("background status must expose structuredContent")
    }

    #[cfg(not(windows))]
    fn auto_detached_running_result() -> CallToolResult {
        let detached = crate::server::background_shell::run_foreground_or_detach(
            format!("{HELD_OPEN_COMMAND} # MES1609_RUNNING_STATUS"),
            ".".to_string(),
            std::collections::HashMap::default(),
            Some(HELD_OPEN_TIMEOUT_MS),
            std::time::Duration::from_millis(10),
            None,
            None,
        );
        let job_id = match detached {
            crate::server::background_shell::ForegroundResult::Detached { job_id } => job_id,
            // Diagnostic on purpose: if this ever fires on CI we need to see
            // *why* the child never reached the soft cap. A failed spawn or a
            // missing `sleep` shows up here as an exit code and a shell error
            // rather than as an unexplained timing story.
            crate::server::background_shell::ForegroundResult::Finished { output, exit_code } => {
                panic!(
                    "the reproduction must exercise the auto-detached path; the child \
                     finished inside the soft cap instead (exit {exit_code}): {output}"
                )
            }
        };
        let _job = BackgroundJobGuard::new(job_id.clone());

        let mut args = serde_json::Map::new();
        args.insert("background_action".to_string(), serde_json::json!("status"));
        args.insert("job_id".to_string(), serde_json::json!(job_id));
        let output = crate::tools::registered::ctx_shell::CtxShellTool
            .handle(&args, &ToolContext::default())
            .expect("status must return a tool result");
        finalize_call_result(&output.text, output.shell_outcome)
    }

    #[cfg(not(windows))]
    fn shell_context() -> ToolContext {
        // #1778: a compile-time constant, not the process cwd. These tests only
        // need *some* real directory, but `std::env::current_dir()` is
        // process-global and other tests mutate it via `set_current_dir`, so
        // reading it here raced with them under `cargo test`'s parallelism.
        let cwd = env!("CARGO_MANIFEST_DIR").to_string();
        let mut session = crate::core::session::SessionState::new();
        session.project_root = Some(cwd.clone());
        session.shell_cwd = Some(cwd.clone());
        ToolContext {
            project_root: cwd,
            session: Some(std::sync::Arc::new(tokio::sync::RwLock::new(session))),
            ..ToolContext::default()
        }
    }

    #[cfg(not(windows))]
    fn completed_background_job(command: &str) -> (String, BackgroundJobGuard) {
        let job_id = crate::server::background_shell::start(
            command.to_string(),
            ".".to_string(),
            std::collections::HashMap::default(),
            Some(30_000),
        );
        let job = BackgroundJobGuard::new(job_id.clone());
        wait_for_completed(&job_id);
        (job_id, job)
    }

    #[cfg(not(windows))]
    async fn pipeline_background_status(
        job_id: &str,
        minimal: bool,
        raw: bool,
        inline: bool,
    ) -> CallToolResult {
        // #1778: compile-time constant, not the process-global cwd (see
        // `shell_context` above).
        let root = env!("CARGO_MANIFEST_DIR").to_string();
        let server = crate::tools::LeanCtxServer::new_with_project_root(Some(&root));
        let mut args = serde_json::Map::new();
        args.insert("background_action".to_string(), serde_json::json!("status"));
        args.insert("job_id".to_string(), serde_json::json!(job_id));
        if raw {
            args.insert("raw".to_string(), serde_json::json!(true));
        }
        if inline {
            args.insert("inline".to_string(), serde_json::json!(true));
        }
        dispatch_and_post_process(
            &server,
            "ctx_shell",
            Some(&args),
            minimal,
            crate::core::config::Config::load_arc(),
            false,
            None,
            None,
            format!("mes1609_pipeline_{job_id}"),
            None,
        )
        .await
        .expect("background status pipeline must succeed")
    }

    fn call_shell(
        args: &serde_json::Map<String, serde_json::Value>,
        ctx: &ToolContext,
    ) -> CallToolResult {
        let output = crate::tools::registered::ctx_shell::CtxShellTool
            .handle(args, ctx)
            .expect("ctx_shell must return a tool result");
        finalize_call_result(&output.text, output.shell_outcome)
    }

    #[test]
    fn success_exit_is_not_an_error() {
        let r = finalize_call_result("ok", Some(ShellOutcome::Exit(0)));
        assert_ne!(r.is_error, Some(true), "exit 0 must not set isError");
        assert!(
            r.structured_content.is_none(),
            "happy path stays token-neutral: no structuredContent on exit 0"
        );
        assert_eq!(text_of(&r), "ok");
    }

    #[test]
    fn nonzero_exit_with_output_is_not_error() {
        // #1090: exit 1 with command output (before [exit:] footer) is NOT
        // a tool error — grep/diff/test exit 1 with results is normal.
        let r = finalize_call_result("boom\n[exit:1]", Some(ShellOutcome::Exit(1)));
        assert_ne!(
            r.is_error,
            Some(true),
            "exit 1 with output must NOT set isError (#1090)"
        );
        // #1127: and it must carry no structuredContent either — clients that
        // prefer it would render `{"exitCode":1}` and drop the output.
        assert!(
            r.structured_content.is_none(),
            "benign exit 1 must not shadow its output with structuredContent (#1127)"
        );
        assert_eq!(text_of(&r), "boom\n[exit:1]", "output text is preserved");
    }

    #[test]
    fn stderr_only_failure_keeps_its_text() {
        // #1127: `ls /nonexistent` writes only to stderr and exits 1. The text
        // block is the sole carrier of the diagnostic, so it must survive.
        let text = "ls: /nonexistent-path-xyz: No such file or directory\n[exit:1]";
        let r = finalize_call_result(text, Some(ShellOutcome::Exit(1)));
        assert_ne!(r.is_error, Some(true));
        assert!(r.structured_content.is_none());
        assert_eq!(text_of(&r), text);
    }

    #[test]
    fn timeout_with_partial_output_keeps_its_text() {
        // #1086/#1127: same shape for exit 124 — partial output is a success
        // result, so structuredContent must not displace it.
        let text = "line one\nERROR: command timed out after 5000ms";
        let r = finalize_call_result(text, Some(ShellOutcome::Exit(124)));
        assert_ne!(r.is_error, Some(true));
        assert!(r.structured_content.is_none());
        assert_eq!(text_of(&r), text);
    }

    #[test]
    fn exit_1_without_output_is_error() {
        // Exit 1 with only the [exit:] footer (no command output) IS an error.
        let r = finalize_call_result("[exit:1]", Some(ShellOutcome::Exit(1)));
        assert_eq!(
            r.is_error,
            Some(true),
            "exit 1 with no command output must set isError"
        );
    }

    /// MES-1609: once a foreground command auto-detaches, its terminal
    /// failure is a job verdict, not grep-like output that may use exit 1 as
    /// data. The MCP result must therefore keep both state and exit code.
    #[test]
    #[cfg(not(windows))]
    fn auto_detached_exit_1_is_a_structured_failed_job() {
        let result = auto_detached_result("sleep 0.1; printf TAP_FAILURE; exit 1");

        assert_eq!(result.is_error, Some(true));
        let structured = structured_of(&result);
        assert_eq!(structured["state"], serde_json::json!("failed"));
        assert_eq!(structured["exitCode"], serde_json::json!(1));
        assert!(structured["jobId"].as_str().is_some());
        assert!(
            structured["summary"]
                .as_str()
                .is_some_and(|s| s.contains("TAP_FAILURE"))
        );
        assert!(text_of(&result).contains("TAP_FAILURE"));
    }

    #[test]
    #[cfg(not(windows))]
    fn auto_detached_exit_7_is_a_structured_failed_job() {
        let result = auto_detached_result("sleep 0.1; exit 7");

        assert_eq!(result.is_error, Some(true));
        let structured = structured_of(&result);
        assert_eq!(structured["state"], serde_json::json!("failed"));
        assert_eq!(structured["exitCode"], serde_json::json!(7));
    }

    #[test]
    #[cfg(not(windows))]
    fn auto_detached_running_job_has_no_exit_code() {
        let result = auto_detached_running_result();

        assert_ne!(result.is_error, Some(true));
        let structured = structured_of(&result);
        assert_eq!(structured["state"], serde_json::json!("running"));
        assert!(structured["jobId"].as_str().is_some());
        assert!(structured.get("exitCode").is_none());
    }

    #[test]
    #[cfg(not(windows))]
    fn auto_detached_cancel_returns_cancelled_exit_130_without_error() {
        let detached = crate::server::background_shell::run_foreground_or_detach(
            format!("{HELD_OPEN_COMMAND} # MES1609_CANCEL_STATUS"),
            ".".to_string(),
            std::collections::HashMap::default(),
            Some(HELD_OPEN_TIMEOUT_MS),
            std::time::Duration::from_millis(10),
            None,
            None,
        );
        let job_id = match detached {
            crate::server::background_shell::ForegroundResult::Detached { job_id } => job_id,
            // Diagnostic on purpose: if this ever fires on CI we need to see
            // *why* the child never reached the soft cap. A failed spawn or a
            // missing `sleep` shows up here as an exit code and a shell error
            // rather than as an unexplained timing story.
            crate::server::background_shell::ForegroundResult::Finished { output, exit_code } => {
                panic!(
                    "the reproduction must exercise the auto-detached path; the child \
                     finished inside the soft cap instead (exit {exit_code}): {output}"
                )
            }
        };
        let _job = BackgroundJobGuard::new(job_id.clone());

        let mut args = serde_json::Map::new();
        args.insert("background_action".to_string(), serde_json::json!("cancel"));
        args.insert("job_id".to_string(), serde_json::json!(job_id));
        let output = crate::tools::registered::ctx_shell::CtxShellTool
            .handle(&args, &ToolContext::default())
            .expect("cancel must return a tool result");
        let cancel_result = finalize_call_result(&output.text, output.shell_outcome);

        assert_ne!(cancel_result.is_error, Some(true));
        let mut cancelled = false;
        for _ in 0..80 {
            if matches!(
                crate::server::background_shell::status(&job_id),
                Some(crate::server::background_shell::JobState::Cancelled { .. })
            ) {
                cancelled = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert!(cancelled, "cancelled job did not reach a terminal state");

        args.insert("background_action".to_string(), serde_json::json!("status"));
        let output = crate::tools::registered::ctx_shell::CtxShellTool
            .handle(&args, &ToolContext::default())
            .expect("status must return a tool result");
        let result = finalize_call_result(&output.text, output.shell_outcome);

        assert_ne!(result.is_error, Some(true));
        let structured = structured_of(&result);
        assert_eq!(structured["state"], serde_json::json!("cancelled"));
        assert_eq!(structured["exitCode"], serde_json::json!(130));
    }

    #[test]
    fn missing_cancel_is_a_success_ack_without_a_fabricated_terminal_state() {
        let mut args = serde_json::Map::new();
        args.insert("background_action".to_string(), serde_json::json!("cancel"));
        args.insert(
            "job_id".to_string(),
            serde_json::json!("shell_mes1609_missing_cancel"),
        );

        let result = call_shell(&args, &ToolContext::default());

        assert_ne!(result.is_error, Some(true));
        assert!(
            result.structured_content.is_none(),
            "an unknown terminal must not be fabricated: {:?}",
            result.structured_content
        );
        assert!(text_of(&result).contains("already finished or cancelled"));
    }

    #[test]
    fn missing_status_is_a_lookup_error_without_a_fabricated_lifecycle_state() {
        let mut args = serde_json::Map::new();
        args.insert("background_action".to_string(), serde_json::json!("status"));
        args.insert(
            "job_id".to_string(),
            serde_json::json!("shell_mes1609_missing_status"),
        );

        let result = call_shell(&args, &ToolContext::default());

        assert_eq!(result.is_error, Some(true));
        let structured = result
            .structured_content
            .as_ref()
            .expect("missing status must expose a structured lookup error");
        assert_eq!(
            structured["jobId"],
            serde_json::json!("shell_mes1609_missing_status")
        );
        assert_eq!(
            structured["errorCode"],
            serde_json::json!("background_job_not_found_or_expired")
        );
        assert!(structured["reason"].as_str().is_some());
        assert!(structured.get("state").is_none());
        assert!(structured.get("exitCode").is_none());
        assert!(text_of(&result).contains("not found or expired"));
    }

    #[tokio::test(flavor = "multi_thread")]
    #[cfg(not(windows))]
    async fn explicit_background_launch_ack_is_structured_running() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let _archive = ScopedEnvVar::set("LEAN_CTX_ARCHIVE", "1");
        let _threshold = ScopedEnvVar::set("LEAN_CTX_ARCHIVE_THRESHOLD", "4096");
        let mut args = serde_json::Map::new();
        args.insert(
            "command".to_string(),
            serde_json::json!("sleep 0.1; printf MES1727_EXPLICIT_OUTPUT"),
        );
        args.insert("run_in_background".to_string(), serde_json::json!(true));

        let result = call_shell(&args, &shell_context());

        let job = launched_job_guard(&result);
        assert_ne!(result.is_error, Some(true));
        let structured = structured_of(&result);
        assert_eq!(structured["state"], serde_json::json!("running"));
        assert!(structured["jobId"].as_str().is_some());
        assert!(structured.get("exitCode").is_none());

        wait_for_completed(&job.job_id);
        let terminal = pipeline_background_status(&job.job_id, false, false, false).await;
        let archive_id = structured_of(&terminal)["archiveId"]
            .as_str()
            .expect("short explicit background output must be archived");
        assert_ne!(archive_id, job.job_id);
        let expanded = crate::tools::ctx_expand::handle(&serde_json::json!({"id": &job.job_id}));
        assert!(expanded.contains(&format!("Archive {archive_id}")));
        assert!(expanded.contains("MES1727_EXPLICIT_OUTPUT"));

        let repeated = pipeline_background_status(&job.job_id, false, false, false).await;
        assert_eq!(
            structured_of(&repeated)["archiveId"],
            serde_json::json!(archive_id)
        );
    }

    // #1980: with `[cache] shell_cache_enabled = true` (since removed), a
    // repeated state-observing command replayed its first output after the
    // file it reads had changed. Every call must observe the workspace as is.
    #[tokio::test(flavor = "multi_thread")]
    #[cfg(not(windows))]
    async fn repeated_state_observing_command_sees_the_current_file() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let config = crate::core::config::Config::path().expect("isolated config path");
        std::fs::create_dir_all(config.parent().expect("config dir")).unwrap();
        std::fs::write(&config, "[cache]\nshell_cache_enabled = true\n").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("probe.txt");
        let mut args = serde_json::Map::new();
        args.insert(
            "command".to_string(),
            serde_json::json!(format!("wc -c {}", file.display())),
        );
        let byte_count = |content: &str| {
            std::fs::write(&file, content).unwrap();
            let result = call_shell(&args, &shell_context());
            text_of(&result)
                .split_whitespace()
                .next()
                .map(str::to_owned)
        };

        assert_eq!(byte_count("a").as_deref(), Some("1"));
        assert_eq!(byte_count("abcdef").as_deref(), Some("6"));
    }

    #[tokio::test(flavor = "multi_thread")]
    #[cfg(not(windows))]
    async fn soft_cap_auto_detach_ack_is_structured_running() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let _archive = ScopedEnvVar::set("LEAN_CTX_ARCHIVE", "1");
        let _soft_cap = ScopedEnvVar::set("LEAN_CTX_SHELL_FG_CAP_MS", "10");
        let mut args = serde_json::Map::new();
        args.insert(
            "command".to_string(),
            serde_json::json!(format!("{HELD_OPEN_COMMAND} # MES1609_SOFT_CAP_LAUNCH")),
        );

        let result = call_shell(&args, &shell_context());

        let _job = launched_job_guard(&result);
        assert_ne!(result.is_error, Some(true));
        let structured = structured_of(&result);
        assert_eq!(structured["state"], serde_json::json!("running"));
        assert!(structured["jobId"].as_str().is_some());
        assert!(structured.get("exitCode").is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    #[cfg(not(windows))]
    async fn running_background_status_is_not_archived() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let _archive = ScopedEnvVar::set("LEAN_CTX_ARCHIVE", "1");
        let job_id = crate::server::background_shell::start(
            HELD_OPEN_COMMAND.to_string(),
            ".".to_string(),
            std::collections::HashMap::default(),
            Some(HELD_OPEN_TIMEOUT_MS),
        );
        let _job = BackgroundJobGuard::new(job_id.clone());

        let result = pipeline_background_status(&job_id, false, false, false).await;

        assert_eq!(
            structured_of(&result)["state"],
            serde_json::json!("running")
        );
        assert!(structured_of(&result).get("archiveId").is_none());
        assert!(crate::core::archive::list_entries(None).is_empty());
    }

    /// MES-1609: a successful auto-detached child remains queryable after
    /// the OS process exits and exposes an explicit terminal code 0.
    #[test]
    #[cfg(not(windows))]
    fn auto_detached_exit_0_remains_queryable_as_completed() {
        let result = auto_detached_result("sleep 0.1; printf LONG_OK");

        assert_ne!(result.is_error, Some(true));
        let structured = structured_of(&result);
        assert_eq!(structured["state"], serde_json::json!("completed"));
        assert_eq!(structured["exitCode"], serde_json::json!(0));
        assert!(structured["jobId"].as_str().is_some());
        assert!(
            structured["summary"]
                .as_str()
                .is_some_and(|s| s.contains("LONG_OK"))
        );
        assert!(text_of(&result).contains("LONG_OK"));
    }

    #[tokio::test(flavor = "multi_thread")]
    #[cfg(not(windows))]
    async fn auto_detached_short_output_is_recoverable_by_job_id() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let _archive = ScopedEnvVar::set("LEAN_CTX_ARCHIVE", "1");
        let _threshold = ScopedEnvVar::set("LEAN_CTX_ARCHIVE_THRESHOLD", "4096");
        let (result, job) = auto_detached_pipeline_result("printf MES1727_AUTO_OUTPUT").await;

        let structured = structured_of(&result);
        let archive_id = structured["archiveId"]
            .as_str()
            .expect("short auto-detached output must be archived");
        assert_ne!(archive_id, job.job_id);
        let expanded = crate::tools::ctx_expand::handle(&serde_json::json!({"id": &job.job_id}));
        assert!(expanded.contains(&format!("Archive {archive_id}")));
        assert!(expanded.contains("MES1727_AUTO_OUTPUT"));
    }

    /// MES-1609: terminal output too large for an inline response is archived
    /// before generic filtering can erase both the verdict and retrieval id.
    #[tokio::test(flavor = "multi_thread")]
    #[cfg(not(windows))]
    async fn auto_detached_large_output_keeps_summary_and_archive_id_inline() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let _archive = ScopedEnvVar::set("LEAN_CTX_ARCHIVE", "1");
        let (result, _job) = auto_detached_pipeline_result("sleep 0.1; seq 1 50000").await;

        assert_ne!(result.is_error, Some(true));
        let structured = structured_of(&result);
        assert_eq!(structured["state"], serde_json::json!("completed"));
        assert_eq!(structured["exitCode"], serde_json::json!(0));
        let archive_id = structured["archiveId"]
            .as_str()
            .expect("large terminal output must expose its archive id");
        assert!(
            structured["summary"]
                .as_str()
                .is_some_and(|s| s.contains("50000 lines"))
        );
        let text = text_of(&result);
        assert!(text.contains(&format!("ctx_expand(id=\"{archive_id}\")")));
        assert!(text.len() < 20_000, "large output was returned inline");
        let archived = crate::core::archive::retrieve(archive_id)
            .expect("archive id must resolve to the terminal output");
        assert!(archived.starts_with("1\n2\n"));
        assert!(archived.ends_with("49999\n50000\n"));
    }

    #[tokio::test(flavor = "multi_thread")]
    #[cfg(not(windows))]
    async fn auto_detached_private_key_block_is_fully_redacted_before_archive() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let _archive = ScopedEnvVar::set("LEAN_CTX_ARCHIVE", "1");
        let begin_marker = ["-----BEGIN ", "PRIVATE", " KEY-----"].concat();
        let end_marker = ["-----END ", "PRIVATE", " KEY-----"].concat();
        let command = format!(
            "sleep 0.1; printf '%s\\n' '{begin_marker}'; \
             yes MES1609_FAKE_KEY_BODY_0123456789 | head -n 2000; \
             printf '%s\\n' '{end_marker}'; \
             yes MES1609_SAFE_OUTPUT | head -n 2000"
        );
        let (result, _job) = auto_detached_pipeline_result(&command).await;

        let archive_id = structured_of(&result)["archiveId"]
            .as_str()
            .expect("large redacted output must be archived");
        let archived = crate::core::archive::retrieve(archive_id)
            .expect("archive id must resolve to redacted output");
        assert!(!archived.contains("BEGIN PRIVATE KEY"));
        assert!(!archived.contains("END PRIVATE KEY"));
        assert!(!archived.contains("MES1609_FAKE_KEY_BODY_0123456789"));
        assert!(archived.contains("[REDACTED:Private key block]"));
        assert!(
            archived.contains("MES1609_SAFE_OUTPUT"),
            "safe text outside the detected key block should remain available"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    #[cfg(not(windows))]
    async fn oversized_background_archive_reports_truncation_and_exact_sizes() {
        const ARCHIVE_LIMIT: usize = crate::core::archive::MAX_ARCHIVE_SIZE;
        const CAPTURED: usize = ARCHIVE_LIMIT + 4096;
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let _archive = ScopedEnvVar::set("LEAN_CTX_ARCHIVE", "1");
        let _shell_cap = ScopedEnvVar::set("LCTX_MAX_SHELL_BYTES", (CAPTURED + 1024).to_string());
        let (result, _job) = auto_detached_pipeline_result(&format!(
            "sleep 0.1; head -c {CAPTURED} /dev/zero | tr '\\0' X"
        ))
        .await;

        let structured = structured_of(&result);
        assert_eq!(structured["state"], serde_json::json!("completed"));
        assert_eq!(structured["exitCode"], serde_json::json!(0));
        assert_eq!(structured["archiveTruncated"], serde_json::json!(true));
        assert_eq!(structured["capturedChars"], serde_json::json!(CAPTURED));
        assert_eq!(
            structured["archivedChars"],
            serde_json::json!(ARCHIVE_LIMIT)
        );
        let archive_id = structured["archiveId"]
            .as_str()
            .expect("oversized output must expose archiveId");
        let archived = crate::core::archive::retrieve(archive_id)
            .expect("truncated archive must remain retrievable");
        assert_eq!(archived.len(), ARCHIVE_LIMIT);
        assert!(text_of(&result).contains("archive truncated"));
    }

    #[tokio::test(flavor = "multi_thread")]
    #[cfg(not(windows))]
    async fn background_status_pipeline_keeps_single_archive_and_structured_metadata() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let _archive = ScopedEnvVar::set("LEAN_CTX_ARCHIVE", "1");
        let _threshold = ScopedEnvVar::set("LEAN_CTX_ARCHIVE_THRESHOLD", "1");
        let _references = ScopedEnvVar::set("LEAN_CTX_REFERENCE_RESULTS", "1");
        let job_id = crate::server::background_shell::start(
            "sleep 0.1; printf MES1609_PIPELINE; head -c 50000 /dev/zero | tr '\\000' P"
                .to_string(),
            ".".to_string(),
            std::collections::HashMap::default(),
            Some(10_000),
        );
        let _job = BackgroundJobGuard::new(job_id.clone());
        wait_for_completed(&job_id);

        // #1778: compile-time constant, not the process-global cwd (see
        // `shell_context` above).
        let root = env!("CARGO_MANIFEST_DIR").to_string();
        let server = crate::tools::LeanCtxServer::new_with_project_root(Some(&root));
        let mut args = serde_json::Map::new();
        args.insert("background_action".to_string(), serde_json::json!("status"));
        args.insert("job_id".to_string(), serde_json::json!(job_id));
        let result = dispatch_and_post_process(
            &server,
            "ctx_shell",
            Some(&args),
            false,
            crate::core::config::Config::load_arc(),
            false,
            None,
            None,
            "mes1609_pipeline".to_string(),
            None,
        )
        .await
        .expect("background status pipeline must succeed");

        let structured = structured_of(&result);
        assert_eq!(structured["state"], serde_json::json!("completed"));
        assert_eq!(structured["exitCode"], serde_json::json!(0));
        let archive_id = structured["archiveId"]
            .as_str()
            .expect("pipeline must retain archiveId");
        assert_eq!(crate::core::archive::list_entries(None).len(), 1);
        assert_eq!(
            text_of(&result)
                .matches(&format!("ctx_expand(id=\"{archive_id}\")"))
                .count(),
            1
        );
    }

    /// AP05-G2: the shared-context hint decorated every tool result with the
    /// newest facts of one installation-wide store, whatever the project.
    #[tokio::test(flavor = "multi_thread")]
    async fn shared_context_hint_carries_only_the_request_projects_facts() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let own = tempfile::tempdir().unwrap();
        let foreign = tempfile::tempdir().unwrap();
        for (dir, fact) in [(&own, "own-hint-marker"), (&foreign, "foreign-hint-marker")] {
            crate::core::shared_context::SharedContext::for_project(dir.path().to_str().unwrap())
                .unwrap()
                .put(fact, "codex", "fact")
                .unwrap();
        }
        let root = own.path().to_string_lossy().into_owned();
        let server = crate::tools::LeanCtxServer::new_with_project_root(Some(&root));
        let mut args = serde_json::Map::new();
        args.insert("path".to_string(), serde_json::json!(root));
        let result = crate::server::call_tool::dispatch_and_post_process(
            &server,
            "ctx_tree",
            Some(&args),
            false,
            crate::core::config::Config::load_arc(),
            false,
            None,
            None,
            "ap05_hint".to_string(),
            None,
        )
        .await
        .expect("tree pipeline");
        let text = text_of(&result);
        assert!(text.contains("own-hint-marker"), "{text}");
        assert!(!text.contains("foreign-hint-marker"), "{text}");
    }

    #[tokio::test(flavor = "multi_thread")]
    #[cfg(not(windows))]
    async fn background_status_triage_filters_display_after_preserving_complete_archive() {
        const KEEP: &str = "MES1609_TRIAGE_KEEP";
        const DROP: &str = "MES1609_TRIAGE_DROP";
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let _archive = ScopedEnvVar::set("LEAN_CTX_ARCHIVE", "1");
        let _archive_threshold = ScopedEnvVar::set("LEAN_CTX_ARCHIVE_THRESHOLD", "1");
        let _firewall_threshold = ScopedEnvVar::set("LEAN_CTX_EPHEMERAL_MIN_TOKENS", "100000");
        let (job_id, _job) = completed_background_job(&format!(
            "printf 'coding coding_fix {KEEP}\\n'; yes '// unrelated {DROP}' | head -n 80"
        ));

        // #1778: compile-time constant, not the process-global cwd (see
        // `shell_context` above).
        let root = env!("CARGO_MANIFEST_DIR").to_string();
        let server = crate::tools::LeanCtxServer::new_with_project_root(Some(&root));
        let session_id = server.session.read().await.id.clone();
        let context = crate::core::decision_loop_runtime::DecisionLoopRuntime::get_or_init()
            .on_tool_start(
                "ctx_shell",
                Some("fix a coding bug in one file"),
                &session_id,
                "mes1609-triage-test",
            );
        let profile = crate::core::decision_loop_runtime::DecisionLoopRuntime::get_or_init()
            .profile_for_session(&session_id)
            .expect("task profile must be remembered for the pipeline session");
        assert!(crate::server::context_gate::triage_filter_level(&profile) > 0);

        let mut args = serde_json::Map::new();
        args.insert("background_action".to_string(), serde_json::json!("status"));
        args.insert("job_id".to_string(), serde_json::json!(job_id));
        // Output filtering is opt-in since decision_loop.max_filter_level
        // landed (default 0 = off); this test exercises the filtered-display
        // path, so it enables the level explicitly on its own config.
        let config = {
            let mut config = crate::core::config::Config::load_arc().as_ref().clone();
            config.decision_loop.max_filter_level = 2;
            std::sync::Arc::new(config)
        };
        let result = dispatch_and_post_process(
            &server,
            "ctx_shell",
            Some(&args),
            false,
            config,
            false,
            None,
            None,
            "mes1609_triage_pipeline".to_string(),
            Some(context),
        )
        .await
        .expect("background status pipeline must succeed");

        let structured = structured_of(&result);
        let archive_id = structured["archiveId"]
            .as_str()
            .expect("normal background status must retain its complete archive");
        let archived = crate::core::archive::retrieve(archive_id)
            .expect("structured archiveId must remain retrievable");
        assert!(archived.contains(KEEP));
        assert!(archived.contains(DROP));
        let displayed = text_of(&result);
        assert!(displayed.contains(KEEP));
        assert!(!displayed.contains(DROP));
        assert!(!displayed.contains("ctx_expand"));
    }

    #[tokio::test(flavor = "multi_thread")]
    #[cfg(not(windows))]
    async fn unbound_background_recovery_is_withheld_after_policy_and_input_filters() {
        const POLICY_PATTERN: &str = "MES1609_POLICY_PII_7F3A9C2E";
        const TEST_CARD: &str = "4111111111111111";
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let _archive = ScopedEnvVar::set("LEAN_CTX_ARCHIVE", "1");
        let _threshold = ScopedEnvVar::set("LEAN_CTX_ARCHIVE_THRESHOLD", "1");
        let _policy = ScopedPolicy::redacting("mes1609_policy", POLICY_PATTERN);
        let (job_id, _job) = completed_background_job(&format!(
            "printf '%s\\n%s\\n' '{POLICY_PATTERN}' '{TEST_CARD}'; \
             yes MES1609_POLICY_SAFE | head -n 3000"
        ));

        let result = pipeline_background_status(&job_id, false, false, false).await;

        let structured = structured_of(&result);
        assert!(structured.get("archiveId").is_none());
        assert!(structured.get("archiveTruncated").is_none());
        assert!(structured.get("archivedChars").is_none());
        assert!(structured["capturedChars"].as_u64().unwrap() > 0);
        assert!(
            structured["summary"]
                .as_str()
                .unwrap()
                .contains("archive unavailable")
        );
        assert!(crate::core::archive::list_entries(None).is_empty());
        let displayed = text_of(&result);
        assert!(!displayed.contains(POLICY_PATTERN));
        assert!(!displayed.contains(TEST_CARD));
        assert!(displayed.contains("[REDACTED:mes1609_policy]"));
        assert!(displayed.contains("[REDACTED:card]"));
        assert!(
            crate::core::audit_trail::load_recent(10)
                .iter()
                .any(|entry| {
                    entry.tool == "ctx_shell"
                        && entry.role == "mes1609-test-policy"
                        && matches!(
                            entry.event_type,
                            crate::core::audit_trail::AuditEventType::SecretDetected
                        )
                })
        );
        assert!(
            result.meta.is_some(),
            "finalize metadata must still be present"
        );
    }

    #[test]
    #[cfg(not(windows))]
    fn scoped_policy_overrides_serialize_parallel_mutation() {
        // The deliberately active global overrides below must not leak into
        // other tests that hold isolated data/config state but use live policy.
        // Each override takes the environment lock itself, on its own thread
        // and for its whole lifetime; this thread holds none while it waits,
        // or the first child would wait for the lock its parent keeps.
        let (first_ready_tx, first_ready_rx) = std::sync::mpsc::channel();
        let (release_first_tx, release_first_rx) = std::sync::mpsc::channel();
        let first = std::thread::spawn(move || {
            let _policy = ScopedPolicy::redacting("mes1609_first", "MES1609_FIRST");
            first_ready_tx.send(()).expect("signal first override");
            release_first_rx.recv().expect("release first override");
        });
        // Taking the override means taking the process-wide environment lock,
        // which any other test may hold for as long as it runs: this wait has
        // no bound of its own and is no oracle. A sender that died is an error.
        first_ready_rx
            .recv()
            .expect("first override must become active");

        let (second_started_tx, second_started_rx) = std::sync::mpsc::channel();
        let (second_acquired_tx, second_acquired_rx) = std::sync::mpsc::channel();
        let (release_second_tx, release_second_rx) = std::sync::mpsc::channel();
        let second = std::thread::spawn(move || {
            second_started_tx.send(()).expect("signal second thread");
            let _policy = ScopedPolicy::redacting("mes1609_second", "MES1609_SECOND");
            second_acquired_tx.send(()).expect("signal second override");
            release_second_rx.recv().expect("release second override");
        });
        // Controlled readiness: the second thread only has to start, which
        // waits for no lock.
        second_started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("second thread must start");
        // The oracle: while the first override is provably held, the second
        // must not be acquired.
        let overlapped = second_acquired_rx
            .recv_timeout(std::time::Duration::from_millis(500))
            .is_ok();

        release_first_tx.send(()).expect("release first thread");
        first.join().expect("first policy thread");
        if !overlapped {
            // A lock wait again: other tests may be queued for the same
            // environment lock ahead of the second thread.
            second_acquired_rx
                .recv()
                .expect("second override must acquire after first drops");
        }
        release_second_tx.send(()).expect("release second thread");
        second.join().expect("second policy thread");

        assert!(!overlapped, "policy test overrides must not overlap");
    }

    #[tokio::test(flavor = "multi_thread")]
    #[cfg(not(windows))]
    async fn raw_background_status_keeps_secured_output_inline_without_digest() {
        const RAW_SIZE: usize = 50_000;
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let _archive = ScopedEnvVar::set("LEAN_CTX_ARCHIVE", "1");
        let _threshold = ScopedEnvVar::set("LEAN_CTX_ARCHIVE_THRESHOLD", "1");
        let _turn_limit = ScopedEnvVar::set("LEAN_CTX_TURN_FRESH_LIMIT", "0");
        let (job_id, _job) = completed_background_job(&format!(
            "printf MES1609_RAW_HEAD; head -c {RAW_SIZE} /dev/zero | tr '\\000' R; printf MES1609_RAW_TAIL"
        ));

        let result = pipeline_background_status(&job_id, false, true, false).await;

        let text = text_of(&result);
        assert!(text.len() >= RAW_SIZE);
        assert!(text.contains("MES1609_RAW_HEAD"));
        assert!(text.contains("MES1609_RAW_TAIL"));
        assert!(!text.contains("ctx_expand"));
        assert!(!text.contains("[Archived:"));
        assert!(structured_of(&result)["archiveId"].as_str().is_some());
        assert_eq!(crate::core::archive::list_entries(None).len(), 1);
    }

    // #1540: the `minimal_overhead` CONFIG key (default true) must not disable
    // the archive/firewall safety net — a large background delivery still
    // becomes a lossless digest with drilldown.
    #[tokio::test(flavor = "multi_thread")]
    #[cfg(not(windows))]
    async fn config_minimal_still_archives_and_firewalls_background_status() {
        const MINIMAL_SIZE: usize = 50_000;
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let _archive = ScopedEnvVar::set("LEAN_CTX_ARCHIVE", "1");
        let _threshold = ScopedEnvVar::set("LEAN_CTX_ARCHIVE_THRESHOLD", "1");
        let (job_id, _job) = completed_background_job(&format!(
            "printf MES1609_MINIMAL_HEAD; head -c {MINIMAL_SIZE} /dev/zero | tr '\\000' M; printf MES1609_MINIMAL_TAIL"
        ));

        let result = pipeline_background_status(&job_id, true, false, false).await;

        assert_eq!(crate::core::archive::list_entries(None).len(), 1);
        let text = text_of(&result);
        assert!(text.contains("ctx_expand"), "digest must offer drilldown");
        assert!(
            text.len() < MINIMAL_SIZE,
            "delivery must be the digest, not the body"
        );
    }

    // #1540: only the LEAN_CTX_MINIMAL env escape hatch (the documented
    // do-not-touch-anything switch since the 3.9.19 incident) skips the
    // archive/firewall entirely.
    #[tokio::test(flavor = "multi_thread")]
    #[cfg(not(windows))]
    async fn env_minimal_escape_hatch_skips_archive_and_digest() {
        const MINIMAL_SIZE: usize = 50_000;
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let _archive = ScopedEnvVar::set("LEAN_CTX_ARCHIVE", "1");
        let _threshold = ScopedEnvVar::set("LEAN_CTX_ARCHIVE_THRESHOLD", "1");
        let _hatch = ScopedEnvVar::set("LEAN_CTX_MINIMAL", "1");
        let (job_id, _job) = completed_background_job(&format!(
            "printf MES1609_HATCH_HEAD; head -c {MINIMAL_SIZE} /dev/zero | tr '\\000' M; printf MES1609_HATCH_TAIL"
        ));

        let result = pipeline_background_status(&job_id, true, false, false).await;

        let structured = structured_of(&result);
        assert!(structured.get("archiveId").is_none());
        assert_eq!(crate::core::archive::list_entries(None).len(), 0);
        let text = text_of(&result);
        assert!(!text.contains("ctx_expand"));
        assert!(!text.contains("[Archived:"));
    }

    #[tokio::test(flavor = "multi_thread")]
    #[cfg(not(windows))]
    async fn inline_background_status_under_limit_stays_inline_without_digest() {
        const INLINE_SIZE: usize = 5_000;
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let _archive = ScopedEnvVar::set("LEAN_CTX_ARCHIVE", "1");
        let _threshold = ScopedEnvVar::set("LEAN_CTX_ARCHIVE_THRESHOLD", "1");
        let _turn_limit = ScopedEnvVar::set("LEAN_CTX_TURN_FRESH_LIMIT", "0");
        let (job_id, _job) = completed_background_job(&format!(
            "printf MES1609_INLINE_HEAD; head -c {INLINE_SIZE} /dev/zero | tr '\\000' I; printf MES1609_INLINE_TAIL"
        ));

        let result = pipeline_background_status(&job_id, false, false, true).await;

        let text = text_of(&result);
        assert!(text.len() >= INLINE_SIZE);
        assert!(text.contains("MES1609_INLINE_HEAD"));
        assert!(text.contains("MES1609_INLINE_TAIL"));
        assert!(!text.contains("ctx_expand"));
        assert!(!text.contains("[Archived:"));
        assert!(structured_of(&result)["archiveId"].as_str().is_some());
        assert_eq!(crate::core::archive::list_entries(None).len(), 1);
    }

    // #1541: inline=true is honored up to the context-window cap; above it the
    // delivery becomes a lossless digest with drilldown instead of flooding
    // the caller's context. Explicit raw stays verbatim at any size.
    #[tokio::test(flavor = "multi_thread")]
    #[cfg(not(windows))]
    async fn inline_background_status_above_verbatim_cap_is_digested() {
        const CAP_SIZE: usize = 20_000;
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let _archive = ScopedEnvVar::set("LEAN_CTX_ARCHIVE", "1");
        let _threshold = ScopedEnvVar::set("LEAN_CTX_ARCHIVE_THRESHOLD", "1");
        let _cap = ScopedEnvVar::set("LEAN_CTX_VERBATIM_MAX_TOKENS", "1000");
        let _turn_limit = ScopedEnvVar::set("LEAN_CTX_TURN_FRESH_LIMIT", "0");
        let (job_id, _job) = completed_background_job(&format!(
            "printf MES1609_CAP_HEAD; head -c {CAP_SIZE} /dev/zero | tr '\\000' C; printf MES1609_CAP_TAIL"
        ));

        let result = pipeline_background_status(&job_id, false, false, true).await;

        let text = text_of(&result);
        assert!(
            text.len() < CAP_SIZE,
            "above the cap the delivery is a digest, got {} bytes",
            text.len()
        );
        assert!(text.contains("ctx_expand"), "digest must offer drilldown");
        assert_eq!(crate::core::archive::list_entries(None).len(), 1);
    }

    #[test]
    #[cfg(not(windows))]
    fn background_status_handler_does_not_write_archives_before_pipeline() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let _archive = ScopedEnvVar::set("LEAN_CTX_ARCHIVE", "1");
        let _threshold = ScopedEnvVar::set("LEAN_CTX_ARCHIVE_THRESHOLD", "1");
        let (job_id, _job) = completed_background_job(
            "printf MES1609_HANDLER_ONLY; head -c 50000 /dev/zero | tr '\\000' H",
        );
        let mut args = serde_json::Map::new();
        args.insert("background_action".to_string(), serde_json::json!("status"));
        args.insert("job_id".to_string(), serde_json::json!(job_id));

        let output = crate::tools::registered::ctx_shell::CtxShellTool
            .handle(&args, &ToolContext::default())
            .expect("status handler must succeed without pipeline I/O");

        assert_eq!(crate::core::archive::list_entries(None).len(), 0);
        assert!(
            output
                .shell_outcome
                .and_then(|outcome| outcome.structured())
                .is_some_and(|structured| structured.get("archiveId").is_none())
        );
    }

    #[test]
    fn exit_2_is_always_error() {
        let r = finalize_call_result("error output", Some(ShellOutcome::Exit(2)));
        assert_eq!(
            r.is_error,
            Some(true),
            "exit >= 2 must always set isError (#389)"
        );
        assert_eq!(
            r.structured_content,
            Some(serde_json::json!({ "exitCode": 2 })),
            "guards must be able to read exitCode without text parsing"
        );
    }

    #[test]
    fn negative_exit_codes_are_reported() {
        // Signal terminations are mapped to negative/128+n codes by execute();
        // whatever the value, non-zero must surface as an error.
        let r = finalize_call_result("killed", Some(ShellOutcome::Exit(-1)));
        assert_eq!(r.is_error, Some(true));
        assert_eq!(
            r.structured_content,
            Some(serde_json::json!({ "exitCode": -1 }))
        );
    }

    #[test]
    fn blocked_command_sets_is_error_and_blocked_marker() {
        let r = finalize_call_result("[BLOCKED] nope", Some(ShellOutcome::Blocked));
        assert_eq!(
            r.is_error,
            Some(true),
            "blocked commands never ran — that is a failure"
        );
        assert_eq!(
            r.structured_content,
            Some(serde_json::json!({ "blocked": true }))
        );
    }

    #[test]
    fn non_shell_tools_are_unaffected() {
        let r = finalize_call_result("file contents", None);
        assert_ne!(r.is_error, Some(true));
        assert!(r.structured_content.is_none());
    }

    /// A real registered file-output tool must pass through the model-bound
    /// policy redaction before its MCP result is returned.
    #[tokio::test(flavor = "multi_thread")]
    #[cfg(not(windows))]
    async fn registered_file_output_is_policy_redacted_before_mcp_reply() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let root = tempfile::tempdir().unwrap();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        // Unique per run, and letters only: as dash-separated digits the value
        // sometimes passed the card checksum, and the PII detector masked it
        // before the policy rule under test could match.
        let nonce_text = nonce
            .to_string()
            .bytes()
            .map(|digit| char::from(b"qwrtypsdfg"[usize::from(digit - b'0')]).to_string())
            .collect::<Vec<_>>()
            .join("-");
        let secret = format!("cobalt badger egress marker {nonce_text} private note");
        assert!(
            !secret.bytes().any(|byte| byte.is_ascii_digit()),
            "no card, AHV or phone shape without a digit: {secret}"
        );
        let (unmasked, pii_hits) = crate::core::input_filters::pii::redact_checksummed(&secret);
        assert!(pii_hits.is_empty() && unmasked == secret, "{unmasked}");
        assert_eq!(
            crate::core::redaction::redact_text(&secret),
            secret,
            "no built-in secret shape either"
        );
        std::fs::write(root.path().join("probe.txt"), &secret).unwrap();
        let _policy = ScopedPolicy::redacting("cq06_runtime_probe", &regex::escape(&secret));

        let server = crate::tools::LeanCtxServer::new_with_project_root(Some(
            root.path().to_str().expect("temporary root is UTF-8"),
        ));
        *server.presence_agent_id.write().await = Some("cq06-egress-test".to_owned());
        let args = serde_json::Map::from_iter([
            ("path".to_owned(), serde_json::json!("probe.txt")),
            ("mode".to_owned(), serde_json::json!("full")),
        ]);
        let request = rmcp::model::CallToolRequestParams::new("ctx_read").with_arguments(args);

        let result = server
            .call_tool_guarded(request)
            .await
            .expect("registered ctx_read call must succeed");
        let visible = result
            .content
            .iter()
            .filter_map(|block| block.as_text())
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            !visible.contains(&secret),
            "model-visible MCP result leaked runtime secret: {visible}"
        );
        assert!(
            visible.contains("[REDACTED:cq06_runtime_probe]"),
            "policy redaction marker missing from model-visible result: {visible}"
        );
    }

    /// Security invariant across the registry and dispatch files: every
    /// registered McpTool is invoked only by the one generic dispatcher that
    /// the MCP handler sends through dispatch_and_post_process. The sole
    /// exemption is this cfg(test)-only file, whose direct calls test handlers
    /// in isolation and never create model-visible MCP replies.
    #[test]
    fn registered_tool_dispatch_has_one_outbound_pipeline_entry() {
        // Test-only fixtures that call a handler directly. `task_lineage_tests`
        // wraps ctx_read in a rendezvous tool that is itself dispatched through
        // the one production entry, so it adds no second outbound path.
        const TEST_ONLY_EXEMPTIONS: &[&str] = &[
            "call_tool/tests.rs",
            "call_tool/guarded/task_lineage_tests.rs",
        ];

        let server_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/server");
        for exemption in TEST_ONLY_EXEMPTIONS {
            let path = server_dir.join(exemption);
            assert!(path.is_file(), "stale egress exemption: {exemption}");
            let source = std::fs::read_to_string(path).expect("read test-only exemption");
            assert!(
                source.contains(".handle("),
                "stale egress exemption no longer contains isolated direct handler tests: {exemption}"
            );
        }

        let mut direct_handler_calls = Vec::new();
        for entry in walkdir::WalkDir::new(&server_dir)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "rs"))
        {
            let relative = entry
                .path()
                .strip_prefix(&server_dir)
                .expect("entry is under server source")
                .to_string_lossy()
                // Windows yields `call_tool\tests.rs`; compare in one separator.
                .replace('\\', "/");
            if TEST_ONLY_EXEMPTIONS.contains(&relative.as_str()) {
                continue;
            }
            let source = std::fs::read_to_string(entry.path()).expect("read server source");
            for (line, text) in source.lines().enumerate() {
                if text.contains(".handle(") {
                    direct_handler_calls.push((relative.clone(), line + 1, text.trim().to_owned()));
                }
            }
        }

        assert_eq!(
            direct_handler_calls.len(),
            1,
            "registered McpTool handlers must have one production invocation site: {direct_handler_calls:?}"
        );
        assert_eq!(direct_handler_calls[0].0, "dispatch/mod.rs");
        assert!(direct_handler_calls[0].2.contains("tool.handle("));

        let server_handler = include_str!("../server_handler.rs");
        let guarded = include_str!("guarded.rs");
        let pipeline = include_str!("pipeline.rs");
        let dispatch = include_str!("../dispatch/mod.rs");
        let resources = include_str!("../resources.rs");
        let file_resource = include_str!("../file_resource.rs");
        let prompts = include_str!("../prompts.rs");
        let cli_call = include_str!("../../cli/call_cmd.rs");
        let cli_agent_tools = include_str!("../../cli/agent_tools_cmd.rs");
        let embed_engine = include_str!("../../../crates/lean-ctx-embed/src/engine.rs");
        assert!(server_handler.contains("self.call_tool_guarded(request)"));
        // The guarded lifecycle driver runs the one pipeline in its two
        // stages: dispatch, then reversible post-processing.
        assert!(guarded.contains("super::pipeline::dispatch_primitive("));
        assert!(guarded.contains("super::pipeline::reversible_post_process("));
        // Dispatch runs under source-authority capture; content decisions go
        // through the shared policy guard (redaction or withholding) and the
        // sensitivity floor.
        assert!(
            pipeline
                .contains("authority::capture(server.dispatch_tool(name, args, minimal)).await")
        );
        assert!(pipeline.contains("policy_guard::protect_result(name, &result_text)"));
        assert!(pipeline.contains("sensitivity::enforce_text"));
        assert!(dispatch.contains("_ => self.dispatch_inner(name, args, minimal).await"));
        assert!(dispatch.contains("r.get_arc(name)"));
        assert!(resources.contains("policy_guard::redact_model_text"));
        assert!(file_resource.contains("resources::text_resource(&content, uri)"));
        assert!(prompts.contains("policy_guard::redact_model_text"));
        // `lean-ctx call` has no outbound step of its own: the handler runs
        // inside the shared CLI tool run, and that helper is the one place
        // in `cli/` where the release point and the output redaction are
        // applied — once, in `released`.
        let cli_common = include_str!("../../cli/common.rs");
        let cli_production = |source: &'static str| {
            source
                .split("#[cfg(test)]")
                .next()
                .expect("a production part")
        };
        assert!(cli_production(cli_call).contains("super::common::CliToolRun::new("));
        assert!(!cli_production(cli_call).contains("redact_model_text"));
        assert!(!cli_production(cli_call).contains("release_result"));
        let cli_helper = cli_production(cli_common);
        assert_eq!(
            cli_helper.matches("policy_guard::release_result(").count(),
            1,
            "the CLI helper has one release point"
        );
        assert_eq!(
            cli_helper
                .matches("crate::server::policy_guard::redact_model_text(&released_text)")
                .count(),
            1,
            "the CLI helper redacts the released text in one place"
        );
        assert!(cli_agent_tools.contains("policy_guard::redact_model_text(&output.text)"));
        assert!(embed_engine.contains("policy_guard::redact_model_text(&out.text)"));

        let registry = crate::server::registry::build_registry();
        let names = registry.names();
        assert!(!names.is_empty(), "registry must include registered tools");
        for name in names {
            let tool = registry.get(name).expect("every registered name resolves");
            assert_eq!(tool.name(), name);
        }
    }
}

#[cfg(test)]
mod roots_retry_tests {
    use super::roots_list_failure_is_permanent;

    /// Cursor's pattern (#699): roots capability declared, `roots/list`
    /// answered with `-32601` — retrying is pointless and must stop.
    #[test]
    fn method_not_found_is_permanent() {
        let err = rmcp::ServiceError::McpError(rmcp::model::ErrorData::new(
            rmcp::model::ErrorCode::METHOD_NOT_FOUND,
            "Method not found",
            None,
        ));
        assert!(roots_list_failure_is_permanent(&err));
    }

    /// The VS Code multi-window pattern (GH #694): the second window's client
    /// is still starting up, `roots/list` times out or the transport hiccups —
    /// these must stay retryable so root detection recovers.
    #[test]
    fn timeouts_and_other_mcp_errors_are_transient() {
        let timeout = rmcp::ServiceError::Timeout {
            timeout: std::time::Duration::from_secs(5),
        };
        assert!(!roots_list_failure_is_permanent(&timeout));

        let internal = rmcp::ServiceError::McpError(rmcp::model::ErrorData::new(
            rmcp::model::ErrorCode::INTERNAL_ERROR,
            "boom",
            None,
        ));
        assert!(!roots_list_failure_is_permanent(&internal));
    }
}

#[cfg(test)]
mod response_cache_tests {
    use super::super::guarded::{cache_call_result, cached_call_result, response_cache_key};
    use crate::core::ocla::response_cache::{CachedResponse, ResponseCache};
    use rmcp::model::{CallToolResult, ContentBlock};
    use serde_json::{Map, json};
    use std::time::{Duration, Instant};

    fn arguments() -> Map<String, serde_json::Value> {
        Map::from_iter([("path".to_owned(), json!("src/lib.rs"))])
    }

    fn text_of(result: &CallToolResult) -> &str {
        result.content[0]
            .as_text()
            .expect("cached result must be text")
            .text
            .as_str()
    }

    #[test]
    fn only_deterministic_tools_receive_cache_keys() {
        for tool in ["ctx_search", "ctx_tree", "ctx_glob"] {
            assert!(response_cache_key(tool, Some(&arguments()), "/project").is_some());
        }
        for tool in ["ctx_shell", "ctx_session", "ctx_knowledge", "ctx_call"] {
            assert!(response_cache_key(tool, Some(&arguments()), "/project").is_none());
        }
    }

    #[test]
    fn cache_keys_include_arguments_and_project_scope() {
        // The key binds the active policy; pin "no policy" so a concurrent
        // test's override cannot change the key between the calls.
        let _env = crate::core::data_dir::test_env_lock();
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let base = response_cache_key("ctx_search", Some(&arguments()), "/project").unwrap();
        let other_root = response_cache_key("ctx_search", Some(&arguments()), "/other").unwrap();
        let other_args = Map::from_iter([("path".to_owned(), json!("src/main.rs"))]);
        let other_path = response_cache_key("ctx_search", Some(&other_args), "/project").unwrap();

        assert_ne!(base, other_root);
        assert_ne!(base, other_path);
        assert_eq!(
            base,
            response_cache_key("ctx_search", Some(&arguments()), "/project").unwrap()
        );
    }

    #[test]
    fn successful_text_response_round_trips_through_cache() {
        let cache = ResponseCache::new(8, Duration::from_mins(1));
        let key = response_cache_key("ctx_search", Some(&arguments()), "/project").unwrap();
        let mut result = CallToolResult::success(vec![ContentBlock::text("search result")]);
        result.structured_content = Some(json!({"matches": 1}));

        cache_call_result(&cache, key.clone(), &result);

        let cached = cached_call_result(&cache, &key).expect("response should be cached");
        assert_eq!(text_of(&cached), "search result");
        assert_eq!(cached.structured_content, Some(json!({"matches": 1})));
        assert_ne!(cached.is_error, Some(true));
    }

    #[test]
    fn expired_response_is_not_returned() {
        let cache = ResponseCache::new(8, Duration::from_secs(1));
        let key = response_cache_key("ctx_tree", Some(&arguments()), "/project").unwrap();
        cache.put(
            key.clone(),
            CachedResponse {
                body: b"stale".to_vec(),
                status: 200,
                tokens: 1,
                created_at: Instant::now().checked_sub(Duration::from_secs(2)).unwrap(),
                ttl: Duration::from_secs(1),
            },
        );

        assert!(cached_call_result(&cache, &key).is_none());
    }
}

mod request_binding_tests {
    use crate::tools::LeanCtxServer;

    fn git_projects() -> (tempfile::TempDir, tempfile::TempDir) {
        let projects = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        for project in [&projects.0, &projects.1] {
            std::fs::create_dir(project.path().join(".git")).unwrap();
        }
        projects
    }

    fn uri(project: &tempfile::TempDir) -> String {
        format!("file://{}", project.path().display())
    }

    async fn adopt(server: &LeanCtxServer, uris: &[String]) {
        for save in server.adopt_client_roots(uris).await {
            save.await.unwrap();
        }
    }

    /// AP04-G1: a call bound to project A that writes after the host switched
    /// its roots to project B writes into A's session; B's starts without it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_late_write_stays_with_the_project_its_call_started_in() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let (first, second) = git_projects();
        let server = LeanCtxServer::new_with_project_root(Some(&first.path().to_string_lossy()));
        let first_root = server.session.read().await.project_root.clone();

        server
            .session
            .scope(async {
                server.session.pin();
                adopt(&server, &[uri(&second)]).await;
                let mut bound = server.session.write().await;
                bound.record_manual_evidence("ap04:late", None);
                assert_eq!(bound.project_root, first_root);
            })
            .await;

        let live = server.session.read().await;
        assert_eq!(
            live.project_root,
            crate::server::roots::uri_to_path(&uri(&second))
        );
        assert!(live.evidence.iter().all(|e| e.key != "ap04:late"));
    }

    /// AP04 review S1: the late write of a call whose session was replaced
    /// while it ran is saved to that session's project when the call ends,
    /// not left to a batch save that only the live session gets.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_late_write_to_a_replaced_session_is_saved_when_the_call_ends() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let (first, second) = git_projects();
        let server = LeanCtxServer::new_with_project_root(Some(&first.path().to_string_lossy()));
        let first_root = server.session.read().await.project_root.clone().unwrap();

        server
            .session
            .scope(async {
                server.session.pin();
                adopt(&server, &[uri(&second)]).await;
                let mut bound = server.session.write().await;
                bound.last_flush = Some(std::time::Instant::now());
                bound.record_manual_evidence("ap04:late", None);
            })
            .await;

        let saved = crate::core::session::SessionState::load_latest_for_project_root(&first_root)
            .expect("the first project's session is saved");
        assert!(saved.evidence.iter().any(|e| e.key == "ap04:late"));
    }

    /// AP04 review S2: on a switch to project B, project A's local
    /// configuration does not decide B's configured extra roots.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_root_switch_does_not_carry_the_previous_projects_configured_roots() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let (first, second) = git_projects();
        let extra = tempfile::tempdir().unwrap();
        let extra_root = extra.path().to_string_lossy().to_string();
        std::fs::write(
            first.path().join(".lean-ctx.toml"),
            format!("extra_roots = [{extra_root:?}]\n"),
        )
        .unwrap();
        let _trust = crate::test_env::VarGuard::set("LEAN_CTX_TRUST_WORKSPACE", "1");
        let server = LeanCtxServer::new_with_project_root(Some(&first.path().to_string_lossy()));

        crate::core::policy::runtime::REQUEST_PROJECT
            .scope(
                std::cell::RefCell::new(Some(first.path().to_path_buf())),
                adopt(&server, &[uri(&second)]),
            )
            .await;

        let live = server.session.read().await;
        assert_eq!(
            live.project_root,
            crate::server::roots::uri_to_path(&uri(&second))
        );
        assert!(!live.extra_roots.contains(&extra_root));
    }

    /// AP04-G5: one client's roots do not move a shared (HTTP daemon) session
    /// that every other client of that root works in.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn client_roots_do_not_move_a_shared_session() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let (shared, other) = git_projects();
        let server = LeanCtxServer::new_shared_with_context(
            &shared.path().to_string_lossy(),
            "ap04-g5",
            "ap04-g5",
        );
        let root = server.session.read().await.project_root.clone();
        assert!(root.is_some());

        adopt(&server, &[uri(&other)]).await;

        assert_eq!(server.session.read().await.project_root, root);
    }

    /// AP04 review B1: a root one client of a shared session declares is not
    /// added to the session — and so not writable — for its other clients.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_clients_extra_root_is_not_shared_with_the_other_clients() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let (shared, private) = git_projects();
        let client = |name: &str| {
            LeanCtxServer::new_shared_with_context(&shared.path().to_string_lossy(), name, "b1")
        };
        let (x, y) = (client("ap04-b1"), client("ap04-b1"));

        adopt(&x, &[uri(&shared), uri(&private)]).await;

        let private_root = crate::server::roots::uri_to_path(&uri(&private)).unwrap();
        assert!(!y.session.read().await.extra_roots.contains(&private_root));
    }

    /// H02: Claude and Codex connected at the same time, each over its own
    /// MCP connection and in its own project, as real hosts do (identity in
    /// `initialize`, project in `roots/list`). Each binds its own project and
    /// task, and the session status it reads — its diagnosis — names its own
    /// root and task and nothing of the other host's.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[allow(deprecated)] // MCP roots (SEP-2577) is what these hosts still send
    async fn claude_and_codex_at_once_each_see_only_their_own_project_and_task() {
        use rmcp::ServiceExt;
        use rmcp::model::{
            CallToolRequestParams, ClientCapabilities, ClientInfo, Implementation, ListRootsResult,
            Root,
        };

        struct Host {
            name: &'static str,
            root: String,
        }
        impl rmcp::ClientHandler for Host {
            fn get_info(&self) -> ClientInfo {
                ClientInfo::new(
                    ClientCapabilities::builder().enable_roots().build(),
                    Implementation::new(self.name, "1.0.0"),
                )
            }
            fn list_roots(
                &self,
                _context: rmcp::service::RequestContext<rmcp::RoleClient>,
            ) -> impl std::future::Future<Output = Result<ListRootsResult, rmcp::ErrorData>> + Send + '_
            {
                std::future::ready(Ok(ListRootsResult::new(vec![Root::new(self.root.clone())])))
            }
        }

        let _data = crate::core::data_dir::isolated_data_dir();
        let (claude_project, codex_project) = git_projects();
        let connect = |name: &'static str, project: &tempfile::TempDir| {
            let root = uri(project);
            async move {
                let server = LeanCtxServer::new();
                let (server_io, client_io) = tokio::io::duplex(1 << 16);
                let served = server.clone();
                tokio::spawn(async move {
                    if let Ok(running) = served.serve(server_io).await {
                        let _ = running.waiting().await;
                    }
                });
                let client = Host { name, root }.serve(client_io).await.expect("connect");
                (server, client)
            }
        };
        async fn session(
            client: &rmcp::service::RunningService<rmcp::RoleClient, Host>,
            arguments: serde_json::Value,
        ) -> String {
            let request = CallToolRequestParams::new("ctx_session")
                .with_arguments(arguments.as_object().cloned().unwrap_or_default());
            let result = client.call_tool(request).await.expect("ctx_session");
            result
                .content
                .iter()
                .filter_map(|block| block.as_text().map(|text| text.text.clone()))
                .collect()
        }
        async fn run(
            client: &rmcp::service::RunningService<rmcp::RoleClient, Host>,
            host: &str,
        ) -> String {
            let task = format!("{host} owns this task");
            session(client, serde_json::json!({"action": "task", "value": task})).await;
            session(client, serde_json::json!({"action": "status"})).await
        }
        let (claude, codex) = tokio::join!(
            connect("claude-code", &claude_project),
            connect("codex-mcp-client", &codex_project)
        );
        // Both hosts work at the same time.
        let (claude_status, codex_status) =
            tokio::join!(run(&claude.1, "claude"), run(&codex.1, "codex"));

        let name_of = |project: &tempfile::TempDir| {
            project
                .path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        };
        for (server, status, own, other, host, foreign) in [
            (
                &claude.0,
                &claude_status,
                &claude_project,
                &codex_project,
                "claude",
                "codex",
            ),
            (
                &codex.0,
                &codex_status,
                &codex_project,
                &claude_project,
                "codex",
                "claude",
            ),
        ] {
            let bound = server.session.read().await;
            assert_eq!(
                bound.project_root,
                crate::server::roots::uri_to_path(&uri(own)),
                "{host} is bound to its own project"
            );
            assert_eq!(
                bound.task.as_ref().map(|task| task.description.as_str()),
                Some(format!("{host} owns this task").as_str())
            );
            drop(bound);
            assert!(server.client_name.read().await.starts_with(host));
            assert!(
                status.contains(&format!("Task: {host} owns this task")),
                "{status}"
            );
            assert!(
                status.contains(&name_of(own)),
                "{host} sees its root: {status}"
            );
            assert!(!status.contains(&name_of(other)), "{status}");
            assert!(!status.contains(&format!("{foreign} owns")), "{status}");
        }
        let _ = claude.1.cancel().await;
        let _ = codex.1.cancel().await;
    }

    fn save_session_of_another_agent(project: &tempfile::TempDir, task: &str) -> String {
        let mut session = crate::core::session::SessionState::new();
        session.project_root = crate::server::roots::uri_to_path(&uri(project));
        session.task = Some(crate::core::session::TaskInfo {
            description: task.to_owned(),
            intent: None,
            progress_pct: None,
        });
        session.save().expect("saved session");
        session.id
    }

    /// I07/C06: a server restarted while two projects have saved sessions
    /// takes up neither when its host reports roots: each is another
    /// agent's private state, and the project's latest saved session is no
    /// shortcut. A server going back to a project gets its OWN session there.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_root_switch_takes_up_no_other_agents_saved_session() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let (first, second) = git_projects();
        let saved = [
            save_session_of_another_agent(&first, "c06-first-private-task"),
            save_session_of_another_agent(&second, "c06-second-private-task"),
        ];
        let server = LeanCtxServer::new_with_startup(
            None,
            None,
            crate::tools::SessionMode::Personal,
            "default",
            "default",
        );
        assert_eq!(server.session.read().await.project_root, None);

        let live = |server: &LeanCtxServer| {
            let session = server.session.live();
            let session = session.try_read().expect("no call holds the session");
            assert!(session.task.is_none(), "took over {:?}", session.task);
            assert!(!saved.contains(&session.id));
            session.id.clone()
        };
        adopt(&server, &[uri(&second)]).await;
        let own_in_second = live(&server);
        server
            .session
            .write()
            .await
            .record_manual_evidence("c06:own-second", None);
        adopt(&server, &[uri(&first)]).await;
        live(&server);
        // Another agent saves the second project's session again meanwhile.
        save_session_of_another_agent(&second, "c06-second-newer-task");
        adopt(&server, &[uri(&second)]).await;
        assert_eq!(live(&server), own_in_second);
        let back = server.session.read().await;
        assert!(back.evidence.iter().any(|e| e.key == "c06:own-second"));
    }

    /// C06: two hosts work live, each in its own project, and save. One host
    /// restarts: its new server re-adopting its root binds that project with
    /// a session of its own — not the project's latest saved session (its
    /// predecessor's) and nothing of the other project. The other host
    /// reconnects (its roots are sent again) and keeps its own session.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_restarted_host_and_a_reconnecting_one_keep_their_own_project_bindings() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let (first, second) = git_projects();
        let host = |project: &tempfile::TempDir| {
            LeanCtxServer::new_with_project_root(Some(&project.path().to_string_lossy()))
        };
        let (a, b) = (host(&first), host(&second));
        for (server, task) in [(&a, "c06-a-live-task"), (&b, "c06-b-live-task")] {
            let mut session = server.session.write().await;
            session.set_task(task, None);
            session.record_manual_evidence(task, None);
            session.save().expect("saved live session");
        }
        let a_saved = a.session.read().await.id.clone();
        let b_before = b.session.read().await.id.clone();
        drop(a);

        let restarted = LeanCtxServer::new_with_startup(
            None,
            None,
            crate::tools::SessionMode::Personal,
            "default",
            "default",
        );
        adopt(&restarted, &[uri(&first)]).await;
        adopt(&b, &[uri(&second)]).await;

        let fresh = restarted.session.read().await;
        assert_eq!(
            fresh.project_root,
            crate::server::roots::uri_to_path(&uri(&first))
        );
        assert_ne!(fresh.id, a_saved, "took up the latest saved session");
        assert_ne!(fresh.id, b_before);
        assert!(fresh.task.is_none(), "took over {:?}", fresh.task);
        assert!(fresh.evidence.iter().all(|e| !e.key.starts_with("c06-")));
        let kept = b.session.read().await;
        assert_eq!(kept.id, b_before, "the reconnect replaced the session");
        assert_eq!(
            kept.project_root,
            crate::server::roots::uri_to_path(&uri(&second))
        );
        assert!(kept.evidence.iter().any(|e| e.key == "c06-b-live-task"));
        assert!(kept.evidence.iter().all(|e| e.key != "c06-a-live-task"));
    }

    /// C06: a call cancelled after its host switched projects leaves nothing
    /// in the session of the project switched to; the session it was bound
    /// to was saved, under its own project, at the switch.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_cancelled_call_leaves_nothing_in_the_project_switched_to() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let (first, second) = git_projects();
        let server = LeanCtxServer::new_with_project_root(Some(&first.path().to_string_lossy()));
        let first_root = server.session.read().await.project_root.clone().unwrap();
        server
            .session
            .write()
            .await
            .record_manual_evidence("c06:before-switch", None);
        let (wrote, written) = tokio::sync::oneshot::channel();
        let call = server.session.scope(async {
            server.session.pin();
            adopt(&server, &[uri(&second)]).await;
            server
                .session
                .write()
                .await
                .record_manual_evidence("c06:cancelled", None);
            let _ = wrote.send(());
            std::future::pending::<()>().await;
        });
        tokio::select! {
            () = call => unreachable!("the call only ends by cancellation"),
            _ = written => {}
        }

        let live = server.session.read().await;
        assert_eq!(
            live.project_root,
            crate::server::roots::uri_to_path(&uri(&second))
        );
        assert!(live.evidence.iter().all(|e| !e.key.starts_with("c06:")));
        let saved = crate::core::session::SessionState::load_latest_for_project_root(&first_root)
            .expect("the first project's session was saved at the switch");
        assert_eq!(saved.project_root.as_deref(), Some(first_root.as_str()));
        assert!(saved.evidence.iter().any(|e| e.key == "c06:before-switch"));
    }

    async fn call_text(server: &LeanCtxServer, tool: &str, args: &serde_json::Value) -> String {
        let result = crate::server::call_tool::dispatch_and_post_process(
            server,
            tool,
            args.as_object(),
            false,
            crate::core::config::Config::load_arc(),
            false,
            None,
            None,
            format!("{tool}:{args}"),
            None,
        )
        .await;
        match result {
            Ok(result) => result
                .content
                .iter()
                .filter_map(|content| content.as_text().map(|t| t.text.clone()))
                .collect(),
            Err(error) => error.message.to_string(),
        }
    }

    /// I08: one canary in a project's source (read into the content cache,
    /// indexed, in the call graph), its knowledge, knowledge archive and
    /// shared memory. Tool calls and the dashboard inspector bound to another
    /// project answer exactly as they answer for a term that exists nowhere:
    /// no title, snippet, count or hint of the canary.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_canary_in_every_store_of_a_project_is_invisible_from_another() {
        const CANARY: &str = "zqcanaryeight";
        const ABSENT: &str = "zqnowhereeight";
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let (first, second) = git_projects();
        let owner = LeanCtxServer::new_with_project_root(Some(&first.path().to_string_lossy()));
        let first_root = owner.session.read().await.project_root.clone().unwrap();
        let source = std::path::Path::new(&first_root).join(format!("{CANARY}_vault.rs"));
        std::fs::write(
            &source,
            format!("pub fn {CANARY}_vault() -> u8 {{ {CANARY}_seal() }}\nfn {CANARY}_seal() -> u8 {{ 8 }}\n"),
        )
        .unwrap();
        let read = call_text(&owner, "ctx_read", &serde_json::json!({"path": source})).await;
        assert!(read.contains(CANARY), "{read}");
        let indexed = call_text(
            &owner,
            "ctx_search",
            &serde_json::json!({"action": "semantic", "query": CANARY}),
        )
        .await;
        assert!(indexed.contains(CANARY), "{indexed}");
        let graph =
            crate::core::call_graph::CallGraph::build(&crate::core::call_graph::CallGraphInputs {
                project_root: first_root.clone(),
                file_paths: vec![format!("{CANARY}_vault.rs")],
                ..Default::default()
            });
        graph.save().unwrap();
        let fact = format!("{CANARY} keys live in the vault");
        call_text(
            &owner,
            "ctx_knowledge",
            &serde_json::json!({"action": "remember", "category": "decision", "key": CANARY, "value": fact}),
        )
        .await;
        crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
            std::cell::RefCell::new(Some(std::path::PathBuf::from(&first_root))),
            || {
                let knowledge = crate::core::knowledge::ProjectKnowledge::load(&first_root)
                    .expect("the canary fact was stored");
                let stored: Vec<_> = knowledge
                    .facts
                    .into_iter()
                    .filter(|f| f.key == CANARY)
                    .collect();
                assert_eq!(stored.len(), 1);
                crate::core::memory_lifecycle::archive_facts(&stored).unwrap();
            },
        );
        crate::core::shared_context::SharedContext::for_project(&first_root)
            .and_then(|store| store.put(format!("{CANARY} shared note"), "a1", "fact"))
            .unwrap();

        let other = LeanCtxServer::new_with_project_root(Some(&second.path().to_string_lossy()));
        let second_root = other.session.read().await.project_root.clone().unwrap();
        let probes = |term: &str| {
            [
                (
                    "ctx_knowledge",
                    serde_json::json!({"action": "recall", "query": term}),
                ),
                (
                    "ctx_knowledge",
                    serde_json::json!({"action": "search", "query": term}),
                ),
                ("ctx_search", serde_json::json!({"pattern": term})),
                (
                    "ctx_search",
                    serde_json::json!({"action": "semantic", "query": term}),
                ),
                (
                    "ctx_memory",
                    serde_json::json!({"action": "get", "query": term}),
                ),
                (
                    "ctx_callgraph",
                    serde_json::json!({"action": "callers", "symbol": format!("{term}_seal")}),
                ),
            ]
        };
        for ((tool, canary_args), (_, absent_args)) in
            probes(CANARY).into_iter().zip(probes(ABSENT))
        {
            // A one-time setup tip may trail whichever call comes first; it
            // is session state, not project content.
            let untipped = |text: String| match text.find("\n\n--- tip:") {
                Some(at) => text[..at].to_string(),
                None => text,
            };
            let seen = untipped(call_text(&other, tool, &canary_args).await);
            let baseline = untipped(call_text(&other, tool, &absent_args).await);
            // An answer may echo the caller's own query term; whether it
            // leaks is decided against a term no project has ever held.
            assert_eq!(
                seen.replace(CANARY, "TERM"),
                baseline.replace(ABSENT, "TERM"),
                "{tool} answers differently for a foreign canary"
            );
        }
        // The content cache holds the first project's file: a read of it
        // from the other project is refused, not served from the cache.
        // `no-jail` turns the path jail off on purpose, so only with it on.
        #[cfg(not(feature = "no-jail"))]
        {
            let foreign_read =
                call_text(&other, "ctx_read", &serde_json::json!({"path": source})).await;
            assert!(
                !foreign_read.contains(&format!("{CANARY}_seal")),
                "{foreign_read}"
            );
        }

        let _bound = crate::test_env::VarGuard::set("LEAN_CTX_DASHBOARD_PROJECT", &second_root);
        for route in [
            "/api/knowledge",
            "/api/knowledge-relations",
            "/api/memory",
            "/api/search",
            "/api/search-index",
            "/api/symbols",
            "/api/call-graph",
            "/api/graph",
            "/api/session",
            "/api/intent",
            "/api/procedures",
            "/api/gotchas",
        ] {
            let body_for = |term: &str| {
                let query = format!("q={term}");
                crate::dashboard::routes::route_response(route, &query, None, None, true, "GET", "")
                    .2
            };
            // The bound project's own session remembers the last semantic
            // query it ran — the absent term, probed last (I07: the query
            // lands in the caller's own session). That is its own state, not
            // the first project's.
            let own_last_query = format!("\"last_semantic_query\":\"{ABSENT}\"");
            let (seen, baseline) = (
                body_for(CANARY).replace(&own_last_query, ""),
                body_for(ABSENT).replace(&own_last_query, ""),
            );
            // The bound project's own session lists its own queries and a
            // route may echo the query; anything from the first project
            // would make the canary appear more often than the absent term.
            assert_eq!(
                seen.matches(CANARY).count(),
                baseline.matches(ABSENT).count(),
                "{route}: {seen}"
            );
        }
    }

    /// I04: a host that authorises two roots without asking for an
    /// aggregation gets answers from the active project only — knowledge,
    /// cross-session search, text search and the index search.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_authorised_roots_answer_from_the_active_project_only() {
        use crate::server::call_tool::dispatch_and_post_process;
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let (first, second) = git_projects();
        let server = LeanCtxServer::new_with_project_root(Some(&first.path().to_string_lossy()));
        adopt(&server, &[uri(&first), uri(&second)]).await;
        let active = server.session.read().await.project_root.clone().unwrap();
        let roots = [first.path(), second.path()].map(|p| {
            crate::server::roots::uri_to_path(&format!("file://{}", p.display())).unwrap()
        });
        assert!(roots.contains(&active));
        assert_eq!(server.session.read().await.extra_roots.len(), 1);
        let policy = crate::core::memory_policy::MemoryPolicy::default();
        for (index, root) in roots.iter().enumerate() {
            // Query term and project mark are separate tokens, so the
            // index search matches the term and the mark names the project.
            let marker = format!("widgetcanary zqmark{index}");
            std::fs::write(
                std::path::Path::new(root).join("notes.md"),
                format!("{marker} lives here\n"),
            )
            .unwrap();
            crate::core::knowledge::ProjectKnowledge::mutate_locked(root, |k| {
                k.remember("decision", "widget", &marker, "i04", 0.9, &policy);
            })
            .unwrap();
            let mut session = crate::core::session::SessionState::new();
            session.project_root = Some(root.clone());
            session.add_finding(None, None, &format!("{marker} finding"));
            session.save().unwrap();
        }
        let own = format!("zqmark{}", roots.iter().position(|r| *r == active).unwrap());
        let foreign = format!("zqmark{}", roots.iter().position(|r| *r != active).unwrap());

        for (tool, args) in [
            (
                "ctx_knowledge",
                serde_json::json!({"action": "recall", "query": "widget"}),
            ),
            (
                "ctx_knowledge",
                serde_json::json!({"action": "search", "query": "widgetcanary"}),
            ),
            ("ctx_search", serde_json::json!({"pattern": "widgetcanary"})),
            (
                "ctx_search",
                serde_json::json!({"action": "semantic", "query": "widgetcanary"}),
            ),
        ] {
            let result = dispatch_and_post_process(
                &server,
                tool,
                args.as_object(),
                false,
                crate::core::config::Config::load_arc(),
                false,
                None,
                None,
                "i04".to_string(),
                None,
            )
            .await
            .expect("tool pipeline");
            let text: String = result
                .content
                .iter()
                .filter_map(|content| content.as_text().map(|t| t.text.clone()))
                .collect();
            assert!(text.contains(&own), "{tool} {args}: {text}");
            if tool == "ctx_search" && args.get("action").is_none() {
                // Text search spans the roots of a multi-root workspace the
                // host authorised, each hit under its own root's header and
                // never merged into another root's block.
                let mut root_of_block = None;
                for line in text.lines() {
                    if let Some(header) = line.strip_prefix("── ") {
                        root_of_block = Some(header.trim_end_matches(" ──").to_string());
                    }
                    for (index, root) in roots.iter().enumerate() {
                        if line.contains(&format!("zqmark{index}")) {
                            // Without a header the answer covers the active
                            // project alone (the path jail keeps it there).
                            let Some(block) = root_of_block.as_deref() else {
                                assert!(!line.contains(&foreign), "{text}");
                                continue;
                            };
                            assert!(
                                std::path::Path::new(block) == std::path::Path::new(root)
                                    || std::fs::canonicalize(block).ok()
                                        == std::fs::canonicalize(root).ok(),
                                "hit of {root} listed under {block:?}: {text}"
                            );
                        }
                    }
                }
            } else {
                assert!(!text.contains(&foreign), "{tool} {args}: {text}");
            }
        }
    }
}
