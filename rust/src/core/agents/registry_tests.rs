// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;

use chrono::Utc;

use super::{AgentDiary, AgentEntry, AgentRegistry, AgentStatus, DiaryEntryType, ScratchpadEntry};
use crate::core::a2a::message::{MessagePriority, PrivacyLevel};

#[test]
fn register_and_list() {
    let mut reg = AgentRegistry::new();
    let id = reg
        .register("cursor", Some("dev"), "/tmp/project", None)
        .expect("current process can be registered");
    assert!(!id.is_empty(), "`id` is empty");
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

/// AP07-G9: a post retried after a lost response was stored twice. With a
/// key the retry returns the first message; the key reused for another
/// message is refused; without a key nothing changes; a registry written
/// before keys existed loads and its messages never match a key.
#[test]
fn keyed_post_is_stored_once_and_refuses_a_changed_request() {
    let mut reg: AgentRegistry = serde_json::from_str(
        r#"{"agents":[],"scratchpad":[{"id":"legacy","from_agent":"agent-a","to_agent":null,
            "category":"status","message":"build green","timestamp":"2026-01-01T00:00:00Z",
            "read_by":[]}],"updated_at":"2026-01-01T00:00:00Z"}"#,
    )
    .expect("a registry without keys loads");
    let post = |reg: &mut AgentRegistry, project: &str, key: Option<&str>, message: &str| {
        reg.post_message_keyed(
            Some(project),
            "agent-a",
            None,
            "status",
            message,
            PrivacyLevel::Team,
            MessagePriority::Normal,
            Some(1),
            key,
        )
    };

    let first = post(&mut reg, "/project-a", Some("k1"), "build green").expect("first post");
    assert_ne!(first, "legacy");
    let replay = post(&mut reg, "/project-a", Some("k1"), "build green").expect("replay");
    assert_eq!(replay, first);
    let conflict = post(&mut reg, "/project-a", Some("k1"), "build red").expect_err("changed");
    assert!(conflict.contains("idempotency conflict"), "{conflict}");
    assert!(!conflict.contains("build"), "the refusal is content-free");
    // A key is scoped to its project.
    let other = post(&mut reg, "/project-b", Some("k1"), "build red").expect("other project");
    assert_ne!(other, first);
    // Without a key every post is a message of its own.
    let a = post(&mut reg, "/project-a", None, "build green").expect("unkeyed");
    let b = post(&mut reg, "/project-a", None, "build green").expect("unkeyed");
    assert_ne!(a, b);
    assert_eq!(
        reg.scratchpad.len(),
        5,
        "legacy, k1, k1 in project-b, two unkeyed"
    );
    assert!(
        post(&mut reg, "/project-a", Some(""), "x").is_err(),
        "empty key"
    );
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
fn diary_entry_types() {
    let types = vec![
        DiaryEntryType::Discovery,
        DiaryEntryType::Decision,
        DiaryEntryType::Blocker,
        DiaryEntryType::Progress,
        DiaryEntryType::Insight,
    ];
    for t in types {
        assert!(!format!("{t}").is_empty(), "`format!(\"{{t}}\")` is empty");
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
        transport: None,
        role_source: None,
        parent: None,
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
        transport: None,
        role_source: None,
        parent: None,
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
        transport: None,
        role_source: None,
        parent: None,
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
                            transport: None,
                            role_source: None,
                            parent: None,
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

/// A bus message is admitted when posted and re-authorized on every read.
/// Before, the registry stored the raw message and `read` returned it raw.
#[test]
fn bus_message_is_admitted_on_post_and_reauthorized_under_the_current_policy() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let entry = |message: String, metadata: HashMap<String, String>| ScratchpadEntry {
        id: "m1".into(),
        from_agent: "agent-a".into(),
        to_agent: None,
        task_id: None,
        category: "status".into(),
        priority: MessagePriority::default(),
        privacy: PrivacyLevel::Team,
        message,
        metadata,
        project_root: Some("/project".into()),
        timestamp: Utc::now(),
        read_by: Vec::new(),
        expires_at: None,
    };

    // Post: a credential never reaches the store; allowed text is unchanged.
    let key = concat!("AK", "IAIOSFODNN7EXAMPLE");
    let (stored, _) = ScratchpadEntry::admit_message(
        "agent-a",
        Some("/project"),
        &format!("deploy with aws_key = {key}"),
    )
    .expect("developer mode stores the redacted message");
    assert!(!stored.contains(key), "old code stored the raw message");
    let text = "customer CUS-1234 escalated";
    let (stored, metadata) =
        ScratchpadEntry::admit_message("agent-a", Some("/project"), text).expect("admitted");
    assert_eq!(stored, text);
    let bound = entry(stored, metadata);
    let legacy = entry(text.to_owned(), HashMap::new());
    assert_eq!(bound.served_message(Some("/project")), text);
    assert_eq!(
        legacy.served_message(Some("/project")),
        text,
        "Community developer mode keeps serving records without a binding"
    );
    assert!(
        bound
            .served_message(Some("/other-project"))
            .contains("store.project_mismatch"),
        "a reader in another project is refused"
    );

    // The policy tightens AFTER the write; the same records are read again,
    // also through a fresh load from their serialized form.
    drop(community);
    let pack = crate::core::policy::parse(
        "name = 'bus'\nversion = '1.0.0'\ndescription = 'test'\n[redaction]\ncustomer = 'CUS-[0-9]{4}'\n",
    )
    .expect("valid policy");
    let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(
        crate::core::policy::resolve(&pack).expect("resolved policy"),
    ));
    let reloaded: ScratchpadEntry =
        serde_json::from_str(&serde_json::to_string(&bound).expect("serialize")).expect("load");
    for record in [&bound, &reloaded] {
        let served = record.served_message(Some("/project"));
        assert!(!served.contains("CUS-1234"), "old code returned: {served}");
        assert!(served.contains("[REDACTED:customer]"), "{served}");
    }
    let served = legacy.served_message(Some("/project"));
    assert!(!served.contains("CUS-1234"));
    assert!(
        served.contains("store.no_source_authority"),
        "a record without a binding is withheld under an active policy: {served}"
    );
}

