// SPDX-License-Identifier: Apache-2.0
use super::{
    CtxShellTool, collect_extra_env, detect_heredoc_reroute, format_background_state,
    is_timeout_notice_only, resolve_effective_cwd, shell_access_denial, should_auto_background,
};
use crate::server::background_shell::JobState;
use crate::server::tool_trait::{
    BackgroundJobState, BackgroundShellOutcome, McpTool, ShellOutcome, ToolContext,
};
use serde_json::{Map, Value};

fn env_args(pairs: &[(&str, &str)]) -> Map<String, Value> {
    let mut env = Map::new();
    for (key, value) in pairs {
        env.insert((*key).to_string(), Value::String((*value).to_string()));
    }
    let mut args = Map::new();
    args.insert("env".to_string(), Value::Object(env));
    args
}

/// #1771: the inline-override refusal points callers at `env` and uses `PATH`
/// as its example, so silently dropping `PATH` here made the recommended
/// recovery run with the inherited value and report success.
#[test]
fn env_refuses_protected_key_by_name_instead_of_dropping_it() {
    let args = env_args(&[("PATH", "/tmp/evil"), ("RUST_LOG", "debug")]);
    let error = collect_extra_env(&args).expect_err("PATH must be refused, not dropped");
    assert!(
        error.contains("`PATH`"),
        "the refusal must name the offending key, got: {error}"
    );
    assert!(
        error.contains("nothing was run"),
        "the caller must learn the command did not run, got: {error}"
    );
}

#[test]
fn env_refusal_names_every_rejected_key_in_sorted_order() {
    let args = env_args(&[("PATH", "/x"), ("LD_PRELOAD", "/y")]);
    let error = collect_extra_env(&args).expect_err("both keys are protected");
    let ld = error.find("LD_PRELOAD").expect("LD_PRELOAD must be named");
    let path = error.find("`PATH`").expect("PATH must be named");
    assert!(ld < path, "keys must be listed sorted, got: {error}");
}

#[test]
fn env_passes_ordinary_keys_through() {
    let args = env_args(&[("RUST_LOG", "debug")]);
    let accepted = collect_extra_env(&args).expect("ordinary keys are allowed");
    assert_eq!(accepted.get("RUST_LOG").map(String::as_str), Some("debug"));
}

#[test]
fn env_absent_is_not_an_error() {
    let args = Map::new();
    assert!(
        collect_extra_env(&args)
            .expect("a missing env object is fine")
            .is_empty()
    );
}

fn background(outcome: ShellOutcome) -> BackgroundShellOutcome {
    let ShellOutcome::Background(outcome) = outcome else {
        panic!("expected a background outcome");
    };
    outcome
}

/// #1246: a cancel must never come back as a tool error, and must not read
/// like a status poll that did nothing.
///
/// #1674: the second half of that only became true here. This asserted
/// `Running` — the very value that made a cancel indistinguishable from a
/// poll, against the intent stated one line above. #1246 fixed the
/// error-reporting half and left the state; the state is the half the
/// caller reads.
#[test]
fn cancel_is_acknowledged_and_never_reports_a_failure() {
    let running = JobState::Running {
        output: String::new(),
    };
    let (text, outcome) = format_background_state("shell_x", true, Some(running.clone()));
    let outcome = background(outcome);
    assert!(!outcome.is_error);
    assert_eq!(outcome.state, BackgroundJobState::Cancelled);
    assert_eq!(outcome.exit_code, None);
    assert!(text.is_empty());
    assert!(
        outcome
            .display
            .as_ref()
            .is_some_and(|display| display.header.contains("cancel requested"))
    );

    // A status poll of the same state keeps the old wording.
    let (text, outcome) = format_background_state("shell_x", false, Some(running));
    let outcome = background(outcome);
    assert!(!outcome.is_error);
    assert_eq!(outcome.state, BackgroundJobState::Running);
    assert!(text.is_empty());
    assert!(
        outcome
            .display
            .as_ref()
            .is_some_and(|display| display.header == "[background:shell_x running]")
    );

    // The terminal child code is data; the requested cancel remains a
    // successful tool action even though structuredContent keeps 130.
    let (text, outcome) = format_background_state(
        "shell_x",
        true,
        Some(JobState::Cancelled {
            output: "[cancelled: command stopped on request]".to_string(),
        }),
    );
    let outcome = background(outcome);
    assert!(!outcome.is_error);
    assert_eq!(outcome.state, BackgroundJobState::Cancelled);
    assert_eq!(outcome.exit_code, Some(130));
    assert!(text.contains("[cancelled: command stopped on request]"));
    assert!(
        outcome
            .display
            .as_ref()
            .and_then(|display| display.footer.as_deref())
            .is_some_and(|footer| footer == "[cancelled: shell_x, exit 130]")
    );

    // Idempotent: already finished, or finished and pruned.
    let finished = JobState::Completed {
        output: "boom".to_string(),
        exit_code: 1,
    };
    let cancelled_finished =
        background(format_background_state("shell_x", true, Some(finished.clone())).1);
    assert!(!cancelled_finished.is_error);
    assert_eq!(cancelled_finished.state, BackgroundJobState::Failed);
    assert_eq!(cancelled_finished.exit_code, Some(1));

    let polled_finished = background(format_background_state("shell_x", false, Some(finished)).1);
    assert!(polled_finished.is_error);
    assert_eq!(polled_finished.state, BackgroundJobState::Failed);
    assert_eq!(polled_finished.exit_code, Some(1));

    let missing_cancel = format_background_state("shell_x", true, None).1;
    assert!(!missing_cancel.is_error());
    assert_eq!(missing_cancel, ShellOutcome::Exit(0));

    let missing_status = format_background_state("shell_x", false, None).1;
    assert!(missing_status.is_error());
    assert!(matches!(
        missing_status,
        ShellOutcome::BackgroundLookupError(_)
    ));
}

