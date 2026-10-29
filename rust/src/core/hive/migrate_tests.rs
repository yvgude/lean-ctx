// SPDX-License-Identifier: Apache-2.0
//! T-UPGRADE (SPEC-ENGINE §6.2, V13.E1): a 3.11.5 and a 3.12.0-rc data
//! directory reach the 3.12 layout with no data lost, no stale facts and no
//! dangling relations.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::json;

use super::*;
use crate::core::hive::knowledge::draft::{QueuedDraft, Watermark, session_ref};
use crate::core::hive::replica::log::StoredEvent;
use crate::core::knowledge::KnowledgeFact;
use crate::core::knowledge_relations::KnowledgeEdgeKind;
use lean_ctx_protocol::hive::knowledge::{
    ContextPatchV1, DRAFT_PROPOSAL_SCHEMA, DraftProposalV1, FactV1, ProposalState,
    ResearchContextDocumentV1,
};

const VAULT: &str = "vlt_pay";
const SALT: [u8; 32] = [7; 32];
/// Pulled claims: the first two have a node in the replica, the third not.
const PULLED: [&str; 3] = [
    "The billing service retries webhooks three times.",
    "Refunds settle through the ledger queue only.",
    "Pay copies this claim nobody migrated.",
];

struct Fixture {
    _dirs: tempfile::TempDir,
    store: HiveStore,
    root: PathBuf,
    stable: String,
}

