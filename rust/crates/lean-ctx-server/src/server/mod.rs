// SPDX-License-Identifier: Apache-2.0
pub mod background_shell;
pub use lean_ctx::core::bounded_lock;
pub use lean_ctx::core::bypass_hint;
mod call_identity;
#[cfg(test)]
mod catalog_tests;
pub mod compaction_sync;
pub mod context_gate;
#[cfg(test)]
mod decision_loop_integration_tests;
mod dispatch;
pub use dispatch::canonicalize_arg_aliases;
pub mod dynamic_tools;
pub mod elicitation;
pub(crate) mod execute;
#[cfg(unix)]
pub use execute::register_protected_shell_runner;
mod file_resource;
pub mod helpers;
#[cfg(any(test, feature = "test-support"))]
pub mod isolation_fixture;
pub mod mcp_idle;
pub mod multi_path;
pub mod native_receipts;
pub mod notifications;
pub mod permission_inheritance;
pub mod policy_guard;
pub mod predictive_preload;
pub mod progress;
pub mod prompts;
pub use lean_ctx::core::reference_store;
pub mod registry;
pub mod resources;
pub mod role_guard;
pub use lean_ctx::core::mcp_roots as roots;
pub mod schema_hook;
pub(crate) mod shell_approval;
pub use lean_ctx::core::mcp_roots::{derive_project_root_from, derive_project_root_from_cwd};
use lean_ctx::core::pathutil::has_project_marker;
pub mod tool_trait;
pub mod tool_visibility;
pub mod tools_config_watch;
pub(crate) mod walk_hint;
// Workspace authority verifies file ownership, which needs a Unix host.
#[cfg(all(test, unix))]
mod workspace_roots_tests;

use futures::FutureExt;
use rmcp::ErrorData;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, Implementation, InitializeRequestParams,
    InitializeResult, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
};
use rmcp::service::{RequestContext, RoleServer};

use crate::tools::{CrpMode, LeanCtxServer};
mod call_tool;
pub(crate) use call_tool::SessionSwitch;
mod post_dispatch;
mod post_process;
mod server_handler;

pub fn build_instructions_for_test(crp_mode: CrpMode) -> String {
    lean_ctx::instructions::build_instructions_for_test(crp_mode)
}

pub fn build_claude_code_instructions_for_test() -> String {
    lean_ctx::instructions::claude_code_instructions()
}

/// Deterministic STATIC Claude Code instructions (cold first-contact, no dynamic
/// session/knowledge/gotcha payload) for the char-budget benchmark.
pub fn build_claude_code_static_instructions_for_test() -> String {
    lean_ctx::instructions::claude_code_static_instructions_for_test()
}

// Delegated to lean_ctx::core::pathutil::is_broad_or_unsafe_root
#[cfg(test)]
use lean_ctx::core::pathutil::is_broad_or_unsafe_root;

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

pub use lean_ctx::core::workflow::WORKFLOW_PASSTHROUGH_TOOLS;

/// A workflow is stale if it hasn't been updated in 30 minutes.
/// This prevents dead workflows from blocking tools across sessions.
///
/// The rule itself is `core::workflow::is_run_stale` (shared with the store
/// and the tool gate): the stand-in for a withheld workflow is never stale.
pub fn is_workflow_stale(run: &lean_ctx::core::workflow::types::WorkflowRun) -> bool {
    lean_ctx::core::workflow::is_run_stale(run)
}

/// Whether the MCP client has lean-ctx hooks installed that compress native
/// Read/Shell/Grep/Glob transparently. Used by `list_tools` to decide whether
/// the shadow-only surface (just `ctx_call`) is safe.
fn is_client_hook_covered(client_name: &str) -> bool {
    lean_ctx::core::home::resolve_home_dir()
        .is_some_and(|home| lean_ctx::core::rules_channel::client_hook_covered(client_name, &home))
}

fn is_shell_tool_name(name: &str) -> bool {
    matches!(name, "ctx_shell" | "ctx_execute")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_dir_detected_as_broad_root() {
        // HOME is read here and again inside the check: another test may move
        // it in between (macOS release prebuild, 3.12.1).
        let _env = lean_ctx::core::data_dir::test_env_lock();
        if let Some(home) = dirs::home_dir() {
            assert!(is_broad_or_unsafe_root(&home));
        }
    }

    #[test]
    fn agent_dirs_detected() {
        let claude = std::path::PathBuf::from("/home/user/.claude");
        assert!(is_broad_or_unsafe_root(&claude));
        let codex = std::path::PathBuf::from("/home/user/.codex");
        assert!(is_broad_or_unsafe_root(&codex));
        let project = std::path::PathBuf::from("/home/user/projects/myapp");
        assert!(!is_broad_or_unsafe_root(&project));
    }

    #[test]
    fn test_unified_tool_count() {
        let tools = lean_ctx::tool_defs::unified_tool_defs();
        assert_eq!(tools.len(), 5, "Expected 5 unified tools");
    }

    #[test]
    fn test_granular_tool_count() {
        registry::register_tool_catalog();
        let tools = lean_ctx::tool_defs::granular_tool_defs();
        assert!(tools.len() >= 25, "Expected at least 25 granular tools");
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
        registry::register_tool_catalog();
        let all = lean_ctx::tool_defs::granular_tool_defs();
        let total = all.len();
        // `ctx_agent` is retained as a direct compatibility path but is no
        // longer part of the advertised granular surface. Exercise filtering
        // with one actual public tool instead of treating a hidden alias as
        // visible inventory.
        let disabled = ["ctx_graph".to_string()];
        let filtered: Vec<_> = all
            .into_iter()
            .filter(|t| !disabled.iter().any(|d| t.name.as_ref() == d.as_str()))
            .collect();
        assert_eq!(filtered.len(), total - 1);
        assert!(!filtered.iter().any(|t| t.name.as_ref() == "ctx_graph"));
        assert!(!filtered.iter().any(|t| t.name.as_ref() == "ctx_agent"));
    }

    #[test]
    fn empty_disabled_tools_returns_all() {
        let all = lean_ctx::tool_defs::granular_tool_defs();
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
        let all = lean_ctx::tool_defs::granular_tool_defs();
        let total = all.len();
        let disabled = ["ctx_nonexistent_tool".to_string()];
        let filtered: Vec<_> = all
            .into_iter()
            .filter(|t| !disabled.iter().any(|d| t.name.as_ref() == d.as_str()))
            .collect();
        assert_eq!(filtered.len(), total);
    }

    #[test]
    fn is_broad_or_unsafe_root_rejects_home() {
        // HOME is read here and again inside the check: another test may move
        // it in between (macOS release prebuild, 3.12.1).
        let _env = lean_ctx::core::data_dir::test_env_lock();
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
        // HOME is read here and again inside the check: another test may move
        // it in between (macOS release prebuild, 3.12.1).
        let _env = lean_ctx::core::data_dir::test_env_lock();
        if let Some(home) = dirs::home_dir() {
            let subdir = home.join("projects").join("my-app");
            assert!(!is_broad_or_unsafe_root(&subdir));
        }
    }
}
