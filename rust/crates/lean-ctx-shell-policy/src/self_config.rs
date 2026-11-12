// SPDX-License-Identifier: Apache-2.0
//! Agent shells must not loosen lean-ctx's own guardrails.
//!
//! `lean-ctx` is on the default allowlist (agents legitimately run `read`,
//! `gain`, `doctor`, …), so without this check a prompt-injected agent could
//! run `lean-ctx yolo --yes`, `lean-ctx allow curl` or `lean-ctx config set
//! shell_security off` through its shell and then do what the allowlist had
//! just refused. Rejection messages even name those commands — for the user
//! to run.
//!
//! A lean-ctx call from an agent shell (the hooks, inside `lean-ctx -c`,
//! `ctx_shell`) may run only the subcommands listed here: views, reads and
//! searches, the `-c`/`-t` wrapper (whose inner command is checked like any
//! other line), the read actions of `config`/`knowledge`/`session`, `config
//! set` of a presentation/compression key, and the forms that tighten. Every
//! other subcommand — config, policy, hooks, trust, allowlist, addons, daemon
//! and proxy state, install/setup/wrap/migrate/import — is refused. The words
//! that select what runs must be literal: a `$`, a backtick, or an unquoted
//! brace, glob or `~` could turn into another subcommand after this check.
//! The user runs the refused commands in their own terminal, where no gating
//! applies.

use std::cell::Cell;

use lean_ctx::core::error::ShellError;

#[path = "download_targets.rs"]
mod download_targets;
use download_targets::is_relative_operand;
pub use download_targets::{UNKNOWN_DOWNLOAD_TARGET, download_targets};

/// Subcommands that only read or show, whatever their arguments; only the
/// subcommand word itself must be literal.
const VIEW_SUBCOMMANDS: &[&str] = &[
    "read",
    "grep",
    "glob",
    "find",
    "ls",
    "diff",
    "deps",
    "search",
    "semantic-search",
    "search-code",
    "explore",
    "repomap",
    "repo-map",
    "overview",
    "summary",
    "graph",
    "smells",
    "heatmap",
    "index",
    "gain",
    "spend",
    "savings",
    "stats",
    "status",
    "health",
    "roi",
    "value-report",
    "value_report",
    "output-savings",
    "output_savings",
    "token-report",
    "report-tokens",
    "inspect",
    "introspect",
    "gotchas",
    "bugs",
    "cheat",
    "cheatsheet",
    "cheat-sheet",
    "safety-levels",
    "safety",
    "instructions",
    "discover",
    "ledger",
    "evidence",
    "proof",
    "verify",
    "audit",
    "compliance",
    "doctor",
    "help",
    "--help",
    "-h",
    "--version",
    "-V",
    "-v",
    "version",
];

/// The wrapper forms whose inner command is itself a shell line.
const WRAPPER_SUBCOMMANDS: &[&str] = &["-c", "exec", "-t", "--track"];

/// Programs that would hand lean-ctx arguments the check cannot see.
const ARGUMENT_FEEDERS: &[&str] = &["xargs", "parallel", "find", "rush"];

/// Keys an agent shell may change with `config set`: how lean-ctx presents
/// and compresses output. The [`lean_ctx::core::config`] project-key class plus
/// the theme; none of them can loosen security, destinations, redaction,
/// tools, hooks, rules, policy or upstreams.
#[must_use]
pub(crate) fn agent_settable_config_key(key: &str) -> bool {
    key == "theme" || key == "tee_on_error" || lean_ctx::core::config::is_project_key(key)
}

/// True when `word` (as written, quotes kept) reaches the program as itself:
/// no `$` or backtick outside single quotes, and no brace, glob or `~`
/// outside quotes.
fn is_literal(word: &str) -> bool {
    let mut single = false;
    let mut double = false;
    let mut chars = word.chars().peekable();
    let mut first = true;
    while let Some(c) = chars.next() {
        let at_start = std::mem::replace(&mut first, false);
        match c {
            '\\' if !single => {
                chars.next();
            }
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            '$' | '`' if !single => return false,
            '{' | '}' | '*' | '?' | '[' if !single && !double => return false,
            '~' if !single && !double && at_start => return false,
            _ => {}
        }
    }
    true
}

/// The arguments after the lean-ctx program word in `segment`, as written
/// (quotes kept), or `None` when no word is lean-ctx.
fn raw_leanctx_args(segment: &str) -> Option<Vec<&str>> {
    let mut rest = segment.trim();
    let mut words = Vec::new();
    while !rest.is_empty() {
        let end = match super::tokenizer::quote_aware_token_end(rest) {
            0 => rest.chars().next().map_or(rest.len(), char::len_utf8),
            n => n.min(rest.len()),
        };
        words.push(&rest[..end]);
        rest = rest[end..].trim_start();
    }
    let at = words.iter().position(|w| is_leanctx_word(w))?;
    Some(words.split_off(at + 1))
}

fn is_leanctx_word(word: &str) -> bool {
    let bare: String = word.chars().filter(|c| !matches!(c, '"' | '\'')).collect();
    matches!(
        bare.rsplit(['/', '\\']).next(),
        Some("lean-ctx" | "lean-ctx.exe")
    )
}

fn is_help(args: &[String]) -> bool {
    matches!(args, [only] if matches!(only.as_str(), "-h" | "--help" | "help"))
}

/// How a permitted lean-ctx call is checked further.
enum Verdict {
    /// Runs as written; only the subcommand word must be literal.
    View,
    /// Runs as written; every argument must be literal.
    Literal,
    /// A wrapper: its inner command (`args[skip..]`) is checked as a line.
    Wrapper(usize),
}

/// What an agent shell may run of `lean-ctx args…`; `Err` names the call.
fn classify(args: &[String]) -> Result<Verdict, String> {
    let sub = args[0].as_str();
    let rest = &args[1..];
    let next = rest.first().map(String::as_str);
    let third = rest.get(1).map(String::as_str);
    let refused = |what: String| Err(what);
    if WRAPPER_SUBCOMMANDS.contains(&sub) {
        return Ok(Verdict::Wrapper(1 + usize::from(next == Some("--raw"))));
    }
    if sub == "doctor" && rest.iter().any(|a| matches!(a.as_str(), "--fix" | "fix")) {
        return refused("lean-ctx doctor --fix".to_string());
    }
    if sub == "gain"
        && rest
            .iter()
            .any(|a| matches!(a.as_str(), "--web" | "--live"))
    {
        return refused(format!("lean-ctx gain {}", rest.join(" ")));
    }
    // A view whose flags can be refused takes literal arguments only: the
    // check above reads the flags as written (`f=--fix; lean-ctx doctor $f`).
    if matches!(sub, "doctor" | "gain") {
        return Ok(Verdict::Literal);
    }
    // Only the reads take words the shell expands (`lean-ctx read "$f"`);
    // a view can write a file it is given (`graph export-html --out "$D"`).
    if VIEW_SUBCOMMANDS.contains(&sub) {
        return Ok(if LEANCTX_READ_SUBCOMMANDS.contains(&sub) {
            Verdict::View
        } else {
            Verdict::Literal
        });
    }
    if is_help(rest) {
        return Ok(Verdict::Literal);
    }
    let allowed = match sub {
        // Tightening.
        "secure" | "lockdown" | "untrust" => true,
        "allow" | "allow-path" => matches!(
            next,
            None | Some("--list" | "list" | "ls" | "--remove" | "-r" | "remove" | "rm")
        ),
        "security" => matches!(
            (next, third),
            (None | Some("status"), _)
                | (
                    Some("secrets" | "secret"),
                    Some("on" | "enable" | "true" | "status")
                )
        ),
        "config" => match next {
            None | Some("show" | "effective" | "path" | "which" | "schema" | "validate") => true,
            Some("set") => third.is_some_and(agent_settable_config_key),
            _ => false,
        },
        "knowledge" => match next {
            Some(
                "recall" | "search" | "status" | "health" | "export" | "stale" | "outdated"
                | "conflicts",
            ) => true,
            // A dry run only; merging is the person's (or the Queen's) call.
            Some("dedupe") => !rest.iter().any(|a| a == "--apply"),
            // Hive's reviewed knowledge reaches AGENTS.md only when a person
            // exports it (owner decision 22.10.).
            Some("export-agents-md") => !rest.iter().any(|a| a == "--include-hive"),
            _ => false,
        },
        "session" => matches!(next, Some("status")),
        // A Workspace bounds the agent itself (#2120): `remove` narrows,
        // `show`/`diagnose` only read.
        "workspace" => lean_ctx::core::workspace_args::agent_shell_may_run(rest),
        "addon" | "addons" => matches!(next, None | Some("list" | "ls")),
        "policy" => matches!(
            (next, third),
            (None | Some("show" | "status" | "list"), _)
                | (Some("org"), Some("verify" | "status" | "show" | "list"))
        ),
        "agent" => matches!(next, Some("presence" | "list" | "status")),
        // Hive: an agent may work (tasks, commons, bus), pause or stop itself;
        // never connect, link, grant, consent or lift the person's pause.
        "hive" => matches!(
            (next, third),
            (
                Some("status" | "replica" | "tasks" | "commons" | "bus" | "sync" | "review"),
                _
            ) | (Some("worker"), Some("pause" | "stop" | "status" | "list"))
                | (Some("metrics"), Some("preview"))
        ),
        "uninstall" => next == Some("--dry-run"),
        _ => false,
    };
    if allowed {
        Ok(Verdict::Literal)
    } else {
        let shown: Vec<&str> = args.iter().take(3).map(String::as_str).collect();
        refused(format!("lean-ctx {}", shown.join(" ")))
    }
}

thread_local! {
    /// How many `lean-ctx -c` layers the current check is inside.
    static WRAP_DEPTH: Cell<u8> = const { Cell::new(0) };
}

/// Deeper `lean-ctx -c 'lean-ctx -c …'` nesting is refused, not followed.
const MAX_WRAP_DEPTH: u8 = 4;

fn blocked(reason: &str) -> ShellError {
    format!("[BLOCKED — DO NOT RETRY] {reason}").into()
}

/// Checks the inner command of `lean-ctx -c`/`-t` like any other line. A
/// non-literal inner command cannot be known here; the gate inside that
/// lean-ctx checks what it receives, under the same policy.
fn check_wrapped(inner: &[String], raw: &[&str]) -> Result<(), ShellError> {
    if inner.is_empty() || raw.len() < inner.len() || !raw.iter().all(|w| is_literal(w)) {
        return Ok(());
    }
    let line = if inner.len() == 1 {
        inner[0].clone()
    } else {
        inner.join(" ")
    };
    let depth = WRAP_DEPTH.with(Cell::get);
    if depth >= MAX_WRAP_DEPTH {
        return Err(blocked(
            "`lean-ctx -c` is nested too deeply to validate. Run the inner command directly.",
        ));
    }
    WRAP_DEPTH.with(|d| d.set(depth + 1));
    let verdict = super::enforcement::enforce_shell_allowlist(&line);
    WRAP_DEPTH.with(|d| d.set(depth));
    verdict
}

/// Rejects a lean-ctx invocation (`tokens[0]` already basename-normalized)
/// that an agent shell may not run. `segment` is the command as written: the
/// words that select what runs must be literal, since the shell expands them
/// after this check (`k=yolo; lean-ctx $k --yes`, `lean-ctx {yolo,} --yes`).
pub(super) fn check_self_reconfiguration(
    segment: &str,
    tokens: &[String],
) -> Result<(), ShellError> {
    let Some(base) = tokens.first() else {
        return Ok(());
    };
    if ARGUMENT_FEEDERS.contains(&base.as_str()) && tokens[1..].iter().any(|t| is_leanctx_word(t)) {
        return Err(blocked(&format!(
            "lean-ctx cannot run as a `{base}` target from an agent shell: its subcommand \
             and arguments would come from input the check cannot see. Call `lean-ctx \
             <subcommand> …` directly with literal arguments."
        )));
    }
    if base != "lean-ctx" && base != "lean-ctx.exe" {
        return Ok(());
    }
    let args = &tokens[1..];
    if args.is_empty() {
        return Err(blocked(
            "A bare `lean-ctx` from an agent shell starts the MCP server on the shell's \
             stdin, which runs tools this check cannot see. Call a subcommand directly.",
        ));
    }
    let raw = raw_leanctx_args(segment).unwrap_or_default();
    let sub_literal = raw.first().is_none_or(|w| is_literal(w));
    if !sub_literal || args[0].starts_with("__") {
        return Err(blocked(
            "A lean-ctx subcommand must be written literally from an agent shell (no `$`, \
             backtick, brace, glob or `~`), and its internal `__` modes cannot be started: \
             the shell would decide what runs after this check.",
        ));
    }
    match classify(args) {
        Ok(Verdict::View) => Ok(()),
        Ok(Verdict::Literal) => {
            if raw.iter().all(|w| is_literal(w)) {
                Ok(())
            } else {
                Err(blocked(&format!(
                    "`lean-ctx {}` takes only literal arguments from an agent shell (no `$`, \
                     backtick, brace, glob or `~`): the shell would expand them into other \
                     words after this check. Write them out.",
                    args[0]
                )))
            }
        }
        Ok(Verdict::Wrapper(skip)) => {
            let inner = args.get(skip..).unwrap_or_default();
            check_wrapped(inner, raw.get(skip..).unwrap_or_default())
        }
        Err(cmd) => Err(blocked(&format!(
            "`{cmd}` is not one of the lean-ctx commands an agent shell may run (reads, \
             views, searches, `-c`, and settings that only tighten or change presentation); \
             it can change lean-ctx's own settings, policy or state. Ask the user to run it \
             in their own terminal if they want it."
        ))),
    }
}

