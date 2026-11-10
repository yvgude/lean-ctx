// SPDX-License-Identifier: Apache-2.0
//! Local Read v1 shapes: what a local reader (Hive Desktop) builds against.

use serde_json::{Value, json};

use super::*;

fn parse(document: &str) -> Value {
    serde_json::from_str(document).expect("one JSON object")
}

#[test]
fn every_document_and_error_names_the_contract_and_kind() {
    let ok = parse(&document("docs.list", &json!({"pages": []})));
    assert_eq!(ok["contract"], "local-read/1");
    assert_eq!(ok["kind"], "docs.list");
    let err = parse(&error_document(&Failure::new(Code::NoVault, "not linked")));
    assert_eq!(err["contract"], "local-read/1");
    assert_eq!(err["kind"], "error");
    assert_eq!(err["error"]["code"], "no_vault");
    assert_eq!(Code::InvalidArgument.exit_status(), 2);
    assert_eq!(Code::NotConnected.exit_status(), 1);
}

#[test]
fn hive_status_adds_the_local_read_keys_only_on_opt_in() {
    use lean_ctx_protocol::hive::status::HiveCliStatusV1;
    let dir = tempfile::tempdir().unwrap();
    let store = lean_ctx_hive::HiveStore::at(dir.path().join("c"), dir.path().join("d"));
    let report = lean_ctx_hive::status::report(
        &store,
        &lean_ctx_hive::HiveConfig::default(),
        chrono::Utc::now(),
    );
    let args = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    assert!(status::opted_in(&args(&[
        "--json",
        "--contract",
        "local-read/1"
    ])));
    assert!(status::opted_in(&args(&[
        "--contract=local-read/1",
        "--json"
    ])));
    assert!(!status::opted_in(&args(&["--json"])));

    // Plain --json stays the strict cockpit shape (0.5.0 readers).
    let plain = serde_json::to_value(report.cli_status()).unwrap();
    assert!(plain.get("contract").is_none());
    serde_json::from_value::<HiveCliStatusV1>(plain).expect("strict readers decode it");

    let facts = status::LocalFacts {
        capture: None,
        project_root: Some("/work/app".into()),
        linked_vault: Some("acme/app".into()),
        auto_capture: true,
    };
    let shown = serde_json::to_value(status::of_report(&report, facts)).unwrap();
    for key in [
        "plan",
        "lease_state",
        "lease_valid_until",
        "lease_grace_until",
        "locked",
        "trial_ends_at",
        "capture",
        "problem",
        "workspace",
        "seats_limit",
        "org_settings",
    ] {
        assert_eq!(shown[key], Value::Null, "{key} is printed as null");
    }
    assert_eq!(shown["rights"], json!([]));
    assert_eq!(shown["contract"], "local-read/1");
    assert_eq!(shown["kind"], "hive.status");
    assert_eq!(shown["connected"], false, "the cockpit fields stay");
    assert_eq!(
        shown["project"],
        json!({"root": "/work/app", "vault": "acme/app", "linked": true})
    );
    // Without a project the document is folder-less: `project` is null,
    // every other field is there.
    let folderless = serde_json::to_value(status::of_report(
        &report,
        status::LocalFacts {
            capture: None,
            project_root: None,
            linked_vault: None,
            auto_capture: true,
        },
    ))
    .unwrap();
    assert_eq!(folderless["project"], Value::Null);
    let keys = |v: &Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
    assert_eq!(keys(&folderless), keys(&shown), "the same fields");
    assert_eq!(shown["capture_gate"]["enabled"], true);
    assert_eq!(
        shown["trial"],
        json!({"active": null, "ends_at": null}),
        "no lease, no trial"
    );
    assert_eq!(shown["workers"], json!({"active": 0, "retiring": 0}));
}

#[test]
fn knowledge_rows_type_facts_leave_read_noise_out_and_mark_vault_copies() {
    use lean_ctx::core::knowledge::ProjectKnowledge;
    use lean_ctx::core::memory_policy::MemoryPolicy;
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _policy = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let root = tempfile::tempdir().unwrap();
    let policy = MemoryPolicy::default();
    let mut k = ProjectKnowledge::new(root.path().to_str().unwrap());
    k.remember(
        "decision",
        "db",
        "Postgres for the ledger",
        "s1",
        0.9,
        &policy,
    );
    k.remember(
        "finding",
        "src/a.rs",
        "Read src/a.rs (12L)",
        "s1",
        0.6,
        &policy,
    );
    k.remember(
        "hive/acme/app",
        "lc-1",
        "Retries are capped at 3",
        "s2",
        0.8,
        &policy,
    );
    k.facts
        .iter_mut()
        .find(|f| f.key == "lc-1")
        .unwrap()
        .imported_from = Some("hive-repo:@acme/app@r4".into());

    // A second current fact on the same key from another origin.
    let mut twin = k.facts.iter().find(|f| f.key == "db").unwrap().clone();
    twin.origin = lean_ctx::core::knowledge::FactOrigin::Local;
    twin.value = "Postgres 17 for the ledger".into();
    k.facts.push(twin);

    let (rows, noise) = knowledge::local_rows(&k.facts, false);
    assert_eq!(noise, 1, "only the read record is noise");
    let mut ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), rows.len(), "ids are unique");
    let db: Vec<&str> = rows
        .iter()
        .filter(|r| r.key == "db")
        .map(|r| r.id.as_str())
        .collect();
    assert!(db.contains(&"decision/db"), "{db:?}");
    assert!(db.iter().any(|id| id.starts_with("decision/db@")), "{db:?}");
    let rows: Vec<_> = rows
        .into_iter()
        .filter(|r| !(r.key == "db" && r.id != "decision/db"))
        .collect();

    let decision = rows.iter().find(|r| r.key == "db").unwrap();
    assert_eq!(decision.id, "decision/db");
    assert_eq!(decision.fact_type, "decision");
    assert_eq!(decision.status, "local");
    assert_eq!(decision.source.as_ref().unwrap().kind, "session");
    let vault = rows.iter().find(|r| r.key == "lc-1").unwrap();
    assert_eq!(vault.origin, "vault");
    assert_eq!(vault.status, "provisional");
    assert_eq!(vault.source.as_ref().unwrap().reference, "acme/app");

    let query = knowledge::Query {
        categories: vec!["decision".into()],
        limit: 10,
        ..Default::default()
    };
    let list = knowledge::select("/p", rows, &query, noise);
    assert_eq!((list.total, list.returned, list.excluded_noise), (1, 1, 1));
    let shown = parse(&document("knowledge.list", &list));
    let fact = &shown["facts"][0];
    for key in [
        "anchors",
        "source",
        "review",
        "created",
        "updated",
        "confidence",
    ] {
        assert!(fact.get(key).is_some(), "{key} is always present");
    }
    assert_eq!(fact["type"], "decision");
}

#[test]
fn vault_nodes_carry_the_state_hive_derives_and_rejected_ones_are_left_out() {
    use lean_ctx_hive::replica::fold::{FoldEvent, Graph};
    let add = |seq: u64, id: &str, node_type: &str, body: Value| FoldEvent {
        seq,
        event_id: format!("ev{seq}"),
        op: "node.add".into(),
        target: Some(id.into()),
        node_type: Some(node_type.into()),
        body: Some(body),
        sig_invalid: false,
        provenance: Some("workspace".into()),
        review_state: None,
    };
    let mut graph = Graph::default();
    graph
        .fold(&[
            add(
                1,
                "n_obs",
                "fact",
                json!({"statement": "Observed", "evidence": "observed"}),
            ),
            add(
                2,
                "n_new",
                "gotcha",
                json!({"statement": "Not observed yet"}),
            ),
            add(
                3,
                "n_old",
                "decision",
                json!({"statement": "Moved", "anchor_state": "gone",
                "anchors": [{"path": "src/a.rs", "symbol": "f", "digest": "sha256:x"}]}),
            ),
            add(4, "n_ep", "episode", json!({"summary": "not knowledge"})),
        ])
        .unwrap();
    let states = lean_ctx_hive::delivery::knowledge_state::Index::new(&graph);
    let rows = knowledge::vault_rows(&graph, &states, "acme/app");
    let by = |id: &str| rows.iter().find(|r| r.key == id).unwrap();
    assert_eq!(rows.len(), 3, "episodes are not listed");
    assert_eq!(by("n_obs").status, "verified");
    assert_eq!(by("n_new").status, "provisional");
    assert_eq!(by("n_new").fact_type, "gotcha");
    assert_eq!(by("n_old").status, "outdated");
    assert_eq!(by("n_old").review, Some("gone"));
    assert_eq!(by("n_old").anchors[0].symbol.as_deref(), Some("f"));
    assert_eq!(by("n_obs").id, "hive/acme/app/n_obs");
}

