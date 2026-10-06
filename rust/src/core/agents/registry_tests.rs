// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;

use chrono::Utc;

use super::{
    AgentDiary, AgentEntry, AgentRegistry, AgentStatus, DiaryEntryType, ScratchpadEntry, truncate,
};
use crate::core::a2a::message::{MessagePriority, PrivacyLevel};

#[test]
fn register_and_list() {
    let mut reg = AgentRegistry::new();
    let id = reg
        .register("cursor", Some("dev"), "/tmp/project", None)
        .expect("current process can be registered");
    assert!(!id.is_empty());
    assert_eq!(reg.list_active(None).len(), 1);
    assert_eq!(reg.list_active(None)[0].agent_type, "cursor");
}

#[test]
fn reregister_same_pid() {
    let mut reg = AgentRegistry::new();
    let id1 = reg
        .register("cursor", Some("dev"), "/tmp/project", None)
        .expect("current process can be registered");
    let id2 = reg
        .register("cursor", Some("review"), "/tmp/project", None)
        .expect("same process can be re-registered");
    assert_eq!(id1, id2);
    assert_eq!(reg.agents.len(), 1);
    assert_eq!(reg.agents[0].role, Some("review".to_string()));
}

/// #1766: the presence retry registers as `context-engine` for the same PID;
/// an explicit role must survive it, while an explicit role still replaces
/// the placeholder (the `initialize` upgrade).
#[test]
fn placeholder_retry_never_overwrites_an_explicit_role() {
    let mut reg = AgentRegistry::new();
    let id = reg
        .register("mcp", Some("reviewer"), "/project", None)
        .expect("reviewer");
    let again = reg
        .register("mcp", Some("context-engine"), "/project", None)
        .expect("retry for the same process");
    assert_eq!(id, again);
    assert_eq!(reg.agents[0].role.as_deref(), Some("reviewer"));

    let mut fresh = AgentRegistry::new();
    fresh
        .register("mcp", Some("context-engine"), "/project", None)
        .expect("placeholder");
    fresh
        .register("mcp", Some("debugger"), "/project", None)
        .expect("upgrade");
    assert_eq!(fresh.agents[0].role.as_deref(), Some("debugger"));
}

#[test]
fn explicit_binding_requires_active_durable_identity() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let mut reg = AgentRegistry::new();
    assert!(
        reg.register(
            "mcp",
            Some("context-engine"),
            "/tmp/project",
            Some("missing")
        )
        .is_err()
    );

    crate::core::agent_registry::register("bound", "coder", "owner").expect("durable identity");
    crate::core::agent_registry::suspend("bound", "review").expect("suspend identity");
    assert!(
        reg.register("mcp", Some("context-engine"), "/tmp/project", Some("bound"))
            .is_err()
    );

    crate::core::agent_registry::register("closed", "coder", "owner").expect("durable identity");
    crate::core::agent_registry::decommission("closed").expect("decommission identity");
    assert!(
        reg.register(
            "mcp",
            Some("context-engine"),
            "/tmp/project",
            Some("closed")
        )
        .is_err()
    );
}

#[test]
fn same_pid_preserves_binding_and_rejects_rebind() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    crate::core::agent_registry::register("identity-a", "coder", "owner").expect("identity a");
    crate::core::agent_registry::register("identity-b", "coder", "owner").expect("identity b");

    let mut reg = AgentRegistry::new();
    let id = reg
        .register(
            "mcp",
            Some("context-engine"),
            "/tmp/project",
            Some("identity-a"),
        )
        .expect("bound registration");
    assert_eq!(
        reg.register("mcp", Some("context-engine"), "/tmp/project", None)
            .expect("same process re-registration"),
        id
    );
    assert_eq!(
        reg.agents[0].durable_identity_id.as_deref(),
        Some("identity-a")
    );
    let error = reg
        .register(
            "mcp",
            Some("context-engine"),
            "/tmp/project",
            Some("identity-b"),
        )
        .expect_err("live presence cannot silently rebind");
    assert!(error.contains("refusing rebind"));
}

