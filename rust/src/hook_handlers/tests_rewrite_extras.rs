//! Overflow tests extracted from tests.rs to satisfy LOC gate (#660).
use super::*;

#[test]
fn rg_type_falls_through() {
    assert_eq!(
        rewrite_candidate("rg -t rust pattern src/", "lean-ctx"),
        Some(expect_wrapped("rg -t rust pattern src/", "lean-ctx"))
    );
}
#[test]
fn rg_glob_falls_through() {
    assert_eq!(
        rewrite_candidate("rg --glob=*.rs pattern src/", "lean-ctx"),
        Some(expect_wrapped("rg --glob=*.rs pattern src/", "lean-ctx"))
    );
}
#[test]
fn rg_context_falls_through() {
    assert_eq!(
        rewrite_candidate("rg -A5 pattern file.rs", "lean-ctx"),
        Some(expect_wrapped("rg -A5 pattern file.rs", "lean-ctx"))
    );
}
#[test]
fn rg_json_falls_through() {
    assert_eq!(
        rewrite_candidate("rg --json pattern src/", "lean-ctx"),
        Some(expect_wrapped("rg --json pattern src/", "lean-ctx"))
    );
}
// --- is_shell_tool covers Gemini/Antigravity tool names ---
#[test]
fn is_shell_tool_covers_all_ide_variants() {
    for name in [
        "run_command",
        "run_shell_command",
        "execute_command",
        "exec_command",
        "command_exec",
        "run_terminal",
        "runterminal",
        "run",
        "exec",
        "execute",
        "command",
        "cmd",
        "sh",
    ] {
        assert!(
            is_shell_tool(name),
            "{name} must be recognized as shell tool"
        );
    }
}
// BRE metacharacters whose meaning differs from the Rust regex crate must stay
// on native grep; egrep and plain unambiguous grep patterns keep the fast path.
#[test]
fn andis_real_world_grep_keeps_bre_alternation_on_native_grep() {
    // This test used to assert the opposite: that `\|` was rewritten onto
    // `lean-ctx grep` as long as the pattern text survived the trip. Preserving
    // the *text* is not preserving the *meaning* — `lean-ctx grep` compiles
    // with the Rust `regex` crate, which reads `\|` as a literal pipe, so the
    // rewrite turned a matching search into `0 matches` (#1827).
    //
    // The rewrite is therefore declined and the command falls through to the
    // `lean-ctx -c` wrap, where the platform's own grep applies BRE.
    let cmd = r#"grep -rn "func\|Interval\|Duration" src/"#;
    let rewritten = rewrite_candidate(cmd, "lean-ctx").expect("grep -rn is still wrapped");
    assert!(
        !rewritten.starts_with("lean-ctx grep"),
        "BRE alternation must not be handed to lean-ctx grep: {rewritten}"
    );
    assert!(
        rewritten.starts_with("lean-ctx -c"),
        "must fall through to the -c wrap so native grep resolves it: {rewritten}"
    );
    assert!(
        rewritten.contains("func") && rewritten.contains("Interval"),
        "pattern must be preserved: {rewritten}"
    );
}

#[test]
fn egrep_alternation_still_routes_to_lean_ctx_grep() {
    // ERE and the Rust `regex` crate agree, so `egrep` keeps the fast path.
    let rewritten = rewrite_candidate(r#"egrep -rn "func|Interval" src/"#, "lean-ctx")
        .expect("egrep must be rewritten");
    assert!(
        rewritten.starts_with("lean-ctx grep"),
        "egrep is ERE and maps losslessly: {rewritten}"
    );
}

#[test]
fn bare_bre_metachars_are_literals_and_must_not_be_handed_over() {
    // The other direction of the same defect: in BRE a bare `|` or `+` is a
    // literal, but `regex` reads them as operators — which reports matches on
    // lines native grep never matches. Both directions take the -c wrap.
    for pattern in [
        r#"grep -n "headroom|HEADROOM" db.py"#,
        r#"grep -n "headroom+" db.py"#,
        r#"grep -n "set_(headroom)" db.py"#,
        r#"grep -n "o{2}" db.py"#,
    ] {
        let rewritten = rewrite_candidate(pattern, "lean-ctx").expect("still wrapped");
        assert!(
            !rewritten.starts_with("lean-ctx grep"),
            "{pattern} must not reach lean-ctx grep: {rewritten}"
        );
    }
}

#[test]
fn plain_bre_patterns_keep_the_fast_path() {
    // No ambiguous metacharacter — the pattern means the same on both sides,
    // so the rewrite (and its compression) is still applied.
    let rewritten =
        rewrite_candidate(r#"grep -rn "headroom" src/"#, "lean-ctx").expect("must be rewritten");
    assert!(
        rewritten.starts_with("lean-ctx grep"),
        "unambiguous BRE must keep the fast path: {rewritten}"
    );
}
