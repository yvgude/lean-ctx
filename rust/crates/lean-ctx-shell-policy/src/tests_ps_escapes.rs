// SPDX-License-Identifier: Apache-2.0
//! PowerShell here-strings and backtick escapes through the whole gate, as
//! `ctx_shell` and a host's PowerShell tool run it on a PowerShell host.

use super::ps_statements::HostGuard;

fn gate(cmd: &str, powershell: bool) -> Result<(), String> {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let _allowlist = lean_ctx::test_env::VarGuard::set(
        "LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE",
        "git,echo,sleep,true,ls",
    );
    let _host = HostGuard::powershell(powershell);
    super::enforce_shell_allowlist(cmd).map_err(|err| err.to_string())
}

fn blocked(cmds: &[&str], powershell: bool) {
    for cmd in cmds {
        assert!(
            gate(cmd, powershell).is_err(),
            "must be blocked (powershell host: {powershell}): {cmd:?}"
        );
    }
}

fn passes(cmds: &[&str], powershell: bool) {
    for cmd in cmds {
        if let Err(err) = gate(cmd, powershell) {
            panic!("should pass (powershell host: {powershell}): {cmd:?}\n{err}");
        }
    }
}

/// A here-string `@'…'@` / `@"…"@` opens and closes on line-anchored
/// delimiters; the gate paired its inner quotes as ordinary quotes, so a `;`
/// and a command after the closing delimiter stood inside one "argument".
#[test]
fn here_strings_cannot_hide_a_command() {
    blocked(
        &[
            "echo @'\n'\"\n'@; evil_binary #\"",
            "echo @\"\n\"'\n\"@; evil_binary #'",
            "echo @'\r\n'\"\r\n'@; evil_binary #\"",
            "echo @' \t\n'\"\n'@; evil_binary #\"",
            "echo @\"\nx $(evil_binary)\n\"@",
        ],
        true,
    );
}

/// The gate does not follow a here-string's line-anchored delimiters, so a
/// here-string with no hidden command is refused too under the PowerShell
/// host (an over-block, pinned here): its text belongs in a file written with
/// the file-write tool.
#[test]
fn here_strings_are_refused_under_powershell() {
    blocked(&["echo @'\nhello\n'@", "echo @\"\nhello $x\n\"@"], true);
    // `@'` not followed by a line break opens no here-string: `@` is an
    // ordinary character there and the quote a quote.
    passes(&["echo @'x'", "echo @\"x\"", "echo a@'b'"], true);
}

/// The backtick is PowerShell's escape character. Read as a substitution
/// whose closing-backtick scan ignores quotes, a backtick right before a quote
/// shielded a `; command` from the segment split; before `#` it made the
/// substitution's body a comment.
#[test]
fn backtick_cannot_shield_a_quote_or_separator() {
    blocked(
        &[
            "echo `'; evil_binary; '`'",
            "echo x`'; evil_binary; '`'",
            "echo \"`'; evil_binary; '`'\"",
            "echo `#; evil_binary",
            // Refused before this fix too: must stay so.
            "echo `\"; evil_binary; \"`\"",
            "echo `; evil_binary",
            "echo `| evil_binary",
            "echo `& evil_binary",
            // `` `$ `` is NOT escaped by the new rule (see below), but this
            // still blocks the old way: the backtick body `$x; evil_binary`
            // is scanned and `evil_binary` fails the allowlist.
            "echo `$x; evil_binary",
        ],
        true,
    );
}

/// A backtick before `$` is left alone. Escaping a `$` (`` `$5 ``, `` `$( ``)
/// is a common, legitimate PowerShell form that hides no separator, so the
/// rule does not refuse it — refusing it would only add false positives. It
/// is dropped from the escaped-delimiter set on purpose; a maintainer who
/// wants it refused adds `b'$'` back to that set in lexer.rs.
#[test]
fn backtick_before_dollar_is_not_over_blocked() {
    // Bare `` `$ `` at top level is no longer refused by the escape rule.
    // (A `` `$ `` inside a double-quoted string can still block for an
    // unrelated, pre-existing reason; that is not this rule's doing.)
    passes(&["echo `$x", "echo `$5"], true);
}

/// Backticks elsewhere keep their verdict: literal inside single quotes, and
/// `` `n ``/`` `t `` in a double-quoted string still read as a substitution
/// whose body (`nb`) meets the allowlist and fails it, as before this fix.
#[test]
fn backtick_escapes_of_ordinary_characters_are_unchanged() {
    passes(&["echo hi", "echo 'a`'", "echo 'a`nb'"], true);
    blocked(&["echo \"a`nb\"", "echo \"a`tb`n\"", "echo a`nb"], true);
}

