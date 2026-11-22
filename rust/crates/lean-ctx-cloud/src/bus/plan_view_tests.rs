// SPDX-License-Identifier: Apache-2.0

use super::*;
use lean_ctx::core::agents::DirectiveState;
use serde_json::{Value, json};

/// Two vaults as Cloud writes them into a plan document (32 hex).
const APP: &str = "3f1c2b6a8d4e4a7b9c105e2f6a7b8c01";
const WEB: &str = "aaaabbbbccccddddeeeeffff00001111";

/// The body of every test plan, as Cloud serves it, and its digest.
fn body() -> (String, String) {
    use base64::Engine as _;
    let bytes = lean_ctx_protocol::cloud::canonical_json(&json!({
        "title": "Refunds", "goal": "Ship refunds.",
        "sessions": [{"slot": "api", "role": "backend", "brief": "Build POST /refunds.", "folder": "api"}]
    }))
    .unwrap();
    (
        base64::engine::general_purpose::STANDARD.encode(&bytes),
        super::super::plan_body::sha256_of(&bytes),
    )
}

fn doc(plan_id: &str, vault: &str, version: u64) -> PlanDoc {
    let (body, digest) = body();
    serde_json::from_value(json!({
        "schema": "leanctx.cloud.plan/v1",
        "plan_id": plan_id,
        "vault": vault,
        "version": version,
        "content_sha256": digest,
        "issued_by": "per_01",
        "published_at": "2026-11-19T10:00:00Z",
        "body": body,
        "sessions": [{"slot": "api", "task_id": "t-7", "worker_id": "w-api",
                      "brief_sha256": super::super::plan_body::sha256_of(b"Build POST /refunds."),
                      "changed": true}]
    }))
    .expect("plan doc")
}

fn directive(
    agent: &str,
    plan_id: &str,
    version: u64,
    slot: &str,
    state: DirectiveState,
) -> Directive {
    Directive {
        directive_id: format!("{plan_id}:{version}:{slot}"),
        agent_id: agent.to_string(),
        project_root: "/work/app".to_string(),
        kind: DirectiveKind::Plan,
        subject: plan_id.to_string(),
        version,
        slot: Some(slot.to_string()),
        worker_id: None,
        task_id: None,
        vault_id: "vlt_1".to_string(),
        digest: body().1,
        message_id: format!("m-{agent}"),
        state,
        reason_code: (state == DirectiveState::Refused).then(|| "task_finished".to_string()),
        reason: None,
        reported: None,
        text: String::new(),
        release_sha256: (state == DirectiveState::Offered).then(|| "f".repeat(64)),
        release_id: (state == DirectiveState::Offered).then(|| RID.to_string()),
        released_at: matches!(state, DirectiveState::Accepted | DirectiveState::Refused)
            .then(|| format!("2026-11-19T10:0{version}:00Z")),
        brief_sha256: None,
        goal_sha256: None,
    }
}

