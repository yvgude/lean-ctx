#[allow(clippy::wildcard_imports)]
use super::super::*;
use super::{finalize_call_result, roots_list_failure_is_permanent};

/// Community regression (3.9.19): every documented lossless escape hatch was
/// ignored — triage ran after the compression pipeline, so per-call
/// parameters never reached it. These pins guarantee the bypass contract
/// stays wired at the dispatch chokepoint.
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

    #[cfg(not(windows))]
    fn auto_detached_result(command: &str) -> CallToolResult {
        let detached = crate::server::background_shell::run_foreground_or_detach(
            command.to_string(),
            ".".to_string(),
            std::collections::HashMap::default(),
            Some(10_000),
            std::time::Duration::from_millis(10),
            None,
        );
        let crate::server::background_shell::ForegroundResult::Detached { job_id } = detached
        else {
            panic!("the reproduction must exercise the auto-detached path");
        };
        let _job = BackgroundJobGuard::new(job_id.clone());

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
        let detached = crate::server::background_shell::run_foreground_or_detach(
            command.to_string(),
            ".".to_string(),
            std::collections::HashMap::default(),
            Some(30_000),
            std::time::Duration::from_millis(10),
            None,
        );
        let crate::server::background_shell::ForegroundResult::Detached { job_id } = detached
        else {
            panic!("the reproduction must exercise the auto-detached path");
        };
        let job = BackgroundJobGuard::new(job_id.clone());
        for _ in 0..400 {
            if matches!(
                crate::server::background_shell::status(&job_id),
                Some(crate::server::background_shell::JobState::Completed { .. })
            ) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert!(matches!(
            crate::server::background_shell::status(&job_id),
            Some(crate::server::background_shell::JobState::Completed { .. })
        ));
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
            "sleep 2 # MES1609_RUNNING_STATUS".to_string(),
            ".".to_string(),
            std::collections::HashMap::default(),
            Some(10_000),
            std::time::Duration::from_millis(10),
            None,
        );
        let crate::server::background_shell::ForegroundResult::Detached { job_id } = detached
        else {
            panic!("the reproduction must exercise the auto-detached path");
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
        let cwd = std::env::current_dir()
            .expect("current directory")
            .to_string_lossy()
            .into_owned();
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
        for _ in 0..400 {
            if matches!(
                crate::server::background_shell::status(&job_id),
                Some(
                    crate::server::background_shell::JobState::Completed { .. }
                        | crate::server::background_shell::JobState::Cancelled { .. }
                )
            ) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert!(matches!(
            crate::server::background_shell::status(&job_id),
            Some(crate::server::background_shell::JobState::Completed { .. })
        ));
        (job_id, job)
    }

    #[cfg(not(windows))]
    async fn pipeline_background_status(
        job_id: &str,
        minimal: bool,
        raw: bool,
        inline: bool,
    ) -> CallToolResult {
        let root = std::env::current_dir()
            .expect("current directory")
            .to_string_lossy()
            .into_owned();
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
            "sleep 2 # MES1609_CANCEL_STATUS".to_string(),
            ".".to_string(),
            std::collections::HashMap::default(),
            Some(10_000),
            std::time::Duration::from_millis(10),
            None,
        );
        let crate::server::background_shell::ForegroundResult::Detached { job_id } = detached
        else {
            panic!("the reproduction must exercise the auto-detached path");
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
        let mut args = serde_json::Map::new();
        args.insert(
            "command".to_string(),
            serde_json::json!("sleep 2 # MES1609_EXPLICIT_LAUNCH"),
        );
        args.insert("run_in_background".to_string(), serde_json::json!(true));

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
    async fn soft_cap_auto_detach_ack_is_structured_running() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let _archive = ScopedEnvVar::set("LEAN_CTX_ARCHIVE", "1");
        let _soft_cap = ScopedEnvVar::set("LEAN_CTX_SHELL_FG_CAP_MS", "10");
        let mut args = serde_json::Map::new();
        args.insert(
            "command".to_string(),
            serde_json::json!("sleep 2 # MES1609_SOFT_CAP_LAUNCH"),
        );

        let result = call_shell(&args, &shell_context());

        let _job = launched_job_guard(&result);
        assert_ne!(result.is_error, Some(true));
        let structured = structured_of(&result);
        assert_eq!(structured["state"], serde_json::json!("running"));
        assert!(structured["jobId"].as_str().is_some());
        assert!(structured.get("exitCode").is_none());
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
    }

    #[tokio::test(flavor = "multi_thread")]
    #[cfg(not(windows))]
    async fn oversized_background_archive_reports_truncation_and_exact_sizes() {
        const ARCHIVE_LIMIT: usize = 10 * 1024 * 1024;
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
        let job_id = crate::server::background_shell::start(
            "sleep 0.1; printf MES1609_PIPELINE; head -c 50000 /dev/zero | tr '\\000' P"
                .to_string(),
            ".".to_string(),
            std::collections::HashMap::default(),
            Some(10_000),
        );
        let _job = BackgroundJobGuard::new(job_id.clone());
        for _ in 0..240 {
            if matches!(
                crate::server::background_shell::status(&job_id),
                Some(crate::server::background_shell::JobState::Completed { .. })
            ) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert!(matches!(
            crate::server::background_shell::status(&job_id),
            Some(crate::server::background_shell::JobState::Completed { .. })
        ));

        let root = std::env::current_dir()
            .expect("current directory")
            .to_string_lossy()
            .into_owned();
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

        let root = std::env::current_dir()
            .expect("current directory")
            .to_string_lossy()
            .into_owned();
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
    async fn background_archive_is_created_after_policy_and_input_filters() {
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
        let archive_id = structured["archiveId"]
            .as_str()
            .expect("secured background output must be archived");
        assert_eq!(crate::core::archive::list_entries(None).len(), 1);
        assert_eq!(structured["archiveTruncated"], serde_json::json!(false));
        let archived = crate::core::archive::retrieve(archive_id)
            .expect("structured archiveId must remain retrievable");
        assert!(!archived.contains(POLICY_PATTERN));
        assert!(!archived.contains(TEST_CARD));
        assert!(archived.contains("[REDACTED:mes1609_policy]"));
        assert!(archived.contains("[REDACTED:card]"));
        assert!(!text_of(&result).contains(POLICY_PATTERN));
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
        let (first_ready_tx, first_ready_rx) = std::sync::mpsc::channel();
        let (release_first_tx, release_first_rx) = std::sync::mpsc::channel();
        let first = std::thread::spawn(move || {
            let _policy = ScopedPolicy::redacting("mes1609_first", "MES1609_FIRST");
            first_ready_tx.send(()).expect("signal first override");
            release_first_rx.recv().expect("release first override");
        });
        first_ready_rx
            .recv_timeout(std::time::Duration::from_secs(1))
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
        second_started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("second thread must start");
        let overlapped = second_acquired_rx
            .recv_timeout(std::time::Duration::from_millis(500))
            .is_ok();

        release_first_tx.send(()).expect("release first thread");
        first.join().expect("first policy thread");
        if !overlapped {
            second_acquired_rx
                .recv_timeout(std::time::Duration::from_secs(1))
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
