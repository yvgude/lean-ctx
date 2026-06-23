use crate::compound_lexer;
use crate::core::debug_log::{self, Route};
use crate::rewrite_registry;
use std::io::Read;
use std::sync::mpsc;
use std::time::Duration;

const HOOK_STDIN_TIMEOUT: Duration = Duration::from_secs(3);
mod observe;
pub use observe::*;
#[cfg(test)]
mod tests;

fn is_disabled() -> bool {
    std::env::var("LEAN_CTX_DISABLED").is_ok()
}

fn is_harden_active() -> bool {
    matches!(std::env::var("LEAN_CTX_HARDEN"), Ok(v) if v.trim() == "1")
}

fn is_shadow_mode_active() -> bool {
    if matches!(std::env::var("LEAN_CTX_SHADOW"), Ok(v) if v.trim() == "1") {
        return true;
    }
    crate::core::config::Config::load().shadow_mode
}

fn log_shadow_intercept(tool: &str, detail: &str) {
    if !is_shadow_mode_active() {
        return;
    }
    let Some(data_dir) = crate::core::data_dir::lean_ctx_data_dir().ok() else {
        return;
    };
    let log_path = data_dir.join("shadow.log");
    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let line = format!("[{ts}] intercepted {tool}: {detail}\n");
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
}

fn is_quiet() -> bool {
    matches!(std::env::var("LEAN_CTX_QUIET"), Ok(v) if v.trim() == "1")
}

/// Mark this process as a hook child so the daemon-client never auto-starts
/// the daemon from inside a hook (which would create zombie processes).
pub fn mark_hook_environment() {
    // SAFETY: called once at hook-process startup (CLI dispatch), before any
    // threads that read the environment are spawned.
    unsafe { std::env::set_var("LEAN_CTX_HOOK_CHILD", "1") };
}

/// Arms a watchdog that force-exits the process after the given duration.
/// Prevents hook processes from becoming zombies when stdin pipes break or
/// the IDE cancels the call. Since hooks MUST NOT spawn child processes
/// (to avoid orphan zombies), a simple exit(1) suffices.
pub fn arm_watchdog(timeout: Duration) {
    std::thread::spawn(move || {
        std::thread::sleep(timeout);
        eprintln!(
            "[lean-ctx hook] watchdog timeout after {}s — force exit",
            timeout.as_secs()
        );
        std::process::exit(1);
    });
}

/// Reads all of stdin with a timeout. Returns None if stdin is empty, broken, or times out.
fn read_stdin_with_timeout(timeout: Duration) -> Option<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let result = std::io::stdin().read_to_string(&mut buf);
        let _ = tx.send(result.ok().map(|_| buf));
    });
    match rx.recv_timeout(timeout) {
        Ok(Some(s)) if !s.is_empty() => Some(s),
        _ => None,
    }
}

fn build_dual_allow_output() -> String {
    serde_json::json!({
        "permission": "allow",
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow"
        }
    })
    .to_string()
}

fn build_dual_rewrite_output(tool_input: Option<&serde_json::Value>, rewritten: &str) -> String {
    let updated_input = if let Some(obj) = tool_input.and_then(|v| v.as_object()) {
        let mut m = obj.clone();
        m.insert(
            "command".to_string(),
            serde_json::Value::String(rewritten.to_string()),
        );
        serde_json::Value::Object(m)
    } else {
        serde_json::json!({ "command": rewritten })
    };

    serde_json::json!({
        // Cursor hook output format
        "permission": "allow",
        "updated_input": updated_input,
        // Claude Code / CodeBuddy hook output format (extra fields are ignored by other hosts)
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": {
                "command": rewritten
            }
        }
    })
    .to_string()
}