/// Commands that only read; anything else touching lean-ctx's own state is
/// refused from an agent shell.
const READ_ONLY_COMMANDS: &[&str] = &[
    "cat",
    "less",
    "more",
    "head",
    "tail",
    "ls",
    "stat",
    "file",
    "wc",
    "grep",
    "rg",
    "egrep",
    "fgrep",
    "du",
    "md5sum",
    "sha256sum",
    "shasum",
    "jq",
    "diff",
    "cmp",
    "test",
    "[",
    "[[",
    "df",
    "eza",
    "exa",
    "lsd",
    "xdg-open",
    "realpath",
    "readlink",
    "echo",
    "printf",
];

/// lean-ctx subcommands that only read the paths they are given; every
/// other lean-ctx call that names lean-ctx's state may write it
/// (`graph export-html --out …`, `knowledge export --output …`).
const LEANCTX_READ_SUBCOMMANDS: &[&str] = &[
    "read", "grep", "glob", "find", "ls", "diff", "deps", "search",
];

/// Wrappers that only run the command after their own words, judged by that
/// command (`timeout 120 find ~ -name x` reads like `find ~ -name x`). Not
/// here: wrappers that change who runs it (`sudo`, `doas`), take the
/// command from input (`xargs`) or write a file of their own (`flock`,
/// `strace -o`).
const PEELED_WRAPPERS: &[&str] = &[
    "timeout", "nice", "nohup", "env", "command", "builtin", "stdbuf", "setsid",
];

/// Reserved words that introduce the command after them.
const COMMAND_KEYWORDS: &[&str] = &["do", "then", "else", "elif", "if", "while", "until", "!"];

/// Most keywords and wrappers peeled off one command; deeper nesting is
/// judged as written.
const MAX_PEEL: usize = 8;

/// Programs that carry a script in an argument: a state path anywhere in
/// such a word (`sed -n '1w ~/.config/lean-ctx/x'`) names that state. For
/// every other program only a whole word counts, so a commit message that
/// mentions the config path names nothing.
const SCRIPT_PROGRAMS: &[&str] = &[
    "sed",
    "gsed",
    "awk",
    "gawk",
    "mawk",
    "nawk",
    "perl",
    "python",
    "python3",
    "node",
    "ruby",
    "php",
    "lua",
    "deno",
    "bun",
    "pwsh",
    "powershell",
];

/// Programs known to write the paths they name. Only for these does a
/// directory ABOVE lean-ctx's state count (`cp -r x ~/.config/`,
/// `rm -rf ~/.config`, `tar xf e.tar -C ~`); any other program is refused
/// only for a path inside the state.
const WRITE_PROGRAMS: &[&str] = &[
    "cp", "mv", "rsync", "install", "ditto", "scp", "tar", "bsdtar", "gtar", "cpio", "pax", "7z",
    "7za", "unzip", "ln", "rm", "rmdir", "tee", "truncate", "touch", "chmod", "chown", "shred",
    "unlink", "mkdir", "dd", "curl", "wget",
];

/// `git` subcommands that only read the repository.
const GIT_READ_SUBCOMMANDS: &[&str] = &[
    "status",
    "log",
    "diff",
    "show",
    "rev-parse",
    "ls-files",
    "blame",
    "grep",
];

/// The shell's working directory as the line has moved it so far.
#[derive(Clone, Debug)]
enum LineCwd {
    /// This process's working directory (no `cd` yet).
    Process,
    /// Moved by literal `cd`s: every directory the shell may be in. A `cd`
    /// may fail (`cd ~; cd /missing; rm …` stays in `~`), so a directory
    /// change adds its target and keeps the earlier ones (GHSA-cfmv).
    At(Vec<std::path::PathBuf>),
    /// Moved somewhere this check cannot tell (`cd "$d"`, `cd -`, `popd`);
    /// holds that directory change as written, for the refusal.
    Unknown(String),
}

/// Most directories [`LineCwd::At`] follows before it gives up (unknown).
const MAX_LINE_CWDS: usize = 64;

thread_local! {
    static LINE_CWD: std::cell::RefCell<LineCwd> = const { std::cell::RefCell::new(LineCwd::Process) };
    /// Variables the line binds, with the words each may stand for: loop
    /// variables (`for d in ~ /tmp`), assignments (`d=~/x`, `x=$d`), the
    /// arguments a local function is called with (`$1`, `$@`), and every
    /// variable set in a way this check cannot follow (`read`, `mapfile`,
    /// `printf -v`, arrays), which stands for lean-ctx's own state.
    static LINE_VARS: std::cell::RefCell<Vec<(String, Vec<String>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Why no `cd` of the line can be followed, when one cannot: a `cd` run
    /// again and again (in a loop, a function, a trap), or a line that sets
    /// `CDPATH`, `HOME` or `OLDPWD`.
    static LINE_CD_HAZARD: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// Restores the enclosing line's working directory and loop variables when
/// dropped.
pub(super) struct LineScope(Option<(LineCwd, Vec<(String, Vec<String>)>)>);

/// Scopes the `cd` tracking and the loop variables to one command line: a
/// nested line (`cd x && lean-ctx -c '…'`) starts where the outer one is,
/// and whatever it moves to or binds is undone when it ends.
pub(super) fn enter_line() -> LineScope {
    LineScope(Some((
        LINE_CWD.with(|c| c.borrow().clone()),
        LINE_VARS.with(|v| v.borrow().clone()),
    )))
}

impl Drop for LineScope {
    fn drop(&mut self) {
        if let Some((cwd, vars)) = self.0.take() {
            LINE_CWD.with(|c| *c.borrow_mut() = cwd);
            LINE_VARS.with(|v| *v.borrow_mut() = vars);
        }
    }
}

/// Most words one loop variable is followed through; a line binding more
/// is judged with every variable it cannot follow refused (fail closed).
const MAX_BOUND_VALUES: usize = 256;

/// Most command variants one segment is judged in (one per combination of
/// the values of the loop variables it names).
const MAX_VARIANTS: usize = 1024;

/// A shell variable name.
fn is_identifier(word: &str) -> bool {
    word.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && word.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `word` with every expansion of variable `name` (`$name`, `${name}`, and
/// `${name…}` forms such as `${name%/}`, read as the whole value) replaced
/// by `value`.
fn substitute(word: &str, name: &str, value: &str) -> String {
    let mut out = String::with_capacity(word.len());
    let mut rest = word;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        if let Some(braced) = after.strip_prefix('{')
            && let Some(tail) = braced.strip_prefix(name)
            && !tail.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
            && let Some(close) = tail.find('}')
        {
            out.push_str(value);
            rest = &tail[close + 1..];
            continue;
        }
        if let Some(tail) = after.strip_prefix(name)
            && !tail.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
        {
            out.push_str(value);
            rest = tail;
            continue;
        }
        out.push('$');
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Whether `word` expands variable `name`.
fn expands(word: &str, name: &str) -> bool {
    substitute(word, name, "\u{0}") != word
}

/// Binds the loop variables of `command` for the state check of its
/// segments: `for d in ~ /tmp; do rm -rf "$d/.config/lean-ctx"; done` writes
/// lean-ctx's state through `$d`, although no single segment names it. A
/// `for`/`select` header only lists its words; the commands of the body are
/// judged once for every word the variable stands for. A variable assigned
/// from a bound one (`x=$d`) is followed the same way. The scan is
/// flow-insensitive and reads the whole line, so it holds for both the
/// segment and the leaf walk (`( for … )`, `{ for …; }`). The bindings hold
/// until the returned guard drops.
#[must_use]
pub(super) fn bind_loop_variables(command: &str) -> LoopVars {
    let guard = LoopVars(Some((
        LINE_VARS.with(|v| v.borrow().clone()),
        LINE_CD_HAZARD.with(|h| h.borrow().clone()),
    )));
    bind_into_line(command);
    guard
}

/// The bound variables and the pending directory-change hazard, as saved.
type SavedLoopState = (Vec<(String, Vec<String>)>, Option<String>);

/// Restores the enclosing check's loop variables when dropped.
pub(super) struct LoopVars(Option<SavedLoopState>);

impl Drop for LoopVars {
    fn drop(&mut self) {
        if let Some((vars, hazard)) = self.0.take() {
            LINE_VARS.with(|v| *v.borrow_mut() = vars);
            LINE_CD_HAZARD.with(|h| *h.borrow_mut() = hazard);
        }
    }
}

/// What a variable set in a way this check cannot follow stands for: every
/// directory of lean-ctx's own state, so a command that would change the
/// state through it is refused (fail closed).
fn opaque_values() -> Vec<String> {
    protected_dirs()
        .iter()
        .map(|d| format!("{OPAQUE_MARK}{}", d.display()))
        .collect()
}

/// Marks a word that stands for a value this check could not follow, so
/// only a command that writes through it is refused: a read-only or
/// unknown-effect program (`gh issue view "$line"`) given a run-time value
/// is not assumed to name lean-ctx's state. Never in a real command.
const OPAQUE_MARK: char = '\u{1}';

/// A word that separates commands, as the tokenizer leaves it.
fn ends_command(word: &str) -> bool {
    matches!(
        word,
        ";" | "&&" | "||" | "|" | "&" | "}" | ")" | "do" | "done" | "then"
    ) || word.ends_with(';')
        || word.contains('\n')
}

/// The function names `tokens` defines (`f() { …; }`, `function f { …; }`).
fn defined_functions(tokens: &[String]) -> Vec<String> {
    let mut names = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        let name = if let Some(name) = token.strip_suffix("()") {
            name.trim_start_matches(['(', '{', ';'])
        } else if token == "function" {
            tokens.get(i + 1).map_or("", |n| n.trim_end_matches("()"))
        } else if tokens.get(i + 1).is_some_and(|n| n == "()") {
            token.as_str()
        } else {
            continue;
        };
        if is_identifier(name) && !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
    }
    names
}

/// Variables the words after `tokens[i]` (a `read`, `mapfile`, `readarray`,
/// `getopts` or `printf -v`) fill from input this check never sees.
fn opaque_targets(tokens: &[String], i: usize) -> Vec<String> {
    let program = tokens[i].trim_start_matches(['(', '{']);
    let rest = tokens[i + 1..]
        .iter()
        .take_while(|w| !ends_command(w) && !w.starts_with(['<', '>']))
        .chain(tokens[i + 1..].iter().find(|w| w.ends_with(';')))
        .map(|w| w.trim_end_matches(';').to_string());
    let mut names: Vec<String> = match program {
        "printf" => {
            let words: Vec<String> = rest.collect();
            words
                .iter()
                .enumerate()
                .find_map(|(k, w)| {
                    if w == "-v" {
                        words.get(k + 1).cloned()
                    } else {
                        w.strip_prefix("-v").map(str::to_string)
                    }
                })
                .into_iter()
                .collect()
        }
        "mapfile" | "readarray" => {
            let mut names: Vec<String> = rest.filter(|w| is_identifier(w)).collect();
            names.push("MAPFILE".to_string());
            names
        }
        _ => {
            let mut names: Vec<String> = rest.filter(|w| is_identifier(w)).collect();
            names.push("REPLY".to_string());
            names
        }
    };
    names.retain(|n| is_identifier(n));
    names
}

/// Records [`LINE_CD_HAZARD`] for `tokens`: a `cd`, `pushd` or `popd` in a
/// loop or function body or a trap (it runs an unknown number of times), or
/// a line that sets `CDPATH`, `HOME` or `OLDPWD` (which move `cd`'s target).
fn record_cd_hazard(tokens: &[String]) {
    let is_cd = |w: &str| {
        let w = w.trim_start_matches(['(', '{', ';']);
        matches!(w, "cd" | "pushd" | "popd")
            || ["cd ", "pushd ", "popd "].iter().any(|p| w.starts_with(p))
            || w.contains(";cd ")
            || w.contains("; cd ")
    };
    let mut hazard = None;
    let mut stack: Vec<u8> = Vec::new();
    let mut function_pending = false;
    for (i, token) in tokens.iter().enumerate() {
        let word = token.trim_start_matches(['(', ';']).trim_end_matches(';');
        if let Some((name, _)) = token.split_once('=')
            && matches!(
                name.trim_start_matches(['(', '{', ';']),
                "CDPATH" | "HOME" | "OLDPWD"
            )
        {
            hazard = Some(format!("the `{token}` the line sets"));
            break;
        }
        if word == "trap" && tokens[i + 1..].iter().take(1).any(|a| is_cd(a)) {
            hazard = Some("the `cd` in a trap".to_string());
            break;
        }
        if token.ends_with("()") || token == "()" || word == "function" {
            function_pending = true;
        }
        if word == "do" {
            stack.push(b'd');
        } else if word == "done" || word == "}" {
            stack.pop();
        } else if word.starts_with('{') {
            stack.push(if function_pending { b'f' } else { b'b' });
            function_pending = false;
        }
        if stack.iter().any(|&k| k != b'b') && is_cd(token) {
            hazard = Some("the `cd` in a loop or function body".to_string());
            break;
        }
    }
    if hazard.is_none()
        && tokens
            .iter()
            .any(|t| t.contains("(){") && is_cd(t.split_once("(){").map_or("", |(_, b)| b)))
    {
        // A function body written as one word (`f(){ cd ..;}`).
        hazard = Some("the `cd` in a function body".to_string());
    }
    if hazard.is_some() {
        LINE_CD_HAZARD.with(|h| *h.borrow_mut() = hazard);
    }
}

/// The values a function's positional parameters may stand for: every word
/// any call of a local function passes it (`f() { rm -rf "$1"; }; f ~/x`).
fn positional_values(tokens: &[String], functions: &[String]) -> Vec<String> {
    let mut values = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        let word = token.trim_start_matches(['(', '{', ';']);
        let call = word.trim_end_matches(';');
        if !functions.iter().any(|f| f == call) || word.ends_with("()") {
            continue;
        }
        if tokens.get(i + 1).is_some_and(|n| n == "()") || word.ends_with(';') {
            continue;
        }
        for arg in &tokens[i + 1..] {
            if matches!(arg.as_str(), ";" | "&&" | "||" | "|" | "&" | "}" | ")") {
                break;
            }
            let last = arg.ends_with(';');
            let arg = arg.trim_end_matches(';');
            if !arg.is_empty() {
                values.push(arg.to_string());
            }
            if last {
                break;
            }
        }
    }
    values
}

/// Brace expansion of one word (`lean-{ctx,x}` → `lean-ctx`, `lean-x`).
/// A list this check does not expand (a sequence, too many words) is
/// replaced from its `{` by `*`, which the path checks read as a glob.
fn expand_braces(word: &str) -> Vec<String> {
    const MAX_WORDS: usize = 64;
    let bytes = word.as_bytes();
    let mut open = None;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 1,
            b'{' if i == 0 || bytes[i - 1] != b'$' => {
                open = Some(i);
                break;
            }
            _ => {}
        }
        i += 1;
    }
    let Some(open) = open else {
        return vec![word.to_string()];
    };
    let mut depth = 0usize;
    let mut close = None;
    let mut commas = Vec::new();
    for (k, &c) in bytes.iter().enumerate().skip(open) {
        match c {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(k);
                    break;
                }
            }
            b',' if depth == 1 => commas.push(k),
            _ => {}
        }
    }
    let Some(close) = close else {
        return vec![word.to_string()];
    };
    let (head, tail) = (&word[..open], &word[close + 1..]);
    let inner = &word[open + 1..close];
    if commas.is_empty() {
        if inner.contains("..") {
            return vec![format!("{head}*")];
        }
        // `{x}` is no list: keep it and expand what follows.
        return expand_braces(tail)
            .into_iter()
            .map(|t| format!("{head}{{{inner}}}{t}"))
            .collect();
    }
    let mut parts = Vec::new();
    let mut start = open + 1;
    for &c in commas.iter().chain(std::iter::once(&close)) {
        parts.push(&word[start..c]);
        start = c + 1;
    }
    let mut out = Vec::new();
    for part in parts {
        for expanded in expand_braces(&format!("{head}{part}{tail}")) {
            if out.len() >= MAX_WORDS {
                return vec![format!("{head}*")];
            }
            out.push(expanded);
        }
    }
    out
}

