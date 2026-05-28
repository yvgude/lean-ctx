//! Workspace-scope MCP registration detection (issue #312).
//!
//! Editors such as VS Code, Copilot, Cursor and Cline support a project-local
//! MCP config (e.g. `.vscode/mcp.json`) in addition to the user-global one.
//! When lean-ctx is registered in BOTH scopes — or when a workspace config is
//! malformed — Copilot/VS Code surface opaque runtime errors later, e.g.
//! `Collection or definition not found for mcp.config.ws0` or
//! "Tool … was not contributed". This module gives `doctor` a clear, early
//! diagnosis instead of leaving the user to trace a Copilot runtime failure.

use super::{Outcome, BOLD, DIM, GREEN, RED, RST, YELLOW};

/// A workspace-scope MCP config location, relative to the project root (cwd).
struct WorkspaceLocation {
    /// Human-facing editor label.
    label: &'static str,
    /// Path relative to the current working directory.
    rel: &'static str,
}

/// Known project-local MCP config files across editors that support a
/// workspace scope. Kept deliberately small and explicit for maintainability.
const WORKSPACE_LOCATIONS: &[WorkspaceLocation] = &[
    WorkspaceLocation {
        label: "VS Code / Cline",
        rel: ".vscode/mcp.json",
    },
    WorkspaceLocation {
        label: "Copilot",
        rel: ".github/mcp.json",
    },
    WorkspaceLocation {
        label: "Cursor",
        rel: ".cursor/mcp.json",
    },
    WorkspaceLocation {
        label: "Zed",
        rel: ".zed/settings.json",
    },
];

/// Inspect workspace-scope MCP configs in the current project directory.
///
/// Returns `Some(Outcome)` only when there is something worth surfacing:
/// a malformed workspace config, or a user+workspace duplicate registration,
/// or a healthy workspace-only registration. Returns `None` when no workspace
/// MCP config is present, so the doctor output stays uncluttered for the
/// common (user-scope only) case.
pub(super) fn workspace_scope_outcome(user_scope_has_lean_ctx: bool) -> Option<Outcome> {
    let cwd = std::env::current_dir().ok()?;

    let mut registered: Vec<String> = Vec::new();
    let mut malformed: Vec<String> = Vec::new();

    for loc in WORKSPACE_LOCATIONS {
        let path = cwd.join(loc.rel);
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if content.trim().is_empty() {
            continue;
        }
        match crate::core::jsonc::parse_jsonc(&content) {
            Ok(_) => {
                if super::has_lean_ctx_mcp_entry(&content) {
                    registered.push(format!("{} ({})", loc.label, loc.rel));
                }
            }
            Err(e) => {
                malformed.push(format!("{} ({}): {e}", loc.label, loc.rel));
            }
        }
    }

    // 1) Malformed workspace config is the highest-priority signal: it commonly
    //    manifests later as opaque Copilot "ws0 not found" runtime errors.
    if !malformed.is_empty() {
        return Some(Outcome {
            ok: false,
            line: format!(
                "{BOLD}Workspace MCP{RST}  {RED}malformed workspace config{RST}  \
                 {DIM}{}{RST}  {DIM}(fix or remove this file — a broken workspace entry \
                 surfaces later as Copilot 'ws0 not found' errors){RST}",
                malformed.join("; ")
            ),
        });
    }

    if registered.is_empty() {
        return None;
    }

    // 2) Duplicate registration across user + workspace scope.
    if user_scope_has_lean_ctx {
        return Some(Outcome {
            ok: false,
            line: format!(
                "{BOLD}Workspace MCP{RST}  {YELLOW}lean-ctx registered in BOTH user and \
                 workspace scope{RST} {DIM}({}){RST}  {DIM}(keep only one scope — duplicate \
                 registration can cause Copilot 'ws0 not found' / 'tool not contributed' \
                 errors){RST}",
                registered.join(", ")
            ),
        });
    }

    // 3) Workspace-only registration → informational, healthy.
    Some(Outcome {
        ok: true,
        line: format!(
            "{BOLD}Workspace MCP{RST}  {GREEN}lean-ctx found in workspace scope: {}{RST}",
            registered.join(", ")
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &std::path::Path, rel: &str, content: &str) {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    /// Run `workspace_scope_outcome` with the cwd temporarily set to `dir`.
    /// Serialized via a mutex because `set_current_dir` is process-global.
    fn with_cwd<T>(dir: &std::path::Path, f: impl FnOnce() -> T) -> T {
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir).unwrap();
        let out = f();
        std::env::set_current_dir(prev).unwrap();
        out
    }

    #[test]
    fn none_when_no_workspace_config() {
        let tmp = tempfile::tempdir().unwrap();
        let out = with_cwd(tmp.path(), || workspace_scope_outcome(true));
        assert!(out.is_none());
    }

    #[test]
    fn duplicate_when_user_and_workspace_both_have_lean_ctx() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            ".vscode/mcp.json",
            r#"{"servers": {"lean-ctx": {"command": "lean-ctx"}}}"#,
        );
        let out = with_cwd(tmp.path(), || workspace_scope_outcome(true)).unwrap();
        assert!(!out.ok);
        assert!(out.line.contains("BOTH user and"));
    }

    #[test]
    fn workspace_only_is_healthy() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            ".vscode/mcp.json",
            r#"{"servers": {"lean-ctx": {"command": "lean-ctx"}}}"#,
        );
        let out = with_cwd(tmp.path(), || workspace_scope_outcome(false)).unwrap();
        assert!(out.ok);
        assert!(out.line.contains("workspace scope"));
    }

    #[test]
    fn malformed_workspace_config_is_flagged() {
        let tmp = tempfile::tempdir().unwrap();
        // Unbalanced braces — not recoverable even as JSONC.
        write(
            tmp.path(),
            ".vscode/mcp.json",
            r#"{"servers": {"lean-ctx": "#,
        );
        let out = with_cwd(tmp.path(), || workspace_scope_outcome(true)).unwrap();
        assert!(!out.ok);
        assert!(out.line.contains("malformed"));
    }

    #[test]
    fn jsonc_workspace_config_with_trailing_comma_is_accepted() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            ".vscode/mcp.json",
            "{\n  \"servers\": {\n    \"lean-ctx\": { \"command\": \"lean-ctx\" },\n  },\n}",
        );
        let out = with_cwd(tmp.path(), || workspace_scope_outcome(false)).unwrap();
        assert!(out.ok, "JSONC with trailing commas must parse cleanly");
    }
}