pub fn handle_rewrite() {
    let allow = build_dual_allow_output();
    if is_disabled() {
        print!("{allow}");
        return;
    }
    let binary = resolve_binary();
    let Some(input) = read_stdin_with_timeout(HOOK_STDIN_TIMEOUT) else {
        print!("{allow}");
        return;
    };

    let Ok(v) = serde_json::from_str::<serde_json::Value>(&input) else {
        tracing::warn!("[hook rewrite] invalid JSON payload, allowing passthrough");
        print!("{allow}");
        return;
    };

    let tool = v.get("tool_name").and_then(|t| t.as_str());
    let Some(tool_name) = tool else {
        print!("{allow}");
        return;
    };

    let is_shell_tool = matches!(
        tool_name,
        "Bash" | "bash" | "Shell" | "shell" | "runInTerminal" | "run_in_terminal" | "terminal"
    );
    if !is_shell_tool {
        print!("{allow}");
        return;
    }

    let tool_input = v.get("tool_input");
    let Some(cmd) = tool_input
        .and_then(|ti| ti.get("command"))
        .and_then(|c| c.as_str())
        .or_else(|| v.get("command").and_then(|c| c.as_str()))
    else {
        print!("{allow}");
        return;
    };

    if let Some(rewritten) = rewrite_candidate(cmd, &binary) {
        debug_log::log_hook_decision(
            "rewrite",
            tool_name,
            Route::LeanCtx,
            cmd,
            "rewritable command",
        );
        print!("{}", build_dual_rewrite_output(tool_input, &rewritten));
    } else {
        debug_log::log_hook_decision(
            "rewrite",
            tool_name,
            Route::Native,
            cmd,
            rewrite_skip_reason(cmd),
        );
        print!("{allow}");
    }
}

/// Human-readable reason a shell command was left to the native tool. Mirrors
/// the `None` branches of [`rewrite_candidate`] so #520's debug log can explain
/// *why* a call fell back to native instead of routing through lean-ctx.
fn rewrite_skip_reason(cmd: &str) -> &'static str {
    if cmd.starts_with("lean-ctx ") {
        "already a lean-ctx command"
    } else if cmd.contains("<<") {
        "heredoc cannot be rewritten safely"
    } else {
        "not a known read/search/list command"
    }
}

fn is_rewritable(cmd: &str) -> bool {
    rewrite_registry::is_rewritable_command(cmd)
}

fn wrap_single_command(cmd: &str, binary: &str) -> String {
    if cfg!(windows) {
        let escaped = cmd.replace('"', "\\\"");
        format!("{binary} -c \"{escaped}\"")
    } else {
        let shell_escaped = cmd.replace('\'', "'\\''");
        format!("{binary} -c '{shell_escaped}'")
    }
}

fn rewrite_candidate(cmd: &str, binary: &str) -> Option<String> {
    if cmd.starts_with("lean-ctx ") || cmd.starts_with(&format!("{binary} ")) {
        return None;
    }

    // Heredocs cannot survive the quoting round-trip through `lean-ctx -c '...'`.
    // Newlines get escaped, breaking the heredoc syntax entirely (GitHub #140).
    if cmd.contains("<<") {
        return None;
    }

    if let Some(rewritten) = rewrite_file_read_command(cmd, binary) {
        return Some(rewritten);
    }

    if let Some(rewritten) = rewrite_search_command(cmd, binary) {
        return Some(rewritten);
    }

    if let Some(rewritten) = rewrite_dir_list_command(cmd, binary) {
        return Some(rewritten);
    }

    if let Some(rewritten) = build_rewrite_compound(cmd, binary) {
        return Some(rewritten);
    }

    if is_rewritable(cmd) {
        return Some(wrap_single_command(cmd, binary));
    }

    None
}

