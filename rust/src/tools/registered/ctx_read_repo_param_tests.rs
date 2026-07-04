//! `repo` param tests for `ctx_read` (#696), split out of `ctx_read.rs` to
//! keep that file under the LOC gate's 1500-line cap (#660).

use super::*;

/// #696: `repo=<alias>` must resolve `path` against *that* repo's root,
/// jailed there — and because the cache key is the resolved absolute
/// path, reading the same relative path from two different repos must
/// never collide (each must see its own file's content).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repo_param_resolves_against_repo_root_no_cache_collision() {
    use crate::core::cache::SessionCache;
    use crate::core::session::SessionState;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    std::fs::write(dir_a.path().join("shared.rs"), "fn a() {}\n").unwrap();
    std::fs::write(dir_b.path().join("shared.rs"), "fn b() {}\n").unwrap();

    let alias_a = "test-repo-696-a";
    let alias_b = "test-repo-696-b";
    {
        let manager = crate::core::multi_repo::global_manager();
        let mut mgr = manager.lock().unwrap();
        mgr.add_root(&dir_a.path().to_string_lossy(), Some(alias_a))
            .unwrap();
        mgr.add_root(&dir_b.path().to_string_lossy(), Some(alias_b))
            .unwrap();
    }

    let cache: Arc<RwLock<SessionCache>> = Arc::new(RwLock::new(SessionCache::new()));
    let session = Arc::new(RwLock::new(SessionState::new()));
    let ctx = ToolContext {
        project_root: dir_a.path().to_string_lossy().to_string(),
        extra_roots: Vec::new(),
        minimal: false,
        resolved_paths: std::collections::HashMap::new(),
        crp_mode: crate::tools::CrpMode::Off,
        cache: Some(cache),
        session: Some(session),
        tool_calls: None,
        agent_id: None,
        workflow: None,
        ledger: None,
        client_name: None,
        pipeline_stats: None,
        call_count: None,
        autonomy: None,
        pressure_snapshot: None,
        path_errors: std::collections::HashMap::new(),
        bm25_cache: None,
        progress_sender: None,
    };

    let args_a = json!({ "repo": alias_a, "path": "shared.rs", "mode": "full" })
        .as_object()
        .unwrap()
        .clone();
    let out_a = tokio::task::block_in_place(|| CtxReadTool.handle(&args_a, &ctx))
        .expect("repo=a read failed");
    assert!(out_a.text.contains("fn a()"), "got: {}", out_a.text);

    let args_b = json!({ "repo": alias_b, "path": "shared.rs", "mode": "full" })
        .as_object()
        .unwrap()
        .clone();
    let out_b = tokio::task::block_in_place(|| CtxReadTool.handle(&args_b, &ctx))
        .expect("repo=b read failed");
    assert!(out_b.text.contains("fn b()"), "got: {}", out_b.text);
    assert!(
        !out_b.text.contains("fn a()"),
        "repo=b must not see repo=a's cached content: {}",
        out_b.text
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repo_param_unknown_alias_errors_with_known_aliases() {
    use crate::core::cache::SessionCache;
    use crate::core::session::SessionState;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    {
        let manager = crate::core::multi_repo::global_manager();
        let mut mgr = manager.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        mgr.add_root(&dir.path().to_string_lossy(), Some("test-repo-696-known"))
            .ok();
        std::mem::forget(dir); // keep the tempdir alive for the process
    }

    let cache: Arc<RwLock<SessionCache>> = Arc::new(RwLock::new(SessionCache::new()));
    let session = Arc::new(RwLock::new(SessionState::new()));
    let ctx = ToolContext {
        project_root: ".".to_string(),
        extra_roots: Vec::new(),
        minimal: false,
        resolved_paths: std::collections::HashMap::new(),
        crp_mode: crate::tools::CrpMode::Off,
        cache: Some(cache),
        session: Some(session),
        tool_calls: None,
        agent_id: None,
        workflow: None,
        ledger: None,
        client_name: None,
        pipeline_stats: None,
        call_count: None,
        autonomy: None,
        pressure_snapshot: None,
        path_errors: std::collections::HashMap::new(),
        bm25_cache: None,
        progress_sender: None,
    };

    let args = json!({ "repo": "this-alias-does-not-exist", "path": "x.rs", "mode": "full" })
        .as_object()
        .unwrap()
        .clone();
    let result = tokio::task::block_in_place(|| CtxReadTool.handle(&args, &ctx));
    let Err(err) = result else {
        panic!("unknown repo alias must error, not fall back to project root");
    };
    let msg = err.message.to_string();
    assert!(msg.contains("unknown repo alias"), "got: {msg}");
    assert!(
        msg.contains("test-repo-696-known"),
        "error must name a known alias so the caller isn't guessing: {msg}"
    );
}
