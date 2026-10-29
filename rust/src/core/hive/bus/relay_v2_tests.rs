// SPDX-License-Identifier: Apache-2.0
//! Bus v2 relay decisions against the C0 2.0 `bus/` fixtures.

use super::*;

const VAULT_REF: &str = "vault:3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01";
const WORKER: &str = "wkr_vk4oelpv5lzjmev44hun";
const REVIEWER: &str = "wkr_gawyravj36igdp3irdfo";

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/crates/lean-ctx-protocol/tests/fixtures/hive-v2/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// The control keys of the C0 key set v3 fixture, as the relay takes them.
fn keys() -> ControlKeys {
    use crate::core::hive::keyset_v3::{Purpose, verify};
    let signing = fixture("vectors/signing.vectors.json");
    let mut root = [0u8; 32];
    hex::decode_to_slice(
        signing["keys"]["root_new"]["public_key_hex"]
            .as_str()
            .unwrap(),
        &mut root,
    )
    .unwrap();
    verify(&fixture("routes/keyset.v3.response.json"), &[root], &[])
        .unwrap()
        .keys_of(Purpose::Control)
}

fn control(name: &str) -> (bool, Control) {
    let frame = fixture(&format!("bus/{name}.json"));
    match super::super::v2::parse_frame(&frame.to_string()).unwrap() {
        super::super::v2::Inbound::Control { lifted, control } => (lifted, *control),
        other => panic!("{other:?}"),
    }
}

struct Setup {
    _data: crate::core::data_dir::IsolatedDataDir,
    dir: tempfile::TempDir,
    registry: AgentRegistry,
    map: VaultMap,
    agent: String,
}

fn setup() -> Setup {
    let data = crate::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_string_lossy().into_owned();
    let mut registry = AgentRegistry::new();
    let agent = registry
        .register("claude", Some("backend"), &root, None)
        .unwrap();
    let mut map = VaultMap::default();
    map.vaults.insert(
        "s".repeat(64),
        VaultRecord {
            vault_id: "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01".into(),
            ns: "acme".into(),
            name: "payments".into(),
            sealed: false,
            roots: vec![root],
        },
    );
    Setup {
        _data: data,
        dir,
        registry,
        map,
        agent,
    }
}

fn screen() -> Screen {
    Screen::with_rules(Vec::new(), std::sync::Arc::new(Vec::new()), Vec::new())
}

fn post(s: &mut Setup, to: Option<&str>, category: &str, text: &str) -> String {
    let root = s.dir.path().to_string_lossy().into_owned();
    s.registry
        .post_message_keyed(
            Some(&root),
            &s.agent,
            to,
            category,
            text,
            PrivacyLevel::Team,
            MessagePriority::Normal,
            None,
            None,
        )
        .unwrap()
}

#[test]
fn only_vault_addressed_posts_become_publishes() {
    let mut s = setup();
    let local = post(&mut s, None, "status", "E4 merged");
    let other = post(&mut s, Some("#acme/other"), "status", "E4 merged");
    let answer = post(&mut s, Some("#acme/payments"), "answer", "yes");
    let status = post(
        &mut s,
        Some("#acme/payments"),
        "status",
        "V6.2a relay wired",
    );
    let question = post(
        &mut s,
        Some("#acme/payments"),
        "question",
        "Is E14 v2 live?",
    );
    let mut state = StateV2::default();
    let worker = |_: &AgentEntry| Some(WORKER.to_string());
    let out = outgoing(&s.registry, &s.map, &mut state, &mut screen(), &worker);
    let ids: Vec<_> = out.iter().map(|o| o.entry_id.clone()).collect();
    assert_eq!(ids, [status.clone(), question.clone()]);
    for item in &out {
        assert_eq!(item.frame["t"], "place.publish");
        let mut body = item.frame.clone();
        body.as_object_mut().unwrap().remove("t");
        let publish: Publish = serde_json::from_value(body).unwrap();
        publish.validate().unwrap();
        assert_eq!(publish.to, "#acme/payments");
        assert_eq!(publish.worker_id, WORKER);
    }
    assert_eq!(out[0].frame["type"], "progress");
    assert_eq!(out[0].frame["body"]["step"], "V6.2a relay wired");
    assert_eq!(out[1].frame["body"]["text"], "Is E14 v2 live?");
    for id in [&local, &other, &answer] {
        assert!(state.was_decided(id), "decided once");
    }
    assert_eq!(state.withheld, 2, "unlinked vault and a bare answer");
    // In flight until stored: not sent twice.
    assert!(outgoing(&s.registry, &s.map, &mut state, &mut screen(), &worker).is_empty());
    let first = out[0].frame["client_msg_id"].as_str().unwrap().to_string();
    state.stored(&first);
    assert!(state.was_decided(&status));
}

