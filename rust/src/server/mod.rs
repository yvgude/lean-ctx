pub mod bounded_lock;
pub mod bypass_hint;
pub mod compaction_sync;
pub mod context_gate;
mod dispatch;
pub mod dynamic_tools;
pub mod elicitation;
pub(crate) mod execute;
pub mod helpers;
pub mod multi_path;
pub mod notifications;
pub mod permission_inheritance;
pub mod policy_guard;
pub mod progress;
pub mod prompts;
pub mod reference_store;
pub mod registry;
pub mod resources;
pub mod role_guard;
pub mod roots;
use roots::has_project_marker;
pub mod tool_trait;
pub mod tool_visibility;

use futures::FutureExt;
use rmcp::ErrorData;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, Implementation, InitializeRequestParams,
    InitializeResult, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo,
};
use rmcp::service::{RequestContext, RoleServer};

use crate::tools::{CrpMode, LeanCtxServer};
mod call_tool;
mod post_dispatch;
mod post_process;
mod server_handler;

pub fn build_instructions_for_test(crp_mode: CrpMode) -> String {
    crate::instructions::build_instructions_for_test(crp_mode)
}

pub fn build_claude_code_instructions_for_test() -> String {
    crate::instructions::claude_code_instructions()
}

/// Deterministic STATIC Claude Code instructions (cold first-contact, no dynamic
/// session/knowledge/gotcha payload) for the char-budget benchmark.
pub fn build_claude_code_static_instructions_for_test() -> String {
    crate::instructions::claude_code_static_instructions_for_test()
}

fn is_home_or_agent_dir(dir: &std::path::Path) -> bool {
    if let Some(home) = dirs::home_dir()
        && dir == home
    {
        return true;
    }
    crate::core::pathutil::is_agent_config_dir(dir)
}

fn git_toplevel_from(dir: &std::path::Path) -> Option<String> {
    std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        })
}

pub fn derive_project_root_from_cwd() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let canonical = crate::core::pathutil::safe_canonicalize_or_self(&cwd);

    if is_home_or_agent_dir(&canonical) {
        return git_toplevel_from(&canonical);
    }

    if has_project_marker(&canonical) {
        return Some(canonical.to_string_lossy().to_string());
    }

    if let Some(git_root) = git_toplevel_from(&canonical) {
        return Some(git_root);
    }

    if let Some(root) = detect_multi_root_workspace(&canonical) {
        return Some(root);
    }

    // Fallback: use CWD as project root if it's a specific, safe directory.
    // This ensures bare directories (no .git, no markers) still work.
    // Guard: reject home dir, filesystem root, and agent sandbox dirs.
    if !crate::core::pathutil::is_broad_or_unsafe_root(&canonical) {
        tracing::info!(
            "No project markers found — using CWD as project root: {}",
            canonical.display()
        );
        return Some(canonical.to_string_lossy().to_string());
    }

    None
}

// Delegated to crate::core::pathutil::is_broad_or_unsafe_root
#[cfg(test)]
use crate::core::pathutil::is_broad_or_unsafe_root;

/// Detect a multi-root workspace: a directory that has no project markers
/// itself, but contains child directories that do. In this case, use the
/// parent as jail root and auto-allow all child projects via LEAN_CTX_ALLOW_PATH.
fn detect_multi_root_workspace(dir: &std::path::Path) -> Option<String> {
    // Never enumerate the home dir or macOS TCC-protected dirs (Documents/Desktop/
    // Downloads): read_dir there triggers a macOS privacy prompt (#356), and a real
    // project under them is already handled upstream via has_project_marker.
    if crate::core::pathutil::is_tcc_sensitive_home_dir(dir) {
        return None;
    }
    let entries = std::fs::read_dir(dir).ok()?;
    let mut child_projects: Vec<String> = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && has_project_marker(&path) {
            let canonical = crate::core::pathutil::safe_canonicalize_or_self(&path);
            child_projects.push(canonical.to_string_lossy().to_string());
        }
    }

    if child_projects.len() >= 2 {
        let existing = std::env::var("LEAN_CTX_ALLOW_PATH").unwrap_or_default();
        let sep = if cfg!(windows) { ";" } else { ":" };
        let merged = if existing.is_empty() {
            child_projects.join(sep)
        } else {
            format!("{existing}{sep}{}", child_projects.join(sep))
        };
        // SAFETY: set during MCP `initialize` (connection bootstrap), before any
        // tool-handler thread reads the jail allow-list via `pathjail`. The only
        // concurrent startup tasks (proxy spawn, savings publish) never consult it.
        unsafe { std::env::set_var("LEAN_CTX_ALLOW_PATH", &merged) };
        tracing::info!(
            "Multi-root workspace detected at {}: auto-allowing {} child projects",
            dir.display(),
            child_projects.len()
        );
        return Some(dir.to_string_lossy().to_string());
    }

    None
}

