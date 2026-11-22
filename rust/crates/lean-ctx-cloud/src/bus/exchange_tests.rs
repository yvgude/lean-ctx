// SPDX-License-Identifier: Apache-2.0
//! Agent exchange rules and audit (§40) against real floors, screens and
//! ledgers in temporary directories.

use super::*;
use crate::bus::relay_v2::StateV2;
use crate::vault::{VaultMap, VaultRecord};
use lean_ctx::core::policy::cloud_floor;
use serde_json::json;

const VAULT: &str = "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01";
const VAULT_HEX: &str = "3f1c2b6a8d4e4a7b9c105e2f6a7b8c01";
const LOCAL_WORKER: &str = "wkr_vk4oelpv5lzjmev44hun";
const TOKEN: &str = concat!("gh", "p_aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789");

fn policy(sharing: &Value) -> EffectivePolicyV1 {
    serde_json::from_value(json!({
        "schema": "leanctx.cloud.policy/v1",
        "security": {}, "context": {}, "sharing": sharing
    }))
    .unwrap()
}

/// A floor under `cloud_data` whose default policy has `sharing`.
fn install(cloud_data: &Path, sharing: &Value, not_after: &str) {
    let dir = cloud_floor::floor_dir_in(cloud_data);
    std::fs::create_dir_all(&dir).unwrap();
    let floor = json!({
        "schema": cloud_floor::FLOOR_SCHEMA, "version": 3,
        "digest": format!("sha256:{}", "a".repeat(64)),
        "not_after": not_after,
        "policy": {"effective": {
            "schema": "leanctx.cloud.policy/v1",
            "security": {}, "context": {}, "sharing": sharing
        }, "vaults": []}
    });
    std::fs::write(dir.join(cloud_floor::FLOOR_FILE), floor.to_string()).unwrap();
    std::fs::write(dir.join(cloud_floor::REQUIRED_FILE), r#"{"version":3}"#).unwrap();
}

fn plain_screen() -> Screen {
    Screen::with_rules(Vec::new(), std::sync::Arc::new(Vec::new()), Vec::new())
}

fn now() -> lean_ctx_time::UtcDateTime {
    lean_ctx_time::UtcDateTime::now()
}

#[test]
fn a_personal_account_exchanges_nothing() {
    let refusal = account_refusal(Some("personal")).unwrap();
    assert_eq!(refusal.rule, "account");
    assert!(account_refusal(Some("team")).is_none());
    assert!(account_refusal(None).is_none());
}

#[test]
fn the_kinds_member_is_read_fail_closed() {
    assert_eq!(allowed_kinds(&policy(&json!({}))), Ok(None));
    assert_eq!(
        allowed_kinds(&policy(&json!({"agent_exchange": ["finding", "handoff"]}))),
        Ok(Some(BTreeSet::from([
            MessageType::Finding,
            MessageType::Handoff
        ])))
    );
    for bad in [
        json!(["handoff", "finding"]),
        json!(["finding", "finding"]),
        json!(["progress"]),
        json!(["gossip"]),
        json!("finding"),
        json!([1]),
    ] {
        assert_eq!(
            allowed_kinds(&policy(&json!({ "agent_exchange": bad.clone() }))),
            Err(()),
            "{bad}"
        );
    }
    // An empty list lets nothing leave.
    let none = policy(&json!({"agent_exchange": []}));
    assert!(policy_refusal(&none, VAULT, MessageType::Finding).is_some());
}

#[test]
fn the_team_policy_decides_vault_and_kind() {
    let open = policy(&json!({}));
    assert!(policy_refusal(&open, VAULT, MessageType::Handoff).is_none());
    let listed = policy(&json!({"allowed_vaults": [VAULT_HEX]}));
    assert!(policy_refusal(&listed, VAULT, MessageType::Handoff).is_none());
    let other = policy(&json!({"allowed_vaults": ["0".repeat(32)]}));
    let refusal = policy_refusal(&other, VAULT, MessageType::Handoff).unwrap();
    assert_eq!(refusal.rule, "policy");
    assert!(refusal.why.contains("sharing.allowed_vaults"));
    let findings_only = policy(&json!({"agent_exchange": ["finding"]}));
    assert!(policy_refusal(&findings_only, VAULT, MessageType::Finding).is_none());
    let refusal = policy_refusal(&findings_only, VAULT, MessageType::Handoff).unwrap();
    assert!(refusal.why.contains("handoff"), "{refusal}");
    let broken = policy(&json!({"agent_exchange": "all"}));
    assert!(policy_refusal(&broken, VAULT, MessageType::Finding).is_some());
}

#[test]
fn the_installed_floor_applies_and_an_expired_one_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let cloud_data = dir.path().join("cloud");
    // No policy installed: Cloud decides.
    assert!(floor_refusal(&cloud_data, VAULT, MessageType::Handoff, now()).is_none());
    install(
        &cloud_data,
        &json!({"agent_exchange": ["finding"]}),
        "2099-01-01T00:00:00Z",
    );
    assert!(floor_refusal(&cloud_data, VAULT, MessageType::Finding, now()).is_none());
    assert!(floor_refusal(&cloud_data, VAULT, MessageType::Handoff, now()).is_some());
    install(&cloud_data, &json!({}), "2000-01-01T00:00:00Z");
    let expired = floor_refusal(&cloud_data, VAULT, MessageType::Finding, now()).unwrap();
    assert!(expired.why.contains("expired"), "{expired}");
    // A damaged floor refuses.
    std::fs::write(
        cloud_floor::floor_dir_in(&cloud_data).join(cloud_floor::FLOOR_FILE),
        "{",
    )
    .unwrap();
    assert!(floor_refusal(&cloud_data, VAULT, MessageType::Finding, now()).is_some());
}