fn bind_into_line(command: &str) {
    let tokens = crate::shell_tokenize(command);
    record_cd_hazard(&tokens);
    let mut bound: Vec<(String, Vec<String>)> = Vec::new();
    let functions = defined_functions(&tokens);
    let positional = positional_values(&tokens, &functions);
    if !positional.is_empty() {
        for name in ["1", "2", "3", "4", "5", "6", "7", "8", "9", "@", "*"] {
            bound.push((name.to_string(), positional.clone()));
        }
    }
    let opaque = opaque_values();
    let bind_opaque = |bound: &mut Vec<(String, Vec<String>)>, name: &str| {
        if let Some((_, values)) = bound.iter_mut().find(|(n, _)| n == name) {
            for v in &opaque {
                if !values.contains(v) {
                    values.push(v.clone());
                }
            }
        } else {
            bound.push((name.to_string(), opaque.clone()));
        }
    };
    for (i, token) in tokens.iter().enumerate() {
        let program = token.trim_start_matches(['(', '{', ';']);
        if matches!(
            program,
            "read" | "mapfile" | "readarray" | "getopts" | "printf"
        ) {
            for name in opaque_targets(&tokens, i) {
                bind_opaque(&mut bound, &name);
            }
        }
        // `set -- a b` / `set a b`: new positional parameters.
        if program == "set"
            && tokens
                .get(i + 1)
                .is_some_and(|w| w == "--" || !w.starts_with(['-', '+']))
        {
            for name in ["1", "2", "3", "4", "5", "6", "7", "8", "9", "@", "*"] {
                bind_opaque(&mut bound, name);
            }
        }
        // An array or a value built at run time (`a=(x ~/s)`, `d=$(…)`,
        // `d+=x`): this check does not follow it.
        if let Some((name, value)) = token.split_once('=') {
            let name = name.trim_start_matches(['(', '{', ';']);
            let appended = name.strip_suffix('+');
            let name = appended.unwrap_or(name);
            if is_identifier(name)
                && (appended.is_some()
                    || value.starts_with('(')
                    || value.contains("$(")
                    || value.contains('`')
                    || value.contains("${!"))
            {
                bind_opaque(&mut bound, name);
            }
        }
        // `${d:=value}` / `${d=value}` assigns `d` where it expands.
        let mut rest = token.as_str();
        while let Some(at) = rest.find("${") {
            let body = &rest[at + 2..];
            let name_len = body
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(body.len());
            let (name, op) = body.split_at(name_len);
            if is_identifier(name) && (op.starts_with(":=") || op.starts_with('=')) {
                bind_opaque(&mut bound, name);
            }
            rest = body;
        }
    }
    for (i, token) in tokens.iter().enumerate() {
        if !matches!(token.trim_start_matches(['(', '{']), "for" | "select") {
            continue;
        }
        let Some(name) = tokens.get(i + 1).map(|n| n.trim_end_matches(';')) else {
            continue;
        };
        if !is_identifier(name) {
            continue;
        }
        if tokens.get(i + 2).map(String::as_str) != Some("in") {
            // `for x; do` / `for x do`: the positional parameters.
            if tokens[i + 1].ends_with(';') || tokens.get(i + 2).is_some_and(|w| w == "do") {
                bound.push((name.to_string(), positional.clone()));
            }
            continue;
        }
        let mut words = Vec::new();
        for word in &tokens[i + 3..] {
            if word == "do" || word.starts_with(';') {
                break;
            }
            if let Some(last) = word.strip_suffix(';') {
                if !last.is_empty() {
                    words.push(last.to_string());
                }
                break;
            }
            words.push(word.clone());
        }
        bound.push((name.to_string(), words));
    }
    // GHSA-cfmv-qg29-3rvp: `NAME=value` anywhere on the line (a prefix,
    // `export`, `local`, a group): NAME may stand for that value. A variable
    // the environment gives a meaning to (`HOME`, `XDG_CONFIG_HOME`) may
    // also still stand for it (the scan is flow-insensitive).
    let homes = homes();
    for token in &tokens {
        let Some((name, value)) = token.split_once('=') else {
            continue;
        };
        let name = name.trim_start_matches(['(', '{', ';']);
        let value = value.trim_end_matches(';');
        if !is_identifier(name) || value.starts_with('(') {
            continue;
        }
        if let Some((_, values)) = bound.iter_mut().find(|(n, _)| n == name) {
            if !values.iter().any(|v| v == value) {
                values.push(value.to_string());
            }
        } else {
            let mut values = vec![value.to_string()];
            if homes.iter().any(|h| variable_base(name, h).is_some()) {
                values.push(format!("${{{name}}}"));
            }
            bound.push((name.to_string(), values));
        }
    }
    if bound.is_empty() {
        return;
    }
    // `e=$d/x`: e stands for what `$d/x` does, for every value of `d`.
    for _ in 0..4 {
        let snapshot = bound.clone();
        let mut changed = false;
        for (name, values) in &mut bound {
            let mut next: Vec<String> = Vec::new();
            for value in values.iter() {
                let mut expanded = vec![value.clone()];
                for (var, words) in &snapshot {
                    if var == name || !expanded.iter().any(|e| expands(e, var)) {
                        continue;
                    }
                    expanded = expanded
                        .iter()
                        .flat_map(|e| words.iter().map(move |w| substitute(e, var, w)))
                        .collect();
                    if expanded.len() > MAX_BOUND_VALUES {
                        break;
                    }
                }
                for e in expanded {
                    if !next.contains(&e) {
                        next.push(e);
                    }
                }
            }
            if next.len() > MAX_BOUND_VALUES {
                // Too many to follow: it may stand for anything.
                next.clone_from(&opaque);
            }
            if next != *values {
                *values = next;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    LINE_VARS.with(|v| v.borrow_mut().extend(bound));
}

/// `word` with a leading directory-stack tilde spelled as the variable it
/// stands for: `~+` as `${PWD}`, `~-` as `${OLDPWD}`, and `~N`/`~+N`/`~-N`
/// (a `pushd` stack entry, not tracked) as the opaque value.
fn directory_stack_tilde(word: &str, opaque: &str) -> String {
    let Some(rest) = word.strip_prefix('~') else {
        return word.to_string();
    };
    let sign_len = usize::from(rest.starts_with(['+', '-']));
    let digits = rest[sign_len..]
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len() - sign_len);
    let end = sign_len + digits;
    if end == 0 || !(rest.len() == end || rest[end..].starts_with('/')) {
        return word.to_string();
    }
    let head = match (&rest[..sign_len], digits) {
        ("+", 0) => "${PWD}",
        ("-", 0) => "${OLDPWD}",
        _ => opaque,
    };
    format!("{head}{}", &rest[end..])
}

/// `token` with every command substitution (`$(…)`, `` `…` ``) replaced by
/// lean-ctx's state directory (fail closed): its output is a run-time value
/// this check cannot know. Arithmetic (`$((…))`) is left as it is.
fn opaque_substitutions(token: &str, opaque: &str) -> String {
    if !token.contains("$(") && !token.contains('`') {
        return token.to_string();
    }
    let bytes = token.as_bytes();
    let mut out = String::with_capacity(token.len());
    let (mut i, mut last) = (0, 0);
    while i < bytes.len() {
        let end = if bytes[i] == b'$' && bytes.get(i + 1) == Some(&b'(') {
            let close = matching_paren(bytes, i + 1);
            // `$((…))` is arithmetic only when the inner group closes right
            // before the outer one; `$((cmd) )` is a subshell substitution.
            if bytes.get(i + 2) == Some(&b'(')
                && let (Some(inner), Some(outer)) = (matching_paren(bytes, i + 2), close)
                && inner + 1 == outer
            {
                i = outer + 1;
                continue;
            }
            close.map_or(bytes.len(), |c| c + 1)
        } else if bytes[i] == b'`' {
            token[i + 1..].find('`').map_or(bytes.len(), |k| i + k + 2)
        } else {
            i += 1;
            continue;
        };
        out.push_str(&token[last..i]);
        out.push_str(opaque);
        i = end;
        last = end;
    }
    out.push_str(&token[last..]);
    out
}

/// The index of the `)` closing the `(` at `open`, if any.
fn matching_paren(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (j, b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
            _ => {}
        }
    }
    None
}

/// The length of a `${…}` body starting right after `${`, nested braces
/// included; `None` when it is not closed.
fn expansion_body_len(inner: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in inner.char_indices() {
        match c {
            '{' => depth += 1,
            '}' if depth == 0 => return Some(i),
            '}' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// The words `token` may stand for, with every expansion this check cannot
/// evaluate replaced by lean-ctx's state directory (fail closed): an
/// indirection (`${!r}`), an operator on a bound variable (`${d/x/y}`,
/// `${d%/*}`, `${@:1}`) and a value-changing operator on any other variable
/// (`${HOME%/}`). A default or alternative (`${d:-w}`, `${d=w}`,
/// `${d:+w}`) is judged both ways: as the variable and as the word.
fn opaque_expansions(token: &str, bound: &[(String, Vec<String>)], opaque: &str) -> Vec<String> {
    if !token.contains("${") {
        return vec![token.to_string()];
    }
    let mut outs = vec![String::with_capacity(token.len())];
    let push = |outs: &mut Vec<String>, alternatives: &[String]| {
        *outs = outs
            .iter()
            .flat_map(|o| alternatives.iter().map(move |a| format!("{o}{a}")))
            .collect();
    };
    let mut rest = token;
    while let Some(at) = rest.find("${") {
        push(&mut outs, &[rest[..at].to_string()]);
        let inner = &rest[at + 2..];
        let Some(close) = expansion_body_len(inner) else {
            push(&mut outs, &[rest[at..].to_string()]);
            return outs;
        };
        let body = &inner[..close];
        let name_len = body
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '@' || c == '*'))
            .unwrap_or(body.len());
        let (name, op) = body.split_at(name_len);
        let plain = || format!("${{{name}}}");
        let alternatives: Vec<String> = if body.starts_with('!') {
            vec![opaque.to_string()]
        } else if op.is_empty() || op.starts_with('[') {
            vec![rest[at..=at + 2 + close].to_string()]
        } else if bound.iter().any(|(n, _)| n == name) {
            vec![opaque.to_string()]
        } else if let Some(word) = [":-", ":=", "-", "="]
            .iter()
            .find_map(|p| op.strip_prefix(p))
        {
            let mut words = vec![plain()];
            words.extend(opaque_expansions(word, bound, opaque));
            words
        } else if let Some(word) = [":+", "+"].iter().find_map(|p| op.strip_prefix(p)) {
            let mut words = vec![String::new()];
            words.extend(opaque_expansions(word, bound, opaque));
            words
        } else {
            vec![opaque.to_string()]
        };
        push(&mut outs, &alternatives);
        if outs.len() > MAX_VARIANTS {
            return vec![opaque.to_string()];
        }
        rest = &inner[close + 1..];
    }
    push(&mut outs, &[rest.to_string()]);
    outs
}

/// `tokens` once for every combination of the values of the bound loop
/// variables they expand (just `tokens` when they expand none); `None`
/// when there are too many to judge.
fn bound_variants(tokens: &[String]) -> Option<Vec<(Vec<String>, bool)>> {
    let variants = expanded_variants(tokens)?;
    let marked = opaque_values();
    let below: Vec<String> = marked.iter().map(|v| format!("{v}/")).collect();
    Some(
        variants
            .into_iter()
            .map(|variant| {
                // A run-time value inside a word (`~/.config/"$d"`,
                // `lean"$d"`) may complete it to any name there: read as a
                // glob (`~/.config/*`). After an `=` it is the whole value.
                let mut embedded = false;
                let variant: Vec<String> = variant
                    .into_iter()
                    .map(|t| {
                        let mut t = t;
                        for v in &marked {
                            while let Some(at) = t.find(v.as_str()) {
                                if at == 0 || t[..at].ends_with('=') {
                                    break;
                                }
                                t.replace_range(at..at + v.len(), "*");
                                embedded = true;
                            }
                        }
                        t
                    })
                    .collect();
                // A run-time value used as a directory (`"$d/x.db"`) names a
                // path under it, whatever the program: judged like a literal.
                let opaque = !embedded
                    && variant.iter().any(|t| t.contains(OPAQUE_MARK))
                    && !variant.iter().any(|t| below.iter().any(|b| t.contains(b)));
                let words = variant
                    .into_iter()
                    .map(|t| t.replace(OPAQUE_MARK, ""))
                    .collect();
                (words, opaque)
            })
            .collect(),
    )
}

/// [`bound_variants`] before the opaque marks are taken out.
fn expanded_variants(tokens: &[String]) -> Option<Vec<Vec<String>>> {
    let mut vars = LINE_VARS.with(|v| v.borrow().clone());
    let opaque = opaque_values().into_iter().next().unwrap_or_default();
    // Variables the shell itself keeps: `$PWD` is where this segment runs,
    // `$OLDPWD` and `$_` hold values this check does not track.
    let pwd = line_cwds().map_or_else(
        || vec![opaque.clone()],
        |dirs| dirs.iter().map(|d| d.display().to_string()).collect(),
    );
    for (name, values) in [
        ("PWD", pwd),
        ("OLDPWD", vec![opaque.clone()]),
        ("_", vec![opaque.clone()]),
    ] {
        match vars.iter_mut().find(|(n, _)| n == name) {
            Some((_, bound)) => bound.extend(values),
            None => vars.push((name.to_string(), values)),
        }
    }
    let mut variants: Vec<Vec<String>> = vec![Vec::new()];
    for token in tokens {
        let token = opaque_substitutions(&directory_stack_tilde(token, &opaque), &opaque);
        let alternatives: Vec<Vec<String>> = opaque_expansions(&token, &vars, &opaque)
            .iter()
            .map(|t| expand_braces(t))
            .collect();
        if variants.len() * alternatives.len() > MAX_VARIANTS {
            return None;
        }
        variants = variants
            .iter()
            .flat_map(|variant| {
                alternatives.iter().map(move |words| {
                    let mut next = variant.clone();
                    next.extend(words.iter().cloned());
                    next
                })
            })
            .collect();
    }
    for (name, values) in &vars {
        if !variants.iter().any(|v| v.iter().any(|t| expands(t, name))) {
            continue;
        }
        if values.len() > MAX_BOUND_VALUES || variants.len() * values.len().max(1) > MAX_VARIANTS {
            return None;
        }
        variants = variants
            .iter()
            .flat_map(|variant| {
                values.iter().map(move |value| {
                    variant
                        .iter()
                        .map(|t| substitute(t, name, value))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        if variants.is_empty() {
            // `for d in; do …` runs no body.
            return Some(variants);
        }
    }
    Some(variants)
}

thread_local! {
    /// The directory the checked command will run in, when the caller
    /// knows it (ctx_shell's `cwd`, a hook payload's `cwd`).
    static RUN_CWD: std::cell::RefCell<Option<std::path::PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Runs `f` with relative paths resolved against `cwd`, the directory the
/// checked command runs in, instead of this process's own.
pub fn with_run_cwd<T>(cwd: Option<std::path::PathBuf>, f: impl FnOnce() -> T) -> T {
    let previous = RUN_CWD.with(|c| c.replace(cwd));
    let out = f();
    RUN_CWD.with(|c| *c.borrow_mut() = previous);
    out
}

/// The directory a line starts in.
fn start_cwd() -> Option<std::path::PathBuf> {
    RUN_CWD
        .with(|c| c.borrow().clone())
        .or_else(|| std::env::current_dir().ok())
}

/// Every directory relative words may resolve against, `None` when unknown.
fn line_cwds() -> Option<Vec<std::path::PathBuf>> {
    if LINE_CD_HAZARD.with(|h| h.borrow().is_some()) {
        return None;
    }
    match LINE_CWD.with(|c| c.borrow().clone()) {
        LineCwd::Process => start_cwd().map(|d| vec![d]),
        LineCwd::At(dirs) => Some(dirs),
        LineCwd::Unknown(_) => None,
    }
}

/// Why relative words cannot be placed, when [`line_cwds`] is `None`: the
/// directory change this check could not follow, as written.
fn unknown_cwd_cause() -> String {
    if let Some(hazard) = LINE_CD_HAZARD.with(|h| h.borrow().clone()) {
        return hazard;
    }
    match LINE_CWD.with(|c| c.borrow().clone()) {
        LineCwd::Unknown(change) => format!("the earlier `{change}`"),
        LineCwd::Process | LineCwd::At(_) => "the unreadable working directory".to_string(),
    }
}

/// Follows a `cd`/`pushd`/`popd` segment. The directory it names joins the
/// ones the shell may already be in (the `cd` may fail); one this check
/// cannot tell makes the working directory unknown for the rest of the line.
fn follow_directory_change(tokens: &[String], homes: &[std::path::PathBuf]) {
    let target = tokens[1..]
        .iter()
        .find(|t| !t.starts_with('-') || t.as_str() == "-");
    let unknown = || LineCwd::Unknown(tokens.join(" "));
    let Some(mut dirs) = line_cwds() else {
        return;
    };
    let hazard = LINE_CD_HAZARD.with(|h| h.borrow().clone());
    let cdpath_set = std::env::var_os("CDPATH").is_some_and(|v| !v.is_empty());
    let targets: Option<Vec<std::path::PathBuf>> = match (tokens[0].as_str(), target) {
        _ if hazard.is_some() => None,
        ("popd", _) => None,
        (_, None) => homes.first().map(|h| vec![h.clone()]),
        (_, Some(word)) if word == "-" || word.contains(['$', '`', '*', '?', '[', '{']) => None,
        // `CDPATH` may move a relative `cd` anywhere.
        (_, Some(word)) if cdpath_set && !word.starts_with(['/', '~', '.']) => None,
        (_, Some(word)) => Some(as_paths(word, homes)).filter(|p| !p.is_empty()),
    };
    let next = match targets {
        Some(targets) => {
            for t in targets {
                if !dirs.contains(&t) {
                    dirs.push(t);
                }
            }
            if dirs.len() > MAX_LINE_CWDS {
                unknown()
            } else {
                LineCwd::At(dirs)
            }
        }
        None => match hazard {
            Some(cause) => LineCwd::Unknown(format!("{} ({cause})", tokens.join(" "))),
            None => unknown(),
        },
    };
    LINE_CWD.with(|c| *c.borrow_mut() = next);
}

/// lean-ctx's own config and data directories: the worker pause, held
/// claims, verification nonces, opt-in caches and keys live there — as this
/// process resolves them and at the real user's home, where the
/// shell-security policy lives (`config::shell_policy`).
fn protected_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = [
        lean_ctx::core::paths::config_dir_read_only(),
        lean_ctx::core::paths::data_dir_read_only(),
    ]
    .into_iter()
    .filter_map(Result::ok)
    .collect();
    if let Some(home) = lean_ctx::core::config::shell_policy::real_user_home() {
        dirs.push(home.join(".config").join("lean-ctx"));
        dirs.push(home.join(".local").join("share").join("lean-ctx"));
        dirs.push(home.join(".lean-ctx"));
    }
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    for dir in dirs {
        for candidate in [resolve_existing(&dir), dir] {
            if !out.contains(&candidate) {
                out.push(candidate);
            }
        }
    }
    out
}

/// The home directories `~` and `$HOME` may stand for: the environment's
/// and the real user's.
fn homes() -> Vec<std::path::PathBuf> {
    let mut homes: Vec<std::path::PathBuf> = Vec::new();
    for home in [
        dirs::home_dir(),
        lean_ctx::core::config::shell_policy::real_user_home(),
    ]
    .into_iter()
    .flatten()
    {
        if !homes.contains(&home) {
            homes.push(home);
        }
    }
    homes
}

/// `path` with its longest existing ancestor resolved through symlinks, so
/// `/tmp/link/config.toml` names the directory the link points to.
fn resolve_existing(path: &std::path::Path) -> std::path::PathBuf {
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    let mut probe = path.to_path_buf();
    loop {
        if let Ok(real) = std::fs::canonicalize(&probe) {
            let mut out = real;
            for part in rest.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (
            probe.file_name().map(std::ffi::OsStr::to_os_string),
            probe.parent(),
        ) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                probe = parent.to_path_buf();
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Variables a path may start with, and what they stand for under `home`.
fn variable_base(name: &str, home: &std::path::Path) -> Option<std::path::PathBuf> {
    let from_env = |var: &str| {
        std::env::var_os(var)
            .filter(|v| !v.is_empty())
            .map(std::path::PathBuf::from)
    };
    Some(match name {
        "HOME" | "USERPROFILE" => home.to_path_buf(),
        "XDG_CONFIG_HOME" => from_env(name).unwrap_or_else(|| home.join(".config")),
        "XDG_DATA_HOME" => from_env(name).unwrap_or_else(|| home.join(".local").join("share")),
        "XDG_STATE_HOME" => from_env(name).unwrap_or_else(|| home.join(".local").join("state")),
        "APPDATA" => from_env(name).unwrap_or_else(|| home.join("AppData").join("Roaming")),
        "LOCALAPPDATA" => from_env(name).unwrap_or_else(|| home.join("AppData").join("Local")),
        _ => return None,
    })
}

/// Splits `$NAME/rest`, `${NAME}/rest`, `$env:NAME\rest` (PowerShell) into
/// the variable name and the rest after its separator.
fn split_variable(raw: &str) -> Option<(&str, &str)> {
    let body = raw
        .strip_prefix("$env:")
        .or_else(|| raw.strip_prefix("$ENV:"))
        .or_else(|| raw.strip_prefix('$'))?;
    let (name, rest) = if let Some(braced) = body.strip_prefix('{') {
        let close = braced.find('}')?;
        (&braced[..close], &braced[close + 1..])
    } else {
        let end = body
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(body.len());
        (&body[..end], &body[end..])
    };
    if !(rest.is_empty() || rest.starts_with(['/', '\\'])) {
        return None;
    }
    Some((name, rest.trim_start_matches(['/', '\\'])))
}

/// `token` as absolute, lexically normalised paths, one per home `~` may
/// stand for (`~`, `~user`, `$HOME`, `${XDG_CONFIG_HOME}`, `$env:APPDATA`
/// and drive paths expanded; `key=value` forms such as `dd of=…` read by
/// value), each also with its existing ancestors resolved through symlinks.
fn as_paths(token: &str, homes: &[std::path::PathBuf]) -> Vec<std::path::PathBuf> {
    // `key=value` (`dd of=…`, `--out=…`) is read by value; `=~` is no key.
    let raw = match token.rsplit_once('=') {
        Some((key, value))
            if !key.is_empty()
                && key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':')) =>
        {
            value
        }
        _ => token,
    };
    let raw = raw.trim_matches(['"', '\'']);
    let mut out = Vec::new();
    let mut push = |expanded: std::path::PathBuf| {
        let mut normal = std::path::PathBuf::new();
        for part in expanded.components() {
            match part {
                std::path::Component::ParentDir => {
                    normal.pop();
                }
                std::path::Component::CurDir => {}
                other => normal.push(other),
            }
        }
        for candidate in [resolve_existing(&normal), normal] {
            if !out.contains(&candidate) {
                out.push(candidate);
            }
        }
    };
    let relative = |rest: &str| rest.replace('\\', "/");
    if raw.starts_with('/') || std::path::Path::new(raw).is_absolute() {
        push(std::path::PathBuf::from(raw));
        return out;
    }
    if !raw.is_empty() && !raw.starts_with(['~', '$', '-']) {
        for cwd in line_cwds().unwrap_or_default() {
            push(cwd.join(relative(raw)));
        }
        return out;
    }
    for home in homes {
        if raw == "~" {
            push(home.clone());
        } else if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
            push(home.join(relative(rest)));
        } else if let Some(user_path) = raw.strip_prefix('~') {
            // `~user/…`: that user's home next to this one.
            let (user, rest) = user_path.split_once(['/', '\\']).unwrap_or((user_path, ""));
            if let Some(parent) = home.parent() {
                push(parent.join(user).join(relative(rest)));
            }
        } else if let Some((name, rest)) = split_variable(raw)
            && let Some(base) = variable_base(name, home)
        {
            push(base.join(relative(rest)));
        }
    }
    out
}

/// Path text compared as the filesystem does: case-insensitive on macOS and
/// Windows, `/` as the separator.
fn path_key(path: &std::path::Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    if cfg!(any(target_os = "macos", windows)) {
        text.to_lowercase()
    } else {
        text
    }
}

/// Whether `path` is `dir` or inside it.
fn within(path: &std::path::Path, dir: &std::path::Path) -> bool {
    let (path, dir) = (path_key(path), path_key(dir));
    let dir = dir.trim_end_matches('/');
    path == dir || path.starts_with(&format!("{dir}/"))
}

/// Whether `path` is a strict ancestor of `dir`: writing below it can
/// replace `dir` (`cp -r x ~/.config/`).
fn ancestor_of(path: &std::path::Path, dir: &std::path::Path) -> bool {
    let (path, dir) = (path_key(path), path_key(dir));
    let path = path.trim_end_matches('/');
    // The filesystem root is everyone's ancestor; `tr / _` names no state.
    !path.is_empty() && !path.ends_with(':') && dir.starts_with(&format!("{path}/"))
}

/// The forms a protected `dir` may take inside a longer word (a sed or awk
/// script): its absolute path and its `~`/`$HOME` spellings.
fn spellings(dir: &std::path::Path, homes: &[std::path::PathBuf]) -> Vec<String> {
    let mut out = vec![path_key(dir)];
    for home in homes {
        if let Ok(rel) = dir.strip_prefix(home) {
            let rel = path_key(rel);
            for prefix in ["~/", "$HOME/", "${HOME}/"] {
                out.push(path_key(std::path::Path::new(&format!("{prefix}{rel}"))));
            }
        }
    }
    out
}

/// A command that would touch lean-ctx's own state, and how sure that is.
#[derive(Debug)]
struct Tampering {
    /// `program path`, for the refusal.
    what: String,
    /// `true`: the command writes there (`rm`, `sed -i`, a `>` redirect).
    /// `false`: a program whose effect cannot be told from its arguments
    /// (an interpreter, an unknown tool) — it may write.
    writes: bool,
    /// Set when the command writes a relative path that cannot be placed
    /// because of a directory change this check cannot follow: names that
    /// cause (``the earlier `cd $D` ``) so the refusal points at it.
    unplaced: Option<String>,
}

impl Tampering {
    fn new(what: String, writes: bool) -> Self {
        Self {
            what,
            writes,
            unplaced: None,
        }
    }
}

/// The target of a `>`, `>>`, `>|`, `&>`, `N>`, `<>` or `>&FILE` redirect at
/// `tokens[i]` (attached or the next word); `None` for any other word and
/// for a descriptor duplication (`>&2`, `>&-`).
fn redirect_target(tokens: &[String], i: usize) -> Option<&str> {
    let token = tokens[i].as_str();
    let op = token.trim_start_matches(|c: char| c.is_ascii_digit() || c == '&');
    let rest = op
        .strip_prefix(">>")
        .or_else(|| op.strip_prefix(">|"))
        .or_else(|| op.strip_prefix("<>"))
        .or_else(|| op.strip_prefix('>'))?;
    let (rest, duplicates) = match rest.strip_prefix('&') {
        Some(after) => (after, true),
        None => (rest, false),
    };
    let target = if rest.is_empty() {
        tokens.get(i + 1).map(String::as_str)?
    } else {
        rest
    };
    if duplicates && (target == "-" || target.chars().all(|c| c.is_ascii_digit())) {
        return None;
    }
    Some(target)
}

/// What `sed` with these arguments does to its files: `None` reads,
/// `Some(true)` writes (`-i`/`--in-place` in any spelling, a `w`/`W`
/// command, an `e` command or `s///w`/`s///e` flag), `Some(false)` cannot be
/// told (a script file, an unparsable script).
fn sed_effect(args: &[String]) -> Option<bool> {
    let mut scripts: Vec<&str> = Vec::new();
    let mut script_given = false;
    let mut options_done = false;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if !options_done && arg == "--" {
            options_done = true;
        } else if !options_done && arg.starts_with("--") {
            let (name, value) = arg
                .split_once('=')
                .map_or((arg, None), |(n, v)| (n, Some(v)));
            // GNU accepts any unambiguous prefix (`--in`, `--in-pl`).
            let long = |full: &str| name.len() >= 4 && full.starts_with(name);
            match name {
                _ if long("--in-place") => return Some(true),
                _ if long("--file") => return Some(false),
                _ if long("--expression") => {
                    script_given = true;
                    if let Some(v) = value {
                        scripts.push(v);
                    } else {
                        i += 1;
                        scripts.extend(args.get(i).map(String::as_str));
                    }
                }
                _ if long("--line-length") && value.is_none() => i += 1,
                _ => {}
            }
        } else if !options_done && arg.len() > 1 && arg.starts_with('-') {
            let cluster = &arg[1..];
            for (k, c) in cluster.char_indices() {
                let rest = &cluster[k + c.len_utf8()..];
                match c {
                    // GNU `-i[SUFFIX]`, BSD `-i SUFFIX` / `-I`.
                    'i' | 'I' => return Some(true),
                    'f' => return Some(false),
                    'e' => {
                        script_given = true;
                        if rest.is_empty() {
                            i += 1;
                            scripts.extend(args.get(i).map(String::as_str));
                        } else {
                            scripts.push(rest);
                        }
                        break;
                    }
                    'l' => {
                        if rest.is_empty() {
                            i += 1;
                        }
                        break;
                    }
                    _ => {}
                }
            }
        } else if !script_given {
            script_given = true;
            scripts.push(arg);
        }
        i += 1;
    }
    scripts.into_iter().filter_map(sed_script_effect).max()
}

/// [`sed_effect`] of one script: `None` when every command only reads.
fn sed_script_effect(script: &str) -> Option<bool> {
    let b = script.as_bytes();
    let mut i = 0;
    let skip_to = |i: &mut usize, stops: &[u8]| {
        while *i < b.len() && !stops.contains(&b[*i]) {
            *i += 1;
        }
    };
    // Past a `delim`-terminated part starting at `*i`; false if unterminated.
    let delimited = |i: &mut usize, delim: u8| -> bool {
        while *i < b.len() {
            match b[*i] {
                b'\\' => *i += 2,
                c if c == delim => {
                    *i += 1;
                    return true;
                }
                b'\n' => return false,
                _ => *i += 1,
            }
        }
        false
    };
    let unclear = Some(false);
    loop {
        while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | b';' | b'{' | b'}') {
            i += 1;
        }
        if i >= b.len() {
            return None;
        }
        // Up to two addresses, then `!`.
        for n in 0..2 {
            if n == 1 {
                if b.get(i) != Some(&b',') {
                    break;
                }
                i += 1;
            }
            match b.get(i) {
                Some(b'0'..=b'9' | b'+' | b'~') => {
                    i += 1;
                    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'~') {
                        i += 1;
                    }
                }
                Some(b'$') => i += 1,
                Some(b'/') => {
                    i += 1;
                    if !delimited(&mut i, b'/') {
                        return unclear;
                    }
                    while matches!(b.get(i), Some(b'I' | b'M')) {
                        i += 1;
                    }
                }
                Some(b'\\') => {
                    let Some(&delim) = b.get(i + 1) else {
                        return unclear;
                    };
                    i += 2;
                    if !delimited(&mut i, delim) {
                        return unclear;
                    }
                    while matches!(b.get(i), Some(b'I' | b'M')) {
                        i += 1;
                    }
                }
                _ => {}
            }
        }
        while matches!(b.get(i), Some(b' ' | b'\t' | b'!')) {
            i += 1;
        }
        let Some(&cmd) = b.get(i) else {
            return unclear;
        };
        i += 1;
        match cmd {
            b'w' | b'W' | b'e' => return Some(true),
            b's' => {
                let Some(&delim) = b.get(i) else {
                    return unclear;
                };
                i += 1;
                if !delimited(&mut i, delim) || !delimited(&mut i, delim) {
                    return unclear;
                }
                while let Some(&flag) = b.get(i) {
                    match flag {
                        b'w' | b'e' => return Some(true),
                        b'g' | b'p' | b'i' | b'I' | b'm' | b'M' | b'0'..=b'9' => i += 1,
                        b' ' | b'\t' | b'\n' | b';' | b'}' => break,
                        _ => return unclear,
                    }
                }
            }
            b'y' => {
                let Some(&delim) = b.get(i) else {
                    return unclear;
                };
                i += 1;
                if !delimited(&mut i, delim) || !delimited(&mut i, delim) {
                    return unclear;
                }
            }
            // Text, a file to read, or a comment, up to the end of the line.
            b'a' | b'i' | b'c' | b'r' | b'R' | b'#' => skip_to(&mut i, b"\n"),
            // A label or GNU version.
            b'b' | b't' | b'T' | b':' | b'v' => skip_to(&mut i, b";\n"),
            b'q' | b'Q' | b'l' | b'L' => {
                while i < b.len() && (b[i].is_ascii_digit() || b[i] == b' ') {
                    i += 1;
                }
            }
            b'=' | b'd' | b'D' | b'g' | b'G' | b'h' | b'H' | b'n' | b'N' | b'p' | b'P' | b'x'
            | b'z' | b'F' => {}
            _ => return unclear,
        }
    }
}

/// What a script-carrying or other non-writer program does to the lean-ctx
/// path one of its arguments names: `None` reads it, `Some(true)` writes
/// it, `Some(false)` may.
fn program_effect(tokens: &[String]) -> Option<bool> {
    let args = &tokens[1..];
    match tokens[0].as_str() {
        "sed" | "gsed" => sed_effect(args),
        "awk" | "gawk" | "mawk" | "nawk" => {
            // Output redirection, pipes and system() are the only ways an
            // awk program writes; options that load code are unclear.
            let unclear = args.iter().any(|a| {
                matches!(
                    a.as_str(),
                    "-f" | "--file" | "-i" | "--include" | "-E" | "--exec" | "-l" | "--load"
                ) || a.contains(['>', '|'])
                    || a.contains("system")
            });
            unclear.then_some(false)
        }
        "perl" => {
            // `-i`, `-pi`, `-i.bak`: in place. Any other perl may write.
            let in_place = args.iter().any(|a| {
                a.strip_prefix('-')
                    .filter(|c| !c.starts_with('-'))
                    .is_some_and(|c| c.split(['e', 'E']).next().unwrap_or("").contains('i'))
            });
            Some(in_place)
        }
        // `find` reads unless it deletes, writes a listing or runs a command.
        "find" => args
            .iter()
            .any(|a| {
                matches!(
                    a.as_str(),
                    "-delete"
                        | "-fprint"
                        | "-fprint0"
                        | "-fprintf"
                        | "-fls"
                        | "-ok"
                        | "-okdir"
                        | "-exec"
                        | "-execdir"
                )
            })
            .then_some(true),
        _ => Some(false),
    }
}

/// The paths a word names: the word itself and, for a short option with an
/// attached value (`-o$HOME/x`, `-C/tmp`), that value.
fn word_paths(token: &str, homes: &[std::path::PathBuf]) -> Vec<std::path::PathBuf> {
    let mut out = as_paths(token, homes);
    let bytes = token.as_bytes();
    if bytes.len() > 2 && bytes[0] == b'-' && bytes[1].is_ascii_alphabetic() {
        out.extend(as_paths(&token[2..], homes));
    }
    out
}

/// The value of option `short` (`-C v`, `-Cv`) or `long` (`--long v`,
/// `--long=v`) in `args`.
fn option_value<'a>(args: &'a [String], short: &str, long: &str) -> Option<&'a str> {
    args.iter().enumerate().find_map(|(k, a)| {
        if (!short.is_empty() && a == short) || (!long.is_empty() && a == long) {
            return args.get(k + 1).map(String::as_str);
        }
        if !long.is_empty()
            && let Some(v) = a.strip_prefix(long).and_then(|r| r.strip_prefix('='))
        {
            return Some(v);
        }
        a.strip_prefix(short)
            .filter(|v| !v.is_empty() && short.len() == 2)
    })
}

/// Whether a short-option cluster in `args` (or the dash-less mode word of
/// `tar`) holds `letter`, or a long option equals `long`.
fn has_flag(args: &[String], letter: char, long: &[&str], mode_word: bool) -> bool {
    args.iter().enumerate().any(|(k, a)| {
        long.contains(&a.as_str())
            || (a.starts_with('-') && !a.starts_with("--") && a[1..].contains(letter))
            || (mode_word && k == 0 && !a.starts_with('-') && a.contains(letter))
    })
}

/// What program `base` with `args` writes in the working directory, as a
/// word for the refusal; `None` when it writes nothing there. Read when the
/// working directory is unknown (GHSA-cfmv-qg29-3rvp): an in-place editor
/// (`sed -i`, `perl -i`), `sort -o`, a (de)compressor, `patch`, `split`, an
/// extractor without an absolute destination.
fn writes_into_cwd(base: &str, args: &[String]) -> Option<String> {
    let relative = || args.iter().find(|a| is_relative_operand(a)).cloned();
    let into_cwd = || Some("(the working directory)".to_string());
    match base {
        "sed" | "gsed" | "perl" => {
            let mut tokens = vec![base.to_string()];
            tokens.extend_from_slice(args);
            if program_effect(&tokens) != Some(true) && !has_flag(args, 'i', &[], false) {
                return None;
            }
            relative().or_else(into_cwd)
        }
        "sort" => {
            let out = option_value(args, "-o", "--output")?;
            is_relative_operand(out).then(|| out.to_string())
        }
        "gzip" | "gunzip" | "bzip2" | "bunzip2" | "xz" | "unxz" | "lzma" | "unlzma" | "zstd"
        | "unzstd" | "lz4" | "compress" | "uncompress" | "zip" => {
            let to_stdout = has_flag(args, 'c', &["--stdout", "--to-stdout"], false)
                || has_flag(args, 'l', &["--list"], false)
                || has_flag(args, 't', &["--test"], false);
            if to_stdout && base != "zip" {
                return None;
            }
            relative()
        }
        "patch" => {
            if args.iter().any(|a| a == "--dry-run") {
                return None;
            }
            relative().or_else(into_cwd)
        }
        "split" | "csplit" => relative().or_else(into_cwd),
        "tar" | "bsdtar" | "gtar" => {
            let extract = has_flag(args, 'x', &["--extract", "--get"], true);
            let dest = option_value(args, "-C", "--directory");
            (extract && dest.is_none_or(is_relative_operand))
                .then(|| dest.map_or("(the working directory)".to_string(), str::to_string))
        }
        "unzip" => {
            let listing = has_flag(args, 'l', &[], false)
                || has_flag(args, 't', &[], false)
                || has_flag(args, 'p', &[], false)
                || has_flag(args, 'Z', &[], false);
            let dest = option_value(args, "-d", "");
            (!listing && dest.is_none_or(is_relative_operand))
                .then(|| dest.map_or("(the working directory)".to_string(), str::to_string))
        }
        "7z" | "7za" | "cpio" | "pax" => into_cwd(),
        _ => None,
    }
}

/// The narrow cases where a writer naming a directory ABOVE lean-ctx's state
/// is judged by the path it creates instead of refused: a non-recursive
/// `cp`/`mv`/`install` of plain files (`cp f ..` creates `../f`). `None`:
/// not such a case; `Some(Err(word))`: the created path reaches the state.
fn plain_copy_effect(
    tokens: &[String],
    inside: &dyn Fn(&str) -> bool,
    reaches: &dyn Fn(&str) -> bool,
    target_hits: &dyn Fn(&std::path::Path) -> bool,
    homes: &[std::path::PathBuf],
) -> Option<Result<(), String>> {
    let base = tokens[0].as_str();
    if !matches!(base, "cp" | "mv" | "install") {
        return None;
    }
    let args = &tokens[1..];
    let recursive = has_flag(args, 'r', &["--recursive", "--archive"], false)
        || has_flag(args, 'R', &[], false)
        || (base == "cp" && has_flag(args, 'a', &[], false))
        || has_flag(args, 'T', &["--no-target-directory"], false);
    if recursive {
        return None;
    }
    let target_dir = option_value(args, "-t", "--target-directory");
    let operands: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    let dest = target_dir.or_else(|| operands.last().map(|a| a.as_str()))?;
    let sources: Vec<&String> = operands
        .iter()
        .copied()
        .filter(|a| a.as_str() != dest)
        .collect();
    let plain = |word: &str| {
        !word.is_empty()
            && !word.ends_with('/')
            && !word.ends_with("/.")
            && word != "."
            && word != ".."
            && !word.contains(['*', '?', '[', '{', '$', '`'])
            && !word_paths(word, homes).iter().any(|p| p.is_dir())
    };
    if sources.is_empty() || !sources.iter().all(|s| plain(s)) {
        return None;
    }
    // Moving lean-ctx's state (or a directory above it) away writes it.
    if base == "mv"
        && let Some(src) = sources.iter().find(|a| reaches(a))
    {
        return Some(Err((*src).clone()));
    }
    if inside(dest) {
        return Some(Err(dest.to_string()));
    }
    let lands = word_paths(dest, homes).into_iter().any(|dp| {
        let into_dir =
            target_dir.is_some() || sources.len() > 1 || dest.ends_with('/') || dp.is_dir();
        if !into_dir {
            return target_hits(&dp);
        }
        sources.iter().any(|src| {
            std::path::Path::new(src.as_str())
                .file_name()
                .is_some_and(|name| target_hits(&dp.join(name)))
        })
    });
    Some(if lands { Err(dest.to_string()) } else { Ok(()) })
}

/// Extraction (`tar x`, `unzip`, `7z x`, `cpio -i`, `pax -r`) writes below
/// its destination, the run directory by default; absolute member names
/// write anywhere. The word that reaches the state, if any.
fn extraction_effect(tokens: &[String], reaches: &dyn Fn(&str) -> bool) -> Option<String> {
    let base = tokens[0].as_str();
    let args = &tokens[1..];
    let operands: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    let extracts = match base {
        "unzip" => true,
        "cpio" => has_flag(args, 'i', &["--extract"], false),
        "pax" => has_flag(args, 'r', &[], false),
        "7z" | "7za" => matches!(operands.first().map(|a| a.as_str()), Some("x" | "e")),
        "tar" | "bsdtar" | "gtar" => {
            has_flag(args, 'x', &["--extract", "--get"], true)
                || args.first().is_some_and(|m| m == "x" || m == "-x")
        }
        _ => false,
    };
    if !extracts {
        return None;
    }
    if (matches!(base, "tar" | "bsdtar" | "gtar")
        && has_flag(args, 'P', &["--absolute-names"], true))
        || (base == "unzip" && args.iter().any(|a| a == "-:"))
    {
        return Some(format!("{} (absolute member paths)", args.join(" ")));
    }
    let dest = match base {
        "unzip" => option_value(args, "-d", ""),
        "cpio" => option_value(args, "-D", "--directory"),
        "7z" | "7za" => args.iter().find_map(|a| a.strip_prefix("-o")),
        "pax" => None,
        _ => option_value(args, "-C", "--directory"),
    }
    .unwrap_or(".");
    reaches(dest).then(|| dest.to_string())
}

/// Flags and subcommands with which these listing/launching programs write
/// or run commands (`tree -o`, `fd -x`, `code --user-data-dir`, `open
/// --stdout`, `bat cache --build`).
fn viewer_writes(tokens: &[String]) -> bool {
    let args = &tokens[1..];
    let long = |names: &[&str]| {
        args.iter().any(|a| {
            names
                .iter()
                .any(|n| a == n || a.strip_prefix(n).is_some_and(|r| r.starts_with('=')))
        })
    };
    match tokens[0].as_str() {
        "tree" => has_flag(args, 'o', &[], false) || long(&["--output"]),
        "fd" => {
            has_flag(args, 'x', &[], false)
                || has_flag(args, 'X', &[], false)
                || long(&["--exec", "--exec-batch"])
        }
        "code" => long(&["--user-data-dir", "--extensions-dir"]),
        "open" => long(&["--stdout", "--stderr", "--stdin"]),
        "bat" => args.iter().any(|a| a == "cache"),
        _ => false,
    }
}

/// `git -c` keys a read may set: display only, none names a program or a
/// file git runs or writes.
fn git_config_key_is_display(setting: &str) -> bool {
    let key = setting
        .split_once('=')
        .map_or(setting, |(k, _)| k)
        .to_ascii_lowercase();
    key.starts_with("color.")
        || matches!(
            key.as_str(),
            "core.quotepath"
                | "log.date"
                | "log.decorate"
                | "log.abbrevcommit"
                | "log.showsignature"
                | "diff.renames"
                | "diff.algorithm"
                | "diff.relative"
                | "column.ui"
        )
}

/// GHSA-fv44-pm58-9c8p: options with which a git "read" subcommand writes
/// a file (`--output`, abbreviated as far as `--ou`) or runs a program (a
/// pager, external diff, textconv or clean/smudge filter, `grep -O`).
fn git_read_option_runs_or_writes(arg: &str) -> bool {
    ["--ou", "--ext", "--textc", "--filt", "--open"]
        .iter()
        .any(|p| arg.starts_with(p))
        || (arg.starts_with("-O") && !arg.starts_with("--"))
}

/// The files a git command writes through `--output` (`--output=f`,
/// `--output f`, an abbreviation down to `--ou`).
pub(super) fn git_output_targets(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut k = 0;
    while k < args.len() {
        let a = args[k].as_str();
        if let Some(rest) = a.strip_prefix("--ou")
            && "tput".starts_with(rest.split('=').next().unwrap_or(""))
        {
            if let Some((_, v)) = rest.split_once('=') {
                out.push(v.to_string());
            } else {
                if let Some(v) = args.get(k + 1) {
                    out.push(v.clone());
                }
                k += 1;
            }
        }
        k += 1;
    }
    out
}

/// `git` at or above lean-ctx's state — through `-C`/`--git-dir`/
/// `--work-tree` or the directory it runs in — may only read: a dotfiles
/// checkout there rewrites the config. A read that pages (`-p`,
/// `--paginate`), sets a `-c` key other than a display one, loads programs
/// (`--exec-path`, `--config-env`) or carries a writing or program-running
/// option is no read (GHSA-fv44-pm58-9c8p). The repository word when
/// refused.
fn git_effect(
    tokens: &[String],
    reaches: &dyn Fn(&str) -> bool,
    cwd_reaches: bool,
) -> Option<String> {
    let mut repo = Vec::new();
    let mut runs = false;
    let mut i = 1;
    while let Some(t) = tokens.get(i) {
        match t.as_str() {
            "-C" | "--git-dir" | "--work-tree" => {
                repo.extend(tokens.get(i + 1).cloned());
                i += 2;
            }
            "-c" => {
                runs |= !tokens
                    .get(i + 1)
                    .is_some_and(|kv| git_config_key_is_display(kv));
                i += 2;
            }
            "-p" | "--paginate" | "--config-env" | "--exec-path" => {
                runs = true;
                i += if t == "--config-env" { 2 } else { 1 };
            }
            t if t.starts_with("--git-dir=") || t.starts_with("--work-tree=") => {
                repo.push(t.to_string());
                i += 1;
            }
            t if t.starts_with("--config-env=") || t.starts_with("--exec-path=") => {
                runs = true;
                i += 1;
            }
            t if t.starts_with("-c") && t.len() > 2 => {
                runs |= !git_config_key_is_display(&t[2..]);
                i += 1;
            }
            t if t.starts_with('-') => i += 1,
            _ => break,
        }
    }
    let named = repo
        .into_iter()
        .find(|r| reaches(r))
        .or_else(|| cwd_reaches.then(|| "(run directory)".to_string()))?;
    let sub = tokens.get(i).map_or("", String::as_str);
    let reads = !runs
        && !tokens[i.min(tokens.len())..]
            .iter()
            .any(|a| git_read_option_runs_or_writes(a))
        && (GIT_READ_SUBCOMMANDS.contains(&sub)
            || (matches!(sub, "branch" | "tag") && tokens.len() == i + 1));
    (!reads).then_some(named)
}

/// Builtins that move the shell into a directory.
const DIRECTORY_CHANGERS: &[&str] = &["cd", "pushd"];

/// Listing and launching programs that only read unless given one of the
/// flags [`viewer_writes`] knows.
const VIEWERS: &[&str] = &["tree", "fd", "code", "open", "bat"];

/// Whether `token` names a path inside lean-ctx's state (a glob: may match one).
fn names_inside(
    token: &str,
    protected: &[std::path::PathBuf],
    homes: &[std::path::PathBuf],
) -> bool {
    if let Some(glob) = token.find(['*', '?', '[']) {
        // A glob may match lean-ctx's directories: compare its literal
        // prefix with them in both directions. A prefix without a directory
        // (`*`, `lean-c*`) lies in the working directory.
        let prefix = &token[..glob];
        let placed;
        let prefix = if prefix.starts_with(['~', '$']) || prefix.contains(['/', '\\']) {
            prefix
        } else {
            placed = format!("./{prefix}");
            placed.as_str()
        };
        return word_paths(prefix, homes).iter().any(|p| {
            let key = path_key(p);
            protected
                .iter()
                .any(|dir| within(p, dir) || path_key(dir).starts_with(&key))
        });
    }
    word_paths(token, homes)
        .iter()
        .any(|p| protected.iter().any(|dir| within(p, dir)))
}

/// Whether `token` names lean-ctx's state or a directory above it.
fn names_reaching(
    token: &str,
    protected: &[std::path::PathBuf],
    homes: &[std::path::PathBuf],
) -> bool {
    names_inside(token, protected, homes)
        || word_paths(token, homes).iter().any(|p| {
            protected
                .iter()
                .any(|dir| within(p, dir) || ancestor_of(p, dir))
        })
}

/// How many words the redirection at `tokens[i]` takes (`>f`, `2>>f`,
/// `&>f`, `<f`, `<<<w`, `>&2`: one; `> f`: two), or 0 for any other word.
fn redirection_words(tokens: &[String], i: usize) -> usize {
    let token = tokens[i].as_str();
    if token.starts_with("<(") || token.starts_with(">(") {
        // Process substitution: a command, not a redirection.
        return 0;
    }
    let op = token.trim_start_matches(|c: char| c.is_ascii_digit());
    let op = op
        .strip_prefix('&')
        .filter(|r| r.starts_with('>'))
        .unwrap_or(op);
    let rest = [">>", ">|", ">&", "<>", "<<<", "<<-", "<<", "<&", ">", "<"]
        .iter()
        .find_map(|p| op.strip_prefix(p));
    match rest {
        None => 0,
        Some("") if i + 1 < tokens.len() => 2,
        Some(_) => 1,
    }
}

/// `tokens` without their redirections, wherever they stand (`>f timeout 5
/// cat`, `timeout 5 2>/dev/null ls`): the shell applies them before it runs
/// the program, so they are not words of the command. Returns the kept words
/// and the removed ones.
fn split_redirections(tokens: &[String]) -> (Vec<String>, Vec<String>) {
    let mut kept = Vec::with_capacity(tokens.len());
    let mut removed = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        match redirection_words(tokens, i) {
            0 => {
                kept.push(tokens[i].clone());
                i += 1;
            }
            n => {
                removed.extend_from_slice(&tokens[i..i + n]);
                i += n;
            }
        }
    }
    (kept, removed)
}

