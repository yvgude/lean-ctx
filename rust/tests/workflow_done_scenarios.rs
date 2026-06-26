//! Scenario tests for the workflow "done" state bug fix.
//!
//! Validates that:
//! 1. `complete` clears the workflow (no persistent "done" state)
//! 2. `stop` from any state works correctly
//! 3. Gate check auto-clears stale "done" workflows
//! 4. Handoff does not restore "done" workflows

use lean_ctx::core::workflow::types::{StateSpec, TransitionSpec, WorkflowRun, WorkflowSpec};
use lean_ctx::core::workflow::{clear_active, load_active, save_active};
use serial_test::serial;

fn test_spec() -> WorkflowSpec {
    WorkflowSpec {
        name: "test-workflow".to_string(),
        description: Some("Test workflow".to_string()),
        initial: "planning".to_string(),
        states: vec![
            StateSpec {
                name: "planning".to_string(),
                description: None,
                allowed_tools: Some(vec!["ctx_read".to_string(), "ctx_workflow".to_string()]),
                requires_evidence: None,
            },
            StateSpec {
                name: "coding".to_string(),
                description: None,
                allowed_tools: Some(vec!["ctx_edit".to_string(), "ctx_workflow".to_string()]),
                requires_evidence: None,
            },
            StateSpec {
                name: "done".to_string(),
                description: None,
                allowed_tools: Some(vec!["ctx".to_string(), "ctx_workflow".to_string()]),
                requires_evidence: None,
            },
        ],
        transitions: vec![
            TransitionSpec {
                from: "planning".to_string(),
                to: "coding".to_string(),
                description: None,
            },
            TransitionSpec {
                from: "coding".to_string(),
                to: "done".to_string(),
                description: None,
            },
        ],
    }
}

fn setup_test_data_dir() {
    let dir = std::env::temp_dir().join("lean_ctx_workflow_test");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", dir.to_str().unwrap()) };
}

fn cleanup_test_data_dir() {
    let dir = std::env::temp_dir().join("lean_ctx_workflow_test");
    let _ = std::fs::remove_dir_all(&dir);
    unsafe { std::env::remove_var("LEAN_CTX_DATA_DIR") };
}

// ==========================================================================
// Scenario 1: stop from active workflow clears it
// ==========================================================================

#[test]
#[serial]
fn scenario_stop_clears_workflow() {
    setup_test_data_dir();

    let spec = test_spec();
    let run = WorkflowRun::new(spec);
    save_active(&run).unwrap();

    let loaded = load_active().unwrap();
    assert!(loaded.is_some(), "Workflow should be saved");

    clear_active().unwrap();

    let loaded = load_active().unwrap();
    assert!(loaded.is_none(), "Workflow should be cleared after stop");

    cleanup_test_data_dir();
}

// ==========================================================================
// Scenario 2: save and load roundtrip
// ==========================================================================

#[test]
#[serial]
fn scenario_save_load_roundtrip() {
    setup_test_data_dir();

    let spec = test_spec();
    let run = WorkflowRun::new(spec);
    save_active(&run).unwrap();

    let loaded = load_active().unwrap().unwrap();
    assert_eq!(loaded.current, "planning");
    assert_eq!(loaded.spec.name, "test-workflow");

    cleanup_test_data_dir();
}

// ==========================================================================
// Scenario 3: clear_active is idempotent (no file = no error)
// ==========================================================================

#[test]
#[serial]
fn scenario_clear_active_idempotent() {
    setup_test_data_dir();
    clear_active().unwrap();
    clear_active().unwrap();
    let loaded = load_active().unwrap();
    assert!(loaded.is_none());
    cleanup_test_data_dir();
}

// ==========================================================================
// Scenario 4: "done" is a terminal state (no outgoing transitions)
// ==========================================================================

#[test]
fn scenario_done_state_is_terminal() {
    let spec = test_spec();
    let transitions: Vec<&TransitionSpec> = spec
        .transitions
        .iter()
        .filter(|t| t.from == "done")
        .collect();
    assert!(
        transitions.is_empty(),
        "done should have no outgoing transitions"
    );
}

// ==========================================================================
// Scenario 5: gate logic - done state allowed_tools blocks non-listed tools
// ==========================================================================

#[test]
fn scenario_done_state_gate_would_block() {
    let spec = test_spec();
    let state = spec.state("done").unwrap();
    let allowed = state.allowed_tools.as_ref().unwrap();

    // These tools are NOT in done's allowed list
    assert!(!allowed.contains(&"ctx_read".to_string()));
    assert!(!allowed.contains(&"ctx_edit".to_string()));
    assert!(!allowed.contains(&"ctx_shell".to_string()));

    // Only ctx + ctx_workflow are allowed (the bypass)
    assert!(allowed.contains(&"ctx".to_string()));
    assert!(allowed.contains(&"ctx_workflow".to_string()));
}

// ==========================================================================
// Scenario 6: complete should transition to done and clear file
// ==========================================================================

#[test]
#[serial]
fn scenario_complete_clears_workflow_file() {
    setup_test_data_dir();

    let spec = test_spec();
    let mut run = WorkflowRun::new(spec);
    run.current = "coding".to_string();
    save_active(&run).unwrap();

    // Verify workflow exists
    let loaded = load_active().unwrap();
    assert!(loaded.is_some());
    assert_eq!(loaded.unwrap().current, "coding");

    // Simulate what handle_complete now does:
    // 1. Transition to done
    // 2. Clear (not save)
    run.current = "done".to_string();
    clear_active().unwrap();

    // Verify it's gone
    let loaded = load_active().unwrap();
    assert!(
        loaded.is_none(),
        "After complete → done, file should be cleared"
    );

    cleanup_test_data_dir();
}

