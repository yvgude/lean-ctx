//! Native git support: remote-repo reading (cached shallow clone) and a shadow
//! history of agent changes.
//!
//! * [`repo_url`] parses repository URLs into a [`repo_url::RepoRef`].
//! * [`clone`] maintains a bounded, SSRF-guarded local clone cache.
//! * [`shadow`] records agent edits in a git history kept *outside* the user's
//!   own `.git`.
//!
//! All git invocations go through [`run_git`], which never uses a shell, always
//! disables interactive credential prompts (so a private/auth-required remote
//! fails fast instead of hanging), and enforces a wall-clock timeout.

pub mod clone;
pub mod repo_url;
pub mod shadow;

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Result of a single `git` invocation.
#[derive(Debug, Clone)]
pub struct GitOutput {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

impl GitOutput {
    /// Stdout if the command succeeded, else an error carrying trimmed stderr.
    pub fn ok_stdout(self) -> Result<String, String> {
        if self.success {
            Ok(self.stdout)
        } else {
            let msg = self.stderr.trim();
            Err(if msg.is_empty() {
                "git command failed".to_string()
            } else {
                msg.to_string()
            })
        }
    }
}

/// `true` if a `git` binary is callable.
#[must_use]
pub fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Run `git <args>` in `cwd` with extra env vars and a wall-clock `timeout`.
///
/// No shell is involved (args are passed directly). Interactive credential
/// prompts are disabled so an auth-required remote errors immediately rather
/// than blocking. stdout/stderr are drained on dedicated threads to avoid
/// pipe-buffer deadlock on chatty commands (e.g. `clone` progress).
pub fn run_git(
    args: &[&str],
    cwd: &Path,
    timeout: Duration,
    env: &[(&str, &str)],
) -> Result<GitOutput, String> {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Fail fast instead of prompting for credentials / hanging.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_ASKPASS", "")
        .env("SSH_ASKPASS", "");
    for (k, v) in env {
        cmd.env(k, v);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to start git (is it installed?): {e}"))?;

    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let out_handle = std::thread::spawn(move || drain(out_pipe.as_mut()));
    let err_handle = std::thread::spawn(move || drain(err_pipe.as_mut()));

    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("git timed out after {}s", timeout.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            Err(e) => return Err(format!("git wait failed: {e}")),
        }
    };

    let stdout = out_handle.join().unwrap_or_default();
    let stderr = err_handle.join().unwrap_or_default();
    Ok(GitOutput {
        stdout,
        stderr,
        success: status.success(),
    })
}

fn drain(pipe: Option<&mut impl Read>) -> String {
    let mut buf = String::new();
    if let Some(p) = pipe {
        let _ = p.read_to_string(&mut buf);
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_version_runs() {
        if !git_available() {
            return; // CI without git — nothing to assert
        }
        let out = run_git(&["--version"], Path::new("."), Duration::from_secs(5), &[])
            .expect("git --version");
        assert!(out.success);
        assert!(out.stdout.to_lowercase().contains("git version"));
    }

    #[test]
    fn failed_command_surfaces_stderr() {
        if !git_available() {
            return;
        }
        let out = run_git(
            &["rev-parse", "--verify", "definitely-not-a-ref"],
            Path::new("."),
            Duration::from_secs(5),
            &[],
        )
        .expect("git should run");
        assert!(!out.success);
        assert!(out.ok_stdout().is_err());
    }
}
