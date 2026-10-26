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
}

/// `$[ … ]` is arithmetic to bash/zsh and text with live quotes to dash, so
/// where it ends depends on the shell — refused everywhere.
#[test]
fn legacy_arithmetic_is_refused_everywhere() {
    assert_all_block(
        "echo,cat",
        &[
            "echo $[1+2]",
            "echo \"$[1+2]\"",
            // dash: `"$["` is a string, `]` a word, `" # "` a string, then
            // `evil_binary` runs; the old lexer skipped to `]` and took `#`
            // for a comment.
            "echo \"$[\"]\" # \"; evil_binary",
            "cat <<EOF\n$[1+2]\nEOF",
        ],
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
            "echo $((1<<2))",
            "f() { true; }; f",
            "x=$(true) && echo \"$x\"",
            "GIT_PAGER=cat git log",
            "PAGER=cat git log",
            "FOO=1 BAR=2 git status",
            "export FOO=1 && echo $FOO",
            "IFS= read -r line < /dev/null",
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