#[test]
fn revoked_binding_gates_active_operations_but_allows_finished_cleanup() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    crate::core::agent_registry::register("bound", "coder", "owner").expect("durable identity");
    let mut reg = AgentRegistry::new();
    let presence = reg
        .register("mcp", Some("context-engine"), "/tmp/project", Some("bound"))
        .expect("bound registration");
    crate::core::agent_registry::suspend("bound", "review").expect("suspend identity");

    assert!(reg.update_heartbeat(&presence).is_err());
    assert!(
        reg.set_status(&presence, AgentStatus::Idle, Some("waiting"))
            .is_err()
    );
    assert!(
        reg.post_message(&presence, None, "status", "blocked")
            .is_err()
    );
    assert!(reg.read_unread(&presence).is_err());
    assert!(reg.list_active(None).is_empty());

    reg.set_status(&presence, AgentStatus::Finished, Some("revoked"))
        .expect("cleanup must remain available after revocation");
    assert_eq!(reg.agents[0].status, AgentStatus::Finished);
}

#[test]
fn legacy_presence_entry_roundtrips_without_a_binding() {
    let json = r#"{
            "agent_id":"mcp-1",
            "agent_type":"mcp",
            "role":null,
            "project_root":"/project",
            "started_at":"2026-01-01T00:00:00Z",
            "last_active":"2026-01-01T00:00:00Z",
            "pid":1,
            "status":"Active",
            "status_message":null
        }"#;
    let entry: AgentEntry = serde_json::from_str(json).expect("legacy presence entry");
    assert!(entry.durable_identity_id.is_none());
    let encoded = serde_json::to_string(&entry).expect("presence entry");
    assert!(!encoded.contains("durable_identity_id"));
}

#[test]
fn post_and_read_messages() {
    let mut reg = AgentRegistry::new();
    reg.post_message("agent-a", None, "finding", "Found a bug in auth.rs")
        .expect("message");
    reg.post_message("agent-b", Some("agent-a"), "request", "Please review")
        .expect("message");

    let msgs = reg.read_unread("agent-a").expect("messages");
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].category, "request");
}

#[test]
fn read_unread_scoped_consumes_each_message_once() {
    let mut reg = AgentRegistry::new();
    reg.post_message_scoped(
        Some("/project-a"),
        "agent-a",
        Some("agent-b"),
        "request",
        "targeted handoff",
        PrivacyLevel::Private,
        MessagePriority::Normal,
        Some(1),
    )
    .expect("scoped message");
    reg.post_message_scoped(
        Some("/project-a"),
        "agent-a",
        None,
        "broadcast",
        "broadcast handoff",
        PrivacyLevel::Private,
        MessagePriority::Normal,
        Some(1),
    )
    .expect("scoped message");
    reg.post_message_scoped(
        Some("/project-a"),
        "agent-a",
        Some("agent-c"),
        "request",
        "other recipient handoff",
        PrivacyLevel::Private,
        MessagePriority::Normal,
        Some(1),
    )
    .expect("scoped message");
    reg.post_message_scoped(
        Some("/project-b"),
        "agent-a",
        Some("agent-b"),
        "request",
        "other project handoff",
        PrivacyLevel::Private,
        MessagePriority::Normal,
        Some(1),
    )
    .expect("scoped message");
    reg.scratchpad.push(ScratchpadEntry {
        id: "expired-scoped".to_string(),
        from_agent: "agent-a".to_string(),
        to_agent: Some("agent-b".to_string()),
        task_id: None,
        category: "request".to_string(),
        priority: MessagePriority::Normal,
        privacy: PrivacyLevel::Private,
        message: "expired handoff".to_string(),
        metadata: HashMap::new(),
        project_root: Some("/project-a".to_string()),
        timestamp: Utc::now(),
        read_by: vec![],
        expires_at: Some(Utc::now() - chrono::Duration::hours(1)),
    });

    let first: Vec<String> = reg
        .read_unread_scoped("agent-b", "/project-a")
        .expect("scoped messages")
        .into_iter()
        .map(|entry| entry.message.clone())
        .collect();
    assert_eq!(
        first,
        vec![
            "targeted handoff".to_string(),
            "broadcast handoff".to_string()
        ]
    );
    assert!(
        reg.read_unread_scoped("agent-b", "/project-a")
            .expect("scoped messages")
            .is_empty()
    );

    reg.post_message_scoped(
        Some("/project-a"),
        "agent-a",
        Some("agent-b"),
        "request",
        "new handoff",
        PrivacyLevel::Private,
        MessagePriority::Normal,
        Some(1),
    )
    .expect("scoped message");
    let new_messages: Vec<String> = reg
        .read_unread_scoped("agent-b", "/project-a")
        .expect("scoped messages")
        .into_iter()
        .map(|entry| entry.message.clone())
        .collect();
    assert_eq!(new_messages, vec!["new handoff"]);
    assert!(
        reg.read_unread_scoped("agent-b", "/project-a")
            .expect("scoped messages")
            .is_empty()
    );

    let other_recipient: Vec<String> = reg
        .read_unread_scoped("agent-c", "/project-a")
        .expect("scoped messages")
        .into_iter()
        .map(|entry| entry.message.clone())
        .collect();
    assert_eq!(
        other_recipient,
        vec![
            "broadcast handoff".to_string(),
            "other recipient handoff".to_string()
        ]
    );

    let other_project: Vec<String> = reg
        .read_unread_scoped("agent-b", "/project-b")
        .expect("scoped messages")
        .into_iter()
        .map(|entry| entry.message.clone())
        .collect();
    assert_eq!(other_project, vec!["other project handoff".to_string()]);
}

