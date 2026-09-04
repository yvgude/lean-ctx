#![allow(clippy::await_holding_lock)]

use serde_json::json;

fn setup_project() -> (tempfile::TempDir, lean_ctx::engine::ContextEngine) {
    let dir = tempfile::tempdir().expect("tempdir");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&project).expect("create project");

    std::fs::create_dir_all(project.join("src")).expect("create src");
    std::fs::write(
        project.join("src/main.rs"),
        r#"fn main() {
    let config = load_config();
    println!("Running with {:?}", config);
}

fn load_config() -> Config {
    Config { debug: false, port: 8080 }
}

#[derive(Debug)]
struct Config {
    debug: bool,
    port: u16,
}

fn helper_a() -> String {
    "hello".to_string()
}

fn helper_b(x: i32) -> i32 {
    x * 2
}
"#,
    )
    .expect("write main.rs");

    std::fs::write(
        project.join("src/lib.rs"),
        r#"pub mod utils;

pub fn greet(name: &str) -> String {
    format!("Hello, {name}!")
}

pub fn add(a: i32, b: i32) -> i32 {
    a + b
}
"#,
    )
    .expect("write lib.rs");

    std::fs::create_dir_all(project.join("src/utils")).expect("create utils");
    std::fs::write(
        project.join("src/utils/mod.rs"),
        r#"pub fn format_bytes(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    }
}
"#,
    )
    .expect("write utils/mod.rs");

    std::fs::write(
        project.join("Cargo.toml"),
        r#"[package]
name = "test-project"
version = "0.1.0"
edition = "2021"
"#,
    )
    .expect("write Cargo.toml");

    std::fs::write(
        project.join("README.md"),
        "# Test Project\nA sample project.\n",
    )
    .expect("write README.md");

    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    // SAFETY: serialized by `test_env_lock()`.
    unsafe {
        std::env::set_var("LEAN_CTX_DATA_DIR", data_dir.to_string_lossy().to_string());
    }

    let engine = lean_ctx::engine::ContextEngine::with_project_root(&project);
    (dir, engine)
}

// ── Phase 1: Setup & Orientation ───────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phase1_session_task_and_overview() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let (_dir, engine) = setup_project();

    let out = engine
        .call_tool_text(
            "ctx_session",
            Some(json!({"action": "task", "content": "Implement caching layer"})),
        )
        .await
        .expect("session task");
    assert!(
        out.contains("task") || out.contains("Task") || out.contains("set"),
        "task output: {out}"
    );

    let overview = engine
        .call_tool_text("ctx_overview", None)
        .await
        .expect("overview");
    assert!(
        overview.contains("src") || overview.contains("project"),
        "overview should contain project structure: {overview}"
    );

    // SAFETY: serialized by `test_env_lock()`.
    unsafe {
        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phase1_tree_depth() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let (_dir, engine) = setup_project();
    let project = _dir.path().join("project");

    let tree = engine
        .call_tool_text("ctx_tree", Some(json!({"path": project, "depth": 3})))
        .await
        .expect("tree");
    assert!(tree.contains("src"), "tree should show src: {tree}");
    assert!(
        tree.contains("Cargo.toml") || tree.contains("main.rs"),
        "tree should show files: {tree}"
    );

    // SAFETY: serialized by `test_env_lock()`.
    unsafe {
        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }
}

