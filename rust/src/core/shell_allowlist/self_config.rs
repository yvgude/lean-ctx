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

use crate::core::error::ShellError;

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
/// and compresses output. The [`crate::core::config`] project-key class plus
/// the theme; none of them can loosen security, destinations, redaction,
/// tools, hooks, rules, policy or upstreams.
#[must_use]
pub(crate) fn agent_settable_config_key(key: &str) -> bool {
    key == "theme" || key == "tee_on_error" || crate::core::config::is_project_key(key)
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
            Some("recall" | "search" | "status" | "health" | "export") => true,
            // Hive's reviewed knowledge reaches AGENTS.md only when a person
            // exports it (owner decision 22.10.).
            Some("export-agents-md") => !rest.iter().any(|a| a == "--include-hive"),
            _ => false,
        },
        "session" => matches!(next, Some("status")),
        // A Workspace bounds the agent itself (#2120): `remove` narrows,
        // `show`/`diagnose` only read.
        "workspace" => crate::core::workspace_args::agent_shell_may_run(rest),
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
    /// Moved by a literal `cd`.
    At(std::path::PathBuf),
    /// Moved somewhere this check cannot tell (`cd "$d"`, `cd -`, `popd`).
    Unknown,
}

thread_local! {
    static LINE_CWD: std::cell::RefCell<LineCwd> = const { std::cell::RefCell::new(LineCwd::Process) };
}

/// Restores the enclosing line's working directory when dropped.
pub(super) struct LineScope(Option<LineCwd>);

/// Scopes the `cd` tracking to one command line: a nested line (`cd x &&
/// lean-ctx -c '…'`) starts where the outer one is, and whatever it moves
/// to is undone when it ends.
pub(super) fn enter_line() -> LineScope {
    LineScope(Some(LINE_CWD.with(|c| c.borrow().clone())))
}

impl Drop for LineScope {
    fn drop(&mut self) {
        if let Some(previous) = self.0.take() {
            LINE_CWD.with(|c| *c.borrow_mut() = previous);
        }
    }
}

