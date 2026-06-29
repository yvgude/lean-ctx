use crate::compound_lexer;
use crate::core::debug_log::{self, Route};
use crate::rewrite_registry;
use std::io::Read;
use std::sync::mpsc;
use std::time::Duration;

const HOOK_STDIN_TIMEOUT: Duration = Duration::from_secs(3);

/// Hard wall-clock budget for a command-gating hook (rewrite/redirect) to produce
/// its decision. Sized above the worst legitimate single read path (stdin 3s +
/// redirect subprocess 10s) so valid work always completes; a true hang — or a
/// dead-winner dedup loser that would otherwise wait then redo the work — is
/// bounded here and FAILS OPEN instead of wedging the host's tool call (#1035).
const HOOK_GATING_TIMEOUT: Duration = Duration::from_secs(15);
mod dedup;
mod edit_health;
mod observe;
mod payload;
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

/// Run a command-gating hook's decision logic under a hard wall-clock timeout and
/// print the result exactly once.
///
/// On timeout the hook FAILS OPEN — it prints the allow/pass-through decision so a
/// slow or hung hook (a stalled subprocess, a wedged dedup wait, a saturated host)
/// can never block the host's tool call: the command simply runs unmodified
/// (#1035). The worker thread is abandoned on timeout (it only sends to a channel,
/// never prints, and dies with the process), so there is no double-output race —
/// `emit_gating_decision` is the single writer to stdout.
fn emit_gating_decision<F>(timeout: Duration, work: F)
where
    F: FnOnce() -> String + Send + 'static,
{
    let out = decide_with_timeout(timeout, build_dual_allow_output(), work);
    print!("{out}");
}

/// Run `work` under a hard wall-clock timeout, returning `fallback` if it does not
/// finish in time. Split from [`emit_gating_decision`]'s printing so the fail-open
/// behavior is unit-testable. The worker only sends to a channel (it never prints)
/// and is abandoned on timeout, so it can never double-write the host's stdout
/// (#1035).
fn decide_with_timeout<F>(timeout: Duration, fallback: String, work: F) -> String
where
    F: FnOnce() -> String + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    rx.recv_timeout(timeout).unwrap_or(fallback)
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
        // Cursor hook output format.
        "permission": "allow",
        "updated_input": updated_input.clone(),
        // GitHub Copilot CLI preToolUse format: top-level `permissionDecision`
        // + `modifiedArgs` (a full substitute-args object). Copilot ignores
        // `hookSpecificOutput`, so without these fields it runs the command
        // unmodified even after the camelCase payload parses correctly (#551).
        "permissionDecision": "allow",
        "modifiedArgs": updated_input.clone(),
        // Claude Code / CodeBuddy hook output format (other hosts ignore it).
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": updated_input
        }
    })
    .to_string()
}

/// True when a host tool name denotes a shell/terminal command tool.
///
/// Copilot CLI exposes `powershell` as a first-class shell tool on Windows
/// (paired with `bash` per the CLI tool reference); without it Windows shell
/// calls bypass rewrite (#556). Shared by `handle_rewrite` and `handle_copilot`.
fn is_shell_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "Bash"
            | "bash"
            | "Shell"
            | "shell"
            | "runInTerminal"
            | "run_in_terminal"
            | "terminal"
            | "PowerShell"
            | "powershell"
            | "pwsh"
    )
}

pub fn handle_rewrite() {
    emit_gating_decision(HOOK_GATING_TIMEOUT, compute_rewrite);
}

