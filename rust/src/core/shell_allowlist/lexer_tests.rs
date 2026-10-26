//! GHSA-rxv5-4c3x-g4q6 E: the shared lexer (`lexer.rs`) against inputs that
//! bash, dash or zsh execute but the gate used to read as comments, heredoc
//! bodies or quoted text.

use super::*;

/// Run `enforce_shell_allowlist` on every command under `allowlist`.
fn verdicts<'a>(allowlist: &str, commands: &[&'a str]) -> Vec<(&'a str, Result<(), ShellError>)> {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", allowlist);
    let results = commands
        .iter()
        .map(|c| (*c, enforce_shell_allowlist(c)))
        .collect();
    crate::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");
    results
}

fn assert_all_block(allowlist: &str, commands: &[&str]) {
    for (command, result) in verdicts(allowlist, commands) {
        assert!(result.is_err(), "must block: {command:?}");
    }
}

fn assert_all_pass(allowlist: &str, commands: &[&str]) {
    for (command, result) in verdicts(allowlist, commands) {
        assert!(result.is_ok(), "must pass: {command:?}: {result:?}");
    }
}

/// A: `#` inside `${…}` starts no comment.
#[test]
fn hash_inside_parameter_expansion_is_not_a_comment() {
    assert_all_block(
        "echo",
        &[
            "echo ${x:- #$(evil_binary)}",
            "echo ${x:-(#$(evil_binary))}",
            "echo ${x:-#`evil_binary`}",
            "echo \"${x:- #$(evil_binary)}\"",
            "echo ${x:- #}$(evil_binary)",
        ],
    );
    assert_eq!(strip_comments("echo ${x:- #a} # c"), "echo ${x:- #a} ");
}

/// A: constructs whose `${…}` end differs between the shells are refused.
#[test]
fn ambiguous_parameter_expansions_are_refused() {
    assert_all_block(
        "echo",
        &[
            // bash: `'` literal; dash/zsh: a quote.
            "echo \"${x:-'}'} $(echo ok)\"",
            // bash/dash end at the first `}`; zsh nests.
            "echo ${x:-{a} #$(evil_binary)}",
        ],
    );
}

/// B: a CR that is not part of CRLF is a word character to the shells.
#[test]
fn bare_carriage_return_is_refused_crlf_is_not() {
    assert_all_block(
        "echo",
        &[
            "echo a\r#$(evil_binary)",
            "echo a\rb",
            "echo a\u{2028}#$(evil_binary)",
            "echo a\u{2029}b",
        ],
    );
    assert_all_pass("echo", &["echo a\r\necho b", "echo a\r\n"]);
}

/// C: `<<` inside `${…}`, `$[…]` or `(( … ))` is no heredoc operator; one
/// whose terminator never follows is refused instead of swallowing the rest.
#[test]
fn shift_operators_and_unterminated_heredocs_hide_no_lines() {
    assert_all_block(
        "echo,cat",
        &[
            "echo ${x:-a<<EOF}\nevil_binary",
            "echo \"${x:-a<<EOF}\"\nevil_binary",
            "echo ${x:0:1<<EOF}\nevil_binary",
            "echo ${arr[1<<EOF]}\nevil_binary",
            "echo $[1<<EOF]\nevil_binary",
            "echo $[1<<2]\nevil_binary",
            "(( x = 1 << 2 ))\nevil_binary",
            "echo $((1<<EOF))\nevil_binary",
            "cat <<EOF\nbody\nevil_binary",
            "cat <<EOF\nbody\n EOF\nevil_binary",
            "cat <<'EOF'\nbody\nEOF \nevil_binary",
            "echo \"$(cat <<EOF)\"\nbody\nevil_binary",
        ],
    );
    assert_eq!(
        heredoc_delims("echo ${x:-a<<EOF}", false),
        Vec::<String>::new()
    );
    assert_eq!(
        heredoc_delims("echo $[1<<EOF]", false),
        Vec::<String>::new()
    );
}

/// D: a `#` right after a `)` that closed a group starts a comment, so its
/// apostrophe opens no string; after the `)` of `$(…)` it is part of a word.
#[test]
fn hash_after_a_group_close_is_a_comment() {
    assert_all_block(
        "echo,true",
        &[
            "(true)#'\necho $(evil_binary)",
            "f()#'\n{ true; }\necho $(evil_binary)",
            "case a in a)#'\ntrue;; esac\necho $(evil_binary)",
            "echo $(true)#$(evil_binary)",
        ],
    );
    assert_eq!(strip_comments("(true)#c"), "(true)");
    assert_eq!(strip_comments("echo $(a)#b"), "echo $(a)#b");
}

