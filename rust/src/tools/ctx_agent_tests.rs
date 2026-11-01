// SPDX-License-Identifier: Apache-2.0

use super::handle;

fn call(action: &str, project: &str, agent: Option<&str>) -> String {
    handle(
        action,
        Some("codex"),
        None,
        None,
        project,
        agent,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        false,
        None,
        None,
    )
}

/// AP05-G10: diaries were one file per agent id across projects, and
/// `diaries`, `recall_diary` and the sync "Shared contexts" count read the
/// whole installation.
#[test]
fn diaries_and_sync_counts_stay_in_their_project() {
    use crate::core::agents::{AgentDiary, DiaryEntryType};
    let _iso = crate::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let (p1, p2) = (dir.path().join("p1"), dir.path().join("p2"));
    let (p1, p2) = (p1.to_str().unwrap(), p2.to_str().unwrap());
    for (project, note) in [(p1, "p1-only-note"), (p2, "p2-only-note")] {
        let mut diary = AgentDiary::load_or_create("agent-x", "codex", project);
        diary.add_entry(DiaryEntryType::Discovery, note, None);
        diary.save().unwrap();
    }
    let recalled = call("recall_diary", p2, Some("agent-x"));
    assert!(recalled.contains("p2-only-note"), "{recalled}");
    assert!(!recalled.contains("p1-only-note"), "{recalled}");
    assert!(call("diaries", p2, None).contains("agent-x: 1 entries"));

    let shared = crate::core::data_dir::lean_ctx_data_dir()
        .unwrap()
        .join("agents/shared");
    for (project, files) in [(p1, 2), (p2, 1)] {
        let partition = shared.join(crate::core::project_hash::hash_project_root(project));
        std::fs::create_dir_all(&partition).unwrap();
        for n in 0..files {
            std::fs::write(partition.join(format!("{n}.json")), "{}").unwrap();
        }
    }
    call("register", p2, None);
    call("register", p2, None);
    let sync = call("sync", p2, None);
    assert!(sync.contains("Shared contexts: 1"), "{sync}");
}

/// `export` prunes the shared task store. It must do so under the store
/// lock: a task another writer creates meanwhile survives, and the prune
/// itself is durable instead of a discarded in-memory copy.
#[test]
fn export_prune_and_concurrent_task_mutations_both_survive() {
    use crate::core::a2a::task::{Task, TaskState, TaskStore};
    let _iso = crate::core::data_dir::isolated_data_dir();
    let path = TaskStore::default_path().unwrap();
    let mut stale = Task::new("agent-x", "agent-x", "stale finished task");
    stale.transition(TaskState::Working, None).unwrap();
    stale.transition(TaskState::Completed, None).unwrap();
    // Age the whole task consistently: the store refuses timestamps outside its lifetime.
    let age = chrono::Duration::hours(100);
    stale.created_at -= age;
    stale.updated_at -= age;
    for step in &mut stale.history {
        step.timestamp -= age;
    }
    for message in &mut stale.messages {
        message.timestamp -= age;
    }
    let stale_id = stale.id.clone();
    TaskStore::mutate_locked(&path, |store| {
        store.tasks.push(stale);
        Ok::<_, std::io::Error>(())
    })
    .unwrap();

    let writes = 8;
    let writer = {
        let path = path.clone();
        std::thread::spawn(move || {
            (0..writes)
                .map(|n| {
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                    loop {
                        match TaskStore::mutate_locked(&path, |store| {
                            Ok::<_, std::io::Error>(store.create_task(
                                "agent-y",
                                "agent-x",
                                &format!("live task {n}"),
                            ))
                        }) {
                            Ok(id) => break id,
                            Err(error)
                                if error.to_string().contains("timed out")
                                    && std::time::Instant::now() < deadline => {}
                            Err(error) => panic!("task mutation failed: {error}"),
                        }
                    }
                })
                .collect::<Vec<_>>()
        })
    };
    // The store lock is a polled try-lock, not a fair queue: under load the
    // writer's back-to-back mutations keep a waiter out past the default
    // 750 ms. Export waits longer for the lock, so it never answers "task
    // storage unavailable" here, and neither side's update is lost.
    let export = || {
        let exported = call("export", "/project", Some("agent-x"));
        assert!(!exported.contains("task storage unavailable"), "{exported}");
    };
    for _ in 0..writes {
        export();
    }
    let created = writer.join().unwrap();
    export();

    let ids = TaskStore::read_locked(&path, |store| {
        store
            .tasks
            .iter()
            .map(|task| task.id.clone())
            .collect::<std::collections::HashSet<_>>()
    })
    .unwrap();
    assert!(!ids.contains(&stale_id), "the prune was not persisted");
    for id in &created {
        assert!(ids.contains(id), "concurrent task {id} was lost");
    }
    assert_eq!(ids.len(), writes);
}