#[test]
fn scoped_messages_are_invisible_to_other_projects() {
    let mut reg = AgentRegistry::new();
    reg.post_message_scoped(
        Some("/project-a"),
        "agent-a",
        Some("agent-b"),
        "finding",
        "private project-a finding",
        PrivacyLevel::Private,
        MessagePriority::Normal,
        Some(1),
    )
    .expect("message");

    assert!(
        reg.read_unread_scoped("agent-b", "/project-b")
            .expect("messages")
            .is_empty()
    );

    let messages = reg
        .read_unread_scoped("agent-b", "/project-a")
        .expect("messages");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].project_root.as_deref(), Some("/project-a"));
}

#[test]
fn expired_messages_are_skipped_in_read_unread() {
    let mut reg = AgentRegistry::new();
    reg.scratchpad.push(ScratchpadEntry {
        id: "expired-1".to_string(),
        from_agent: "agent-a".to_string(),
        to_agent: None,
        task_id: None,
        category: "test".to_string(),
        priority: MessagePriority::default(),
        privacy: PrivacyLevel::default(),
        message: "I am expired".to_string(),
        metadata: HashMap::new(),
        project_root: None,
        timestamp: Utc::now(),
        read_by: vec![],
        expires_at: Some(Utc::now() - chrono::Duration::hours(1)),
    });
    reg.post_message("agent-a", None, "test", "I am fresh")
        .expect("message");

    let msgs = reg.read_unread("agent-b").expect("messages");

    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].message, "I am fresh");
}

#[test]
fn cleanup_stale_removes_expired_scratchpad() {
    let _isolated = crate::core::data_dir::isolated_data_dir();
    let mut reg = AgentRegistry::new();
    reg.scratchpad.push(ScratchpadEntry {
        id: "exp-1".to_string(),
        from_agent: "a".to_string(),
        to_agent: None,
        task_id: None,
        category: "test".to_string(),
        priority: MessagePriority::default(),
        privacy: PrivacyLevel::default(),
        message: "expired".to_string(),
        metadata: HashMap::new(),
        project_root: None,
        timestamp: Utc::now(),
        read_by: vec![],
        expires_at: Some(Utc::now() - chrono::Duration::hours(1)),
    });

    reg.cleanup_stale(super::presence_ttl());

    assert!(reg.scratchpad.is_empty());
}

#[test]
fn post_message_gets_default_ttl() {
    let mut reg = AgentRegistry::new();

    reg.post_message("agent-a", None, "finding", "test")
        .expect("message");

    assert!(
        reg.scratchpad[0].expires_at.is_some(),
        "default TTL must be set"
    );
}

