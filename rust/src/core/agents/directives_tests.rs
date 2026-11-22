// SPDX-License-Identifier: Apache-2.0

use super::*;

fn issue(version: u64, slot: &str) -> DirectiveIssue {
    DirectiveIssue {
        directive_id: format!("plan_AbCdEf12:{version}:{slot}"),
        kind: DirectiveKind::Plan,
        subject: "plan_AbCdEf12".to_string(),
        version,
        slot: Some(slot.to_string()),
        worker_id: Some("w-api".to_string()),
        task_id: Some("t-7".to_string()),
        vault_id: "vlt_1".to_string(),
        digest: format!("sha256:{}", "a".repeat(64)),
        brief_sha256: (slot != "all").then(|| brief_digest(&brief(version, slot))),
        goal_sha256: None,
        text: format!("plan v{version}"),
    }
}

const GOAL: &str = "Ship billing v2";

fn brief(version: u64, slot: &str) -> String {
    format!("Build the {slot} part, v{version}.")
}

/// The body the person confirms for `issue` in LeanCTX Code.
fn body_for(issue: &DirectiveIssue) -> ReleaseBody {
    let slot = issue.slot.as_deref().unwrap_or("all");
    ReleaseBody {
        version: issue.version,
        goal: GOAL.to_string(),
        brief: (slot != "all").then(|| brief(issue.version, slot)),
    }
}

/// Offer `issue` to `agent`: its release id.
fn offer(store: &mut DirectiveStore, agent: &str, issue: &DirectiveIssue) -> String {
    store
        .issue("/tmp/plan-project", agent, issue)
        .expect("offered");
    rid(store, agent, &issue.directive_id)
}

fn registry_with_agent() -> (AgentRegistry, String) {
    let mut registry = AgentRegistry::new();
    let agent = registry
        .register("claude", Some("coder"), "/tmp/plan-project", None)
        .expect("current process can be registered");
    (registry, agent)
}

/// The release id the app reads for `agent`'s `directive_id`.
fn rid(store: &DirectiveStore, agent: &str, directive_id: &str) -> String {
    store
        .directives
        .iter()
        .find(|d| d.agent_id == agent && d.directive_id == directive_id)
        .and_then(|d| d.release_id.clone())
        .expect("offered with a release id")
}

/// Offer and release (the person's "Send to agents"): the bus entry id.
fn deliver(
    store: &mut DirectiveStore,
    registry: &mut AgentRegistry,
    agent: &str,
    issue: &DirectiveIssue,
) -> String {
    let drawn = offer(store, agent, issue);
    let release = store
        .release_offered(registry, &drawn, Some(&body_for(issue)))
        .expect("released");
    assert!(release.released);
    store
        .directives
        .iter()
        .find(|d| d.agent_id == agent && d.directive_id == issue.directive_id)
        .map(|d| d.message_id.clone())
        .unwrap()
}

fn directive_entries(registry: &AgentRegistry) -> usize {
    registry
        .scratchpad
        .iter()
        .filter(|e| e.category == DIRECTIVE_CATEGORY)
        .count()
}

/// O1 (review 84/bd): Cloud's frame only offers. Before the person's
/// release the agent sees nothing: no bus entry, no pending line for
/// `read`/`wait`/tool answers, no ack, nothing reported to Cloud.
#[test]
fn an_offered_directive_is_invisible_until_released() {
    let (mut registry, agent) = registry_with_agent();
    let mut store = DirectiveStore::default();
    assert!(
        store
            .issue("/tmp/plan-project", &agent, &issue(2, "api"))
            .unwrap()
    );
    assert!(
        !store
            .issue("/tmp/plan-project", &agent, &issue(2, "api"))
            .unwrap()
    );
    assert_eq!(store.directives.len(), 1);
    assert_eq!(store.directives[0].state, DirectiveState::Offered);
    assert!(store.directives[0].message_id.is_empty());
    assert_eq!(directive_entries(&registry), 0);
    assert!(store.pending_for(&agent).is_empty());
    assert_eq!(pending_lines(&store, &agent), None);
    assert!(store.unreported_plans().is_empty());
    let hidden = store
        .ack(
            &mut registry,
            &agent,
            "plan_AbCdEf12:2:api",
            Verdict::Accept,
            None,
            None,
        )
        .expect_err("an offered directive cannot be answered");
    assert!(hidden.contains("no directive"), "{hidden}");
    let rid = rid(&store, &agent, "plan_AbCdEf12:2:api");
    assert!(is_release_id(&rid), "{rid}");
    assert!(lines_without(&store, &agent, &rid));
}

