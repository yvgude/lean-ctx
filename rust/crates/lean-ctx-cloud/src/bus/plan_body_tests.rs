// SPDX-License-Identifier: Apache-2.0

use super::*;

const KEY: &str = "6f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01";

fn input(body: &Value) -> String {
    json!({"idempotency_key": KEY, "plan_id": "plan_K7mQ2xVb9Z", "supersedes_version": 2, "body": body})
        .to_string()
}

fn body() -> Value {
    json!({
        "title": "Refunds",
        "goal": "Ship refunds for card payments.",
        "sessions": [
            {"slot": "api", "role": "backend", "brief": "Build POST /refunds.",
             "folder": "api", "task_id": "t-7", "worker_id": "w-api"},
            {"slot": "web", "role": "frontend", "brief": "Add the refund button."}
        ]
    })
}

#[test]
fn a_publish_carries_the_body_in_schema_order_its_digest_and_text_free_routing() {
    let publish = publish_request(&input(&body())).expect("publish");
    assert_eq!(publish.idempotency_key, KEY);
    let request = &publish.request;
    assert_eq!(request["schema"], PLAN_PUBLISH_SCHEMA);
    assert_eq!(request["plan_id"], "plan_K7mQ2xVb9Z");
    assert_eq!(request["supersedes_version"], 2);
    let bytes = STANDARD
        .decode(request["body"].as_str().unwrap())
        .expect("base64 with padding");
    assert_eq!(request["content_sha256"], sha256_of(&bytes));
    assert_eq!(publish.content_sha256, sha256_of(&bytes));
    // Schema order, no whitespace, the schema named, the routing taken out.
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(
        text.starts_with(
            r#"{"schema":"leanctx.plan-body/v1","title":"Refunds","goal":"Ship refunds"#
        ),
        "{text}"
    );
    assert!(!text.contains("w-api") && !text.contains("t-7"), "{text}");
    let opened = PlanBody::parse(&bytes).expect("a plan body");
    assert_eq!(opened.sessions[1].folder, None);
    assert_eq!(
        request["sessions"],
        json!([
            {"slot": "api", "task_id": "t-7", "worker_id": "w-api",
             "brief_sha256": sha256_of(b"Build POST /refunds.")},
            {"slot": "web", "task_id": null, "worker_id": null,
             "brief_sha256": sha256_of(b"Add the refund button.")}
        ])
    );
    // The same body in another key order publishes the same bytes.
    let reordered = json!({
        "sessions": body()["sessions"].clone(),
        "goal": "Ship refunds for card payments.",
        "title": "Refunds",
    });
    assert_eq!(
        publish_request(&input(&reordered)).unwrap().request["body"],
        request["body"]
    );
    // A first version names no plan and no superseded version.
    let first = publish_request(&json!({"idempotency_key": KEY, "body": body()}).to_string())
        .expect("first version");
    assert_eq!(first.request["plan_id"], Value::Null);
    assert_eq!(first.request["supersedes_version"], Value::Null);
    // A body that names another schema is no plan body.
    let mut other = body();
    other["schema"] = json!("leanctx.plan-body/v2");
    assert!(publish_request(&input(&other)).is_err());
}