// ── Phase 2: Codebase Research (Read Modes) ────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phase2_read_full_and_cache_stub() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let (_dir, engine) = setup_project();

    let project = _dir.path().join("project");
    let main_path = project.join("src/main.rs").to_string_lossy().to_string();

    let full1 = engine
        .call_tool_text("ctx_read", Some(json!({"path": main_path, "mode": "full"})))
        .await
        .expect("full read 1");
    assert!(
        full1.contains("load_config"),
        "full read should contain fn body: {full1}"
    );

    let full2 = engine
        .call_tool_text("ctx_read", Some(json!({"path": main_path, "mode": "full"})))
        .await
        .expect("full read 2");
    assert!(
        full2.len() <= full1.len(),
        "re-read should be stub or same size (full1={}, full2={})",
        full1.len(),
        full2.len()
    );

    // SAFETY: serialized by `test_env_lock()`.
    unsafe {
        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phase2_read_signatures() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let (_dir, engine) = setup_project();

    let project = _dir.path().join("project");
    let main_path = project.join("src/main.rs").to_string_lossy().to_string();

    let sigs = engine
        .call_tool_text(
            "ctx_read",
            Some(json!({"path": main_path, "mode": "signatures"})),
        )
        .await
        .expect("signatures read");
    assert!(
        sigs.contains("fn main") || sigs.contains("load_config") || sigs.contains("helper_a"),
        "signatures should contain fn names: {sigs}"
    );

    // SAFETY: serialized by `test_env_lock()`.
    unsafe {
        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phase2_read_map() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let (_dir, engine) = setup_project();

    let project = _dir.path().join("project");
    let lib_path = project.join("src/lib.rs").to_string_lossy().to_string();

    let map = engine
        .call_tool_text("ctx_read", Some(json!({"path": lib_path, "mode": "map"})))
        .await
        .expect("map read");
    assert!(!map.is_empty(), "map should not be empty");

    // SAFETY: serialized by `test_env_lock()`.
    unsafe {
        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phase2_read_lines_range() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let (_dir, engine) = setup_project();

    let project = _dir.path().join("project");
    let main_path = project.join("src/main.rs").to_string_lossy().to_string();

    let lines = engine
        .call_tool_text(
            "ctx_read",
            Some(json!({"path": main_path, "mode": "lines:1-5"})),
        )
        .await
        .expect("lines read");
    assert!(
        lines.contains("fn main"),
        "lines 1-5 should contain fn main: {lines}"
    );

    // SAFETY: serialized by `test_env_lock()`.
    unsafe {
        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phase2_read_auto() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let (_dir, engine) = setup_project();

    let project = _dir.path().join("project");
    let main_path = project.join("src/main.rs").to_string_lossy().to_string();

    let auto = engine
        .call_tool_text("ctx_read", Some(json!({"path": main_path, "mode": "auto"})))
        .await
        .expect("auto read");
    assert!(!auto.is_empty(), "auto mode should return content");

    // SAFETY: serialized by `test_env_lock()`.
    unsafe {
        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phase2_read_aggressive() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let (_dir, engine) = setup_project();

    let project = _dir.path().join("project");
    let main_path = project.join("src/main.rs").to_string_lossy().to_string();

    let aggressive = engine
        .call_tool_text(
            "ctx_read",
            Some(json!({"path": main_path, "mode": "aggressive"})),
        )
        .await
        .expect("aggressive read");
    assert!(
        !aggressive.is_empty(),
        "aggressive mode should return content"
    );

    // SAFETY: serialized by `test_env_lock()`.
    unsafe {
        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phase2_read_diff_after_edit() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let (_dir, engine) = setup_project();

    let project = _dir.path().join("project");
    let main_path = project.join("src/main.rs").to_string_lossy().to_string();

    let _ = engine
        .call_tool_text("ctx_read", Some(json!({"path": main_path, "mode": "full"})))
        .await
        .expect("initial read");

    let content = std::fs::read_to_string(project.join("src/main.rs")).unwrap();
    let modified = content.replace("debug: false", "debug: true");
    std::fs::write(project.join("src/main.rs"), &modified).unwrap();

    let diff = engine
        .call_tool_text("ctx_read", Some(json!({"path": main_path, "mode": "diff"})))
        .await
        .expect("diff read");
    assert!(
        diff.contains("debug")
            || diff.contains("changed")
            || diff.contains("modified")
            || diff.contains("true"),
        "diff should show changes: {diff}"
    );

    // SAFETY: serialized by `test_env_lock()`.
    unsafe {
        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }
}

// ── Phase 3: Code Search ───────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phase3_search_regex() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let (_dir, engine) = setup_project();

    let out = engine
        .call_tool_text("ctx_search", Some(json!({"pattern": "fn main"})))
        .await
        .expect("search");
    assert!(
        out.contains("main") || out.contains("src/main.rs"),
        "search should find fn main: {out}"
    );

    // SAFETY: serialized by `test_env_lock()`.
    unsafe {
        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phase3_search_with_path_filter() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let (_dir, engine) = setup_project();

    let out = engine
        .call_tool_text(
            "ctx_search",
            Some(json!({"pattern": "pub fn", "path": "src/utils"})),
        )
        .await
        .expect("search with path");
    assert!(
        out.contains("format_bytes"),
        "filtered search should find format_bytes: {out}"
    );

    // SAFETY: serialized by `test_env_lock()`.
    unsafe {
        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }
}

// ── Phase 4: Shell & Execution ─────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phase4_shell_echo() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let (_dir, engine) = setup_project();

    let out = engine
        .call_tool_text(
            "ctx_shell",
            Some(json!({"command": "echo hello-from-shell"})),
        )
        .await
        .expect("shell echo");
    assert!(
        out.contains("hello-from-shell"),
        "shell should contain echo output: {out}"
    );

    // SAFETY: serialized by `test_env_lock()`.
    unsafe {
        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phase4_shell_raw_mode() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let (_dir, engine) = setup_project();

    let out = engine
        .call_tool_text(
            "ctx_shell",
            Some(json!({"command": "echo raw-test-value", "raw": true})),
        )
        .await
        .expect("shell raw");
    assert!(
        out.contains("raw-test-value"),
        "raw shell should contain output: {out}"
    );

    // SAFETY: serialized by `test_env_lock()`.
    unsafe {
        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }
}

// ── Phase 5: Edit & Iteration ──────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phase5_edit_and_verify() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let (_dir, engine) = setup_project();

    let project = _dir.path().join("project");
    let lib_path = project.join("src/lib.rs").to_string_lossy().to_string();

    let edit_result = engine
        .call_tool_text(
            "ctx_edit",
            Some(json!({
                "path": lib_path,
                "old_string": "pub fn greet(name: &str) -> String {\n    format!(\"Hello, {name}!\")\n}",
                "new_string": "pub fn greet(name: &str) -> String {\n    format!(\"Hi there, {name}!\")\n}",
            })),
        )
        .await
        .expect("edit");
    assert!(
        edit_result.contains("applied")
            || edit_result.contains("ok")
            || edit_result.contains("✓")
            || edit_result.contains("success")
            || !edit_result.is_empty(),
        "edit should succeed: {edit_result}"
    );

    let content = std::fs::read_to_string(project.join("src/lib.rs")).unwrap();
    assert!(
        content.contains("Hi there"),
        "file should contain edited text"
    );

    // SAFETY: serialized by `test_env_lock()`.
    unsafe {
        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }
}

// ── Phase 6: Context Management ────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phase6_compress_and_cache_status() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let (_dir, engine) = setup_project();

    let project = _dir.path().join("project");
    let main_path = project.join("src/main.rs").to_string_lossy().to_string();
    let _ = engine
        .call_tool_text("ctx_read", Some(json!({"path": main_path, "mode": "full"})))
        .await
        .expect("prime cache");

    let compress = engine.call_tool_text("ctx_compress", None).await;
    match compress {
        Ok(out) => assert!(!out.is_empty(), "compress should return output"),
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("timeout") || msg.contains("timed out"),
                "compress unexpected error: {msg}"
            );
        }
    }

    let cache = engine
        .call_tool_text("ctx_cache", Some(json!({"action": "status"})))
        .await
        .expect("cache status");
    let cache_lower = cache.to_lowercase();
    assert!(
        cache_lower.contains("cache")
            || cache_lower.contains("entries")
            || cache_lower.contains("file"),
        "cache status should show info: {cache}"
    );

    // SAFETY: serialized by `test_env_lock()`.
    unsafe {
        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phase6_ledger_status() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let (_dir, engine) = setup_project();

    let ledger = engine
        .call_tool_text("ctx_ledger", Some(json!({"action": "status"})))
        .await
        .expect("ledger status");
    assert!(!ledger.is_empty(), "ledger status should return output");

    // SAFETY: serialized by `test_env_lock()`.
    unsafe {
        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phase6_dedup() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let (_dir, engine) = setup_project();

    let project = _dir.path().join("project");
    let main_path = project.join("src/main.rs").to_string_lossy().to_string();
    let lib_path = project.join("src/lib.rs").to_string_lossy().to_string();
    let _ = engine
        .call_tool_text("ctx_read", Some(json!({"path": main_path, "mode": "full"})))
        .await
        .unwrap();
    let _ = engine
        .call_tool_text("ctx_read", Some(json!({"path": lib_path, "mode": "full"})))
        .await
        .unwrap();

    let dedup = engine
        .call_tool_text("ctx_dedup", None)
        .await
        .expect("dedup");
    assert!(!dedup.is_empty(), "dedup should return output");

    // SAFETY: serialized by `test_env_lock()`.
    unsafe {
        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phase6_plan() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let (_dir, engine) = setup_project();

    let plan = engine
        .call_tool_text(
            "ctx_plan",
            Some(json!({"task": "Add a caching layer to the Config struct"})),
        )
        .await
        .expect("plan");
    assert!(!plan.is_empty(), "plan should return output");

    // SAFETY: serialized by `test_env_lock()`.
    unsafe {
        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }
}

// ── Phase 7: Knowledge & Session ───────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phase7_knowledge_remember_and_recall() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let (_dir, engine) = setup_project();

    let remember = engine
        .call_tool_text(
            "ctx_knowledge",
            Some(json!({
                "action": "remember",
                "category": "architecture",
                "key": "cache_strategy",
                "value": "LRU with zstd compression, 128MB limit",
            })),
        )
        .await
        .expect("knowledge remember");
    assert!(!remember.is_empty(), "remember should return confirmation");

    let recall = engine
        .call_tool_text(
            "ctx_knowledge",
            Some(json!({"action": "recall", "category": "architecture"})),
        )
        .await
        .expect("knowledge recall");
    assert!(
        recall.contains("cache_strategy") || recall.contains("LRU"),
        "recall should contain remembered fact: {recall}"
    );

    // SAFETY: serialized by `test_env_lock()`.
    unsafe {
        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phase7_knowledge_timeline_and_rooms() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let (_dir, engine) = setup_project();

    let _ = engine
        .call_tool_text(
            "ctx_knowledge",
            Some(json!({
                "action": "remember",
                "category": "decisions",
                "key": "db_choice",
                "value": "PostgreSQL for persistence",
            })),
        )
        .await
        .expect("remember decision");

    let timeline = engine
        .call_tool_text(
            "ctx_knowledge",
            Some(json!({"action": "timeline", "category": "decisions"})),
        )
        .await
        .expect("timeline");
    assert!(!timeline.is_empty(), "timeline should return entries");

    let rooms = engine
        .call_tool_text("ctx_knowledge", Some(json!({"action": "rooms"})))
        .await
        .expect("rooms");
    assert!(
        rooms.contains("decisions"),
        "rooms should list the decisions category: {rooms}"
    );

    // SAFETY: serialized by `test_env_lock()`.
    unsafe {
        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phase7_session_finding_and_decision() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let (_dir, engine) = setup_project();

    let finding = engine
        .call_tool_text(
            "ctx_session",
            Some(json!({
                "action": "finding",
                "content": "Config struct lacks validation for port range",
            })),
        )
        .await
        .expect("session finding");
    assert!(!finding.is_empty(), "finding should return confirmation");

    let decision = engine
        .call_tool_text(
            "ctx_session",
            Some(json!({
                "action": "decision",
                "content": "Use builder pattern for Config validation",
            })),
        )
        .await
        .expect("session decision");
    assert!(!decision.is_empty(), "decision should return confirmation");

    // SAFETY: serialized by `test_env_lock()`.
    unsafe {
        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }
}

// ── Phase 8: Multi-Agent Coordination ──────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phase8_agent_register_and_diary() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let (_dir, engine) = setup_project();

    let reg = engine
        .call_tool_text(
            "ctx_agent",
            Some(json!({
                "action": "register",
                "name": "backend-agent",
                "role": "Implements API endpoints",
            })),
        )
        .await
        .expect("agent register");
    assert!(!reg.is_empty(), "register should return confirmation");

    let diary = engine
        .call_tool_text(
            "ctx_agent",
            Some(json!({
                "action": "diary",
                "category": "progress",
                "content": "Completed Config struct refactoring",
            })),
        )
        .await
        .expect("agent diary");
    assert!(!diary.is_empty(), "diary should return confirmation");

    // SAFETY: serialized by `test_env_lock()`.
    unsafe {
        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }
}

// ── Phase 9: Metrics & Verification ────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phase9_metrics() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let (_dir, engine) = setup_project();

    let project = _dir.path().join("project");
    let main_path = project.join("src/main.rs").to_string_lossy().to_string();
    let _ = engine
        .call_tool_text("ctx_read", Some(json!({"path": main_path, "mode": "full"})))
        .await
        .unwrap();

    let metrics = engine
        .call_tool_text("ctx_metrics", None)
        .await
        .expect("metrics");
    assert!(!metrics.is_empty(), "metrics should return data");

    // SAFETY: serialized by `test_env_lock()`.
    unsafe {
        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phase9_session_save() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let (_dir, engine) = setup_project();

    let _ = engine
        .call_tool_text(
            "ctx_session",
            Some(json!({"action": "task", "content": "Final verification"})),
        )
        .await
        .expect("set task");

    let save = engine
        .call_tool_text("ctx_session", Some(json!({"action": "save"})))
        .await
        .expect("session save");
    assert!(!save.is_empty(), "save should return confirmation");

    // SAFETY: serialized by `test_env_lock()`.
    unsafe {
        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }
}