pub fn tool_descriptions_for_test() -> Vec<(String, String)> {
    crate::server::registry::build_registry()
        .tool_defs()
        .into_iter()
        .map(|t| {
            (
                t.name.to_string(),
                t.description.as_deref().unwrap_or("").to_string(),
            )
        })
        .collect()
}

pub fn tool_schemas_json_for_test() -> String {
    crate::server::registry::build_registry()
        .tool_defs()
        .iter()
        .map(|t| {
            format!(
                "{}: {}",
                t.name,
                serde_json::to_string(&t.input_schema).unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Tools that always pass through the workflow gate regardless of state.
/// Read-only tools should never be blocked — agents need them for context
/// recovery after crashes or session transitions.
pub const WORKFLOW_PASSTHROUGH_TOOLS: &[&str] = &[
    "ctx",
    "ctx_workflow",
    "ctx_read",
    "ctx_multi_read",
    "ctx_smart_read",
    "ctx_search",
    "ctx_tree",
    "ctx_session",
    "ctx_ledger",
];

/// A workflow is stale if it hasn't been updated in 30 minutes.
/// This prevents dead workflows from blocking tools across sessions.
pub fn is_workflow_stale(run: &crate::core::workflow::types::WorkflowRun) -> bool {
    let elapsed = chrono::Utc::now()
        .signed_duration_since(run.updated_at)
        .num_minutes();
    elapsed > 30
}

fn is_shell_tool_name(name: &str) -> bool {
    matches!(name, "ctx_shell" | "ctx_execute")
}

fn extract_file_read_from_shell(cmd: &str) -> Option<String> {
    let trimmed = cmd.trim();
    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    if parts.len() < 2 {
        return None;
    }
    let bin = parts[0].rsplit('/').next().unwrap_or(parts[0]);
    match bin {
        "cat" | "head" | "tail" | "less" | "more" | "bat" | "batcat" => {
            let file_arg = parts.iter().skip(1).find(|a| !a.starts_with('-'))?;
            Some(file_arg.to_string())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_markers_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("myproject");
        std::fs::create_dir_all(&root).unwrap();
        assert!(!has_project_marker(&root));

        std::fs::create_dir(root.join(".git")).unwrap();
        assert!(has_project_marker(&root));
    }

    #[test]
    fn home_dir_detected_as_agent_dir() {
        if let Some(home) = dirs::home_dir() {
            assert!(is_home_or_agent_dir(&home));
        }
    }

    #[test]
    fn agent_dirs_detected() {
        let claude = std::path::PathBuf::from("/home/user/.claude");
        assert!(is_home_or_agent_dir(&claude));
        let codex = std::path::PathBuf::from("/home/user/.codex");
        assert!(is_home_or_agent_dir(&codex));
        let project = std::path::PathBuf::from("/home/user/projects/myapp");
        assert!(!is_home_or_agent_dir(&project));
    }

    #[test]
    fn test_unified_tool_count() {
        let tools = crate::tool_defs::unified_tool_defs();
        assert_eq!(tools.len(), 5, "Expected 5 unified tools");
    }

    #[test]
    fn test_granular_tool_count() {
        let tools = crate::tool_defs::granular_tool_defs();
        assert!(tools.len() >= 25, "Expected at least 25 granular tools");
    }

    #[test]
    fn test_registry_tool_count_ssot() {
        let registry = crate::server::registry::build_registry();
        assert_eq!(
            registry.len(),
            81,
            "Registry tool count drift! Update this test AND all docs when adding/removing tools."
        );
    }

    #[test]
    fn production_server_always_has_registry() {
        // The list_tools fallback that serves static defs when `registry` is None
        // must stay unreachable in production: every public constructor funnels
        // through new_with_startup, which sets registry = Some. Lock that invariant
        // so the advertised tool set can never silently drift from what dispatch
        // (which requires the registry) can actually execute.
        let server = crate::tools::create_server();
        assert!(
            server.registry.is_some(),
            "production server must carry a tool registry"
        );
    }

    #[test]
    fn disabled_tools_filters_list() {
        let all = crate::tool_defs::granular_tool_defs();
        let total = all.len();
        let disabled = ["ctx_graph".to_string(), "ctx_agent".to_string()];
        let filtered: Vec<_> = all
            .into_iter()
            .filter(|t| !disabled.iter().any(|d| t.name.as_ref() == d.as_str()))
            .collect();
        assert_eq!(filtered.len(), total - 2);
        assert!(!filtered.iter().any(|t| t.name.as_ref() == "ctx_graph"));
        assert!(!filtered.iter().any(|t| t.name.as_ref() == "ctx_agent"));
    }

    #[test]
    fn empty_disabled_tools_returns_all() {
        let all = crate::tool_defs::granular_tool_defs();
        let total = all.len();
        let disabled: Vec<String> = vec![];
        let filtered: Vec<_> = all
            .into_iter()
            .filter(|t| !disabled.iter().any(|d| t.name.as_ref() == d.as_str()))
            .collect();
        assert_eq!(filtered.len(), total);
    }

    #[test]
    fn misspelled_disabled_tool_is_silently_ignored() {
        let all = crate::tool_defs::granular_tool_defs();
        let total = all.len();
        let disabled = ["ctx_nonexistent_tool".to_string()];
        let filtered: Vec<_> = all
            .into_iter()
            .filter(|t| !disabled.iter().any(|d| t.name.as_ref() == d.as_str()))
            .collect();
        assert_eq!(filtered.len(), total);
    }

    #[test]
    fn detect_multi_root_workspace_with_child_projects() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        let proj_a = workspace.join("project-a");
        let proj_b = workspace.join("project-b");
        std::fs::create_dir_all(proj_a.join(".git")).unwrap();
        std::fs::create_dir_all(&proj_b).unwrap();
        std::fs::write(proj_b.join("package.json"), "{}").unwrap();

        let result = detect_multi_root_workspace(&workspace);
        assert!(
            result.is_some(),
            "should detect workspace with 2 child projects"
        );

        crate::test_env::remove_var("LEAN_CTX_ALLOW_PATH");
    }

    #[test]
    fn detect_multi_root_workspace_returns_none_for_single_project() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        let proj_a = workspace.join("project-a");
        std::fs::create_dir_all(proj_a.join(".git")).unwrap();

        let result = detect_multi_root_workspace(&workspace);
        assert!(
            result.is_none(),
            "should not detect workspace with only 1 child project"
        );
    }

    #[test]
    fn is_broad_or_unsafe_root_rejects_home() {
        if let Some(home) = dirs::home_dir() {
            assert!(is_broad_or_unsafe_root(&home));
        }
    }

    #[test]
    fn is_broad_or_unsafe_root_rejects_filesystem_root() {
        assert!(is_broad_or_unsafe_root(std::path::Path::new("/")));
    }

    #[test]
    fn is_broad_or_unsafe_root_rejects_agent_dirs() {
        assert!(is_broad_or_unsafe_root(std::path::Path::new(
            "/home/user/.claude"
        )));
        assert!(is_broad_or_unsafe_root(std::path::Path::new(
            "/home/user/.codex"
        )));
    }

    #[test]
    fn is_broad_or_unsafe_root_allows_project_subdir() {
        let tmp = tempfile::tempdir().unwrap();
        let subdir = tmp.path().join("my-project");
        std::fs::create_dir_all(&subdir).unwrap();
        assert!(!is_broad_or_unsafe_root(&subdir));
    }

    #[test]
    fn is_broad_or_unsafe_root_allows_tmp_subdirs() {
        assert!(!is_broad_or_unsafe_root(std::path::Path::new(
            "/tmp/leanctx-test"
        )));
        assert!(!is_broad_or_unsafe_root(std::path::Path::new(
            "/tmp/my-project"
        )));
    }

    #[test]
    fn is_broad_or_unsafe_root_allows_home_subdirs() {
        if let Some(home) = dirs::home_dir() {
            let subdir = home.join("projects").join("my-app");
            assert!(!is_broad_or_unsafe_root(&subdir));
        }
    }

    #[test]
    fn derive_project_root_falls_back_to_bare_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let bare = tmp.path().join("bare-dir");
        std::fs::create_dir_all(&bare).unwrap();

        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(&bare).unwrap();
        let result = derive_project_root_from_cwd();
        std::env::set_current_dir(original).unwrap();

        assert!(result.is_some(), "bare dir should produce a project root");
        let root = result.unwrap();
        assert!(
            root.contains("bare-dir"),
            "fallback should use the bare dir path"
        );
    }
}