/// No agent-readable text carries the release id.
fn lines_without(store: &DirectiveStore, agent: &str, rid: &str) -> bool {
    pending_lines(store, agent).is_none_or(|l| !l.contains(rid))
}

#[test]
fn a_released_directive_is_a_tagged_high_priority_entry_posted_once() {
    let (mut registry, agent) = registry_with_agent();
    let mut store = DirectiveStore::default();
    let rid = offer(&mut store, &agent, &issue(2, "api"));
    let first = store
        .release_offered(&mut registry, &rid, Some(&body_for(&issue(2, "api"))))
        .expect("released");
    assert!(first.released);
    let id = store.directives[0].message_id.clone();
    // Releasing again (idempotent: the earlier delivery stands, with or
    // without the body), or offering again, changes nothing.
    for body in [None, Some(body_for(&issue(2, "api")))] {
        let again = store
            .release_offered(&mut registry, &rid, body.as_ref())
            .expect("consumed id");
        assert!(!again.released);
        assert_eq!(again.released_at, first.released_at);
    }
    assert!(
        !store
            .issue("/tmp/plan-project", &agent, &issue(2, "api"))
            .unwrap()
    );
    assert_eq!(directive_entries(&registry), 1);
    assert_eq!(store.directives.len(), 1);
    let d = &store.directives[0];
    assert_eq!(d.state, DirectiveState::Pending);
    assert!(d.released_at.as_deref().is_some_and(|t| t.ends_with('Z')));
    let entry = registry.scratchpad.iter().find(|e| e.id == id).unwrap();
    assert_eq!(entry.category, DIRECTIVE_CATEGORY);
    assert_eq!(entry.priority, MessagePriority::High);
    assert_eq!(entry.to_agent.as_deref(), Some(agent.as_str()));
    assert_eq!(entry.task_id.as_deref(), Some("t-7"));
    assert_eq!(entry.metadata["plan_id"], "plan_AbCdEf12");
    assert_eq!(entry.metadata["plan_version"], "2");
    assert_eq!(entry.metadata["directive_id"], "plan_AbCdEf12:2:api");
    assert!(!entry.message.contains(&rid));
    // Never re-published: it carries the Cloud inbound key.
    assert!(entry.is_cloud_inbound());
    assert!(lines_without(&store, &agent, &rid));
    assert!(pending_lines(&store, &agent).is_some());
}

/// A directive id rebuilt from what an agent can know
/// (`<plan>:<version>:<slot>`) or any other guess releases nothing.
#[test]
fn only_the_drawn_release_id_releases() {
    let (mut registry, agent) = registry_with_agent();
    let mut store = DirectiveStore::default();
    store
        .issue("/tmp/plan-project", &agent, &issue(2, "api"))
        .unwrap();
    let drawn = rid(&store, &agent, "plan_AbCdEf12:2:api");
    for guess in [
        "plan_AbCdEf12:2:api".to_string(),
        "plan_AbCdEf12:2:all".to_string(),
        drawn.to_uppercase(),
        format!("{drawn}0"),
        "0".repeat(64),
        String::new(),
    ] {
        store
            .release_offered(&mut registry, &guess, Some(&body_for(&issue(2, "api"))))
            .expect_err("not a release");
    }
    assert_eq!(store.directives[0].state, DirectiveState::Offered);
    assert_eq!(directive_entries(&registry), 0);
    // Two offers never share an id.
    store
        .issue("/tmp/plan-project", &agent, &issue(2, "web"))
        .unwrap();
    assert_ne!(drawn, rid(&store, &agent, "plan_AbCdEf12:2:web"));
}

