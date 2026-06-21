//! Scenario tests for issues reported by `BazsiBazsi`:
//!
//! 1. Workflow persists after agent crash → blocks `ctx_multi_read` in next session
//! 2. Cache-hit message is misleading when subagent cached the file
//!
//! These tests simulate real multi-session scenarios to verify the fixes.

use chrono::{Duration, Utc};
use lean_ctx::core::protocol::CrpMode;
use lean_ctx::core::workflow::types::{StateSpec, TransitionSpec, WorkflowRun, WorkflowSpec};
use lean_ctx::core::workflow::{load_active, save_active};
use lean_ctx::server::{WORKFLOW_PASSTHROUGH_TOOLS, is_workflow_stale};
use serial_test::serial;
use std::io::Write;

fn bazsi_workflow_spec() -> WorkflowSpec {
    WorkflowSpec {
        name: "feature-dev".to_string(),
        description: Some("Feature development workflow".to_string()),
        initial: "planning".to_string(),
        states: vec![
            StateSpec {
                name: "planning".to_string(),
                description: Some("Plan the feature".to_string()),
                allowed_tools: Some(vec!["ctx_shell".to_string(), "ctx_workflow".to_string()]),
                requires_evidence: None,
            },
            StateSpec {
                name: "implementing".to_string(),
                description: Some("Write the code".to_string()),
                allowed_tools: Some(vec![
                    "ctx_shell".to_string(),
                    "ctx_edit".to_string(),
                    "ctx_workflow".to_string(),
                ]),
                requires_evidence: None,
            },
            StateSpec {
                name: "reviewing".to_string(),
                description: Some("Review phase".to_string()),
                allowed_tools: Some(vec!["ctx_shell".to_string(), "ctx_workflow".to_string()]),
                requires_evidence: None,
            },
        ],
        transitions: vec![
            TransitionSpec {
                from: "planning".to_string(),
                to: "implementing".to_string(),
                description: None,
            },
            TransitionSpec {
                from: "implementing".to_string(),
                to: "reviewing".to_string(),
                description: None,
            },
        ],
    }
}

fn setup_data_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("lean_ctx_bazsi_test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", dir.to_str().unwrap()) };
    dir
}

fn cleanup_data_dir() {
    let dir = std::env::temp_dir().join("lean_ctx_bazsi_test");
    let _ = std::fs::remove_dir_all(&dir);
    unsafe { std::env::remove_var("LEAN_CTX_DATA_DIR") };
}

// ==========================================================================
// SCENARIO 1: Agent crashes mid-workflow, new session is NOT blocked
// ==========================================================================
// Bazsi's problem: "if you crash/the agent becomes unstable/stops, you get
// stuck in the workflow, so it doesn't terminate once you leave the conversation"

#[test]
#[serial]
fn scenario_crash_mid_workflow_stale_after_30min() {
    setup_data_dir();

    // Simulate: Agent was in "implementing" state and crashed 45 minutes ago
    let spec = bazsi_workflow_spec();
    let mut run = WorkflowRun::new(spec);
    run.current = "implementing".to_string();
    run.updated_at = Utc::now() - Duration::minutes(45);
    save_active(&run).unwrap();

    // Next session: load_active should return None (auto-cleared)
    let loaded = load_active().unwrap();
    assert!(
        loaded.is_none(),
        "Workflow crashed 45min ago should be auto-cleared on load"
    );

    cleanup_data_dir();
}

#[test]
#[serial]
fn scenario_crash_mid_workflow_still_valid_within_30min() {
    setup_data_dir();

    // Agent was in "implementing" state, crashed 10 minutes ago
    let spec = bazsi_workflow_spec();
    let mut run = WorkflowRun::new(spec);
    run.current = "implementing".to_string();
    run.updated_at = Utc::now() - Duration::minutes(10);
    save_active(&run).unwrap();

    // Within 30min window: workflow should still be active
    let loaded = load_active().unwrap();
    assert!(
        loaded.is_some(),
        "Workflow crashed 10min ago should still be active"
    );
    assert_eq!(loaded.unwrap().current, "implementing");

    cleanup_data_dir();
}

