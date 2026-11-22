// SPDX-License-Identifier: Apache-2.0

use ed25519_dalek::SigningKey;

use super::*;
use lean_ctx::core::session::{Decision, FileTouched, TaskInfo};

fn session(task: &str, progress: Option<u8>, files: &[(&str, bool)]) -> SessionState {
    let mut s = SessionState::new();
    s.id = format!("ses-{}", task.len());
    s.started_at = lean_ctx_time::UtcDateTime::from_timestamp_millis(1_792_796_400_000).unwrap();
    s.updated_at = lean_ctx_time::UtcDateTime::from_timestamp_millis(1_792_799_940_000).unwrap();
    s.task = Some(TaskInfo {
        description: task.into(),
        intent: None,
        progress_pct: progress,
    });
    for (path, modified) in files {
        s.files_touched.push(FileTouched {
            path: (*path).into(),
            file_ref: None,
            read_count: 1,
            modified: *modified,
            last_mode: "full".into(),
            tokens: 0,
            stale: false,
            context_item_id: None,
            summary: None,
        });
    }
    s
}

fn commit(sha: &str, subject: &str) -> Commit {
    Commit {
        sha: sha.into(),
        time: None,
        subject: subject.into(),
        body: String::new(),
    }
}

fn schema() -> Value {
    let path = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .join("crates/lean-ctx-protocol/tests/fixtures/cloud-v2/schemas/event.schema.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// The body has every required field, no other field, and anchors of the
/// contract's shape.
fn assert_contract_body(body: &Value, schema: &Value) {
    let def = &schema["$defs"]["episode_body"];
    let allowed = def["properties"].as_object().unwrap();
    let obj = body.as_object().unwrap();
    for key in obj.keys() {
        assert!(allowed.contains_key(key), "unknown field {key}");
    }
    for key in def["required"].as_array().unwrap() {
        assert!(obj.contains_key(key.as_str().unwrap()), "missing {key}");
    }
    let anchor = schema["$defs"]["anchor"]["properties"].as_object().unwrap();
    for t in body["touched"].as_array().into_iter().flatten() {
        assert!(
            t.as_object()
                .unwrap()
                .keys()
                .all(|k| anchor.contains_key(k))
        );
        assert_eq!(t["digest"].as_str().unwrap().len(), 64);
    }
    let outcomes = def["properties"]["outcome"]["enum"].as_array().unwrap();
    assert!(outcomes.contains(&body["outcome"]));
}

#[test]
fn the_outcome_and_goal_come_from_the_last_task() {
    let cases = [
        (
            "Fix the relay [100%]",
            None,
            Outcome::Succeeded,
            "Fix the relay",
        ),
        (
            "Fix the relay",
            Some(100),
            Outcome::Succeeded,
            "Fix the relay",
        ),
        (
            "Fix the relay [done]",
            None,
            Outcome::Succeeded,
            "Fix the relay",
        ),
        (
            "Fix the relay [45%]",
            Some(45),
            Outcome::Partial,
            "Fix the relay",
        ),
        (
            "Fix the relay: failed [100%]",
            None,
            Outcome::Failed,
            "Fix the relay: failed",
        ),
        (
            "Port the parser, abandoned",
            None,
            Outcome::Abandoned,
            "Port the parser, abandoned",
        ),
        (
            "Unfailedness is no keyword",
            None,
            Outcome::Partial,
            "Unfailedness is no keyword",
        ),
    ];
    for (task, progress, outcome, goal) in cases {
        assert_eq!(Outcome::of(task, progress), outcome, "{task}");
        assert_eq!(goal_of(task), goal, "{task}");
    }
}

#[test]
fn twenty_recorded_sessions_become_contract_episodes() {
    let repo = crate::capture::tests::repo();
    let root = repo.path();
    let abs = root.join("src/lib.rs").to_string_lossy().into_owned();
    let schema = schema();
    let mut checkout = Checkout::new(root);
    let tasks = [
        "Find why refunds double-post on retries [100%]",
        "Speed up the relay handshake [60%]",
        "Rewrite the outbox retry loop: failed",
        "Port the scheduler to async, abandoned",
        "Add sealed vault key rotation [done]",
    ];
    let mut built = 0;
    for i in 0..20 {
        let task = tasks[i % tasks.len()];
        let mut s = session(
            task,
            None,
            &[
                (abs.as_str(), i % 2 == 0),
                ("src/lib.rs", true),
                ("src/missing.rs", true),
                ("/elsewhere/outside.rs", true),
                ("README.md", false),
            ],
        );
        if i % 3 == 0 {
            s.decisions.push(Decision {
                summary: "Keyed the refund calls by request id.".into(),
                rationale: None,
                timestamp: s.updated_at,
            });
        }
        let commits = if i % 4 == 0 {
            vec![
                commit(
                    "3e9f0c1a7b3e9f0c1a7b3e9f0c1a7b3e9f0c1a7b",
                    "fix: key refunds",
                ),
                commit(
                    "4a551fc4524a551fc4524a551fc4524a551fc452",
                    "Revert \"feat: retry refunds twice\"",
                ),
            ]
        } else {
            vec![]
        };
        let dead = vec![
            "Suspected clock skew between workers".to_string(),
            "Build failed at 2026-10-24T14:02 with code 101".to_string(),
            "Two\nlines".to_string(),
        ];
        let e = assemble(&s, root, &dead, &commits, &mut checkout).unwrap();
        assert_eq!(e.touched.len(), 1, "only HEAD files inside the checkout");
        assert_eq!(e.touched[0]["path"], "src/lib.rs");
        assert_eq!(
            e.dead_ends[0], "Suspected clock skew between workers",
            "log text and multi-line dead ends are left out"
        );
        assert_eq!(e.dead_ends.len(), if i % 4 == 0 { 2 } else { 1 });
        assert!(e.session_ref.starts_with("ses_"));
        assert_contract_body(&e.body().unwrap(), &schema);
        built += 1;
    }
    assert_eq!(built, 20);
}

#[test]
fn an_episode_without_task_or_reference_is_not_captured() {
    let repo = crate::capture::tests::repo();
    let mut checkout = Checkout::new(repo.path());
    let mut s = session("Look", None, &[("src/lib.rs", false)]);
    assert_eq!(
        assemble(&s, repo.path(), &[], &[], &mut checkout),
        Err(ReasonCode::EpisodeShape),
        "a goal under 8 characters"
    );
    s.task.as_mut().unwrap().description = "Look around the relay code".into();
    assert_eq!(
        assemble(&s, repo.path(), &[], &[], &mut checkout),
        Err(ReasonCode::NoReference),
        "nothing changed, nothing committed"
    );
    s.task = None;
    assert_eq!(
        assemble(&s, repo.path(), &[], &[], &mut checkout),
        Err(ReasonCode::EpisodeShape)
    );
}

#[test]
fn an_episode_is_a_signed_node_add() {
    let repo = crate::capture::tests::repo();
    let mut checkout = Checkout::new(repo.path());
    let s = session(
        "Find why refunds double-post [100%]",
        None,
        &[("src/lib.rs", true)],
    );
    let e = assemble(&s, repo.path(), &[], &[], &mut checkout).unwrap();
    let key = SigningKey::from_bytes(&[0x49; 32]);
    let place = Place {
        kind: "vault".into(),
        id: "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01".into(),
        pk_id: [0; 32],
        causal_base: 5,
    };
    let signer = Signer {
        key: &key,
        hlc_node: "6f2b9c4e-1a3d-4e5f-8a7b-0c1d2e3f4a5b".into(),
        worker_id: "wkr_vk4oelpv5lzjmev44hun".into(),
    };
    let (event, target) = e
        .node_add(
            &place,
            Stamp {
                wall_ms: 1_792_800_006_000,
                counter: 0,
            },
            &signer,
            Some("hs_0123456789abcdef0123456789abcdef"),
        )
        .unwrap();
    assert_eq!(event["node_type"], "episode");
    assert_eq!(event["target"], target.as_str());
    assert_eq!(event["capture"]["evidence"], "observed");
    assert!(sign::verify(&event, key.verifying_key().as_bytes()));
    // V9: the signed envelope names the session by its keyed ref; the
    // body keeps its own episode ref (contract body field, unchanged).
    assert_eq!(event["session_ref"], "hs_0123456789abcdef0123456789abcdef");
    assert_ne!(event["session_ref"], event["body"]["session_ref"]);
    let (without, _) = e.node_add(&place, Stamp::default(), &signer, None).unwrap();
    assert!(without.get("session_ref").is_none(), "{without}");
    assert_eq!(
        event["body_commit"],
        sign::body_commit(&place.pk_id, &event["body"]).unwrap()
    );
    assert!(
        event["body"].get("anchors").is_none(),
        "episodes carry no anchors"
    );
}

#[test]
fn an_uploaded_episode_never_ties_its_keyed_ref_to_the_session_id() {
    const ID: &str = "20261116-091500u123456p4242s0";
    let repo = crate::capture::tests::repo();
    let mut checkout = Checkout::new(repo.path());
    let mut s = session(
        "Find why refunds double-post [100%]",
        None,
        &[("src/lib.rs", true)],
    );
    s.id = ID.into();
    let key = SigningKey::from_bytes(&[0x49; 32]);
    let place = Place {
        kind: "vault".into(),
        id: "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01".into(),
        pk_id: [0; 32],
        causal_base: 5,
    };
    let signer = Signer {
        key: &key,
        hlc_node: "6f2b9c4e-1a3d-4e5f-8a7b-0c1d2e3f4a5b".into(),
        worker_id: "wkr_vk4oelpv5lzjmev44hun".into(),
    };
    let unsalted = crate::knowledge::draft::session_ref(ID);

    // Keyed: the body ref is keyed apart, the envelope carries `hs_`.
    let refs = SessionRefs::from_key(crate::receipts::ReceiptKey::from_bytes([7; 32]));
    let mut e = assemble(&s, repo.path(), &[], &[], &mut checkout).unwrap();
    let data = tempfile::tempdir().unwrap();
    let (body_ref, envelope) = uploaded_refs(Some(&refs), data.path(), ID).unwrap();
    e.session_ref = body_ref;
    assert_eq!(envelope, refs.event_ref(ID));
    let (event, _) = e
        .node_add(&place, Stamp::default(), &signer, envelope.as_deref())
        .unwrap();
    assert_eq!(event["session_ref"].as_str(), envelope.as_deref());
    assert_eq!(event["body"]["session_ref"], refs.episode_ref(ID));
    assert_contract_body(&event["body"], &schema());
    let wire = serde_json::to_string(&event).unwrap();
    assert!(!wire.contains(ID), "raw session id leaves: {wire}");
    assert!(!wire.contains(&unsalted), "unsalted ref leaves: {wire}");
    assert!(
        !wire.contains(&unsalted[4..]),
        "unsalted digest leaves: {wire}"
    );

    // A Cloud that did not announce `session_ref` gets no envelope ref, but
    // the body is still keyed under the local key, never unsalted.
    let mut e = assemble(&s, repo.path(), &[], &[], &mut checkout).unwrap();
    let (body_ref, envelope) = uploaded_refs(None, data.path(), ID).unwrap();
    assert_eq!(envelope, None);
    e.session_ref = body_ref;
    let (event, _) = e.node_add(&place, Stamp::default(), &signer, None).unwrap();
    assert!(event.get("session_ref").is_none(), "{event}");
    let local = crate::receipts::ReceiptKey::load_or_create(data.path()).unwrap();
    assert_eq!(
        event["body"]["session_ref"],
        SessionRefs::from_key(local).episode_ref(ID)
    );
    assert_contract_body(&event["body"], &schema());
    let wire = serde_json::to_string(&event).unwrap();
    assert!(!wire.contains(ID), "raw session id leaves: {wire}");
    assert!(
        !wire.contains(&unsalted[4..]),
        "unsalted ref leaves: {wire}"
    );

    // Without a readable local key no ref is made at all: the episode is
    // withheld rather than sent with the unsalted digest.
    let keyless = tempfile::tempdir().unwrap();
    crate::session_ref::make_keyless(keyless.path());
    let error = uploaded_refs(None, keyless.path(), ID).unwrap_err();
    assert!(error.to_string().contains("receipts.hmac-key"), "{error}");
    // The announced path holds its key in memory and is unaffected.
    assert!(uploaded_refs(Some(&refs), keyless.path(), ID).is_ok());
}

/// !406 follow-up at the `emit_ended` level: a session that ended while
/// the local key is unavailable gets no episode and is not marked done,
/// so the next run (with the key back) emits it exactly once, keyed.
#[test]
fn an_ended_session_withheld_without_the_key_is_emitted_once_on_retry() {
    const ID: &str = "20261116-091500u123456p4242s0";
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let repo = crate::capture::tests::repo();
    let root = repo.path().canonicalize().unwrap();
    let now_ms = u64::try_from(lean_ctx_time::UtcDateTime::now().timestamp_millis()).unwrap();
    let mut s = session(
        "Find why refunds double-post [100%]",
        None,
        &[("src/lib.rs", true)],
    );
    s.id = ID.into();
    s.project_root = Some(root.to_string_lossy().into_owned());
    s.started_at = lean_ctx_time::UtcDateTime::now() - lean_ctx_time::Duration::hours(2);
    s.updated_at = lean_ctx_time::UtcDateTime::now() - lean_ctx_time::Duration::hours(1);
    s.save().unwrap();

    let cloud = tempfile::tempdir().unwrap();
    let (replica, _) =
        crate::replica::Replica::open(cloud.path(), "pl_episode_retry", &[3; 32]).unwrap();
    // Episodes are emitted for sessions that ended after the first run.
    replica
        .set_meta(
            EPISODES_SINCE,
            (now_ms - 3 * 3_600_000).to_string().as_bytes(),
        )
        .unwrap();
    let key = SigningKey::from_bytes(&[0x49; 32]);
    let place = Place {
        kind: "vault".into(),
        id: "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01".into(),
        pk_id: [9; 32],
        causal_base: 0,
    };
    let signer = Signer {
        key: &key,
        hlc_node: "6f2b9c4e-1a3d-4e5f-8a7b-0c1d2e3f4a5b".into(),
        worker_id: "wkr_vk4oelpv5lzjmev44hun".into(),
    };
    // A Cloud without `session_ref`: the body ref needs the local key.
    let target = super::super::emit::Target {
        replica: &replica,
        place: &place,
        signer: &signer,
        project_hash: "ph",
        decision_anchors: false,
        sessions: None,
    };
    let data = tempfile::tempdir().unwrap();
    let run = |clock: Stamp| {
        let mut screen =
            Screen::with_rules(Vec::new(), std::sync::Arc::new(Vec::new()), Vec::new());
        let mut run = super::super::CaptureRun::new(&mut screen, &root, [9; 32], Vec::new(), 0);
        emit_ended(&target, &root, data.path(), &mut run, clock, now_ms).unwrap()
    };
    let done = done_key(&crate::knowledge::draft::session_ref(ID));

    crate::session_ref::make_keyless(data.path());
    let withheld = run(Stamp::default());
    assert_eq!(withheld.queued, 0);
    let error = withheld.withheld.expect("withheld without the key");
    assert!(error.to_string().contains("receipts.hmac-key"), "{error}");
    assert!(replica.pending().unwrap().is_empty(), "nothing queued");
    assert!(replica.meta(&done).unwrap().is_none(), "not marked done");

    // The key is readable again: the retry emits the episode, keyed.
    std::fs::remove_dir(data.path().join("receipts.hmac-key")).unwrap();
    let retried = run(withheld.clock);
    assert!(retried.withheld.is_none(), "{:?}", retried.withheld);
    assert_eq!(retried.queued, 1);
    assert!(replica.meta(&done).unwrap().is_some());
    let pending = replica.pending().unwrap();
    assert_eq!(pending.len(), 1);
    let event: Value = serde_json::from_slice(&pending[0].1).unwrap();
    assert_eq!(
        event["body"]["session_ref"],
        crate::session_ref::uploaded_ses_ref(data.path(), ID).unwrap()
    );
    let wire = event.to_string();
    let unsalted = crate::knowledge::draft::session_ref(ID);
    assert!(
        !wire.contains(&unsalted[4..]),
        "unsalted ref leaves: {wire}"
    );

    // Once only: a further run adds no duplicate.
    let again = run(retried.clock);
    assert_eq!((again.queued, again.withheld.is_none()), (0, true));
    assert_eq!(replica.pending().unwrap().len(), 1, "no duplicate");
}

#[test]
fn dead_ends_are_kept_per_session_and_private() {
    let data = tempfile::tempdir().unwrap();
    record_dead_end(data.path(), "s1", "Suspected clock skew between workers").unwrap();
    record_dead_end(data.path(), "s1", "Suspected clock skew between workers").unwrap();
    record_dead_end(data.path(), "s1", "Tried a bigger retry budget").unwrap();
    record_dead_end(data.path(), "s2", "Other session").unwrap();
    assert_eq!(
        dead_ends(data.path(), "s1"),
        vec![
            "Suspected clock skew between workers".to_string(),
            "Tried a bigger retry budget".to_string()
        ]
    );
    assert!(dead_ends(data.path(), "s3").is_empty());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = data.path().join("capture/dead_ends");
        for entry in std::fs::read_dir(dir).unwrap() {
            let mode = entry.unwrap().metadata().unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "no group or world access");
        }
    }
}
