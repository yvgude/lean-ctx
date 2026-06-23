//! Opt-in human-readable debug log of intercepted tool activity (#520).
//!
//! Off by default. Enable with the `LEAN_CTX_DEBUG_LOG` env var (truthy) or the
//! `debug_log = true` config key. Records two kinds of events to
//! `<state_dir>/logs/debug.log`:
//!
//! 1. **MCP tool calls** handled by the lean-ctx server (`ctx_*`) — tool name, a
//!    redacted argument summary, a one-line result preview, byte size, token
//!    savings and wall time.
//! 2. **Hook routing decisions** — for every native tool call lean-ctx *can*
//!    intercept (shell / Read / Grep), whether it was routed to `lean-ctx` or
//!    left to the editor's **native** tool, and *why*. This is #520's core ask:
//!    explain why one call used lean-ctx and the next fell back to the native
//!    Read/Grep tool.
//!
//! All writes are best-effort and never panic — logging must never break a hook
//! subprocess or a tool call. Secrets in arguments/commands/results are scrubbed
//! via [`crate::core::redaction::redact_text`] before they hit disk.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value};

/// Rotate once the log crosses this size, keeping a single `.1` backup so the
/// file cannot grow without bound on a long-lived daemon.
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// Clamp any single field rendered into a log line so one huge argument or
/// result cannot dominate the file (or leak a large blob). Bytes; clamped on a
/// UTF-8 char boundary.
const FIELD_CLAMP: usize = 200;

/// Where a hook sent an intercepted native tool call.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Route {
    /// Rewritten / redirected through lean-ctx (compression + caching).
    LeanCtx,
    /// Left to the editor's native tool.
    Native,
}

impl Route {
    fn label(self) -> &'static str {
        match self {
            Route::LeanCtx => "lean-ctx",
            Route::Native => "native",
        }
    }
}

/// Whether the opt-in debug log is active.
///
/// The env var wins over config so a user can flip it per-session
/// (`LEAN_CTX_DEBUG_LOG=1`) without editing config; the `debug_log` config key
/// makes it persistent and is what hook subprocesses read when the IDE does not
/// export the env var into the hook environment.
#[must_use]
pub fn is_enabled() -> bool {
    if let Ok(v) = std::env::var("LEAN_CTX_DEBUG_LOG") {
        let v = v.trim().to_ascii_lowercase();
        return !matches!(v.as_str(), "" | "0" | "false" | "off" | "no");
    }
    crate::core::config::Config::load().debug_log
}

/// `<state_dir>/logs/debug.log`. Returns `None` if the state dir cannot be
/// resolved or the `logs/` directory cannot be created.
#[must_use]
pub fn log_path() -> Option<PathBuf> {
    let dir = crate::core::paths::state_dir().ok()?.join("logs");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("debug.log"))
}

/// Record an MCP tool call handled by the lean-ctx server.
pub fn log_mcp_call(
    tool: &str,
    args: Option<&Map<String, Value>>,
    result_first_line: &str,
    result_bytes: usize,
    saved_tokens: usize,
    elapsed: Duration,
) {
    if !is_enabled() {
        return;
    }
    let args_summary = summarize_args(args);
    let preview = clamp(&redact(result_first_line));
    append(&format!(
        "mcp  {tool}({args_summary}) -> {preview} [{result_bytes}B, saved≈{saved_tokens} tok, {}ms]",
        elapsed.as_millis()
    ));
}

/// Record an MCP tool call that failed before producing a result.
pub fn log_mcp_error(tool: &str, args: Option<&Map<String, Value>>, error: &str) {
    if !is_enabled() {
        return;
    }
    let args_summary = summarize_args(args);
    append(&format!(
        "mcp  {tool}({args_summary}) -> ERROR: {}",
        clamp(&redact(error))
    ));
}