/// Continuations: only the ones the shell removes are joined.
#[test]
fn only_real_line_continuations_are_joined() {
    assert_all_block(
        "echo,cat",
        &[
            // `\\` is an escaped backslash; the newline runs the next line.
            "echo \\\\\nevil_binary",
            // `\`+CR+LF is an escaped CR and a line break.
            "echo a\\\r\nevil_binary",
            // `x\⏎EOF` is `xEOF` in an unquoted body: not the terminator.
            "cat <<EOF\nx\\\nEOF\nit's $(evil_binary)\nEOF",
        ],
    );
    assert_eq!(normalize_line_continuations("echo a \\\nb"), "echo a b");
    assert_eq!(normalize_line_continuations("echo \\\\\nb"), "echo \\\\\nb");
    assert_eq!(
        normalize_line_continuations("echo 'a\\\nb'"),
        "echo 'a\\\nb'"
    );
}

/// ANSI-C strings with an escaped quote end at different quotes in dash.
#[test]
fn ansi_c_escaped_quote_is_refused() {
    assert_all_block("echo", &[r"echo $'\'' '$(evil_binary)' $'\''"]);
    assert_all_pass("echo", &[r"echo $'a\tb'"]);
}

#[test]
fn ordinary_commands_keep_passing() {
    assert_all_pass(
        "echo,git,cat,ls,true",
        &[
            "echo ${HOME}",
            "echo ${x:-default}",
            "echo \"${x:-default}\" ${#x} ${x#pre} ${x%%.*}",
            "echo \"${x:-\"quoted default\"}\"",
            "echo $[1+2]",
            "echo $((1<<2))",
            "echo a#b",
            "ls # trailing comment",
            "# comment line\nls",
            "x=1 # it's fine\necho $x",
            "(true) # done",
            "git commit -m \"$(cat <<EOF\nfix: thing\n\nDon't (x).\nEOF\n)\"",
            "git commit -m \"$(cat <<'EOF'\nfeat(x): y\nEOF\n)\"",
            "cat <<-EOF\n\tbody\n\tEOF",
            "cat <<EOF",
            "echo a \\\n  b",
        ],
    );
}

/// Every input whose marker command actually runs in an installed POSIX
/// shell must be blocked. Deterministic and offline: the marker is a local
/// script that touches a file in a temp dir.
#[cfg(unix)]
#[test]
fn inputs_that_execute_the_marker_are_blocked() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Command, Stdio};

    let inputs = [
        "echo ${x:- #$(evil_marker)}",
        "echo ${x:-(#$(evil_marker))}",
        "echo \"${x:-'$(evil_marker)'}\"",
        "echo a\r#$(evil_marker)",
        "echo ${x:-a<<EOF}\nevil_marker",
        "x=abc; echo ${x:0:1<<EOF}\nevil_marker",
        "echo $[1<<EOF]\nevil_marker",
        "(true)#'\necho $(evil_marker)",
        "f()#'\n{ true; }\necho $(evil_marker)",
        "echo \"it's $(evil_marker)\"",
        "(true)>$(evil_marker)",
        "echo \\\\\nevil_marker",
        "cat <<EOF\nx\\\nEOF\nit's $(evil_marker)\nEOF",
        "echo $((1<<2))\nevil_marker",
        "(( x = 1 << 2 ))\nevil_marker",
        r"echo $'\'' '$(evil_marker)' $'\''",
        "cat <<EOF\n$(evil_marker)\nEOF",
        "echo a\u{2028}#$(evil_marker)",
        "echo $(true)#$(evil_marker)",
    ];
    let shells: Vec<&str> = ["/bin/bash", "/bin/dash", "/bin/zsh", "/bin/sh"]
        .into_iter()
        .filter(|s| std::path::Path::new(s).exists())
        .collect();
    assert!(!shells.is_empty(), "no POSIX shell to probe");

    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("evil_marker");
    let flag = dir.path().join("ran");
    std::fs::write(&marker, "#!/bin/sh\n: > \"$MARK\"\n").expect("marker script");
    std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o700))
        .expect("chmod marker");
    let path = format!(
        "{}:{}",
        dir.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let verdicts = verdicts("echo,cat,true", &inputs);
    let mut ran_somewhere = 0;
    for (input, verdict) in &verdicts {
        for shell in &shells {
            let _ = std::fs::remove_file(&flag);
            let _ = Command::new(shell)
                .arg("-c")
                .arg(input)
                .env("PATH", &path)
                .env("MARK", &flag)
                .current_dir(dir.path())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if flag.exists() {
                ran_somewhere += 1;
                assert!(
                    verdict.is_err(),
                    "{shell} runs the marker but the gate passes: {input:?}"
                );
            }
        }
    }
    assert!(ran_somewhere > 0, "the probe never ran the marker");
}