/// A release id as the engine draws one.
const RID: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[test]
fn applied_versions_show_each_agents_answer_and_the_verified_text() {
    let docs = vec![doc("plan_AAAA1111", APP, 3), doc("plan_BBBB2222", WEB, 1)];
    let store = DirectiveStore {
        directives: vec![
            directive(
                "b-agent",
                "plan_AAAA1111",
                3,
                "all",
                DirectiveState::Refused,
            ),
            directive(
                "a-agent",
                "plan_AAAA1111",
                3,
                "api",
                DirectiveState::Accepted,
            ),
            directive(
                "a-agent",
                "plan_AAAA1111",
                2,
                "api",
                DirectiveState::Superseded,
            ),
            directive(
                "c-agent",
                "plan_AAAA1111",
                3,
                "api",
                DirectiveState::Superseded,
            ),
            directive(
                "d-agent",
                "plan_AAAA1111",
                3,
                "all",
                DirectiveState::Offered,
            ),
            directive(
                "a-agent",
                "plan_AAAA1111",
                1,
                "api",
                DirectiveState::Accepted,
            ),
        ],
    };
    let addresses = BTreeMap::from([(
        "3F1C2B6A-8D4E-4A7B-9C10-5E2F6A7B8C01".to_string(),
        "#acme/app".to_string(),
    )]);
    let all = updates(&docs, &store, &addresses, None, true);
    assert_eq!(all.len(), 2);
    let value = serde_json::to_value(&all[0]).unwrap();
    let text = |title: &str, goal: &str, part: bool, changed: bool| {
        json!({"plan_id": "plan_AAAA1111", "title": title, "goal": goal,
               "role": part.then_some("backend"), "brief": part.then_some("Build POST /refunds."),
               "folder": part.then_some("api"), "changed": changed})
    };
    let agent = |head: Value, title: &str, goal: &str, part: bool, changed: bool| {
        let mut entry = head;
        for (k, v) in text(title, goal, part, changed).as_object().unwrap() {
            entry[k] = v.clone();
        }
        entry
    };
    assert_eq!(
        value,
        json!({
            "id": "plan_AAAA1111", "vault": "#acme/app", "version": 3,
            "digest": body().1, "issued_by": "per_01",
            "verified": true,
            "published_at": "2026-11-19T10:00:00Z",
            "title": "Refunds", "goal": "Ship refunds.",
            "parts": [{"slot": "api", "role": "backend", "task_id": "t-7", "worker_id": "w-api",
                       "brief": "Build POST /refunds.", "folder": "api",
                       "brief_sha256": super::super::plan_body::sha256_of(b"Build POST /refunds."),
                       "changed": true}],
            "agents": [
                agent(json!({"agent_id": "a-agent", "version": 3, "directive_id": "plan_AAAA1111:3:api", "slot": "api",
                 "state": "acked", "release_id": null, "released_at": "2026-11-19T10:03:00Z",
                 "reason_code": null, "reason": null}), "Refunds", "Ship refunds.", true, true),
                agent(json!({"agent_id": "b-agent", "version": 3, "directive_id": "plan_AAAA1111:3:all", "slot": "all",
                 "state": "refused", "release_id": null, "released_at": "2026-11-19T10:03:00Z",
                 "reason_code": "task_finished", "reason": null}), "Refunds", "Ship refunds.", false, false),
                agent(json!({"agent_id": "d-agent", "version": 3, "directive_id": "plan_AAAA1111:3:all", "slot": "all",
                 "state": "offered", "release_id": RID, "released_at": null,
                 "reason_code": null, "reason": null}), "Refunds", "Ship refunds.", false, false)
            ],
            "body": {"schema": "leanctx.plan-body/v1", "title": "Refunds", "goal": "Ship refunds.",
                     "sessions": [{"slot": "api", "role": "backend",
                                   "brief": "Build POST /refunds.", "folder": "api"}]},
            "body_error": null,
            "releases": [
                {"version": 1, "agent_id": "a-agent", "directive_id": "plan_AAAA1111:1:api",
                 "released_at": "2026-11-19T10:01:00Z"},
                {"version": 3, "agent_id": "a-agent", "directive_id": "plan_AAAA1111:3:api",
                 "released_at": "2026-11-19T10:03:00Z"},
                {"version": 3, "agent_id": "b-agent", "directive_id": "plan_AAAA1111:3:all",
                 "released_at": "2026-11-19T10:03:00Z"}
            ]
        })
    );
    assert!(all[1].agents.is_empty());
    // Only the offered directive is an offer, with what Code releases.
    assert_eq!(
        serde_json::to_value(offers(&all)).unwrap(),
        json!([{"release_id": RID, "plan_id": "plan_AAAA1111", "version": 3, "slot": "all",
                "agent_id": "d-agent", "title": "Refunds", "goal": "Ship refunds.",
                "role": null, "brief": null, "folder": null, "changed": false}])
    );
    // A reader without text gets the same document with every text null.
    let bare = serde_json::to_value(&updates(&docs, &store, &addresses, None, false)[0]).unwrap();
    assert_eq!(bare["title"], Value::Null);
    assert_eq!(
        (&bare["body"], &bare["body_error"]),
        (&Value::Null, &Value::Null)
    );
    assert_eq!(bare["parts"][0]["brief"], Value::Null);
    for agent in bare["agents"].as_array().unwrap() {
        for field in ["title", "goal", "role", "brief", "folder"] {
            assert_eq!(agent[field], Value::Null, "{field}");
        }
    }
    assert!(!bare.to_string().contains("refunds."), "{bare}");
    // A vault this instance does not name keeps the document's vault id.
    assert_eq!(all[1].vault, WEB);
    let one = updates(
        &docs,
        &store,
        &addresses,
        Some("aaaabbbb-cccc-dddd-eeee-ffff00001111"),
        false,
    );
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].id, "plan_BBBB2222");
}

#[test]
fn a_kept_body_that_does_not_open_shows_why_and_no_text() {
    use base64::Engine as _;
    let store = DirectiveStore::default();
    let none = BTreeMap::new();
    let mut changed = doc("plan_AAAA1111", APP, 3);
    changed.body = base64::engine::general_purpose::STANDARD.encode(b"{\"title\":\"Other\"}");
    let mut broken = doc("plan_BBBB2222", WEB, 1);
    broken.body = "%%%".to_string();
    let mut foreign = doc("plan_CCCC3333", WEB, 1);
    let bytes = br#"{"title":"T","goal":"G","sessions":[],"extra":1}"#;
    foreign.body = base64::engine::general_purpose::STANDARD.encode(bytes);
    foreign.content_sha256 = super::super::plan_body::sha256_of(bytes);
    let seen = updates(&[changed, broken, foreign], &store, &none, None, true);
    let why: Vec<_> = seen
        .iter()
        .map(|p| (p.body.is_none(), p.body_error, p.verified))
        .collect();
    // Only the body that still hashes to its digest stays verified.
    assert_eq!(
        why,
        [
            (true, Some("digest_mismatch"), false),
            (true, Some("invalid"), false),
            (true, Some("invalid"), true)
        ]
    );
    assert!(seen.iter().all(|p| p.title.is_none() && p.goal.is_none()));
}

#[test]
fn kept_plans_are_read_sorted_and_unparsable_files_left_out() {
    let dir = tempfile::tempdir().unwrap();
    let plans = dir.path().join("plans");
    std::fs::create_dir_all(&plans).unwrap();
    for (name, d) in [
        ("plan_BBBB2222", doc("plan_BBBB2222", "#acme/web", 1)),
        ("plan_AAAA1111", doc("plan_AAAA1111", "#acme/app", 3)),
    ] {
        std::fs::write(
            plans.join(format!("{name}.json")),
            serde_json::to_string(&d).unwrap(),
        )
        .unwrap();
    }
    std::fs::write(plans.join("broken.json"), "{").unwrap();
    let kept = kept_plans(dir.path());
    let ids: Vec<&str> = kept.iter().map(|d| d.plan_id.as_str()).collect();
    assert_eq!(ids, ["plan_AAAA1111", "plan_BBBB2222"]);
    assert!(kept_plans(&dir.path().join("none")).is_empty());
}