#[test]
fn set_status() {
    let mut reg = AgentRegistry::new();
    let id = reg
        .register("claude", None, "/tmp/project", None)
        .expect("current process can be registered");
    reg.set_status(&id, AgentStatus::Idle, Some("waiting for review"))
        .expect("registered agent exists");
    assert_eq!(reg.agents[0].status, AgentStatus::Idle);
    assert_eq!(
        reg.agents[0].status_message,
        Some("waiting for review".to_string())
    );
}

#[test]
fn unknown_status_update_fails_closed() {
    let mut reg = AgentRegistry::new();
    let error = reg
        .set_status("missing-agent", AgentStatus::Finished, None)
        .expect_err("unknown agent must be rejected");
    assert!(error.contains("missing-agent"));
    assert!(reg.agents.is_empty());
}

#[test]
fn broadcast_message() {
    let mut reg = AgentRegistry::new();
    reg.post_message("agent-a", None, "status", "Starting refactor")
        .expect("message");

    let msgs_b = reg.read_unread("agent-b").expect("messages");
    assert_eq!(msgs_b.len(), 1);
    assert_eq!(msgs_b[0].message, "Starting refactor");

    let msgs_a = reg.read_unread("agent-a").expect("messages");
    assert!(msgs_a.is_empty());
}

#[test]
fn diary_add_and_format() {
    let mut diary = AgentDiary::new("test-agent-001", "cursor", "/tmp/project");
    diary.add_entry(
        DiaryEntryType::Discovery,
        "Found auth module at src/auth.rs",
        Some("auth"),
    );
    diary.add_entry(
        DiaryEntryType::Decision,
        "Use JWT RS256 for token signing",
        None,
    );
    diary.add_entry(
        DiaryEntryType::Progress,
        "Implemented login endpoint",
        Some("auth"),
    );

    assert_eq!(diary.entries.len(), 3);

    let summary = diary.format_summary();
    assert!(summary.contains("test-agent-001"));
    assert!(summary.contains("FOUND"));
    assert!(summary.contains("DECIDED"));
    assert!(summary.contains("DONE"));
}

#[test]
fn diary_compact_format() {
    let mut diary = AgentDiary::new("test-agent-002", "claude", "/tmp/project");
    diary.add_entry(DiaryEntryType::Insight, "DB queries are N+1", None);
    diary.add_entry(
        DiaryEntryType::Blocker,
        "Missing API credentials",
        Some("deploy"),
    );

    let compact = diary.format_compact();
    assert!(compact.contains("diary:test-agent-002"));
    assert!(compact.contains("B:Missing API credentials"));
    assert!(compact.contains("I:DB queries are N+1"));
}

#[test]
fn diary_entry_types() {
    let types = vec![
        DiaryEntryType::Discovery,
        DiaryEntryType::Decision,
        DiaryEntryType::Blocker,
        DiaryEntryType::Progress,
        DiaryEntryType::Insight,
    ];
    for t in types {
        assert!(!format!("{t}").is_empty());
    }
}

#[test]
fn diary_truncation() {
    let mut diary = AgentDiary::new("test-agent", "cursor", "/tmp");
    for i in 0..150 {
        diary.add_entry(DiaryEntryType::Progress, &format!("Step {i}"), None);
    }
    assert!(diary.entries.len() <= 100);
}

#[test]
fn truncate_utf8_emoji_no_panic() {
    let result = truncate("Agent 🤖 Name ist lang genug", 15);
    assert!(result.ends_with("..."));
}

#[test]
fn truncate_utf8_cyrillic_no_panic() {
    let result = truncate("агент выполняет длинную задачу", 15);
    assert!(result.ends_with("..."));
}

#[test]
fn truncate_short_utf8_unchanged() {
    assert_eq!(truncate("短い", 20), "短い");
}