fn claude_transport() -> super::TransportClient {
    super::TransportClient {
        host: "claude-code".to_string(),
        name: Some("claude-code".to_string()),
        version: Some("2.1.300".to_string()),
    }
}

/// Audit bug 1: a quiet but connected MCP transport idles; it is not
/// finished by the worker lease (which still finishes plain workers).
#[test]
fn worker_lease_idles_a_live_transport_instead_of_finishing_it() {
    let _isolated = crate::core::data_dir::isolated_data_dir();
    let mut registry = AgentRegistry::new();
    let id = registry
        .register_transport(
            "coder",
            super::RoleSource::Assumed,
            "/proj/a",
            None,
            claude_transport(),
        )
        .expect("current process has an identity");
    registry.agents[0].last_active =
        Utc::now() - chrono::Duration::seconds(super::active_worker_lease_seconds() + 1);

    registry.cleanup_stale(super::presence_ttl());

    assert_eq!(registry.agents[0].status, AgentStatus::Idle);
    assert_eq!(
        registry.agents[0].status_message.as_deref(),
        Some(super::TRANSPORT_IDLE)
    );
    assert_eq!(registry.list_active(Some("/proj/a")).len(), 1);

    // The next tool call's heartbeat makes it active again.
    registry.update_transport_heartbeat(&id).expect("heartbeat");
    assert_eq!(registry.agents[0].status, AgentStatus::Active);
    assert_eq!(registry.agents[0].status_message, None);
}

/// Audit bug 1 (mixed versions): an older binary still finishes quiet
/// transports. The owning process's heartbeat revives exactly that case.
#[test]
fn transport_heartbeat_revives_a_lease_expired_presence_of_its_own_process() {
    let _isolated = crate::core::data_dir::isolated_data_dir();
    let mut registry = AgentRegistry::new();
    let id = registry
        .register_transport(
            "coder",
            super::RoleSource::Assumed,
            "/proj/a",
            None,
            claude_transport(),
        )
        .expect("current process has an identity");
    registry.agents[0].status = AgentStatus::Finished;
    registry.agents[0].status_message = Some(super::WORKER_LEASE_EXPIRED.to_string());

    // A plain worker heartbeat keeps the old contract.
    assert!(registry.update_heartbeat(&id).is_err());
    registry.update_transport_heartbeat(&id).expect("revived");
    assert_eq!(registry.agents[0].status, AgentStatus::Active);

    // Any other reason to be finished stays final.
    registry.agents[0].status = AgentStatus::Finished;
    registry.agents[0].status_message = Some("connection closed".to_string());
    assert!(registry.update_transport_heartbeat(&id).is_err());

    // So does a presence owned by another process identity.
    registry.agents[0].status_message = Some(super::WORKER_LEASE_EXPIRED.to_string());
    if let Some(identity) = registry.agents[0].process_identity.as_mut() {
        identity.start_marker = identity.start_marker.saturating_add(1);
    }
    assert!(registry.update_transport_heartbeat(&id).is_err());
    assert_eq!(registry.agents[0].status, AgentStatus::Finished);
}

