//! Opt-in `lean-ctx:` commit trailer.
//!
//! `lean-ctx init --git-trailer` installs a `prepare-commit-msg` hook in the
//! current repository (honouring `core.hooksPath`) and turns on
//! `[value_display] git_trailer`. On each commit the hook runs the hidden
//! `lean-ctx git-trailer <msg-file> [source]`, which appends e.g.
//!
//! ```text
//! lean-ctx: 840.0K tokens saved, 1 secret kept out of context
//! ```
//!
//! The numbers are what lean-ctx measured in this project *since the previous
//! trailer* of the same session, so two commits never both claim the same
//! savings. The hook can never fail a commit: any error means no trailer.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::core::config::{Config, ValueDisplayMode};
use crate::core::security_events::SecurityCounts;
use crate::core::value::{format, snapshot};
use crate::core::wrapped::format_tokens;

const MARKER: &str = "# lean-ctx git trailer";
const KEY: &str = "lean-ctx";
/// A snapshot older than this belongs to other work than this commit.
const MAX_AGE: Duration = Duration::from_hours(12);

/// What the previous trailer in a project already reported.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Reported {
    session_id: String,
    tokens_saved: u64,
    security: SecurityCounts,
}

/// `init --git-trailer` (`enable`) or `init --git-trailer off`.
pub(crate) fn cmd_init_git_trailer(enable: bool) {
    let Some(hook) = hook_path() else {
        eprintln!(
            "Not inside a git repository — run this in the repository to add the trailer to."
        );
        std::process::exit(1);
    };
    let existing = std::fs::read_to_string(&hook).ok();
    let ours = existing.as_deref().is_some_and(|c| c.contains(MARKER));
    if !enable {
        if ours {
            if let Err(e) = std::fs::remove_file(&hook) {
                eprintln!("Cannot remove {}: {e}", hook.display());
                std::process::exit(1);
            }
            println!("✓ lean-ctx commit trailer removed from this repository");
        } else {
            println!("No lean-ctx commit trailer hook in this repository.");
        }
        return;
    }
    if existing.is_some() && !ours {
        println!(
            "This repository already has a prepare-commit-msg hook:\n  {}\n",
            hook.display()
        );
        println!("lean-ctx leaves it untouched. To add the trailer, append this line to it:\n");
        println!("  lean-ctx git-trailer \"$1\" \"$2\" || true\n");
        enable_config();
        return;
    }
    let binary =
        crate::hooks::to_bash_compatible_path(&crate::core::portable_binary::stable_shell_binary(
            &crate::core::portable_binary::resolve_portable_binary(),
        ));
    if let Some(parent) = hook.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&hook, hook_script(&binary)) {
        eprintln!("Cannot write {}: {e}", hook.display());
        std::process::exit(1);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755));
    }
    enable_config();
    println!("◆ lean-ctx commit trailer on for this repository");
    println!("  Commits get e.g. `lean-ctx: 840.0K tokens saved, 1 secret kept out of context`");
    println!("  — only what was measured since the previous trailer. Proof: lean-ctx value");
    println!("  Remove: lean-ctx init --git-trailer off");
}

fn enable_config() {
    if !Config::load().value_display.git_trailer
        && let Err(e) = crate::core::config::setter::set_by_key("value_display.git_trailer", "true")
    {
        eprintln!("Cannot enable value_display.git_trailer: {e}");
    }
}

/// `prepare-commit-msg` for this repository, honouring `core.hooksPath`.
fn hook_path() -> Option<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--git-path", "hooks/prepare-commit-msg"])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let rel = String::from_utf8(out.stdout).ok()?;
    let rel = Path::new(rel.trim());
    Some(if rel.is_absolute() {
        rel.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(rel)
    })
}

fn hook_script(binary: &str) -> String {
    let bin = format!("'{}'", binary.replace('\'', r"'\''"));
    format!(
        "#!/bin/sh\n\
         {MARKER} — written by `lean-ctx init --git-trailer`.\n\
         # Remove with `lean-ctx init --git-trailer off`. Never fails a commit.\n\
         [ -x {bin} ] || exit 0\n\
         {bin} git-trailer \"$1\" \"$2\" >/dev/null 2>&1 || true\n\
         exit 0\n"
    )
}

/// The hidden hook entry point: `lean-ctx git-trailer <msg-file> [source]`.
pub(crate) fn cmd_git_trailer(args: &[String]) {
    let Some(msg_file) = args.first() else {
        return;
    };
    let source = args.get(1).map_or("", String::as_str);
    // Merges, squashes and amends (`-c`/`-C`/`--amend`) carry a message that
    // is not this commit's work.
    if matches!(source, "merge" | "squash" | "commit") {
        return;
    }
    let cfg = Config::load_arc();
    if !cfg.value_display.git_trailer || cfg.value_display.effective_mode() == ValueDisplayMode::Off
    {
        return;
    }
    let (Some(dir), Ok(cwd)) = (snapshot::value_dir(), std::env::current_dir()) else {
        return;
    };
    let Some(snap) = snapshot::load_for_dir_in(&dir, &cwd).filter(|s| s.is_fresh(MAX_AGE)) else {
        return;
    };
    let Some(root) = snap.project_root.clone() else {
        return;
    };
    let state_path = state_path(&dir, &root);
    let last = std::fs::read(&state_path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Reported>(&b).ok());
    let (tokens, security) = unreported(&snap, last.as_ref());
    let Some(value) = trailer_value(tokens, &security) else {
        return;
    };
    let added = Command::new("git")
        .args(["interpret-trailers", "--in-place", "--if-exists", "replace"])
        .arg("--trailer")
        .arg(format!("{KEY}: {value}"))
        .arg(msg_file)
        .status()
        .is_ok_and(|s| s.success());
    if !added {
        return;
    }
    // Recorded before the commit completes: an aborted commit under-reports
    // the next trailer, it never double-counts.
    let reported = Reported {
        session_id: snap.session_id.clone(),
        tokens_saved: snap.tokens_saved,
        security: snap.security,
    };
    if let (Some(parent), Ok(json)) = (state_path.parent(), serde_json::to_vec(&reported)) {
        let _ = std::fs::create_dir_all(parent);
        let _ = crate::core::atomic_fs::try_atomic_write(&state_path, &json, None);
    }
}