/// Decide the rewrite hook's stdout (a rewrite or an allow-passthrough) without
/// printing, so [`handle_rewrite`] can run it under the fail-open timeout (#1035).
fn compute_rewrite() -> String {
    if is_disabled() {
        return build_dual_allow_output();
    }
    let binary = resolve_binary();
    let Some(input) = read_stdin_with_timeout(HOOK_STDIN_TIMEOUT) else {
        return build_dual_allow_output();
    };

    let Ok(v) = serde_json::from_str::<serde_json::Value>(&input) else {
        tracing::warn!("[hook rewrite] invalid JSON payload, allowing passthrough");
        return build_dual_allow_output();
    };

    // Resolve across host shapes: Claude/Cursor send snake_case `tool_name` +
    // `tool_input`; Copilot CLI sends camelCase `toolName` + `toolArgs` (a
    // JSON-encoded string). Before #551 only the snake_case path was read.
    let Some(tool_name) = payload::resolve_tool_name(&v) else {
        return build_dual_allow_output();
    };

    if !is_shell_tool(&tool_name) {
        return build_dual_allow_output();
    }

    let tool_args = payload::resolve_tool_args(&v);
    let Some(cmd) = payload::resolve_command(&v, tool_args.as_ref()) else {
        return build_dual_allow_output();
    };

    // #1032: Cursor fires preToolUse twice. Dedup on a PID-independent key (tool +
    // command) so the second fire replays the decision instead of re-logging.
    let key_material = format!("{tool_name}\u{0}{cmd}");
    dedup::deduped("rewrite", &key_material, || {
        if let Some(rewritten) = rewrite_candidate(&cmd, &binary) {
            debug_log::log_hook_decision(
                "rewrite",
                &tool_name,
                Route::LeanCtx,
                &cmd,
                "rewritable command",
            );
            build_dual_rewrite_output(tool_args.as_ref(), &rewritten)
        } else {
            debug_log::log_hook_decision(
                "rewrite",
                &tool_name,
                Route::Native,
                &cmd,
                rewrite_skip_reason(&cmd),
            );
            build_dual_allow_output()
        }
    })
}

/// Human-readable reason a shell command was left to the native tool. Mirrors
/// the `None` branches of [`rewrite_candidate`] so #520's debug log can explain
/// *why* a call fell back to native instead of routing through lean-ctx.
fn rewrite_skip_reason(cmd: &str) -> &'static str {
    if cmd.starts_with("lean-ctx ") {
        "already a lean-ctx command"
    } else if cmd.contains("<<") {
        "heredoc cannot be rewritten safely"
    } else if is_compound(cmd) && !crate::core::shell_allowlist::passes_enforced(cmd) {
        "compound pipes/chains into a non-allowlisted or interpreter sink — left raw for the agent shell"
    } else {
        "not a known read/search/list command"
    }
}

fn is_rewritable(cmd: &str) -> bool {
    rewrite_registry::is_rewritable_command(cmd)
}

/// True when `cmd` carries a top-level shell operator (`&&`, `||`, `;`, `|`),
/// i.e. it is a compound/pipeline rather than a single command. Compounds are
/// handled authoritatively by [`build_rewrite_compound`]; this guards the
/// single-command `is_rewritable` fallback in [`rewrite_candidate`] so a
/// compound the compound-handler declined is never re-wrapped whole.
fn is_compound(cmd: &str) -> bool {
    compound_lexer::split_compound(cmd)
        .iter()
        .any(|s| matches!(s, compound_lexer::Segment::Operator(_)))
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

    // Single-command fallback only. A compound that `build_rewrite_compound`
    // declined (tricky pipe/chain sink, or no rewritable segment) must NOT be
    // re-wrapped here: wrapping the whole string in `lean-ctx -c '…'` would newly
    // subject its sink to the allowlist gate and could block a command the
    // agent's shell ran fine before (#589). Compounds are authoritative above.
    if !is_compound(cmd) && is_rewritable(cmd) {
        return Some(wrap_single_command(cmd, binary));
    }

    None
}