/// POSIX hosts keep their own reading: a backtick pair is a substitution
/// whose body meets the allowlist, `@'` opens an ordinary quoted word and the
/// shell runs nothing behind it.
#[test]
fn posix_hosts_are_unchanged() {
    passes(
        &[
            "echo @'\n'\"\n'@; evil_binary #\"",
            "echo @'\nhello\n'@",
            "echo `echo x`",
            "echo `#; evil_binary",
        ],
        false,
    );
    blocked(
        &[
            "echo `evil_binary`",
            "echo `'; evil_binary; '`'",
            "echo \"a`nb\"",
        ],
        false,
    );
}

/// After the stop-parsing token `--%` PowerShell passes the rest of the line
/// to a native program verbatim up to a `|` or line break: quotes no longer
/// pair, so `echo --% '| evil_binary #'` pipes into `evil_binary` while the
/// quote rules read one quoted word. Under the PowerShell host every `--%`,
/// quoted or not, is refused unless glued into a plain word.
#[test]
fn stop_parsing_token_cannot_hide_a_command() {
    blocked(
        &[
            // Passed the gate before the fix.
            "echo --% '| evil_binary #'",
            "git log --% '| evil_binary #'",
            "echo --% \"| evil_binary #\"",
            "git log --% x",
            // Refused before only because `--%` is no git global option.
            "git --% '| evil_binary #'",
            "echo a; git --% '| evil_binary'",
            "git --%",
            // Refused conservatively: pwsh 7.4.6 stops parsing only at ASCII
            // `--%` (Windows PowerShell 5.1 untested). Typographic dashes
            // (U+2013, U+2014, U+2015) and a `,` before the token.
            "echo \u{2013}-% '| evil_binary #'",
            "echo -\u{2013}% '| evil_binary #'",
            "echo \u{2014}\u{2014}% '| evil_binary #'",
            "echo \u{2015}-% '| evil_binary #'",
            "echo a,--% '| evil_binary #'",
            // Any whitespace PowerShell splits words at starts the token
            // (re-review note 23484, piped in pwsh 7.4.6).
            "echo a\u{00A0}--% '| evil_binary #'",
            "echo a\u{000B}--% '| evil_binary #'",
            "echo a\u{000C}--% '| evil_binary #'",
            "echo a\u{0085}--% '| evil_binary #'",
            "echo a\u{2003}--% '| evil_binary #'",
            "echo a\u{3000}--% '| evil_binary #'",
            "echo a\r--% '| evil_binary #'",
            "git log\u{00A0}--% '| evil_binary #'",
            // A token also starts after a closing quote, `}`, `)` or a
            // redirection (re-review note 23512, piped in pwsh 7.4.6).
            "echo 'x'--% '| evil_binary #'",
            "echo \"x\"--% '| evil_binary #'",
            "echo \u{2018}x\u{2019}--% '| evil_binary #'",
            "echo {x}--% '| evil_binary #'",
            "echo $(echo x)--% '| evil_binary #'",
            "echo @{a=1}--% '| evil_binary #'",
            "echo a 2>&1--% '| evil_binary #'",
            "echo a 2>&1--%",
            "echo (1)--% '| evil_binary #'",
            // A token also starts after a member access (re-review note
            // 23520, piped in pwsh 7.4.6).
            "echo 'x'.Length--% '| evil_binary #'",
            "echo \"x\".Length--% '| evil_binary #'",
            "echo $(echo x).x--% '| evil_binary #'",
            "echo $x.y--% '| evil_binary #'",
            "echo $x.Length--% '| evil_binary #'",
            "git log $x.y--% '| evil_binary #'",
            // An argument whose value is exactly `--%` makes pwsh re-split
            // the following native arguments at spaces (note 23520): git
            // would receive `-u evil_binary` as options.
            "git clone \"--%\" 'x -u evil_binary d'",
            "git clone '--%' 'x -u evil_binary d'",
            "git clone \"$e--%\" 'x -u evil_binary d'",
            "git clone $nope--% 'x -u evil_binary d'",
            "echo '--%'",
            "echo \"--%\"",
            "echo \u{2018}--%\u{2019}",
            "git log --grep='a --%'",
        ],
        true,
    );
    // PowerShell passes `——`/`––` to git unchanged, so they end no option
    // scan: git still parses `-u` (upload-pack command) after them.
    blocked(
        &[
            "git clone \u{2014}\u{2014} -u evil_binary d",
            "git clone \u{2013}\u{2013} -u evil_binary d",
            "git clone \u{2015}\u{2015} -u evil_binary d",
        ],
        true,
    );
    // Glued into a plain word: an ordinary argument.
    passes(
        &[
            "git log --format=--%",
            "git log --grep=--%",
            "git log --format=%H",
            "echo a--%",
            "echo x=--%",
            "echo -x--%",
            "echo 100%",
            "git log -- path",
            "git diff -- '%'",
        ],
        true,
    );
    // POSIX hosts: an ordinary word.
    passes(&["git log --% x"], false);
}