#[test]
fn an_unregistered_or_held_worker_waits() {
    let mut s = setup();
    let id = post(&mut s, Some("#acme/payments"), "status", "waiting");
    let mut state = StateV2::default();
    let none = |_: &AgentEntry| None;
    assert!(outgoing(&s.registry, &s.map, &mut state, &mut screen(), &none).is_empty());
    assert!(
        !state.was_decided(&id),
        "not decided: it goes once registered"
    );
    let (lifted, hold) = control("frame.control.issued");
    assert!(state.apply_control(lifted, hold, &keys()));
    let reviewer = |_: &AgentEntry| Some(REVIEWER.to_string());
    assert!(
        outgoing(&s.registry, &s.map, &mut state, &mut screen(), &reviewer).is_empty(),
        "hold wins over post"
    );
    let worker = |_: &AgentEntry| Some(WORKER.to_string());
    assert_eq!(
        outgoing(&s.registry, &s.map, &mut state, &mut screen(), &worker).len(),
        1,
        "the hold names the reviewer only"
    );
}

#[test]
fn controls_count_only_when_signed_by_a_control_key() {
    let mut state = StateV2::default();
    let (lifted, hold) = control("frame.control.issued");
    // No key of purpose control (a v2 key set): ignored and counted.
    assert!(!state.apply_control(lifted, hold.clone(), &ControlKeys::new()));
    assert_eq!(state.sig_invalid, 1);
    let mut tampered = hold.clone();
    tampered.reason = "Holding for no reason at all.".into();
    assert!(!state.apply_control(false, tampered, &keys()));
    assert_eq!(state.sig_invalid, 2);
    assert!(state.apply_control(lifted, hold, &keys()));
    assert_eq!(presence_state(&state, REVIEWER, true), PresenceState::Held);
    assert_eq!(presence_state(&state, WORKER, true), PresenceState::Active);
    assert_eq!(presence_state(&state, WORKER, false), PresenceState::Idle);
    assert!(state.held(Some(VAULT_REF), REVIEWER).is_some());
    assert!(
        state
            .held(Some("vault:00000000-0000-0000-0000-000000000000"), REVIEWER)
            .is_none()
    );
    // A veto names one ref; lifting it ends it.
    let veto: Control = serde_json::from_value(fixture("bus/control.veto.json")).unwrap();
    assert!(state.apply_control(false, veto, &keys()));
    assert!(state.vetoed("msg_00000000000000a4").is_some());
    let lifted: Control = serde_json::from_value(fixture("bus/control.veto.lifted.json")).unwrap();
    assert!(state.apply_control(true, lifted, &keys()));
    assert!(state.vetoed("msg_00000000000000a4").is_none());
}

#[test]
fn deliveries_land_once_never_as_a_local_agent_and_never_forwarded() {
    let mut s = setup();
    let mut state = StateV2::default();
    let mut deliver = fixture("bus/frame.message_deliver.json");
    deliver.as_object_mut().unwrap().remove("t");
    let workers = BTreeSet::from([WORKER.to_string()]);
    assert_eq!(
        apply_delivery(&mut s.registry, &s.map, &mut state, &deliver, &workers).unwrap(),
        Some(("#acme/payments".to_string(), 2))
    );
    assert_eq!(
        apply_delivery(&mut s.registry, &s.map, &mut state, &deliver, &workers).unwrap(),
        None,
        "replay"
    );
    let landed: Vec<_> = s
        .registry
        .scratchpad
        .iter()
        .filter(|e| e.from_agent == REVIEWER)
        .collect();
    assert_eq!(landed.len(), 1);
    assert_eq!(
        landed[0].message,
        "Yes; the lock is released only after the export commit."
    );
    assert!(super::super::relay::is_inbound(landed[0]));
    let worker = |_: &AgentEntry| Some(WORKER.to_string());
    assert!(
        outgoing(&s.registry, &s.map, &mut state, &mut screen(), &worker).is_empty(),
        "never forwarded"
    );
    // A delivery claiming one of this machine's workers is dropped.
    let mut spoof = deliver.clone();
    spoof["seq"] = 3.into();
    spoof["sender"]["worker_id"] = WORKER.into();
    apply_delivery(&mut s.registry, &s.map, &mut state, &spoof, &workers).unwrap();
    assert_eq!(state.cursors["#acme/payments"], 3);
    assert!(s.registry.scratchpad.iter().all(|e| e.from_agent != WORKER));
}