fn git(root: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

fn root_str(f: &Fixture) -> String {
    f.root.to_string_lossy().into_owned()
}

/// A connected 3.11.5 engine: own and pulled knowledge, local relations,
/// a queued session draft of the old shape, a watermark under `drafts/`,
/// v1 relay cursors and the agent-keyed presence map.
fn data_dir_3115() -> Fixture {
    let dirs = tempfile::tempdir().unwrap();
    let store = HiveStore::at(dirs.path().join("config"), dirs.path().join("data"));
    crate::core::hive::bus::control::enroll_for_tests(&store, 1);
    let root = dirs.path().join("pay");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/relay.rs"), "pub fn relay() {}\n").unwrap();
    git(&root, &["init", "-q"]);
    git(
        &root,
        &["remote", "add", "origin", "https://github.com/acme/pay.git"],
    );
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "init"]);
    let root = root.canonicalize().unwrap();
    let stable = lean_ctx_protocol::hive::vault::stable_identity(&project_identity(&root).unwrap());
    let f = Fixture {
        _dirs: dirs,
        store,
        root,
        stable,
    };
    let data = f.store.data_dir();
    let mut map = VaultMap::default();
    map.vaults.insert(
        f.stable.clone(),
        VaultRecord {
            vault_id: VAULT.into(),
            ns: "acme".into(),
            name: "pay".into(),
            sealed: false,
            roots: vec![root_str(&f)],
        },
    );
    map.save(data).unwrap();

    // Own knowledge: one claim that passes the gate, one that does not.
    let policy = crate::core::memory_policy::MemoryPolicy::default();
    crate::core::knowledge::ProjectKnowledge::mutate_locked(&root_str(&f), |k| {
        k.remember(
            "architecture",
            "relay-frames",
            "The relay drops frames when the link is slower than 1 Mbit/s.",
            "s1",
            0.9,
            &policy,
        );
        k.facts.last_mut().unwrap().capture.refs = vec!["file:src/relay.rs".into()];
        k.remember(
            "architecture",
            "unreferenced",
            "The relay keeps one socket per peer for its lifetime.",
            "s1",
            0.8,
            &policy,
        );
    })
    .unwrap();
    // What 1.x pulls copied in from the vault.
    let doc = ResearchContextDocumentV1 {
        facts: PULLED
            .iter()
            .enumerate()
            .map(|(i, statement)| FactV1 {
                id: format!("lc-pulled-{i}"),
                statement: (*statement).into(),
                kind: if i == 1 { "decision" } else { "observation" }.into(),
                applies_to: vec![],
                sources: vec![],
                from_contributions: vec![],
                anchors: vec![],
            })
            .collect(),
        ..ResearchContextDocumentV1::default()
    };
    learn::sync_into_knowledge(&root_str(&f), "acme", "pay", 4, &doc).unwrap();

    // Local relations: own → pulled (twice), pulled → own, own → own.
    let knowledge = ProjectKnowledge::load(&root_str(&f)).unwrap();
    let mut graph = KnowledgeRelationGraph::load_or_create(&knowledge.project_hash);
    let own = KnowledgeNodeRef::new("architecture", "relay-frames");
    let other = KnowledgeNodeRef::new("architecture", "unreferenced");
    let pulled = |i: usize| pulled_ref(&knowledge, PULLED[i]);
    graph.upsert_edge(own.clone(), pulled(0), KnowledgeEdgeKind::Supports, "s1");
    graph.upsert_edge(own.clone(), pulled(2), KnowledgeEdgeKind::RelatedTo, "s1");
    graph.upsert_edge(pulled(1), other.clone(), KnowledgeEdgeKind::DependsOn, "s1");
    graph.upsert_edge(own, other, KnowledgeEdgeKind::RelatedTo, "s1");
    graph.save().unwrap();

    // A session draft of the old shape waits in the outbox.
    let old = QueuedDraft {
        stable_identity: f.stable.clone(),
        root: root_str(&f),
        draft: DraftProposalV1 {
            schema: DRAFT_PROPOSAL_SCHEMA.into(),
            state: ProposalState::Draft,
            summary: Some("Session learnings: 2 facts".into()),
            body: Some("Learned in a lean-ctx session:\n- error[E0308]".into()),
            sources: Vec::new(),
            context_patch: ContextPatchV1 {
                base_revision: 0,
                ops: Vec::new(),
                sealed: None,
            },
            session_ref: session_ref("old"),
            engine_screening: Screen::for_installation().counts(),
            origin: None,
        },
    };
    Outbox::at(data)
        .enqueue(
            CHANNEL_KNOWLEDGE,
            serde_json::to_value(old).unwrap(),
            Utc::now(),
        )
        .unwrap();

    let mark = serde_json::to_vec(&Watermark {
        since: Some(Utc::now()),
        last_draft_at: Some(Utc::now()),
    })
    .unwrap();
    write_private(
        &data.join("drafts"),
        &format!("{}.watermark", f.stable),
        &mark,
    )
    .unwrap();
    write_private(
        &data.join("drafts"),
        "unbound0.watermark",
        b"{\"since\":null}",
    )
    .unwrap();
    write_private(&data.join("drafts"), "unbound0.import-watermark", b"{}").unwrap();
    write_private(
        &data.join("bus"),
        "state.json",
        br#"{"cursors":{"general":12,"acme-pay":3},"forwarded":["m1"]}"#,
    )
    .unwrap();
    write_private(&data.join("bus"), "presence.json", br#"{"agents":{}}"#).unwrap();
    f
}

fn pulled_ref(k: &ProjectKnowledge, statement: &str) -> KnowledgeNodeRef {
    let f = k
        .facts
        .iter()
        .find(|f| f.imported_from.is_some() && f.value.contains(statement))
        .unwrap();
    KnowledgeNodeRef::new(&f.category, &f.key)
}

/// The clone a C0 2.0 Hive delivers: the first two pulled claims as nodes.
fn clone_replica(f: &Fixture) -> Replica {
    let (mk, _) =
        crate::core::hive::replica::crypt::master_key(f.store.config_dir(), false).unwrap();
    let (mut replica, _) = Replica::open(f.store.data_dir(), VAULT, &mk).unwrap();
    replica.set_id_salt(&SALT).unwrap();
    let add = |seq: u64, node: &str, node_type: &str, statement: &str| StoredEvent {
        seq,
        event: json!({
            "event_id": format!("E{seq:025}"),
            "op": "node.add",
            "target": node,
            "node_type": node_type,
            "hlc": {"wall_ms": 1000 + seq, "counter": 0, "node": "x"},
            "worker_id": "wkr_aaaaaaaaaaaaaaaaaaaa",
            "signature": "00",
            "seq": seq,
            "body": {"statement": statement},
        }),
        sig_invalid: false,
    };
    replica
        .insert_events(&[
            add(1, "n_webhooks", "fact", PULLED[0]),
            add(2, "n_refunds", "decision", PULLED[1]),
            add(3, "n_other", "fact", "A claim no checkout ever pulled."),
        ])
        .unwrap();
    replica.refold().unwrap();
    replica
}

