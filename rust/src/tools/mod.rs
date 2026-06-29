pub mod autonomy;
pub mod ctx_agent;
pub mod ctx_analyze;
pub mod ctx_architecture;
pub mod ctx_artifacts;
pub mod ctx_benchmark;
pub mod ctx_callgraph;
pub mod ctx_compile;
pub mod ctx_compose;
pub mod ctx_compress;
pub mod ctx_compress_memory;
pub mod ctx_context;
pub mod ctx_control;
pub mod ctx_cost;
pub mod ctx_dedup;
pub mod ctx_delta;
pub mod ctx_discover;
pub mod ctx_edit;
pub mod ctx_execute;
pub mod ctx_expand;
pub mod ctx_explore;
pub mod ctx_feedback;
pub mod ctx_fill;
pub mod ctx_gain;
pub mod ctx_glob;
pub mod ctx_graph;
pub mod ctx_graph_diagram;
pub mod ctx_graph_diff;
pub mod ctx_graph_primitives;
pub mod ctx_handoff;
pub mod ctx_heatmap;
pub mod ctx_impact;
pub mod ctx_index;
pub mod ctx_intent;
pub mod ctx_knowledge;
pub mod ctx_knowledge_relations;
pub mod ctx_metrics;
pub mod ctx_multi_read;
pub mod ctx_multi_repo;
pub mod ctx_outline;
pub mod ctx_overview;
pub mod ctx_pack;
pub mod ctx_package;
pub mod ctx_patch;
pub mod ctx_plan;
pub mod ctx_plugins;
pub mod ctx_prefetch;
pub mod ctx_preload;
pub mod ctx_proof;
pub mod ctx_provider;
pub mod ctx_quality;
pub mod ctx_read;
pub mod ctx_refactor;
pub mod ctx_repomap;
pub mod ctx_response;
pub mod ctx_review;
pub mod ctx_routes;
pub mod ctx_rules;
pub mod ctx_search;
pub mod ctx_semantic_search;
pub mod ctx_session;
pub mod ctx_share;
pub mod ctx_shell;
pub mod ctx_skillify;
pub mod ctx_smart_read;
pub mod ctx_smells;
pub mod ctx_summary;
pub mod ctx_symbol;
pub mod ctx_task;
pub mod ctx_tools;
pub mod ctx_transcript_compact;
pub mod ctx_tree;
pub mod ctx_verify;
pub mod ctx_workflow;
pub(crate) mod edit_io;
pub(crate) mod edit_recovery;
pub(crate) mod graph_meta;
pub(crate) mod knowledge_shared;
pub(crate) mod output_format;
pub mod registered;
pub(crate) mod walk_guard;

mod server;
mod server_lifecycle;
mod server_metrics;
mod server_paths;
pub(crate) mod startup;

pub use server::*;
pub use startup::create_server;

#[cfg(test)]
mod resolve_path_tests {
    use super::startup::canonicalize_path;
    use super::*;

    fn create_git_root(path: &std::path::Path) -> String {
        std::fs::create_dir_all(path.join(".git")).unwrap();
        canonicalize_path(path)
    }

    #[cfg(not(feature = "no-jail"))]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn resolve_path_can_reroot_to_trusted_startup_root_when_session_root_is_stale() {
        // #991: serialize via `isolated_data_dir` (holds `test_env_lock`) so the
        // `LEAN_CTX_ALLOW_REROOT` set below cannot be removed by a sibling
        // reroot test running in parallel (e.g. the `remove_var` in
        // `..._without_opt_in`) between set and `resolve_path` — which would
        // silently disable rerooting and flake this assertion. Cleaned up at the
        // end so the opt-in never leaks to other tests.
        let _iso = crate::core::data_dir::isolated_data_dir();
        crate::test_env::set_var("LEAN_CTX_ALLOW_REROOT", "1");
        let tmp = tempfile::tempdir().unwrap();
        let stale = tmp.path().join("stale");
        let real = tmp.path().join("real");
        std::fs::create_dir_all(&stale).unwrap();
        let real_root = create_git_root(&real);
        std::fs::write(real.join("a.txt"), "ok").unwrap();

        let server = LeanCtxServer::new_with_startup(
            None,
            Some(real.as_path()),
            SessionMode::Personal,
            "default",
            "default",
        );
        {
            let mut session = server.session.write().await;
            session.project_root = Some(stale.to_string_lossy().to_string());
            session.shell_cwd = Some(stale.to_string_lossy().to_string());
        }