#[test]
fn a_doc_page_shows_sections_anchor_states_and_the_section_of_a_note() {
    use lean_ctx_doc::replica::{LogEvent, assemble};
    let ev = |seq: u64, op: &str, target: &str, node_type: Option<&str>, body: Value| LogEvent {
        seq,
        event_id: format!("ev{seq}"),
        op: op.into(),
        target: Some(target.into()),
        node_type: node_type.map(Into::into),
        body: Some(body),
    };
    let space = assemble(&[
        ev(
            1,
            "node.add",
            "n_p",
            Some("page"),
            json!({"slug": "guide/start", "title": "Start",
            "type": "guide", "status": "canonical",
            "anchors": [{"ref": "file:a.rs", "digest": "d1"}, {"ref": "file:b.md", "digest": "d2", "role": "cites"}]}),
        ),
        ev(
            2,
            "node.add",
            "n_s1",
            Some("section"),
            json!({"body": "Intro.\n", "order": 1024}),
        ),
        ev(
            3,
            "node.add",
            "n_s2",
            Some("section"),
            json!({"body": "## Two\nb\n", "order": 2048}),
        ),
        ev(
            4,
            "edge.add",
            "e_1",
            None,
            json!({"kind": "part_of", "from": "n_s1", "to": "n_p"}),
        ),
        ev(
            5,
            "edge.add",
            "e_2",
            None,
            json!({"kind": "part_of", "from": "n_s2", "to": "n_p"}),
        ),
        ev(
            6,
            "node.add",
            "n_n",
            Some("note"),
            json!({"body": "a.rs moved", "status": "open",
            "covers": [{"ref": "file:a.rs", "digest": "d9"}]}),
        ),
        ev(
            7,
            "edge.add",
            "e_3",
            None,
            json!({"kind": "about", "from": "n_n", "to": "n_s2"}),
        ),
        ev(
            8,
            "field.set",
            "n_p",
            None,
            json!({"field": "anchor_state", "value": "to_verify"}),
        ),
        ev(
            9,
            "field.set",
            "n_p",
            None,
            json!({"field": "drift",
            "value": [{"ref": "file:a.rs", "observed": "d9"}]}),
        ),
    ]);
    let rows = docs::list_rows(&space);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "guide/start");
    assert_eq!(
        rows[0].anchor_state, "fresh",
        "a covering note makes it read fresh"
    );
    assert_eq!(
        (rows[0].sections, rows[0].notes, rows[0].anchors),
        (2, 1, 2)
    );

    let page = docs::page_detail(&space, space.page_by_slug("guide/start").unwrap());
    assert_eq!(page.status, "canonical");
    assert_eq!(page.page_kind, "guide");
    assert_eq!(page.sections[1].heading.as_deref(), Some("Two"));
    assert_eq!(page.sections[0].heading, None);
    let a = &page.anchors[0];
    assert_eq!(
        (a.path.as_str(), a.state, a.noted),
        ("a.rs", "to_verify", true)
    );
    assert_eq!(page.anchors[1].role, "cites");
    assert_eq!(page.anchors[1].state, "fresh");
    assert_eq!(page.notes[0].section, Some(2048));
    assert_eq!(page.notes[0].anchor_state, "fresh");
}

#[test]
fn task_rows_fill_what_the_listing_and_the_view_carry() {
    use lean_ctx_protocol::hive::tasks::{TaskSummaryV1, TaskV1};
    let summary: TaskSummaryV1 = serde_json::from_value(json!({"number": 12, "state": "claimed",
        "lifecycle": "running", "title": "Fix flaky login test", "origin": "person",
        "updated_at": "2026-11-04T07:10:00Z", "mine": true, "priority": "high"}))
    .unwrap();
    let row = tasks::summary_row(&summary);
    assert_eq!(row.priority, "high");
    assert_eq!(row.lifecycle.as_deref(), Some("running"));
    assert_eq!(row.goal, None);
    assert_eq!(row.claimed_by.as_ref().map(|c| c.mine), Some(true));
    assert_eq!(row.origin.kind, "person");

    let task: TaskV1 = serde_json::from_value(json!({"schema": "leanctx.hive.task.v1",
        "vault_id": "vlt_1", "number": 12, "state": "claimed", "title": "Fix", "goal": "Green CI",
        "acceptance": "10 runs pass", "origin": {"kind": "agent"},
        "created_by": {"kind": "person", "label": "Yves", "mine": false},
        "created_at": "2026-11-03T10:00:00Z",
        "claim": {"holder": {"kind": "instance", "label": "Studio · tester", "agent": "claude", "mine": true},
            "since": "2026-11-04T07:00:00Z", "until": "2026-11-04T07:15:00Z"},
        "updates": [{"by": {"kind": "instance", "label": "Studio · tester", "mine": true},
            "at": "2026-11-04T07:10:00Z", "summary": "Reproduced."}]}))
    .unwrap();
    let detail = tasks::detail(&lean_ctx_hive::tasks::TaskView {
        task,
        held_until: None,
    });
    let shown = serde_json::to_value(&detail).unwrap();
    assert_eq!(shown["goal"], "Green CI");
    assert_eq!(shown["claimed_by"]["label"], "Studio · tester");
    assert_eq!(shown["claim_until"], "2026-11-04T07:15:00Z");
    assert_eq!(shown["updated"], "2026-11-04T07:10:00Z");
    assert_eq!(shown["created_by"]["label"], "Yves");
    assert_eq!(shown["updates"][0]["by"], "Studio · tester");
    assert_eq!(shown["result"], Value::Null);
}

#[test]
fn a_person_posts_unread_messages_on_a_thread_and_reads_them_back() {
    use lean_ctx::core::agents::AgentRegistry;
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let mut registry = AgentRegistry::default();
    let (ids, _) = bus::post(
        &mut registry,
        &["agent-a".into(), "agent-b".into()],
        "Please rebase",
        Some("t_1"),
        None,
    )
    .unwrap();
    assert_eq!(ids.len(), 2, "one entry per recipient");
    bus::post(&mut registry, &[], "Standup in 5", None, None).unwrap();

    let all = bus::select(
        &registry.scratchpad,
        &bus::Selection {
            limit: 10,
            ..Default::default()
        },
        chrono::Utc::now(),
    );
    assert_eq!(all.len(), 3);
    assert!(all.iter().all(|m| m.from == bus::PERSON));
    assert!(
        all.iter().all(|m| m.read_by == [bus::PERSON]),
        "recipients have not read it"
    );
    let to_b = bus::select(
        &registry.scratchpad,
        &bus::Selection {
            to: Some("agent-b".into()),
            limit: 10,
            ..Default::default()
        },
        chrono::Utc::now(),
    );
    assert_eq!(to_b.len(), 1);
    assert_eq!(to_b[0].thread.as_deref(), Some("t_1"));
    assert_eq!(to_b[0].message, "Please rebase");
    assert_eq!(all[2].to, None, "a broadcast has no recipient");
}

#[test]
fn a_reader_sees_its_own_project_and_project_free_messages_only() {
    use lean_ctx::core::agents::AgentRegistry;
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let mut registry = AgentRegistry::default();
    bus::post(&mut registry, &[], "for app", None, Some("/work/app")).unwrap();
    bus::post(&mut registry, &[], "for other", None, Some("/work/other")).unwrap();
    bus::post(&mut registry, &[], "for everyone", None, None).unwrap();
    let seen = bus::select(
        &registry.scratchpad,
        &bus::Selection {
            project_root: Some("/work/app".into()),
            limit: 10,
            ..Default::default()
        },
        chrono::Utc::now(),
    );
    let texts: Vec<&str> = seen.iter().map(|m| m.message.as_str()).collect();
    assert_eq!(texts, ["for app", "for everyone"]);

    // The person's console (`--all`): every project, each row naming its own.
    let console = bus::select(
        &registry.scratchpad,
        &bus::Selection {
            all: true,
            limit: 10,
            ..Default::default()
        },
        chrono::Utc::now(),
    );
    let rows: Vec<(&str, Option<&str>)> = console
        .iter()
        .map(|m| (m.message.as_str(), m.project_root.as_deref()))
        .collect();
    assert_eq!(
        rows,
        [
            ("for app", Some("/work/app")),
            ("for other", Some("/work/other")),
            ("for everyone", None)
        ]
    );
    assert!(console.iter().all(|m| m.remote_address.is_none()));

    // One project spelled two ways (a trailing slash): still the reader's.
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().to_str().unwrap().to_owned();
    let spelled = format!("{app}/");
    bus::post(&mut registry, &[], "for app, spelled", None, Some(&spelled)).unwrap();
    let seen = bus::select(
        &registry.scratchpad,
        &bus::Selection {
            project_root: Some(app),
            limit: 10,
            ..Default::default()
        },
        chrono::Utc::now(),
    );
    let texts: Vec<&str> = seen.iter().map(|m| m.message.as_str()).collect();
    assert_eq!(texts, ["for everyone", "for app, spelled"]);
}

#[test]
fn a_post_without_project_root_belongs_to_the_detected_project() {
    use lean_ctx::core::agents::AgentRegistry;
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let args: Vec<String> = ["--json", "--broadcast", "--message", "Standup in 5"]
        .map(String::from)
        .to_vec();
    let answer = bus::post_answer(&args);
    assert_eq!(answer.failed, None, "{}", answer.text);
    let detected = project_root(&[]).expect("the test runs inside a project");
    let registry = AgentRegistry::load().expect("the posted registry");
    let posted = registry
        .scratchpad
        .iter()
        .find(|e| e.message == "Standup in 5")
        .expect("the posted message");
    assert_eq!(posted.project_root.as_deref(), Some(detected.as_str()));
}

#[test]
fn a_post_to_a_finished_recipient_writes_nothing() {
    use lean_ctx::core::agents::{AgentRegistry, AgentStatus};
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let mut registry = AgentRegistry::default();
    let gone = registry
        .register("claude", Some("tester"), "/work/app", None)
        .unwrap();
    let agent = registry
        .agents
        .iter_mut()
        .find(|a| a.agent_id == gone)
        .unwrap();
    agent.durable_identity_id = Some("did-1".into());
    agent.status = AgentStatus::Finished;
    let before = registry.scratchpad.len();
    let refused = bus::post(
        &mut registry,
        &["agent-a".into(), gone.clone()],
        "Please rebase",
        None,
        None,
    );
    assert!(
        matches!(refused, Err(bus::PostError::Recipient(_))),
        "{refused:?}"
    );
    assert_eq!(
        registry.scratchpad.len(),
        before,
        "nothing posted to agent-a either"
    );

    let (ids, stored_at) = bus::post(&mut registry, &["agent-a".into()], "ok", None, None).unwrap();
    let entry = registry.scratchpad.iter().find(|e| e.id == ids[0]).unwrap();
    assert_eq!(
        entry.timestamp, stored_at,
        "the answer names the stored time"
    );
}