#[test]
fn long_cargo_test_is_auto_backgrounded() {
    assert!(should_auto_background(
        "cargo test --lib a\ncargo test --lib b",
        Some(3_600_000)
    ));
    assert!(should_auto_background("cargo test --lib a", Some(300_000)));
    assert!(!should_auto_background("cargo test --lib a", Some(299_999)));
}

#[test]
fn timeout_notice_without_child_output_is_not_recoverable() {
    assert!(is_timeout_notice_only(
        "ERROR: command timed out after 200ms",
        124
    ));
    assert!(is_timeout_notice_only(
        "  ERROR: command timed out after 200ms\n",
        124
    ));
    assert!(!is_timeout_notice_only(
        "useful output\nERROR: command timed out after 200ms",
        124
    ));
    assert!(!is_timeout_notice_only(
        "ERROR: command timed out after 200ms",
        1
    ));
    // #1173: the notice now carries the idle wording and the still-running
    // segment list. It is still pure metadata — nothing to recover — so it
    // must not become a tee artifact just because it grew.
    assert!(is_timeout_notice_only(
        "ERROR: command timed out after 200ms without new output\n\
             [still running at timeout: sleep 300]",
        124
    ));
    assert!(!is_timeout_notice_only(
        "useful output\nERROR: command timed out after 200ms\n\
             [still running at timeout: sleep 300]",
        124
    ));
    // Exit 124 from something that is not our watchdog carries no marker.
    assert!(!is_timeout_notice_only("some tool output", 124));
}

#[test]
fn unavailable_session_lock_rejects_explicit_cwd() {
    let error = resolve_effective_cwd(None, Some("/tmp/unvalidated"))
        .expect_err("an unavailable session lock must reject an unvalidated cwd");
    assert!(
        error.message.contains("cannot validate working directory"),
        "{error:?}"
    );
}

#[test]
fn untrusted_and_readonly_clients_cannot_run_shell_commands() {
    for role in ["untrusted", "readonly"] {
        let ctx = ToolContext {
            client_role: Some(role.to_string()),
            ..ToolContext::default()
        };
        let output = CtxShellTool
            .handle(&serde_json::Map::new(), &ctx)
            .expect("role denial must be a tool result");

        assert_eq!(output.shell_outcome, Some(ShellOutcome::Blocked));
        assert!(
            output.text.contains("SHELL ACCESS DENIED"),
            "{role}: {}",
            output.text
        );
        assert!(output.text.contains(role), "{role}: {}", output.text);
    }
}

#[test]
fn absent_shell_context_preserves_default_access() {
    assert!(shell_access_denial(&ToolContext::default()).is_none());
}

#[test]
fn explicitly_disabled_shell_access_blocks_the_request() {
    let ctx = ToolContext {
        shell_access: Some(false),
        ..ToolContext::default()
    };
    let output = CtxShellTool
        .handle(&serde_json::Map::new(), &ctx)
        .expect("shell-access denial must be a tool result");

    assert_eq!(output.shell_outcome, Some(ShellOutcome::Blocked));
    assert!(output.text.contains("shell_access=true"), "{}", output.text);
}

#[test]
fn detect_heredoc_reroute_python_quoted() {
    let cmd = "python3 - <<'PY'\nprint(1)\nPY";
    let (lang, code, rest) = detect_heredoc_reroute(cmd).expect("must detect python heredoc");
    assert_eq!(lang, "python");
    assert_eq!(code, "print(1)");
    assert!(rest.is_none());
}

#[test]
fn detect_heredoc_reroute_python_with_remainder() {
    let cmd = "python3 <<'PY'\nprint(1)\nPY\nnode --test file.js";
    let (lang, code, rest) = detect_heredoc_reroute(cmd).expect("must detect split heredoc");
    assert_eq!(lang, "python");
    assert_eq!(code, "print(1)");
    assert_eq!(rest.as_deref(), Some("node --test file.js"));
}

#[test]
fn detect_heredoc_reroute_unquoted_and_tab_stripped() {
    let unquoted = "ruby <<EOF\nputs 1\nEOF";
    let (lang, code, rest) = detect_heredoc_reroute(unquoted).unwrap();
    assert_eq!(lang, "ruby");
    assert_eq!(code, "puts 1");
    assert!(rest.is_none());

    let tabbed = "python3 <<-\tSCRIPT\n\tprint('ok')\nSCRIPT";
    let (lang, code, rest) = detect_heredoc_reroute(tabbed).unwrap();
    assert_eq!(lang, "python");
    assert_eq!(code, "\tprint('ok')");
    assert!(rest.is_none());
}

#[test]
fn detect_heredoc_reroute_rejects_compound_prefix() {
    assert!(detect_heredoc_reroute("echo hi; python3 <<'PY'\nx\nPY").is_none());
    assert!(detect_heredoc_reroute("python3 -c 'print(1)'").is_none());
    assert!(detect_heredoc_reroute("python3 <<'PY' | cat\nx\nPY").is_none());
}