fn without_redirections(tokens: &[String]) -> Vec<String> {
    split_redirections(tokens).0
}

/// `NAME=VALUE`, or one whose name is built at run time (`${N}=x`).
fn is_assignment_word(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        is_identifier(name) || (!name.is_empty() && name.contains(['$', '`', '{', '\\']))
    })
}

/// Variables that change which program, library or startup file a wrapped
/// command runs (`env PATH=/x ls`, `env LD_PRELOAD=… ls`).
fn changes_lookup(assignment: &str) -> bool {
    let Some((name, _)) = assignment.split_once('=') else {
        return false;
    };
    !is_identifier(name)
        || super::side_doors::is_blocked_env(name)
        || matches!(
            name,
            "PATH"
                | "IFS"
                | "SHELL"
                | "CDPATH"
                | "BASHOPTS"
                | "SHELLOPTS"
                | "GLOBIGNORE"
                | "HOME"
                | "BASH_ENV"
                | "ENV"
        )
}

/// Whether `env`'s own words split a string into the command (`-S`,
/// `--split-string`), move it into another directory (`-C`, `--chdir`) or
/// search another path for it (BSD `-P`). Any single-dash word holding one
/// of those letters counts, even as an option value: refusing too much is
/// the safe side.
fn env_relocates(own: &[String]) -> bool {
    own.iter().any(|w| {
        if let Some(long) = w.strip_prefix("--") {
            let name = long.split('=').next().unwrap_or("");
            return !name.is_empty()
                && ["split-string", "chdir"]
                    .iter()
                    .any(|o| o.starts_with(name));
        }
        w.strip_prefix('-')
            .is_some_and(|cluster| cluster.contains(['S', 'C', 'P']))
    })
}

