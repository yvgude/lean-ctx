//! Workspace-scope MCP registration detection (issue #312).
//!
//! Editors such as VS Code, Copilot, Cursor and Cline support a project-local
//! MCP config (e.g. `.vscode/mcp.json`) in addition to the user-global one.
//! When lean-ctx is registered in BOTH scopes — or when a workspace config is
//! malformed — Copilot/VS Code surface opaque runtime errors later, e.g.
//! `Collection or definition not found for mcp.config.ws0` or
//! "Tool … was not contributed". This module gives `doctor` a clear, early
//! diagnosis instead of leaving the user to trace a Copilot runtime failure.

use super::{BOLD, DIM, GREEN, Outcome, RED, RST, YELLOW};
use std::collections::BTreeSet;

/// A workspace-scope MCP config location, relative to the project root (cwd).
struct WorkspaceLocation {
    /// Human-facing editor label.
    label: &'static str,
    /// User/global MCP config location names that share this workspace surface.
    user_scope_names: &'static [&'static str],
    /// Path relative to the current working directory.
    rel: &'static str,
}

/// Known project-local MCP config files across editors that support a
/// workspace scope. Kept deliberately small and explicit for maintainability.
const WORKSPACE_LOCATIONS: &[WorkspaceLocation] = &[
    WorkspaceLocation {
        label: "VS Code / Cline",
        user_scope_names: &["VS Code", "Cline"],
        rel: ".vscode/mcp.json",
    },
    WorkspaceLocation {
        label: "Copilot",
        user_scope_names: &[],
        rel: ".github/mcp.json",
    },
    WorkspaceLocation {
        label: "Cursor",
        user_scope_names: &["Cursor"],
        rel: ".cursor/mcp.json",
    },
    WorkspaceLocation {
        label: "Zed",
        user_scope_names: &["Zed"],
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
pub(super) fn workspace_scope_outcome(
    user_scope_mcp_locations: &BTreeSet<&'static str>,
) -> Option<Outcome> {
    let cwd = std::env::current_dir().ok()?;

    let mut registered: Vec<(&WorkspaceLocation, String)> = Vec::new();
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
                    registered.push((loc, format!("{} ({})", loc.label, loc.rel)));
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
    //
    // This is informational (WARN), not a hard failure: dual-scope *can* cause
    // Copilot "ws0 not found" errors, but is also the expected state when
    // running inside the lean-ctx repo itself (the workspace config is part of
    // the distribution). Marking it `ok: true` keeps it out of the failure
    // count while still surfacing the hint.
    let duplicated: Vec<String> = registered
        .iter()
        .filter(|(loc, _)| {
            loc.user_scope_names
                .iter()
                .any(|name| user_scope_mcp_locations.contains(name))
        })
        .map(|(_, display)| display.clone())
        .collect();

    if !duplicated.is_empty() {
        return Some(Outcome {
            ok: true,
            line: format!(
                "{BOLD}Workspace MCP{RST}  {YELLOW}lean-ctx registered in BOTH user and \
                 workspace scope{RST} {DIM}({}){RST}  {DIM}(keep only one scope — duplicate \
                 registration can cause Copilot 'ws0 not found' / 'tool not contributed' \
                 errors){RST}",
                duplicated.join(", ")
            ),
        });
    }

    // 3) Workspace-only registration → informational, healthy.
    Some(Outcome {
        ok: true,
        line: format!(
            "{BOLD}Workspace MCP{RST}  {GREEN}lean-ctx found in workspace scope: {}{RST}",
            registered
                .into_iter()
                .map(|(_, display)| display)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    })
}

/// Removes lean-ctx from workspace-scope MCP configs when user-scope already
/// has it registered. Called by `doctor --fix` to resolve the dual-scope conflict.
/// Returns the number of files cleaned up.
pub(super) fn fix_workspace_dual_scope(user_scope_mcp_locations: &BTreeSet<&'static str>) -> usize {
    if user_scope_mcp_locations.is_empty() {
        return 0;
    }
    let Some(cwd) = std::env::current_dir().ok() else {
        return 0;
    };

    let mut fixed = 0;
    for loc in WORKSPACE_LOCATIONS {
        if !loc
            .user_scope_names
            .iter()
            .any(|name| user_scope_mcp_locations.contains(name))
        {
            continue;
        }
        let path = cwd.join(loc.rel);
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if content.trim().is_empty() || !super::has_lean_ctx_mcp_entry(&content) {
            continue;
        }
        if let Ok(mut json) = crate::core::jsonc::parse_jsonc(&content) {
            let removed = remove_lean_ctx_from_json(&mut json);
            if removed
                && let Ok(out) = serde_json::to_string_pretty(&json)
                && std::fs::write(&path, out.as_bytes()).is_ok()
            {
                tracing::info!(
                    "Removed lean-ctx from workspace-scope {} (user-scope preferred)",
                    path.display()
                );
                fixed += 1;
            }
        }
    }
    fixed
}

/// Remove lean-ctx server entries from a parsed JSON value.
fn remove_lean_ctx_from_json(json: &mut serde_json::Value) -> bool {
    let containers = ["servers", "mcpServers", "mcp.servers"];
    let mut removed = false;
    for key in containers {
        if let Some(map) = navigate_mut(json, key)
            && let Some(obj) = map.as_object_mut()
        {
            if obj.remove("lean-ctx").is_some() {
                removed = true;
            }
            if obj.remove("user-lean-ctx").is_some() {
                removed = true;
            }
        }
    }
    removed
}

fn navigate_mut<'a>(
    json: &'a mut serde_json::Value,
    dotted: &str,
) -> Option<&'a mut serde_json::Value> {
    let parts: Vec<&str> = dotted.split('.').collect();
    let mut current = json;
    for part in parts {
        current = current.get_mut(part)?;
    }
    Some(current)
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
    /// Under the test environment lock because the cwd is process-global.
    fn with_cwd<T>(dir: &std::path::Path, f: impl FnOnce() -> T) -> T {
        let _cwd = crate::test_env::CurrentDirGuard::enter(dir);
        f()
    }

    #[test]
    fn none_when_no_workspace_config() {
        let tmp = tempfile::tempdir().unwrap();
        let out = with_cwd(tmp.path(), || {
            workspace_scope_outcome(&BTreeSet::from(["VS Code"]))
        });
        assert!(out.is_none());
    }

    #[test]
    fn duplicate_is_informational_warning_not_failure() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            ".vscode/mcp.json",
            r#"{"servers": {"lean-ctx": {"command": "lean-ctx"}}}"#,
        );
        let out = with_cwd(tmp.path(), || {
            workspace_scope_outcome(&BTreeSet::from(["VS Code"]))
        })
        .unwrap();
        // Dual-scope is a WARN (informational), not a hard failure — it's the
        // expected state inside the lean-ctx repo itself.
        assert!(out.ok, "dual-scope should be ok:true (informational WARN)");
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
        let out = with_cwd(tmp.path(), || workspace_scope_outcome(&BTreeSet::new())).unwrap();
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
        let out = with_cwd(tmp.path(), || {
            workspace_scope_outcome(&BTreeSet::from(["VS Code"]))
        })
        .unwrap();
        assert!(!out.ok);
        assert!(out.line.contains("malformed"));
    }

    #[test]
    fn copilot_cli_does_not_duplicate_vscode_workspace_mcp() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            ".vscode/mcp.json",
            r#"{"servers": {"lean-ctx": {"command": "lean-ctx"}}}"#,
        );
        let out = with_cwd(tmp.path(), || {
            workspace_scope_outcome(&BTreeSet::from(["GitHub Copilot CLI"]))
        })
        .unwrap();
        assert!(out.ok);
        assert!(out.line.contains("workspace scope"));
        assert!(!out.line.contains("BOTH user and"));
    }

    #[test]
    fn jsonc_workspace_config_with_trailing_comma_is_accepted() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            ".vscode/mcp.json",
            "{\n  \"servers\": {\n    \"lean-ctx\": { \"command\": \"lean-ctx\" },\n  },\n}",
        );
        let out = with_cwd(tmp.path(), || workspace_scope_outcome(&BTreeSet::new())).unwrap();
        assert!(out.ok, "JSONC with trailing commas must parse cleanly");
    }

    #[test]
    fn fix_skips_vscode_workspace_when_only_copilot_cli_is_user_scope() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            ".vscode/mcp.json",
            r#"{"servers": {"lean-ctx": {"command": "lean-ctx"}}}"#,
        );
        let fixed = with_cwd(tmp.path(), || {
            fix_workspace_dual_scope(&BTreeSet::from(["GitHub Copilot CLI"]))
        });
        assert_eq!(fixed, 0);

        let content = fs::read_to_string(tmp.path().join(".vscode/mcp.json")).unwrap();
        assert!(content.contains("lean-ctx"));
    }

    #[test]
    fn fix_removes_vscode_workspace_when_vscode_is_user_scope() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            ".vscode/mcp.json",
            r#"{"servers": {"lean-ctx": {"command": "lean-ctx"}}}"#,
        );
        let fixed = with_cwd(tmp.path(), || {
            fix_workspace_dual_scope(&BTreeSet::from(["VS Code"]))
        });
        assert_eq!(fixed, 1);

        let content = fs::read_to_string(tmp.path().join(".vscode/mcp.json")).unwrap();
        assert!(!content.contains("lean-ctx"));
    }
}