#[test]
fn every_outbound_field_is_screened() {
    let mut screen = plain_screen();
    assert!(screen_fields(&mut screen, &["the lock is released after commit", ""]).is_ok());
    let in_ref = format!("url:https://example.com/?t={TOKEN}");
    let refusal = screen_fields(&mut screen, &["clean text", &in_ref]).unwrap_err();
    assert_eq!(refusal.rule, "content");
    // The reason names the kind of content, never the content.
    assert!(!refusal.why.contains(TOKEN), "{refusal}");
    assert!(screen_fields(&mut screen, &["mail jane.doe@example.com"]).is_err());
}

#[test]
fn the_audit_ledger_is_private_bounded_and_content_free() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    assert!(entries(data).unwrap().is_empty());
    let mut entry = Entry::now(
        now(),
        "out",
        "queued",
        MessageType::Finding,
        "#acme/payments",
    );
    entry.digest = Some(digest(&json!({"statement": "s", "evidence": "stated"})));
    record(data, &entry).unwrap();
    let refused = Entry::now(
        now(),
        "out",
        "refused",
        MessageType::Handoff,
        "#acme/payments",
    )
    .refused_by(&Refusal::new("policy", "not allowed"));
    record(data, &refused).unwrap();
    assert_eq!(entries(data).unwrap(), vec![entry.clone(), refused]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(data.join("bus").join(AUDIT_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "{mode:o}");
    }
    // Damaged (a line cut short, a foreign file): moved aside, never
    // appended to, and a new ledger starts.
    let path = data.join("bus").join(AUDIT_FILE);
    std::fs::write(&path, "not json").unwrap();
    assert!(entries(data).is_err());
    let last = Entry::now(
        now(),
        "in",
        "delivered",
        MessageType::Answer,
        "#acme/payments",
    );
    record(data, &last).unwrap();
    assert_eq!(entries(data).unwrap(), vec![last]);
    assert!(
        data.join("bus")
            .join(format!("{AUDIT_FILE}.damaged-1"))
            .exists()
    );
}

