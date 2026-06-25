//! Frictionless share helpers: copy text to the OS clipboard and open files/URLs.
//!
//! Pure process orchestration — no third-party crates. Every function degrades
//! gracefully (returns `false`) when no suitable tool exists, so callers can fall
//! back to printing the text. Used by the Wrapped share flow (`gain --copy` / `--open`).

use std::io::Write;
use std::process::{Command, Stdio};

/// Copies `text` to the system clipboard. Returns `true` on success.
///
/// Tries the platform-native tools in order; the first that accepts the text on
/// stdin and exits 0 wins. Never panics — returns `false` when none are available
/// so the caller can fall back to printing.
#[must_use]
pub fn copy_to_clipboard(text: &str) -> bool {
    clipboard_commands()
        .into_iter()
        .any(|(bin, args)| pipe_to(bin, &args, text))
}

/// Opens `target` (a file path or URL) in the default handler. Returns `true` if
/// the launcher process spawned successfully (not whether the GUI actually opened).
#[must_use]
pub fn open_in_browser(target: &str) -> bool {
    let (bin, mut args): (&str, Vec<&str>) = if cfg!(target_os = "macos") {
        ("open", vec![])
    } else if cfg!(target_os = "windows") {
        // `start` is a cmd builtin; the empty "" is the window-title placeholder.
        ("cmd", vec!["/C", "start", ""])
    } else {
        ("xdg-open", vec![])
    };
    args.push(target);
    Command::new(bin)
        .args(&args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .is_ok()
}

/// Platform-ordered list of `(binary, args)` candidates that read clipboard text on stdin.
fn clipboard_commands() -> Vec<(&'static str, Vec<&'static str>)> {
    #[cfg(target_os = "macos")]
    {
        vec![("pbcopy", vec![])]
    }
    #[cfg(target_os = "windows")]
    {
        vec![("clip", vec![])]
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        vec![
            ("wl-copy", vec![]),
            ("xclip", vec!["-selection", "clipboard"]),
            ("xsel", vec!["--clipboard", "--input"]),
            ("clip.exe", vec![]), // WSL fallback
        ]
    }
}

/// Spawns `bin args`, writes `text` to its stdin, and reports whether it exited 0.
fn pipe_to(bin: &str, args: &[&str], text: &str) -> bool {
    let Ok(mut child) = Command::new(bin)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    // Scope the stdin handle so it is flushed and closed before we wait, otherwise
    // tools that read to EOF (pbcopy, xclip) would block forever.
    {
        let Some(mut stdin) = child.stdin.take() else {
            return false;
        };
        if stdin.write_all(text.as_bytes()).is_err() {
            return false;
        }
    }
    matches!(child.wait(), Ok(status) if status.success())
}

#[cfg(test)]
mod tests {
    use super::clipboard_commands;

    #[test]
    fn clipboard_candidates_are_present_for_this_platform() {
        // Every supported platform must offer at least one clipboard tool to try.
        assert!(!clipboard_commands().is_empty());
    }

    #[test]
    fn clipboard_candidate_binaries_are_non_empty() {
        for (bin, _) in clipboard_commands() {
            assert!(!bin.is_empty(), "clipboard binary name must not be empty");
        }
    }
}