        let out = server
            .resolve_path(&real.join("a.txt").to_string_lossy())
            .await
            .unwrap();

        assert!(out.ends_with("/a.txt"));

        let session = server.session.read().await;
        assert_eq!(session.project_root.as_deref(), Some(real_root.as_str()));
        assert_eq!(session.shell_cwd.as_deref(), Some(real_root.as_str()));
        drop(session);
        crate::test_env::remove_var("LEAN_CTX_ALLOW_REROOT");
    }

    #[cfg(not(feature = "no-jail"))]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn resolve_path_rejects_absolute_path_outside_trusted_startup_root() {
        // Hermetic config + serialized via test_env_lock so a parallel test that
        // flips `path_jail` cannot disable this jail-enforcement assertion (#406).
        let _iso = crate::core::data_dir::isolated_data_dir();
        let tmp = tempfile::tempdir().unwrap();
        let stale = tmp.path().join("stale");
        let root = tmp.path().join("root");
        let other = tmp.path().join("other");
        std::fs::create_dir_all(&stale).unwrap();
        create_git_root(&root);
        let _other_value = create_git_root(&other);
        std::fs::write(other.join("b.txt"), "no").unwrap();

        let server = LeanCtxServer::new_with_startup(
            None,
            Some(root.as_path()),
            SessionMode::Personal,
            "default",
            "default",
        );
        {
            let mut session = server.session.write().await;
            session.project_root = Some(stale.to_string_lossy().to_string());
            session.shell_cwd = Some(stale.to_string_lossy().to_string());
        }

        let err = server
            .resolve_path(&other.join("b.txt").to_string_lossy())
            .await
            .unwrap_err();
        assert!(err.contains("path escapes project root"));

        let session = server.session.read().await;
        assert_eq!(
            session.project_root.as_deref(),
            Some(stale.to_string_lossy().as_ref())
        );
    }

    #[cfg(not(feature = "no-jail"))]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn resolve_path_auto_reroots_from_agent_config_dir_without_opt_in() {
        // #580: the MCP server is launched from an agent/IDE config dir
        // (~/.copilot-style) and wrongly adopts it as the root. With no
        // `allow_auto_reroot` opt-in and no trusted startup root, the first
        // absolute path into a real project must still correct the root — an
        // agent config dir is never a real jail boundary.
        let _iso = crate::core::data_dir::isolated_data_dir();
        crate::test_env::remove_var("LEAN_CTX_ALLOW_REROOT");
        let tmp = tempfile::tempdir().unwrap();
        let agent = tmp.path().join(".copilot");
        let real = tmp.path().join("repo");
        std::fs::create_dir_all(&agent).unwrap();
        let real_root = create_git_root(&real);
        std::fs::write(real.join("a.txt"), "ok").unwrap();

        let server = LeanCtxServer::new_with_startup(
            None,
            None,
            SessionMode::Personal,
            "default",
            "default",
        );
        {
            let mut session = server.session.write().await;
            session.project_root = Some(agent.to_string_lossy().to_string());
            session.shell_cwd = Some(agent.to_string_lossy().to_string());
        }

        let out = server
            .resolve_path(&real.join("a.txt").to_string_lossy())
            .await
            .unwrap();
        assert!(
            out.ends_with("/a.txt"),
            "agent-dir jail must auto-correct: {out}"
        );

        let session = server.session.read().await;
        assert_eq!(session.project_root.as_deref(), Some(real_root.as_str()));
    }

    #[cfg(not(feature = "no-jail"))]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn resolve_path_agent_dir_still_blocks_markerless_escape() {
        // The agent-dir bypass only reroots to a *real* project (one carrying a
        // marker). A markerless absolute path outside the jail stays blocked —
        // PathJail enforcement is unchanged, only the root *choice* is corrected.
        let _iso = crate::core::data_dir::isolated_data_dir();
        crate::test_env::remove_var("LEAN_CTX_ALLOW_REROOT");
        let tmp = tempfile::tempdir().unwrap();
        let agent = tmp.path().join(".copilot");
        let loose = tmp.path().join("loose");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::create_dir_all(&loose).unwrap();
        std::fs::write(loose.join("data.txt"), "no").unwrap();

        let server = LeanCtxServer::new_with_startup(
            None,
            None,
            SessionMode::Personal,
            "default",
            "default",
        );
        {
            let mut session = server.session.write().await;
            session.project_root = Some(agent.to_string_lossy().to_string());
            session.shell_cwd = Some(agent.to_string_lossy().to_string());
        }

        let err = server
            .resolve_path(&loose.join("data.txt").to_string_lossy())
            .await
            .unwrap_err();
        assert!(err.contains("path escapes project root"), "got: {err}");

        let session = server.session.read().await;
        assert_eq!(
            session.project_root.as_deref(),
            Some(agent.to_string_lossy().as_ref())
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn startup_prefers_workspace_scoped_session_over_global_latest() {
        let _lock = crate::core::data_dir::test_env_lock();
        let _data = tempfile::tempdir().unwrap();
        let _tmp = tempfile::tempdir().unwrap();

        crate::test_env::set_var("LEAN_CTX_DATA_DIR", _data.path());

        let repo_a = _tmp.path().join("repo-a");
        let repo_b = _tmp.path().join("repo-b");
        let root_a = create_git_root(&repo_a);
        let root_b = create_git_root(&repo_b);

        let mut session_b = crate::core::session::SessionState::new();
        session_b.project_root = Some(root_b.clone());
        session_b.shell_cwd = Some(root_b.clone());
        session_b.set_task("repo-b task", None);
        session_b.save().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(50));

        let mut session_a = crate::core::session::SessionState::new();
        session_a.project_root = Some(root_a.clone());
        session_a.shell_cwd = Some(root_a.clone());
        session_a.set_task("repo-a latest task", None);
        session_a.save().unwrap();

        let server = LeanCtxServer::new_with_startup(
            None,
            Some(repo_b.as_path()),
            SessionMode::Personal,
            "default",
            "default",
        );
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");

        let session = server.session.read().await;
        assert_eq!(session.project_root.as_deref(), Some(root_b.as_str()));
        assert_eq!(session.shell_cwd.as_deref(), Some(root_b.as_str()));
        assert_eq!(
            session.task.as_ref().map(|t| t.description.as_str()),
            Some("repo-b task")
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn startup_creates_fresh_session_for_new_workspace_and_preserves_subdir_cwd() {
        let _lock = crate::core::data_dir::test_env_lock();
        let _data = tempfile::tempdir().unwrap();
        let _tmp = tempfile::tempdir().unwrap();

        crate::test_env::set_var("LEAN_CTX_DATA_DIR", _data.path());

        let repo_a = _tmp.path().join("repo-a");
        let repo_b = _tmp.path().join("repo-b");
        let repo_b_src = repo_b.join("src");
        let root_a = create_git_root(&repo_a);
        let root_b = create_git_root(&repo_b);
        std::fs::create_dir_all(&repo_b_src).unwrap();
        let repo_b_src_value = canonicalize_path(&repo_b_src);

        let mut session_a = crate::core::session::SessionState::new();
        session_a.project_root = Some(root_a.clone());
        session_a.shell_cwd = Some(root_a.clone());
        session_a.set_task("repo-a latest task", None);
        let old_id = session_a.id.clone();
        session_a.save().unwrap();

        let server = LeanCtxServer::new_with_startup(
            None,
            Some(repo_b_src.as_path()),
            SessionMode::Personal,
            "default",
            "default",
        );
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");

        let session = server.session.read().await;
        assert_eq!(session.project_root.as_deref(), Some(root_b.as_str()));
        assert_eq!(
            session.shell_cwd.as_deref(),
            Some(repo_b_src_value.as_str())
        );
        assert!(session.task.is_none());
        assert_ne!(session.id, old_id);
    }

    #[cfg(not(feature = "no-jail"))]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn resolve_path_does_not_auto_update_when_current_root_is_real_project() {
        // Hermetic config + serialized via test_env_lock so a parallel test that
        // flips `path_jail` cannot disable this jail-enforcement assertion (#406).
        let _iso = crate::core::data_dir::isolated_data_dir();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let other = tmp.path().join("other");
        let root_value = create_git_root(&root);
        create_git_root(&other);
        std::fs::write(other.join("b.txt"), "no").unwrap();

        let root_str = root.to_string_lossy().to_string();
        let server = LeanCtxServer::new_with_project_root(Some(&root_str));

        let err = server
            .resolve_path(&other.join("b.txt").to_string_lossy())
            .await
            .unwrap_err();
        assert!(err.contains("path escapes project root"));

        let session = server.session.read().await;
        assert_eq!(session.project_root.as_deref(), Some(root_value.as_str()));
    }
}