fn own_facts(f: &Fixture) -> Vec<(String, String, String)> {
    let mut v: Vec<_> = ProjectKnowledge::load(&root_str(f))
        .unwrap()
        .facts
        .iter()
        .filter(|x: &&KnowledgeFact| x.imported_from.is_none())
        .map(|x| (x.category.clone(), x.key.clone(), x.value.clone()))
        .collect();
    v.sort();
    v
}

/// Every relation endpoint names a current fact or a live replica node.
fn assert_no_dangling(f: &Fixture, replica: &Replica) {
    let k = ProjectKnowledge::load(&root_str(f)).unwrap();
    let graph = KnowledgeRelationGraph::load(&k.project_hash).unwrap();
    let mut folded = Graph::default();
    folded.fold(&replica.fold_events().unwrap()).unwrap();
    for edge in &graph.edges {
        for end in [&edge.from, &edge.to] {
            let fact = k
                .facts
                .iter()
                .any(|x| x.is_current() && x.category == end.category && x.key == end.key);
            let node = end.category == format!("{REPLICA_NODE_CATEGORY}{VAULT}")
                && folded.live_nodes().any(|(id, _)| *id == end.key);
            assert!(fact || node, "dangling relation endpoint {end:?}");
        }
    }
}

fn pulled_left(f: &Fixture) -> usize {
    ProjectKnowledge::load(&root_str(f))
        .unwrap()
        .facts
        .iter()
        .filter(|x| x.imported_from.is_some())
        .count()
}

/// The whole upgrade of a data directory, as the scheduler runs it: the
/// local phase on the first start, the 2.0 phase once Hive speaks 2.0.
fn upgrade(f: &Fixture) -> (UpgradeReport, Replica) {
    let config = HiveConfig::default();
    let before = own_facts(f);

    // First 3.12 start against a 1.x Hive (prod before the cutover).
    let local = run(&f.store, &config).unwrap();
    assert_eq!(local.drafts_dropped, 1, "{local:?}");
    assert_eq!(local.drafts_requeued, 1, "{local:?}");
    assert_eq!(local.watermarks_moved, 2);
    assert_eq!(pulled_left(f), 3, "offline engines keep pulled context");
    let data = f.store.data_dir();
    assert!(data.join(format!("capture/{VAULT}.watermark")).is_file());
    assert_eq!(
        std::fs::read(data.join("capture/unbound0.watermark")).unwrap(),
        b"{\"since\":null}"
    );
    assert!(!data.join(format!("drafts/{}.watermark", f.stable)).exists());
    assert!(data.join("drafts/unbound0.import-watermark").is_file());
    // The old draft is gone; what was queued instead passed the gate.
    let queued: Vec<_> = Outbox::at(data)
        .list(CHANNEL_KNOWLEDGE)
        .into_iter()
        .filter_map(|i| draft::queued(&i.payload))
        .collect();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].draft.summary.as_deref(), Some("Captured 1 fact"));

    // The Hive speaks 2.0 and the vault is cloned.
    let replica = clone_replica(f);
    super::super::bus::relay_v2::record_protocol(&f.store, true).unwrap();
    let report = run(&f.store, &config).unwrap();
    assert_eq!(report.facts_removed, 3);
    assert_eq!(report.relations_rewritten, 2, "{report:?}");
    assert_eq!(report.relations_dropped, 1, "{report:?}");
    assert_eq!(report.relay_cursors_reset, 2);
    assert!(report.presence_discarded);
    assert_eq!(pulled_left(f), 0, "no stale facts");
    assert_eq!(own_facts(f), before, "no own data lost");
    assert_no_dangling(f, &replica);

    let k = ProjectKnowledge::load(&root_str(f)).unwrap();
    let graph = KnowledgeRelationGraph::load(&k.project_hash).unwrap();
    let own = KnowledgeNodeRef::new("architecture", "relay-frames");
    let has = |from: &KnowledgeNodeRef, to: &KnowledgeNodeRef, kind| {
        graph
            .edges
            .iter()
            .any(|e| &e.from == from && &e.to == to && e.kind == kind)
    };
    assert!(has(
        &own,
        &replica_node_ref(VAULT, "n_webhooks"),
        KnowledgeEdgeKind::Supports
    ));
    assert!(has(
        &replica_node_ref(VAULT, "n_refunds"),
        &KnowledgeNodeRef::new("architecture", "unreferenced"),
        KnowledgeEdgeKind::DependsOn
    ));
    assert_eq!(
        graph.edges.len(),
        3,
        "own → own stays, the unmatched one went"
    );
    assert!(
        super::super::bus::relay::RelayState::load(data)
            .cursors
            .is_empty()
    );
    assert_eq!(
        super::super::bus::relay::RelayState::load(data).forwarded,
        ["m1"],
        "only the cursors start over"
    );
    assert!(!data.join("bus/presence.json").exists());

    // Printed once, then quiet; running again changes nothing.
    let lines = status_lines_once(&f.store);
    let says = |text: &str| lines.iter().any(|l| l.contains(text));
    assert!(says("1 unsent session draft(s) dropped"), "{lines:?}");
    assert!(
        says("2 relation(s) moved to replica nodes, 1 dropped"),
        "{lines:?}"
    );
    assert!(status_lines_once(&f.store).is_empty());
    assert_eq!(run(&f.store, &config).unwrap(), report);
    assert!(status_lines_once(&f.store).is_empty());
    (report, replica)
}

