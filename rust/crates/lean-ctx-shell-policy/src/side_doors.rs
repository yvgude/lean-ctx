// SPDX-License-Identifier: Apache-2.0
//! Assignments and builtins that make the shell run code the segment check
//! never sees as a command (GHSA-rxv5-4c3x-g4q6 E): an `LD_PRELOAD` or
//! `BASH_ENV` set by `export` or by a second inline assignment, a `trap`
//! action, `hash -p`, `enable -f`.

use lean_ctx::core::error::ShellError;

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
    // Git: transports that run commands (`ext::`), where git finds its
    // sub-programs and hook templates, a proxy command.
    "GIT_ALLOW_PROTOCOL",
    "GIT_EXEC_PATH",
    "GIT_TEMPLATE_DIR",
    "GIT_PROXY_COMMAND",
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
pub const LEAN_CTX_RUN_CONTROL_ENV: &[&str] = &[
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

/// Variables an explicit environment override (`ctx_shell`'s `env`, `lean-ctx
/// client exec --env`) may never set, in addition to [`BLOCKED_ENV`] and its
/// families: an override is not a command line, so nothing narrows these to
/// "first word" or "starts lean-ctx" — the child simply runs with them.
const BLOCKED_OVERRIDE_ENV: &[&str] = &[
    // Binary resolution: the child looks its program up in its own PATH.
    "PATH",
    "GIT_EXEC_PATH",
    "GIT_PROXY_COMMAND",
    "GIT_TEMPLATE_DIR",
    // Git transports that run a command (`ext::`).
    "GIT_ALLOW_PROTOCOL",
    // Shell startup and parsing.
    "SHELL",
    "IFS",
    "CDPATH",
    // Identity and home: they relocate every config, a nested lean-ctx's too.
    "HOME",
    "USER",
    "LOGNAME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
    // Interpreter search paths and startup hooks.
    "PYTHONPATH",
    "PYTHONSTARTUP",
    "PYTHONHOME",
    "NODE_PATH",
    "RUBYLIB",
    "GEM_PATH",
    "GEM_HOME",
    "PERL5LIB",
    "CLASSPATH",
    "JAVA_HOME",
    "JAVA_TOOL_OPTIONS",
    "_JAVA_OPTIONS",
    "JDK_JAVA_OPTIONS",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "GOPATH",
    "GOROOT",
];

/// Override families: npm configuration (`npm_config_node_options`,
/// `npm_config_script_shell`), lean-ctx's own configuration (`LCTX_*`, e.g.
/// `LCTX_ALLOW_PATH`), and every `LEAN_CTX_*` — the run-control exemption of
/// a command line does not apply to an override.
const BLOCKED_OVERRIDE_PREFIXES: &[&str] = &["NPM_CONFIG_", "LCTX_", "LEAN_CTX_"];

/// Whether an explicit environment override `name=value` is refused: the
/// ONE policy of `ctx_shell`'s `env` and `lean-ctx client exec --env`.
/// Everything the command-line gate blocks wherever it is assigned
/// (`BLOCKED_ENV`, `BASH_FUNC_*`, `GIT_CONFIG*`, `LD_*`, `DYLD_*`), plus
/// `BLOCKED_OVERRIDE_ENV` and `BLOCKED_OVERRIDE_PREFIXES`; names compare
/// case-insensitively. A pager/editor variable keeps the gate's harmless
/// values (`GIT_PAGER=cat`).
pub fn env_override_refused(name: &str, value: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    let upper = upper.as_str();
    if upper.is_empty() || is_dynamic_name(upper) || upper.contains(['=', '\0']) {
        return true;
    }
    if COMMAND_VALUED_ENV.contains(&upper) {
        return !HARMLESS_COMMAND_VALUES.contains(&value.trim());
    }
    BLOCKED_ENV.contains(&upper)
        || BLOCKED_ENV_PREFIXES.iter().any(|p| upper.starts_with(p))
        || BLOCKED_OVERRIDE_ENV.contains(&upper)
        || BLOCKED_OVERRIDE_PREFIXES
            .iter()
            .any(|p| upper.starts_with(p))
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
/// become any name, including the ones blocked here. A leading `~` is tilde
/// expansion (`$HOME`, `$PWD`, `$OLDPWD`, the directory stack): `printf ~-`
/// with `OLDPWD=-vNAME` is `printf -vNAME`.
fn is_dynamic_name(name: &str) -> bool {
    name.starts_with('~') || name.contains(['$', '`', '{', '}', '*', '?', '[', '\\'])
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

/// The name a command word runs (see [`super::tokenizer::exec_name`]).
fn program_name(token: &str) -> String {
    super::tokenizer::exec_name(token)
}

/// Every variable `segment` assigns before (or through) its command, as
/// `NAME=value` words or bare names: leading assignments, and the arguments
/// of `export`/`declare`/`env`/`sudo`/`read`/… — not only the first word, so
/// `FOO=1 PATH=… cmd` and `export PATH=…` count. The program is matched by
/// its name, whatever path runs it (`/usr/bin/env X=… cmd`); wrappers in
/// front of it (`command`, `nice`, `timeout`, …) are unwrapped by the
/// delegation walk, which checks each target like a segment of its own.
fn assigned_names(segment: &str) -> Vec<String> {
    let tokens = shell_tokenize(segment.trim());
    let mut names = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        if assigned_name(token).is_some() {
            names.push(token.clone());
            continue;
        }
        match program_name(token).as_str() {
            // Options, assignments and the command: parsed as the gate's
            // delegation walk parses them (clustered `-vS`, abbreviated
            // `--split-s`, `-S` contents). `sudo NAME=value cmd` and
            // `strace -E NAME=value cmd` set the variable for `cmd` exactly
            // like `env` does.
            wrapper @ ("env" | "sudo" | "strace" | "arch") => {
                names.extend(super::enforcement::wrapper_assignment_words(
                    wrapper,
                    &tokens[i + 1..],
                ));
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
            // Read from the raw words: bash parses printf's options after
            // expansion, so `o=-v; printf $o NAME …` assigns NAME, and an
            // option word that may split hides the name (refused as built
            // at run time).
            "printf" => {
                use super::operand_eval::{PrintfTarget, literal_text, printf_target, raw_words};
                let raw = raw_words(segment.trim());
                if let Some(at) = raw
                    .iter()
                    .position(|w| program_name(&literal_text(w)) == "printf")
                {
                    match printf_target(&raw[at + 1..]) {
                        PrintfTarget::Variable(name) | PrintfTarget::Unknown(name) => {
                            names.push(name);
                        }
                        PrintfTarget::NoVariable => {}
                    }
                }
            }
            // `wait -p NAME` (bash 5.1) assigns NAME, evaluating a subscript
            // in it like `read`.
            "wait" => {
                let mut args = tokens[i + 1..].iter();
                while let Some(arg) = args.next() {
                    // A glob where an option may stand (`-?BASH_ENV`,
                    // `?pNAME`) may match a file `-pNAME`, and brace
                    // expansion makes `{-pNAME,}` / `-{p,}NAME` into
                    // `-pNAME`: a name built at run time. `${…}` is an
                    // expansion, not a brace. A leading `~` expands to
                    // `$HOME`, `$PWD` or `$OLDPWD`, which the line may set
                    // to `-pNAME`.
                    let brace = arg
                        .match_indices('{')
                        .any(|(at, _)| !arg[..at].ends_with('$'));
                    if brace
                        || arg.starts_with('~')
                        || (arg.starts_with(['-', '*', '?', '[']) && arg.contains(['*', '?', '[']))
                    {
                        names.push(arg.clone());
                        break;
                    }
                    let Some(cluster) = arg.strip_prefix('-').filter(|c| !c.is_empty()) else {
                        break;
                    };
                    if cluster == "-" {
                        break;
                    }
                    if let Some(at) = cluster.find('p') {
                        let attached = &cluster[at + 1..];
                        if attached.is_empty() {
                            names.extend(args.next().cloned());
                        } else {
                            names.push(attached.to_string());
                        }
                        break;
                    }
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

/// `declare -i` evaluates every later assignment arithmetically, and `-a`/`-A`
/// re-parse a quoted word as a compound assignment: both run a `$(…)` that
/// sits inside single quotes, where no scanner looks.
fn declares_evaluating_attribute(tokens: &[String]) -> bool {
    matches!(
        tokens.first().map(String::as_str),
        Some("declare" | "typeset" | "local" | "readonly")
    ) && tokens[1..].iter().any(|t| {
        t.starts_with('-')
            && !t.starts_with("--")
            && t[1..].chars().any(|c| matches!(c, 'i' | 'a' | 'A'))
    })
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
    if declares_evaluating_attribute(&tokens) {
        return Err(
            "[BLOCKED — DO NOT RETRY] `declare/local/typeset/readonly -i`, `-a` and `-A` \
                    evaluate what they assign (arithmetic subscripts, compound arrays), so it \
                    cannot be checked. Assign the variable plainly, without these attributes."
                .into(),
        );
    }
    if let Some(operand) = super::tokenizer::refused_builtin_operand(segment) {
        return Err(format!(
            "[BLOCKED — DO NOT RETRY] `{operand}` is not a plain variable name, assignment \
             or umask mode, and bash evaluates some of these (array subscripts, compound \
             arrays) or assigns integer-attributed variables (SECONDS, RANDOM, OPTIND, …) \
             arithmetically, so it cannot be checked. Values must be a bare word, a \
             single-quoted string or a double-quoted string with only plain $NAME \
             expansions. Use a plain NAME=value."
        )
        .into());
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
             the variable it sets cannot be checked. Write the name out. A printf \
             format held in a variable needs `--` before it: `printf -- \"$fmt\" …`."
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
    // #34: `jobs -x CMD` runs CMD; `compgen`/`complete -C CMD` run a command
    // and `-F FUNC` a function to produce completions.
    let letters: &[char] = match base.as_str() {
        "hash" => &['p'],
        "enable" => &['f'],
        "jobs" => &['x'],
        "compgen" | "complete" => &['C', 'F'],
        _ => return Ok(()),
    };
    // `compgen`/`complete` options may follow a word-list value, so every
    // option-shaped word counts there.
    let anywhere = matches!(base.as_str(), "compgen" | "complete");
    let used = options
        .iter()
        .take_while(|t| anywhere || (t.starts_with('-') && t.as_str() != "--"))
        .filter(|t| t.starts_with('-') && !t.starts_with("--"))
        .find_map(|t| t[1..].chars().find(|c| letters.contains(c)));
    if let Some(letter) = used {
        return Err(format!(
            "[BLOCKED — DO NOT RETRY] '{base} -{letter}' makes the shell run code that is \
             not visible as a command, so it cannot be gated."
        )
        .into());
    }
    if super::operand_eval::wordlist_expands(tokens) {
        return Err(format!(
            "[BLOCKED — DO NOT RETRY] '{base} -W' expands its word list once more, so an \
             expansion in it runs although it is quoted, and it cannot be gated. Give the \
             words literally."
        )
        .into());
    }
    Ok(())
}

/// Git configuration sections in which a key runs, or points git at, code:
/// every key of them counts (`alias.*`, `filter.*.smudge`, `guitool.*.cmd`,
/// `sendemail.*cmd`, `imap.tunnel`, `interactive.diffFilter`, …).
const GIT_COMMAND_SECTIONS: &[&str] = &[
    "alias",
    "pager",
    "include",
    "includeif",
    "filter",
    "credential",
    "gpg",
    "mergetool",
    "difftool",
    "guitool",
    "browser",
    "man",
    "instaweb",
    "sendemail",
    "imap",
    "interactive",
    "trailer",
    "uploadpack",
    "protocol",
    // `safe.directory=*` trusts every repository, hooks and config included.
    "safe",
    // `url.<base>.insteadOf` rewrites any URL, to `ext::…` too.
    "url",
];

/// Key names (the last part) whose value git executes or loads code from,
/// in any section.
const GIT_COMMAND_NAMES: &[&str] = &[
    "pager",
    "editor",
    "askpass",
    "fsmonitor",
    "hookspath",
    "gitproxy",
    "program",
    "helper",
    "textconv",
    "driver",
    "clean",
    "smudge",
    "process",
    "tunnel",
    "difffilter",
    "httpd",
    "external",
    "templatedir",
    "uploadpack",
    "receivepack",
    "browser",
    "smtpserver",
    "update",
    "attributesfile",
];

/// Whether setting git configuration `key` can make git run a command: a
/// key of a [`GIT_COMMAND_SECTIONS`] section, a [`GIT_COMMAND_NAMES`] name,
/// or any name ending in `cmd` / `command` (`core.sshCommand`,
/// `sendemail.toCmd`, `core.alternateRefsCommand`) — deny by the shape of
/// the key, not by a closed list. Compared case-insensitively, like git.
fn is_git_command_key(key: &str) -> bool {
    let key = key.trim().to_ascii_lowercase();
    let section = key.split('.').next().unwrap_or_default();
    let name = key.rsplit('.').next().unwrap_or_default();
    GIT_COMMAND_SECTIONS.contains(&section)
        || GIT_COMMAND_NAMES.contains(&name)
        || name.ends_with("cmd")
        || name.ends_with("command")
}

fn git_command_key_refusal(key: &str, how: &str) -> ShellError {
    format!(
        "[BLOCKED — DO NOT RETRY] 'git' {how} '{key}' is blocked: its value is a command git \
         would run (like GIT_SSH_COMMAND or GIT_CONFIG_*), which the shell allowlist cannot \
         check. Run git without it; such settings are changed by the person, outside the \
         agent.\n\
         This is a permanent security restriction."
    )
    .into()
}

/// Git's global options (`git --help`, git 2.4x) that take a value, as the
/// next word or attached with `=`.
const GIT_GLOBAL_VALUE_OPTIONS: &[&str] = &[
    "-c",
    "-C",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--config-env",
    "--super-prefix",
    "--attr-source",
];

/// Git's global options without a value.
const GIT_GLOBAL_FLAGS: &[&str] = &[
    "-v",
    "--version",
    "-h",
    "--help",
    "-p",
    "--paginate",
    "-P",
    "--no-pager",
    "--bare",
    "--no-replace-objects",
    "--no-lazy-fetch",
    "--no-optional-locks",
    "--no-advice",
    "--literal-pathspecs",
    "--glob-pathspecs",
    "--noglob-pathspecs",
    "--icase-pathspecs",
    "--html-path",
    "--man-path",
    "--info-path",
    "--exec-path",
];

/// Global options with an attached value only (`--list-cmds=…`).
const GIT_GLOBAL_ATTACHED_ONLY: &[&str] = &["--list-cmds"];

/// The index of git's subcommand in `tokens` (which start at the `git`
/// word): the first word after git's global options, parsed with git's own
/// option table. `Ok(None)`: no subcommand (`git --version`). An option the
/// table does not know is refused: what follows it cannot be told apart
/// from the subcommand, so no position is guessed.
fn git_subcommand(tokens: &[String]) -> Result<Option<usize>, ShellError> {
    let mut i = 1;
    while let Some(word) = tokens.get(i) {
        if !word.starts_with('-') {
            return Ok(Some(i));
        }
        if word == "--" {
            return Ok(tokens.get(i + 1).map(|_| i + 1));
        }
        let name = word.split_once('=').map_or(word.as_str(), |(name, _)| name);
        let attached = word.contains('=');
        if GIT_GLOBAL_VALUE_OPTIONS.contains(&name) {
            i += if attached { 1 } else { 2 };
        } else if GIT_GLOBAL_FLAGS.contains(&name) && !attached
            || GIT_GLOBAL_ATTACHED_ONLY.contains(&name) && attached
        {
            i += 1;
        } else if name == "--exec-path" {
            return Err(git_option_refusal(word));
        } else {
            return Err(format!(
                "[BLOCKED — DO NOT RETRY] 'git' global option '{word}' is not one the gate \
                 knows, so it cannot tell which git command runs. Write the git command \
                 without it.\nThis is a permanent security restriction."
            )
            .into());
        }
    }
    Ok(None)
}

fn git_option_refusal(option: &str) -> ShellError {
    format!(
        "[BLOCKED — DO NOT RETRY] 'git' option '{option}' is blocked: it makes git run a \
         program or code the shell allowlist cannot check.\n\
         This is a permanent security restriction."
    )
    .into()
}

/// Options of git subcommands whose value is a program git runs, a pager,
/// a directory it takes code from, or a server it starts: `(subcommand,
/// long options, short options)`.
const GIT_RUNNER_OPTIONS: &[(&str, &[&str], &[&str])] = &[
    ("clone", &["--upload-pack", "--template"], &["-u"]),
    ("ls-remote", &["--upload-pack"], &["-u"]),
    ("fetch", &["--upload-pack"], &[]),
    ("pull", &["--upload-pack"], &[]),
    ("remote", &["--upload-pack"], &[]),
    ("push", &["--exec", "--receive-pack"], &[]),
    ("send-pack", &["--exec", "--receive-pack"], &[]),
    ("archive", &["--exec"], &[]),
    ("init", &["--template"], &[]),
    ("grep", &["--open-files-in-pager"], &["-O"]),
    (
        "send-email",
        &[
            "--to-cmd",
            "--cc-cmd",
            "--header-cmd",
            "--sendmail-cmd",
            "--smtp-server",
        ],
        &[],
    ),
    ("instaweb", &["--httpd", "--browser"], &["-d", "-b"]),
];

/// Whether `word` (`--name`, `--name=value`) spells the long option `long`
/// the way git's option parser accepts it: in full or abbreviated to any
/// prefix (`--upload-p`, `--exe=`), also negated (`--no-…`). Refusing
/// every prefix fails closed where git would call it ambiguous.
fn spells_long(word: &str, long: &str) -> bool {
    let Some(name) = word.strip_prefix("--") else {
        return false;
    };
    let name = name.split_once('=').map_or(name, |(name, _)| name);
    let long = long.trim_start_matches('-');
    let negated = name.strip_prefix("no-").unwrap_or(name);
    !name.is_empty()
        && (long.starts_with(name) || (!negated.is_empty() && long.starts_with(negated)))
}

/// Whether the short-option word `word` holds the letter of `short` (`-u`)
/// anywhere in its packed group (`-qu`, `-iOless`): git reads every letter
/// of a group, and a letter taking a value takes the rest of the word.
fn packs_short(word: &str, short: &str) -> bool {
    let Some(letter) = short.strip_prefix('-').and_then(|s| s.chars().next()) else {
        return false;
    };
    word.len() > 1 && word.starts_with('-') && !word.starts_with("--") && word[1..].contains(letter)
}

/// Whether `word` names the `ext::` transport (`ext::sh -c …` runs a
/// command): `ext::` where it starts the word or follows a non-alphanumeric
/// character (`git+ext::…`, `--upload-pack=x ext::…`, `url.ext::…`), not
/// inside an identifier a search names (`context::`, `next::`).
fn names_ext_transport(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    lower.match_indices("ext::").any(|(at, _)| {
        lower[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_ascii_alphanumeric())
    })
}

/// An option of a git subcommand that runs code (`clone -u CMD`, `push
/// --exec=CMD`, `grep -O`, `send-email --to-cmd`, `--template=DIR`, …) in
/// any spelling git accepts (abbreviated, packed), `--upload-pack` /
/// `--receive-pack` after any subcommand, or a URL of the `ext::` transport
/// (`ext::sh -c …` runs a command).
fn check_git_runner_options(tokens: &[String], sub: Option<usize>) -> Result<(), ShellError> {
    if let Some(url) = tokens.iter().skip(1).find(|t| names_ext_transport(t)) {
        return Err(git_option_refusal(url));
    }
    let Some(sub) = sub else { return Ok(()) };
    let (longs, shorts): (&[&str], &[&str]) = GIT_RUNNER_OPTIONS
        .iter()
        .find(|(name, _, _)| *name == tokens[sub])
        .map_or((&[], &[]), |(_, longs, shorts)| (*longs, *shorts));
    for word in &tokens[sub + 1..] {
        if word == "--" {
            break;
        }
        let runs = longs
            .iter()
            .chain(&["--upload-pack", "--receive-pack"])
            .any(|long| spells_long(word, long))
            || shorts.iter().any(|short| packs_short(word, short));
        if runs {
            return Err(git_option_refusal(word));
        }
    }
    Ok(())
}

/// `git config` options that take a value as the next word.
const GIT_CONFIG_VALUE_OPTIONS: &[&str] = &[
    "-f",
    "--file",
    "--blob",
    "-t",
    "--type",
    "--default",
    "--comment",
    "--value",
];

/// `git config` subcommands that only read, delete or open an editor.
const GIT_CONFIG_NON_WRITES: &[&str] = &[
    "get",
    "get-all",
    "get-regexp",
    "get-urlmatch",
    "get-color",
    "get-colorbool",
    "list",
    "unset",
    "unset-all",
    "remove-section",
    "edit",
];

/// The key a `git config` invocation writes (`git config KEY VALUE`, `git
/// config set KEY VALUE`, `--add`, `--replace-all`), or the new name of a
/// renamed section. `args` follow the `config` word.
fn git_config_written_key(args: &[String]) -> Option<&str> {
    let mut positional: Vec<&str> = Vec::new();
    let mut flags: Vec<&str> = Vec::new();
    let mut words = args.iter().map(String::as_str);
    while let Some(word) = words.next() {
        if word == "--" {
            positional.extend(words.by_ref());
            break;
        }
        if word.starts_with('-') && word.len() > 1 {
            let takes_value = GIT_CONFIG_VALUE_OPTIONS.iter().any(|option| {
                if option.starts_with("--") {
                    spells_long(word, option) && !word.contains('=') && !word.starts_with("--no-")
                } else {
                    word == *option
                }
            });
            if takes_value {
                words.next();
            } else {
                flags.push(word.split('=').next().unwrap_or(word));
            }
            continue;
        }
        positional.push(word);
    }
    if positional.first() == Some(&"rename-section") || flags.contains(&"--rename-section") {
        return positional.last().copied();
    }
    let reads = flags.iter().any(|f| {
        f.starts_with("--get")
            || f.starts_with("--unset")
            || matches!(*f, "--list" | "-l" | "--edit" | "-e" | "--remove-section")
    });
    match positional.first() {
        Some(&"set") => positional.get(1).copied(),
        Some(sub) if GIT_CONFIG_NON_WRITES.contains(sub) => None,
        Some(key) if !reads && positional.len() >= 2 => Some(key),
        _ => None,
    }
}

/// The value of a `-c` / `--config` / `--config-env` setting at
/// `tokens[*i]` in any spelling git accepts (abbreviated `--conf=…`, packed
/// `-qc …`, attached `-ckey=…`); advances `*i` past a separate value.
fn config_setting<'a>(tokens: &'a [String], i: &mut usize) -> Option<&'a str> {
    let word = tokens[*i].as_str();
    let separate = |i: &mut usize| {
        *i += 1;
        tokens.get(*i).map(String::as_str)
    };
    if word.starts_with("--") && !word.starts_with("--no-") {
        if !(spells_long(word, "--config") || spells_long(word, "--config-env")) {
            return None;
        }
        return match word.split_once('=') {
            Some((_, value)) => Some(value),
            None => separate(i),
        };
    }
    if !packs_short(word, "-c") {
        return None;
    }
    let rest = &word[word.find('c').map_or(word.len(), |at| at + 1)..];
    if rest.is_empty() {
        separate(i)
    } else {
        Some(rest)
    }
}

/// A configuration a git command line sets for this one run (`git -c
/// key=value`, `-ckey=value`, `--config-env=key=VAR`, `clone --config
/// key=value`) or writes for good (`git config key value`, `git config set
/// …`), whose value is a command git would run. The environment forms
/// (`GIT_CONFIG_*`, `GIT_SSH_COMMAND`) are blocked as assignments; these are
/// the same settings spelt as arguments. `tokens` starts at the `git` word.
pub(super) fn check_git_config_arguments(tokens: &[String]) -> Result<(), ShellError> {
    let mut i = 1;
    while i < tokens.len() {
        let setting = config_setting(tokens, &mut i);
        i += 1;
        let Some(setting) = setting else { continue };
        let key = setting.split_once('=').map_or(setting, |(key, _)| key);
        if is_git_command_key(key) {
            return Err(git_command_key_refusal(key, "setting"));
        }
    }
    let sub = git_subcommand(tokens)?;
    check_git_runner_options(tokens, sub)?;
    if let Some(sub) = sub
        && tokens[sub] == "config"
    {
        let args = &tokens[sub + 1..];
        // An abbreviated `--re…` may be `--rename-section`: its new name
        // is checked as a key too.
        let renamed = args
            .iter()
            .any(|a| spells_long(a, "--rename-section"))
            .then(|| args.last().map(String::as_str))
            .flatten();
        for key in git_config_written_key(args).into_iter().chain(renamed) {
            if is_git_command_key(key) {
                return Err(git_command_key_refusal(key, "config write of"));
            }
        }
    }
    Ok(())
}

/// `filter-branch` options whose value is a shell script.
const GIT_FILTER_OPTIONS: &[&str] = &[
    "--env-filter",
    "--tree-filter",
    "--index-filter",
    "--parent-filter",
    "--msg-filter",
    "--commit-filter",
    "--tag-name-filter",
    "--setup",
];

/// The value of option `long` (`--opt v`, `--opt=v`, abbreviated `--op=v`)
/// or of its short letter anywhere in a packed group (`-x v`, `-xv`,
/// `-ix v`) at `words[i]`.
fn option_value(words: &[String], i: usize, long: &str, short: Option<&str>) -> Option<String> {
    let word = words[i].as_str();
    if word.starts_with("--") {
        if word.starts_with("--no-") || !spells_long(word, long) {
            return None;
        }
        return match word.split_once('=') {
            Some((_, value)) => Some(value.to_owned()),
            None => words.get(i + 1).cloned(),
        };
    }
    let short = short?;
    if !packs_short(word, short) {
        return None;
    }
    let letter = short.trim_start_matches('-');
    let at = word[1..].find(letter)? + 1 + letter.len();
    match &word[at..] {
        "" => words.get(i + 1).cloned(),
        rest => Some(rest.to_owned()),
    }
}

/// The commands a git subcommand runs that the command line names as text
/// (`submodule foreach CMD`, `rebase --exec CMD`, `bisect run CMD…`,
/// `difftool --extcmd CMD`, `filter-branch --tree-filter CMD`): each runs
/// like a command line of its own and is gated like one. `tokens` starts at
/// the `git` word.
pub(super) fn git_embedded_commands(tokens: &[String]) -> Vec<String> {
    // An unparsable global option is refused by `check_git_config_arguments`.
    let Ok(Some(sub)) = git_subcommand(tokens) else {
        return Vec::new();
    };
    let args = &tokens[sub + 1..];
    let mut commands = Vec::new();
    let options: &[(&str, Option<&str>)] = match tokens[sub].as_str() {
        "submodule" => {
            if let Some(at) = args.iter().position(|a| a == "foreach") {
                let script: Vec<&str> = args[at + 1..]
                    .iter()
                    .map(String::as_str)
                    .skip_while(|a| a.starts_with('-'))
                    .collect();
                if !script.is_empty() {
                    commands.push(script.join(" "));
                }
            }
            return commands;
        }
        "bisect" => {
            if args.first().is_some_and(|a| a == "run") && args.len() > 1 {
                commands.push(lean_ctx::shell::join_command(&args[1..]));
            }
            return commands;
        }
        "rebase" => &[("--exec", Some("-x"))],
        "difftool" => &[("--extcmd", Some("-x"))],
        "filter-branch" => &[],
        _ => return commands,
    };
    let filters = tokens[sub] == "filter-branch";
    for i in 0..args.len() {
        for (long, short) in options {
            commands.extend(option_value(args, i, long, *short));
        }
        if filters {
            for long in GIT_FILTER_OPTIONS {
                commands.extend(option_value(args, i, long, None));
            }
        }
    }
    commands
}

/// `watch` options that take a value.
const WATCH_VALUE_OPTIONS: &[&str] = &["-n", "--interval", "-q", "--equexit"];
/// `script` options that take a value (util-linux and BSD).
const SCRIPT_VALUE_OPTIONS: &[&str] = &[
    "-E",
    "--echo",
    "-I",
    "--log-in",
    "-O",
    "--log-out",
    "-B",
    "--log-io",
    "-T",
    "--log-timing",
    "-m",
    "--logging-format",
    "-t",
    "-F",
];

/// The command a program runs that its arguments spell as text: `watch
/// CMD…` (joined and run by `sh -c`, or exec'd with `-x`), `script -c CMD`
/// (util-linux) and `script FILE CMD…` (BSD, macOS). Each is gated like a
/// command line of its own. `tokens` start at the program word.
pub(super) fn inline_scripts(tokens: &[String]) -> Vec<String> {
    let Some(program) = tokens.first() else {
        return Vec::new();
    };
    let script = match program_name(program).as_str() {
        "watch" => false,
        "script" => true,
        _ => return Vec::new(),
    };
    let value_options = if script {
        SCRIPT_VALUE_OPTIONS
    } else {
        WATCH_VALUE_OPTIONS
    };
    let mut positional: Vec<String> = Vec::new();
    let mut exec = false;
    let mut words = tokens[1..].iter();
    while let Some(word) = words.next() {
        if word == "--" {
            positional.extend(words.by_ref().cloned());
            break;
        }
        // `watch` stops at its first operand: the rest is the command.
        if !script && !positional.is_empty() {
            positional.push(word.clone());
            positional.extend(words.by_ref().cloned());
            break;
        }
        if !word.starts_with('-') || word == "-" {
            positional.push(word.clone());
            // BSD `script FILE CMD…`: after the file comes the command.
            if script && positional.len() == 2 {
                positional.extend(words.by_ref().cloned());
                break;
            }
            continue;
        }
        let option = word.split_once('=').map_or(word.as_str(), |(name, _)| name);
        if script && (option == "-c" || option == "--command") {
            let value = match word.split_once('=') {
                Some((_, value)) => Some(value.to_owned()),
                None => words.next().cloned(),
            };
            return value.into_iter().collect();
        }
        if script && word.starts_with("-c") && !word.starts_with("--") {
            return vec![word[2..].to_owned()];
        }
        if !script && (option == "-x" || option == "--exec") {
            exec = true;
        }
        if value_options.contains(&option) && !word.contains('=') {
            words.next();
        }
    }
    let command = if script {
        // `script FILE CMD…`: the first operand is the typescript file.
        positional.get(1..).unwrap_or_default().to_vec()
    } else {
        positional
    };
    if command.is_empty() {
        return Vec::new();
    }
    vec![if exec || script {
        lean_ctx::shell::join_command(&command)
    } else {
        command.join(" ")
    }]
}
