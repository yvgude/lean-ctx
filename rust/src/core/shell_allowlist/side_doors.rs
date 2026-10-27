//! Assignments and builtins that make the shell run code the segment check
//! never sees as a command (GHSA-rxv5-4c3x-g4q6 E): an `LD_PRELOAD` or
//! `BASH_ENV` set by `export` or by a second inline assignment, a `trap`
//! action, `hash -p`, `enable -f`.

use crate::core::error::ShellError;

use super::shell_tokenize;

/// Variables whose value redirects which binary, library, startup file or
/// hook runs, wherever they are assigned. `PATH` is not among them: by owner
/// decision only a segment that *starts* with `PATH=` is blocked (agents
/// prepend to `PATH` constantly, and the allowlist is no sandbox).
const BLOCKED_ENV: &[&str] = &[
    "GIT_ASKPASS",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_EDITOR",
    "GIT_EXTERNAL_DIFF",
    "GIT_PAGER",
    "SSH_ASKPASS",
    "BASH_ENV",
    "ENV",
    "PROMPT_COMMAND",
    "PS4",
    "SHELLOPTS",
    "ZDOTDIR",
    "EDITOR",
    "VISUAL",
    "PAGER",
    "NODE_OPTIONS",
    "PERL5OPT",
    "RUBYOPT",
];

/// Variable families: exported bash functions, git config injected through
/// the environment, dynamic-loader controls.
const BLOCKED_ENV_PREFIXES: &[&str] = &["BASH_FUNC_", "GIT_CONFIG", "LD_", "DYLD_"];

/// Builtins and wrappers whose arguments assign variables.
const SETTERS: &[&str] = &[
    "export", "readonly", "local", "declare", "typeset", "let", "env",
];

/// `env` options that take a separate argument.
const ENV_OPTIONS_WITH_ARG: &[&str] = &["-u", "--unset", "-C", "--chdir"];

fn is_blocked_env(name: &str) -> bool {
    BLOCKED_ENV.contains(&name) || BLOCKED_ENV_PREFIXES.iter().any(|p| name.starts_with(p))
}

/// The variable a `NAME=value` / `NAME+=value` word assigns, if it is one.
fn assigned_name(token: &str) -> Option<&str> {
    let (name, _) = token.split_once('=')?;
    let name = name.strip_suffix('+').unwrap_or(name);
    (!name.is_empty() && !name.starts_with('-')).then_some(name)
}

/// Pager/editor variables whose value is a command line: these few values
/// run nothing but a fixed, harmless program (`GIT_PAGER=cat git log`).
const COMMAND_VALUED_ENV: &[&str] = &["GIT_EDITOR", "GIT_PAGER", "EDITOR", "VISUAL", "PAGER"];
const HARMLESS_COMMAND_VALUES: &[&str] = &["", "cat", "true", ":", "less", "more"];

/// Whether assigning `entry` (`NAME=value`, or a bare `NAME` whose value is
/// unknown) is blocked.
fn is_blocked_assignment(entry: &str) -> bool {
    let (name, value) = match entry.split_once('=') {
        Some((name, value)) => (name.strip_suffix('+').unwrap_or(name), Some(value)),
        None => (entry, None),
    };
    is_blocked_env(name)
        && !(COMMAND_VALUED_ENV.contains(&name)
            && value.is_some_and(|v| HARMLESS_COMMAND_VALUES.contains(&v.trim())))
}

