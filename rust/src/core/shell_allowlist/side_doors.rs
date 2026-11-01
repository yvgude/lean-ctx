// SPDX-License-Identifier: Apache-2.0
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

/// The `LEAN_CTX_*` variables a command line may set: they only switch
/// compression or the hook re-entry guard for that one command. `lean-ctx -c`
/// applies these from the command text (#1321) and no others. Every other
/// `LEAN_CTX_*` variable — the shell security mode, the allowlist, the config
/// and data directories, the path jail — is lean-ctx's own policy, which a
/// command line must not set for the lean-ctx it starts.
pub(crate) const LEAN_CTX_RUN_CONTROL_ENV: &[&str] = &[
    "LEAN_CTX_DISABLED",
    "LEAN_CTX_NO_HOOK",
    "LEAN_CTX_RAW",
    "LEAN_CTX_COMPRESS",
    "LEAN_CTX_QUIET",
    "LEAN_CTX_ACTIVE",
];

/// Variables that move where lean-ctx loads its config and state from;
/// blocked for a segment that starts lean-ctx or exports them.
const LEAN_CTX_LOCATION_ENV: &[&str] = &[
    "HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
];

fn is_blocked_env(name: &str) -> bool {
    // Windows looks environment names up case-insensitively.
    let upper;
    let name = if cfg!(windows) {
        upper = name.to_ascii_uppercase();
        upper.as_str()
    } else {
        name
    };
    BLOCKED_ENV.contains(&name)
        || BLOCKED_ENV_PREFIXES.iter().any(|p| name.starts_with(p))
        || (name.starts_with("LEAN_CTX_") && !LEAN_CTX_RUN_CONTROL_ENV.contains(&name))
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

/// A variable name assembled at run time (`$v`, `${N}`, a glob): it can
/// become any name, including the ones blocked here.
fn is_dynamic_name(name: &str) -> bool {
    name.contains(['$', '`', '{', '}', '*', '?', '[', '\\'])
}

/// `read` options that take a value.
const READ_OPTIONS_WITH_ARG: &[&str] = &["-p", "-d", "-t", "-n", "-N", "-u", "-i"];

/// Whether assigning `entry` (`NAME=value`, or a bare `NAME` whose value is
/// unknown) is blocked.
fn is_blocked_assignment(entry: &str) -> bool {
    let (name, value) = match entry.split_once('=') {
        Some((name, value)) => (name.strip_suffix('+').unwrap_or(name), Some(value)),
        None => (entry, None),
    };
    is_dynamic_name(name)
        || (is_blocked_env(name)
            && !(COMMAND_VALUED_ENV.contains(&name)
                && value.is_some_and(|v| HARMLESS_COMMAND_VALUES.contains(&v.trim()))))
}

/// Every variable `segment` assigns before (or through) its command, as
/// `NAME=value` words or bare names: leading assignments, and the arguments
/// of `export`/`declare`/`env`/`read`/… — not only the first word, so
/// `FOO=1 PATH=… cmd` and `export PATH=…` count.
fn assigned_names(segment: &str) -> Vec<String> {
    let tokens = shell_tokenize(segment.trim());
    let mut names = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        if assigned_name(token).is_some() {
            names.push(token.clone());
            continue;
        }
        match token.as_str() {
            // Options, assignments and the command: parsed as the gate's
            // delegation walk parses them (clustered `-vS`, abbreviated
            // `--split-s`, `-S` contents).
            "env" => {
                names.extend(super::enforcement::env_assignment_words(&tokens[i + 1..]));
            }
            // Every argument is a name or an assignment; a bare name exports
            // or marks a value set earlier (`: ${N:=off}; export N`).
            setter if SETTERS.contains(&setter) => {
                names.extend(
                    tokens[i + 1..]
                        .iter()
                        .filter(|t| !t.starts_with(['-', '+']) || t.contains('='))
                        .cloned(),
                );
            }
            // `read NAME…`, `getopts SPEC NAME`, `printf -v NAME` assign the
            // variables they name.
            "read" => {
                let mut skip = false;
                for word in &tokens[i + 1..] {
                    if std::mem::take(&mut skip) {
                        continue;
                    }
                    if word.starts_with('-') {
                        skip = READ_OPTIONS_WITH_ARG.contains(&word.as_str());
                        continue;
                    }
                    names.push(word.clone());
                }
            }
            "getopts" => names.extend(tokens[i + 1..].iter().skip(1).cloned()),
            "printf" => {
                if let Some(at) = tokens[i + 1..].iter().position(|t| t == "-v") {
                    names.extend(tokens.get(i + 2 + at).cloned());
                }
            }
            _ => {}
        }
        break;
    }
    names
}

/// Names a `${NAME:=value}` / `${NAME=value}` expansion assigns.
fn default_assigned_names(segment: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = segment;
    while let Some(at) = rest.find("${") {
        rest = &rest[at + 2..];
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        let after = &rest[end..];
        if end > 0 && (after.starts_with(":=") || after.starts_with('=')) {
            names.push(rest[..end].to_string());
        }
    }
    names
}