fn state_path(dir: &Path, root: &str) -> PathBuf {
    let project = snapshot::project_path(dir, root);
    let name = project
        .file_name()
        .map(ToOwned::to_owned)
        .unwrap_or_default();
    dir.join("trailers").join(name)
}

/// What `snap` measured that no earlier trailer reported. A new session (or a
/// counter that went backwards) starts over from the session's own totals.
fn unreported(snap: &snapshot::ValueSnapshot, last: Option<&Reported>) -> (u64, SecurityCounts) {
    match last {
        Some(last)
            if last.session_id == snap.session_id && snap.tokens_saved >= last.tokens_saved =>
        {
            (
                snap.tokens_saved - last.tokens_saved,
                snap.security.since(&last.security),
            )
        }
        _ => (snap.tokens_saved, snap.security),
    }
}

/// `840.0K tokens saved, 1 secret kept out of context` — plain text, since
/// commit messages outlive every terminal's glyph support.
fn trailer_value(tokens: u64, security: &SecurityCounts) -> Option<String> {
    let mut parts = Vec::new();
    if tokens > 0 {
        parts.push(format!("{} tokens saved", format_tokens(tokens)));
    }
    parts.extend(format::security_phrases(security));
    (!parts.is_empty()).then(|| parts.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(session: &str, saved: u64, secrets: u64) -> snapshot::ValueSnapshot {
        let mut s = snapshot::ValueSnapshot {
            session_id: session.into(),
            tokens_saved: saved,
            ..snapshot::ValueSnapshot::default()
        };
        s.security.secrets_redacted = secrets;
        s
    }

    fn reported(session: &str, saved: u64, secrets: u64) -> Reported {
        let s = snap(session, saved, secrets);
        Reported {
            session_id: s.session_id,
            tokens_saved: s.tokens_saved,
            security: s.security,
        }
    }

    #[test]
    fn trailer_reads_as_plain_text() {
        let sec = SecurityCounts {
            secrets_redacted: 1,
            ..Default::default()
        };
        assert_eq!(
            trailer_value(840_000, &sec).unwrap(),
            "840.0K tokens saved, 1 secret kept out of context"
        );
        assert_eq!(trailer_value(0, &SecurityCounts::default()), None);
    }

    #[test]
    fn a_second_commit_only_claims_what_is_new() {
        let first = unreported(&snap("s1", 800_000, 1), None);
        assert_eq!(first.0, 800_000);
        let second = unreported(&snap("s1", 900_000, 1), Some(&reported("s1", 800_000, 1)));
        assert_eq!(second.0, 100_000);
        assert!(second.1.is_empty());
        let nothing = unreported(&snap("s1", 900_000, 1), Some(&reported("s1", 900_000, 1)));
        assert_eq!(trailer_value(nothing.0, &nothing.1), None);
    }

    #[test]
    fn a_new_session_starts_from_its_own_totals() {
        let fresh = unreported(&snap("s2", 50_000, 0), Some(&reported("s1", 900_000, 3)));
        assert_eq!(fresh, (50_000, SecurityCounts::default()));
    }

    #[test]
    fn the_hook_never_fails_a_commit() {
        let script = hook_script("/opt/it's/lean-ctx");
        assert!(script.starts_with("#!/bin/sh\n"));
        assert!(script.contains(MARKER));
        assert!(script.contains(r"'/opt/it'\''s/lean-ctx' git-trailer"));
        assert!(script.contains("|| true"));
        assert!(script.trim_end().ends_with("exit 0"));
    }

    /// End to end against real git: the trailer lands after the message body
    /// and before git's comment block.
    #[cfg(unix)]
    #[test]
    fn interpret_trailers_places_the_trailer() {
        let dir = tempfile::tempdir().unwrap();
        let msg = dir.path().join("COMMIT_EDITMSG");
        std::fs::write(&msg, "fix: a thing\n\n# Please enter the commit message\n").unwrap();
        let ok = Command::new("git")
            .current_dir(dir.path())
            .args(["interpret-trailers", "--in-place", "--if-exists", "replace"])
            .arg("--trailer")
            .arg("lean-ctx: 1.0K tokens saved")
            .arg(&msg)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            return; // no git on this machine
        }
        let out = std::fs::read_to_string(&msg).unwrap();
        assert!(
            out.starts_with("fix: a thing\n\nlean-ctx: 1.0K tokens saved\n"),
            "{out}"
        );
    }
}