#[test]
fn a_reader_gets_its_unread_messages_once_marked_and_list_filters_by_thread_and_sender() {
    use lean_ctx::core::agents::AgentRegistry;
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let mut registry = AgentRegistry::default();
    let root = "/work/app";
    bus::post(
        &mut registry,
        &["agent-a".into()],
        "to a",
        Some("t_1"),
        Some(root),
    )
    .unwrap();
    bus::post(&mut registry, &[], "everyone here", None, Some(root)).unwrap();
    bus::post(&mut registry, &["agent-b".into()], "to b", None, Some(root)).unwrap();
    bus::post(
        &mut registry,
        &[],
        "other project",
        None,
        Some("/work/other"),
    )
    .unwrap();

    let peek = bus::read_unread(&mut registry, "agent-a", root, false);
    let texts: Vec<&str> = peek.iter().map(|m| m.message.as_str()).collect();
    assert_eq!(
        texts,
        ["to a", "everyone here"],
        "own and broadcast, own project"
    );
    assert_eq!(
        bus::read_unread(&mut registry, "agent-a", root, true).len(),
        2
    );
    assert!(
        bus::read_unread(&mut registry, "agent-a", root, false).is_empty(),
        "marked messages are read"
    );

    let listed = |selection: bus::Selection| {
        bus::select(&registry.scratchpad, &selection, chrono::Utc::now())
    };
    let threaded = listed(bus::Selection {
        project_root: Some(root.into()),
        thread: Some("t_1".into()),
        limit: 10,
        ..Default::default()
    });
    assert_eq!(threaded.len(), 1);
    assert_eq!(threaded[0].task_id, None);
    assert_eq!(threaded[0].remote_address, None);
    let everywhere = listed(bus::Selection {
        all: true,
        from: Some(bus::PERSON.into()),
        limit: 10,
        ..Default::default()
    });
    assert_eq!(everywhere.len(), 4, "--all spans every project");
}

#[test]
fn a_claimed_path_lists_with_its_project_path_and_only_its_owner_releases_it() {
    use lean_ctx::core::agent_lease;
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_string_lossy().into_owned();
    let args = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let claim = |agent: &str| {
        leases::build_claim(&args(&[
            "--agent",
            agent,
            "--resource-kind",
            "path",
            "--resource",
            "src/lib.rs",
            "--project-root",
            &root,
        ]))
        .unwrap()
    };
    let granted = claim("agent-a");
    assert!(granted.granted);
    let lease = granted.lease.clone().unwrap();
    assert_eq!(lease.resource_kind, "path");
    assert_eq!(lease.path.as_deref(), Some("src/lib.rs"));
    let refused = claim("agent-b");
    assert_eq!(
        (refused.granted, refused.held_by.as_deref()),
        (false, Some("agent-a"))
    );
    assert_eq!(agent_lease::active_shared().unwrap().len(), 1);

    let release = |agent: &str| {
        leases::build_release(&args(&["--agent", agent, "--lease", &lease.lease_ref]))
            .unwrap()
            .released
    };
    assert!(!release("agent-b"), "someone else's lease stays");
    assert!(release("agent-a"));
    assert!(agent_lease::active_shared().unwrap().is_empty());
}

#[test]
fn presence_rows_name_the_session_host_machine_and_vault() {
    use lean_ctx::core::agents::AgentRegistry;
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let mut registry = AgentRegistry::default();
    registry
        .register("claude", Some("tester"), "/work/app", None)
        .unwrap();
    registry.agents[0].display_name = Some("LeanCTX · Contracts".into());
    let mut rows = bus_presence::rows(&registry, "/work/app", false, false, |_| (true, "active"));
    let machine = bus_presence::InstanceRow {
        id: Some("ins_7".into()),
        name: Some("fitim".into()),
    };
    bus_presence::place(&mut rows, &machine, |root| {
        (root == "/work/app").then(|| "acme/app".to_owned())
    });
    let row = serde_json::to_value(&rows[0]).unwrap();
    assert_eq!(row["kind"], "agent");
    assert_eq!(row["session_name"], "LeanCTX · Contracts");
    assert_eq!(row["display_name"], "LeanCTX · Contracts");
    assert_eq!(row["instance"], json!({"id": "ins_7", "name": "fitim"}));
    assert_eq!(
        row["project"],
        json!({"root": "/work/app", "vault": "acme/app"})
    );
    assert!(
        row.get("host").is_some(),
        "host is always present (null until identified)"
    );
}

#[test]
fn a_person_post_says_whether_it_leaves_the_machine() {
    use bus::{CloudFacts, CloudSender, VaultState};
    let to = |t: &[&str]| t.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let vault = |ws: &str, vault: &str| match (ws, vault) {
        ("acme", "app") => VaultState::Open,
        ("acme", "closed") => VaultState::Denied,
        _ => VaultState::Unknown,
    };
    let cloud = |t: &[&str], category: &str, enrolled: bool, screened: bool, accepted: bool| {
        let facts = CloudFacts {
            enrolled,
            vault: &vault,
            screened,
            person_publish: accepted,
            held: false,
        };
        let row = bus::cloud_as(CloudSender::Person, &to(t), category, &facts);
        (row.state, row.reason)
    };
    let held = bus::cloud_as(
        CloudSender::Person,
        &to(&["#acme/app"]),
        "question",
        &CloudFacts {
            enrolled: true,
            vault: &vault,
            screened: false,
            person_publish: true,
            held: true,
        },
    );
    assert_eq!(
        (held.state, held.reason),
        ("held", Some("bus_on_hold")),
        "a bus-wide hold is said, not reported as queued"
    );
    assert_eq!(
        cloud(&["#acme/app"], "question", true, false, true),
        ("queued", None)
    );
    assert_eq!(
        cloud(&["#acme/app"], "progress", true, false, false),
        ("queued", Some("awaiting_hive")),
        "waits until the Hive accepts a person's publish"
    );
    for local in [&[][..], &["mcp-1-a"][..]] {
        assert_eq!(
            cloud(local, "direct", true, false, true),
            ("local_only", Some("not_vault_address"))
        );
    }
    assert_eq!(
        cloud(&["#acme/app"], "question", false, false, true),
        ("local_only", Some("not_enrolled"))
    );
    assert_eq!(
        cloud(&["#acme/other"], "question", true, false, true),
        ("local_only", Some("unknown_vault"))
    );
    assert_eq!(
        cloud(&["#acme/closed"], "question", true, false, true),
        ("withheld", Some("vault_denied"))
    );
    assert_eq!(
        cloud(&["#acme/app"], "answer", true, false, true),
        ("withheld", Some("type_unsupported"))
    );
    assert_eq!(
        cloud(&["#acme/app"], "question", true, true, true),
        ("withheld", Some("screened")),
        "a text the relay's screen blocks is reported, not queued"
    );
    // HN0: a worker's post to a vault stays here; the registered Queen's
    // goes as her worker, without waiting for a person's publish.
    let facts = CloudFacts {
        enrolled: true,
        vault: &vault,
        screened: false,
        person_publish: false,
        held: false,
    };
    let as_sender = |sender| {
        let row = bus::cloud_as(sender, &to(&["#acme/app"]), "progress", &facts);
        (row.state, row.reason)
    };
    assert_eq!(
        as_sender(CloudSender::Worker),
        ("local_only", Some("not_queen"))
    );
    assert_eq!(as_sender(CloudSender::Queen), ("queued", None));
}

#[test]
fn presence_rows_keep_the_project_and_live_agents_newest_first() {
    use lean_ctx::core::agents::AgentRegistry;
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let mut registry = AgentRegistry::default();
    let a = registry
        .register("claude", Some("tester"), "/work/app", None)
        .unwrap();
    // One process registers once; the other presences are copies of it.
    let template = registry.agents[0].clone();
    let entry = |id: &str, root: &str| {
        let mut e = template.clone();
        e.agent_id = id.into();
        e.project_root = root.into();
        e.last_active = chrono::Utc::now();
        e
    };
    let b = "codex-2-b".to_owned();
    let other = "claude-3-other".to_owned();
    registry.agents.push(entry(&b, "/work/app"));
    registry.agents.push(entry(&other, "/work/other"));
    let old = chrono::Utc::now() - chrono::Duration::minutes(5);
    for agent in &mut registry.agents {
        if agent.agent_id == a {
            agent.last_active = old;
        }
    }
    let liveness = |agent: &lean_ctx::core::agents::AgentEntry| {
        if agent.agent_id == b {
            (false, "finished")
        } else {
            (true, "active")
        }
    };
    let ids = |rows: Vec<bus_presence::AgentRow>| {
        rows.into_iter().map(|r| r.agent_id).collect::<Vec<_>>()
    };
    assert_eq!(
        ids(bus_presence::rows(
            &registry,
            "/work/app",
            false,
            false,
            liveness
        )),
        std::slice::from_ref(&a)
    );
    assert_eq!(
        ids(bus_presence::rows(
            &registry,
            "/work/app",
            false,
            true,
            liveness
        )),
        [b.clone(), a.clone()],
        "--all adds finished agents, newest first"
    );
    assert_eq!(
        bus_presence::rows(&registry, "/work/app", true, false, liveness).len(),
        2,
        "--all-projects adds {other}"
    );
    let session = |workspace: &str, id: &str| lean_ctx::core::agents::LogicalSessionPresence {
        source: "claude".into(),
        workspace: workspace.into(),
        session_id: id.into(),
        opened_at: chrono::Utc::now(),
        last_heartbeat: chrono::Utc::now(),
    };
    registry
        .logical_sessions
        .push(session("/work/app", "s-app"));
    registry
        .logical_sessions
        .push(session("/work/other", "s-other"));
    let own: Vec<String> = bus_presence::logical_rows(&registry, "/work/app", false)
        .into_iter()
        .map(|s| s.session_id)
        .collect();
    assert_eq!(own, ["s-app"], "another project's sessions stay out");
    assert_eq!(
        bus_presence::logical_rows(&registry, "/work/app", true).len(),
        2
    );
}