/// Audit bugs 2 and 5: the transport records its host and the role's
/// provenance; an assumed role never replaces a declared one, and an
/// identified host is never downgraded to `unknown`.
#[test]
fn transport_registration_keeps_declared_role_and_identified_host() {
    let mut registry = AgentRegistry::new();
    let id = registry
        .register_transport(
            "coder",
            super::RoleSource::Assumed,
            "/project",
            None,
            claude_transport(),
        )
        .expect("transport");
    assert_eq!(
        registry.agents[0].role_source,
        Some(super::RoleSource::Assumed)
    );
    assert_eq!(
        registry.agents[0]
            .transport
            .as_ref()
            .map(|t| t.host.as_str()),
        Some("claude-code")
    );

    // The agent declares itself through ctx_agent on the same process.
    let same = registry
        .register("claude", Some("reviewer"), "/project", None)
        .expect("ctx_agent register");
    assert_eq!(same, id);
    assert_eq!(registry.agents[0].role.as_deref(), Some("reviewer"));
    assert_eq!(
        registry.agents[0].role_source,
        Some(super::RoleSource::Declared)
    );
    assert!(
        registry.agents[0].transport.is_some(),
        "still the transport"
    );

    // A later presence retry (assumed role, handshake not visible) changes
    // neither the declared role and type nor the identified host.
    registry
        .register_transport(
            "coder",
            super::RoleSource::Assumed,
            "/project",
            None,
            super::TransportClient::pending(),
        )
        .expect("retry");
    let entry = &registry.agents[0];
    assert_eq!(entry.role.as_deref(), Some("reviewer"));
    assert_eq!(entry.role_source, Some(super::RoleSource::Declared));
    assert_eq!(entry.agent_type, "claude");
    assert_eq!(
        entry.transport.as_ref().map(|t| t.host.as_str()),
        Some("claude-code")
    );
}

#[test]
fn startup_transport_is_attributed_once_the_handshake_arrives() {
    let mut registry = AgentRegistry::new();
    registry
        .register_transport(
            super::PLACEHOLDER_ROLE,
            super::RoleSource::Assumed,
            "/project",
            None,
            super::TransportClient::pending(),
        )
        .expect("startup presence");
    // A ctx_agent worker of another kind is not a transport and is untouched.
    let mut worker = test_entry("worker", "/project", std::process::id());
    worker.process_identity = None;
    registry.agents.push(worker);

    assert_eq!(registry.record_transport_client(&claude_transport()), 1);
    assert_eq!(registry.agents[0].transport, Some(claude_transport()));
    assert_eq!(registry.agents[1].transport, None);
}

#[test]
fn transport_fields_roundtrip_and_old_records_still_load() {
    let mut registry = AgentRegistry::new();
    registry
        .register_transport(
            "coder",
            super::RoleSource::Assumed,
            "/project",
            None,
            claude_transport(),
        )
        .expect("transport");
    let json = serde_json::to_value(&registry.agents[0]).expect("serialize");
    assert_eq!(json["transport"]["host"], "claude-code");
    assert_eq!(json["role_source"], "assumed");

    let mut legacy = json.clone();
    let legacy_object = legacy.as_object_mut().expect("object");
    legacy_object.remove("transport");
    legacy_object.remove("role_source");
    let loaded: AgentEntry = serde_json::from_value(legacy).expect("legacy record loads");
    assert!(loaded.transport.is_none());
    assert!(loaded.role_source.is_none());
}