/// Rewrites cat/head/tail to lean-ctx read with appropriate arguments.
/// Only rewrites simple single-file reads within the project scope.
fn rewrite_file_read_command(cmd: &str, binary: &str) -> Option<String> {
    // Unix file-read commands come from the central registry; PowerShell-native
    // cmdlets (Get-Content/gc) are detected here so they are not added to the POSIX
    // shell-alias/registry surface (#561).
    if !rewrite_registry::is_file_read_command(cmd) && !is_powershell_file_read(cmd) {
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
        "Get-Content" | "gc" => rewrite_get_content(&parts, binary),
        _ => None,
    }
}

/// True if the command is a PowerShell-native file-read cmdlet (`Get-Content`/`gc`).
fn is_powershell_file_read(cmd: &str) -> bool {
    matches!(cmd.split_whitespace().next(), Some("Get-Content" | "gc"))
}

/// Maps `Get-Content`/`gc` to `lean-ctx read`, honoring `-Path`/`-LiteralPath`, the
/// positional path, `-TotalCount`/`-Head`/`-First` (first N lines) and `-Tail`/`-Last`
/// (last N lines). PowerShell parameter names are case-insensitive. Any other flag, a
/// missing path, multiple files, or both head+tail makes it pass through (conservative,
/// mirroring the Unix cat/head/tail handling).
fn rewrite_get_content(parts: &[String], binary: &str) -> Option<String> {
    let mut path: Option<String> = None;
    let mut head_n: Option<u64> = None;
    let mut tail_n: Option<u64> = None;
    let mut i = 1;
    while i < parts.len() {
        if let Some(flag) = parts[i].strip_prefix('-') {
            let value = parts.get(i + 1);
            match flag.to_ascii_lowercase().as_str() {
                "path" | "literalpath" => path = Some(value?.clone()),
                "totalcount" | "head" | "first" => head_n = Some(value?.parse().ok()?),
                "tail" | "last" => tail_n = Some(value?.parse().ok()?),
                _ => return None,
            }
            i += 2;
        } else if path.is_none() {
            path = Some(parts[i].clone());
            i += 1;
        } else {
            return None;
        }
    }
    let path = path?;
    if is_outside_project_path(&path) || (head_n.is_some() && tail_n.is_some()) {
        return None;
    }
    let qp = shell_quote(&path);
    match (head_n, tail_n) {
        (Some(n), None) => Some(format!("{binary} read {qp} -m lines:1-{n}")),
        (None, Some(n)) => Some(format!("{binary} read {qp} -m lines:-{n}")),
        _ => Some(format!("{binary} read {qp}")),
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

/// Rewrites `rg <pattern> [path]` (and PowerShell `Select-String`/`sls`, #561) to
/// `lean-ctx grep <pattern> [path]` for simple forms.
fn rewrite_search_command(cmd: &str, binary: &str) -> Option<String> {
    let parts = shell_tokenize(cmd);
    match parts.first().map(String::as_str) {
        Some("rg") => {
            if parts.len() < 2 || parts.len() > 3 || parts[1].starts_with('-') {
                return None;
            }
            let pattern = &parts[1];
            match parts.get(2) {
                Some(p) if p.starts_with('-') => None,
                Some(p) => Some(format!("{binary} grep {pattern} {}", shell_quote(p))),
                None => Some(format!("{binary} grep {pattern}")),
            }
        }
        Some("Select-String" | "sls") => rewrite_select_string(&parts, binary),
        _ => None,
    }
}

/// Maps `Select-String`/`sls` to `lean-ctx grep`, honoring `-Pattern` and
/// `-Path`/`-LiteralPath` plus the positional `<pattern> [path]` form. Patterns are
/// quoted (PowerShell patterns often contain spaces). Any other flag, a missing
/// pattern, or extra operands makes it pass through.
fn rewrite_select_string(parts: &[String], binary: &str) -> Option<String> {
    let mut pattern: Option<String> = None;
    let mut path: Option<String> = None;
    let mut i = 1;
    while i < parts.len() {
        if let Some(flag) = parts[i].strip_prefix('-') {
            let value = parts.get(i + 1);
            match flag.to_ascii_lowercase().as_str() {
                "pattern" => pattern = Some(value?.clone()),
                "path" | "literalpath" => path = Some(value?.clone()),
                _ => return None,
            }
            i += 2;
        } else if pattern.is_none() {
            pattern = Some(parts[i].clone());
            i += 1;
        } else if path.is_none() {
            path = Some(parts[i].clone());
            i += 1;
        } else {
            return None;
        }
    }
    let pattern = shell_quote(&pattern?);
    match path {
        Some(p) if is_outside_project_path(&p) => None,
        Some(p) => Some(format!("{binary} grep {pattern} {}", shell_quote(&p))),
        None => Some(format!("{binary} grep {pattern}")),
    }
}

/// Rewrites simple `ls [path]` (and PowerShell `Get-ChildItem`/`gci`, #561) to
/// `lean-ctx ls [path]`.
fn rewrite_dir_list_command(cmd: &str, binary: &str) -> Option<String> {
    let parts = shell_tokenize(cmd);
    match parts.first().map(String::as_str) {
        Some("ls") => match parts.len() {
            1 => Some(format!("{binary} ls")),
            2 if !parts[1].starts_with('-') => {
                Some(format!("{binary} ls {}", shell_quote(&parts[1])))
            }
            _ => None,
        },
        Some("Get-ChildItem" | "gci") => rewrite_get_childitem(&parts, binary),
        _ => None,
    }
}

/// Maps `Get-ChildItem`/`gci` to `lean-ctx ls`, honoring `-Path`/`-LiteralPath` and the
/// positional path. Other flags (e.g. `-Recurse`, `-Filter`) or extra operands pass
/// through.
fn rewrite_get_childitem(parts: &[String], binary: &str) -> Option<String> {
    let mut path: Option<String> = None;
    let mut i = 1;
    while i < parts.len() {
        if let Some(flag) = parts[i].strip_prefix('-') {
            let value = parts.get(i + 1);
            match flag.to_ascii_lowercase().as_str() {
                "path" | "literalpath" => path = Some(value?.clone()),
                _ => return None,
            }
            i += 2;
        } else if path.is_none() {
            path = Some(parts[i].clone());
            i += 1;
        } else {
            return None;
        }
    }
    match path {
        Some(p) => Some(format!("{binary} ls {}", shell_quote(&p))),
        None => Some(format!("{binary} ls")),
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

/// Rewrites a compound/pipeline (`a | b`, `a && b`, `a; b`, …) by wrapping the
/// WHOLE string in a single `lean-ctx -c "…"` — but only when it would pass the
/// allowlist gate. Otherwise it declines (`None`) and the command is left to the
/// agent's shell unchanged.
///
/// Why wrap-whole (not per-segment, the previous behavior): `lean-ctx -c` runs
/// the command in a profile-free POSIX shell and compresses only the FINAL
/// output, so `|`, `&&`, `||`, `;` all work natively inside it. The old
/// per-segment split left the operators in the OUTER (hooked) shell, which broke
/// two real cases (#589, idea by @getappz):
///   1. Aliased builtins (`head`, `tail`, …) resolve to an undefined `_lc`
///      helper in non-interactive git-bash → `_lc: command not found` on Windows.
///   2. The LEFT side of a pipe got compressed, so the downstream command read
///      the lean-ctx digest instead of the raw bytes it expected.
///
/// Why gate-clean only (compat-first, no new block, no bypass): wrapping subjects
/// every segment — including the pipe sink — to the allowlist. For gate-clean
/// compounds (`git log | head`, `cargo test && npm run lint`) that is exactly
/// right (compressed + fully gated). For a compound whose sink is an
/// interpreter-eval (`python3 -c …`) or a non-allowlisted tool, wrapping would
/// NEWLY block a command the agent's shell ran fine before. We decline instead
/// and leave it raw, so the user's own shell-security config keeps governing it
/// — the pre-existing behavior, with no agent-reachable raw/no-gate path opened.
fn build_rewrite_compound(cmd: &str, binary: &str) -> Option<String> {
    let segments = compound_lexer::split_compound(cmd);
    let commands: Vec<&str> = segments
        .iter()
        .filter_map(|s| match s {
            compound_lexer::Segment::Command(c) => Some(c.trim()),
            compound_lexer::Segment::Operator(_) => None,
        })
        .collect();

    // No top-level operator → single command; the caller's wrap_single_command
    // fallback owns it.
    if segments.len() == commands.len() {
        return None;
    }

    let is_leanctx = |c: &str| c.starts_with("lean-ctx ") || c.starts_with(&format!("{binary} "));

    // A segment is already a lean-ctx call → don't nest `-c "… lean-ctx -c …"`.
    if commands.iter().any(|c| is_leanctx(c)) {
        return None;
    }

    // Nothing lean-ctx could compress/redirect → leave it to the native shell.
    if !commands.iter().any(|c| is_rewritable(c)) {
        return None;
    }

    // Wrap-whole only when the entire compound would pass the allowlist gate;
    // otherwise a tricky sink would be newly blocked (see doc above).
    if crate::core::shell_allowlist::passes_enforced(cmd) {
        Some(wrap_single_command(cmd, binary))
    } else {
        None
    }
}

/// The lean-ctx redirect a host tool name maps to, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RedirectKind {
    Read,
    Grep,
    Glob,
    None,
}

/// Classify a host tool name into the lean-ctx redirect it should take.
///
/// Covers the documented read/search/glob tool names across hosts. Copilot CLI
/// fires the redirect hook for *every* tool call and dispatches purely on the tool
/// name, so its aliases must be listed here: `view` (its read tool) and `rg` (its
/// search alias) were previously unmatched and passed through uncompressed (#562).
fn classify_redirect(tool_name: &str) -> RedirectKind {
    match tool_name {
        "Read" | "read" | "read_file" | "view" => RedirectKind::Read,
        "Grep" | "grep" | "search" | "ripgrep" | "rg" => RedirectKind::Grep,
        "Glob" | "glob" => RedirectKind::Glob,
        _ => RedirectKind::None,
    }
}

pub fn handle_redirect() {
    emit_gating_decision(HOOK_GATING_TIMEOUT, compute_redirect);
}

/// Decide the redirect hook's stdout (a redirect or an allow-passthrough) without
/// printing, so [`handle_redirect`] can run it under the fail-open timeout (#1035).
fn compute_redirect() -> String {
    if is_disabled() {
        let _ = read_stdin_with_timeout(HOOK_STDIN_TIMEOUT);
        return build_dual_allow_output();
    }

    let Some(input) = read_stdin_with_timeout(HOOK_STDIN_TIMEOUT) else {
        return build_dual_allow_output();
    };

    let Ok(v) = serde_json::from_str::<serde_json::Value>(&input) else {
        tracing::warn!("[hook redirect] invalid JSON payload, allowing passthrough");
        return build_dual_allow_output();
    };

    // Normalise host payload shapes (snake_case vs Copilot CLI camelCase, #551).
    let tool_name = payload::resolve_tool_name(&v).unwrap_or_default();
    let tool_args = payload::resolve_tool_args(&v);

    let kind = classify_redirect(&tool_name);
    if matches!(kind, RedirectKind::None) {
        return build_dual_allow_output();
    }

    // #1032: Cursor fires preToolUse twice (two processes, identical payload), so a
    // naive redirect runs the lean-ctx subprocess and logs twice. Dedup on a
    // PID-independent key (tool + args) so the second fire replays the first's
    // response — one subprocess, one log entry.
    let args_json = tool_args
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default();
    let key_material = format!("{tool_name}\u{0}{args_json}");
    dedup::deduped("redirect", &key_material, || {
        produce_redirect_output(kind, tool_args.as_ref())
    })
}

/// Build the redirect stdout for a classified tool call. Returns the full hook
/// response (redirect or allow-passthrough) so [`handle_redirect`] can route it
/// through the double-fire dedup before printing exactly once.
fn produce_redirect_output(kind: RedirectKind, tool_args: Option<&serde_json::Value>) -> String {
    match kind {
        RedirectKind::Read => redirect_read(tool_args),
        RedirectKind::Grep => redirect_grep(tool_args),
        RedirectKind::Glob => redirect_glob(tool_args),
        RedirectKind::None => build_dual_allow_output(),
    }
}

/// Argv for the `lean-ctx read` subprocess a redirected native Read runs.
///
/// Pinned to `-m full` (verbatim, edit-ready content). The default `auto`
/// mode degrades a large file to a structure MAP — signatures, not content —
/// so the host's native Read would receive the wrong thing and silently
/// ignore `offset`/`limit` (#1021). With the temp file holding faithful full
/// content the host applies its own `offset`/`limit` to it, so windowed reads
/// keep working without lean-ctx having to reimplement them.
fn redirect_read_args(path: &str) -> [&str; 4] {
    ["read", path, "-m", "full"]
}

/// Redirect Read through lean-ctx for compression + caching.
/// Safe because `mark_hook_environment()` sets LEAN_CTX_HOOK_CHILD=1 which
/// prevents daemon auto-start. The subprocess uses the fast local-only path.
fn redirect_read(tool_input: Option<&serde_json::Value>) -> String {
    // Hosts disagree on the path field: Cursor/Claude send `file_path`, some MCP
    // schemas use `path`. Resolve across all of them and remember WHICH field
    // matched so the redirect rewrites the same field the host reads back.
    let Some((path_field, path)) =
        payload::resolve_path_field(tool_input, payload::READ_PATH_FIELDS)
    else {
        debug_log::log_hook_decision(
            "redirect",
            "Read",
            Route::Native,
            "<none>",
            "no path in tool input",
        );
        return build_dual_allow_output();
    };
    if should_passthrough(&path) {
        debug_log::log_hook_decision(
            "redirect",
            "Read",
            Route::Native,
            &path,
            "passthrough path (sensitive/binary/excluded)",
        );
        return build_dual_allow_output();
    }

    let shadow = is_shadow_mode_active();
    if is_harden_active() || shadow {
        tracing::info!(
            "[hook redirect] {} active, redirecting Read through lean-ctx",
            if shadow { "shadow mode" } else { "harden mode" }
        );
    }

    let binary = resolve_binary();
    let temp_path = redirect_temp_path(&path);

    if let Some(output) = run_with_timeout(
        &binary,
        &redirect_read_args(&path),
        REDIRECT_SUBPROCESS_TIMEOUT,
    ) {
        // #1019: never prepend a banner to `output` — it is written to the temp
        // file the host reads *as the file's content*, so an edit would round-trip
        // the banner back into the real file (it corrupted config.toml). The
        // shadow nudge rides the model-visible `additionalContext` side channel
        // instead, and the intercept is still recorded in shadow.log.
        if !output.is_empty() && std::fs::write(&temp_path, &output).is_ok() {
            let temp_str = temp_path.to_str().unwrap_or("");
            debug_log::log_hook_decision(
                "redirect",
                "Read",
                Route::LeanCtx,
                &path,
                "redirected to ctx_read",
            );
            let shadow_note = shadow.then(|| {
                format!(
                    "lean-ctx shadow mode: this Read was served by ctx_read(\"{path}\", \"full\"). Call ctx_read directly for better performance."
                )
            });
            log_shadow_intercept("Read", &path);
            return build_redirect_output(tool_input, path_field, temp_str, shadow_note.as_deref());
        }
    }

    debug_log::log_hook_decision(
        "redirect",
        "Read",
        Route::Native,
        &path,
        "lean-ctx read produced no output",
    );
    build_dual_allow_output()
}

/// Redirect Grep through lean-ctx for compressed results.
/// The Grep redirect rewrites `path` to a temp file the host re-greps, which is
/// only faithful for `output_mode=content` (see [`redirect_grep`]). For
/// `files_with_matches` the host would report the temp file itself as the match,
/// and for `count` it would count lines in the temp file — both wrong. The hook
/// is host-agnostic (Cursor defaults to `content`, Claude Code to
/// `files_with_matches`), so an absent mode cannot be assumed safe: only an
/// explicit `content` mode is redirectable. (GH #398 hook follow-up)
fn grep_content_mode(tool_input: Option<&serde_json::Value>) -> bool {
    tool_input
        .and_then(|ti| ti.get("output_mode"))
        .and_then(|m| m.as_str())
        == Some("content")
}

fn redirect_grep(tool_input: Option<&serde_json::Value>) -> String {
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
        return build_dual_allow_output();
    }

    if !grep_content_mode(tool_input) {
        debug_log::log_hook_decision(
            "redirect",
            "Grep",
            Route::Native,
            &format!("{pattern} in {search_path}"),
            "non-content output_mode — native passthrough (path-swap only valid for content)",
        );
        if is_shadow_mode_active() {
            log_shadow_intercept("Grep", &format!("{pattern} in {search_path}"));
        }
        return build_dual_allow_output();
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

    if let Some(output) = run_with_timeout(
        &binary,
        &["grep", pattern, search_path],
        REDIRECT_SUBPROCESS_TIMEOUT,
    ) {
        // #1019: the temp file is re-grepped by the host, so a banner line would
        // be a spurious match (and skew counts). Keep `output` byte-faithful; the
        // shadow nudge rides `additionalContext`, and shadow.log records it.
        if !output.is_empty() && std::fs::write(&temp_path, &output).is_ok() {
            let temp_str = temp_path.to_str().unwrap_or("");
            debug_log::log_hook_decision(
                "redirect",
                "Grep",
                Route::LeanCtx,
                &format!("{pattern} in {search_path}"),
                "redirected to ctx_search",
            );
            let shadow_note = shadow.then(|| {
                format!(
                    "lean-ctx shadow mode: this Grep was served by ctx_search(\"{pattern}\", \"{search_path}\"). Call ctx_search directly for better performance."
                )
            });
            log_shadow_intercept("Grep", &format!("{pattern} in {search_path}"));
            return build_redirect_output(tool_input, "path", temp_str, shadow_note.as_deref());
        }
    }

    debug_log::log_hook_decision(
        "redirect",
        "Grep",
        Route::Native,
        &format!("{pattern} in {search_path}"),
        "lean-ctx grep produced no output",
    );
    build_dual_allow_output()
}

/// Redirect Glob through lean-ctx in shadow/harden mode (#556).
///
/// Glob differs from Read/Grep: its result is a list of paths matched against
/// the filesystem, not file content, so `build_redirect_output` (which swaps a
/// field to a temp file the host then *reads*) cannot carry it.
///
/// Won't-fix (#1033): a true Read/Grep-style redirect is impossible *by
/// construction*, not merely unimplemented. The host consumes the path list
/// directly and never re-reads a file we could substitute, so there is no
/// redirectable result to rewrite. We therefore only act when shadow or harden
/// mode is active — warm lean-ctx's own glob path (parity with `ctx_glob`) and
/// record the intercept in shadow.log — then allow the native call through
/// unchanged. Outside those modes there is nothing to gain, so we pass through
/// immediately without spawning a subprocess.
fn redirect_glob(tool_input: Option<&serde_json::Value>) -> String {
    let allow = build_dual_allow_output();
    let shadow = is_shadow_mode_active();
    if !shadow && !is_harden_active() {
        return allow;
    }

    let pattern = tool_input
        .and_then(|ti| ti.get("pattern"))
        .and_then(|p| p.as_str())
        .unwrap_or("");
    if pattern.is_empty() {
        debug_log::log_hook_decision(
            "redirect",
            "Glob",
            Route::Native,
            "<none>",
            "no pattern in tool input",
        );
        return allow;
    }

    let search_path = tool_input
        .and_then(|ti| ti.get("path"))
        .and_then(|p| p.as_str())
        .unwrap_or(".");

    tracing::info!(
        "[hook redirect] {} active, warming ctx_glob for {pattern}",
        if shadow { "shadow mode" } else { "harden mode" }
    );

    // Warm lean-ctx's glob path (populates caches, parity with the ctx_glob the
    // shadow header nudges toward); the native result is kept untouched.
    let binary = resolve_binary();
    let _ = run_with_timeout(
        &binary,
        &["glob", pattern, search_path],
        REDIRECT_SUBPROCESS_TIMEOUT,
    );

    debug_log::log_hook_decision(
        "redirect",
        "Glob",
        Route::Native,
        &format!("{pattern} in {search_path}"),
        "shadow/harden warm — native passthrough",
    );
    log_shadow_intercept("Glob", &format!("{pattern} in {search_path}"));
    allow
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
    shadow_note: Option<&str>,
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

    // Claude Code / CodeBuddy hook output format (other hosts ignore it).
    let mut hook_specific = serde_json::json!({
        "hookEventName": "PreToolUse",
        "permissionDecision": "allow",
        "updatedInput": updated_input.clone(),
    });
    // #1019: the shadow nudge travels here, not inside the file content. Hosts
    // that honor it (Claude Code / Codex) surface it as model-visible context;
    // others ignore it. Either way the temp file the host reads stays faithful.
    if let Some(note) = shadow_note {
        hook_specific["additionalContext"] = serde_json::Value::String(note.to_string());
    }

    serde_json::json!({
        // Cursor hook output format.
        "permission": "allow",
        "updated_input": updated_input.clone(),
        // GitHub Copilot CLI preToolUse format: top-level `permissionDecision`
        // + `modifiedArgs` (full substitute args) so the read/grep redirect to
        // the lean-ctx temp file actually takes effect on Copilot (#551).
        "permissionDecision": "allow",
        "modifiedArgs": updated_input.clone(),
        "hookSpecificOutput": hook_specific
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

/// Dedicated Copilot PreToolUse handler (dispatched via `hook copilot`).
///
/// NOTE: the live Copilot CLI integration installed by `init --agent copilot`
/// registers `hook rewrite` + `hook redirect` (see `hooks::agents::copilot`),
/// so this entry point is currently unused by setup. It is kept correct for any
/// host wired to `hook copilot` directly. It parses the same normalised payload
/// as the other handlers so Copilot CLI's camelCase `toolName`/`toolArgs`
/// (JSON-encoded string) are read correctly (#551).
pub fn handle_copilot() {
    if is_disabled() {
        return;
    }
    let binary = resolve_binary();
    let Some(input) = read_stdin_with_timeout(HOOK_STDIN_TIMEOUT) else {
        return;
    };

    let Ok(v) = serde_json::from_str::<serde_json::Value>(&input) else {
        return;
    };

    let Some(tool_name) = payload::resolve_tool_name(&v) else {
        return;
    };

    if !is_shell_tool(&tool_name) {
        return;
    }

    let tool_args = payload::resolve_tool_args(&v);
    let Some(cmd) = payload::resolve_command(&v, tool_args.as_ref()) else {
        return;
    };

    if let Some(rewritten) = rewrite_candidate(&cmd, &binary) {
        print!(
            "{}",
            build_dual_rewrite_output(tool_args.as_ref(), &rewritten)
        );
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