#[cfg(unix)]
#[test]
fn dense_live_presence_does_not_block_registration_or_duplicate_reregistration() {
    let _isolated = crate::core::data_dir::isolated_data_dir();
    // Reproduce dense legacy presence without launching dozens of helpers.
    // Each retained record refers to a real, still-live parent process.
    // SAFETY: getppid has no arguments and does not modify process state.
    let parent_pid = unsafe { libc::getppid() } as u32;
    let template = test_entry("legacy", "/project", parent_pid);
    assert!(template.process_identity.is_some());
    let mut registry = AgentRegistry::new();
    for index in 0..64 {
        let mut entry = template.clone();
        entry.agent_id = format!("legacy-{index}");
        registry.agents.push(entry);
    }
    let id = registry
        .register("codex", Some("implementer"), "/project", None)
        .expect("presence is not execution admission");
    assert_eq!(registry.agents.len(), 65);
    assert_eq!(
        registry
            .register("codex", Some("implementer"), "/project", None)
            .unwrap(),
        id
    );
    assert_eq!(registry.agents.len(), 65);
    registry.save().unwrap();
    assert_eq!(AgentRegistry::load().unwrap().agents.len(), 65);
}

/// #1765: a fifth `coder` session on a machine that already runs four live
/// `coder` sessions of the same project lost every tool — reads included —
/// until another lease lapsed. Presence is not admission: the session joins
/// with its real role, and no duplicate-role or mutating-slot check refuses it.
#[cfg(unix)]
#[test]
fn over_former_mutating_cap_session_keeps_its_real_role() {
    let _isolated = crate::core::data_dir::isolated_data_dir();
    // SAFETY: getppid has no arguments and does not modify process state.
    let parent_pid = unsafe { libc::getppid() } as u32;
    let mut registry = AgentRegistry::new();
    for index in 0..4 {
        let mut entry = test_entry(&format!("coder-{index}"), "/project", parent_pid);
        entry.agent_type = "mcp".to_string();
        entry.role = Some("coder".to_string());
        registry.agents.push(entry);
    }
    let id = registry
        .register("mcp", Some("coder"), "/project", None)
        .expect("a session over the former cap is still admitted");
    let own = registry
        .agents
        .iter()
        .find(|agent| agent.agent_id == id)
        .expect("own presence recorded");
    assert_eq!(own.role.as_deref(), Some("coder"));
    assert_eq!(own.status, AgentStatus::Active);
    assert_eq!(registry.list_active(None).len(), 5);
}

fn test_entry(agent_id: &str, project_root: &str, pid: u32) -> AgentEntry {
    let now = Utc::now();
    AgentEntry {
        agent_id: agent_id.to_string(),
        agent_type: "cursor".to_string(),
        role: Some("dev".to_string()),
        project_root: project_root.to_string(),
        started_at: now,
        last_active: now,
        pid,
        process_identity: crate::ipc::process::identity(pid),
        durable_identity_id: None,
        status: AgentStatus::Active,
        status_message: None,
    }
}

#[test]
fn cleanup_stale_caps_recent_finished_history() {
    let _isolated = crate::core::data_dir::isolated_data_dir();
    let mut reg = AgentRegistry::new();
    let now = Utc::now();
    reg.agents = (0..(super::MAX_RETAINED_FINISHED_AGENTS + 2))
        .map(|offset| {
            let mut agent = test_entry(
                &format!("finished-{offset}"),
                "/project",
                std::process::id(),
            );
            agent.status = AgentStatus::Finished;
            agent.last_active = now - chrono::Duration::seconds(offset as i64);
            agent
        })
        .collect();

    reg.cleanup_stale(1);

    assert_eq!(reg.agents.len(), super::MAX_RETAINED_FINISHED_AGENTS);
    assert!(
        reg.agents
            .iter()
            .any(|agent| agent.agent_id == "finished-0")
    );
    assert!(!reg.agents.iter().any(|agent| {
        agent.agent_id == format!("finished-{}", super::MAX_RETAINED_FINISHED_AGENTS + 1)
    }));
}