thread_local! {
    /// The directory the checked command will run in, when the caller
    /// knows it (ctx_shell's `cwd`, a hook payload's `cwd`).
    static RUN_CWD: std::cell::RefCell<Option<std::path::PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Runs `f` with relative paths resolved against `cwd`, the directory the
/// checked command runs in, instead of this process's own.
pub(crate) fn with_run_cwd<T>(cwd: Option<std::path::PathBuf>, f: impl FnOnce() -> T) -> T {
    let previous = RUN_CWD.with(|c| c.replace(cwd));
    let out = f();
    RUN_CWD.with(|c| *c.borrow_mut() = previous);
    out
}

/// The directory relative words resolve against, `None` when unknown.
fn line_cwd() -> Option<std::path::PathBuf> {
    match LINE_CWD.with(|c| c.borrow().clone()) {
        LineCwd::Process => RUN_CWD
            .with(|c| c.borrow().clone())
            .or_else(|| std::env::current_dir().ok()),
        LineCwd::At(dir) => Some(dir),
        LineCwd::Unknown => None,
    }
}

/// Follows a `cd`/`pushd`/`popd` segment.
fn follow_directory_change(tokens: &[String], homes: &[std::path::PathBuf]) {
    let target = tokens[1..]
        .iter()
        .find(|t| !t.starts_with('-') || t.as_str() == "-");
    let next = match (tokens[0].as_str(), target) {
        ("popd", _) => LineCwd::Unknown,
        (_, None) => homes
            .first()
            .map_or(LineCwd::Unknown, |h| LineCwd::At(h.clone())),
        (_, Some(word)) if word == "-" || word.contains(['$', '`', '*', '?', '[', '{']) => {
            LineCwd::Unknown
        }
        (_, Some(word)) => as_paths(word, homes)
            .pop()
            .map_or(LineCwd::Unknown, LineCwd::At),
    };
    LINE_CWD.with(|c| *c.borrow_mut() = next);
}

/// lean-ctx's own config and data directories: the worker pause, held
/// claims, verification nonces, opt-in caches and keys live there — as this
/// process resolves them and at the real user's home, where the
/// shell-security policy lives (`config::shell_policy`).
fn protected_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = [
        crate::core::paths::config_dir_read_only(),
        crate::core::paths::data_dir_read_only(),
    ]
    .into_iter()
    .filter_map(Result::ok)
    .collect();
    if let Some(home) = crate::core::config::shell_policy::real_user_home() {
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
        crate::core::config::shell_policy::real_user_home(),
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
        if let Some(cwd) = line_cwd() {
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

/// `git` at or above lean-ctx's state — through `-C`/`--git-dir`/
/// `--work-tree` or the directory it runs in — may only read: a dotfiles
/// checkout there rewrites the config. The repository word when refused.
fn git_effect(
    tokens: &[String],
    reaches: &dyn Fn(&str) -> bool,
    cwd_reaches: bool,
) -> Option<String> {
    let mut repo = Vec::new();
    let mut i = 1;
    while let Some(t) = tokens.get(i) {
        match t.as_str() {
            "-C" | "--git-dir" | "--work-tree" => {
                repo.extend(tokens.get(i + 1).cloned());
                i += 2;
            }
            "-c" => i += 2,
            t if t.starts_with("--git-dir=") || t.starts_with("--work-tree=") => {
                repo.push(t.to_string());
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
    let reads = GIT_READ_SUBCOMMANDS.contains(&sub)
        || (matches!(sub, "branch" | "tag") && tokens.len() == i + 1);
    (!reads).then_some(named)
}

/// Builtins that move the shell into a directory.
const DIRECTORY_CHANGERS: &[&str] = &["cd", "pushd"];

/// Listing and launching programs that only read unless given one of the
/// flags [`viewer_writes`] knows.
const VIEWERS: &[&str] = &["tree", "fd", "code", "open", "bat"];

fn touches_protected(
    tokens: &[String],
    protected: &[std::path::PathBuf],
    homes: &[std::path::PathBuf],
) -> Option<Tampering> {
    let base = tokens.first()?.as_str();
    let inside = |token: &str| {
        if let Some(glob) = token.find(['*', '?', '[']) {
            // A glob may match lean-ctx's directories: compare its literal
            // prefix with them in both directions.
            return word_paths(&token[..glob], homes).iter().any(|p| {
                let key = path_key(p);
                protected
                    .iter()
                    .any(|dir| within(p, dir) || path_key(dir).starts_with(&key))
            });
        }
        word_paths(token, homes)
            .iter()
            .any(|p| protected.iter().any(|dir| within(p, dir)))
    };
    let target_hits = |p: &std::path::Path| {
        protected
            .iter()
            .any(|dir| within(p, dir) || ancestor_of(p, dir))
    };
    let reaches =
        |token: &str| inside(token) || word_paths(token, homes).iter().any(|p| target_hits(p));
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
    let writes = |what: String| Some(Tampering { what, writes: true });
    // A redirect into lean-ctx's state writes it, whatever the program.
    if let Some(target) = (1..tokens.len())
        .filter_map(|i| redirect_target(tokens, i))
        .find(|t| inside(t))
    {
        return writes(shown(target));
    }
    if DIRECTORY_CHANGERS.contains(&base) {
        let named = tokens[1..].iter().find(|t| inside(t))?;
        return Some(Tampering {
            what: shown(named),
            writes: false,
        });
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
            if line_cwd().is_none()
                && let Some(word) = args.iter().skip(1).find(|t| !t.starts_with('-'))
            {
                return Some(Tampering {
                    what: format!("{base} {word} (after a cd that cannot be followed)"),
                    writes: false,
                });
            }
            let named = args.iter().find(|t| inside(t))?;
            return writes(shown(named));
        }
    }
    // After a `cd` this check cannot follow, a relative operand of a
    // writing program may be anywhere.
    if WRITE_PROGRAMS.contains(&base) && line_cwd().is_none() {
        let relative = tokens[1..]
            .iter()
            .find(|t| !t.starts_with(['-', '/', '~', '$']) && !t.contains('='));
        if let Some(word) = relative {
            return Some(Tampering {
                what: format!("{base} {word} (after a cd that cannot be followed)"),
                writes: false,
            });
        }
    }
    if base == "git" {
        let cwd_reaches = line_cwd().is_some_and(|cwd| target_hits(&cwd));
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
    // directory above it (`wget -P ~`) only creates a file there.
    if matches!(base, "curl" | "wget") {
        return tokens[1..]
            .iter()
            .find(|t| inside(t))
            .and_then(|t| writes(shown(t)));
    }
    if let Some(verdict) = plain_copy_effect(tokens, &inside, &reaches, &target_hits, homes) {
        return verdict.err().and_then(|word| writes(shown(&word)));
    }
    // Scripts and `find` are judged by what they do: `sed -n` reads.
    if SCRIPT_PROGRAMS.contains(&base) || base == "find" {
        let named = tokens[1..].iter().find(|t| mentions(t) || reaches(t))?;
        return program_effect(tokens).map(|writes| Tampering {
            what: shown(named),
            writes,
        });
    }
    // Every other program naming the state or a directory above it may
    // write there.
    let named = tokens[1..].iter().find(|t| reaches(t))?;
    Some(Tampering {
        what: shown(named),
        writes: WRITE_PROGRAMS.contains(&base),
    })
}

/// Rejects a command that would change lean-ctx's own state from an agent
/// shell (deleting `worker.json` would lift the person's pause; editing the
/// claims file would steal a fencing token). Reading stays allowed, judged by
/// what the command does rather than by its program: `sed -n 1,3p` reads,
/// `sed -i` writes (#2165).
pub(super) fn check_state_tampering(tokens: &[String]) -> Result<(), ShellError> {
    let homes = homes();
    let verdict = touches_protected(tokens, &protected_dirs(), &homes);
    if tokens
        .first()
        .is_some_and(|b| matches!(b.as_str(), "cd" | "pushd" | "popd"))
    {
        follow_directory_change(tokens, &homes);
    }
    match verdict {
        Some(Tampering { what, writes: true }) => Err(format!(
            "[BLOCKED — DO NOT RETRY] `{what}` changes lean-ctx's own state (pause, claims, \
             keys) and cannot run from an agent shell. Ask the user to run it in their own \
             terminal if they want it."
        )
        .into()),
        Some(Tampering {
            what,
            writes: false,
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
