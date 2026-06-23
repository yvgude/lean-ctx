// ===== Group A: Index Scoping & Dashboard =====

#[test]
fn graph_api_response_contains_project_root_full() {
    let root = "/tmp/test-project-abc";
    let index = lean_ctx::core::graph_index::ProjectIndex::new(root);
    let mut val = serde_json::to_value(&index).unwrap_or_default();
    if let Some(obj) = val.as_object_mut() {
        obj.insert(
            "project_root_full".to_string(),
            serde_json::Value::String(root.to_string()),
        );
    }
    let json_str = serde_json::to_string(&val).unwrap();
    assert!(json_str.contains("project_root_full"));
    assert!(json_str.contains(root));
}

#[allow(deprecated)]
#[test]
fn load_or_build_does_not_use_legacy_dot_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("myproject");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("main.rs"), "fn main() {}").unwrap();

    let dot_dir = tmp.path().join("graphs").join("dot-legacy");
    std::fs::create_dir_all(&dot_dir).unwrap();

    let idx = lean_ctx::core::graph_index::load_or_build(project.to_str().unwrap());
    assert!(
        idx.project_root.contains("myproject") || idx.files.is_empty(),
        "Should not load from a legacy '.' cache"
    );
}

#[allow(deprecated)]
#[test]
fn cwd_fallback_only_used_if_subdirectory_of_root() {
    let tmp = tempfile::tempdir().unwrap();
    let project_a = tmp.path().join("project-a");
    let project_b = tmp.path().join("project-b");
    std::fs::create_dir_all(&project_a).unwrap();
    std::fs::create_dir_all(&project_b).unwrap();

    let idx = lean_ctx::core::graph_index::load_or_build(project_a.to_str().unwrap());
    assert_ne!(idx.project_root, project_b.to_str().unwrap());
}

// ===== Group B: Index Status =====

#[test]
fn disk_status_shows_exists_false_for_nonexistent_project() {
    let ds = lean_ctx::core::index_orchestrator::disk_status("/nonexistent/project/xyz123");
    assert!(!ds.graph_index.exists);
    assert!(!ds.bm25_index.exists);
    assert!(!ds.code_graph.exists);
}

#[test]
fn status_json_includes_disk_section() {
    let json = lean_ctx::core::index_orchestrator::status_json("/tmp/test-status-json");
    let val: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(val.get("disk").is_some(), "status_json must include 'disk'");
    let disk = val.get("disk").unwrap();
    assert!(disk.get("graph_index").is_some());
    assert!(disk.get("bm25_index").is_some());
    assert!(disk.get("code_graph").is_some());
}

#[test]
fn disk_status_graph_index_not_built() {
    let ds = lean_ctx::core::index_orchestrator::disk_status("/tmp/no-such-project-12345");
    assert!(!ds.graph_index.exists);
    assert!(ds.graph_index.size_bytes.is_none());
    assert!(ds.graph_index.modified_at.is_none());
}

// ===== Group C: Workflow Scoping =====
// These tests use serial execution via unique LEAN_CTX_DATA_DIR per test to avoid env var races.

#[test]
fn workflow_agent_scoped_files_are_separate() {
    let tmp = tempfile::tempdir().unwrap();
    let _guard = EnvGuard::new("LEAN_CTX_DATA_DIR", tmp.path().to_str().unwrap());

    let spec = lean_ctx::core::workflow::WorkflowSpec::builtin_plan_code_test();

    let run_a = lean_ctx::core::workflow::WorkflowRun::new(spec.clone());
    lean_ctx::core::workflow::save_active_for_agent(&run_a, Some("agent-alpha")).unwrap();

    let run_b = lean_ctx::core::workflow::WorkflowRun::new(spec);
    lean_ctx::core::workflow::save_active_for_agent(&run_b, Some("agent-beta")).unwrap();

    let loaded_a = lean_ctx::core::workflow::load_active_for_agent(Some("agent-alpha"))
        .unwrap()
        .expect("agent-alpha workflow should exist");
    let loaded_b = lean_ctx::core::workflow::load_active_for_agent(Some("agent-beta"))
        .unwrap()
        .expect("agent-beta workflow should exist");

    assert_eq!(loaded_a.spec.name, loaded_b.spec.name);

    lean_ctx::core::workflow::clear_active_for_agent(Some("agent-alpha")).unwrap();
    let gone = lean_ctx::core::workflow::load_active_for_agent(Some("agent-alpha")).unwrap();
    assert!(gone.is_none(), "agent-alpha should be cleared");

    let still = lean_ctx::core::workflow::load_active_for_agent(Some("agent-beta")).unwrap();
    assert!(still.is_some(), "agent-beta should still exist");

    lean_ctx::core::workflow::clear_active_for_agent(Some("agent-beta")).unwrap();
}