/// Review 84 (d): a release id is bound to one directive and one agent.
#[test]
fn a_release_for_one_agent_never_reaches_another() {
    let (mut registry, agent_a) = registry_with_agent();
    let agent_b = registry
        .register_child("codex", "reviewer", "/tmp/plan-project", &agent_a)
        .expect("second agent");
    let mut store = DirectiveStore::default();
    for agent in [&agent_a, &agent_b] {
        store
            .issue("/tmp/plan-project", agent, &issue(3, "all"))
            .unwrap();
    }
    let rid_a = rid(&store, &agent_a, "plan_AbCdEf12:3:all");
    let rid_b = rid(&store, &agent_b, "plan_AbCdEf12:3:all");
    assert_ne!(rid_a, rid_b);
    let release = store
        .release_offered(&mut registry, &rid_a, Some(&body_for(&issue(3, "all"))))
        .unwrap();
    assert!(release.released);
    assert_eq!(release.agent_id, agent_a);
    assert_eq!(store.pending_for(&agent_a).len(), 1);
    assert!(store.pending_for(&agent_b).is_empty());
    assert_eq!(pending_lines(&store, &agent_b), None);
    assert!(
        registry
            .scratchpad
            .iter()
            .filter(|e| e.category == DIRECTIVE_CATEGORY)
            .all(|e| e.to_agent.as_deref() == Some(agent_a.as_str()))
    );
}

/// Review 84 (e) and contract item 7: a newer version published by Cloud
/// is only offered, even when the agent had the older one released; the
/// older id never releases the newer version, and a superseded offer's id
/// releases nothing.
#[test]
fn a_new_version_is_only_offered_even_after_an_older_release() {
    let (mut registry, agent) = registry_with_agent();
    let mut store = DirectiveStore::default();
    let rid_1 = offer(&mut store, &agent, &issue(1, "api"));
    store
        .release_offered(&mut registry, &rid_1, Some(&body_for(&issue(1, "api"))))
        .unwrap();
    store
        .ack(
            &mut registry,
            &agent,
            "plan_AbCdEf12:1:api",
            Verdict::Accept,
            None,
            None,
        )
        .unwrap();
    assert_eq!(pending_lines(&store, &agent), None);

    let rid_2 = offer(&mut store, &agent, &issue(2, "api"));
    assert_eq!(pending_lines(&store, &agent), None);
    assert_eq!(directive_entries(&registry), 1);
    // v1's id, even with v2's body, releases nothing new.
    assert!(
        !store
            .release_offered(&mut registry, &rid_1, Some(&body_for(&issue(2, "api"))))
            .unwrap()
            .released
    );
    assert_eq!(pending_lines(&store, &agent), None);

    // v3 arrives before v2 was released: v2's id is spent.
    let rid_3 = offer(&mut store, &agent, &issue(3, "api"));
    let older = store
        .release_offered(&mut registry, &rid_2, Some(&body_for(&issue(2, "api"))))
        .expect_err("superseded before its release");
    assert_eq!(older, ReleaseError::Superseded);
    assert_eq!(older.code(), "superseded");
    assert_eq!(pending_lines(&store, &agent), None);
    assert_eq!(directive_entries(&registry), 1);

    // v3's id with v2's body is refused: the body names another version.
    let wrong = store
        .release_offered(&mut registry, &rid_3, Some(&body_for(&issue(2, "api"))))
        .expect_err("version mismatch");
    assert_eq!(wrong.code(), "invalid_argument");
    assert_eq!(directive_entries(&registry), 1);
    assert!(
        store
            .release_offered(&mut registry, &rid_3, Some(&body_for(&issue(3, "api"))))
            .unwrap()
            .released
    );
    let lines = pending_lines(&store, &agent).expect("v3 pending");
    assert!(
        lines.contains("plan_AbCdEf12:3:api") && !lines.contains(":2:"),
        "{lines}"
    );
}

/// A released, unanswered older version stays pending while the newer one
/// is offered, and is superseded when the newer one is released.
#[test]
fn releasing_a_newer_version_supersedes_the_unanswered_older_one() {
    let (mut registry, agent) = registry_with_agent();
    let mut store = DirectiveStore::default();
    deliver(&mut store, &mut registry, &agent, &issue(2, "api"));
    store
        .issue("/tmp/plan-project", &agent, &issue(3, "api"))
        .unwrap();
    let lines = pending_lines(&store, &agent).expect("v2 pending");
    assert!(
        lines.contains(":2:api") && !lines.contains(":3:"),
        "{lines}"
    );
    let rid_3 = rid(&store, &agent, "plan_AbCdEf12:3:api");
    store
        .release_offered(&mut registry, &rid_3, Some(&body_for(&issue(3, "api"))))
        .unwrap();
    let old = store
        .ack(
            &mut registry,
            &agent,
            "plan_AbCdEf12:2:api",
            Verdict::Accept,
            None,
            None,
        )
        .expect_err("superseded");
    assert!(old.contains("superseded"), "{old}");
}