/// A command with its reserved words and wrappers peeled off.
#[derive(Default)]
struct Peeled {
    /// The simple command that runs, its program word reduced to its
    /// basename; empty when none runs.
    program: Vec<String>,
    /// The command from the first wrapper whose own words reach lean-ctx's
    /// state, or that changes how or where the command is found
    /// (`env -S`, `env -C`, `env PATH=…`): judged as written, the wrapper
    /// as the program, in addition to [`Self::program`].
    conservative: Option<Vec<String>>,
    /// The words a `for`/`select` header lists.
    header: Option<Vec<String>>,
}

/// The last path component of a program word.
fn basename(word: &str) -> String {
    word.rsplit('/').next().unwrap_or(word).to_string()
}

/// Peels reserved words (`do`, `then`, `!`, `time -p`, …), a `for`/`select`
/// header and the wrappers in [`PEELED_WRAPPERS`] (parsed by
/// `delegated_command_tokens`) off `tokens`, whose redirections are already
/// removed.
fn peel(mut words: Vec<String>, reaches: &dyn Fn(&str) -> bool) -> Peeled {
    let mut out = Peeled::default();
    let original = words.clone();
    let mut after_wrapper = false;
    for _ in 0..MAX_PEEL {
        if !after_wrapper {
            let skip = words
                .iter()
                .take_while(|w| is_assignment_word(w) || matches!(w.as_str(), "{" | "("))
                .count();
            words.drain(..skip);
        }
        let Some(first) = words.first().cloned() else {
            return out;
        };
        if COMMAND_KEYWORDS.contains(&first.as_str()) {
            words.remove(0);
            after_wrapper = false;
            continue;
        }
        if first == "time" {
            words.remove(0);
            while matches!(words.first().map(String::as_str), Some("-p" | "--")) {
                words.remove(0);
            }
            after_wrapper = false;
            continue;
        }
        if matches!(first.as_str(), "for" | "select") {
            let body_at = words.iter().position(|w| w == "do");
            let list_end = body_at.unwrap_or(words.len());
            let list = if words.get(2).map(String::as_str) == Some("in") {
                words[3.min(list_end)..list_end]
                    .iter()
                    .map(|w| w.trim_end_matches(';').to_string())
                    .collect()
            } else {
                Vec::new()
            };
            out.header.get_or_insert_with(Vec::new).extend(list);
            words = body_at.map_or_else(Vec::new, |at| words[at + 1..].to_vec());
            after_wrapper = false;
            continue;
        }
        let wrapper = basename(&first);
        if !PEELED_WRAPPERS.contains(&wrapper.as_str()) {
            words[0] = wrapper;
            out.program = words;
            return out;
        }
        let rest = &words[1..];
        let inner = super::enforcement::delegated_command_tokens(&wrapper, rest);
        let own = if rest.ends_with(&inner) {
            &rest[..rest.len() - inner.len()]
        } else {
            rest
        };
        let risky = own.iter().any(|w| reaches(w))
            || (wrapper == "env" && env_relocates(own))
            || super::enforcement::wrapper_assignment_words(&wrapper, rest)
                .iter()
                .any(|a| changes_lookup(a));
        if risky && out.conservative.is_none() {
            // The wrapper as written, plus the words it splits out of a
            // string (`env -S 'rm -rf ~'`) when they are not its own.
            let mut as_written = words.clone();
            as_written[0] = wrapper;
            if !rest.ends_with(&inner) {
                as_written.extend(inner.iter().cloned());
            }
            out.conservative = Some(as_written);
        }
        words = inner;
        after_wrapper = true;
    }
    // Nested deeper than this check peels: judged as written.
    let mut as_written = original;
    if let Some(first) = as_written.first_mut() {
        *first = basename(first);
    }
    out.conservative.get_or_insert(as_written);
    out
}

