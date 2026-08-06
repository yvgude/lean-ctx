use crate::core::error::ShellError;

use super::{
    ShellSecurity, allowlist_block_message, check_substitution_in_args, effective_allowlist,
    expand_to_leaf_segments, extract_all_commands, extract_base_from_segment, shell_tokenize,
    skip_env_assignments, split_on_operators, strip_all_heredoc_bodies, strip_comments,
    strip_quoted_heredoc_bodies,
};

/// Checks whether a command may run, honouring the active [`ShellSecurity`] mode
/// (GL #788). This is the single chokepoint shared by MCP `ctx_shell` and the
/// CLI shell entrypoints, so the mode applies consistently:
///
/// - [`ShellSecurity::Off`] → always `Ok` (gating skipped; compression intact).
/// - [`ShellSecurity::Warn`] → run the checks, log any violation, return `Ok`.
/// - [`ShellSecurity::Enforce`] → block on violation (the secure default).
pub fn check_shell_allowlist(command: &str) -> Result<(), ShellError> {
    match ShellSecurity::resolve() {
        ShellSecurity::Off => Ok(()),
        ShellSecurity::Warn => {
            if let Err(msg) = enforce_shell_allowlist(command) {
                tracing::warn!(
                    target: "shell_security",
                    "warn-only: would block ({})",
                    msg.lines().next().unwrap_or("blocked")
                );
            }
            Ok(())
        }
        ShellSecurity::Enforce => enforce_shell_allowlist(command),
    }
}

/// True when `command` would pass the allowlist / dangerous-pattern checks in
/// `enforce` semantics — independent of the active [`ShellSecurity`] mode and
/// without any logging or blocking side effects.
///
/// The PreToolUse hook uses this to decide whether a compound/pipeline is safe
/// to route through the compressing `lean-ctx -c` wrap: only gate-clean compounds
/// are wrapped, so a pipeline whose sink is an interpreter-eval or a
/// non-allowlisted tool is never *newly* blocked by the rewrite (#589). It is
/// mode-independent on purpose: a data-sink pipeline should stay raw (left to the
/// agent shell) even in `off`/`warn` mode, where compressing its output would be
/// just as wrong as blocking it would be in `enforce`.
#[must_use]
pub fn passes_enforced(command: &str) -> bool {
    enforce_shell_allowlist(command).is_ok()
}

/// Allowlist + dangerous-pattern enforcement, evaluated as if in `enforce` mode.
/// [`check_shell_allowlist`] decides whether a violation blocks, warns, or is
/// skipped based on the active [`ShellSecurity`] mode.
///
/// When the allowlist is empty, all commands pass (blocklist-only mode).
/// When non-empty, EVERY command segment in the pipeline must match.
pub(super) fn enforce_shell_allowlist(command: &str) -> Result<(), ShellError> {
    let normalized = normalize_line_continuations(command);
    // #876: a quoted-delimiter heredoc body (`<<'EOF' … EOF`) is literal stdin
    // data, not commands. Strip it before analysis so the operator-splitter can't
    // dice a commit message (`feat(...)`) into bogus "segments" and block them.
    // #876: quoted-delimiter heredoc body = literal stdin, not commands.
    // Substitution checks ($(), backticks) need the quoted-only strip so they
    // can still flag expanding substitutions in unquoted bodies.
    // #1109: a shell comment (`# …`) is not a command. Strip comments after the
    // heredoc bodies are gone, so a `#` inside a (now-removed) body can't be
    // mistaken for a comment and, conversely, a real comment line between
    // commands isn't diced into a bogus `#` segment and blocked.
    let quoted_stripped = strip_comments(&strip_quoted_heredoc_bodies(&normalized));
    // #931: for command-segment and redirect checks, strip ALL heredoc bodies
    // (quoted + unquoted) — a `>` or command word in any body is opaque data.
    let all_stripped = strip_comments(&strip_all_heredoc_bodies(&normalized));
    let cmd = quoted_stripped.as_str();
    let cmd_all = all_stripped.as_str();

    if has_dangerous_patterns(cmd) {
        return Err(format!(
            "[BLOCKED — DO NOT RETRY] Command uses eval or $()/ backticks at command position, \
             which is blocked regardless of allowlist. \
             This is a permanent security restriction, not a transient error.\n\
             Command: {command}"
        )
        .into());
    }

    let strict = crate::core::config::Config::load().shell_strict_mode;
    check_substitution_in_args(cmd, strict)?;
    check_pipe_to_bare_interpreter(cmd, strict)?;

    let allowlist = effective_allowlist();
    if allowlist.is_empty() {
        check_unconditional_blocked_only(cmd_all)?;
        return Ok(());
    }
    check_all_segments(cmd_all, &allowlist)
}