#[test]
fn a_newer_version_supersedes_and_acks_are_validated() {
    let (mut registry, agent) = registry_with_agent();
    let mut store = DirectiveStore::default();
    deliver(&mut store, &mut registry, &agent, &issue(2, "api"));
    deliver(&mut store, &mut registry, &agent, &issue(3, "api"));
    let old = store
        .ack(
            &mut registry,
            &agent,
            "plan_AbCdEf12:2:api",
            Verdict::Accept,
            None,
            None,
        )
        .expect_err("superseded");
    assert!(old.contains("superseded"), "{old}");
    let missing = store
        .ack(
            &mut registry,
            &agent,
            "plan_AbCdEf12:3:api",
            Verdict::Refuse,
            None,
            None,
        )
        .expect_err("refusal without a code");
    assert!(missing.contains("reason_code"), "{missing}");
    let refused = store
        .ack(
            &mut registry,
            &agent,
            "plan_AbCdEf12:3:api",
            Verdict::Refuse,
            Some("task_finished"),
            Some("  done already  "),
        )
        .expect("refused");
    assert_eq!(refused.state, DirectiveState::Refused);
    assert_eq!(refused.reason.as_deref(), Some("done already"));
    let entry = registry
        .scratchpad
        .iter()
        .find(|e| e.id == refused.message_id)
        .unwrap();
    assert!(entry.read_by.contains(&agent));
    assert!(store.pending_for(&agent).is_empty());
}

#[test]
fn pending_lines_are_sorted_and_byte_stable() {
    let (mut registry, agent) = registry_with_agent();
    let mut store = DirectiveStore::default();
    for slot in ["web", "api"] {
        deliver(&mut store, &mut registry, &agent, &issue(4, slot));
    }
    let lines = pending_lines(&store, &agent).expect("pending");
    assert_eq!(
        lines,
        "[directive pending: plan plan_AbCdEf12 v4, ack with ctx_agent action=ack directive_id=plan_AbCdEf12:4:api]\n\
         [directive pending: plan plan_AbCdEf12 v4, ack with ctx_agent action=ack directive_id=plan_AbCdEf12:4:web]"
    );
    assert_eq!(pending_lines(&store, "someone-else"), None);
}

#[test]
fn only_changed_plan_states_are_unreported() {
    let (mut registry, agent) = registry_with_agent();
    let mut store = DirectiveStore::default();
    deliver(&mut store, &mut registry, &agent, &issue(5, "api"));
    assert_eq!(store.unreported_plans().len(), 1);
    let id = "plan_AbCdEf12:5:api".to_string();
    store.mark_reported(&[(agent.clone(), id.clone(), DirectiveState::Pending)]);
    assert!(store.unreported_plans().is_empty());
    store
        .ack(&mut registry, &agent, &id, Verdict::Accept, None, None)
        .unwrap();
    let due = store.unreported_plans();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].state.wire(), Some("acked"));
    // A report of the older state does not hide the newer one.
    store.mark_reported(&[(agent.clone(), id.clone(), DirectiveState::Pending)]);
    assert_eq!(store.unreported_plans().len(), 1);
}