/// What `tokens`, one simple command as written, does to lean-ctx's state:
/// a redirection into it writes it, whatever the program and wherever the
/// redirection stands; a `for`/`select` header may not list a word inside
/// it; otherwise the command behind its reserved words and wrappers is
/// judged by its program (#2231).
fn touches_protected(
    tokens: &[String],
    protected: &[std::path::PathBuf],
    homes: &[std::path::PathBuf],
) -> Option<Tampering> {
    let inside = |token: &str| names_inside(token, protected, homes);
    let reaches = |token: &str| names_reaching(token, protected, homes);
    let (words, removed) = split_redirections(tokens);
    let shown = |base: &str, token: &str| {
        let path = word_paths(token, homes)
            .last()
            .map_or_else(|| token.to_string(), |p| p.display().to_string());
        format!("{base} {path}")
    };
    // A redirect into lean-ctx's state writes it, whatever the program.
    if let Some(target) = (0..tokens.len())
        .filter_map(|i| redirect_target(tokens, i))
        .find(|t| inside(t))
    {
        let base = words.first().map_or_else(String::new, |w| basename(w));
        return Some(Tampering::new(shown(&base, target), true));
    }
    // A removed word naming a directory above the state was likely a quoted
    // operand (`rm -rf ">" ~`), not a redirection: judge the line as written.
    if removed.iter().any(|w| reaches(w)) {
        let mut as_written = tokens.to_vec();
        as_written[0] = basename(&as_written[0]);
        if let Some(verdict) = touches_program(&as_written, protected, homes) {
            return Some(verdict);
        }
    }
    let peeled = peel(words, &reaches);
    if let Some(word) = peeled.header.iter().flatten().find(|w| inside(w)) {
        return Some(Tampering::new(shown("for", word), false));
    }
    if let Some(verdict) = peeled
        .conservative
        .as_deref()
        .and_then(|as_written| touches_program(as_written, protected, homes))
    {
        return Some(verdict);
    }
    touches_program(&peeled.program, protected, homes)
}