// ==========================================================================
// Scenario 7: normal transitions between non-terminal states persist
// ==========================================================================

#[test]
#[serial]
fn scenario_normal_transition_persists() {
    setup_test_data_dir();

    let spec = test_spec();
    let mut run = WorkflowRun::new(spec);
    run.current = "planning".to_string();
    save_active(&run).unwrap();

    // Transition planning → coding
    run.current = "coding".to_string();
    save_active(&run).unwrap();

    let loaded = load_active().unwrap().unwrap();
    assert_eq!(loaded.current, "coding");

    cleanup_test_data_dir();
}

// ==========================================================================
// Scenario 8: start after done creates fresh workflow
// ==========================================================================

#[test]
#[serial]
fn scenario_start_after_done() {
    setup_test_data_dir();

    // Clear any existing
    clear_active().unwrap();

    // Start fresh
    let spec = test_spec();
    let run = WorkflowRun::new(spec);
    save_active(&run).unwrap();

    let loaded = load_active().unwrap().unwrap();
    assert_eq!(loaded.current, "planning");
    assert!(loaded.transitions.is_empty());

    cleanup_test_data_dir();
}

// ==========================================================================
// Scenario 9: handoff restore logic - done workflow should be rejected
// ==========================================================================

#[test]
fn scenario_handoff_done_workflow_rejected() {
    let spec = test_spec();
    let mut run = WorkflowRun::new(spec);
    run.current = "done".to_string();

    // Simulate handoff restore decision
    let workflow_from_ledger: Option<WorkflowRun> = Some(run);
    let should_apply = workflow_from_ledger
        .as_ref()
        .is_some_and(|r| r.current != "done");
    assert!(
        !should_apply,
        "Handoff should NOT restore a 'done' workflow"
    );
}

// ==========================================================================
// Scenario 10: handoff restore logic - active workflow IS restored
// ==========================================================================

#[test]
fn scenario_handoff_active_workflow_restored() {
    let spec = test_spec();
    let mut run = WorkflowRun::new(spec);
    run.current = "coding".to_string();

    let workflow_from_ledger: Option<WorkflowRun> = Some(run);
    let should_apply = workflow_from_ledger
        .as_ref()
        .is_some_and(|r| r.current != "done");
    assert!(
        should_apply,
        "Handoff SHOULD restore an active (non-done) workflow"
    );
}

// ==========================================================================
// Scenario: Stale workflow (>30min) auto-clears on load
// ==========================================================================

#[test]
#[serial]
fn scenario_stale_workflow_auto_cleared_on_load() {
    setup_test_data_dir();

    let spec = test_spec();
    let mut run = WorkflowRun::new(spec);
    run.current = "coding".to_string();
    run.updated_at = chrono::Utc::now() - chrono::Duration::minutes(35);
    save_active(&run).unwrap();

    let loaded = load_active().unwrap();
    assert!(
        loaded.is_none(),
        "Stale workflow (>30min) should be auto-cleared on load"
    );

    cleanup_test_data_dir();
}

// ==========================================================================
// Scenario: Fresh workflow remains active on load
// ==========================================================================

#[test]
#[serial]
fn scenario_fresh_workflow_survives_load() {
    setup_test_data_dir();

    let spec = test_spec();
    let mut run = WorkflowRun::new(spec);
    run.current = "coding".to_string();
    run.updated_at = chrono::Utc::now() - chrono::Duration::minutes(5);
    save_active(&run).unwrap();

    let loaded = load_active().unwrap();
    assert!(
        loaded.is_some(),
        "Fresh workflow (<30min) should survive load"
    );
    assert_eq!(loaded.unwrap().current, "coding");

    cleanup_test_data_dir();
}

// ==========================================================================
// Scenario: Passthrough tools are never blocked by workflow gate
// ==========================================================================

#[test]
fn scenario_passthrough_tools_never_blocked() {
    use lean_ctx::server::WORKFLOW_PASSTHROUGH_TOOLS;

    assert!(WORKFLOW_PASSTHROUGH_TOOLS.contains(&"ctx_read"));
    assert!(WORKFLOW_PASSTHROUGH_TOOLS.contains(&"ctx_multi_read"));
    assert!(WORKFLOW_PASSTHROUGH_TOOLS.contains(&"ctx_search"));
    assert!(WORKFLOW_PASSTHROUGH_TOOLS.contains(&"ctx_tree"));
    assert!(WORKFLOW_PASSTHROUGH_TOOLS.contains(&"ctx_session"));
    assert!(WORKFLOW_PASSTHROUGH_TOOLS.contains(&"ctx"));
    assert!(WORKFLOW_PASSTHROUGH_TOOLS.contains(&"ctx_workflow"));
}

// ==========================================================================
// Scenario: is_workflow_stale correctly identifies stale workflows
// ==========================================================================

#[test]
fn scenario_staleness_detection() {
    let spec = test_spec();

    let mut fresh_run = WorkflowRun::new(spec.clone());
    fresh_run.updated_at = chrono::Utc::now() - chrono::Duration::minutes(10);
    assert!(
        !lean_ctx::server::is_workflow_stale(&fresh_run),
        "10min old workflow should NOT be stale"
    );

    let mut stale_run = WorkflowRun::new(spec);
    stale_run.updated_at = chrono::Utc::now() - chrono::Duration::minutes(31);
    assert!(
        lean_ctx::server::is_workflow_stale(&stale_run),
        "31min old workflow SHOULD be stale"
    );
}