/// PowerShell ends a braced variable `${…}` at the first `}`: quotes inside
/// are part of the name. Read the shell way, `${x'}; evil_binary #'}` was one
/// word with a quoted default, while PowerShell ran `evil_binary`.
#[test]
fn braced_variable_cannot_hide_a_command() {
    blocked(
        &[
            // Passed the gate before the fix.
            "echo ${x'}; evil_binary #'}",
            "echo ${x\"}; evil_binary #\"}",
            "echo ${x:-'}; evil_binary #'}",
            "echo \"${x'}\"; evil_binary; \"'}\"",
            "git log ${x'}| evil_binary #'}",
            // Other bytes the two readings could disagree on.
            "echo ${x$(echo)}",
            "echo ${x`}; evil_binary #}",
            "echo ${x(}; evil_binary #)}",
            "echo ${x\n}",
            "echo ${x",
        ],
        true,
    );
    passes(
        &[
            "echo ${x}",
            "echo ${env:PATH}",
            "echo \"${x}\"",
            "echo ${x}${y}",
            // Single quotes expand nothing: no braced variable there.
            "echo '${x$y}'",
        ],
        true,
    );
    // POSIX hosts keep the shell reading.
    passes(&["echo ${x:-'a b'}"], false);
}

/// PowerShell has no here-documents: `a<<EOF` is one argument and the next
/// line runs as a command, while the gate read that line as here-document
/// text (review note 23601, piped in pwsh 7.4.6).
#[test]
fn heredoc_operator_cannot_hide_a_command_under_powershell() {
    blocked(
        &[
            // Passed the gate before the fix.
            "echo a<<EOF\nevil_binary\nEOF",
            "echo a<<'EOF'\nevil_binary\nEOF",
            "echo a <<EOF\nevil_binary\nEOF",
            "git log <<-EOF\nevil_binary\nEOF",
            "echo <<EOF",
        ],
        true,
    );
    // POSIX hosts keep their here-documents.
    passes(&["echo a <<EOF\nhello\nEOF"], false);
}

/// PowerShell starts a `#` comment wherever a token ends — after a closing
/// quote, `}`, `)`, a member access, `,`, `-Name:`, a redirection or a
/// non-ASCII blank — while the gate read one word there, so a quote after the
/// `#` paired lines away (review note 23601, piped in pwsh 7.4.6).
#[test]
fn attached_comment_cannot_hide_a_command_under_powershell() {
    blocked(
        &[
            // Passed the gate before the fix.
            "echo 'a'#'\nevil_binary\n#'",
            "echo \"a\"#'\nevil_binary\n#'",
            "echo {a}#'\nevil_binary\n#'",
            "echo $(echo a)#'\nevil_binary\n#'",
            "echo (1)#'\nevil_binary\n#'",
            "echo $x.y#'\nevil_binary\n#'",
            "echo a,#'\nevil_binary\n#'",
            "echo -Path:#'\nevil_binary\n#'",
            "echo a 2>&1#'\nevil_binary\n#'",
            "echo a\u{00A0}#'\nevil_binary\n#'",
            "echo a\u{000B}#'\nevil_binary\n#'",
            "echo a\u{000C}#'\nevil_binary\n#'",
            "echo a\u{0085}#'\nevil_binary\n#'",
            "echo a\u{2003}#'\nevil_binary\n#'",
            "echo a\r#'\nevil_binary\n#'",
            // An index `$x[ … ]` is expression mode: `0#` is a number and a
            // comment (re-review note 23662, ran in pwsh 7.4.6).
            "echo $env:PATH[ 0#'\n]\nevil_binary\n#']",
            "git log $x[ 0#'\n]\nevil_binary\n#']",
            "echo $x[ 1#'\n]\nevil_binary\n#']",
            "echo $x.y[ 1#'\n]\nevil_binary\n#']",
            "echo (1 + 1#'\n)\nevil_binary\n#'",
            "echo @{a=1#'\n}\nevil_binary\n#'",
        ],
        true,
    );
    // pwsh reads these as one generic token with a literal `#`.
    passes(
        &[
            "echo a#b",
            "git log --format=#%H",
            "git log --grep=a#b",
            "echo -x#c",
            "echo HEAD~1#c",
            "echo a.b/c#d",
            "echo hi # a comment",
            "echo hi #'",
            "echo 'a#b'",
            "echo \"a#b\"",
        ],
        true,
    );
    // POSIX hosts: `'a'#'…` is one word.
    passes(&["echo 'a'#'\nx\n#'"], false);
}