/// Normalize the command string: remove backslash-newline continuations and
/// replace Unicode line separators (U+2028, U+2029) with newlines.
pub(super) fn normalize_line_continuations(command: &str) -> String {
    command
        .replace("\\\r\n", "")
        .replace("\\\n", "")
        .replace(['\u{2028}', '\u{2029}'], "\n")
}

/// Piping into a bare interpreter (no script file): warn by default, **block**
/// when `shell_strict_mode = true` (GH #391).
pub(super) fn check_pipe_to_bare_interpreter(
    command: &str,
    strict: bool,
) -> Result<(), ShellError> {
    let segments = split_on_operators(command);

    for (idx, seg) in segments.iter().enumerate() {
        if idx == 0 {
            continue;
        }
        if is_bare_interpreter_stdin(seg) {
            let base = extract_base_from_segment(seg);
            if strict {
                tracing::warn!(
                    "[SECURITY] Pipe to bare interpreter '{base}' blocked (shell_strict_mode=true)"
                );
                return Err(format!(
                    "[BLOCKED — DO NOT RETRY] Piping into bare interpreter '{base}' is blocked \
                     because shell_strict_mode = true. Run a script file instead.\n\
                     Command: {command}"
                )
                .into());
            }
            tracing::warn!("[SECURITY] Pipe to bare interpreter '{base}' detected (warn-only)");
        }
    }
    Ok(())
}

/// For empty allowlists: still enforce UNCONDITIONAL_BLOCKED commands.
pub(super) fn check_unconditional_blocked_only(command: &str) -> Result<(), ShellError> {
    let segments = extract_all_commands(command);
    for seg in &segments {
        let base = extract_base_from_segment(seg);
        if !base.is_empty() && UNCONDITIONAL_BLOCKED.contains(&base.as_str()) {
            return Err(format!(
                "[BLOCKED — DO NOT RETRY] '{base}' is unconditionally blocked \
                 regardless of allowlist configuration.\n\
                 Command: {command}"
            )
            .into());
        }
        check_inline_env_block(seg)?;
        check_interpreter_eval_only(seg)?;
        check_dangerous_flags(seg)?;
    }
    Ok(())
}

/// Tokenize a shell command segment respecting single/double quotes and backslash escapes.
/// Returns tokens with outer quotes stripped, matching how the shell would parse them.
/// E.g. `git -C "Program Files" status` → `["git", "-C", "Program Files", "status"]`
/// Like `check_interpreter_abuse` but only checks for eval flags on interpreters.
/// Skips allowlist-membership tests (no allowlist exists in blocklist-only mode),
/// but still follows delegation wrappers so `xargs bash -c …` / `timeout 5 sh -c …`
/// cannot smuggle inline code past the check (GH #391).
fn check_interpreter_eval_only(segment: &str) -> Result<(), ShellError> {
    let inline_ok = crate::core::config::Config::load().shell_allow_inline_scripts_effective();
    check_interpreter_inner(segment, None, 0, inline_ok)
}