/// Review 84: an ack binds to the receiving agent and the delivered
/// version. Agent B cannot answer A's directive, and no version that was
/// not delivered (an older or a future one) can be answered either.
#[test]
fn an_agent_acks_only_its_own_directive_of_the_delivered_version() {
    let (mut registry, agent_a) = registry_with_agent();
    let agent_b = registry
        .register_child("codex", "reviewer", "/tmp/plan-project", &agent_a)
        .expect("second agent");
    assert_ne!(agent_a, agent_b);
    let mut store = DirectiveStore::default();
    deliver(&mut store, &mut registry, &agent_a, &issue(3, "api"));

    let foreign = store
        .ack(
            &mut registry,
            &agent_b,
            "plan_AbCdEf12:3:api",
            Verdict::Accept,
            None,
            None,
        )
        .expect_err("B may not ack A's directive");
    assert!(foreign.contains("no directive"), "{foreign}");
    for other in [
        "plan_AbCdEf12:2:api",
        "plan_AbCdEf12:4:api",
        "plan_AbCdEf12:api",
    ] {
        store
            .ack(&mut registry, &agent_a, other, Verdict::Accept, None, None)
            .expect_err("only the delivered version");
    }
    assert_eq!(store.pending_for(&agent_a).len(), 1);
    assert!(
        store
            .unreported_plans()
            .iter()
            .all(|d| d.agent_id == agent_a)
    );
    let entry = registry
        .scratchpad
        .iter()
        .find(|e| e.id == store.directives[0].message_id)
        .unwrap();
    assert!(!entry.read_by.contains(&agent_b));

    store
        .ack(
            &mut registry,
            &agent_a,
            "plan_AbCdEf12:3:api",
            Verdict::Accept,
            None,
            None,
        )
        .expect("A answers its own");
}

/// Review 84 via af: a released entry is exactly the plan's goal and this
/// agent's own brief of exactly the released version, as the person
/// confirmed them in Code; nothing of another part, another version or a
/// release id.
#[test]
fn a_release_delivers_exactly_the_goal_and_the_agents_own_brief() {
    let (mut registry, agent_a) = registry_with_agent();
    let agent_b = registry
        .register_child("codex", "reviewer", "/tmp/plan-project", &agent_a)
        .expect("second agent");
    let mut store = DirectiveStore::default();
    let rid_a2 = offer(&mut store, &agent_a, &issue(2, "api"));
    let rid_a3 = offer(&mut store, &agent_a, &issue(3, "api"));
    let rid_b = offer(&mut store, &agent_b, &issue(3, "all"));

    // A brief that is not the signed one of this agent's part (another
    // part's, another version's, a missing or a surplus one) is refused.
    let refusals = [
        ReleaseBody {
            brief: Some(brief(3, "web")),
            ..body_for(&issue(3, "api"))
        },
        ReleaseBody {
            brief: Some(brief(2, "api")),
            ..body_for(&issue(3, "api"))
        },
        ReleaseBody {
            brief: None,
            ..body_for(&issue(3, "api"))
        },
        ReleaseBody {
            goal: "  ".to_string(),
            ..body_for(&issue(3, "api"))
        },
        ReleaseBody {
            goal: "Ship\u{1b}[2J".to_string(),
            ..body_for(&issue(3, "api"))
        },
        ReleaseBody {
            goal: "Ship\u{202E}now".to_string(),
            ..body_for(&issue(3, "api"))
        },
        ReleaseBody {
            goal: "Ship\u{2066}now".to_string(),
            ..body_for(&issue(3, "api"))
        },
        ReleaseBody {
            goal: "Ship\u{200B}now".to_string(),
            ..body_for(&issue(3, "api"))
        },
        ReleaseBody {
            goal: "Ship\u{200F}now".to_string(),
            ..body_for(&issue(3, "api"))
        },
    ];
    for body in &refusals {
        let error = store
            .release_offered(&mut registry, &rid_a3, Some(body))
            .expect_err("refused body");
        assert_eq!(error.code(), "invalid_argument", "{body:?}: {error:?}");
    }
    store
        .release_offered(&mut registry, &rid_a3, None)
        .expect_err("a plan release needs its body");
    store
        .release_offered(
            &mut registry,
            &rid_b,
            Some(&ReleaseBody {
                brief: Some(brief(3, "web")),
                ..body_for(&issue(3, "all"))
            }),
        )
        .expect_err("no part names B's plan-wide directive");
    assert_eq!(directive_entries(&registry), 0);

    store
        .release_offered(&mut registry, &rid_a3, Some(&body_for(&issue(3, "api"))))
        .unwrap();
    store
        .release_offered(&mut registry, &rid_b, Some(&body_for(&issue(3, "all"))))
        .unwrap();
    let hint = |id: &str| ack_hint(id);
    let expected = [
        (
            agent_a.as_str(),
            "plan_AbCdEf12:3:api",
            format!(
                "LeanCTX Cloud: plan plan_AbCdEf12 v3, sent by the person from LeanCTX Code.\nGoal:\nShip billing v2\nYour part (api):\nBuild the api part, v3.\n{}",
                hint("plan_AbCdEf12:3:api")
            ),
        ),
        (
            agent_b.as_str(),
            "plan_AbCdEf12:3:all",
            format!(
                "LeanCTX Cloud: plan plan_AbCdEf12 v3, sent by the person from LeanCTX Code.\nGoal:\nShip billing v2\nNo part of this plan names you: check that your work still fits its goal.\n{}",
                hint("plan_AbCdEf12:3:all")
            ),
        ),
    ];
    let entries: Vec<_> = registry
        .scratchpad
        .iter()
        .filter(|e| e.category == DIRECTIVE_CATEGORY)
        .collect();
    assert_eq!(entries.len(), expected.len());
    for (agent, directive_id, text) in &expected {
        let entry = entries
            .iter()
            .find(|e| e.to_agent.as_deref() == Some(*agent))
            .expect("one entry per released agent");
        assert_eq!(&entry.message, text);
        let mut fields: Vec<(&str, &str)> = entry
            .metadata
            .iter()
            .filter(|(k, _)| !k.starts_with("lean_ctx."))
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        fields.sort_unstable();
        assert_eq!(
            fields,
            [
                ("directive_id", *directive_id),
                ("plan_id", "plan_AbCdEf12"),
                ("plan_version", "3"),
            ]
        );
        let whole = serde_json::to_string(entry).unwrap();
        if *agent == agent_b.as_str() {
            assert!(!whole.contains("Build the api part"), "{whole}");
        }
        for absent in [
            rid_a2.as_str(),
            rid_a3.as_str(),
            rid_b.as_str(),
            "Build the web part",
            "v2.",
        ] {
            assert!(!whole.contains(absent), "{absent} in {whole}");
        }
    }
}