/// The program words (`peel`) of a command when it moves the shell into a
/// directory (`cd`, `pushd`, `popd`, also behind `builtin`/`command`).
fn directory_change(tokens: &[String]) -> Option<Vec<String>> {
    let program = peel(without_redirections(tokens), &|_| false).program;
    program
        .first()
        .is_some_and(|b| matches!(b.as_str(), "cd" | "pushd" | "popd"))
        .then_some(program)
}

/// What one simple command, its program word a basename, does to lean-ctx's
/// state.
fn touches_program(
    tokens: &[String],
    protected: &[std::path::PathBuf],
    homes: &[std::path::PathBuf],
) -> Option<Tampering> {
    let base = tokens.first()?.as_str();
    let inside = |token: &str| names_inside(token, protected, homes);
    let target_hits = |p: &std::path::Path| {
        protected
            .iter()
            .any(|dir| within(p, dir) || ancestor_of(p, dir))
    };
    let reaches = |token: &str| names_reaching(token, protected, homes);
    // A word naming a protected directory anywhere inside it (a sed or awk
    // script: `sed -n '1w ~/.config/lean-ctx/config.toml'`).
    let mentions = |token: &str| {
        inside(token) || {
            let text = path_key(std::path::Path::new(token));
            protected.iter().any(|dir| {
                spellings(dir, homes)
                    .iter()
                    .any(|form| text.contains(form.as_str()))
            })
        }
    };
    let shown = |token: &str| {
        let path = word_paths(token, homes)
            .last()
            .map_or_else(|| token.to_string(), |p| p.display().to_string());
        format!("{base} {path}")
    };
    let writes = |what: String| Some(Tampering::new(what, true));
    // A redirect into lean-ctx's state writes it, whatever the program.
    if let Some(target) = (1..tokens.len())
        .filter_map(|i| redirect_target(tokens, i))
        .find(|t| inside(t))
    {
        return writes(shown(target));
    }
    if DIRECTORY_CHANGERS.contains(&base) {
        let named = tokens[1..].iter().find(|t| inside(t))?;
        return Some(Tampering::new(shown(named), false));
    }
    if READ_ONLY_COMMANDS.contains(&base) {
        return None;
    }
    // lean-ctx itself, and `cargo run -- …` (lean-ctx run from its source).
    let leanctx_args = if base == "lean-ctx" || base == "lean-ctx.exe" {
        Some(&tokens[1..])
    } else if base == "cargo" && tokens.get(1).is_some_and(|t| t == "run") {
        tokens
            .iter()
            .position(|t| t == "--")
            .map(|at| &tokens[at + 1..])
    } else {
        None
    };
    if let Some(args) = leanctx_args {
        if args
            .first()
            .is_some_and(|sub| LEANCTX_READ_SUBCOMMANDS.contains(&sub.as_str()))
        {
            return None;
        }
        if base != "cargo" {
            // After a `cd` this check cannot follow, a relative output
            // path may be anywhere.
            if line_cwds().is_none()
                && let Some(word) = args.iter().skip(1).find(|t| !t.starts_with('-'))
            {
                return Some(Tampering {
                    unplaced: Some(unknown_cwd_cause()),
                    ..Tampering::new(format!("{base} {word}"), false)
                });
            }
            let named = args.iter().find(|t| inside(t))?;
            return writes(shown(named));
        }
    }
    // After a `cd` this check cannot follow, a relative path a writing
    // program writes may be anywhere. For curl and wget only the files they
    // write count (`-o`, `-O`, wget's document directory) — never the URL or
    // another option's value (#2201).
    if WRITE_PROGRAMS.contains(&base) && line_cwds().is_none() {
        let relative = if matches!(base, "curl" | "wget") {
            download_targets(base, &tokens[1..])
                .into_iter()
                .find(|t| is_relative_operand(t))
                .map(|t| {
                    if t == "." {
                        "(a download into the working directory)".to_string()
                    } else {
                        t
                    }
                })
        } else {
            tokens[1..].iter().find(|t| is_relative_operand(t)).cloned()
        };
        if let Some(word) = relative {
            return Some(Tampering {
                unplaced: Some(unknown_cwd_cause()),
                ..Tampering::new(format!("{base} {word}"), false)
            });
        }
    }
    // GHSA-cfmv-qg29-3rvp: programs that write into the working directory
    // or a relative path given to them (`sed -i f`, `tar -x`, `sort -o f`,
    // `gzip f`, `patch`, `split`) after a `cd` this check cannot follow.
    if line_cwds().is_none()
        && let Some(word) = writes_into_cwd(base, &tokens[1..])
    {
        return Some(Tampering {
            unplaced: Some(unknown_cwd_cause()),
            ..Tampering::new(format!("{base} {word}"), false)
        });
    }
    if base == "git" {
        // GHSA-fv44-pm58-9c8p: `--output` writes its file wherever the
        // repository is (`git -C /tmp/r log --output=~/.config/lean-ctx/x`).
        let dir = option_value(&tokens[1..], "-C", "");
        for target in git_output_targets(&tokens[1..]) {
            let placed = match dir {
                Some(d) if is_relative_operand(&target) => format!("{d}/{target}"),
                _ => target,
            };
            if inside(&placed) {
                return writes(shown(&placed));
            }
            if is_relative_operand(&placed) && line_cwds().is_none() {
                return Some(Tampering {
                    unplaced: Some(unknown_cwd_cause()),
                    ..Tampering::new(format!("{base} --output={placed}"), false)
                });
            }
        }
        let cwd_reaches = line_cwds().is_some_and(|cwds| cwds.iter().any(|cwd| target_hits(cwd)));
        return git_effect(tokens, &reaches, cwd_reaches).and_then(|repo| writes(shown(&repo)));
    }
    if let Some(word) = extraction_effect(tokens, &reaches) {
        return writes(shown(&word));
    }
    if VIEWERS.contains(&base) && !viewer_writes(tokens) {
        return None;
    }
    // curl/wget: any output option (`-o`, `-O`, `-D`, `-c`, `--trace`,
    // `--stderr`, a log file) inside the state writes it; a download
    // directory above it (`wget -P ~`) only creates a file there. A file
    // curl joins onto `--output-dir` is judged where it lands
    // (`--output-dir ~/.config -o lean-ctx/config.toml`). A config file the
    // call reads (`curl -K`, `-w @FILE`, `wget --config`) may name the state
    // in a way no token shows, so it counts as writing it wherever the call
    // runs (#2212, the #2165 write-anywhere path).
    if matches!(base, "curl" | "wget") {
        let targets = download_targets(base, &tokens[1..]);
        if targets.iter().any(|t| t == UNKNOWN_DOWNLOAD_TARGET) {
            return writes(format!("{base} {UNKNOWN_DOWNLOAD_TARGET}"));
        }
        return tokens[1..]
            .iter()
            .find(|t| inside(t))
            .cloned()
            .or_else(|| targets.into_iter().find(|t| inside(t)))
            .and_then(|t| writes(shown(&t)));
    }
    if let Some(verdict) = plain_copy_effect(tokens, &inside, &reaches, &target_hits, homes) {
        return verdict.err().and_then(|word| writes(shown(&word)));
    }
    // Scripts and `find` are judged by what they do: `sed -n` reads.
    if SCRIPT_PROGRAMS.contains(&base) || base == "find" {
        let named = tokens[1..].iter().find(|t| mentions(t) || reaches(t))?;
        return program_effect(tokens).map(|writes| Tampering::new(shown(named), writes));
    }
    // Every other program naming the state or a directory above it may
    // write there.
    let named = tokens[1..].iter().find(|t| reaches(t))?;
    Some(Tampering::new(shown(named), WRITE_PROGRAMS.contains(&base)))
}