#[test]
fn presence_rows_come_newest_first_with_known_names() {
    use lean_ctx_hive::bus::presence::PresenceMap;
    let now = chrono::Utc::now();
    let ms = |minutes: i64| {
        u64::try_from((now - chrono::Duration::minutes(minutes)).timestamp_millis()).unwrap()
    };
    let mut map = PresenceMap::default();
    let active = serde_json::from_value(json!("active")).unwrap();
    let idle = serde_json::from_value(json!("idle")).unwrap();
    assert!(map.apply("#acme/app", "wkr_old", "tester", idle, ms(30)));
    assert!(map.apply("#acme/app", "wkr_new", "reviewer", active, ms(1)));
    let named = |id: &str| (id == "wkr_new").then(|| "Laptop".to_owned());
    let rows = presence::rows(&map, now, &named);
    assert_eq!(rows[0].worker_id, "wkr_new");
    assert_eq!(rows[0].name.as_deref(), Some("Laptop"));
    assert_eq!(rows[0].state, "active");
    assert_eq!(rows[1].state, "idle");
    assert_eq!(rows[1].address, "#acme/app");
}

#[test]
fn activity_list_prints_bounded_redacted_events_with_every_field() {
    use lean_ctx::core::activity::{Draft, Kind, MAX_TEXT_BYTES, Outcome, Source, record};
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let root = tempfile::tempdir().unwrap();
    let root = root.path().to_str().unwrap().to_owned();
    let token = format!("ghp_{}", "a1".repeat(18));
    record(
        Draft::new("sess-1", "claude", &root, Kind::Shell, Source::Hook)
            .tool("Bash")
            .target(format!("curl -H 'Authorization: token {token}' api"))
            .outcome(Outcome::Ok),
    );
    record(
        Draft::new("sess-1", "claude", &root, Kind::AgentMessage, Source::Hook)
            .text("x".repeat(MAX_TEXT_BYTES * 2)),
    );
    record(Draft::new(
        "other",
        "codex",
        &root,
        Kind::TurnEnd,
        Source::Mcp,
    ));

    let args = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let listed = activity::build(&args(&["--session", "sess-1"]), &root).unwrap();
    let shown = parse(&document("activity.list", &listed));
    assert_eq!(shown["kind"], "activity.list");
    assert_eq!(shown["session_id"], "sess-1");
    assert_eq!(
        shown["project_root"],
        Value::Null,
        "a session spans projects"
    );
    assert_eq!(shown["since"], Value::Null);
    assert_eq!(
        (shown["total"].as_u64(), shown["returned"].as_u64()),
        (Some(2), Some(2))
    );
    let events = shown["events"].as_array().unwrap();
    let shell = &events[0];
    for key in [
        "seq",
        "at",
        "session_id",
        "agent",
        "project_root",
        "kind",
        "source",
        "tool",
        "target",
        "outcome",
        "text",
        "truncated",
    ] {
        assert!(shell.get(key).is_some(), "{key} is always printed");
    }
    assert_eq!(
        (shell["seq"].as_u64(), shell["kind"].as_str()),
        (Some(1), Some("shell"))
    );
    assert_eq!(shell["text"], Value::Null);
    let target = shell["target"].as_str().unwrap();
    assert!(
        !target.contains(&token),
        "the shell redaction masks the token: {target}"
    );
    let reply = &events[1];
    assert_eq!(reply["truncated"], true);
    assert!(reply["text"].as_str().unwrap().len() <= MAX_TEXT_BYTES + 3);

    let project = activity::build(
        &args(&[
            "--project-root",
            &root,
            "--kind",
            "turn_end",
            "--limit",
            "5",
        ]),
        &root,
    )
    .unwrap();
    assert_eq!(project.events.len(), 1);
    assert_eq!(project.events[0].session_id, "other");
    assert!(activity::build(&args(&["--kind", "nope"]), &root).is_err());
    assert!(activity::build(&args(&["--since", "yesterday"]), &root).is_err());
}

#[test]
fn agents_catalog_rows_carry_the_documented_shape_and_never_overclaim() {
    use agents::{CATALOG, Probe, find_binary, parse_version, row};
    let targets =
        lean_ctx::core::editor_registry::build_targets(std::path::Path::new("/nonexistent"));
    let setup_keys = lean_ctx::core::editor_registry::known_agent_keys(&targets);
    let mut ids = std::collections::BTreeSet::new();
    for spec in CATALOG {
        assert!(ids.insert(spec.id), "{} listed twice", spec.id);
        if let Some(key) = spec.setup_key {
            assert!(
                setup_keys.contains(key),
                "{}: `setup --agents {key}` exists",
                spec.id
            );
        }
        for key in spec.registry_keys {
            assert!(setup_keys.contains(*key), "{}: registry key {key}", spec.id);
        }
        if let Some(argv) = spec.resume {
            assert!(
                spec.binaries.contains(&argv[0]),
                "{}: resume runs the agent",
                spec.id
            );
            assert!(
                argv.iter().any(|a| a.contains("{session_id}")),
                "{}",
                spec.id
            );
        }
    }

    let codex = CATALOG.iter().find(|s| s.id == "codex").unwrap();
    let bare = parse(&document(
        "agents.list",
        &agents::AgentList {
            agents: vec![row(codex, Probe::default())],
        },
    ));
    assert_eq!(bare["kind"], "agents.list");
    let a = &bare["agents"][0];
    for key in ["binary", "version", "setup_key"] {
        assert!(a.get(key).is_some(), "{key} is always present");
    }
    assert_eq!(a["installed"], false);
    assert_eq!(
        a["sign_in"]["state"], "unknown",
        "no file, no key: not claimed"
    );
    assert_eq!(a["sign_in"]["evidence"], Value::Null);
    assert_eq!(a["leanctx"]["mcp"], "missing");
    assert_eq!(a["leanctx"]["hooks"], "unsupported");
    assert_eq!(a["leanctx"]["proxy"], "unsupported");
    assert_eq!(a["leanctx"]["mode"], Value::Null);
    assert_eq!(a["leanctx"]["coverage"], Value::Null);
    assert_eq!(a["acp"]["support"], "adapter");
    assert_eq!(a["acp"]["command"][0], "npx");
    assert_eq!(a["acp"]["package"], a["acp"]["command"][2]);
    assert_eq!(
        a["resume"]["command"],
        json!(["codex", "resume", "{session_id}"])
    );

    let attached = row(
        codex,
        Probe {
            binary: Some("/opt/bin/codex".into()),
            version: Some("0.98.0".into()),
            config_dir: true,
            credential_file: true,
            account_config: false,
            api_key_env: vec!["OPENAI_API_KEY"],
            mcp_configured: true,
            hooks_supported: true,
            hook_mode: Some("governor"),
            proxy_supported: true,
            proxy_wired: false,
            coverage: Some("partial"),
        },
    );
    let a = serde_json::to_value(&attached).unwrap();
    assert_eq!(a["installed"], true);
    assert_eq!(
        a["sign_in"],
        json!({"state": "signed_in", "evidence": "credentials_file",
        "api_key_env": ["OPENAI_API_KEY"]})
    );
    assert_eq!(
        a["leanctx"],
        json!({"mcp": "configured", "hooks": "installed",
        "proxy": "not_wired", "mode": "governor", "coverage": "partial"})
    );

    let goose = CATALOG.iter().find(|s| s.id == "goose").unwrap();
    let native = row(
        goose,
        Probe {
            binary: Some("/usr/local/bin/goose".into()),
            mcp_configured: true,
            ..Probe::default()
        },
    );
    assert_eq!(native.acp.command, ["goose", "acp"]);
    assert_eq!(native.leanctx.mode, Some("mcp"));

    // Claude Code on macOS: the token is in the keychain; the account
    // object in ~/.claude.json is the only (non-secret) sign-in evidence.
    let claude = CATALOG.iter().find(|s| s.id == "claude").unwrap();
    let (file, key) = agents::account_marker("claude").unwrap();
    assert_eq!((file, key), (".claude.json", "oauthAccount"));
    assert!(agents::names_account(
        r#"{"oauthAccount": {"emailAddress": "a@b"}}"#,
        key
    ));
    assert!(!agents::names_account(
        r#"{"oauthAccount": {}, "projects": {}}"#,
        key
    ));
    assert!(!agents::names_account("not json", key));
    let signed = row(
        claude,
        Probe {
            account_config: true,
            ..Probe::default()
        },
    );
    assert_eq!(
        (signed.sign_in.state, signed.sign_in.evidence),
        ("signed_in", Some("account_config"))
    );

    use agents::registers_lean_ctx as reg;
    assert!(
        !reg(
            ".claude.json",
            r#"{"projects": {"/code/lean-ctx": {"mcpServers": {}}}}"#
        ),
        "a project named lean-ctx is not a server"
    );
    assert!(reg(
        ".claude.json",
        r#"{"mcpServers": {"lean-ctx": {"command": "x"}}}"#
    ));
    assert!(reg(
        "mcp_config.json",
        r#"{"servers": {"lean-ctx": {}}} // jsonc"#
    ));
    assert!(reg(
        "config.toml",
        "[mcp_servers.lean-ctx]\ncommand = \"x\"\n"
    ));
    assert!(!reg("config.toml", "# see [mcp_servers.lean-ctx] docs\n"));
    assert!(reg("config.yaml", "extensions:\n  lean-ctx:\n    cmd: x\n"));
    assert!(!reg("config.yaml", "lean-ctx: top-level is not an entry\n"));

    for (out, want) in [
        ("codex-cli 0.98.0\n", Some("0.98.0")),
        ("2.1.3 (Claude Code)", Some("2.1.3")),
        ("goose v1.12.0", Some("1.12.0")),
        ("0.4.1-beta.2,", Some("0.4.1-beta.2")),
        ("no version here", None),
    ] {
        assert_eq!(parse_version(out).as_deref(), want, "{out}");
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("kiro-cli");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        assert_eq!(
            find_binary(&["kiro-cli"], &[dir.path().into()]),
            None,
            "not executable"
        );
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(find_binary(&["kiro-cli"], &[dir.path().into()]), Some(bin));
    }
    #[cfg(not(unix))]
    let _ = find_binary;
}

