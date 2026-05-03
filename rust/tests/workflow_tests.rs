use lean_ctx::core::workflow::*;

fn minimal_spec() -> WorkflowSpec {
    WorkflowSpec {
        name: "test".to_string(),
        description: None,
        initial: "a".to_string(),
        states: vec![
            StateSpec {
                name: "a".to_string(),
                description: None,
                allowed_tools: Some(vec!["ctx_read".to_string()]),
                requires_evidence: None,
            },
            StateSpec {
                name: "b".to_string(),
                description: None,
                allowed_tools: None,
                requires_evidence: Some(vec!["tool:ctx_read".to_string()]),
            },
            StateSpec {
                name: "done".to_string(),
                description: None,
                allowed_tools: None,
                requires_evidence: None,
            },
        ],
        transitions: vec![
            TransitionSpec {
                from: "a".to_string(),
                to: "b".to_string(),
                description: None,
            },
            TransitionSpec {
                from: "b".to_string(),
                to: "done".to_string(),
                description: None,
            },
        ],
    }
}

struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(ref previous) = self.previous {
            std::env::set_var(self.key, previous);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

#[test]
fn validate_spec_accepts_valid_spec() {
    let spec = minimal_spec();
    assert!(validate_spec(&spec).is_ok());
}

#[test]
fn validate_spec_rejects_empty_states() {
    let mut spec = minimal_spec();
    spec.states.clear();
    let err = validate_spec(&spec).unwrap_err();
    assert!(err.contains("must not be empty"));
}

#[test]
fn validate_spec_rejects_empty_name() {
    let mut spec = minimal_spec();
    spec.name = "  ".to_string();
    let err = validate_spec(&spec).unwrap_err();
    assert!(err.contains("name must not be empty"));
}

#[test]
fn validate_spec_rejects_duplicate_state_names() {
    let mut spec = minimal_spec();
    spec.states.push(StateSpec {
        name: "a".to_string(),
        description: None,
        allowed_tools: None,
        requires_evidence: None,
    });
    let err = validate_spec(&spec).unwrap_err();
    assert!(err.contains("Duplicate state name"));
}

#[test]
fn validate_spec_rejects_initial_not_in_states() {
    let mut spec = minimal_spec();
    spec.initial = "nonexistent".to_string();
    let err = validate_spec(&spec).unwrap_err();
    assert!(err.contains("is not in states"));
}

#[test]
fn validate_spec_rejects_transition_with_unknown_from() {
    let mut spec = minimal_spec();
    spec.transitions.push(TransitionSpec {
        from: "ghost".to_string(),
        to: "a".to_string(),
        description: None,
    });
    let err = validate_spec(&spec).unwrap_err();
    assert!(err.contains("is not a state"));
}

#[test]
fn validate_spec_rejects_empty_allowed_tools_list() {
    let mut spec = minimal_spec();
    spec.states[0].allowed_tools = Some(vec![]);
    let err = validate_spec(&spec).unwrap_err();
    assert!(err.contains("must not be empty when present"));
}

#[test]
fn allowed_transitions_returns_correct_transitions() {
    let spec = minimal_spec();
    let from_a = allowed_transitions(&spec, "a");
    assert_eq!(from_a.len(), 1);
    assert_eq!(from_a[0].to, "b");

    let from_done = allowed_transitions(&spec, "done");
    assert!(from_done.is_empty());
}

#[test]
fn find_transition_works() {
    let spec = minimal_spec();
    assert!(find_transition(&spec, "a", "b").is_some());
    assert!(find_transition(&spec, "a", "done").is_none());
    assert!(find_transition(&spec, "b", "a").is_none());
}

#[test]
fn missing_evidence_returns_empty_when_no_requirements() {
    let spec = minimal_spec();
    let missing = missing_evidence_for_state(&spec, "a", |_| false);
    assert!(missing.is_empty());
}

#[test]
fn missing_evidence_returns_keys_when_unsatisfied() {
    let spec = minimal_spec();
    let missing = missing_evidence_for_state(&spec, "b", |_| false);
    assert_eq!(missing, vec!["tool:ctx_read"]);
}

#[test]
fn missing_evidence_returns_empty_when_satisfied() {
    let spec = minimal_spec();
    let missing = missing_evidence_for_state(&spec, "b", |k| k == "tool:ctx_read");
    assert!(missing.is_empty());
}

#[test]
fn missing_evidence_unknown_state() {
    let spec = minimal_spec();
    let missing = missing_evidence_for_state(&spec, "nonexistent", |_| true);
    assert_eq!(missing.len(), 1);
    assert!(missing[0].starts_with("unknown_state:"));
}

#[test]
fn can_transition_succeeds_with_evidence() {
    let spec = minimal_spec();
    let result = can_transition(&spec, "a", "b", |k| k == "tool:ctx_read");
    assert!(result.is_ok());
}

#[test]
fn can_transition_blocked_without_evidence() {
    let spec = minimal_spec();
    let result = can_transition(&spec, "a", "b", |_| false);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("Missing evidence"));
}