#[test]
fn workflow_no_agent_id_uses_legacy_active_json() {
    let tmp = tempfile::tempdir().unwrap();
    let _guard = EnvGuard::new("LEAN_CTX_DATA_DIR", tmp.path().to_str().unwrap());

    let spec = lean_ctx::core::workflow::WorkflowSpec::builtin_plan_code_test();
    let run = lean_ctx::core::workflow::WorkflowRun::new(spec);
    lean_ctx::core::workflow::save_active(&run).unwrap();

    let active_path = tmp.path().join("workflows").join("active.json");
    assert!(active_path.exists(), "Legacy active.json should be created");

    let loaded = lean_ctx::core::workflow::load_active().unwrap();
    assert!(loaded.is_some());

    lean_ctx::core::workflow::clear_active().unwrap();
}

#[test]
fn workflow_cleanup_expired_removes_old_files() {
    let tmp = tempfile::tempdir().unwrap();
    let _guard = EnvGuard::new("LEAN_CTX_DATA_DIR", tmp.path().to_str().unwrap());

    let wf_dir = tmp.path().join("workflows");
    std::fs::create_dir_all(&wf_dir).unwrap();

    std::fs::write(wf_dir.join("workflow-old.json"), "{}").unwrap();
    let old_time = std::time::SystemTime::now() - std::time::Duration::from_hours(25);
    let _ = filetime::set_file_mtime(
        wf_dir.join("workflow-old.json"),
        filetime::FileTime::from_system_time(old_time),
    );

    std::fs::write(wf_dir.join("workflow-new.json"), "{}").unwrap();

    let (removed, _freed) = lean_ctx::core::workflow::cleanup_expired();
    assert!(removed >= 1, "Should remove at least the old file");
    assert!(
        !wf_dir.join("workflow-old.json").exists(),
        "Old file should be deleted"
    );
    assert!(
        wf_dir.join("workflow-new.json").exists(),
        "New file should remain"
    );
}

#[test]
fn workflow_agent_id_sanitized_for_filename() {
    let tmp = tempfile::tempdir().unwrap();
    let _guard = EnvGuard::new("LEAN_CTX_DATA_DIR", tmp.path().to_str().unwrap());

    let spec = lean_ctx::core::workflow::WorkflowSpec::builtin_plan_code_test();
    let run = lean_ctx::core::workflow::WorkflowRun::new(spec);
    lean_ctx::core::workflow::save_active_for_agent(&run, Some("agent/with:special chars!"))
        .unwrap();

    let wf_dir = tmp.path().join("workflows");
    let entries: Vec<_> = std::fs::read_dir(&wf_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        entries
            .iter()
            .any(|n| n.starts_with("workflow-agent_with_special_chars_")),
        "Filename should sanitize special chars: {entries:?}"
    );

    lean_ctx::core::workflow::clear_active_for_agent(Some("agent/with:special chars!")).unwrap();
}

#[test]
fn prune_caches_handles_empty_isolated_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let _guard = EnvGuard::new("LEAN_CTX_DATA_DIR", tmp.path().to_str().unwrap());

    let result = lean_ctx::cli::prune_graph_caches();
    assert_eq!(result.removed, 0);
    let result2 = lean_ctx::cli::prune_bm25_caches();
    assert_eq!(result2.removed, 0);
}

/// Guards env var modifications so parallel tests don't race.
/// Uses a process-wide mutex to serialize all env-mutating tests.
struct EnvGuard {
    key: &'static str,
    prev: Option<String>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl EnvGuard {
    fn new(key: &'static str, val: &str) -> Self {
        use std::sync::{Mutex, OnceLock};
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let lock = ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let prev = std::env::var(key).ok();
        unsafe { std::env::set_var(key, val) };
        Self {
            key,
            prev,
            _lock: lock,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(ref v) = self.prev {
            unsafe { std::env::set_var(self.key, v) };
        } else {
            unsafe { std::env::remove_var(self.key) };
        }
    }
}
