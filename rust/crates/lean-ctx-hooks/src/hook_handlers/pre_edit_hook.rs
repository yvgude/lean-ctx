// SPDX-License-Identifier: Apache-2.0

//! The native Write/Edit/MultiEdit leg of the one pre-edit hook
//! ([`lean_ctx::core::pre_edit`]).
//!
//! `lean-ctx hook write-guard` runs before every native write. Once its own
//! guard passes, this module rebuilds the post-edit text (a Write's content,
//! or the edit's replacements applied to the file on disk) and runs the
//! shared checkers. A refusing finding denies the tool call; a hint goes back
//! as PreToolUse `additionalContext`. Nothing is printed when no checker has
//! anything to say, so a clean edit leaves the host's prompt cache alone.
//! LeanCTX's own tools (`ctx_patch`, `ctx_edit`) run the hook themselves and
//! are skipped here.

use std::path::{Path, PathBuf};

use serde_json::Value;

use lean_ctx::core::pre_edit::{self, PreEditInput, Severity};

use super::edit_health::{Replacement, collect_edits};

/// Largest file the hook reads or rebuilds; larger edits are not checked.
const MAX_EDIT_BYTES: u64 = 1_000_000;

/// What the pre-edit checks say about a native write.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    Refuse(String),
    Hint(String),
}

/// The checks' outcome for `payload`; `None` when there is nothing to say or
/// the write cannot be rebuilt reliably.
pub(super) fn outcome(payload: &Value) -> Option<Outcome> {
    let tool = super::payload::resolve_tool_name(payload)?;
    if tool.starts_with("ctx_") || tool.starts_with("mcp__lean-ctx__") {
        return None;
    }
    let args = super::payload::resolve_tool_args(payload)?;
    let (_, file) =
        super::payload::resolve_path_field(Some(&args), super::payload::READ_PATH_FIELDS)?;
    let path = absolute(&file, payload)?;
    // Cheap exit for every project without decisions: no file is read.
    lean_ctx::core::decisions::find_root(&path)?;
    let before = read_small(&path)?;
    let after = match content_of(&args) {
        Some(content) => content,
        None => apply_edits(before.as_deref()?, &collect_edits(&args)?)?,
    };
    let findings = pre_edit::evaluate(&PreEditInput::new(&path, before.as_deref(), &after));
    if let Some(refusal) = pre_edit::refusal(&findings) {
        return Some(Outcome::Refuse(refusal));
    }
    pre_edit::render(&findings, Severity::Hint).map(Outcome::Hint)
}

/// Print the hint as PreToolUse `additionalContext` (Claude Code / Codex
/// dialect; other hosts ignore it).
pub(super) fn emit_hint(hint: &str) {
    let out = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "additionalContext": hint,
        }
    });
    println!("{out}");
}

fn absolute(file: &str, payload: &Value) -> Option<PathBuf> {
    let p = Path::new(file.trim());
    if p.is_absolute() {
        return Some(p.to_path_buf());
    }
    let base = payload
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|c| !c.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())?;
    Some(base.join(p))
}

/// The file on disk before the write.
enum Before {
    /// The file does not exist yet.
    Missing,
    Text(String),
}

impl Before {
    fn as_deref(&self) -> Option<&str> {
        match self {
            Before::Missing => None,
            Before::Text(t) => Some(t),
        }
    }
}

/// `None` when the file is unreadable or too big to check.
fn read_small(path: &Path) -> Option<Before> {
    match std::fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(Before::Missing),
        Err(_) => None,
        Ok(m) if !m.is_file() || m.len() > MAX_EDIT_BYTES => None,
        Ok(_) => std::fs::read_to_string(path).ok().map(Before::Text),
    }
}

/// The full new text of a Write-style call.
fn content_of(args: &Value) -> Option<String> {
    ["content", "contents"]
        .iter()
        .find_map(|k| args.get(*k).and_then(Value::as_str))
        .map(str::to_string)
}

/// `before` with each replacement applied in order, as the host will; `None`
/// when an `old_string` is not in the text (the host will reject the edit).
fn apply_edits(before: &str, edits: &[Replacement]) -> Option<String> {
    let mut content = before.to_string();
    for e in edits {
        if e.old.is_empty() || !content.contains(&e.old) {
            return None;
        }
        content = if e.replace_all {
            content.replace(&e.old, &e.new)
        } else {
            content.replacen(&e.old, &e.new, 1)
        };
    }
    Some(content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn project() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".leanctx/decisions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
        std::fs::write(
            dir.join("dec-no-print.toml"),
            "id = \"dec-no-print\"\ntitle = \"No println\"\nstatement = \"Use tracing.\"\n\
             status = \"active\"\ndecided_by = \"team\"\ndecided_at = \"2026-11-12\"\n\
             [scope]\npaths = [\"src/**\"]\n\
             [[check]]\nkind = \"forbid_regex\"\npattern = \"println!\"\nmessage = \"use tracing\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/a.rs"), "fn a() {}\n").unwrap();
        tmp
    }

    #[test]
    fn native_edit_violation_is_hinted() {
        let tmp = project();
        let payload = json!({
            "tool_name": "Edit",
            "cwd": tmp.path(),
            "tool_input": {
                "file_path": "src/a.rs",
                "old_string": "fn a() {}",
                "new_string": "fn a() { println!(\"x\"); }"
            }
        });
        match outcome(&payload) {
            Some(Outcome::Hint(text)) => {
                assert!(text.starts_with(pre_edit::HINT_HEADER), "{text}");
                assert!(
                    text.contains("dec-no-print") && text.contains("use tracing"),
                    "{text}"
                );
            }
            other => panic!("expected a hint, got {other:?}"),
        }
    }

    #[test]
    fn clean_write_and_unrelated_project_say_nothing() {
        let tmp = project();
        let clean = json!({
            "tool_name": "Write",
            "cwd": tmp.path(),
            "tool_input": { "file_path": "src/b.rs", "content": "fn b() {}\n" }
        });
        assert_eq!(outcome(&clean), None);
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(elsewhere.path().join(".git")).unwrap();
        let other = json!({
            "tool_name": "Write",
            "cwd": elsewhere.path(),
            "tool_input": { "file_path": "x.rs", "content": "println!(\"x\")" }
        });
        assert_eq!(outcome(&other), None);
        let own = json!({
            "tool_name": "mcp__lean-ctx__ctx_edit",
            "cwd": tmp.path(),
            "tool_input": { "path": "src/b.rs", "content": "println!(\"x\")" }
        });
        assert_eq!(outcome(&own), None, "LeanCTX tools check themselves");
    }

    #[test]
    fn edits_apply_forward_or_bail() {
        let edits = vec![Replacement {
            old: "a".into(),
            new: "b".into(),
            replace_all: true,
        }];
        assert_eq!(apply_edits("aXa", &edits).as_deref(), Some("bXb"));
        assert_eq!(apply_edits("zzz", &edits), None);
    }
}