#[test]
fn each_record_appends_one_line_and_the_ledger_stays_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let path = data.join("bus").join(AUDIT_FILE);
    // One timestamp for every entry: comparing a recorded entry with a rebuilt
    // one must not depend on the clock ticking over in between.
    let at = now();
    let entry = |n: usize| {
        let mut e = Entry::now(at, "out", "queued", MessageType::Finding, "#acme/payments");
        e.msg_id = Some(format!("m{n}"));
        e
    };
    record(data, &entry(0)).unwrap();
    let before = std::fs::read(&path).unwrap();
    record(data, &entry(1)).unwrap();
    let after = std::fs::read(&path).unwrap();
    // The earlier bytes stay as they were: only one line is added.
    assert!(after.starts_with(&before));
    let mut line = serde_json::to_vec(&entry(1)).unwrap();
    line.push(b'\n');
    assert_eq!(&after[before.len()..], line.as_slice());
    // Past the size bound the next append compacts to the newest AUDIT_MAX.
    let full: Vec<Entry> = (0..AUDIT_MAX + 5).map(entry).collect();
    std::fs::write(&path, ledger_bytes(&full).unwrap()).unwrap();
    assert_eq!(entries(data).unwrap().len(), AUDIT_MAX);
    let size = std::fs::metadata(&path).unwrap().len();
    record_bounded(data, &entry(9999), size).unwrap();
    let kept = entries(data).unwrap();
    assert_eq!(kept.len(), AUDIT_MAX);
    assert_eq!(kept.last(), Some(&entry(9999)));
    let lines = std::fs::read_to_string(&path).unwrap().lines().count();
    assert_eq!(lines, AUDIT_MAX + 1, "header and AUDIT_MAX entries");
    // A long reason is kept short.
    let long = entry(1).refused_by(&Refusal::new("queue", "x".repeat(5000)));
    assert_eq!(long.reason.unwrap().chars().count(), REASON_MAX);
}

#[test]
fn a_ledger_damaged_mid_file_past_the_bound_is_set_aside_not_grown() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let path = data.join("bus").join(AUDIT_FILE);
    // One timestamp for every entry: comparing a recorded entry with a rebuilt
    // one must not depend on the clock ticking over in between.
    let at = now();
    let entry = |n: usize| {
        let mut e = Entry::now(at, "out", "queued", MessageType::Finding, "#acme/payments");
        e.msg_id = Some(format!("m{n}"));
        e
    };
    record(data, &entry(0)).unwrap();
    // Header and last byte intact, a middle line damaged: `intact` passes,
    // only the full read at compaction sees it.
    let mut bytes = ledger_bytes(&[entry(1), entry(2)]).unwrap();
    bytes.extend_from_slice(b"{\"cut short\n");
    bytes.extend(serde_json::to_vec(&entry(3)).unwrap());
    bytes.push(b'\n');
    std::fs::write(&path, &bytes).unwrap();
    assert!(intact(&path).unwrap());
    let size = std::fs::metadata(&path).unwrap().len();
    // Past the bound the next record still succeeds, sets the file aside
    // and starts a new ledger holding just that record.
    record_bounded(data, &entry(9999), size).unwrap();
    assert_eq!(entries(data).unwrap(), vec![entry(9999)]);
    let aside = data.join("bus").join(format!("{AUDIT_FILE}.damaged-1"));
    assert!(aside.exists());
    // Recording goes on and the ledger stays small.
    record_bounded(data, &entry(10_000), size).unwrap();
    assert_eq!(entries(data).unwrap(), vec![entry(9999), entry(10_000)]);
    assert!(std::fs::metadata(&path).unwrap().len() < size);
}

#[test]
fn an_entry_is_stamped_with_the_time_it_is_given() {
    let at = lean_ctx_time::UtcDateTime::parse_from_rfc3339("2026-11-19T10:00:00Z").unwrap();
    let entry = Entry::now(at, "in", "dropped", MessageType::Offer, "#acme/payments");
    assert_eq!(entry.at, "2026-11-19T10:00:00Z");
}

#[test]
fn the_digest_is_canonical() {
    assert_eq!(
        digest(&json!({"a": 1, "b": 2})),
        digest(&json!({"b": 2, "a": 1}))
    );
    assert!(digest(&json!({})).starts_with("sha256:"));
}

fn deliver(kind: &str, body: &Value, seq: u64) -> Value {
    json!({
        "schema": "leanctx.cloud.bus/v2",
        "t": "message.deliver",
        "id": "msg_00000000000000b1",
        "seq": seq,
        "to": "#acme/payments",
        "type": kind,
        "body": body,
        "refs": [{"kind": "node", "ref": "n-42"}],
        "sender": {
            "kind": "worker", "worker_id": "wkr_gawyravj36igdp3irdfo",
            "name": "mara-pc · Codex · reviewer", "owner_handle": "mara",
            "host_app": "codex"
        },
        "worker_id": "wkr_gawyravj36igdp3irdfo"
    })
}