#[test]
fn can_transition_blocked_for_invalid_transition() {
    let spec = minimal_spec();
    let result = can_transition(&spec, "a", "done", |_| true);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("No transition"));
}

#[test]
fn workflow_run_new_sets_initial_state() {
    let spec = minimal_spec();
    let run = WorkflowRun::new(spec);
    assert_eq!(run.current, "a");
    assert!(run.transitions.is_empty());
    assert!(run.evidence.is_empty());
}

#[test]
fn workflow_run_add_manual_evidence() {
    let spec = minimal_spec();
    let mut run = WorkflowRun::new(spec);
    run.add_manual_evidence("tool:ctx_read", Some("file.rs"));
    assert_eq!(run.evidence.len(), 1);
    assert_eq!(run.evidence[0].key, "tool:ctx_read");
    assert_eq!(run.evidence[0].value.as_deref(), Some("file.rs"));
}

#[test]
fn builtin_plan_code_test_validates() {
    let spec = WorkflowSpec::builtin_plan_code_test();
    assert!(validate_spec(&spec).is_ok());
    assert_eq!(spec.initial, "planning");
    assert_eq!(spec.states.len(), 4);
    assert_eq!(spec.transitions.len(), 3);
}

#[test]
fn builtin_spec_roundtrip_serde() {
    let spec = WorkflowSpec::builtin_plan_code_test();
    let json = serde_json::to_string(&spec).expect("serialize");
    let back: WorkflowSpec = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.name, spec.name);
    assert_eq!(back.states.len(), spec.states.len());
}

#[test]
fn store_roundtrip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("active.json");

    let spec = minimal_spec();
    let mut run = WorkflowRun::new(spec);
    run.add_manual_evidence("test-key", Some("test-value"));

    let json = serde_json::to_string_pretty(&run).expect("serialize");
    std::fs::write(&path, &json).expect("write");

    let content = std::fs::read_to_string(&path).expect("read");
    let loaded: WorkflowRun = serde_json::from_str(&content).expect("deserialize");

    assert_eq!(loaded.current, "a");
    assert_eq!(loaded.evidence.len(), 1);
    assert_eq!(loaded.evidence[0].key, "test-key");
}

#[test]
fn store_load_active_returns_none_when_file_is_missing() {
    let _env_lock = lean_ctx::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().join("leanctx-data");
    let _env = EnvVarGuard::set(
        "LEAN_CTX_DATA_DIR",
        data_dir.to_str().expect("data dir utf8"),
    );

    clear_active().expect("clear missing active should succeed");
    let loaded = load_active().expect("load_active");
    assert!(loaded.is_none());
}

#[test]
fn store_save_load_clear_active_roundtrip() {
    let _env_lock = lean_ctx::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().join("leanctx-data");
    let _env = EnvVarGuard::set(
        "LEAN_CTX_DATA_DIR",
        data_dir.to_str().expect("data dir utf8"),
    );

    let mut run = WorkflowRun::new(minimal_spec());
    run.add_manual_evidence("tool:ctx_read", Some("src/lib.rs"));

    save_active(&run).expect("save_active");
    let loaded = load_active()
        .expect("load_active")
        .expect("active workflow");
    assert_eq!(loaded.current, "a");
    assert_eq!(loaded.evidence.len(), 1);
    assert_eq!(loaded.evidence[0].key, "tool:ctx_read");

    clear_active().expect("clear_active");
    let after_clear = load_active().expect("load after clear");
    assert!(after_clear.is_none());
}