#[test]
fn register_forwards_explicit_durable_identity_binding() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    crate::core::agent_registry::register("durable-tool", "coder", "owner")
        .expect("durable identity");
    let result = handle(
        "register",
        Some("mcp"),
        Some("context-engine"),
        Some("durable-tool"),
        "/project",
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        false,
        None,
        None,
    );
    assert!(
        result.starts_with("Agent registered:"),
        "unexpected: {result}"
    );
    let registry = crate::core::agents::AgentRegistry::load().expect("presence registry");
    assert_eq!(
        registry.agents[0].durable_identity_id.as_deref(),
        Some("durable-tool")
    );
}

#[test]
fn bound_handoff_posts_before_finishing_presence() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    crate::core::agent_registry::register("durable-handoff", "coder", "owner")
        .expect("durable identity");
    let registered = handle(
        "register",
        Some("codex"),
        Some("review"),
        Some("durable-handoff"),
        "/project",
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        false,
        None,
        None,
    );
    let agent_id = registered
        .split_whitespace()
        .nth(2)
        .expect("registered agent id")
        .to_string();

    let result = handle(
        "handoff",
        None,
        None,
        None,
        "/project",
        Some(&agent_id),
        Some("verified summary"),
        None,
        Some(&agent_id),
        None,
        None,
        None,
        None,
        None,
        false,
        None,
        None,
    );
    assert!(
        result.starts_with("Handoff complete:"),
        "unexpected: {result}"
    );

    let registry = crate::core::agents::AgentRegistry::load().expect("presence registry");
    let agent = registry
        .agents
        .iter()
        .find(|agent| agent.agent_id == agent_id)
        .expect("handed-off presence");
    assert_eq!(agent.status, crate::core::agents::AgentStatus::Finished);
    let message = registry.scratchpad.last().expect("handoff message");
    assert_eq!(message.from_agent, agent_id);
    assert_eq!(message.to_agent.as_deref(), Some(agent_id.as_str()));
    assert_eq!(message.category, "handoff");
    assert!(message.message.contains("verified summary"));
}