/// Rewrites cat/head/tail to lean-ctx read with appropriate arguments.
/// Only rewrites simple single-file reads within the project scope.
fn rewrite_file_read_command(cmd: &str, binary: &str) -> Option<String> {
    if !rewrite_registry::is_file_read_command(cmd) {
        return None;
    }

    // Compound commands (pipes, chains) should not be rewritten as file reads.
    if cmd.contains('|') || cmd.contains("&&") || cmd.contains("||") || cmd.contains(';') {
        return None;
    }

    // Shell redirections indicate complex usage — don't rewrite.
    if cmd.contains(">&") || cmd.contains(">>") || cmd.contains(" >") {
        return None;
    }

    let parts = shell_tokenize(cmd);
    if parts.len() < 2 {
        return None;
    }

    match parts[0].as_str() {
        "cat" => {
            let path = parts[1..].join(" ");
            if is_outside_project_path(&path) {
                return None;
            }
            Some(format!("{binary} read {}", shell_quote(&path)))
        }
        "head" => {
            let refs: Vec<&str> = parts[1..].iter().map(String::as_str).collect();
            let (n, path) = parse_head_tail_args(&refs);
            let path = path?;
            if is_outside_project_path(path) {
                return None;
            }
            let qp = shell_quote(path);
            match n {
                Some(lines) => Some(format!("{binary} read {qp} -m lines:1-{lines}")),
                None => Some(format!("{binary} read {qp} -m lines:1-10")),
            }
        }
        "tail" => {
            let refs: Vec<&str> = parts[1..].iter().map(String::as_str).collect();
            let (n, path) = parse_head_tail_args(&refs);
            let path = path?;
            if is_outside_project_path(path) {
                return None;
            }
            let qp = shell_quote(path);
            let lines = n.unwrap_or(10);
            Some(format!("{binary} read {qp} -m lines:-{lines}"))
        }
        _ => None,
    }
}

/// Returns true if the path clearly points outside the current project.
/// Paths starting with `~`, `$`, or absolute paths that don't resolve
/// within the working directory should not be intercepted.
fn is_outside_project_path(path: &str) -> bool {
    let trimmed = path.trim();

    // Home-relative paths are always outside the project
    if trimmed.starts_with('~') {
        return true;
    }

    // Environment variable expansion — too complex, pass through
    if trimmed.starts_with('$') {
        return true;
    }

    // /proc, /sys, /dev, /tmp, /var — system paths
    if trimmed.starts_with("/proc/")
        || trimmed.starts_with("/sys/")
        || trimmed.starts_with("/dev/")
        || trimmed.starts_with("/tmp/")
        || trimmed.starts_with("/var/")
    {
        return true;
    }

    // Absolute paths: only pass through if they clearly point outside.
    // We can't know the project root here (hooks are stateless), but we can
    // detect common external patterns.
    if trimmed.starts_with('/') {
        // Home directory paths (e.g. /Users/*/Library, /home/*/.config)
        if trimmed.contains("/Library/") || trimmed.contains("/.config/") {
            return true;
        }
        // lean-ctx's own data directories
        if trimmed.contains("/.lean-ctx/") || trimmed.contains("/lean-ctx/logs/") {
            return true;
        }
    }

    false
}

/// Rewrites `rg <pattern> [path]` to `lean-ctx grep <pattern> [path]` for simple forms.
fn rewrite_search_command(cmd: &str, binary: &str) -> Option<String> {
    let parts = shell_tokenize(cmd);
    if parts.first().map(String::as_str) != Some("rg") {
        return None;
    }
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    if parts[1].starts_with('-') {
        return None;
    }
    let pattern = &parts[1];
    match parts.get(2) {
        Some(p) if p.starts_with('-') => None,
        Some(p) => Some(format!("{binary} grep {pattern} {}", shell_quote(p))),
        None => Some(format!("{binary} grep {pattern}")),
    }
}

/// Rewrites simple `ls [path]` to `lean-ctx ls [path]`.
fn rewrite_dir_list_command(cmd: &str, binary: &str) -> Option<String> {
    let parts = shell_tokenize(cmd);
    if parts.first().map(String::as_str) != Some("ls") {
        return None;
    }
    match parts.len() {
        1 => Some(format!("{binary} ls")),
        2 if !parts[1].starts_with('-') => Some(format!("{binary} ls {}", shell_quote(&parts[1]))),
        _ => None,
    }
}

