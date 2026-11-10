// SPDX-License-Identifier: Apache-2.0
//! Shell function definitions and inline-env error text (#1488, #1489).
//! Split out of `tests.rs` to keep that file under the LOC gate.

use super::*;

// ---------------------------------------------------------------------------
// #1488: shell function definitions must not be blocked by the allowlist.
// ---------------------------------------------------------------------------

#[test]
fn gh1488_function_definition_not_blocked() {
    let allowlist = allow(&["echo"]);
    let result = check_all_segments("greet() { echo hi; }; greet", &allowlist);
    assert!(
        result.is_ok(),
        "function definition + call should not be blocked: {result:?}"
    );
}

#[test]
fn gh1488_function_with_disallowed_body_is_blocked() {
    let allowlist = allow(&["echo", "ls"]);
    let result = check_all_segments("bad() { evil_command; }; bad", &allowlist);
    assert!(
        result.is_err(),
        "function with disallowed body command must be blocked"
    );
}

#[test]
fn gh1488_detect_function_def_forms() {
    use super::super::tokenizer::detect_function_def;
    assert_eq!(
        detect_function_def("greet() { echo hi; }"),
        Some("greet".into())
    );
    assert_eq!(
        detect_function_def("function greet { echo hi; }"),
        Some("greet".into())
    );
    assert_eq!(
        detect_function_def("function greet() { echo hi; }"),
        Some("greet".into())
    );
    assert_eq!(detect_function_def("echo hello"), None);
    assert_eq!(detect_function_def("ls -la"), None);
}

#[test]
fn gh1488_function_body_is_the_text_between_the_braces() {
    use super::super::tokenizer::function_body;
    assert_eq!(
        function_body("greet() { echo hi; echo bye; }"),
        Some("echo hi; echo bye;")
    );
    assert_eq!(function_body("greet()"), None);
}

// ---------------------------------------------------------------------------
// GH #2131: the compact, next-line and subshell-body function forms.
// ---------------------------------------------------------------------------

#[test]
fn gh2131_detect_function_def_compact_and_subshell_forms() {
    use super::super::tokenizer::{detect_function_def, function_body};
    for (segment, body) in [
        ("run(){ echo hi; }", "echo hi;"),
        ("run (){ echo hi; }", "echo hi;"),
        ("function run(){ echo hi; }", "echo hi;"),
        ("run()\n{\n  echo hi\n}", "echo hi"),
        ("function run\n{ echo hi; }", "echo hi;"),
        ("run() ( echo hi )", "echo hi"),
        ("run()( echo hi )", "echo hi"),
    ] {
        assert_eq!(
            detect_function_def(segment),
            Some("run".into()),
            "{segment:?}"
        );
        assert_eq!(function_body(segment), Some(body), "{segment:?}");
    }
    for segment in [
        // `{echo` is one word, not a group: bash rejects it as well.
        "run(){echo hi; }",
        // A redirect after the body runs on every call; it is not skipped.
        "run() { echo hi; } > $(curl x)",
        "run() ( echo hi ) curl x",
        "function run{ echo hi; }",
        "run() echo hi",
    ] {
        assert_eq!(detect_function_def(segment), None, "{segment:?}");
    }
}

#[test]
fn gh2131_function_forms_pass_the_allowlist() {
    let allowlist = allow(&["gh"]);
    for command in [
        "run(){ echo hi; }; run",
        "reply(){ gh api graphql; }; reply",
        "run()\n{\n  echo hi\n}\nrun",
        "run() ( echo hi ); run",
        "function run\n{\n  echo hi\n}\nrun",
    ] {
        let result = check_all_segments(command, &allowlist);
        assert!(result.is_ok(), "{command:?} must pass: {result:?}");
    }
}

#[test]
fn gh2131_function_body_commands_stay_gated() {
    let allowlist = allow(&["gh"]);
    for command in [
        "run(){ curl x; }; run",
        "run(){ echo hi; curl x; }; run",
        "run()\n{\n  echo hi\n  curl x\n}\nrun",
        "run() ( curl x ); run",
        "run() ( echo hi; curl x ); run",
        "run()\n( curl x )\nrun",
    ] {
        let err = check_all_segments(command, &allowlist)
            .expect_err("a disallowed command in a function body must be blocked")
            .to_string();
        assert!(err.contains("curl"), "{command:?}: {err}");
    }
    // A head without a body is still no definition, and a stray command on
    // the next line is still gated as a command.
    for command in ["run()\ncurl x", "run() { echo hi; } > $(curl x); run"] {
        assert!(
            check_all_segments(command, &allowlist).is_err(),
            "{command:?} must be blocked"
        );
    }
}

// ---------------------------------------------------------------------------
// GH #2002: control flow inside a function body is not a command.
// ---------------------------------------------------------------------------

#[test]
fn gh2002_control_flow_in_function_body_is_not_gated() {
    let allowlist = allow(&["echo", "true", "false"]);
    for command in [
        r#"f() { for i in 1 2; do echo "in function $i"; done; }; f"#,
        r#"g() { if true; then echo "if in function"; fi; }; g"#,
        "w() { while false; do echo never; done; echo ok; }; w",
        "function h { for i in 3; do echo $i; done; }; h",
    ] {
        let result = check_all_segments(command, &allowlist);
        assert!(result.is_ok(), "{command} must pass: {result:?}");
    }
}