/// `declare -n` / `typeset -n` make a name refer to another variable, so
/// an assignment through it reaches a variable this check never sees.
fn declares_nameref(tokens: &[String]) -> bool {
    matches!(
        tokens.first().map(String::as_str),
        Some("declare" | "typeset" | "local")
    ) && tokens[1..]
        .iter()
        .any(|t| t.starts_with('-') && !t.starts_with("--") && t[1..].contains('n'))
}

/// Whether `token` starts lean-ctx.
fn is_leanctx_program(token: &str) -> bool {
    matches!(
        token.rsplit(['/', '\\']).next(),
        Some("lean-ctx" | "lean-ctx.exe")
    )
}

/// `HOME=…` or `XDG_*_HOME=…` anywhere in a line that also starts lean-ctx
/// moves where that lean-ctx looks for its config (`HOME=/tmp/x; lean-ctx -c
/// …`). Only the release build's gate ignores those variables; this refusal
/// stays as defence in depth.
pub(super) fn check_location_with_leanctx(segments: &[String]) -> Result<(), ShellError> {
    let starts_leanctx = segments.iter().any(|seg| {
        shell_tokenize(seg.trim())
            .iter()
            .any(|t| is_leanctx_program(t))
    });
    if !starts_leanctx {
        return Ok(());
    }
    for seg in segments {
        let assigned = assigned_names(seg)
            .into_iter()
            .chain(default_assigned_names(seg));
        for entry in assigned {
            let name = entry.split_once('=').map_or(entry.as_str(), |(n, _)| n);
            let name = name.strip_suffix('+').unwrap_or(name);
            if LEAN_CTX_LOCATION_ENV
                .iter()
                .any(|v| v.eq_ignore_ascii_case(name))
            {
                return Err(format!(
                    "[BLOCKED — DO NOT RETRY] '{name}=' in a command line that starts lean-ctx \
                     would move where lean-ctx reads its own configuration. Run lean-ctx \
                     without it."
                )
                .into());
            }
        }
    }
    Ok(())
}

/// Block assignments to [`BLOCKED_ENV`] anywhere in the assignment prefix of
/// `segment` or through a variable-setting builtin.
pub(super) fn check_env_assignments(segment: &str) -> Result<(), ShellError> {
    let leading_path = segment
        .trim_start()
        .starts_with("PATH=")
        .then(|| "PATH=".to_string());
    // `HOME=/tmp/x lean-ctx -c …` would load a config the agent wrote.
    let tokens = shell_tokenize(segment.trim());
    let moves_leanctx = tokens.first().is_some_and(|t| {
        matches!(
            t.as_str(),
            "export" | "readonly" | "declare" | "typeset" | "local"
        )
    }) || tokens.iter().any(|t| {
        matches!(
            t.rsplit(['/', '\\']).next(),
            Some("lean-ctx" | "lean-ctx.exe")
        )
    });
    if declares_nameref(&tokens) {
        return Err(
            "[BLOCKED — DO NOT RETRY] `declare -n` / `typeset -n` makes a name refer \
                    to another variable, so what it assigns cannot be checked. Assign the \
                    variable by its own name."
                .into(),
        );
    }
    let defaults = default_assigned_names(segment);
    let Some(entry) = leading_path.or_else(|| {
        assigned_names(segment)
            .into_iter()
            .chain(defaults)
            .find(|entry| {
                is_blocked_assignment(entry)
                    || (moves_leanctx
                        && LEAN_CTX_LOCATION_ENV
                            .contains(&entry.split_once('=').map_or(entry.as_str(), |(n, _)| n)))
            })
    }) else {
        return Ok(());
    };
    let name = entry
        .split_once('=')
        .map_or(entry.as_str(), |(name, _)| name);
    if is_dynamic_name(name) {
        return Err(format!(
            "[BLOCKED — DO NOT RETRY] The variable name '{name}' is built at run time, so \
             the variable it sets cannot be checked. Write the name out."
        )
        .into());
    }
    Err(format!(
        "[BLOCKED — DO NOT RETRY] Environment override '{name}=' is blocked (it can redirect \
         which binary, library or startup code runs).\n\
         Use the env parameter instead:  ctx_shell(command=\"<your command>\", \
         env={{\"{name}\": \"<value>\"}})\n\
         Blocked variables: PATH (as the first word of a command), {}, {}*, \
         LEAN_CTX_* other than {}, and {} for a command that starts lean-ctx\n\
         Other assignments (e.g. FOO=bar cmd) are allowed.",
        BLOCKED_ENV.join(", "),
        BLOCKED_ENV_PREFIXES.join("*, "),
        LEAN_CTX_RUN_CONTROL_ENV.join(", "),
        LEAN_CTX_LOCATION_ENV.join(", "),
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