/// #823: unified interpreter-abuse walk. Both eval-only (empty allowlist) and
/// restricted (non-empty allowlist) modes share this single recursive check.
/// `allowlist`: None = blocklist-only mode, Some = restricted mode with delegation gating.
/// `inline_ok`: if true, skip eval-flag/heredoc checks (#814 opt-in).
fn check_interpreter_inner(
    segment: &str,
    allowlist: Option<&[String]>,
    depth: usize,
    inline_ok: bool,
) -> Result<(), ShellError> {
    if depth > 3 {
        return Ok(());
    }
    let trimmed = skip_env_assignments(segment.trim());
    let tokens = shell_tokenize(trimmed);
    if tokens.is_empty() {
        return Ok(());
    }
    let base = tokens[0]
        .rsplit('/')
        .next()
        .unwrap_or(&tokens[0])
        .to_string();

    // Eval-flag / heredoc checks on interpreters (unless opted out via #814).
    if INTERPRETER_COMMANDS.contains(&base.as_str()) && !inline_ok {
        for tok in &tokens[1..] {
            if EVAL_FLAGS.contains(&tok.as_str()) {
                return Err(format!(
                    "[BLOCKED — DO NOT RETRY] Interpreter '{base}' with inline code execution \
                     flag '{tok}' is blocked. Use a script file instead.\n\
                     This is a permanent security restriction."
                )
                .into());
            }
            if has_eval_flag_prefix(tok) {
                return Err(format!(
                    "[BLOCKED — DO NOT RETRY] Interpreter '{base}' with combined flag '{tok}' \
                     containing eval flag is blocked.\n\
                     This is a permanent security restriction."
                )
                .into());
            }
        }
        if tokens[1..].iter().any(|t| t.contains("<<")) {
            return Err(heredoc_blocked_message(&base).into());
        }
    }

    // Delegation-command walk (recursive).
    if DELEGATION_COMMANDS.contains(&base.as_str()) {
        let rest_tokens = delegated_command_tokens(&tokens[1..]);
        if let Some(&delegated_tok) = rest_tokens.first() {
            // In restricted mode, the delegated command must be in the allowlist.
            // #1419: use matches_allowlist_entry so multi-word entries like
            // "terraform plan *" match the delegated base "terraform".
            if let Some(al) = allowlist {
                let delegated = delegated_tok.rsplit('/').next().unwrap_or(delegated_tok);
                if !delegated.is_empty() && !matches_allowlist_entry(delegated, al) {
                    return Err(format!(
                        "[BLOCKED — DO NOT RETRY] '{base}' delegates to '{delegated}' which is not \
                         in the shell allowlist. This is a permanent restriction."
                    )
                    .into());
                }
            }
            let rest_str = rest_tokens.join(" ");
            return check_interpreter_inner(&rest_str, allowlist, depth + 1, inline_ok);
        }
    }

    Ok(())
}

/// Actionable message for the heredoc-stdin block (GL #1161): the restriction
/// is deliberate — inline code embedded in the command string never exists as
/// an inspectable artifact, unlike a script file, which leaves an auditable
/// trail and passes the write path's own guards. Name the exact workaround
/// instead of leaving the agent to rediscover it by trial and error.
fn heredoc_blocked_message(base: &str) -> String {
    format!(
        "[BLOCKED — DO NOT RETRY] Interpreter '{base}' with heredoc stdin is blocked. \
         Inline code in the command string leaves no auditable artifact.\n\
         Do this instead: write the code to a file, then run it —\n\
           1. create /tmp/snippet with your code (Write/ctx_edit tool)\n\
           2. {base} /tmp/snippet\n\
         This is a permanent security restriction."
    )
}

/// Commands that are unconditionally blocked regardless of allowlist membership.
/// These provide direct arbitrary code execution or re-enter the shell.
const UNCONDITIONAL_BLOCKED: &[&str] = &["eval", "exec", "source", "."];

/// POSIX shell builtins that are executed by the shell itself — they cannot
/// spawn an external process or escape any sandbox. Builtins bypass the
/// allowlist check entirely (#1022).
pub(super) const SHELL_BUILTINS: &[&str] = &[
    "exit", "command", ":", "true", "false", "cd", "echo", "test", "[", "read", "set", "unset",
    "export", "local", "return", "shift", "wait", "trap", "type", "hash", "pwd", "printf", "let",
    "declare", "readonly", "getopts", "umask", "ulimit", "break", "continue", "bg", "fg", "jobs",
    "times", "builtin", "enable", "shopt", "complete", "compgen",
];

