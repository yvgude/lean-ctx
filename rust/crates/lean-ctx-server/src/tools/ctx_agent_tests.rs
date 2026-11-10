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
    use lean_ctx::core::agents::{AgentDiary, DiaryEntryType};
    let _iso = lean_ctx::core::data_dir::isolated_data_dir();
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

    let shared = lean_ctx::core::data_dir::lean_ctx_data_dir()
        .unwrap()
        .join("agents/shared");
    for (project, files) in [(p1, 2), (p2, 1)] {
        let partition = shared.join(lean_ctx::core::project_hash::hash_project_root(project));
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
    use lean_ctx::core::a2a::task::{Task, TaskState, TaskStore};
    let _iso = lean_ctx::core::data_dir::isolated_data_dir();
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
    let _iso = lean_ctx::core::data_dir::isolated_data_dir();
    lean_ctx::core::agent_registry::register("durable-tool", "coder", "owner")
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
    let registry = lean_ctx::core::agents::AgentRegistry::load().expect("presence registry");
    assert_eq!(
        registry.agents[0].durable_identity_id.as_deref(),
        Some("durable-tool")
    );
}

#[test]
fn register_stores_a_role_id_and_a_display_name_and_says_what_it_stored() {
    let _iso = lean_ctx::core::data_dir::isolated_data_dir();
    let stored = || {
        let registry = lean_ctx::core::agents::AgentRegistry::load().expect("presence registry");
        let agent = &registry.agents[0];
        (agent.role.clone(), agent.display_name.clone())
    };
    // A session name given as the role is no role: it becomes the name.
    let first = super::register(
        Some("claude"),
        Some("LeanCTX · Contract 3.12.3"),
        None,
        None,
        "/project",
    );
    assert!(
        first.contains("role: none, name: LeanCTX · Contract 3.12.3)"),
        "{first}"
    );
    assert!(first.contains("warning: unknown_role"), "{first}");
    assert_eq!(stored(), (None, Some("LeanCTX · Contract 3.12.3".into())));
    // Re-registering the same presence takes over the new role and name.
    let again = super::register(
        Some("claude"),
        Some("Coder"),
        Some("Contracts"),
        None,
        "/project",
    );
    assert!(again.contains("role: coder, name: Contracts)"), "{again}");
    assert!(!again.contains("warning"), "{again}");
    assert_eq!(stored(), (Some("coder".into()), Some("Contracts".into())));
}

#[test]
fn bound_handoff_posts_before_finishing_presence() {
    let _iso = lean_ctx::core::data_dir::isolated_data_dir();
    lean_ctx::core::agent_registry::register("durable-handoff", "coder", "owner")
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

    let registry = lean_ctx::core::agents::AgentRegistry::load().expect("presence registry");
    let agent = registry
        .agents
        .iter()
        .find(|agent| agent.agent_id == agent_id)
        .expect("handed-off presence");
    assert_eq!(agent.status, lean_ctx::core::agents::AgentStatus::Finished);
    let message = registry.scratchpad.last().expect("handoff message");
    assert_eq!(message.from_agent, agent_id);
    assert_eq!(message.to_agent.as_deref(), Some(agent_id.as_str()));
    assert_eq!(message.category, "handoff");
    assert!(message.message.contains("verified summary"));
}

#[test]
fn hive_vetoes_pause_only_the_hive_side_of_a_lease_and_handoff() {
    use lean_ctx_hive::bus::control;
    let iso = lean_ctx::core::data_dir::isolated_data_dir();
    let project = iso.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("a.rs"), "fn a() {}\n").unwrap();
    let root = project.to_str().unwrap();
    let store = lean_ctx_hive::HiveStore::open().unwrap();
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
    let mut veto = control::control_for_tests("bus", "veto", "ctl_1");
    veto.target_ref = Some(lean_ctx_hive::bus::v2::ControlRef {
        kind: "lease".into(),
        id: vetoed_ref,
    });
    control::keep_for_tests(
        &store,
        vec![veto, control::control_for_tests("bus", "stop", "ctl_3")],
    );

    // Local work is never gated: the vetoed lease holds on this machine and
    // says that Hive was not asked; the handoff here completes.
    let local = call("lease_acquire", Some(&agent), Some("a.rs"), None);
    assert!(local.starts_with("Lease granted:"), "{local}");
    assert!(local.contains("scope=local (claim_vetoed:"), "{local}");
    std::fs::write(project.join("b.rs"), "fn b() {}\n").unwrap();
    let granted = call("lease_acquire", Some(&agent), Some("b.rs"), None);
    assert!(granted.starts_with("Lease granted:"), "{granted}");
    assert!(!granted.contains("claim_vetoed"), "{granted}");
    let handoff = call("handoff", Some(&agent), Some("done"), Some(&peer));
    assert!(handoff.starts_with("Handoff complete:"), "{handoff}");
}