/// A CLI read withheld as a whole prints the `withheld` error document on
/// stdout, like every other error, and the run's one receipt binds exactly
/// those bytes for stdout (not a reason on stderr).
#[test]
fn a_withheld_cli_read_prints_the_withheld_document_on_stdout() {
    use crate::cli::common::carrier_tests_support::{AWS_KEY, governed, project};
    use lean_ctx::core::context_admission::receipt_store::{self, trace};
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    governed();
    let (_project, root) = project();
    let before = receipt_store::latest(&root, 64).len();
    let withheld = answer_released("ctx_agent", &root, "agent.messages", Via::Cli, || {
        let mut keyed = serde_json::Map::new();
        keyed.insert(AWS_KEY.to_owned(), json!("value"));
        Ok(Value::Object(keyed))
    });
    assert_eq!(withheld.failed, Some(Code::Withheld));
    assert_eq!(withheld.body(), Some(withheld.text.as_str()), "written");
    let doc = parse(&withheld.text);
    assert_eq!(doc["kind"], "error");
    assert_eq!(doc["error"]["code"], "withheld");
    assert!(!withheld.text.contains(AWS_KEY), "{}", withheld.text);
    assert_eq!(
        receipt_store::latest(&root, 64).len(),
        before + 1,
        "one receipt"
    );
    let latest = receipt_store::latest(&root, 1);
    let stored = latest[0].1.as_ref().expect("a verified receipt");
    let trace::StoredTrace::Bound { sidecar, .. } = trace::load(&stored.digest) else {
        panic!("a mandatory receipt has its sidecar");
    };
    assert_eq!(sidecar.release_prepared.channel, trace::Channel::Stdout);
    assert_eq!(
        sidecar.release_prepared.bytes,
        receipt_store::output_digest(format!("{}\n", withheld.text).as_bytes())
    );
}

/// G08 for writes: when the release point would withhold the request, the
/// write never runs; the reader gets the `withheld` document under the run's
/// one receipt.
#[test]
fn a_write_the_release_would_withhold_writes_nothing() {
    use crate::cli::common::carrier_tests_support::{AWS_KEY, governed, project};
    use lean_ctx::core::context_admission::receipt_store;
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    governed();
    let (_project, root) = project();
    let before = receipt_store::latest(&root, 64).len();
    let mut keyed = serde_json::Map::new();
    keyed.insert(AWS_KEY.to_owned(), json!("value"));
    let mut wrote = false;
    let answer = answer_released_write(
        "ctx_agent",
        &root,
        "bus.post",
        Via::Cli,
        &Value::Object(keyed),
        || {
            wrote = true;
            Ok(json!({}))
        },
    );
    assert!(!wrote, "the write must not run");
    assert_eq!(answer.failed, Some(Code::Withheld));
    assert_eq!(parse(&answer.text)["error"]["code"], "withheld");
    assert!(!answer.text.contains(AWS_KEY), "{}", answer.text);
    assert_eq!(
        receipt_store::latest(&root, 64).len(),
        before + 1,
        "one receipt"
    );
}

/// G08 for the task and doc writes (CLI and socket alike): a request the
/// release would withhold reaches no store, local or Hive.
#[test]
fn a_withheld_task_or_doc_write_writes_nothing() {
    use crate::cli::common::carrier_tests_support::project;
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    // The project's own policy blocks a pattern: the release withholds the
    // request (a secret would only be masked, and a masked request is
    // released). Scoped to this project, so no parallel test sees it.
    let (_project, root) = project();
    let policy_dir = std::path::Path::new(&root).join(".lean-ctx");
    std::fs::create_dir_all(&policy_dir).expect("policy dir");
    std::fs::write(
        policy_dir.join("policy.toml"),
        "name = \"g08-writes\"\nversion = \"1.0.0\"\ndescription = \"test\"\n\
         [filters.blocked_patterns]\nmarker = 'roadmap-marker'\n",
    )
    .expect("project policy");
    let blocked = "Plan the roadmap-marker release";
    let args = |list: &[&str]| list.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>();
    let task = crate::cli::tasks_cmd::json_answer(
        &args(&[
            "propose",
            "--title",
            "Plan the roadmap",
            "--acceptance",
            blocked,
            "--project-root",
            &root,
            "--json",
        ]),
        &crate::cli::tasks_cmd::Caller::Person,
        Via::Socket,
    );
    assert_eq!(task.failed, Some(Code::Withheld), "{}", task.text);
    assert!(!task.text.contains("roadmap-marker"), "{}", task.text);
    let board = lean_ctx_hive::tasks::local::board(std::path::Path::new(&root)).expect("board");
    assert!(board.tasks.is_empty(), "no task was written");
    for via in [Via::Cli, Via::Socket] {
        let doc = crate::cli::docs_cmd::json_answer(
            &root,
            "write",
            Some("guide"),
            None,
            &args(&["--content", blocked]),
            via,
        );
        assert_eq!(doc.failed, Some(Code::Withheld), "{}", doc.text);
        assert!(!doc.text.contains("roadmap-marker"), "{}", doc.text);
    }
    let docs = lean_ctx_hive::local_docs::store(&root).expect("doc store");
    assert!(!docs.dir().exists(), "no page was written");
}

/// G08 for the plain-text task and doc writes (no `--json`): the same
/// release check as [`a_withheld_task_or_doc_write_writes_nothing`] runs
/// first; a withheld write reaches no store, local or Hive, and says why. A
/// request the release admits is written as before.
#[test]
fn a_withheld_plain_text_task_or_doc_write_writes_nothing() {
    use crate::cli::common::carrier_tests_support::project;
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let (_project, root) = project();
    let policy_dir = std::path::Path::new(&root).join(".lean-ctx");
    std::fs::create_dir_all(&policy_dir).expect("policy dir");
    std::fs::write(
        policy_dir.join("policy.toml"),
        "name = \"g08-text-writes\"\nversion = \"1.0.0\"\ndescription = \"test\"\n\
         [filters.blocked_patterns]\nmarker = 'roadmap-marker'\n",
    )
    .expect("project policy");
    let blocked = "Plan the roadmap-marker release";
    let args = |list: &[&str]| list.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>();
    let person = crate::cli::tasks_cmd::Caller::Person;
    let path = std::path::Path::new(&root);

    // Local task write, the text in a flag and as a positional summary.
    for write in [
        args(&[
            "propose",
            "--title",
            "Plan the roadmap",
            "--acceptance",
            blocked,
        ]),
        args(&["update", "1", blocked, "--agent", "w1"]),
    ] {
        let failure = crate::cli::tasks_cmd::local_text_answer(&write[0], &write, &root, &person)
            .expect_err("withheld");
        assert_eq!(failure.code, Code::Withheld, "{}", failure.message);
        assert!(
            failure.message.contains("nothing was written"),
            "{}",
            failure.message
        );
        assert!(
            !failure.message.contains("roadmap-marker"),
            "{}",
            failure.message
        );
    }
    let board = lean_ctx_hive::tasks::local::board(path).expect("board");
    assert!(board.tasks.is_empty(), "no task was written");

    // The linked path (`hive tasks`) runs the same check before it sends.
    let update = args(&["update", "1", blocked, "--agent", "w1"]);
    let failure = crate::cli::tasks_cmd::release_text_write("update", &update, &root)
        .expect_err("withheld before Hive");
    assert_eq!(failure.code, Code::Withheld);

    // Doc writes: every action ctx_doc writes.
    for action in ["write", "edit", "note"] {
        let call = crate::cli::docs_cmd::text_call(action, Some("guide"), None, Some(blocked));
        let message = crate::cli::docs_cmd::text_answer(&root, &call, &[]).expect_err("withheld");
        assert!(message.contains("nothing was written"), "{message}");
        assert!(!message.contains("roadmap-marker"), "{message}");
    }
    let call = crate::cli::docs_cmd::text_call("move", Some("guide"), Some(blocked), None);
    assert!(crate::cli::docs_cmd::text_answer(&root, &call, &[]).is_err());
    let docs = lean_ctx_hive::local_docs::store(&root).expect("doc store");
    assert!(!docs.dir().exists(), "no page was written");

    // Admitted requests are written as before.
    let ok = args(&[
        "propose",
        "--title",
        "Plan it",
        "--acceptance",
        "50 green runs",
    ]);
    crate::cli::tasks_cmd::local_text_answer("propose", &ok, &root, &person).expect("written");
    let board = lean_ctx_hive::tasks::local::board(path).expect("board");
    assert_eq!(board.tasks.len(), 1, "the admitted task was written");
    let call = crate::cli::docs_cmd::text_call("write", Some("guide"), None, Some("# Guide"));
    crate::cli::docs_cmd::text_answer(&root, &call, &[]).expect("page written");
    assert!(docs.dir().exists(), "the admitted page was written");
}