#[test]
fn hive_vetoes_pause_only_the_hive_side_of_a_lease_and_handoff() {
    use crate::core::hive::bus::control;
    use lean_ctx_protocol::hive::bus::{BusFrameV1, ControlAction};
    let iso = crate::core::data_dir::isolated_data_dir();
    let project = iso.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("a.rs"), "fn a() {}\n").unwrap();
    let root = project.to_str().unwrap();
    let store = crate::core::hive::HiveStore::open().unwrap();
    control::enroll_for_tests(&store, 1);
    let call = |action: &str, agent: Option<&str>, message: Option<&str>, to: Option<&str>| {
        handle(
            action,
            Some("codex"),
            Some("dev"),
            None,
            root,
            agent,
            message,
            None,
            to,
            None,
            None,
            None,
            None,
            None,
            false,
            None,
            None,
        )
    };
    let registered = call("register", None, None, None);
    let agent = registered.split_whitespace().nth(2).unwrap().to_string();
    let peer = call("register", None, None, None);
    let peer = peer.split_whitespace().nth(2).unwrap().to_string();
    let (_, vetoed_ref) = super::lease_resource(root, "a.rs").unwrap();
    let veto = |seq: u64, target_ref: String| BusFrameV1::Control {
        control_id: format!("ctl_{seq}"),
        target_agent: None,
        target_ref: Some(target_ref),
        action: ControlAction::Veto,
        reason: "Owner review.".into(),
        issued_by: "admin@acme".into(),
        seq,
        server_time: lean_ctx_protocol::UtcTimestamp::new("2026-10-21T12:00:00Z").unwrap(),
    };
    control::record(&store, &veto(1, vetoed_ref)).unwrap();
    control::record(&store, &veto(2, format!("handoff:{agent}:{peer}"))).unwrap();
    control::record(
        &store,
        &BusFrameV1::Control {
            control_id: "ctl_3".into(),
            target_agent: None,
            target_ref: None,
            action: ControlAction::Stop,
            reason: "Owner review.".into(),
            issued_by: "admin@acme".into(),
            seq: 3,
            server_time: lean_ctx_protocol::UtcTimestamp::new("2026-10-21T12:00:00Z").unwrap(),
        },
    )
    .unwrap();

    // Local work is never gated: the vetoed lease holds on this machine and
    // says that Hive was not asked; the handoff here completes.
    let local = call("lease_acquire", Some(&agent), Some("a.rs"), None);
    assert!(local.starts_with("Lease granted:"), "{local}");
    assert!(local.contains("scope=local (Hive control:"), "{local}");
    std::fs::write(project.join("b.rs"), "fn b() {}\n").unwrap();
    let granted = call("lease_acquire", Some(&agent), Some("b.rs"), None);
    assert!(granted.starts_with("Lease granted:"), "{granted}");
    assert!(!granted.contains("Hive control"), "{granted}");
    let handoff = call("handoff", Some(&agent), Some("done"), Some(&peer));
    assert!(handoff.starts_with("Handoff complete:"), "{handoff}");
}

/// A refusal or veto Hive sends after the call answered releases the local
/// lease and reaches the agent once, with its next `ctx_agent` call.
#[test]
fn a_late_hive_veto_releases_the_local_lease_and_reaches_the_next_call() {
    use crate::core::hive::background;
    use crate::core::hive::bus::leases::{Outcome, caller};
    let iso = crate::core::data_dir::isolated_data_dir();
    let project = iso.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("a.rs"), "fn a() {}\n").unwrap();
    let root = project.to_str().unwrap();
    let call = |action: &str, agent: Option<&str>, message: Option<&str>| {
        handle(
            action,
            Some("codex"),
            Some("dev"),
            None,
            root,
            agent,
            message,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
            None,
            None,
        )
    };
    let agent = call("register", None, None);
    let agent = agent.split_whitespace().nth(2).unwrap().to_string();
    let peer = call("register", None, None);
    let peer = peer.split_whitespace().nth(2).unwrap().to_string();
    let granted = call("lease_acquire", Some(&agent), Some("a.rs"));
    let lease_ref = granted
        .strip_prefix("Lease granted: ")
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("{granted}"))
        .to_string();
    let (kind, resource_ref) = super::lease_resource(root, "a.rs").unwrap();
    let settle = super::LeaseSettle {
        kind,
        resource_ref,
        agent_id: agent.clone(),
        lease_ref,
    };
    background::submit(&caller(&agent), move || settle.acquired(Outcome::Vetoed));
    assert!(background::wait_idle(std::time::Duration::from_secs(30)));

    let next = call("control_status", Some(&agent), None);
    assert!(
        next.starts_with("Hive answered your earlier requests:"),
        "{next}"
    );
    assert!(next.contains("a workspace veto bars this agent"), "{next}");
    let taken = call("lease_acquire", Some(&peer), Some("a.rs"));
    assert!(
        taken.starts_with("Lease granted:"),
        "the local lease was released: {taken}"
    );
    let again = call("control_status", Some(&agent), None);
    assert!(!again.contains("Hive answered"), "shown once: {again}");
}