#[test]
fn deliveries_from_other_machines_pass_the_same_rules() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let cloud_data = dir.path().join("cloud");
    let mut map = VaultMap::default();
    map.vaults.insert(
        "s".repeat(64),
        VaultRecord {
            vault_id: VAULT.into(),
            ns: "acme".into(),
            name: "payments".into(),
            sealed: false,
            roots: vec![dir.path().to_string_lossy().into_owned()],
        },
    );
    let mine = BTreeSet::from([LOCAL_WORKER.to_string()]);
    let mut state = StateV2::default();
    let finding = json!({
        "statement": "The export lock is released only after the commit.",
        "evidence": "observed"
    });
    // Admitted: audited with peer, id and digest, never the text.
    let Delivery::Allowed(entry) = delivered(
        &cloud_data,
        &map,
        &state,
        &deliver("finding", &finding, 5),
        &mine,
        now(),
    ) else {
        panic!("a clean finding is admitted");
    };
    assert_eq!(entry.direction, "in");
    assert_eq!(entry.outcome, "delivered");
    assert_eq!(entry.kind, "finding");
    assert_eq!(entry.peer.as_deref(), Some("wkr_gawyravj36igdp3irdfo"));
    assert_eq!(entry.msg_id.as_deref(), Some("msg_00000000000000b1"));
    assert_eq!(entry.digest, Some(digest(&finding)));
    assert!(
        !serde_json::to_string(&entry)
            .unwrap()
            .contains("export lock")
    );
    // A secret in the body is dropped.
    let leaking = json!({"text": format!("use {TOKEN}")});
    let Delivery::Refused { seq, entry, .. } = delivered(
        &cloud_data,
        &map,
        &state,
        &deliver("question", &leaking, 6),
        &mine,
        now(),
    ) else {
        panic!("a secret is dropped");
    };
    assert_eq!((seq, entry.outcome.as_str()), (6, "dropped"));
    assert_eq!(entry.rule.as_deref(), Some("content"));
    // The team policy applies to what arrives as well.
    install(
        &cloud_data,
        &json!({"agent_exchange": ["finding"]}),
        "2099-01-01T00:00:00Z",
    );
    let handoff = json!({"to_worker": LOCAL_WORKER, "resource": "task:7"});
    let Delivery::Refused { entry, .. } = delivered(
        &cloud_data,
        &map,
        &state,
        &deliver("handoff", &handoff, 7),
        &mine,
        now(),
    ) else {
        panic!("a kind the team does not exchange is dropped");
    };
    assert_eq!(entry.rule.as_deref(), Some("policy"));
    // Replays, status lines and this machine's own messages pass untouched.
    crate::bus::relay_v2::skip_delivery(&mut state, "#acme/payments", 7);
    assert_eq!(
        delivered(
            &cloud_data,
            &map,
            &state,
            &deliver("finding", &finding, 7),
            &mine,
            now()
        ),
        Delivery::Pass
    );
    assert_eq!(
        delivered(
            &cloud_data,
            &map,
            &state,
            &deliver("progress", &json!({"step": "build"}), 8),
            &mine,
            now()
        ),
        Delivery::Pass
    );
    let mut own = deliver("finding", &finding, 9);
    own["sender"]["worker_id"] = LOCAL_WORKER.into();
    assert_eq!(
        delivered(&cloud_data, &map, &state, &own, &mine, now()),
        Delivery::Pass
    );
}

/// A map with the vault of [`VAULT`] linked at `root`.
fn linked(root: &Path) -> VaultMap {
    let mut map = VaultMap::default();
    map.vaults.insert(
        "s".repeat(64),
        VaultRecord {
            vault_id: VAULT.into(),
            ns: "acme".into(),
            name: "payments".into(),
            sealed: false,
            roots: vec![root.to_string_lossy().into_owned()],
        },
    );
    map
}

/// Block the audit ledger: a directory where its file goes.
fn block_audit(cloud_data: &Path) {
    std::fs::create_dir_all(cloud_data.join("bus").join(AUDIT_FILE)).unwrap();
}