/// Route "ctx_* / MCP tools": `directives.json`, which an agent's read
/// tools may open, holds only the digest of a release id; the ids live in
/// a file every agent read, search and shell tool refuses as a secret.
#[test]
fn release_ids_stay_out_of_what_agent_tools_can_read() {
    let _data = crate::core::data_dir::isolated_data_dir();
    DirectiveStore::mutate(|_, store| {
        store
            .issue("/tmp/plan-project", "agent-x", &issue(3, "api"))
            .map(|_| ())
    })
    .expect("offered");
    let app = DirectiveStore::load_for_app();
    let drawn = app.directives[0]
        .release_id
        .clone()
        .expect("the app's read");
    let agents = crate::core::agents::persistence::agents_dir().unwrap();
    let stored = std::fs::read_to_string(agents.join("directives.json")).unwrap();
    assert!(!stored.contains(&drawn), "{stored}");
    assert_eq!(DirectiveStore::load().directives[0].release_id, None);

    let ids = agents.join(RELEASE_IDS_FILE);
    assert!(std::fs::read_to_string(&ids).unwrap().contains(&drawn));
    assert!(crate::core::io_boundary::is_secret_like(&ids).is_some());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&ids).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // Released, the id leaves the id file (the file goes with the last one).
    let mut released = false;
    DirectiveStore::mutate(|registry, store| {
        let agent = registry
            .register("claude", Some("coder"), "/tmp/plan-project", None)
            .map_err(|e| e.clone())?;
        store.directives[0].agent_id = agent;
        released = store
            .release_offered(registry, &drawn, Some(&body_for(&issue(3, "api"))))
            .map_err(|e| e.message())?
            .released;
        Ok(())
    })
    .expect("released");
    assert!(released);
    assert!(!ids.exists());
    assert_eq!(
        DirectiveStore::load_for_app().directives[0].release_id,
        None
    );
}