/// `amend` over the socket: a withheld amendment changes nothing; the
/// submitter's amendment and, once done, one merge record land in the
/// task history.
#[test]
fn an_amend_goes_through_the_release_and_lands_in_the_history() {
    use crate::cli::common::carrier_tests_support::project;
    use crate::cli::tasks_cmd::{Caller, json_answer};
    use lean_ctx_protocol::hive::tasks::{
        EvidenceKind, EvidenceV1, TaskOutcome, TaskState, TaskVerdict,
    };
    use lean_ctx_tasks::{Actor, Proposal, Submission};
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let (_project, root) = project();
    let policy_dir = std::path::Path::new(&root).join(".lean-ctx");
    std::fs::create_dir_all(&policy_dir).expect("policy dir");
    std::fs::write(
        policy_dir.join("policy.toml"),
        "name = \"g08-amend\"\nversion = \"1.0.0\"\ndescription = \"test\"\n\
         [filters.blocked_patterns]\nmarker = 'roadmap-marker'\n",
    )
    .expect("project policy");
    let path = std::path::Path::new(&root);
    let (worker, reviewer) = (Actor::agent("w1"), Actor::agent("rev"));
    let n = lean_ctx_hive::tasks::local::write(path, |board, now| {
        let n = board
            .propose(
                &worker,
                Proposal {
                    title: "Fix the race".into(),
                    goal: None,
                    acceptance: "The fixture is stable".into(),
                    role: None,
                },
                now,
            )?
            .number;
        board.decide(n, &worker, true, None, now)?;
        board.claim(n, &worker, None, now)?;
        board.submit(
            n,
            &worker,
            Submission {
                summary: "Fixed the race.".into(),
                evidence: vec![EvidenceV1 {
                    kind: EvidenceKind::Commit,
                    reference: Some("7be16a9596".into()),
                    anchor: None,
                }],
                usage: None,
                outcome: TaskOutcome::Completed,
            },
            now,
        )?;
        Ok(n)
    })
    .expect("a submitted task");
    let number = n.to_string();
    let amend = |extra: &[&str], caller: &Caller| {
        let mut args = vec!["amend", number.as_str(), "--project-root", &root, "--json"];
        args.extend_from_slice(extra);
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        json_answer(&args, caller, Via::Socket)
    };
    let history = || {
        let board = lean_ctx_hive::tasks::local::board(path).expect("board");
        board.tasks[&n].task.clone()
    };
    let w1 = Caller::Agent("w1".into());
    // Withheld: nothing is written.
    let answer = amend(
        &[
            "--evidence",
            "commit:409c9eec05",
            "--summary",
            "Ported the roadmap-marker fix",
        ],
        &w1,
    );
    assert_eq!(answer.failed, Some(Code::Withheld), "{}", answer.text);
    assert!(!answer.text.contains("roadmap-marker"), "{}", answer.text);
    let task = history();
    assert!(task.updates.is_empty(), "the withheld amend wrote nothing");
    assert_eq!(
        task.result.as_ref().expect("result").evidence[0]
            .reference
            .as_deref(),
        Some("7be16a9596")
    );
    // Another agent is refused.
    let answer = amend(
        &["--evidence", "commit:409c9eec05"],
        &Caller::Agent("w2".into()),
    );
    assert!(answer.failed.is_some(), "{}", answer.text);
    assert!(history().updates.is_empty());
    // The submitter's amendment lands in the history (old → new).
    let answer = amend(
        &["--evidence", "commit:409c9eec05", "--summary", "Ported."],
        &w1,
    );
    assert_eq!(answer.failed, None, "{}", answer.text);
    let task = history();
    let update = task.updates.last().expect("amendment");
    assert_eq!(
        (
            update.amended_from[0].reference.as_deref(),
            update.amended_to[0].reference.as_deref()
        ),
        (Some("7be16a9596"), Some("409c9eec05"))
    );
    // Done: one merge record, never by the reviewer; a person records it.
    lean_ctx_hive::tasks::local::write(path, |board, now| {
        board.verify(n, &reviewer, TaskVerdict::Accept, None, now)
    })
    .expect("done");
    let answer = amend(&["--merge", "6aded74fcf"], &Caller::Agent("rev".into()));
    assert_eq!(answer.failed, Some(Code::Forbidden), "{}", answer.text);
    assert!(answer.text.contains("not_merger"), "{}", answer.text);
    let answer = amend(&["--merge", "6aded74fcf", "--by", "Yves"], &Caller::Person);
    assert_eq!(answer.failed, None, "{}", answer.text);
    let task = history();
    assert_eq!(task.state, TaskState::Done);
    assert_eq!(
        task.result
            .as_ref()
            .and_then(|r| r.merge.as_ref())
            .map(|m| m.commit.as_str()),
        Some("6aded74fcf")
    );
    let recorded = task.updates.len();
    let answer = amend(&["--merge", "1234567abc"], &w1);
    assert!(
        answer.failed.is_some(),
        "a second merge record: {}",
        answer.text
    );
    assert_eq!(history().updates.len(), recorded, "recorded once");
}

/// A retried post with the same `--request-id` returns the stored ids and
/// writes nothing; the same key with another post is refused.
#[test]
fn a_post_with_a_request_id_is_written_once() {
    use lean_ctx::core::agents::AgentRegistry;
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let mut registry = AgentRegistry::default();
    let a = registry
        .register("claude", Some("tester"), "/work/app", None)
        .unwrap();
    let options = |message_key: &'static str| bus::PostOptions {
        project_root: Some("/work/app"),
        request_id: Some(message_key),
        ..bus::PostOptions::default()
    };
    let to = [a.clone()];
    let (first, _) =
        bus::post_with(&mut registry, &to, "Please rebase", &options("req-1")).unwrap();
    let stored = registry.scratchpad.len();
    let (again, _) =
        bus::post_with(&mut registry, &to, "Please rebase", &options("req-1")).unwrap();
    assert_eq!(first, again, "the retry answers with the stored ids");
    assert_eq!(registry.scratchpad.len(), stored, "and writes nothing");
    let other = bus::post_with(&mut registry, &to, "Something else", &options("req-1"));
    assert!(
        matches!(other, Err(bus::PostError::Conflict(_))),
        "{other:?}"
    );
    assert_eq!(registry.scratchpad.len(), stored);
}