/// Variables that move the repository `git` works on.
const GIT_LOCATION_VARIABLES: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
];

/// GHSA-cfmv-qg29-3rvp: a `git` run with `GIT_DIR`/`GIT_WORK_TREE` (a
/// prefix, `env`, or set earlier on the line) naming lean-ctx's state or a
/// directory above it works there like `--git-dir`/`--work-tree`. The
/// assignment that does, if any.
fn git_environment_reaching(
    segment: &str,
    protected: &[std::path::PathBuf],
    homes: &[std::path::PathBuf],
) -> Option<String> {
    let tokens = crate::shell_tokenize(segment.trim());
    if !tokens.iter().any(|t| basename(t) == "git") {
        return None;
    }
    let reaches = |value: &str| {
        expand_braces(value)
            .iter()
            .any(|v| names_reaching(&v.replace(OPAQUE_MARK, ""), protected, homes))
    };
    let mut assignments: Vec<(String, String)> = tokens
        .iter()
        .filter_map(|t| t.split_once('='))
        .map(|(n, v)| (n.to_string(), v.to_string()))
        .collect();
    LINE_VARS.with(|vars| {
        for (name, values) in vars.borrow().iter() {
            for value in values {
                assignments.push((name.clone(), value.clone()));
            }
        }
    });
    assignments
        .into_iter()
        .find(|(name, value)| GIT_LOCATION_VARIABLES.contains(&name.as_str()) && reaches(value))
        .map(|(name, value)| format!("{name}={value}"))
}

/// Whether an absolute `path` lies in lean-ctx's own config or data
/// directories (as this process resolves them and at the real user's home),
/// also after resolving its existing ancestors through symlinks. The native
/// write guard (`hook write-guard`) uses it to keep an agent's Write/Edit
/// from reconfiguring lean-ctx, as `check_state_tampering` does for the
/// shell.
pub fn is_leanctx_state_path(path: &std::path::Path) -> bool {
    let protected = protected_dirs();
    [resolve_existing(path), path.to_path_buf()]
        .iter()
        .any(|p| protected.iter().any(|dir| within(p, dir)))
}

/// Rejects a command that would change lean-ctx's own state from an agent
/// shell (deleting `worker.json` would lift the person's pause; editing the
/// claims file would steal a fencing token). Reading stays allowed, judged by
/// what the command does rather than by its program: `sed -n 1,3p` reads,
/// `sed -i` writes (#2165).
///
/// `segment` is the simple command as written; `tokens` the caller's words
/// for it, which drop a leading redirection's target (`>~/x timeout 5
/// true`): the segment's own words are judged, the caller's only when the
/// segment has none. A command behind `timeout`, `nice`, `env`, a reserved word or a
/// `for` header is judged by its program (#2231), once for every value of
/// the loop variables it expands.
pub(super) fn check_state_tampering(segment: &str, tokens: &[String]) -> Result<(), ShellError> {
    let homes = homes();
    let mut raw = crate::shell_tokenize(crate::skip_env_assignments(segment.trim()));
    if raw.first().is_some_and(|t| t == "{") {
        raw.remove(0);
    }
    // A segment of a line whose groups are not opened (`(cd /tmp`): the
    // program is the word after the `(`. A `cd` there moves only the
    // subshell, so it is not followed; the opened line is judged separately.
    let subshell = raw.first().is_some_and(|t| t.starts_with('('));
    if subshell {
        let program = raw[0].trim_start_matches('(').to_string();
        if program.is_empty() {
            raw.remove(0);
        } else {
            raw[0] = program;
        }
    }
    let words: &[String] = if raw.is_empty() { tokens } else { &raw };
    let protected = protected_dirs();
    let mut verdict = match bound_variants(words) {
        // A run-time value counts only where the program writes through it.
        Some(variants) => variants.iter().find_map(|(variant, opaque)| {
            touches_protected(variant, &protected, &homes)
                .filter(|t| !opaque || t.writes || t.unplaced.is_some())
        }),
        None => Some(Tampering::new(
            format!("{} (a loop over too many words)", words.join(" ")),
            false,
        )),
    };
    if verdict.is_none() {
        verdict = git_environment_reaching(segment, &protected, &homes)
            .map(|word| Tampering::new(format!("git {word}"), true));
    }
    if !subshell && let Some(program) = directory_change(words) {
        follow_directory_change(&program, &homes);
    }
    match verdict {
        Some(Tampering {
            what,
            unplaced: Some(cause),
            ..
        }) => Err(format!(
            "[BLOCKED — DO NOT RETRY] `{what}` writes a relative path, and {cause} could not \
             be resolved, so this check cannot tell whether it lands in lean-ctx's own state \
             (pause, claims, keys). Use a literal directory in the `cd`, or give the path \
             absolute."
        )
        .into()),
        Some(Tampering {
            what, writes: true, ..
        }) => Err(format!(
            "[BLOCKED — DO NOT RETRY] `{what}` changes lean-ctx's own state (pause, claims, \
             keys) and cannot run from an agent shell. Ask the user to run it in their own \
             terminal if they want it."
        )
        .into()),
        Some(Tampering {
            what,
            writes: false,
            unplaced: None,
        }) => Err(format!(
            "[BLOCKED — DO NOT RETRY] `{what}` runs a program on lean-ctx's own state (pause, \
             claims, keys) whose effect cannot be told from the command, so it may change \
             that state and cannot run from an agent shell. To read the file, use cat, head, \
             grep or `sed -n`; to change it, ask the user to run the command in their own \
             terminal."
        )
        .into()),
        None => Ok(()),
    }
}

#[cfg(test)]
#[path = "self_config_tests.rs"]
mod tests;