/// Offer `issue(3, "api")` with a distinctive text to a registered agent in
/// the isolated agents dir: (agent, release id, agents dir).
fn offer_on_disk() -> (String, String, std::path::PathBuf) {
    let (_, agent) = AgentRegistry::mutate_locked(|registry| {
        registry.register("claude", Some("coder"), "/tmp/plan-project", None)
    })
    .expect("registry");
    let agent = agent.expect("registered");
    let mut offered = issue(3, "api");
    offered.text = "PLAN-TEXT-MARKER: do this next".to_string();
    DirectiveStore::mutate(|_, store| {
        store
            .issue("/tmp/plan-project", &agent, &offered)
            .map(|_| ())
    })
    .expect("offered");
    let drawn = DirectiveStore::load_for_app().directives[0]
        .release_id
        .clone()
        .expect("the app's read");
    let dir = crate::core::agents::persistence::agents_dir().unwrap();
    (agent, drawn, dir)
}

#[test]
fn an_offered_plan_directive_stores_no_plan_text() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let (_, drawn, dir) = offer_on_disk();
    let stored = std::fs::read_to_string(dir.join("directives.json")).unwrap();
    assert!(!stored.contains("PLAN-TEXT-MARKER"), "{stored}");
    assert!(!stored.contains(GOAL), "{stored}");
    assert_eq!(DirectiveStore::load().directives[0].text, "");

    // Released, the record holds the released content.
    let (done, warning) =
        DirectiveStore::mutate_release(&drawn, Some(&body_for(&issue(3, "api")))).unwrap();
    assert!(done.released);
    assert_eq!(warning, None);
    let text = &DirectiveStore::load().directives[0].text;
    assert!(
        text.contains(GOAL) && text.contains(&brief(3, "api")),
        "{text}"
    );
    assert!(!text.contains("PLAN-TEXT-MARKER"), "{text}");
}

#[cfg(unix)]
#[test]
fn the_id_file_is_never_written_through_a_symlink() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let dir = crate::core::agents::persistence::agents_dir().unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let outside = dir.parent().unwrap().join("outside-target.json");
    std::fs::write(&outside, "keep me").unwrap();
    std::os::unix::fs::symlink(&outside, dir.join(RELEASE_IDS_FILE)).unwrap();

    let saved = DirectiveStore::mutate(|_, store| {
        store
            .issue("/tmp/plan-project", "agent-x", &issue(3, "api"))
            .map(|_| ())
    });
    assert!(saved.unwrap_err().contains("symlink"));
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep me");
    let meta = std::fs::symlink_metadata(dir.join(RELEASE_IDS_FILE)).unwrap();
    assert!(meta.file_type().is_symlink());
    // The id file is written first: no record is kept without its id, and
    // the app reads no id through the link either (fail closed).
    assert!(
        DirectiveStore::load_for_app()
            .directives
            .iter()
            .all(|d| d.release_id.is_none())
    );
}

#[test]
fn a_damaged_id_file_is_kept_aside_not_overwritten() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let dir = crate::core::agents::persistence::agents_dir().unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let ids = dir.join(RELEASE_IDS_FILE);
    std::fs::write(&ids, "{not json").unwrap();

    let (_, drawn, _) = offer_on_disk();
    let aside = dir.join(format!("{RELEASE_IDS_FILE}.damaged"));
    assert_eq!(std::fs::read_to_string(&aside).unwrap(), "{not json");
    assert!(crate::core::io_boundary::is_secret_like(&aside).is_some());
    assert!(std::fs::read_to_string(&ids).unwrap().contains(&drawn));
}

#[cfg(unix)]
#[test]
fn a_release_is_reported_delivered_when_only_the_records_fail() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let (agent, drawn, dir) = offer_on_disk();
    // The id file can no longer be written after the bus entry.
    let ids = dir.join(RELEASE_IDS_FILE);
    std::fs::remove_file(&ids).unwrap();
    std::os::unix::fs::symlink(dir.join("elsewhere.json"), &ids).unwrap();

    let (done, warning) = DirectiveStore::mutate_release(&drawn, Some(&body_for(&issue(3, "api"))))
        .expect("delivered is reported as delivered");
    assert!(done.released);
    assert!(warning.expect("the failed write").contains("symlink"));
    let registry = AgentRegistry::load().expect("registry");
    assert_eq!(directive_entries(&registry), 1, "the bus entry is written");
    assert!(registry.agents.iter().any(|a| a.agent_id == agent));
    assert!(!dir.join("elsewhere.json").exists());
}