/// Tokenize a shell command respecting single/double quotes and backslash escapes.
pub fn shell_tokenize(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;

    while let Some(c) = chars.next() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '\\' if !in_single => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Quote a path/arg for shell if it contains spaces or special chars.
pub fn shell_quote(s: &str) -> String {
    if s.contains(|c: char| c.is_whitespace() || c == '\'' || c == '"' || c == '\\') {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        s.to_string()
    }
}

fn parse_head_tail_args<'a>(args: &[&'a str]) -> (Option<usize>, Option<&'a str>) {
    let mut n: Option<usize> = None;
    let mut path: Option<&str> = None;

    let mut i = 0;
    while i < args.len() {
        if args[i] == "-n" && i + 1 < args.len() {
            n = args[i + 1].parse().ok();
            i += 2;
        } else if let Some(num) = args[i].strip_prefix("-n") {
            n = num.parse().ok();
            i += 1;
        } else if args[i].starts_with('-') && args[i].len() > 1 {
            if let Ok(num) = args[i][1..].parse::<usize>() {
                n = Some(num);
            }
            i += 1;
        } else {
            path = Some(args[i]);
            i += 1;
        }
    }

    (n, path)
}

fn build_rewrite_compound(cmd: &str, binary: &str) -> Option<String> {
    compound_lexer::rewrite_compound(cmd, |segment| {
        if segment.starts_with("lean-ctx ") || segment.starts_with(&format!("{binary} ")) {
            return None;
        }
        if is_rewritable(segment) {
            Some(wrap_single_command(segment, binary))
        } else {
            None
        }
    })
}

fn emit_rewrite(rewritten: &str) {
    let json_escaped = rewritten.replace('\\', "\\\\").replace('"', "\\\"");
    print!(
        "{{\"hookSpecificOutput\":{{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"allow\",\"updatedInput\":{{\"command\":\"{json_escaped}\"}}}}}}"
    );
}

pub fn handle_redirect() {
    let allow = build_dual_allow_output();
    if is_disabled() {
        let _ = read_stdin_with_timeout(HOOK_STDIN_TIMEOUT);
        print!("{allow}");
        return;
    }

    let Some(input) = read_stdin_with_timeout(HOOK_STDIN_TIMEOUT) else {
        print!("{allow}");
        return;
    };

    let Ok(v) = serde_json::from_str::<serde_json::Value>(&input) else {
        tracing::warn!("[hook redirect] invalid JSON payload, allowing passthrough");
        print!("{allow}");
        return;
    };

    let tool_name = v.get("tool_name").and_then(|t| t.as_str()).unwrap_or("");
    let tool_input = v.get("tool_input");

    match tool_name {
        "Read" | "read" | "read_file" => redirect_read(tool_input),
        "Grep" | "grep" | "search" | "ripgrep" => redirect_grep(tool_input),
        _ => print!("{allow}"),
    }
}

