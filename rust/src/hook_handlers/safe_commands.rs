// SPDX-License-Identifier: Apache-2.0

//! The built-in set of shell commands that provably have no side effects.
//!
//! This is the ONE place that decides it. A hook may approve a rewritten
//! command only when the original would have run without a prompt on the host
//! anyway ("mirror, don't grant"). Hosts that take a rewrite only together with
//! an approval (Cursor, Copilot CLI, Codex) get a rewrite exactly for the
//! commands in this set; Claude Code also uses the subset its own docs list as
//! read-only ([`SafeCommand::claude_builtin`]).
//!
//! | Program | Accepted form |
//! | :- | :- |
//! | `git` | `status`, `log`, `diff`, `show`, `blame`; `branch` only listing. No global options (`-c`, `-C`, `--git-dir`), no `--output`, no `--ext-diff`, no unquoted glob |
//! | `ls`, `cat`, `head`, `tail`, `wc`, `grep` | any arguments |
//! | `rg` | without `--pre` (runs a preprocessor program) |
//! | `find` | without `-exec`, `-execdir`, `-ok`, `-okdir`, `-delete`, `-fprint*`, `-fls`, `-files0-from`, unquoted globs |
//! | `tree` | without `-o` (writes a file) and `-R` (writes `00Tree.html` files), also inside a cluster such as `-aR` |
//! | `pwd`, `which` | any arguments |
//! | `npm ls`/`npm list`, `docker ps`, `docker images` | subcommand directly after the program |
//!
//! Builds and tests run project code and are deliberately absent (`cargo
//! test`, `npm test`, `make`): a host approves those only from its own
//! configuration. So are `cargo tree` (it may rewrite `Cargo.lock` and reach
//! the registry) and `kubectl get` (`--server`/`--kubeconfig` send the user's
//! cluster credentials wherever the command names).
//!
//! The command must parse completely under a strict lexer: no command or
//! process substitution (`$(…)`, backticks, `<(…)`), no `$` expansion, no
//! backslash escapes, no subshells or braces, no background `&`, no newline,
//! no input redirection, and no output redirection except to `/dev/null` or
//! a file-descriptor duplicate (`2>&1`). A compound (`&&`, `||`, `;`, `|`,
//! `|&`) is safe only when every part is. A leading variable assignment
//! (`GIT_EXTERNAL_DIFF=… git diff`) or a program given by path is never
//! safe. Anything else is "not proven" and treated as unsafe.

/// One word of a segment, with quotes removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Word {
    pub(super) text: String,
    /// The word holds an unquoted glob character (`*`, `?`, `[`).
    pub(super) glob: bool,
}

/// One simple command of a compound, as written and as words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Segment {
    pub(super) raw: String,
    pub(super) words: Vec<Word>,
}

/// Split `cmd` into simple commands under the strict rules above. `None`
/// means the structure cannot be proven (and so nothing about it can be).
pub(super) fn parse_strict(cmd: &str) -> Option<Vec<Segment>> {
    let chars: Vec<char> = cmd.chars().collect();
    let mut segments = Vec::new();
    let mut words: Vec<Word> = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut glob = false;
    let mut seg_start = 0usize;
    let mut i = 0usize;

    let flush_word =
        |words: &mut Vec<Word>, word: &mut String, in_word: &mut bool, glob: &mut bool| {
            if *in_word {
                words.push(Word {
                    text: std::mem::take(word),
                    glob: *glob,
                });
            }
            *in_word = false;
            *glob = false;
        };

    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' => flush_word(&mut words, &mut word, &mut in_word, &mut glob),
            '\'' => {
                in_word = true;
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    word.push(chars[i]);
                    i += 1;
                }
                if i >= chars.len() {
                    return None;
                }
            }
            '"' => {
                in_word = true;
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    if matches!(chars[i], '$' | '`' | '\\') {
                        return None;
                    }
                    word.push(chars[i]);
                    i += 1;
                }
                if i >= chars.len() {
                    return None;
                }
            }
            ';' | '|' | '&' => {
                let op_len = match (c, chars.get(i + 1)) {
                    ('|', Some('|' | '&')) | ('&', Some('&')) => 2,
                    (';' | '|', _) => 1,
                    // A lone `&` runs the command in the background.
                    _ => return None,
                };
                flush_word(&mut words, &mut word, &mut in_word, &mut glob);
                let raw: String = chars[seg_start..i].iter().collect();
                if words.is_empty() {
                    return None;
                }
                segments.push(Segment {
                    raw: raw.trim().to_string(),
                    words: std::mem::take(&mut words),
                });
                i += op_len;
                seg_start = i;
                continue;
            }
            '>' => {
                // Only `>/dev/null`, `1>/dev/null`, `2>/dev/null`, `2>&1`, `1>&2`.
                if in_word && !(word == "1" || word == "2") {
                    return None;
                }
                word.clear();
                in_word = false;
                i += 1;
                if chars.get(i) == Some(&'>') {
                    return None;
                }
                while chars.get(i) == Some(&' ') {
                    i += 1;
                }
                let rest: String = chars[i..].iter().collect();
                let target_len = ["&1", "&2", "/dev/null"]
                    .iter()
                    .find(|t| {
                        rest.starts_with(**t)
                            && rest[t.len()..]
                                .chars()
                                .next()
                                .is_none_or(|n| matches!(n, ' ' | '\t' | ';' | '|' | '&'))
                    })
                    .map(|t| t.chars().count())?;
                i += target_len;
                continue;
            }
            '\n' | '\r' | '\\' | '$' | '`' | '<' | '(' | ')' | '{' | '}' => return None,
            '#' if !in_word => return None,
            '*' | '?' | '[' => {
                in_word = true;
                glob = true;
                word.push(c);
            }
            _ => {
                in_word = true;
                word.push(c);
            }
        }
        i += 1;
    }
    flush_word(&mut words, &mut word, &mut in_word, &mut glob);
    if words.is_empty() {
        return None;
    }
    let raw: String = chars[seg_start..].iter().collect();
    segments.push(Segment {
        raw: raw.trim().to_string(),
        words,
    });
    Some(segments)
}