#[test]
#[serial]
fn scenario_crash_at_boundary_29min_still_active() {
    setup_data_dir();

    let spec = bazsi_workflow_spec();
    let mut run = WorkflowRun::new(spec);
    run.current = "planning".to_string();
    run.updated_at = Utc::now() - Duration::minutes(29);
    save_active(&run).unwrap();

    let loaded = load_active().unwrap();
    assert!(loaded.is_some(), "29min workflow should still be active");

    cleanup_data_dir();
}

#[test]
#[serial]
fn scenario_crash_at_boundary_31min_expired() {
    setup_data_dir();

    let spec = bazsi_workflow_spec();
    let mut run = WorkflowRun::new(spec);
    run.current = "planning".to_string();
    run.updated_at = Utc::now() - Duration::minutes(31);
    save_active(&run).unwrap();

    let loaded = load_active().unwrap();
    assert!(loaded.is_none(), "31min workflow should be expired");

    cleanup_data_dir();
}

// ==========================================================================
// SCENARIO 2: ctx_read / ctx_multi_read NEVER blocked by workflow
// ==========================================================================
// Bazsi's problem: "it also limits the multi-read tool, which is useful
// across any phase"

#[test]
fn scenario_read_tools_in_passthrough_list() {
    // ctx_read and ctx_multi_read must ALWAYS be in passthrough
    assert!(
        WORKFLOW_PASSTHROUGH_TOOLS.contains(&"ctx_read"),
        "ctx_read must always pass through workflow gate"
    );
    assert!(
        WORKFLOW_PASSTHROUGH_TOOLS.contains(&"ctx_multi_read"),
        "ctx_multi_read must always pass through workflow gate"
    );
    assert!(
        WORKFLOW_PASSTHROUGH_TOOLS.contains(&"ctx_smart_read"),
        "ctx_smart_read must always pass through workflow gate"
    );
}

#[test]
fn scenario_search_and_tree_also_passthrough() {
    // Agents need search/tree for context recovery
    assert!(WORKFLOW_PASSTHROUGH_TOOLS.contains(&"ctx_search"));
    assert!(WORKFLOW_PASSTHROUGH_TOOLS.contains(&"ctx_tree"));
}

#[test]
fn scenario_write_tools_not_in_passthrough() {
    // Write tools should still be gated
    assert!(!WORKFLOW_PASSTHROUGH_TOOLS.contains(&"ctx_shell"));
    assert!(!WORKFLOW_PASSTHROUGH_TOOLS.contains(&"ctx_edit"));
    assert!(!WORKFLOW_PASSTHROUGH_TOOLS.contains(&"ctx_write"));
}

#[test]
fn scenario_workflow_with_restricted_tools_still_allows_reads() {
    // Simulate: workflow in "planning" state only allows ctx_shell + ctx_workflow
    let spec = bazsi_workflow_spec();
    let state = spec.state("planning").unwrap();
    let allowed = state.allowed_tools.as_ref().unwrap();

    // ctx_read is NOT in the allowed list
    assert!(!allowed.contains(&"ctx_read".to_string()));
    assert!(!allowed.contains(&"ctx_multi_read".to_string()));

    // But it IS in passthrough — so the gate won't block it
    assert!(WORKFLOW_PASSTHROUGH_TOOLS.contains(&"ctx_read"));
    assert!(WORKFLOW_PASSTHROUGH_TOOLS.contains(&"ctx_multi_read"));
}

// ==========================================================================
// SCENARIO 3: Staleness detection function works correctly
// ==========================================================================

#[test]
fn scenario_is_workflow_stale_fresh() {
    let spec = bazsi_workflow_spec();
    let run = WorkflowRun::new(spec);
    // Just created — should NOT be stale
    assert!(!is_workflow_stale(&run));
}