#[test]
fn a_publish_refuses_what_the_schema_does_not_take() {
    let with = |change: &dyn Fn(&mut Value)| {
        let mut request = json!({"idempotency_key": KEY, "body": body()});
        change(&mut request);
        request.to_string()
    };
    for (what, text) in [
        ("empty", String::new()),
        ("unknown field", with(&|r| r["title"] = json!("t"))),
        ("no key", with(&|r| r["idempotency_key"] = json!("k-1"))),
        ("bad plan id", with(&|r| r["plan_id"] = json!("plan_x"))),
        (
            "version 0",
            with(&|r| {
                r["plan_id"] = json!("plan_K7mQ2xVb9Z");
                r["supersedes_version"] = json!(0);
            }),
        ),
        (
            "supersedes without plan",
            with(&|r| r["supersedes_version"] = json!(2)),
        ),
        ("body field", with(&|r| r["body"]["owner"] = json!("x"))),
        (
            "session field",
            with(&|r| r["body"]["sessions"][0]["notes"] = json!("x")),
        ),
        (
            "slot twice",
            with(&|r| r["body"]["sessions"][1]["slot"] = json!("api")),
        ),
        (
            "slot all",
            with(&|r| r["body"]["sessions"][1]["slot"] = json!("all")),
        ),
        (
            "slot case",
            with(&|r| r["body"]["sessions"][1]["slot"] = json!("Web")),
        ),
        (
            "long title",
            with(&|r| r["body"]["title"] = json!("t".repeat(201))),
        ),
        (
            "long role",
            with(&|r| r["body"]["sessions"][0]["role"] = json!("r".repeat(65))),
        ),
        (
            "long brief",
            with(&|r| r["body"]["sessions"][0]["brief"] = json!("b".repeat(8001))),
        ),
        ("blank goal", with(&|r| r["body"]["goal"] = json!("  "))),
        (
            "bidi",
            with(&|r| r["body"]["goal"] = json!("pay\u{202e}out")),
        ),
        (
            "control",
            with(&|r| r["body"]["sessions"][0]["brief"] = json!("a\u{1b}[2J")),
        ),
        (
            "decision",
            with(&|r| r["body"]["decisions"] = json!(["dec_1"])),
        ),
        (
            "bad worker",
            with(&|r| r["body"]["sessions"][0]["worker_id"] = json!("w api")),
        ),
        (
            "33 sessions",
            with(&|r| {
                r["body"]["sessions"] = Value::Array(
                    (0..33)
                        .map(|n| json!({"slot": format!("s{n}"), "role": "r", "brief": "b"}))
                        .collect(),
                );
            }),
        ),
    ] {
        assert!(publish_request(&text).is_err(), "{what}");
    }
    // A body that would not be UTF-8 (a lone surrogate escape) is refused
    // before anything is sent; Cloud refuses such a body too.
    let lone = input(&body()).replacen("\"goal\":\"", "\"goal\":\"\\ud800", 1);
    assert!(lone.contains("\\ud800"), "{lone}");
    assert!(publish_request(&lone).is_err());
    // The limits themselves are taken; decisions match the schema pattern.
    let edge = with(&|r| {
        r["body"]["title"] = json!("t".repeat(200));
        r["body"]["goal"] = json!("é".repeat(4000));
        r["body"]["sessions"][0]["brief"] = json!("b".repeat(8000));
        r["body"]["decisions"] = json!(["dec-0042", "dec-Use-PG-16"]);
    });
    publish_request(&edge).expect("within the limits");
}

#[test]
fn clouds_answers_map_to_the_cli_answer() {
    let publish = publish_request(&input(&body())).unwrap();
    let published = json!({
        "schema": PLAN_PUBLISHED_SCHEMA, "plan_id": "plan_K7mQ2xVb9Z", "vault": "a".repeat(32),
        "version": 3, "supersedes_version": 2, "content_sha256": publish.content_sha256,
        "issued_by": "per_01", "published_at": "2026-11-20T10:00:00Z", "sessions": []
    });
    assert_eq!(
        publish_answer(&publish, 201, &published),
        Ok(Published {
            plan_id: "plan_K7mQ2xVb9Z".to_owned(),
            version: 3,
            content_sha256: publish.content_sha256.clone(),
        })
    );
    let mut other = published.clone();
    other["content_sha256"] = json!(format!("sha256:{}", "0".repeat(64)));
    assert_eq!(
        publish_answer(&publish, 201, &other).unwrap_err().code,
        "protocol"
    );
    let stale = publish_answer(
        &publish,
        409,
        &json!({"error": {"code": "stale_version", "message": "m", "current_version": 5}}),
    )
    .unwrap_err();
    assert_eq!(stale.code, "stale_version");
    assert_eq!(stale.current_version, Some(5));
    for (status, code) in [
        (409, "plan_exists"),
        (409, "not_ready"),
        (409, "idempotency_key_reused"),
        (404, "not_found"),
        (400, "invalid"),
    ] {
        let refused = publish_answer(
            &publish,
            status,
            &json!({"error": {"code": code, "message": "m"}}),
        )
        .unwrap_err();
        assert_eq!(refused.code, code);
        assert_eq!(refused.current_version, None);
    }
    assert_eq!(
        publish_answer(&publish, 403, &Value::Null)
            .unwrap_err()
            .code,
        "forbidden"
    );
    assert_eq!(
        publish_answer(&publish, 429, &Value::Null)
            .unwrap_err()
            .code,
        "rate_limited"
    );
}