#[test]
fn the_hello_matches_the_contract_shape() {
    let s = setup();
    let agents = s.registry.list_active(None);
    let subscribe = subscriptions(&s.map, &agents);
    assert_eq!(subscribe, ["#acme/payments"]);
    let mut state = StateV2::default();
    state.cursors.insert("#acme/payments".into(), 41);
    state.cursors.insert("#acme/gone".into(), 7);
    let presence = [PresenceEntry {
        worker_id: WORKER.into(),
        role: "backend".into(),
        state: PresenceState::Active,
    }];
    let frame = hello(&subscribe, &state, &presence);
    assert_eq!(
        frame,
        json!({"t": "hello", "protocol": "2.0", "subscribe": ["#acme/payments"],
            "cursors": {"#acme/payments": 41},
            "presence": [{"worker_id": WORKER, "role": "backend", "state": "active"}]})
    );
    let sample = fixture("bus/frame.hello.json");
    let keys = |v: &Value| {
        v.as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(keys(&frame), keys(&sample));
    // No linked session, no subscription.
    assert!(subscriptions(&VaultMap::default(), &agents).is_empty());
}

#[test]
fn a_resent_post_keeps_its_client_msg_id() {
    let mut s = setup();
    post(&mut s, Some("#acme/payments"), "status", "V6 relay wired");
    let worker = |_: &AgentEntry| Some(WORKER.to_string());
    let mut state = StateV2::default();
    let first = outgoing(&s.registry, &s.map, &mut state, &mut screen(), &worker);
    // A new socket clears what was in flight; the post goes again.
    state.in_flight.clear();
    let again = outgoing(&s.registry, &s.map, &mut state, &mut screen(), &worker);
    assert_eq!(
        first[0].frame["client_msg_id"],
        again[0].frame["client_msg_id"]
    );
    assert!(super::super::v2::is_ulid(
        first[0].frame["client_msg_id"].as_str().unwrap()
    ));
}

#[test]
fn queued_publishes_survive_until_stored_and_wait_while_held() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::core::hive::HiveStore::at(dir.path().join("c"), dir.path().join("d"));
    let data = store.data_dir().to_path_buf();
    let message = fixture("bus/message.finding.collaboration.json");
    let mut publish = serde_json::Map::new();
    for field in [
        "schema",
        "client_msg_id",
        "type",
        "worker_id",
        "refs",
        "body",
        "about",
        "counts",
        "window",
    ] {
        publish.insert(field.into(), message[field].clone());
    }
    publish.insert("to".into(), "#acme/payments".into());
    let publish: Publish = serde_json::from_value(Value::Object(publish)).unwrap();
    queue_publish(&store, publish.clone()).unwrap();
    queue_publish(&store, publish.clone()).unwrap();
    let mut bad = publish.clone();
    bad.counts = None;
    assert!(
        queue_publish(&store, bad).is_err(),
        "validated before queueing"
    );

    let mut state = StateV2::default();
    let frames = queued_frames(&data, &mut state);
    assert_eq!(frames.len(), 1, "queued once");
    assert_eq!(frames[0]["t"], "place.publish");
    assert_eq!(frames[0]["about"], "collaboration");
    assert!(queued_frames(&data, &mut state).is_empty(), "in flight");
    // Offline: a new socket sends it again; stored: it is gone.
    state.in_flight.clear();
    assert_eq!(queued_frames(&data, &mut state).len(), 1);
    let id = publish.client_msg_id.clone();
    assert!(settle_queued(&data, &id).unwrap());
    state.in_flight.clear();
    assert!(queued_frames(&data, &mut state).is_empty());

    // A held worker's publish waits.
    queue_publish(&store, publish.clone()).unwrap();
    let (lifted, mut hold) = control("frame.control.issued");
    hold.worker_id = Some(publish.worker_id.clone());
    state.controls.insert(hold.id.clone(), hold);
    let _ = lifted;
    assert!(queued_frames(&data, &mut state).is_empty());
}
