// SPDX-License-Identifier: Apache-2.0

//! Would Gemini CLI have run this shell command without asking?
//!
//! A Gemini BeforeTool hook cannot approve anything: the scheduler acts only
//! on `ask` and `deny`/`block`, and runs its policy check and confirmation on
//! the hook-modified arguments (gemini-cli `scheduler/scheduler.ts`,
//! `scheduler/hook-utils.ts`; geminicli.com/docs/hooks/reference). A rewrite
//! therefore never grants, but it can add a prompt: when the user allowed the
//! original command (`tools.allowed` entries such as
//! `run_shell_command(git status)`, or a policy-engine rule), the rewritten
//! `lean-ctx -c '…'` no longer matches and Gemini asks. For such a command the
//! hook passes the call through unchanged. The same holds for restrictions:
//! a `tools.exclude` entry or a `tools.core` allowlist for the shell tool
//! names the original command, which a rewrite would no longer match, so the
//! call is left unchanged there too. Unreadable settings and policy files
//! that mention the shell tool count as "may be allowed".

use super::safe_commands::parse_strict;
use std::path::{Path, PathBuf};

const SHELL_TOOLS: &[&str] = &["run_shell_command", "ShellTool"];

fn system_dir() -> PathBuf {
    if cfg!(target_os = "macos") {
        PathBuf::from("/Library/Application Support/GeminiCli")
    } else if cfg!(windows) {
        PathBuf::from(r"C:\ProgramData\gemini-cli")
    } else {
        PathBuf::from("/etc/gemini-cli")
    }
}

/// Settings files Gemini merges (system defaults, user, project, system).
fn settings_files(project_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for (var, name) in [
        ("GEMINI_CLI_SYSTEM_DEFAULTS_PATH", "system-defaults.json"),
        ("GEMINI_CLI_SYSTEM_SETTINGS_PATH", "settings.json"),
    ] {
        files.push(std::env::var_os(var).map_or_else(|| system_dir().join(name), PathBuf::from));
    }
    if let Some(home) = dirs::home_dir() {
        files.push(home.join(".gemini").join("settings.json"));
    }
    if let Some(dir) = project_dir {
        files.push(dir.join(".gemini").join("settings.json"));
    }
    files
}

fn policy_dirs(project_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = vec![system_dir().join("policies")];
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".gemini").join("policies"));
    }
    if let Some(dir) = project_dir {
        dirs.push(dir.join(".gemini").join("policies"));
    }
    dirs
}

/// The shell entries of `tools.allowed`, `tools.exclude` and `tools.core`
/// (and their legacy `allowedTools`, `excludeTools`, `coreTools` names).
/// Every one names original command text a rewrite would no longer match.
/// `Err` when a file exists but cannot be read or parsed.
pub(super) fn allowed_shell_entries(files: &[PathBuf]) -> Result<Vec<String>, ()> {
    let mut entries = Vec::new();
    for path in files {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(()),
        };
        let v = crate::core::jsonc::parse_jsonc(&text).map_err(|_| ())?;
        for list in [
            v.pointer("/tools/allowed"),
            v.get("allowedTools"),
            v.pointer("/tools/exclude"),
            v.get("excludeTools"),
            v.pointer("/tools/core"),
            v.get("coreTools"),
        ]
        .into_iter()
        .flatten()
        {
            for entry in list.as_array().ok_or(())? {
                let entry = entry.as_str().ok_or(())?.trim();
                if SHELL_TOOLS.iter().any(|t| entry.starts_with(t)) {
                    entries.push(entry.to_string());
                }
            }
        }
    }
    Ok(entries)
}

/// Whether one of `entries` may pre-approve `cmd`. Over-matching only makes
/// the hook pass more commands through unchanged.
pub(super) fn entries_may_allow(entries: &[String], cmd: &str) -> bool {
    if entries.is_empty() {
        return false;
    }
    let Some(segments) = parse_strict(cmd) else {
        return true;
    };
    entries.iter().any(|entry| {
        let prefix = SHELL_TOOLS
            .iter()
            .find_map(|t| entry.strip_prefix(t))
            .unwrap_or("");
        let Some(inner) = prefix.strip_prefix('(').and_then(|p| p.strip_suffix(')')) else {
            // A bare tool name approves every shell command; anything else
            // is unreadable and treated the same way.
            return true;
        };
        let inner = inner.trim();
        segments.iter().any(|seg| seg.raw.starts_with(inner))
    })
}

fn policies_mention_shell(dirs: &[PathBuf]) -> bool {
    dirs.iter().any(|dir| {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return dir.exists();
        };
        entries.filter_map(Result::ok).any(|e| {
            std::fs::read_to_string(e.path())
                .map_or(true, |text| SHELL_TOOLS.iter().any(|t| text.contains(t)))
        })
    })
}

/// True when Gemini may have run `cmd` without asking, so a rewrite could
/// add a prompt (see the module docs).
pub(super) fn may_run_without_prompt(cmd: &str, payload: &serde_json::Value) -> bool {
    let project_dir = std::env::var_os("GEMINI_PROJECT_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            payload
                .get("cwd")
                .and_then(serde_json::Value::as_str)
                .map(PathBuf::from)
        });
    let project_dir = project_dir.as_deref();
    match allowed_shell_entries(&settings_files(project_dir)) {
        Ok(entries) if entries_may_allow(&entries, cmd) => true,
        Ok(_) => policies_mention_shell(&policy_dirs(project_dir)),
        Err(()) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowed_entries_match_the_original_command() {
        let e = |s: &[&str]| s.iter().map(ToString::to_string).collect::<Vec<_>>();
        let git = e(&["run_shell_command(git status)"]);
        assert!(entries_may_allow(&git, "git status"));
        assert!(entries_may_allow(&git, "git status --short"));
        assert!(entries_may_allow(&git, "ls && git status"));
        assert!(!entries_may_allow(&git, "git log"));
        assert!(entries_may_allow(&git, "echo $(x)"), "unprovable: in doubt");
        assert!(entries_may_allow(&e(&["run_shell_command"]), "cargo test"));
        assert!(entries_may_allow(&e(&["ShellTool(cargo"]), "make"));
        assert!(!entries_may_allow(&[], "git status"));
    }

    #[test]
    fn settings_are_read_and_unreadable_ones_fail_safe() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        std::fs::write(
            &file,
            r#"{"tools":{"allowed":["run_shell_command(git status)","read_file"]}}"#,
        )
        .unwrap();
        let files = vec![file.clone(), dir.path().join("missing.json")];
        assert_eq!(
            allowed_shell_entries(&files).unwrap(),
            vec!["run_shell_command(git status)".to_string()]
        );
        std::fs::write(&file, "{ broken").unwrap();
        assert!(allowed_shell_entries(&files).is_err());
    }

    #[test]
    fn policy_files_about_the_shell_count_as_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let policies = dir.path().join("policies");
        assert!(!policies_mention_shell(std::slice::from_ref(&policies)));
        std::fs::create_dir(&policies).unwrap();
        std::fs::write(policies.join("a.toml"), "toolName = \"read_file\"").unwrap();
        assert!(!policies_mention_shell(std::slice::from_ref(&policies)));
        std::fs::write(
            policies.join("b.toml"),
            "[[rule]]\ntoolName = \"run_shell_command\"\ndecision = \"allow\"",
        )
        .unwrap();
        assert!(policies_mention_shell(std::slice::from_ref(&policies)));
    }
}