/// Record a hook routing decision for an intercepted native tool call.
///
/// `subject` is the command / path / pattern that was inspected; `reason`
/// explains the choice (e.g. `"rewritable shell command"`, `"sensitive path"`).
pub fn log_hook_decision(event: &str, tool: &str, route: Route, subject: &str, reason: &str) {
    if !is_enabled() {
        return;
    }
    append(&format!(
        "hook {event} {tool} -> {} ({reason}): {}",
        route.label(),
        clamp(&redact(subject))
    ));
}

/// Return the log content for display (most-recent `tail_lines`, `0` = all).
#[must_use]
pub fn read_log(tail_lines: usize) -> String {
    let Some(path) = log_path() else {
        return "Debug log unavailable (state dir not resolvable).".to_string();
    };
    if !path.exists() {
        return format!(
            "No debug-log entries yet. Enable with `LEAN_CTX_DEBUG_LOG=1` or \
             `lean-ctx config set debug_log true`, then re-run your tool calls.\nPath: {}",
            path.display()
        );
    }
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    if tail_lines == 0 {
        return content;
    }
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(tail_lines);
    lines[start..].join("\n")
}

/// Delete the debug log (and its rotated backup). Returns a status line.
#[must_use]
pub fn clear() -> String {
    let Some(path) = log_path() else {
        return "Debug log unavailable (state dir not resolvable).".to_string();
    };
    let mut removed = 0u32;
    for p in [path.clone(), rotated_path(&path)] {
        if p.exists() && std::fs::remove_file(&p).is_ok() {
            removed += 1;
        }
    }
    format!(
        "Cleared {removed} debug-log file(s) from {}",
        path.display()
    )
}

// ---- internals -------------------------------------------------------------

fn redact(s: &str) -> String {
    crate::core::redaction::redact_text(s)
}

/// Rotated-backup path for `debug.log` → `debug.log.1`.
fn rotated_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".1");
    path.with_file_name(name)
}

/// Pure rotation predicate (unit-testable without writing `MAX_LOG_BYTES`).
fn should_rotate(len: u64) -> bool {
    len > MAX_LOG_BYTES
}

/// Collapse newlines to a single visible marker and clamp to [`FIELD_CLAMP`]
/// bytes on a char boundary so every event stays on exactly one line.
fn clamp(s: &str) -> String {
    let one_line = s.replace('\n', "⏎").replace('\r', "");
    if one_line.len() <= FIELD_CLAMP {
        return one_line;
    }
    let mut end = FIELD_CLAMP;
    while end > 0 && !one_line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &one_line[..end])
}

/// `key="val", key2=42` with string values redacted + clamped and keys sorted
/// for stable output. Structural args (action / mode / path / pattern / command)
/// are the useful signal; long or secret-bearing values stay short.
fn summarize_args(args: Option<&Map<String, Value>>) -> String {
    let Some(map) = args else {
        return String::new();
    };
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort();
    let summary = keys
        .iter()
        .map(|k| {
            let rendered = match map.get(*k) {
                Some(Value::String(s)) => format!("{:?}", clamp(&redact(s))),
                Some(other) => clamp(&other.to_string()),
                None => String::new(),
            };
            format!("{k}={rendered}")
        })
        .collect::<Vec<_>>()
        .join(", ");
    clamp(&summary)
}

fn rotate_if_large(path: &Path) {
    if let Ok(meta) = std::fs::metadata(path)
        && should_rotate(meta.len())
    {
        let _ = std::fs::rename(path, rotated_path(path));
    }
}