/// #419: the wake-up briefing scopes agents to the current project via
/// `list_active(Some(root))`. Peers working on *other* projects must never
/// leak into the briefing.
#[test]
fn list_active_scopes_to_project_root() {
    let mut reg = AgentRegistry::new();
    reg.agents
        .push(test_entry("a-1", "/proj/a", std::process::id()));
    reg.agents
        .push(test_entry("b-1", "/proj/b", std::process::id()));

    let active_a = reg.list_active(Some("/proj/a"));
    assert_eq!(active_a.len(), 1);
    assert_eq!(active_a[0].agent_id, "a-1");

    // Unscoped still sees both.
    assert_eq!(reg.list_active(None).len(), 2);
}

/// #419: a crashed/exited MCP process leaves an `Active` entry behind.
/// `cleanup_stale` must flip it to `Finished` (regardless of age) so
/// `list_active` no longer surfaces it as a live peer — the ghost the
/// briefing used to show. Previously `#[cfg(unix)]`-only, which is why
/// the non-unix `is_process_alive` hardcoded-`true` regression (see its
/// doc comment) shipped unnoticed: this exact test never ran on Windows.
#[test]
fn cleanup_stale_prunes_dead_pid_from_active_list() {
    let _isolated = crate::core::data_dir::isolated_data_dir();
    // Reap a child so its PID is guaranteed dead at assertion time.
    let reaped = {
        let mut cmd = if cfg!(windows) {
            let mut c = std::process::Command::new("cmd");
            c.args(["/C", "exit"]);
            c
        } else {
            std::process::Command::new("true")
        };
        let mut child = cmd.spawn().expect("spawn short-lived helper process");
        let pid = child.id();
        child.wait().expect("reap helper process");
        pid
    };

    let mut reg = AgentRegistry::new();
    reg.agents.push(test_entry("ghost", "/proj/a", reaped));
    reg.agents
        .push(test_entry("live", "/proj/a", std::process::id()));

    reg.cleanup_stale(super::presence_ttl());

    let ids: Vec<&str> = reg
        .list_active(Some("/proj/a"))
        .iter()
        .map(|a| a.agent_id.as_str())
        .collect();
    assert!(ids.contains(&"live"), "live same-project agent must remain");
    assert!(
        !ids.contains(&"ghost"),
        "dead-pid agent must be pruned from the active list (#419)"
    );
}

#[test]
fn cleanup_stale_expires_quiet_active_worker_even_when_process_lives() {
    let _isolated = crate::core::data_dir::isolated_data_dir();
    let mut quiet = test_entry("quiet", "/proj/a", std::process::id());
    quiet.last_active =
        Utc::now() - chrono::Duration::seconds(super::active_worker_lease_seconds() + 1);
    let mut registry = AgentRegistry::new();
    registry.agents.push(quiet);

    registry.cleanup_stale(super::presence_ttl());

    assert_eq!(registry.agents[0].status, AgentStatus::Finished);
    assert_eq!(
        registry.agents[0].status_message.as_deref(),
        Some("worker lease expired; register again before continuing")
    );
    assert!(registry.list_active(Some("/proj/a")).is_empty());
}

#[test]
fn cleanup_stale_rejects_a_reused_pid_with_the_wrong_identity() {
    let _isolated = crate::core::data_dir::isolated_data_dir();
    let pid = std::process::id();
    let mut stale = test_entry("reused-pid", "/proj/a", pid);
    let identity = stale
        .process_identity
        .as_mut()
        .expect("current process identity");
    identity.start_marker = identity.start_marker.saturating_add(1);
    let mut registry = AgentRegistry::new();
    registry.agents.push(stale);

    registry.cleanup_stale(super::presence_ttl());

    assert_eq!(registry.agents[0].status, AgentStatus::Finished);
    assert_eq!(
        registry.agents[0].status_message.as_deref(),
        Some("process identity no longer matches")
    );
}