/// Redirect Read through lean-ctx for compression + caching.
/// Safe because `mark_hook_environment()` sets LEAN_CTX_HOOK_CHILD=1 which
/// prevents daemon auto-start. The subprocess uses the fast local-only path.
fn redirect_read(tool_input: Option<&serde_json::Value>) {
    let path = tool_input
        .and_then(|ti| ti.get("path"))
        .and_then(|p| p.as_str())
        .unwrap_or("");

    if path.is_empty() {
        debug_log::log_hook_decision(
            "redirect",
            "Read",
            Route::Native,
            "<none>",
            "no path in tool input",
        );
        print!("{}", build_dual_allow_output());
        return;
    }
    if should_passthrough(path) {
        debug_log::log_hook_decision(
            "redirect",
            "Read",
            Route::Native,
            path,
            "passthrough path (sensitive/binary/excluded)",
        );
        print!("{}", build_dual_allow_output());
        return;
    }

    let shadow = is_shadow_mode_active();
    if is_harden_active() || shadow {
        tracing::info!(
            "[hook redirect] {} active, redirecting Read through lean-ctx",
            if shadow { "shadow mode" } else { "harden mode" }
        );
    }

    let binary = resolve_binary();
    let temp_path = redirect_temp_path(path);

    if let Some(mut output) =
        run_with_timeout(&binary, &["read", path], REDIRECT_SUBPROCESS_TIMEOUT)
    {
        if shadow {
            let header = format!(
                "[shadow-mode: Read intercepted → ctx_read(\"{path}\", \"full\"). Use ctx_read directly for better performance.]\n\n"
            );
            let mut prefixed = header.into_bytes();
            prefixed.append(&mut output);
            output = prefixed;
        }
        if !output.is_empty() && std::fs::write(&temp_path, &output).is_ok() {
            let temp_str = temp_path.to_str().unwrap_or("");
            debug_log::log_hook_decision(
                "redirect",
                "Read",
                Route::LeanCtx,
                path,
                "redirected to ctx_read",
            );
            print!("{}", build_redirect_output(tool_input, "path", temp_str));
            log_shadow_intercept("Read", path);
            return;
        }
    }

    debug_log::log_hook_decision(
        "redirect",
        "Read",
        Route::Native,
        path,
        "lean-ctx read produced no output",
    );
    print!("{}", build_dual_allow_output());
}

/// Redirect Grep through lean-ctx for compressed results.
fn redirect_grep(tool_input: Option<&serde_json::Value>) {
    let pattern = tool_input
        .and_then(|ti| ti.get("pattern"))
        .and_then(|p| p.as_str())
        .unwrap_or("");
    let search_path = tool_input
        .and_then(|ti| ti.get("path"))
        .and_then(|p| p.as_str())
        .unwrap_or(".");

    if pattern.is_empty() {
        debug_log::log_hook_decision(
            "redirect",
            "Grep",
            Route::Native,
            "<none>",
            "no pattern in tool input",
        );
        print!("{}", build_dual_allow_output());
        return;
    }

    let shadow = is_shadow_mode_active();
    if is_harden_active() || shadow {
        tracing::info!(
            "[hook redirect] {} active, redirecting Grep through lean-ctx",
            if shadow { "shadow mode" } else { "harden mode" }
        );
    }

    let binary = resolve_binary();
    let key = format!("grep:{pattern}:{search_path}");
    let temp_path = redirect_temp_path(&key);

    if let Some(mut output) = run_with_timeout(
        &binary,
        &["grep", pattern, search_path],
        REDIRECT_SUBPROCESS_TIMEOUT,
    ) {
        if shadow {
            let header = format!(
                "[shadow-mode: Grep intercepted → ctx_search(\"{pattern}\", \"{search_path}\"). Use ctx_search directly for better performance.]\n\n"
            );
            let mut prefixed = header.into_bytes();
            prefixed.append(&mut output);
            output = prefixed;
        }
        if !output.is_empty() && std::fs::write(&temp_path, &output).is_ok() {
            let temp_str = temp_path.to_str().unwrap_or("");
            debug_log::log_hook_decision(
                "redirect",
                "Grep",
                Route::LeanCtx,
                &format!("{pattern} in {search_path}"),
                "redirected to ctx_search",
            );
            print!("{}", build_redirect_output(tool_input, "path", temp_str));
            log_shadow_intercept("Grep", &format!("{pattern} in {search_path}"));
            return;
        }
    }

    debug_log::log_hook_decision(
        "redirect",
        "Grep",
        Route::Native,
        &format!("{pattern} in {search_path}"),
        "lean-ctx grep produced no output",
    );
    print!("{}", build_dual_allow_output());
}

const REDIRECT_SUBPROCESS_TIMEOUT: Duration = Duration::from_secs(10);

