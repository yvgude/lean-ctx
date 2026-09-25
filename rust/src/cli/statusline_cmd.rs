//! `lean-ctx statusline`: the Claude Code status line (`statusLine.command`).
//!
//! Reads the host's JSON from stdin, finds the lean-ctx snapshot of the
//! project it is working in and prints one dim line — nothing when there is
//! nothing measured, the snapshot is stale, or it predates the conversation.
//! `--wrap "<cmd>"` keeps a status line the user already had: it gets the same
//! stdin, and lean-ctx's segment is appended to its first line.

use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::core::config::{Config, ValueDisplayMode};
use crate::core::value::{format, recap, snapshot};

/// Older than this, the status line shows nothing rather than an old number.
const MAX_AGE: Duration = Duration::from_hours(12);
/// A wrapped status line that takes longer than this is dropped for this render.
const WRAP_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) fn cmd_statusline(args: &[String]) {
    if args.iter().any(|a| matches!(a.as_str(), "-h" | "--help")) {
        usage();
        return;
    }
    let wrap = match args {
        [] => None,
        [flag, cmd] if flag == "--wrap" => Some(cmd.as_str()),
        [arg] if arg.starts_with("--wrap=") => arg.strip_prefix("--wrap="),
        _ => {
            usage();
            std::process::exit(2);
        }
    };
    let mut input = Vec::new();
    if !std::io::stdin().is_terminal() {
        let _ = std::io::stdin().take(1 << 20).read_to_end(&mut input);
    }
    let wrapped = wrap.and_then(|cmd| run_wrapped(cmd, &input));
    let ours = segment(&input);
    let out = combine(wrapped.as_deref(), ours.as_deref());
    if !out.is_empty() {
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(stdout, "{out}");
    }
}

/// lean-ctx's own segment for this host payload.
fn segment(input: &[u8]) -> Option<String> {
    if Config::load_arc().value_display.effective_mode() == ValueDisplayMode::Off {
        return None;
    }
    let payload: serde_json::Value = serde_json::from_slice(input).unwrap_or_default();
    let dir = snapshot::value_dir()?;
    let cwd = ["/workspace/current_dir", "/cwd", "/workspace/project_dir"]
        .iter()
        .find_map(|ptr| payload.pointer(ptr).and_then(serde_json::Value::as_str))
        .filter(|cwd| !cwd.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())?;
    let snap = snapshot::load_for_dir_in(&dir, &cwd).filter(|s| s.is_fresh(MAX_AGE))?;
    // A snapshot not touched since this conversation began belongs to an
    // earlier one; it would read as this conversation's savings.
    let host_session = payload
        .get("session_id")
        .and_then(serde_json::Value::as_str);
    if let Some(state) = host_session.and_then(|id| recap::load_turn_state(&dir, id))
        && let (Some(updated), Some(created)) = (snap.updated_at, state.created_at)
        && updated < created
    {
        return None;
    }
    // Status lines render ANSI but are not a TTY: colour follows NO_COLOR only.
    format::one_line(&snap, format::Style::from_env())
}

/// The wrapped line's first row, then ours; its further rows stay below.
fn combine(wrapped: Option<&str>, ours: Option<&str>) -> String {
    let wrapped = wrapped
        .map(|w| w.trim_end_matches(['\n', '\r']))
        .unwrap_or("");
    let Some(ours) = ours else {
        return wrapped.to_string();
    };
    match wrapped.split_once('\n') {
        _ if wrapped.trim().is_empty() => ours.to_string(),
        Some((first, rest)) => format!("{}  {ours}\n{rest}", first.trim_end_matches('\r')),
        None => format!("{wrapped}  {ours}"),
    }
}

fn run_wrapped(cmd: &str, input: &[u8]) -> Option<String> {
    let mut child = shell(cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input);
    }
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    if let Ok(buf) = rx.recv_timeout(WRAP_TIMEOUT) {
        let _ = child.wait();
        Some(String::from_utf8_lossy(&buf).into_owned())
    } else {
        let _ = child.kill();
        let _ = child.wait();
        None
    }
}

#[cfg(windows)]
fn shell(cmd: &str) -> Command {
    let mut c = Command::new("cmd");
    c.args(["/C", cmd]);
    c
}

#[cfg(not(windows))]
fn shell(cmd: &str) -> Command {
    let mut c = Command::new("sh");
    c.args(["-c", cmd]);
    c
}

fn usage() {
    println!(
        "Claude Code status line: what lean-ctx did in this project's session.\n\n\
         Reads the host's status-line JSON from stdin. Prints nothing when nothing\n\
         was measured yet or the numbers are stale. `lean-ctx value` proves them.\n\n\
         Usage: lean-ctx statusline [--wrap \"<command>\"]\n\n\
         Options:\n  \
           --wrap <command>  run your existing status line too; lean-ctx's segment\n                    \
           is appended to its first line\n"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combine_appends_to_the_first_wrapped_line() {
        assert_eq!(combine(Some("main ✓\n"), Some("◆ x")), "main ✓  ◆ x");
        assert_eq!(
            combine(Some("line1\nline2\n"), Some("◆ x")),
            "line1  ◆ x\nline2"
        );
        assert_eq!(combine(Some("mine\n"), None), "mine");
        assert_eq!(combine(Some("  \n"), Some("◆ x")), "◆ x");
        assert_eq!(combine(None, Some("◆ x")), "◆ x");
        assert_eq!(combine(None, None), "");
    }

    #[cfg(unix)]
    #[test]
    fn wrapped_command_gets_the_same_stdin() {
        let out = run_wrapped("tr a-z A-Z", b"{\"model\":\"x\"}").unwrap();
        assert_eq!(out, "{\"MODEL\":\"X\"}");
    }

    #[cfg(unix)]
    #[test]
    fn a_hanging_wrapped_command_is_dropped() {
        let started = std::time::Instant::now();
        assert_eq!(run_wrapped("exec sleep 10", b""), None);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn an_unrelated_directory_shows_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let payload = serde_json::json!({
            "session_id": "s",
            "workspace": { "current_dir": dir.path() }
        });
        assert_eq!(segment(payload.to_string().as_bytes()), None);
    }
}