/// Every variable `segment` assigns before (or through) its command, as
/// `NAME=value` words or bare names: leading assignments, and the arguments
/// of `export`/`declare`/`env`/`read`/… — not only the first word, so
/// `FOO=1 PATH=… cmd` and `export PATH=…` count.
fn assigned_names(segment: &str) -> Vec<String> {
    let tokens = shell_tokenize(segment.trim());
    let mut names = Vec::new();
    // `export`/`declare`/…: every argument is a name or an assignment.
    let mut builtin_setter = false;
    // `env`: options and assignments, then the command.
    let mut env = false;
    let mut skip_next = false;
    for (i, token) in tokens.iter().enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }
        if assigned_name(token).is_some() {
            names.push(token.clone());
            continue;
        }
        if builtin_setter {
            continue;
        }
        if env && token.starts_with('-') {
            // `env -S 'PATH=… cmd'` splits its argument into words, which
            // are read like the rest of an `env` command line.
            let split = match token.as_str() {
                "-S" | "--split-string" => Some(tokens.get(i + 1).map_or("", String::as_str)),
                t if t.starts_with("--split-string=") => Some(&t["--split-string=".len()..]),
                t if t.starts_with("-S") => Some(&t[2..]),
                _ => None,
            };
            if let Some(split) = split {
                names.extend(assigned_names(&format!("env {split}")));
                break;
            }
            skip_next = ENV_OPTIONS_WITH_ARG.contains(&token.as_str());
            continue;
        }
        if env {
            break;
        }
        match token.as_str() {
            "env" => env = true,
            setter if SETTERS.contains(&setter) => builtin_setter = true,
            // `read NAME…`, `getopts SPEC NAME`, `printf -v NAME` assign the
            // variables they name.
            "read" | "getopts" => {
                names.extend(tokens[i + 1..].iter().cloned());
                break;
            }
            "printf" => {
                if let Some(at) = tokens[i + 1..].iter().position(|t| t == "-v") {
                    names.extend(tokens.get(i + 2 + at).cloned());
                }
                break;
            }
            _ => break,
        }
    }
    names
}

/// Block assignments to [`BLOCKED_ENV`] anywhere in the assignment prefix of
/// `segment` or through a variable-setting builtin.
pub(super) fn check_env_assignments(segment: &str) -> Result<(), ShellError> {
    let leading_path = segment
        .trim_start()
        .starts_with("PATH=")
        .then(|| "PATH=".to_string());
    let Some(entry) = leading_path.or_else(|| {
        assigned_names(segment)
            .into_iter()
            .find(|entry| is_blocked_assignment(entry))
    }) else {
        return Ok(());
    };
    let name = entry
        .split_once('=')
        .map_or(entry.as_str(), |(name, _)| name);
    Err(format!(
        "[BLOCKED — DO NOT RETRY] Environment override '{name}=' is blocked (it can redirect \
         which binary, library or startup code runs).\n\
         Use the env parameter instead:  ctx_shell(command=\"<your command>\", \
         env={{\"{name}\": \"<value>\"}})\n\
         Blocked variables: PATH (as the first word of a command), {}, and {}*\n\
         Other assignments (e.g. FOO=bar cmd) are allowed.",
        BLOCKED_ENV.join(", "),
        BLOCKED_ENV_PREFIXES.join("*, "),
    )
    .into())
}

/// The action of `trap ACTION SIGNAL…`, which the shell runs like `eval`.
/// `None` for `trap -p`/`-l`, a reset (`-`, empty) or a lone signal.
pub(super) fn trap_action(tokens: &[String]) -> Option<&str> {
    if tokens.first().map(String::as_str) != Some("trap") {
        return None;
    }
    let mut args = tokens[1..].iter().map(String::as_str).peekable();
    while let Some(&arg) = args.peek() {
        if arg == "--" {
            args.next();
            break;
        }
        if arg.starts_with('-') && arg.len() > 1 {
            args.next();
            continue;
        }
        break;
    }
    let rest: Vec<&str> = args.collect();
    match rest.as_slice() {
        [action, _, ..] if !action.trim().is_empty() && *action != "-" => Some(action),
        _ => None,
    }
}

/// `hash -p PATH NAME` makes `NAME` run `PATH`; `enable -f LIB NAME` loads a
/// shared object into the shell. Neither is visible as a command.
pub(super) fn check_builtin_options(tokens: &[String]) -> Result<(), ShellError> {
    let (Some(base), options) = (tokens.first(), tokens.get(1..).unwrap_or_default()) else {
        return Ok(());
    };
    let letter = match base.as_str() {
        "hash" => 'p',
        "enable" => 'f',
        _ => return Ok(()),
    };
    let uses = options
        .iter()
        .take_while(|t| t.starts_with('-') && t.as_str() != "--")
        .any(|t| !t.starts_with("--") && t[1..].contains(letter));
    if uses {
        return Err(format!(
            "[BLOCKED — DO NOT RETRY] '{base} -{letter}' makes the shell run code that is \
             not visible as a command, so it cannot be gated."
        )
        .into());
    }
    Ok(())
}