/// Run a lean-ctx subprocess with a hard timeout. Returns stdout on success.
/// Kills the child if it exceeds the timeout to prevent orphan processes.
fn run_with_timeout(binary: &str, args: &[&str], timeout: Duration) -> Option<Vec<u8>> {
    let mut child = std::process::Command::new(binary)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let mut stdout = Vec::new();
                if let Some(mut out) = child.stdout.take() {
                    let _ = out.read_to_end(&mut stdout);
                }
                return if stdout.is_empty() {
                    None
                } else {
                    Some(stdout)
                };
            }
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

fn redirect_temp_path(key: &str) -> std::path::PathBuf {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    let hash = hasher.finish();

    let temp_dir = std::env::temp_dir().join("lean-ctx-hook");
    let _ = std::fs::create_dir_all(&temp_dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&temp_dir, std::fs::Permissions::from_mode(0o700));
    }
    temp_dir.join(format!("{hash:016x}.lctx"))
}

fn build_redirect_output(
    tool_input: Option<&serde_json::Value>,
    field: &str,
    temp_path: &str,
) -> String {
    let updated_input = if let Some(obj) = tool_input.and_then(|v| v.as_object()) {
        let mut m = obj.clone();
        m.insert(
            field.to_string(),
            serde_json::Value::String(temp_path.to_string()),
        );
        serde_json::Value::Object(m)
    } else {
        serde_json::json!({ field: temp_path })
    };

    serde_json::json!({
        "permission": "allow",
        "updated_input": updated_input,
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": { field: temp_path }
        }
    })
    .to_string()
}

const PASSTHROUGH_SUBSTRINGS: &[&str] = &[
    ".cursorrules",
    ".cursor/rules",
    ".cursor/hooks",
    "skill.md",
    "agents.md",
    ".env",
    "hooks.json",
    "node_modules",
];

const PASSTHROUGH_EXTENSIONS: &[&str] = &[
    "lock", "png", "jpg", "jpeg", "gif", "webp", "pdf", "ico", "svg", "woff", "woff2", "ttf", "eot",
];

fn should_passthrough(path: &str) -> bool {
    let p = path.to_lowercase();

    if PASSTHROUGH_SUBSTRINGS.iter().any(|s| p.contains(s)) {
        return true;
    }

    std::path::Path::new(&p)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            PASSTHROUGH_EXTENSIONS
                .iter()
                .any(|e| ext.eq_ignore_ascii_case(e))
        })
}

fn codex_rewrite_output(rewritten: &str) -> String {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": {
                "command": rewritten
            }
        }
    })
    .to_string()
}

pub fn handle_codex_pretooluse() {
    if is_disabled() {
        return;
    }
    let binary = resolve_binary();
    let Some(input) = read_stdin_with_timeout(HOOK_STDIN_TIMEOUT) else {
        return;
    };

    let tool = extract_json_field(&input, "tool_name");
    if !matches!(tool.as_deref(), Some("Bash" | "bash")) {
        return;
    }

    let Some(cmd) = extract_json_field(&input, "command") else {
        return;
    };

    if let Some(rewritten) = rewrite_candidate(&cmd, &binary) {
        print!("{}", codex_rewrite_output(&rewritten));
    }
}

/// Emit SessionStart guidance through Codex's documented hidden-context channel.
///
/// Codex's hook contract (<https://developers.openai.com/codex/hooks>) accepts JSON
/// on stdout with `hookSpecificOutput.additionalContext`, which is injected as
/// model-visible developer context rather than surfaced to the user as plain text
/// (#368). Plain stdout text is also added as developer context today, but only the
/// JSON form is the documented additional-context channel; aligning with it
/// future-proofs the hook for Codex's TUI-visibility fix (openai/codex#16933) and
/// matches how the dedicated rules-injection path already emits context.
pub(crate) fn session_start_additional_context_json(additional_context: &str) -> String {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": additional_context,
        }
    })
    .to_string()
}