#[test]
fn legacy_identity_recovery_requires_the_original_lean_ctx_process() {
    let started_at = Utc::now();
    let agent = AgentEntry {
        agent_id: "legacy".to_string(),
        agent_type: "mcp".to_string(),
        role: None,
        project_root: "/project".to_string(),
        started_at,
        last_active: started_at,
        pid: 1,
        process_identity: None,
        durable_identity_id: None,
        status: AgentStatus::Active,
        status_message: None,
    };
    let registered_after_boot = crate::ipc::process::ProcessIdentity {
        start_marker: u64::try_from(started_at.timestamp_micros() - 1_000_000)
            .expect("current timestamps fit u64"),
        executable: "/Users/test/.local/bin/lean-ctx".to_string(),
    };
    assert!(super::legacy_identity_matches_registration(
        &agent,
        &registered_after_boot
    ));

    let reused_pid = crate::ipc::process::ProcessIdentity {
        start_marker: u64::try_from(started_at.timestamp_micros() + 1).expect("fits u64"),
        executable: "/Users/test/.local/bin/lean-ctx".to_string(),
    };
    assert!(!super::legacy_identity_matches_registration(
        &agent,
        &reused_pid
    ));

    let unrelated_process = crate::ipc::process::ProcessIdentity {
        start_marker: registered_after_boot.start_marker,
        executable: "/Applications/Firefox.app/Contents/MacOS/firefox".to_string(),
    };
    assert!(!super::legacy_identity_matches_registration(
        &agent,
        &unrelated_process
    ));
}

#[test]
fn only_the_known_legacy_false_positive_is_recoverable() {
    let now = Utc::now();
    let mut agent = AgentEntry {
        agent_id: "legacy".to_string(),
        agent_type: "mcp".to_string(),
        role: None,
        project_root: "/project".to_string(),
        started_at: now,
        last_active: now,
        pid: 1,
        process_identity: None,
        durable_identity_id: None,
        status: AgentStatus::Finished,
        status_message: Some("process identity no longer matches".to_string()),
    };
    assert!(super::is_recoverable_legacy_finished(&agent));

    agent.status_message = Some("connection closed".to_string());
    assert!(!super::is_recoverable_legacy_finished(&agent));
    agent.status = AgentStatus::Active;
    assert!(!super::is_recoverable_legacy_finished(&agent));
}

/// Regression: concurrent load-mutate-save cycles must not silently drop
/// each other's changes. Before `mutate_locked`, `save()` only locked the
/// final write — the preceding `load()` was unlocked, so a second writer
/// could load a stale snapshot and overwrite the first writer's addition
/// (e.g. a second Claude Code session's agent registration vanishing
/// from the dashboard).
#[test]
fn mutate_locked_survives_concurrent_writers() {
    let _iso = crate::core::data_dir::isolated_data_dir();

    let handles: Vec<_> = (0..8)
        .map(|i| {
            std::thread::spawn(move || {
                // `mutate_locked` bounds its wait for the registry file lock
                // and reports exhaustion as a refusal: the lock was never
                // taken, nothing was written, and no other writer's change
                // was lost. Eight threads on a saturated CI runner can
                // exceed that budget, so a refusal is retried rather than
                // failed — the invariant under test is "no lost update",
                // not "the lock is never contended".
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                loop {
                    let outcome = AgentRegistry::mutate_locked(|registry| {
                        registry.agents.push(AgentEntry {
                            agent_id: format!("agent-{i}"),
                            agent_type: "test".to_string(),
                            role: None,
                            project_root: "/tmp/project".to_string(),
                            started_at: Utc::now(),
                            last_active: Utc::now(),
                            pid: 10_000 + i,
                            process_identity: None,
                            durable_identity_id: None,
                            status: AgentStatus::Active,
                            status_message: None,
                        });
                    });
                    match outcome {
                        Ok(_) => return,
                        Err(error)
                            if error.contains("timed out")
                                && std::time::Instant::now() < deadline =>
                        {
                            std::thread::yield_now();
                        }
                        Err(error) => panic!("mutate_locked must succeed: {error}"),
                    }
                }
            })
        })
        .collect();

    for h in handles {
        h.join().expect("writer thread must not panic");
    }

    let registry = AgentRegistry::load_or_create();
    assert_eq!(
        registry.agents.len(),
        8,
        "all 8 concurrent registrations must survive, got {}",
        registry.agents.len()
    );
}