fn append(message: &str) {
    let Some(path) = log_path() else {
        return;
    };
    rotate_if_large(&path);
    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let line = format!("{ts} {message}\n");
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| f.write_all(line.as_bytes()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_guard() {
        crate::test_env::set_var("LEAN_CTX_DEBUG_LOG", "1");
    }
    fn disable_guard() {
        crate::test_env::remove_var("LEAN_CTX_DEBUG_LOG");
    }

    #[test]
    fn disabled_by_default_writes_nothing() {
        let iso = crate::core::data_dir::isolated_data_dir();
        disable_guard();
        // Config default is `debug_log = false`, so nothing should be written.
        log_mcp_call("ctx_read", None, "hello", 5, 0, Duration::from_millis(1));
        let path = iso.path().join("logs").join("debug.log");
        assert!(!path.exists(), "debug.log must not exist when disabled");
    }

    #[test]
    fn env_enables_and_records_mcp_call() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        enabled_guard();
        let mut args = Map::new();
        args.insert("path".into(), Value::String("src/main.rs".into()));
        args.insert("mode".into(), Value::String("full".into()));

        log_mcp_call(
            "ctx_read",
            Some(&args),
            "first line of result",
            1234,
            87,
            Duration::from_millis(12),
        );

        let content = std::fs::read_to_string(log_path().unwrap()).unwrap();
        assert!(content.contains("mcp  ctx_read("), "tool name + marker");
        assert!(content.contains("path=\"src/main.rs\""), "arg summary");
        assert!(content.contains("mode=\"full\""), "arg summary");
        assert!(content.contains("saved≈87 tok"), "savings");
        assert!(content.contains("1234B"), "byte size");
        disable_guard();
    }

    #[test]
    fn redacts_secrets_in_args_and_results() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        enabled_guard();
        let secret = "token=ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let mut args = Map::new();
        args.insert("command".into(), Value::String(secret.into()));

        log_mcp_call("ctx_shell", Some(&args), secret, 10, 0, Duration::ZERO);

        let content = std::fs::read_to_string(log_path().unwrap()).unwrap();
        assert!(content.contains("[REDACTED"), "secret must be redacted");
        assert!(
            !content.contains("ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"),
            "raw secret must not be written"
        );
        disable_guard();
    }

    #[test]
    fn hook_decision_records_route_and_reason() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        enabled_guard();

        log_hook_decision(
            "rewrite",
            "Bash",
            Route::LeanCtx,
            "cat foo.rs",
            "rewritable",
        );
        log_hook_decision(
            "redirect",
            "Read",
            Route::Native,
            "/etc/passwd",
            "sensitive path",
        );

        let content = std::fs::read_to_string(log_path().unwrap()).unwrap();
        assert!(content.contains("hook rewrite Bash -> lean-ctx (rewritable): cat foo.rs"));
        assert!(content.contains("hook redirect Read -> native (sensitive path): /etc/passwd"));
        disable_guard();
    }

    #[test]
    fn clamp_truncates_long_fields_on_char_boundary() {
        let long = "x".repeat(FIELD_CLAMP + 50);
        let out = clamp(&long);
        assert!(out.len() <= FIELD_CLAMP + 4, "clamped near FIELD_CLAMP");
        assert!(out.ends_with('…'), "clamp marker appended");

        let multiline = "line1\nline2";
        assert_eq!(clamp(multiline), "line1⏎line2", "newlines collapsed");
    }

    #[test]
    fn should_rotate_predicate() {
        assert!(!should_rotate(0));
        assert!(!should_rotate(MAX_LOG_BYTES));
        assert!(should_rotate(MAX_LOG_BYTES + 1));
    }

    #[test]
    fn rotates_when_file_exceeds_max() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        enabled_guard();
        let path = log_path().unwrap();
        // Sparse file of MAX+1 bytes: metadata().len() reports the size without
        // actually writing 5 MiB to disk.
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(MAX_LOG_BYTES + 1).unwrap();
        drop(f);

        log_mcp_call("ctx_read", None, "after rotation", 14, 0, Duration::ZERO);

        assert!(rotated_path(&path).exists(), "backup debug.log.1 created");
        let fresh = std::fs::read_to_string(&path).unwrap();
        assert!(fresh.contains("after rotation"), "new log started");
        disable_guard();
    }
}