pub(crate) fn emit_session_start_additional_context(additional_context: &str) {
    println!(
        "{}",
        session_start_additional_context_json(additional_context)
    );
}

pub fn handle_codex_session_start() {
    if is_quiet() {
        return;
    }
    // Dedicated rules-injection mode (#343): the `hook observe` SessionStart hook
    // injects the full rules summary as additionalContext, so stay silent here to
    // avoid double-injecting on Codex (which fires both hooks on SessionStart).
    if crate::core::config::Config::load().dedicated_session_context_active() {
        return;
    }
    emit_session_start_additional_context(
        "For shell commands matched by lean-ctx compression rules, prefer `lean-ctx -c \"<command>\"`. If a Bash call is blocked, rerun it with the exact command suggested by the hook.",
    );
}

/// Copilot-specific PreToolUse handler.
/// VS Code Copilot Chat uses the same hook format as Claude Code.
/// Tool names differ: "runInTerminal" / "editFile" instead of "Bash" / "Read".
pub fn handle_copilot() {
    if is_disabled() {
        return;
    }
    let binary = resolve_binary();
    let Some(input) = read_stdin_with_timeout(HOOK_STDIN_TIMEOUT) else {
        return;
    };

    let tool = extract_json_field(&input, "tool_name");
    let Some(tool_name) = tool.as_deref() else {
        return;
    };

    let is_shell_tool = matches!(
        tool_name,
        "Bash" | "bash" | "runInTerminal" | "run_in_terminal" | "terminal" | "shell"
    );
    if !is_shell_tool {
        return;
    }

    let Some(cmd) = extract_json_field(&input, "command") else {
        return;
    };

    if let Some(rewritten) = rewrite_candidate(&cmd, &binary) {
        emit_rewrite(&rewritten);
    }
}

/// Inline rewrite: takes a command as CLI args, prints the rewritten command to stdout.
/// The command is passed as positional arguments, not via stdin JSON.
pub fn handle_rewrite_inline() {
    if is_disabled() {
        return;
    }
    let binary = resolve_binary();
    let args: Vec<String> = std::env::args().collect();
    // args: [binary, "hook", "rewrite-inline", ...command parts]
    if args.len() < 4 {
        return;
    }
    let cmd = args[3..].join(" ");

    if let Some(rewritten) = rewrite_candidate(&cmd, &binary) {
        print!("{rewritten}");
        return;
    }

    if cmd.starts_with("lean-ctx ") || cmd.starts_with(&format!("{binary} ")) {
        print!("{cmd}");
        return;
    }

    print!("{cmd}");
}

/// Resolve the lean-ctx executable path for hook command emission and
/// subprocess spawning. Always the **native** OS path: the MSYS/Git-Bash
/// `/c/...` form breaks `CreateProcess` on Windows and cannot be run by
/// PowerShell or cmd (#518). Native `C:/...` runs in PowerShell, cmd *and*
/// Git Bash, so it is the correct universal form for executed commands.
/// (MSYS `/c/...` is only needed for bash *source* lines — see `cli::shell_init`.)
fn resolve_binary() -> String {
    crate::core::portable_binary::resolve_portable_binary()
}

fn extract_json_field(input: &str, field: &str) -> Option<String> {
    let key = format!("\"{field}\":");
    let key_pos = input.find(&key)?;
    let after_colon = &input[key_pos + key.len()..];
    let trimmed = after_colon.trim_start();
    if !trimmed.starts_with('"') {
        return None;
    }
    let rest = &trimmed[1..];
    let bytes = rest.as_bytes();
    let mut end = 0;
    while end < bytes.len() {
        if bytes[end] == b'\\' && end + 1 < bytes.len() {
            end += 2;
            continue;
        }
        if bytes[end] == b'"' {
            break;
        }
        end += 1;
    }
    if end >= bytes.len() {
        return None;
    }
    let raw = &rest[..end];
    Some(raw.replace("\\\"", "\"").replace("\\\\", "\\"))
}