#[test]
fn t_upgrade_from_a_3_11_5_data_dir() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let f = data_dir_3115();
    let (report, _) = upgrade(&f);
    assert_eq!(report.replicas_reset, 0);
}

#[test]
fn t_upgrade_from_a_3_12_0_rc_data_dir() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let f = data_dir_3115();
    // rc-era replica files without `user_version = 1`: one that never got
    // a schema version, one written by a newer pre-release, a directory with
    // only a downloaded segment.
    let replicas = f.store.data_dir().join("replica");
    for (place, version) in [("pl_rc0", 0), (VAULT, 7)] {
        std::fs::create_dir_all(replicas.join(place)).unwrap();
        let conn = rusqlite::Connection::open(replicas.join(place).join("replica.db")).unwrap();
        conn.execute_batch("CREATE TABLE events (seq INTEGER PRIMARY KEY);")
            .unwrap();
        conn.pragma_update(None, "user_version", version).unwrap();
    }
    std::fs::create_dir_all(replicas.join("pl_seg/snapshot")).unwrap();
    std::fs::write(replicas.join("pl_seg/snapshot/1.seg"), b"x").unwrap();
    // A replica this engine wrote stays.
    let (mk, _) =
        crate::core::hive::replica::crypt::master_key(f.store.config_dir(), false).unwrap();
    drop(Replica::open(f.store.data_dir(), "pl_kept", &mk).unwrap());

    let (report, _) = upgrade(&f);
    assert_eq!(report.replicas_reset, 3);
    assert!(!replicas.join("pl_rc0").exists());
    assert!(!replicas.join("pl_seg").exists());
    assert!(replicas.join("pl_kept/replica.db").is_file());
    // The vault's stale rc replica was deleted, then cloned again.
    assert!(replicas.join(VAULT).join("replica.db").is_file());
}

/// A first clone (V4.10 path in `sync_vault`) rewrites the relations too
/// and adds its counts to the report `hive status` prints.
#[test]
fn a_first_clone_moves_relations_to_replica_nodes() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let f = data_dir_3115();
    let replica = clone_replica(&f);
    let record = VaultMap::load(f.store.data_dir()).vaults[&f.stable].clone();
    assert_eq!(
        retire_on_clone(&f.store, &replica, &record, &f.root).unwrap(),
        3
    );
    assert_no_dangling(&f, &replica);
    let lines = status_lines_once(&f.store);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("2 relation(s) moved to replica nodes, 1 dropped")),
        "{lines:?}"
    );
    assert_eq!(
        retire_on_clone(&f.store, &replica, &record, &f.root).unwrap(),
        0
    );
}