/// WEB-2 (25.10.): a subagent shares its parent's MCP process, and under the
/// parent's id its reads consumed the lead's directed messages. Its role now
/// gets its own id next to the parent's; neither reads the other's mail.
#[test]
fn a_subagent_role_gets_its_own_id_and_never_reads_the_parents_messages() {
    // `post_message` stores the message through `admit_and_bind`, which reads
    // process-wide state other tests change under this lock (the working
    // directory, an active project policy): without a project under a
    // protecting policy it is refused (`store.project_unresolved`).
    let _lock = crate::core::data_dir::test_env_lock();
    let mut reg = AgentRegistry::new();
    let lead = reg
        .register("claude", Some("lead"), "/tmp/project", None)
        .expect("parent");
    let web = reg
        .register_child("claude", "web-2", "/tmp/project", &lead)
        .expect("child");
    assert_ne!(web, lead);
    assert!(reg.held_by_this_process(&lead) && reg.held_by_this_process(&web));
    assert!(!reg.held_by_this_process("claude-1-not-here"));
    assert_eq!(reg.declared_role(&lead).as_deref(), Some("lead"));
    assert_eq!(
        reg.register_child("claude", "web-2", "/tmp/project", &lead)
            .expect("child again"),
        web,
        "the same role keeps its id"
    );
    assert_eq!(
        reg.register("claude", Some("lead"), "/tmp/project", None)
            .expect("parent again"),
        lead,
        "the parent's re-register never lands on its child"
    );
    assert!(
        reg.register_child("claude", "x", "/tmp/project", "claude-1-not-here")
            .is_err(),
        "a child needs a parent of this process"
    );

    reg.post_message("engine-1", Some(&lead), "status", "for the lead")
        .expect("message");
    assert!(reg.read_unread(&web).expect("child reads").is_empty());
    assert_eq!(reg.read_unread(&lead).expect("lead reads").len(), 1);
}

/// WEB saw 14 of 55 messages: one read marked all of them read and the host
/// cut the long answer. A read now shows a page and leaves the rest unread;
/// 60 messages arrive exactly once, in order, over several reads.
#[test]
fn sixty_messages_arrive_exactly_once_over_pages() {
    let mut reg = AgentRegistry::new();
    let me = reg
        .register("claude", Some("web"), "/project", None)
        .expect("reader");
    for n in 0..60 {
        reg.post_message_scoped(
            Some("/project"),
            "lead",
            Some(&me),
            "status",
            &format!("directive {n:02} {}", "x".repeat(200)),
            PrivacyLevel::Team,
            MessagePriority::Normal,
            Some(24),
        )
        .expect("message");
    }
    let render = |m: &ScratchpadEntry| format!("{}\n", m.message);
    let mut seen = Vec::new();
    let mut reads = 0;
    loop {
        let (shown, left) = reg
            .read_unread_page(&me, "/project", 2_000, render)
            .expect("page");
        reads += 1;
        assert!(shown.iter().map(String::len).sum::<usize>() <= 2_000 || shown.len() == 1);
        assert_eq!(left, 60 - seen.len() - shown.len(), "the rest stays unread");
        seen.extend(shown);
        if left == 0 {
            break;
        }
    }
    assert!(reads > 1, "60 long messages need more than one page");
    assert_eq!(seen.len(), 60);
    for (n, line) in seen.iter().enumerate() {
        assert!(line.starts_with(&format!("directive {n:02} ")), "{line}");
    }
    let (empty, left) = reg
        .read_unread_page(&me, "/project", 2_000, render)
        .expect("drained");
    assert!(empty.is_empty() && left == 0);
}

/// After the MCP reconnect (25.10.) every session got a new id and with it
/// 123 hours-old broadcasts as unread. A fresh agent gets broadcasts from
/// shortly before its registration on, and every message addressed to it.
#[test]
fn a_new_agent_gets_no_backlog_of_old_broadcasts() {
    let mut reg = AgentRegistry::new();
    let post = |reg: &mut AgentRegistry, to: Option<&str>, text: &str, minutes_ago: i64| {
        reg.post_message_scoped(
            Some("/project"),
            "lead",
            to,
            "status",
            text,
            PrivacyLevel::Team,
            MessagePriority::Normal,
            Some(24),
        )
        .expect("message");
        let last = reg.scratchpad.last_mut().expect("posted");
        last.timestamp = Utc::now() - chrono::Duration::minutes(minutes_ago);
    };
    let me = reg
        .register("claude", Some("improver"), "/project", None)
        .expect("reader");
    post(&mut reg, None, "old broadcast", 120);
    post(&mut reg, Some(&me), "old directive for me", 120);
    post(&mut reg, None, "broadcast during the reconnect", 2);
    post(&mut reg, None, "new broadcast", 0);

    let render = |m: &ScratchpadEntry| format!("{}\n", m.message);
    let (shown, left) = reg
        .read_unread_page(&me, "/project", 10_000, render)
        .expect("page");
    assert_eq!(left, 0);
    assert_eq!(
        shown,
        vec![
            "old directive for me\n",
            "broadcast during the reconnect\n",
            "new broadcast\n"
        ]
    );
}