/// A refusal or veto Hive sends after the call answered releases the local
/// lease and reaches the agent once, with its next `ctx_agent` call.
#[test]
fn a_late_hive_veto_releases_the_local_lease_and_reaches_the_next_call() {
    use lean_ctx_hive::background;
    use lean_ctx_hive::bus::leases::{Outcome, caller};
    let iso = lean_ctx::core::data_dir::isolated_data_dir();
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

#[cfg(unix)]
fn export_written(project: &std::path::Path, filename: &str) -> String {
    handle(
        "export",
        Some("codex"),
        None,
        None,
        &project.to_string_lossy(),
        Some("agent-x"),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        true,
        Some(filename),
        None,
    )
}

/// The A2A snapshot export writes into `<project>/.lean-ctx/proofs/`. A
/// symlinked proofs dir or target file must not redirect it outside.
#[cfg(unix)]
#[test]
fn a2a_snapshot_export_refuses_symlinked_destinations() {
    use std::os::unix::fs::symlink;
    let _iso = lean_ctx::core::data_dir::isolated_data_dir();
    let tmp = tempfile::tempdir().unwrap();
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();

    // Symlinked proofs directory.
    let project = tmp.path().join("p1");
    std::fs::create_dir_all(project.join(".lean-ctx")).unwrap();
    symlink(&outside, project.join(".lean-ctx/proofs")).unwrap();
    let answer = export_written(&project, "snap.json");
    assert!(answer.starts_with("Error: write snapshot:"), "{answer}");
    assert!(answer.contains("symlink"), "{answer}");
    assert!(!outside.join("snap.json").exists());

    // Symlinked target file.
    let victim = tmp.path().join("victim.json");
    std::fs::write(&victim, "keep").unwrap();
    let project = tmp.path().join("p2");
    std::fs::create_dir_all(project.join(".lean-ctx/proofs")).unwrap();
    symlink(&victim, project.join(".lean-ctx/proofs/snap.json")).unwrap();
    let answer = export_written(&project, "snap.json");
    assert!(answer.starts_with("Error: write snapshot:"), "{answer}");
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");

    // Not a plain file name.
    let project = tmp.path().join("p3");
    std::fs::create_dir_all(&project).unwrap();
    let answer = export_written(&project, "..");
    assert!(answer.starts_with("Error: write snapshot:"), "{answer}");

    // A normal export still writes.
    let answer = export_written(&project, "snap.json");
    assert!(answer.starts_with("A2A snapshot exported:"), "{answer}");
    let written = std::fs::read_to_string(project.join(".lean-ctx/proofs/snap.json")).unwrap();
    assert!(written.contains("\"schema_version\""), "{written}");
}

/// QN0 over `ctx_agent post`: `queen` reaches the registered Queen (or is
/// refused when none is active), a kind comes only from that Queen (a role
/// named `queen` is a label), and kind, task and thread land on the entry.
#[test]
fn post_routes_queen_refuses_a_kind_from_others_and_stores_task_and_thread() {
    use lean_ctx::core::agents::AgentRegistry;
    use lean_ctx_hive::bus::queen_kinds::{KIND_KEY, THREAD_KEY};
    let _iso = lean_ctx::core::data_dir::isolated_data_dir();
    let project = "/work/app";
    let post = |from: &str, to: Option<&str>, fields: super::PostFields<'_>| {
        super::handle_with(
            "post",
            None,
            None,
            None,
            project,
            Some(from),
            Some("status update"),
            Some("status"),
            to,
            None,
            None,
            None,
            None,
            None,
            false,
            None,
            None,
            fields,
        )
    };

    let mut registry = AgentRegistry::load_or_create();
    let queen = registry
        .register("claude", Some("queen"), project, None)
        .unwrap();
    let mut entry = registry.agents[0].clone();
    entry.agent_id = "mcp-2-pretender".into();
    entry.app_session_id = Some("app-session-pretender-2".into());
    registry.agents.push(entry);
    registry.save().unwrap();

    // No registered Queen yet: `queen` is no address, not even the agent
    // whose role is `queen`.
    let refused = post(
        "mcp-2-pretender",
        Some("queen"),
        super::PostFields::default(),
    );
    assert!(refused.contains("no_queen"), "{refused}");

    lean_ctx_hive::queens::register_in(
        &lean_ctx_hive::queens::data_dir().unwrap(),
        "app-session-queen-1",
        Some("Queen"),
        "2026-11-09T10:00:00Z",
    )
    .unwrap();
    let mut registry = AgentRegistry::load_or_create();
    registry
        .agents
        .iter_mut()
        .find(|a| a.agent_id == queen)
        .unwrap()
        .app_session_id = Some("app-session-queen-1".into());
    registry.save().unwrap();

    let kind = super::PostFields {
        kind: Some("stop"),
        ..Default::default()
    };
    let refused = post("mcp-2-pretender", None, kind);
    assert!(refused.contains("not_queen"), "{refused}");
    let refused = post(
        &queen,
        None,
        super::PostFields {
            kind: Some("no-such-kind"),
            ..Default::default()
        },
    );
    assert!(refused.contains("not posted"), "{refused}");

    let routed = post(
        "mcp-2-pretender",
        Some("queen"),
        super::PostFields {
            kind: None,
            task_id: Some("67"),
            thread: Some("review-r1"),
        },
    );
    assert!(routed.starts_with("Posted"), "{routed}");
    let registry = AgentRegistry::load_or_create();
    let entry = registry.scratchpad.last().expect("posted entry");
    assert_eq!(entry.to_agent.as_deref(), Some(queen.as_str()));
    assert_eq!(entry.task_id.as_deref(), Some("67"));
    assert_eq!(
        entry.metadata.get(THREAD_KEY).map(String::as_str),
        Some("review-r1")
    );
    assert!(!entry.metadata.contains_key(KIND_KEY));

    let stop = post(
        &queen,
        Some("mcp-2-pretender"),
        super::PostFields {
            kind: Some("stop"),
            task_id: Some("67"),
            thread: None,
        },
    );
    assert!(stop.starts_with("Posted"), "{stop}");
    let registry = AgentRegistry::load_or_create();
    let entry = registry.scratchpad.last().expect("posted entry");
    assert_eq!(
        entry.metadata.get(KIND_KEY).map(String::as_str),
        Some("stop")
    );
    assert_eq!(entry.task_id.as_deref(), Some("67"));
}

/// The per-sender budget holds for every post, not only one with a kind:
/// the 21st plain post within a minute is refused and filed nowhere.
#[test]
fn the_21st_plain_post_within_a_minute_is_refused() {
    let _iso = lean_ctx::core::data_dir::isolated_data_dir();
    let project = "/work/app";
    let mut registry = lean_ctx::core::agents::AgentRegistry::load_or_create();
    let worker = registry
        .register("claude", Some("developer"), project, None)
        .unwrap();
    registry.save().unwrap();
    let post = || {
        super::handle_with(
            "post",
            None,
            None,
            None,
            project,
            Some(&worker),
            Some("progress"),
            Some("status"),
            None,
            None,
            None,
            None,
            None,
            None,
            false,
            None,
            None,
            super::PostFields::default(),
        )
    };
    for n in 1..=20 {
        let posted = post();
        assert!(posted.starts_with("Posted"), "post {n}: {posted}");
    }
    let refused = post();
    assert!(refused.contains("not posted: rate_limited"), "{refused}");
    let registry = lean_ctx::core::agents::AgentRegistry::load_or_create();
    let filed = registry
        .scratchpad
        .iter()
        .filter(|e| e.from_agent == worker)
        .count();
    assert_eq!(filed, 20);
}
