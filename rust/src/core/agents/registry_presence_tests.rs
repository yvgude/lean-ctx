// SPDX-License-Identifier: Apache-2.0
use chrono::Utc;

use super::{AgentRegistry, AgentStatus};

#[test]
fn persistent_presence_roundtrips_lifecycle_for_owning_process() {
    let isolated = crate::core::data_dir::isolated_data_dir();
    let mut registry = AgentRegistry::new();
    let first = registry
        .register_process(
            "mcp",
            Some("context-engine"),
            "/project",
            None,
            std::process::id(),
        )
        .expect("current process has an identity");
    registry.save().expect("save registry");

    assert_eq!(AgentRegistry::load().expect("registry").agents.len(), 1);

    AgentRegistry::heartbeat_persistent(&first).expect("heartbeat");
    AgentRegistry::finish_persistent(&first).expect("finish");
    let loaded = AgentRegistry::load().expect("registry");
    assert_eq!(
        loaded
            .agents
            .iter()
            .find(|agent| agent.agent_id == first)
            .expect("registered agent")
            .status,
        AgentStatus::Finished
    );
    assert!(isolated.path().join("agents/registry.json").exists());
}

#[test]
fn compatibility_index_survives_an_old_registry_writer() {
    const CHILD_FLAG: &str = "LEAN_CTX_REGISTRY_COMPAT_TEST_CHILD";
    if std::env::var_os(CHILD_FLAG).is_none() {
        // The sidecar follows a process-wide data-dir override. Other
        // parallel registry tests can prune it despite this test holding
        // the environment lock, so exercise the unchanged fixture alone.
        let mut command =
            std::process::Command::new(std::env::current_exe().expect("test executable"));
        command
            .args([
                "--exact",
                "core::agents::registry::presence_tests::compatibility_index_survives_an_old_registry_writer",
                "--nocapture",
            ])
            .env(CHILD_FLAG, "1");
        let output =
            crate::ipc::process::run_with_timeout(command, std::time::Duration::from_secs(30))
                .expect("isolated compatibility test must finish within 30 seconds");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed; 0 failed"),
            "isolated compatibility test failed: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let _isolated = crate::core::data_dir::isolated_data_dir();
    let mut registry = AgentRegistry::new();
    registry
        .register_process(
            "mcp",
            Some("context-engine"),
            "/project",
            None,
            std::process::id(),
        )
        .expect("current process has an identity");
    registry.cleanup_stale(super::presence_ttl());

    // An MCP process from the prior release serializes the old schema and
    // therefore drops this new registry field. The sidecar retains the
    // binding, so current readers still recognize the process as live.
    registry.agents[0].process_identity = None;
    assert_eq!(registry.list_active(Some("/project")).len(), 1);
}

#[test]
fn unknown_persistent_heartbeat_fails_closed() {
    let _isolated = crate::core::data_dir::isolated_data_dir();

    let error = AgentRegistry::heartbeat_persistent("missing-agent")
        .expect_err("unknown presence must not report a successful heartbeat");

    assert!(error.contains("was not found"));
}

#[test]
fn corrupt_registry_fails_closed_without_overwrite() {
    let isolated = crate::core::data_dir::isolated_data_dir();
    let agents_dir = isolated.path().join("agents");
    std::fs::create_dir_all(&agents_dir).expect("agents directory");
    let registry_path = agents_dir.join("registry.json");
    let corrupt = "{not valid json";
    std::fs::write(&registry_path, corrupt).expect("corrupt fixture");

    let error = AgentRegistry::mutate_locked(|registry| {
        let _ = registry.register_process("mcp", Some("context-engine"), "/project", None, 101);
    })
    .expect_err("corrupt registry must reject mutation");

    assert!(error.contains("agent registry is corrupt"));
    assert_eq!(std::fs::read_to_string(registry_path).unwrap(), corrupt);
}

/// #1619: a sandbox or filesystem policy denies exactly one path, and the
/// error was reported as a bare `File exists (os error 17)` — no path, no
/// operation, nothing to allow-list. The reporter had to guess
/// `~/.local/share/lean-ctx/agents` to unblock themselves.
#[test]
fn agents_dir_creation_failure_names_the_path_and_operation() {
    let isolated = crate::core::data_dir::isolated_data_dir();
    // A plain file where the directory belongs reproduces the reported
    // EEXIST without needing a sandbox.
    let blocker = isolated.path().join("agents");
    std::fs::write(&blocker, b"not a directory").expect("blocking file");

    let error = AgentRegistry::mutate_locked(|registry| {
        let _ = registry.register_process("mcp", Some("context-engine"), "/project", None, 101);
    })
    .expect_err("a file in place of the agents directory must fail");

    assert!(
        error.contains("create agent registry directory"),
        "the failing operation must be named; got: {error}"
    );
    assert!(
        error.contains(&blocker.display().to_string()),
        "the conflicting path must be in the message so it can be allow-listed; got: {error}"
    );
}

#[test]
fn reregistering_process_refreshes_metadata_without_duplication() {
    let mut registry = AgentRegistry::new();
    let pid = std::process::id();
    let first = registry
        .register_process("unknown", None, "/old", None, pid)
        .expect("current process has an identity");
    let second = registry
        .register_process("mcp", Some("context-engine"), "/new", None, pid)
        .expect("same process can re-register");

    assert_eq!(first, second);
    assert_eq!(registry.agents.len(), 1);
    assert_eq!(registry.agents[0].agent_type, "mcp");
    assert_eq!(registry.agents[0].project_root, "/new");
    assert_eq!(registry.agents[0].role.as_deref(), Some("context-engine"));
}

#[test]
fn logical_sessions_are_keyed_independently_of_transport_processes() {
    let mut registry = AgentRegistry::new();
    registry
        .register_process(
            "mcp",
            Some("context-engine"),
            "/project",
            None,
            std::process::id(),
        )
        .expect("current process has an identity");
    registry.open_or_heartbeat_logical_session("vscode", "/project", "chat-a");
    registry.open_or_heartbeat_logical_session("vscode", "/project", "chat-b");
    let opened_at = registry.logical_sessions[0].opened_at;

    registry.open_or_heartbeat_logical_session("vscode", "/project", "chat-a");

    assert_eq!(registry.agents.len(), 1);
    assert_eq!(registry.logical_sessions.len(), 2);
    assert_eq!(registry.logical_sessions[0].opened_at, opened_at);
    assert!(registry.logical_session_telemetry_seen);
    assert!(registry.close_logical_session("vscode", "/project", "chat-b"));
    assert_eq!(registry.logical_sessions.len(), 1);
}

#[test]
fn persistent_logical_session_presence_validates_and_roundtrips() {
    let _isolated = crate::core::data_dir::isolated_data_dir();

    AgentRegistry::record_logical_session_presence(
        "open",
        "vscode",
        "/project",
        "editor-session-a",
    )
    .expect("open presence");

    let registry = AgentRegistry::load().expect("persisted registry");
    assert_eq!(registry.logical_sessions.len(), 1);
    assert_eq!(registry.logical_sessions[0].session_id, "editor-session-a");
    assert!(registry.logical_session_telemetry_seen);

    assert!(
        AgentRegistry::record_logical_session_presence(
            "invalid",
            "vscode",
            "/project",
            "editor-session-a",
        )
        .is_err()
    );
    assert!(
        AgentRegistry::record_logical_session_presence(
            "heartbeat",
            "",
            "/project",
            "editor-session-a",
        )
        .is_err()
    );

    AgentRegistry::record_logical_session_presence(
        "close",
        "vscode",
        "/project",
        "editor-session-a",
    )
    .expect("close presence");
    assert!(
        AgentRegistry::load()
            .expect("persisted registry")
            .logical_sessions
            .is_empty()
    );
}

#[test]
fn logical_session_expiry_is_bounded_by_heartbeat_not_tool_activity() {
    let mut registry = AgentRegistry::new();
    registry.open_or_heartbeat_logical_session("vscode", "/project", "chat-a");
    registry.logical_sessions[0].last_heartbeat = Utc::now() - chrono::Duration::seconds(181);

    registry.cleanup_stale_logical_sessions(180);

    assert!(registry.logical_sessions.is_empty());
    assert!(registry.logical_session_telemetry_seen);
}

#[test]
fn legacy_registry_deserializes_without_claiming_session_telemetry() {
    let registry: AgentRegistry = serde_json::from_str(
        r#"{"agents":[],"scratchpad":[],"updated_at":"2026-01-01T00:00:00Z"}"#,
    )
    .expect("legacy registry");

    assert!(registry.logical_sessions.is_empty());
    assert!(!registry.logical_session_telemetry_seen);
}