#[test]
fn scenario_is_workflow_stale_25min() {
    let spec = bazsi_workflow_spec();
    let mut run = WorkflowRun::new(spec);
    run.updated_at = Utc::now() - Duration::minutes(25);
    assert!(!is_workflow_stale(&run), "25min should not be stale");
}

#[test]
fn scenario_is_workflow_stale_35min() {
    let spec = bazsi_workflow_spec();
    let mut run = WorkflowRun::new(spec);
    run.updated_at = Utc::now() - Duration::minutes(35);
    assert!(is_workflow_stale(&run), "35min should be stale");
}

#[test]
fn scenario_is_workflow_stale_hours() {
    let spec = bazsi_workflow_spec();
    let mut run = WorkflowRun::new(spec);
    run.updated_at = Utc::now() - Duration::hours(3);
    assert!(
        is_workflow_stale(&run),
        "3 hours should definitely be stale"
    );
}

// ==========================================================================
// SCENARIO 4: Cache-hit message is informative, not misleading
// ==========================================================================
// Bazsi's problem: "the cache even if invalidated sometimes returns that
// the content has already been cached, while i did not see the read"

#[test]
fn scenario_cache_hit_message_format() {
    use lean_ctx::core::cache::SessionCache;

    let mut cache = SessionCache::new();
    let dir = std::env::temp_dir().join("lean_ctx_bazsi_cache_test");
    let _ = std::fs::create_dir_all(&dir);
    let file_path = dir.join("test_file.py");
    {
        let mut f = std::fs::File::create(&file_path).unwrap();
        writeln!(f, "def hello():").unwrap();
        writeln!(f, "    return 'world'").unwrap();
    }
    let path_str = file_path.to_str().unwrap();

    // First read: should return full content
    let result1 = lean_ctx::tools::ctx_read::handle(&mut cache, path_str, "full", CrpMode::Off);
    assert!(
        !result1.contains("[unchanged"),
        "First read should NOT say unchanged: got {result1}"
    );
    assert!(
        result1.contains("hello") || result1.contains("def"),
        "First read should contain file content"
    );

    // Second read (cache hit): should use new message format
    let result2 = lean_ctx::tools::ctx_read::handle(&mut cache, path_str, "full", CrpMode::Off);
    assert!(
        result2.contains("[unchanged") || result2.contains("use cached context"),
        "Cache hit should use new message format: got {result2}"
    );
    // Must NOT say "Already in your context window"
    assert!(
        !result2.contains("Already in your context window"),
        "Old misleading message must not appear: got {result2}"
    );
    // Should show unchanged indicator or hint at fresh=true
    assert!(
        result2.contains("fresh=true")
            || result2.contains("cached context")
            || result2.contains("[unchanged"),
        "Should hint about fresh=true, cached context, or unchanged: got {result2}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[serial]
fn scenario_cache_hit_meta_visible_format() {
    use lean_ctx::core::cache::SessionCache;

    // Enable meta_visible mode via env var
    unsafe { std::env::set_var("LEAN_CTX_META", "1") };

    let mut cache = SessionCache::new();
    let dir = std::env::temp_dir().join("lean_ctx_bazsi_meta_test");
    let _ = std::fs::create_dir_all(&dir);
    let file_path = dir.join("meta_test.rs");
    {
        let mut f = std::fs::File::create(&file_path).unwrap();
        writeln!(f, "fn main() {{}}").unwrap();
    }
    let path_str = file_path.to_str().unwrap();

    // First read
    let _ = lean_ctx::tools::ctx_read::handle(&mut cache, path_str, "full", CrpMode::Off);

    // Second read (meta_visible mode)
    let result = lean_ctx::tools::ctx_read::handle(&mut cache, path_str, "full", CrpMode::Off);

    // Meta-visible format should mention "unchanged on disk"
    assert!(
        result.contains("unchanged") || result.contains("File unchanged on disk"),
        "Meta-visible cache hit should say 'unchanged on disk': got {result}"
    );
    assert!(
        !result.contains("Already in your context window"),
        "Old message must not appear in meta-visible mode: got {result}"
    );
    assert!(
        result.contains("fresh=true"),
        "Meta-visible cache hit should hint fresh=true: got {result}"
    );

    unsafe { std::env::remove_var("LEAN_CTX_META") };
    let _ = std::fs::remove_dir_all(&dir);
}

// ==========================================================================
// SCENARIO 5: Workflow file cleanup on disk
// ==========================================================================

#[test]
#[serial]
fn scenario_stale_workflow_file_removed_from_disk() {
    let dir = setup_data_dir();

    let spec = bazsi_workflow_spec();
    let mut run = WorkflowRun::new(spec);
    run.current = "implementing".to_string();
    run.updated_at = Utc::now() - Duration::minutes(40);
    save_active(&run).unwrap();

    let wf_path = dir.join("workflows").join("active.json");
    assert!(wf_path.exists(), "Workflow file should exist before load");

    // Load triggers auto-clear
    let loaded = load_active().unwrap();
    assert!(loaded.is_none());
    assert!(
        !wf_path.exists(),
        "Stale workflow file should be physically deleted from disk"
    );

    cleanup_data_dir();
}

#[test]
#[serial]
fn scenario_done_workflow_file_removed_from_disk() {
    let dir = setup_data_dir();

    let spec = bazsi_workflow_spec();
    let mut run = WorkflowRun::new(spec);
    run.current = "done".to_string();
    run.updated_at = Utc::now(); // even if just updated
    save_active(&run).unwrap();

    let wf_path = dir.join("workflows").join("active.json");
    assert!(wf_path.exists(), "Workflow file should exist");

    let loaded = load_active().unwrap();
    assert!(loaded.is_none(), "'done' workflow should be auto-cleared");
    assert!(
        !wf_path.exists(),
        "'done' workflow file should be deleted from disk"
    );

    cleanup_data_dir();
}

// ==========================================================================
// SCENARIO 6: Multiple agents / session scenario
// ==========================================================================
// Simulates: Agent A starts workflow, crashes. Agent B starts fresh session.

#[test]
#[serial]
fn scenario_agent_b_not_blocked_after_agent_a_crash() {
    setup_data_dir();

    // Agent A: starts workflow, works for 5 minutes, then crashes
    let spec = bazsi_workflow_spec();
    let mut run = WorkflowRun::new(spec);
    run.current = "implementing".to_string();
    run.updated_at = Utc::now() - Duration::minutes(45); // crashed 45min ago
    save_active(&run).unwrap();

    // Agent B: starts new session, tries to read files
    // load_active returns None → no blocking
    let loaded = load_active().unwrap();
    assert!(
        loaded.is_none(),
        "Agent B should not see Agent A's stale workflow"
    );

    // Even if somehow loaded, passthrough tools wouldn't be blocked
    assert!(WORKFLOW_PASSTHROUGH_TOOLS.contains(&"ctx_read"));
    assert!(WORKFLOW_PASSTHROUGH_TOOLS.contains(&"ctx_multi_read"));

    cleanup_data_dir();
}

#[test]
#[serial]
fn scenario_agent_b_sees_active_workflow_if_recent() {
    setup_data_dir();

    // Agent A: started workflow 5 minutes ago, then session ended normally
    let spec = bazsi_workflow_spec();
    let mut run = WorkflowRun::new(spec);
    run.current = "implementing".to_string();
    run.updated_at = Utc::now() - Duration::minutes(5);
    save_active(&run).unwrap();

    // Agent B: picks up the workflow (it's still fresh)
    let loaded = load_active().unwrap();
    assert!(
        loaded.is_some(),
        "Recent workflow should still be active for Agent B"
    );
    let run = loaded.unwrap();
    assert_eq!(run.current, "implementing");

    // But ctx_read/ctx_multi_read are still allowed via passthrough
    assert!(WORKFLOW_PASSTHROUGH_TOOLS.contains(&"ctx_read"));
    assert!(WORKFLOW_PASSTHROUGH_TOOLS.contains(&"ctx_multi_read"));

    cleanup_data_dir();
}