#[test]
fn gh2002_commands_inside_body_control_flow_stay_gated() {
    let allowlist = allow(&["echo", "true"]);
    for command in [
        "f() { for i in 1 2; do evil_command $i; done; }; f",
        "g() { if true; then evil_command; fi; }; g",
        "h() { if evil_command; then echo y; fi; }; h",
    ] {
        let err = check_all_segments(command, &allowlist)
            .expect_err("a disallowed command inside body control flow must be blocked")
            .to_string();
        assert!(err.contains("evil_command"), "{command}: {err}");
        assert!(
            !err.contains("  shell allowlist"),
            "the message must not carry the stray whitespace run: {err}"
        );
    }
}

// ---------------------------------------------------------------------------
// A definition that may not run does not shadow the real command.
// ---------------------------------------------------------------------------

#[test]
fn conditional_function_definition_does_not_shadow_the_command() {
    let allowlist = allow(&["echo", "true", "false"]);
    // bash still runs the real `evil_command` after each of these.
    for command in [
        "false && evil_command(){ :; }; evil_command",
        "false && evil_command() { :; }; evil_command",
        "true || evil_command() { :; }; evil_command",
        "if false; then evil_command() { :; }; fi; evil_command",
        "evil_command() { :; } | echo; evil_command",
        "evil_command() { :; } & evil_command",
    ] {
        let err = check_all_segments(command, &allowlist)
            .expect_err("a conditional definition must not open its name")
            .to_string();
        assert!(err.contains("evil_command"), "{command:?}: {err}");
    }
    // A definition that always runs does: the call reaches the function.
    for command in [
        "evil_command(){ echo hi; }; evil_command",
        "echo a; evil_command() { echo hi; }\nevil_command",
        "evil_command() { echo hi; } && evil_command",
    ] {
        let result = check_all_segments(command, &allowlist);
        assert!(result.is_ok(), "{command:?} must pass: {result:?}");
    }
}

// ---------------------------------------------------------------------------
// GH #2202: the shell hook gates Claude Code's Bash call as
// `{ <command>\n} && pwd -P >| …`; a function defined and called in the
// command must pass there exactly as it passes `ctx_shell`.
// ---------------------------------------------------------------------------

/// What the shell hook gates for a Claude Code Bash call of `command`: the
/// line `Unwrapped::rebuild` in the Instance's `shell::agent_wrapper` emits
/// after looking through the host's scaffolding. Unwrapping itself lives in
/// the Instance crate and is tested there; this crate gates the result.
fn gh2202_hook_gated(command: &str) -> String {
    format!("{{ {command}\n}} && pwd -P >| /tmp/claude-2202-cwd")
}

#[test]
fn gh2202_function_call_passes_on_the_hook_path_like_ctx_shell() {
    let allowlist = allow(&["echo", "pwd"]);
    for command in [
        r#"reply() { echo "got $1"; }; reply hi"#,
        "reply() { echo \"got $1\"; }\nreply hi",
        "{ reply() { echo hi; }; }; reply",
    ] {
        let direct = check_all_segments(command, &allowlist);
        assert!(direct.is_ok(), "ctx_shell form {command:?}: {direct:?}");
        let gated = gh2202_hook_gated(command);
        let hook = check_all_segments(&gated, &allowlist);
        assert!(hook.is_ok(), "hook form {gated:?} must pass: {hook:?}");
    }
}

#[test]
fn gh2202_brace_group_definition_shadows_only_when_it_always_runs() {
    let allowlist = allow(&["echo", "true", "false", "pwd"]);
    for command in [
        "false && { evil_command() { :; }; }; evil_command",
        "{ evil_command() { :; }; } | echo; evil_command",
        "{ false && evil_command() { :; }; }; evil_command",
        "{ evil_command() { curl x; }; }; evil_command",
    ] {
        for line in [command.to_string(), gh2202_hook_gated(command)] {
            let err = check_all_segments(&line, &allowlist)
                .expect_err("a definition that may not run must not open its name")
                .to_string();
            assert!(
                err.contains("evil_command") || err.contains("curl"),
                "{line:?}: {err}"
            );
        }
    }
}

#[test]
fn gh2202_refused_function_call_never_suggests_allowing_the_function() {
    let allowlist = allow(&["echo", "true", "false"]);
    for command in [
        "false && reply() { echo hi; }; reply hi",
        "if false; then reply() { echo hi; }; fi; reply hi",
        "reply() { echo hi; } | echo; reply hi",
    ] {
        let err = check_all_segments(command, &allowlist)
            .expect_err("a call of a conditionally defined function stays gated")
            .to_string();
        assert!(err.contains("'reply'"), "{command:?}: {err}");
        assert!(
            err.contains("Do NOT run `lean-ctx allow reply`"),
            "{command:?}: {err}"
        );
        assert!(
            !err.contains("run  lean-ctx allow reply"),
            "must not suggest allowlisting a function name: {err}"
        );
    }
}

// ---------------------------------------------------------------------------
// #1489: inline env override error must mention the `env` parameter.
// ---------------------------------------------------------------------------

#[test]
fn gh1489_inline_env_block_message_mentions_env_parameter() {
    let result = check_all_segments("PATH=/evil/bin echo hi", &allow(&["echo"]));
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("env parameter"),
        "error must mention `env` parameter: {err}"
    );
    assert!(
        err.contains("ctx_shell"),
        "error must mention ctx_shell: {err}"
    );
}
