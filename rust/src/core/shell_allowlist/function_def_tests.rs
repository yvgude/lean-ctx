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
        Some(" echo hi; echo bye; ")
    );
    assert_eq!(function_body("greet()"), None);
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
