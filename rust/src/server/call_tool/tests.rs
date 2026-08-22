#[allow(clippy::wildcard_imports)]
use super::super::*;
use super::{finalize_call_result, roots_list_failure_is_permanent};

mod shell_outcome_tests {
    use super::*;
    use crate::server::tool_trait::{McpTool, ShellOutcome, ToolContext};

    fn text_of(result: &CallToolResult) -> String {
        result
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.clone())
            .unwrap_or_default()
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

        let mut terminal = false;
        for _ in 0..80 {
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

    fn structured_of(result: &CallToolResult) -> &serde_json::Value {
        result
            .structured_content
            .as_ref()
            .expect("background status must expose structuredContent")
    }

    #[cfg(not(windows))]
    fn auto_detached_running_result() -> CallToolResult {
        let detached = crate::server::background_shell::run_foreground_or_detach(
            "sleep 2".to_string(),
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

        let mut args = serde_json::Map::new();
        args.insert("background_action".to_string(), serde_json::json!("status"));
        args.insert("job_id".to_string(), serde_json::json!(job_id));
        let output = crate::tools::registered::ctx_shell::CtxShellTool
            .handle(&args, &ToolContext::default())
            .expect("status must return a tool result");
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
    #[cfg_attr(windows, ignore)]
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
    #[cfg_attr(windows, ignore)]
    fn auto_detached_exit_7_is_a_structured_failed_job() {
        let result = auto_detached_result("sleep 0.1; exit 7");

        assert_eq!(result.is_error, Some(true));
        let structured = structured_of(&result);
        assert_eq!(structured["state"], serde_json::json!("failed"));
        assert_eq!(structured["exitCode"], serde_json::json!(7));
    }

    #[test]
    #[cfg_attr(windows, ignore)]
    fn auto_detached_running_job_has_no_exit_code() {
        let result = auto_detached_running_result();

        assert_ne!(result.is_error, Some(true));
        let structured = structured_of(&result);
        assert_eq!(structured["state"], serde_json::json!("running"));
        assert!(structured["jobId"].as_str().is_some());
        assert!(structured.get("exitCode").is_none());
    }

    #[test]
    #[cfg_attr(windows, ignore)]
    fn auto_detached_cancel_returns_cancelled_exit_130_without_error() {
        let detached = crate::server::background_shell::run_foreground_or_detach(
            "sleep 2".to_string(),
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

    /// MES-1609: a successful auto-detached child remains queryable after
    /// the OS process exits and exposes an explicit terminal code 0.
    #[test]
    #[cfg_attr(windows, ignore)]
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
    #[test]
    #[cfg_attr(windows, ignore)]
    fn auto_detached_large_output_keeps_summary_and_archive_id_inline() {
        let result = auto_detached_result("sleep 0.1; seq 1 50000");

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
        crate::core::archive::remove_files(archive_id);
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