#[test]
fn an_unrecorded_delivery_moves_no_cursor_and_reaches_no_one() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let cloud_data = dir.path().join("cloud");
    let map = linked(dir.path());
    let mine = BTreeSet::from([LOCAL_WORKER.to_string()]);
    let finding =
        json!({"statement": "The lock is released after the commit.", "evidence": "observed"});
    let leaking = json!({"text": format!("use {TOKEN}")});
    let mut state = StateV2::default();

    // Recorded: an admitted one goes on, a refused one moves the cursor only.
    let admitted = admit(
        &cloud_data,
        &map,
        &mut state,
        &deliver("finding", &finding, 5),
        &mine,
        now(),
    );
    assert!(admitted.unwrap(), "a clean finding goes on");
    assert!(
        state.cursors.is_empty(),
        "the relay moves it when it applies"
    );
    let dropped = admit(
        &cloud_data,
        &map,
        &mut state,
        &deliver("question", &leaking, 6),
        &mine,
        now(),
    );
    assert!(!dropped.unwrap(), "a secret is dropped");
    assert_eq!(state.cursors.get("#acme/payments"), Some(&6));
    let outcomes: Vec<String> = entries(&cloud_data)
        .unwrap()
        .into_iter()
        .map(|e| e.outcome)
        .collect();
    assert_eq!(outcomes, ["delivered", "dropped"]);

    // The audit cannot be written: neither applied nor skipped, so the
    // same delivery comes again.
    std::fs::remove_file(cloud_data.join("bus").join(AUDIT_FILE)).unwrap();
    block_audit(&cloud_data);
    let before = state.clone();
    assert!(
        admit(
            &cloud_data,
            &map,
            &mut state,
            &deliver("question", &leaking, 7),
            &mine,
            now()
        )
        .is_err()
    );
    assert!(
        admit(
            &cloud_data,
            &map,
            &mut state,
            &deliver("finding", &finding, 8),
            &mine,
            now()
        )
        .is_err()
    );
    assert_eq!(state, before, "no cursor moved");
}

#[test]
fn a_send_whose_audit_cannot_be_written_never_leaves() {
    use crate::bus::messages_v2::{Extras, send_in};
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let store = crate::CloudStore::at(dir.path().join("config"), dir.path().join("cloud"));
    store
        .write_config(
            crate::state::INSTANCE_FILE,
            &crate::state::InstanceRecord {
                instance_id: "inst_a".into(),
                org_id: "org_a".into(),
                org_name: "Acme".into(),
                label: "mac".into(),
                url: "https://cloud.example".into(),
                org_salt_id: "salt".into(),
                enrolled_at: "2026-10-24T00:00:00Z".into(),
                web_url: None,
                org_kind: Some("team".into()),
                company_name: None,
            },
        )
        .unwrap();
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_string_lossy().into_owned();
    linked(project.path()).save(store.data_dir()).unwrap();
    let mut registry = lean_ctx::core::agents::AgentRegistry::new();
    let agent = registry
        .register("claude", Some("ops"), &root, None)
        .unwrap();
    registry.agents[0].app_session_id = None;
    registry.save().unwrap();
    let data = store.data_dir().to_path_buf();
    let queued = || crate::bus::relay_v2::queued_frames(&data, &mut StateV2::default()).len();
    let send = || {
        send_in(
            &store,
            &root,
            &agent,
            Some("question"),
            "Which retry budget does the export use?",
            Extras::default(),
        )
    };

    // No audit record, no message.
    block_audit(&data);
    let refused = send().unwrap_err();
    assert!(refused.contains("audit"), "{refused}");
    assert_eq!(queued(), 0);
    std::fs::remove_dir(data.join("bus").join(AUDIT_FILE)).unwrap();

    // The queue refuses: the queued record is followed by a refused one.
    std::fs::create_dir_all(data.join("bus").join("publish_queue.json")).unwrap();
    assert!(send().is_err());
    let audit = entries(&data).unwrap();
    let last: Vec<(&str, Option<&str>)> = audit
        .iter()
        .map(|e| (e.outcome.as_str(), e.rule.as_deref()))
        .collect();
    assert_eq!(last, [("queued", None), ("refused", Some("queue"))]);
    assert_eq!(audit[0].msg_id, audit[1].msg_id);
    std::fs::remove_dir(data.join("bus").join("publish_queue.json")).unwrap();

    // Recorded: it is queued under the id the audit names.
    let id = send().unwrap();
    assert_eq!(queued(), 1);
    let audit = entries(&data).unwrap();
    let last = audit.last().unwrap();
    assert_eq!(
        (last.outcome.as_str(), last.msg_id.as_deref()),
        ("queued", Some(id.as_str()))
    );
    assert_eq!(last.agent.as_deref(), Some(agent.as_str()));
}