/// Interpreters that can execute arbitrary code via -c/-e flags.
const INTERPRETER_COMMANDS: &[&str] = &[
    "python", "python3", "python2", "node", "ruby", "perl", "lua", "php", "bash", "sh", "zsh",
    "fish", "dash", "ksh",
];

/// Flags that indicate inline code execution for interpreters.
const EVAL_FLAGS: &[&str] = &[
    "-c", "-e", "-r", "-p", "--eval", "--exec", "-exec", "--print", "--run",
];

/// Script file extensions that indicate a file argument (not stdin execution).
const SCRIPT_EXTENSIONS: &[&str] = &[
    ".py", ".rb", ".js", ".ts", ".pl", ".lua", ".php", ".sh", ".bash", ".zsh", ".mjs", ".cjs",
    ".tsx", ".jsx",
];

/// Commands that delegate to another command (the delegated command must also be allowed).
/// `xargs` is here because `… | xargs bash -c '…'` would otherwise smuggle an
/// interpreter past both the allowlist and the inline-code check (GH #391).
const DELEGATION_COMMANDS: &[&str] = &["env", "nice", "timeout", "sudo", "doas", "xargs", "nohup"];

/// Skips a delegation command's own flags/operands to find the delegated
/// command token: leading `-x` flags, `KEY=VALUE` pairs (env), bare numbers
/// (timeout/nice durations) and `{}` placeholders (xargs -I).
fn delegated_command_tokens(tokens: &[String]) -> Vec<&str> {
    tokens
        .iter()
        .map(std::string::String::as_str)
        .skip_while(|t| {
            t.starts_with('-')
                || t.contains('=')
                || *t == "{}"
                || (!t.is_empty() && t.chars().all(|c| c.is_ascii_digit()))
        })
        .collect()
}

/// Check if a segment uses an interpreter with an eval flag, or a delegation command
/// whose target is not in the allowlist.
fn check_interpreter_abuse(segment: &str, allowlist: &[String]) -> Result<(), ShellError> {
    let inline_ok = crate::core::config::Config::load().shell_allow_inline_scripts_effective();
    check_interpreter_inner(segment, Some(allowlist), 0, inline_ok)
}

/// Check for combined flags like -pe, -ne, -ce that contain eval characters.
fn has_eval_flag_prefix(token: &str) -> bool {
    if !token.starts_with('-') || token.starts_with("--") || token.len() < 3 {
        return false;
    }
    let flag_chars = &token[1..];
    let eval_chars = ['c', 'e', 'r', 'p'];
    flag_chars.chars().any(|c| eval_chars.contains(&c))
}

/// Check if a segment is a bare interpreter after a pipe (no script file argument).
pub(super) fn is_bare_interpreter_stdin(segment: &str) -> bool {
    let trimmed = skip_env_assignments(segment.trim());
    let tokens = shell_tokenize(trimmed);
    if tokens.is_empty() {
        return false;
    }
    let base = tokens[0]
        .rsplit('/')
        .next()
        .unwrap_or(&tokens[0])
        .to_string();
    if !INTERPRETER_COMMANDS.contains(&base.as_str()) {
        return false;
    }
    let has_file_arg = tokens[1..]
        .iter()
        .any(|t| !t.starts_with('-') && SCRIPT_EXTENSIONS.iter().any(|ext| t.ends_with(ext)));
    let has_module_flag = tokens[1..].iter().any(|t| *t == "-m");
    !(has_file_arg || has_module_flag)
}

/// Dangerous flag patterns for specific commands.
const DANGEROUS_GIT_FLAGS: &[&str] = &[
    "--upload-pack",
    "--receive-pack",
    "--config=core.sshcommand",
    "--config=core.gitproxy",
];

const DANGEROUS_TAR_FLAGS: &[&str] = &["--to-command", "--use-compress-program"];

/// Blocked inline environment assignments that can hijack execution.
const BLOCKED_INLINE_ENV: &[&str] = &[
    "PATH=",
    "GIT_ASKPASS=",
    "GIT_SSH=",
    "GIT_SSH_COMMAND=",
    "GIT_EDITOR=",
    "GIT_EXTERNAL_DIFF=",
    "SSH_ASKPASS=",
    "LD_PRELOAD=",
    "DYLD_INSERT_LIBRARIES=",
];