/// QN0: a Queen kind comes only from the Queen session the app registered
/// (`not_queen` otherwise, also for the person); `--as` names an active
/// agent of this Instance; rows carry the kind, and an ordinary or older
/// row reads `kind: null` (additive within agent-bus/1).
#[test]
fn a_queen_kind_comes_only_from_the_registered_queen_and_its_row_names_it() {
    use lean_ctx::core::agents::AgentRegistry;
    use lean_ctx_hive::bus::queen_kinds::{KIND_KEY, QueenKind};
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let mut registry = AgentRegistry::default();
    let root = "/work/app";
    let queen = registry
        .register("claude", Some("queen"), root, None)
        .unwrap();
    // Another agent of this Instance (its own process; `register` in this
    // one would return the Queen's entry).
    let mut entry = registry.agents[0].clone();
    entry.agent_id = "mcp-2-developer".into();
    entry.role = Some("developer".into());
    entry.app_session_id = Some("app-session-dev-2".into());
    registry.agents.push(entry.clone());
    // A role is a label: an agent that calls itself `queen` is no Queen.
    entry.agent_id = "mcp-3-pretender".into();
    entry.role = Some("queen".into());
    entry.app_session_id = Some("app-session-pretender-3".into());
    registry.agents.push(entry);
    let worker = "mcp-2-developer".to_string();
    lean_ctx_hive::queens::register_in(
        &lean_ctx_hive::queens::data_dir().unwrap(),
        "app-session-queen-1",
        Some("Queen"),
        "2026-11-09T10:00:00Z",
    )
    .unwrap();
    registry
        .agents
        .iter_mut()
        .find(|a| a.agent_id == queen)
        .unwrap()
        .app_session_id = Some("app-session-queen-1".into());
    // Each sender posts from its own app session.
    let session_of = |sender: &str| match sender {
        "mcp-2-developer" => Some("app-session-dev-2"),
        "mcp-3-pretender" => Some("app-session-pretender-3"),
        bus::PERSON => None,
        _ => Some("app-session-queen-1"),
    };
    let options = |sender: &'static str, kind| bus::PostOptions {
        project_root: Some(root),
        category: "status",
        sender,
        kind,
        task_id: Some("task-7"),
        caller_session: session_of(sender),
        ..bus::PostOptions::default()
    };
    let worker: &'static str = Box::leak(worker.into_boxed_str());
    let queen: &'static str = Box::leak(queen.into_boxed_str());
    for sender in [worker, "mcp-3-pretender", bus::PERSON] {
        let refused = bus::post_with(
            &mut registry,
            &[],
            "take the parser",
            &options(sender, Some(QueenKind::Handoff)),
        );
        assert!(
            matches!(&refused, Err(bus::PostError::Sender(r)) if r.starts_with("not_queen:")),
            "{refused:?}"
        );
    }
    // `--as` the Queen from a worker's session: refused, whatever the kind.
    for kind in [Some(QueenKind::Stop), None] {
        let forged = bus::post_with(
            &mut registry,
            &[],
            "stop everything",
            &bus::PostOptions {
                caller_session: session_of(worker),
                ..options(queen, kind)
            },
        );
        assert!(
            matches!(&forged, Err(bus::PostError::Sender(r)) if r.contains("own session")),
            "{forged:?}"
        );
    }
    let unsessioned = bus::post_with(
        &mut registry,
        &[],
        "hi",
        &bus::PostOptions {
            caller_session: None,
            ..options(worker, None)
        },
    );
    assert!(
        matches!(unsessioned, Err(bus::PostError::Sender(_))),
        "{unsessioned:?}"
    );
    let ghost = bus::post_with(&mut registry, &[], "hi", &options("mcp-9-ghost", None));
    assert!(matches!(ghost, Err(bus::PostError::Sender(_))), "{ghost:?}");
    assert!(registry.scratchpad.is_empty(), "a refusal writes nothing");

    // `queen` reaches the registered Queen, not the agent with the role name.
    let (to_queen, _) = bus::post_with(
        &mut registry,
        &["queen".to_owned()],
        "E4 merged, review?",
        &options(worker, None),
    )
    .unwrap();
    let entry = registry
        .scratchpad
        .iter()
        .find(|e| e.id == to_queen[0])
        .unwrap();
    assert_eq!(entry.to_agent.as_deref(), Some(queen));
    registry.scratchpad.clear();

    let (ids, _) = bus::post_with(
        &mut registry,
        &[],
        "who takes E14?",
        &options(queen, Some(QueenKind::Coordinate)),
    )
    .unwrap();
    let (plain, _) =
        bus::post_with(&mut registry, &[], "E4 merged", &options(worker, None)).unwrap();
    let row_of = |registry: &AgentRegistry, id: &str| {
        let entry = registry.scratchpad.iter().find(|e| e.id == id).unwrap();
        serde_json::to_value(bus::row(entry, Some(root))).unwrap()
    };
    let row = row_of(&registry, &ids[0]);
    assert_eq!(
        (&row["kind"], &row["task_id"], &row["from"]),
        (&json!("coordinate"), &json!("task-7"), &json!(queen))
    );
    let row = row_of(&registry, &plain[0]);
    assert_eq!(row["kind"], Value::Null, "an ordinary message: kind null");
    assert_eq!(row["from"], json!(worker));
    // A row stored before QN0, or with a kind no reader knows, reads null.
    registry.scratchpad[1]
        .metadata
        .insert(KIND_KEY.into(), "summon".into());
    assert_eq!(row_of(&registry, &plain[0])["kind"], Value::Null);
}

/// QN0: a sender posts at most 20 messages a minute (`rate_limited`,
/// served as `unavailable`); another sender keeps its own budget.
#[test]
fn a_sender_over_its_minute_budget_is_refused_with_rate_limited() {
    use lean_ctx::core::agents::AgentRegistry;
    use lean_ctx_hive::bus::queen_kinds::RATE_PER_MINUTE;
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let mut registry = AgentRegistry::default();
    let root = "/work/app";
    let worker = registry
        .register("claude", Some("developer"), root, None)
        .unwrap();
    registry.agents[0].app_session_id = Some("app-session-dev-1".into());
    let options = |sender| bus::PostOptions {
        project_root: Some(root),
        sender,
        caller_session: (sender != bus::PERSON).then_some("app-session-dev-1"),
        ..bus::PostOptions::default()
    };
    for i in 0..RATE_PER_MINUTE {
        bus::post_with(&mut registry, &[], &format!("step {i}"), &options(&worker)).unwrap();
    }
    let refused = bus::post_with(&mut registry, &[], "one more", &options(&worker));
    assert!(
        matches!(&refused, Err(bus::PostError::RateLimited(r)) if r.starts_with("rate_limited:")),
        "{refused:?}"
    );
    bus::post_with(&mut registry, &[], "the person", &options(bus::PERSON)).unwrap();
}

/// QN0 flags on `agent post` / `bus post`: an unknown kind is refused with
/// its reason, `high` only for conflict, escalate and stop.
#[test]
fn post_flags_refuse_an_unknown_kind_and_high_for_an_ordinary_kind() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_string_lossy().into_owned();
    let args = |extra: &[&str]| {
        let mut a: Vec<String> = ["--broadcast", "--message", "hi", "--project-root", &root]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        a.extend(extra.iter().map(|s| (*s).to_owned()));
        a
    };
    let refused = |extra: &[&str]| match bus::build_bus_post(&args(extra)) {
        Err(f) => f.message,
        Ok(_) => panic!("{extra:?} was accepted"),
    };
    let unknown = refused(&["--kind", "summon"]);
    assert!(
        unknown.contains("unknown kind") && unknown.contains("coordinate"),
        "{unknown}"
    );
    let high = refused(&["--kind", "status", "--priority", "high"]);
    assert!(
        high.contains("only for kinds conflict, escalate and stop"),
        "{high}"
    );
    let task = refused(&["--task-id", "a b"]);
    assert!(task.contains("task_id"), "{task}");
    // The answer's `kind` names the document; the message's kind is `message_kind`.
    let posted = bus::build_bus_post(&args(&["--task-id", "task-7"])).unwrap();
    let doc: Value = serde_json::from_str(&super::document("bus.post", &posted)).unwrap();
    assert_eq!(
        (&doc["kind"], &doc["message_kind"], &doc["task_id"]),
        (&json!("bus.post"), &Value::Null, &json!("task-7"))
    );
}

/// #1298: one fact whose key is a session id (the generic-long-secret rule
/// masks it under `"key"`, but its `id` still carries it) withheld the whole
/// `knowledge.list`. Now only that fact is withheld; the clean facts are
/// released and `withheld` counts the one left out.
#[test]
fn knowledge_list_withholds_only_the_fact_that_trips_the_release() {
    use crate::cli::common::carrier_tests_support::{governed, project};
    use lean_ctx::core::knowledge::ProjectKnowledge;
    use lean_ctx::core::memory_policy::MemoryPolicy;
    const SESSION_KEY: &str = "wake-mcp-9fK2xQ7vT4bL8nR3cW6yH1mJ5pD0sGz";
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    governed();
    let (_project, root) = project();
    let mut k = ProjectKnowledge::new(&root);
    let policy = MemoryPolicy::default();
    k.remember(
        "decision",
        "db",
        "Postgres for the ledger",
        "s1",
        0.9,
        &policy,
    );
    k.remember("architecture", "auth", "JWT RS256", "s1", 0.8, &policy);
    let mut tripping = k.facts[0].clone();
    tripping.category = "session".into();
    tripping.key = SESSION_KEY.into();
    tripping.value = "Queen wake record".into();
    k.facts.push(tripping);

    let query = knowledge::Query {
        limit: 10,
        ..Default::default()
    };
    for via in [Via::Cli, Via::Socket] {
        let answer = answer_released_records(
            "ctx_knowledge",
            &root,
            "knowledge.list",
            via,
            knowledge::RECORDS,
            || {
                let (rows, noise) = knowledge::local_rows(&k.facts, false);
                Ok(knowledge::select(&root, rows, &query, noise))
            },
        );
        assert_eq!(answer.failed, None, "{via:?}: {}", answer.text);
        assert!(!answer.text.contains(SESSION_KEY), "{}", answer.text);
        let doc = parse(&answer.text);
        assert_eq!(doc["kind"], "knowledge.list");
        let mut ids: Vec<&str> = doc["facts"]
            .as_array()
            .expect("facts")
            .iter()
            .filter_map(|fact| fact["id"].as_str())
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, ["architecture/auth", "decision/db"], "{via:?}");
        assert_eq!(doc["withheld"], 1, "{via:?}");
        assert_eq!(doc["returned"], 2, "{via:?}");
        assert_eq!(doc["total"], 3, "{via:?}");
    }

    // A clean store keeps its bytes and reports nothing withheld.
    k.facts.pop();
    let clean = answer_released_records(
        "ctx_knowledge",
        &root,
        "knowledge.list",
        Via::Cli,
        knowledge::RECORDS,
        || {
            let (rows, noise) = knowledge::local_rows(&k.facts, false);
            Ok(knowledge::select(&root, rows, &query, noise))
        },
    );
    assert_eq!(clean.notice, None, "{}", clean.text);
    assert_eq!(parse(&clean.text)["withheld"], 0);
}