/// How a segment of the safe set may be treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SafeCommand {
    /// Listed by Claude Code as a built-in read-only command that runs
    /// without a prompt ("`ls`, `cat`, `echo`, `pwd`, `head`, `tail`,
    /// `grep`, `find`, `wc`, `which`, `diff`, `stat`, `du`, `cd`, and
    /// read-only forms of `git`", code.claude.com/docs/en/permissions).
    pub(super) claude_builtin: bool,
}

/// `Some` when one simple command is in the safe set.
pub(super) fn classify_segment(seg: &Segment) -> Option<SafeCommand> {
    let words: Vec<&str> = seg.words.iter().map(|w| w.text.as_str()).collect();
    let (program, args) = words.split_first()?;
    let first = &seg.words[0];
    // A program by path, a glob or a leading `NAME=value` is not the bare
    // program the table names.
    if first.glob || program.contains('/') || program.contains('=') || program.is_empty() {
        return None;
    }
    let any_glob = seg.words.iter().any(|w| w.glob);
    let has = |forbidden: &[&str]| {
        args.iter().any(|a| {
            forbidden
                .iter()
                .any(|f| a == f || a.starts_with(&format!("{f}=")))
        })
    };
    let claude = SafeCommand {
        claude_builtin: true,
    };
    let other = SafeCommand {
        claude_builtin: false,
    };
    match *program {
        "ls" | "cat" | "head" | "tail" | "wc" | "grep" | "pwd" | "which" => Some(claude),
        "rg" => (!has(&["--pre", "--pre-glob"])).then_some(other),
        "find" => (!any_glob
            && !has(&[
                "-exec",
                "-execdir",
                "-ok",
                "-okdir",
                "-delete",
                "-fprint",
                "-fprint0",
                "-fprintf",
                "-fls",
                "-files0-from",
            ]))
        .then_some(claude),
        // tree parses short options as clusters (`-aR`, `-ao out`).
        "tree" => (!args.iter().any(|a| {
            a.starts_with("--output")
                || (a.starts_with('-') && !a.starts_with("--") && a.contains(['o', 'R']))
        }))
        .then_some(other),
        "git" => git_is_safe(args, any_glob).map(|claude_builtin| SafeCommand { claude_builtin }),
        "npm" => matches!(args.first(), Some(&("ls" | "list"))).then_some(other),
        "docker" => matches!(args.first(), Some(&("ps" | "images"))).then_some(other),
        _ => None,
    }
}

/// `Some(claude_builtin)` when the git invocation only reads. Only the plain
/// inspection subcommands count as Claude Code built-ins; `blame` and
/// listing `branch` are safe but not named in Claude's documentation, so
/// Claude may still prompt for them.
fn git_is_safe(args: &[&str], any_glob: bool) -> Option<bool> {
    let (sub, rest) = args.split_first()?;
    // Unquoted globs could expand to an option; Claude Code prompts for them too.
    if any_glob {
        return None;
    }
    let writes = rest.iter().any(|a| {
        *a == "-o"
            || a.starts_with("--output")
            || *a == "--ext-diff"
            || a.starts_with("--exec")
            || a.starts_with("--upload-pack")
    });
    if writes {
        return None;
    }
    match *sub {
        "status" | "log" | "diff" | "show" => Some(true),
        "blame" => Some(false),
        "branch" => git_branch_lists_only(rest).then_some(false),
        _ => None,
    }
}

/// `git branch` lists unless it is given a branch to create, delete, move,
/// copy or configure.
fn git_branch_lists_only(args: &[&str]) -> bool {
    const LISTING: &[&str] = &[
        "-a",
        "--all",
        "-r",
        "--remotes",
        "-v",
        "-vv",
        "--verbose",
        "-l",
        "--list",
        "--show-current",
        "--merged",
        "--no-merged",
        "--contains",
        "--no-contains",
        "--points-at",
        "--sort",
        "--format",
        "--color",
        "--no-color",
        "--column",
        "--no-column",
        "-i",
        "--ignore-case",
        "--abbrev",
        "--no-abbrev",
    ];
    // These take (or allow) a commit or pattern argument while still listing.
    const TAKES_OPERAND: &[&str] = &[
        "-l",
        "--list",
        "--merged",
        "--no-merged",
        "--contains",
        "--no-contains",
        "--points-at",
    ];
    let mut positional_ok = false;
    for arg in args {
        if arg.starts_with('-') {
            let name = arg.split('=').next().unwrap_or(arg);
            if !LISTING.contains(&name) {
                return false;
            }
            positional_ok |= TAKES_OPERAND.contains(&name);
        } else if !positional_ok {
            return false;
        }
    }
    true
}

/// The segments of `cmd` when every one of them is in the safe set.
pub(super) fn safe_segments(cmd: &str) -> Option<Vec<(Segment, SafeCommand)>> {
    parse_strict(cmd)?
        .into_iter()
        .map(|seg| classify_segment(&seg).map(|safe| (seg, safe)))
        .collect()
}

/// Whether `cmd` provably has no side effects (see the module docs).
pub(super) fn is_side_effect_free(cmd: &str) -> bool {
    safe_segments(cmd).is_some()
}

#[cfg(test)]
#[path = "safe_commands_tests.rs"]
mod tests;