fn check_dangerous_flags(segment: &str) -> Result<(), ShellError> {
    let trimmed = skip_env_assignments(segment.trim());
    let tokens = shell_tokenize(trimmed);
    if tokens.is_empty() {
        return Ok(());
    }
    let base = tokens[0]
        .rsplit('/')
        .next()
        .unwrap_or(&tokens[0])
        .to_string();

    match base.as_str() {
        "git" => {
            for tok in &tokens[1..] {
                for flag in DANGEROUS_GIT_FLAGS {
                    if tok.starts_with(flag) {
                        return Err(format!(
                            "[BLOCKED — DO NOT RETRY] 'git' with dangerous flag '{tok}' is blocked.\n\
                             This is a permanent security restriction."
                        ).into());
                    }
                }
            }
        }
        "tar" => {
            for tok in &tokens[1..] {
                for flag in DANGEROUS_TAR_FLAGS {
                    if tok.starts_with(flag) {
                        return Err(format!(
                            "[BLOCKED — DO NOT RETRY] 'tar' with dangerous flag '{tok}' is blocked.\n\
                             This is a permanent security restriction."
                        ).into());
                    }
                }
            }
        }
        "find" => {
            for tok in &tokens[1..] {
                if tok == "-exec" || tok == "-execdir" {
                    return Err(format!(
                        "[BLOCKED — DO NOT RETRY] 'find' with '{tok}' is blocked. \
                         Use 'find ... -print' and pipe to xargs instead.\n\
                         This is a permanent security restriction."
                    )
                    .into());
                }
            }
        }
        "awk" | "gawk" | "mawk" => {
            for tok in &tokens[1..] {
                if tok.contains("system(") {
                    return Err(format!(
                        "[BLOCKED — DO NOT RETRY] '{base}' with 'system()' call is blocked.\n\
                         This is a permanent security restriction."
                    )
                    .into());
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn check_inline_env_block(segment: &str) -> Result<(), ShellError> {
    let trimmed = segment.trim();
    for blocked in BLOCKED_INLINE_ENV {
        if trimmed.starts_with(blocked) {
            return Err(format!(
                "[BLOCKED — DO NOT RETRY] Inline environment override '{blocked}' is blocked.\n\
                 This is a permanent security restriction."
            )
            .into());
        }
    }
    Ok(())
}
/// #813: check whether a command token resolves to an existing file under the
/// project root. Called as a fallback when the base command name isn't in the
/// allowlist — agents frequently build project-local binaries (`go build -o
/// cbc_old`, `cargo build`, `gcc -o bench`) that shouldn't require a manual
/// `lean-ctx allow` round-trip.
///
/// Only auto-allows when ALL of:
/// 1. The token is a path (contains `/` or starts with `./`)
/// 2. The resolved path is an existing file
/// 3. The resolved path is under the project root
pub(super) fn is_project_root_binary(token: &str) -> bool {
    if !token.contains('/') {
        return false;
    }
    let path = std::path::Path::new(token);
    let resolved = if path.is_relative() {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            Err(_) => return false,
        }
    } else {
        path.to_path_buf()
    };
    let Ok(canonical) = resolved.canonicalize() else {
        return false;
    };
    if !canonical.is_file() {
        return false;
    }
    let Some(root) = crate::server::derive_project_root_from_cwd() else {
        return false;
    };
    let root_path = std::path::Path::new(&root);
    let canonical_root = root_path
        .canonicalize()
        .unwrap_or_else(|_| root_path.to_path_buf());
    canonical.starts_with(&canonical_root)
}

/// GH #1419: match a command's base binary against allowlist entries that may
/// contain subcommands or wildcards (e.g. "terraform plan *"). Compares `base`
/// against both the full entry and its first word.
pub(super) fn matches_allowlist_entry(base: &str, allowlist: &[String]) -> bool {
    allowlist
        .iter()
        .any(|entry| entry == base || entry.split_whitespace().next() == Some(base))
}

pub(super) fn check_all_segments(command: &str, allowlist: &[String]) -> Result<(), ShellError> {
    if allowlist.is_empty() {
        return Ok(());
    }

    if has_dangerous_patterns(command) {
        return Err(format!(
            "[BLOCKED — DO NOT RETRY] Command uses eval or $()/ backticks at command position, \
             which is blocked in restricted mode. \
             This is a permanent security restriction, not a transient error.\n\
             Command: {command}"
        )
        .into());
    }

    let segments = expand_to_leaf_segments(command)?;
    if segments.is_empty() {
        return Err("[BLOCKED — DO NOT RETRY] Empty command".into());
    }

    let total = segments.len();
    for (idx, seg) in segments.iter().enumerate() {
        check_inline_env_block(seg)?;
        let base = extract_base_from_segment(seg);
        if base.is_empty() {
            continue;
        }
        if UNCONDITIONAL_BLOCKED.contains(&base.as_str()) {
            return Err(format!(
                "[BLOCKED — DO NOT RETRY] '{base}' is unconditionally blocked \
                 regardless of allowlist membership. \
                 This is a permanent security restriction.\n\
                 Command: {command}"
            )
            .into());
        }
        if SHELL_BUILTINS.contains(&base.as_str()) {
            continue;
        }
        check_interpreter_abuse(seg, allowlist)?;
        check_dangerous_flags(seg)?;
        if !matches_allowlist_entry(&base, allowlist) {
            // #813: auto-allow binaries that resolve to existing files under
            // the project root. The first token (before rsplit) carries the
            // path context (e.g. "./cbc_old", "../bin/bench").
            let first_token = shell_tokenize(skip_env_assignments(seg.trim()))
                .into_iter()
                .next()
                .unwrap_or_default();
            if is_project_root_binary(&first_token) {
                tracing::info!(
                    "[shell_allowlist] auto-allowing project-root binary: {first_token}"
                );
                continue;
            }

            // #815: for compound commands, tell the user which segment was
            // blocked and that nothing ran (the pipeline is rejected as a
            // whole before execution, so no prefix commands executed).
            let mut msg = allowlist_block_message(&base);
            if total > 1 {
                msg.push_str(&format!(
                    "\n\n[pipeline: segment {}/{total} blocked — \
                     the entire command was rejected before execution, \
                     no part of the pipeline ran]",
                    idx + 1,
                ));
            }
            return Err(msg.into());
        }
    }
    Ok(())
}

/// Detect dangerous shell patterns that bypass allowlist intent.
///
/// Only blocks patterns that are genuinely dangerous at command position.
/// `$()` and backticks in *arguments* are allowed — the base command is
/// already validated by the allowlist, and blocking substitutions in
/// arguments breaks legitimate workflows (e.g. `git commit -m "$(cat ...)"`,
/// pre-commit hooks, playwright scripts).
fn has_dangerous_patterns(command: &str) -> bool {
    let trimmed = command.trim();

    for blocked in UNCONDITIONAL_BLOCKED {
        let with_space = format!("{blocked} ");
        if trimmed.starts_with(&with_space) {
            return true;
        }
        for sep in ["; ", "&& ", "|| ", "| ", "\n"] {
            if trimmed.contains(&format!("{sep}{blocked} ")) {
                return true;
            }
        }
    }

    if has_substitution_at_command_pos(trimmed) {
        return true;
    }

    false
}

/// Check if `$()` or backticks appear at command position (first token
/// of any segment). Substitutions in *arguments* are intentionally
/// allowed — the security boundary is the base-command allowlist check.
fn has_substitution_at_command_pos(command: &str) -> bool {
    let segments = split_on_operators(command);
    for seg in segments {
        let trimmed = seg.trim();
        let cmd_start = skip_env_assignments(trimmed);

        if cmd_start.starts_with("$(") {
            return true;
        }

        let tokens = shell_tokenize(cmd_start);
        let first_token = tokens.first().map_or("", std::string::String::as_str);
        if first_token.starts_with('`') || first_token == "`" {
            return true;
        }
    }
    false
}