/// #1298 review: every fact offends → an empty `facts`, `withheld` counts
/// all of them and `returned` is 0; the notice is content-free. A clean fact
/// that names what a withheld fact carried is withheld as well, so the
/// value reaches the reader through no other fact.
#[test]
fn knowledge_list_counts_every_withheld_fact_and_hides_what_they_carried() {
    use crate::cli::common::carrier_tests_support::{governed, project};
    use lean_ctx::core::knowledge::{KnowledgeFact, ProjectKnowledge};
    use lean_ctx::core::memory_policy::MemoryPolicy;
    const SESSION_KEY: &str = "wake-mcp-9fK2xQ7vT4bL8nR3cW6yH1mJ5pD0sGz";
    const OTHER_KEY: &str = "wake-mcp-3Hq8Lz1Rk6Vt0Yw4Bn9Cx2Mf7Jd5Ps";
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    governed();
    let (_project, root) = project();
    let mut k = ProjectKnowledge::new(&root);
    k.remember(
        "decision",
        "db",
        "Postgres for the ledger",
        "s1",
        0.9,
        &MemoryPolicy::default(),
    );
    let clean = k.facts[0].clone();
    let tripping = |key: &str| {
        let mut fact = clean.clone();
        fact.category = "session".into();
        fact.key = key.into();
        fact.value = "Queen wake record".into();
        fact
    };
    let query = knowledge::Query {
        limit: 10,
        ..Default::default()
    };
    let list = |facts: &[KnowledgeFact]| {
        answer_released_records(
            "ctx_knowledge",
            &root,
            "knowledge.list",
            Via::Cli,
            knowledge::RECORDS,
            || {
                let (rows, noise) = knowledge::local_rows(facts, false);
                Ok(knowledge::select(&root, rows, &query, noise))
            },
        )
    };

    // Every fact trips the release: the list is delivered, empty.
    let answer = list(&[tripping(SESSION_KEY), tripping(OTHER_KEY)]);
    assert_eq!(answer.failed, None, "{}", answer.text);
    assert!(!answer.text.contains(SESSION_KEY), "{}", answer.text);
    assert!(!answer.text.contains(OTHER_KEY), "{}", answer.text);
    let doc = parse(&answer.text);
    assert_eq!(doc["facts"], json!([]), "{}", answer.text);
    assert_eq!(doc["withheld"], 2);
    assert_eq!(doc["withheld"], doc["total"]);
    assert_eq!(doc["returned"], 0);
    // The answer's notice is content-free; the count is in `withheld` (the
    // CLI's stderr line naming it is pinned in `common_tests`).
    assert_eq!(answer.notice.as_deref(), Some(MASKED_NOTICE));

    // A clean fact naming the withheld fact's key is left out too.
    let mut naming = clean.clone();
    naming.key = "follow-up".into();
    naming.value = format!("resumes {SESSION_KEY}");
    let answer = list(&[clean.clone(), tripping(SESSION_KEY), naming]);
    assert_eq!(answer.failed, None, "{}", answer.text);
    assert!(!answer.text.contains(SESSION_KEY), "{}", answer.text);
    let doc = parse(&answer.text);
    let ids: Vec<&str> = doc["facts"]
        .as_array()
        .expect("facts")
        .iter()
        .filter_map(|fact| fact["id"].as_str())
        .collect();
    assert_eq!(ids, ["decision/db"], "{}", answer.text);
    assert_eq!(doc["withheld"], 2);
    assert_eq!(doc["returned"], 1);
    assert_eq!(doc["total"], 3);
    assert_eq!(answer.notice.as_deref(), Some(MASKED_NOTICE));
}

/// QN0: a socket post is always the person's, who has no Queen kind and
/// speaks for no agent: the post routes refuse `kind` and `as` as unknown
/// parameters instead of filing a post that can only be refused.
#[test]
fn socket_posts_take_neither_a_kind_nor_another_sender() {
    for path in ["/v1/local/messages", "/v1/local/bus/post"] {
        let route = routes::all()
            .find(|r| r.path == path && r.method == routes::Method::Post)
            .expect("the post route");
        for key in ["kind", "as"] {
            let refused = routes::args_of(
                route,
                None,
                &[(key.into(), json!("stop")), ("message".into(), json!("x"))],
            )
            .expect_err("refused");
            assert_eq!(refused.code, Code::InvalidArgument, "{path} {key}");
            assert_eq!(refused.message, format!("unknown parameter {key}"));
        }
    }
}

/// `bus list --all` / `agent messages --all` (the person's console, Hive
/// Desktop's read of every project) used to run under no project (`""`),
/// whose policy resolves to deny-all: the whole feed came back
/// `withheld` (`release.unadmitted_content`). Each project's messages are
/// now released as that project's own reader is, message by message: the
/// feed spans every project root and only the message that trips the
/// release is left out, counted in `withheld`.
#[test]
fn bus_list_all_releases_every_project_and_withholds_only_the_tripping_message() {
    use crate::cli::common::carrier_tests_support::{AWS_KEY, project};
    use lean_ctx::core::agents::AgentRegistry;
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    // The live policy resolution, not a fixed override: the failure was the
    // policy of the empty project root.
    let _live = lean_ctx::core::policy::runtime::lock_test_override();
    // Governed, and a secret is withheld rather than masked.
    let config = lean_ctx::core::paths::config_dir_read_only().expect("isolated config dir");
    std::fs::create_dir_all(&config).expect("config dir");
    std::fs::write(
        config.join("config.toml"),
        "[context_gateway]\nmode = \"governed\"\nsecrets = \"block\"\n",
    )
    .expect("config");
    let (_a, root_a) = project();
    let (_b, root_b) = project();
    let mut registry = AgentRegistry::default();
    let (from_a, _) = bus::post(&mut registry, &[], "hello from a", None, Some(&root_a)).unwrap();
    // b is strictly the newest message.
    std::thread::sleep(std::time::Duration::from_millis(20));
    let (from_b, _) = bus::post(&mut registry, &[], "hello from b", None, Some(&root_b)).unwrap();
    let (a_id, b_id) = (from_a[0].clone(), from_b[0].clone());
    let mut tripping = registry
        .scratchpad
        .iter()
        .find(|e| e.id == b_id)
        .cloned()
        .expect("b entry");
    tripping.id = "tripping-message".into();
    tripping.timestamp -= chrono::Duration::seconds(60);
    tripping
        .metadata
        .insert(bus::THREAD_KEY.into(), AWS_KEY.into());
    registry.scratchpad.push(tripping);
    registry.save().expect("registry saved");

    let args = |limit: &str| -> Vec<String> {
        [
            "--json",
            "--all",
            "--limit",
            limit,
            "--project-root",
            &root_a,
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
    };
    let ids = |doc: &Value| -> Vec<String> {
        doc["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .filter_map(|m| m["id"].as_str().map(str::to_owned))
            .collect()
    };
    for via in [Via::Cli, Via::Socket] {
        let answer = bus::bus_answer("list", &args("10"), via);
        assert_eq!(answer.failed, None, "{via:?}: {}", answer.text);
        assert!(!answer.text.contains(AWS_KEY), "{}", answer.text);
        let doc = parse(&answer.text);
        assert_eq!(doc["kind"], "bus.list", "{via:?}");
        assert_eq!(ids(&doc), [a_id.clone(), b_id.clone()], "{via:?}");
        assert_eq!(doc["withheld"], 1, "{via:?}");
    }

    // --limit 1: the newest message, from a foreign project root.
    let newest = bus::bus_answer("list", &args("1"), Via::Cli);
    assert_eq!(newest.failed, None, "{}", newest.text);
    let doc = parse(&newest.text);
    assert_eq!(ids(&doc), [b_id.clone()]);
    assert_eq!(doc["messages"][0]["project_root"], root_b.as_str());
    assert_eq!(doc["messages"][0]["message"], "hello from b");
    assert!(doc.get("withheld").is_none(), "{}", newest.text);

    let messages = bus::messages(&args("10"), Via::Cli);
    assert_eq!(messages.failed, None, "{}", messages.text);
    assert!(!messages.text.contains(AWS_KEY), "{}", messages.text);
    let doc = parse(&messages.text);
    assert_eq!(doc["kind"], "agent.messages");
    assert_eq!(ids(&doc), [a_id, b_id]);
    assert_eq!(doc["withheld"], 1);
}

/// A route without a project (`project: false`) and an `all` event stream
/// name no project on the socket. Their documents are released under the
/// folder-less authority, not under the empty root: the policy resolver
/// refuses an empty or missing root (deny-all), which withheld every such
/// document as `release.unadmitted_content` — `/v1/local/gain/daily`,
/// `/v1/local/agents`, `/v1/local/daemon` and the other project-less reads.
#[test]
fn project_less_socket_documents_are_released_not_withheld() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let args = |list: &[&str]| list.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>();
    for (kind, list) in [
        ("gain.daily", &["--days", "7"][..]),
        ("gain.projects", &[][..]),
    ] {
        let answer = kinds::answer(kind, &args(list), Via::Socket);
        assert_eq!(answer.failed, None, "{kind}: {}", answer.text);
        assert!(answer.released, "{kind}");
        assert_eq!(parse(&answer.text)["kind"], kind, "{}", answer.text);
    }
    // An event of an `all` stream is released the same way.
    let event = answer_released("ctx_agent", "", "event.bus_message", Via::Socket, || {
        Ok(json!({ "route": "/v1/local/bus" }))
    });
    assert_eq!(event.failed, None, "{}", event.text);
    assert_eq!(
        parse(&event.text)["route"],
        "/v1/local/bus",
        "{}",
        event.text
    );
    // A named project that is no directory still fails closed.
    let missing = answer_released(
        "ctx_agent",
        "/nonexistent-lean-ctx-project-root",
        "event.bus_message",
        Via::Socket,
        || Ok(json!({ "route": "/v1/local/bus" })),
    );
    assert_eq!(missing.failed, Some(Code::Withheld), "{}", missing.text);
}
