// SPDX-License-Identifier: Apache-2.0

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::propose::{ProposalKind, QueuedProposal};
use super::*;
use crate::core::hive::TaskRefusal;
use lean_ctx_protocol::hive::tasks::TaskState;

const PROJECT: &str = "3f6c1e9a0b7d4c2e8f5a1b6c9d0e7f42";
const AWS_KEY: &str = concat!("AKIA", "IOSFODNN7EXAMPLE");

type Handler = dyn Fn(&str, &str, &Value) -> (u16, Value) + Send + Sync;

/// A scripted Hive on 127.0.0.1: every request is recorded, every answer
/// comes from `handler`.
struct Fake {
    origin: String,
    requests: Arc<Mutex<Vec<(String, String, Value)>>>,
}

impl Fake {
    fn start(handler: impl Fn(&str, &str, &Value) -> (u16, Value) + Send + Sync + 'static) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let handler: Arc<Handler> = Arc::new(handler);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let recorded = Arc::clone(&recorded);
                let handler = Arc::clone(&handler);
                std::thread::spawn(move || serve(stream, &recorded, handler.as_ref()));
            }
        });
        Self { origin, requests }
    }

    fn requests(&self) -> Vec<(String, String, Value)> {
        self.requests.lock().unwrap().clone()
    }
}

fn serve(
    stream: std::net::TcpStream,
    recorded: &Mutex<Vec<(String, String, Value)>>,
    handler: &Handler,
) {
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut parts = line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_owned();
        let path = parts.next().unwrap_or_default().to_owned();
        let mut length = 0;
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).unwrap();
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                length = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let path = path.strip_prefix("/v1").unwrap_or(&path).to_owned();
        let (status, answer) = handler(&method, &path, &body);
        recorded.lock().unwrap().push((method, path, body));
        let text = answer.to_string();
        let _ = write!(
            writer,
            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{text}",
            text.len()
        );
    }
}

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/crates/lean-ctx-protocol/tests/fixtures/hive/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn stamp(minutes: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::minutes(minutes))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn ack(number: u64, state: &str) -> Value {
    json!({"schema": "leanctx.hive.task-ack/v1", "number": number, "state": state})
}

fn claim_ack(number: u64, token: u64, minutes: i64) -> Value {
    json!({"schema": "leanctx.hive.task-claim/v1", "number": number,
        "claim": {"lease_id": "lse_01jbtask7", "fencing_token": token}, "until": stamp(minutes)})
}

/// Hive 1.x (prod until 3.12.0) for task 7: claim, renew (same token, as
/// the service keeps it), updates, result held for a decision.
fn hive_for_task_7() -> Fake {
    hive_for_task_7_on(false)
}

/// Hive for task 7; `v5`: task views carry `lifecycle`, a result ends the
/// task (`done`, or `failed` with `outcome: failed`), and a request with a
/// field 1.x does not know is refused there like 1.x would (unknown fields
/// are denied).
fn hive_for_task_7_on(v5: bool) -> Fake {
    Fake::start(move |method, path, body| {
        let tasks = format!("/vaults/{PROJECT}/tasks");
        let tail = path.strip_prefix(&tasks).unwrap_or(path);
        match (method, tail) {
            ("POST", "/7/claim") => (200, claim_ack(7, 7, 10)),
            ("POST", "/7/claim/renew") => {
                let token = body["claim"]["fencing_token"].as_u64().unwrap();
                (200, claim_ack(7, token, 10))
            }
            ("POST", "/7/updates") => (200, ack(7, "claimed")),
            ("POST", "/7/result") if !v5 && body.get("outcome").is_some() => (
                400,
                json!({"error": {"code": "invalid_request", "message": "unknown field `outcome`"}}),
            ),
            ("POST", "/7/result") if !v5 => (200, ack(7, "submitted")),
            ("POST", "/7/result") if body["outcome"] == "failed" => (200, ack(7, "failed")),
            ("POST", "/7/result") => (200, ack(7, "done")),
            ("POST", "/7/release") => (200, ack(7, "open")),
            ("GET", "/7") if v5 => {
                let mut task = fixture("task.claimed.response.json");
                task["lifecycle"] = "running".into();
                (200, task)
            }
            ("GET", "/7") => (200, fixture("task.claimed.response.json")),
            ("GET", "?state=open" | "") => (200, fixture("task_list.response.json")),
            _ => (
                404,
                json!({"error": {"code": "not_found", "message": "no"}}),
            ),
        }
    })
}

fn optin(vaults: &[&str], active: bool, tasks: u32) -> crate::core::hive::worker::Optin {
    serde_json::from_value(json!({
        "schema": lean_ctx_protocol::hive::optin::ENGINE_OPTIN_SCHEMA,
        "optin_id": "opt_1", "active": active, "vaults": vaults, "networks": [], "topics": [],
        "daily_budget": {"tasks": tasks, "tokens": 100_000},
        "used_today": {"day": "2026-10-22", "tasks": 0, "tokens": 0, "usd_micros": 0},
        "updated_at": "2026-10-22T10:00:00Z"
    }))
    .unwrap()
}

/// The worker gate under an opt-in for this project (L1 decides).
fn gate(
    optin: crate::core::hive::worker::Optin,
    state: crate::core::hive::worker::WorkerState,
) -> impl Fn(&TaskScope) -> Result<Admission, Refusal> {
    move |scope| crate::core::hive::worker::decide_claim(&optin, &Default::default(), &state, scope)
}

fn admitted(scope: &TaskScope) -> Result<Admission, Refusal> {
    gate(optin(&[PROJECT], true, 5), Default::default())(scope)
}

fn submitted(_: &TaskScope, _: &EvidenceSummary) -> Result<(), Refusal> {
    Ok(())
}

/// A completed result, as before v5.
fn submit_as(
    link: &Link<'_>,
    number: u64,
    agent: &str,
    summary: &str,
    evidence: Vec<EvidenceInput>,
    usage: Option<UsageV1>,
    gate: SubmitGate<'_>,
) -> Result<TaskAckV1, HiveError> {
    submit_via(
        link,
        number,
        agent,
        Submission {
            summary,
            evidence,
            usage,
            outcome: TaskOutcome::Completed,
        },
        gate,
    )
}

fn submit_failed(link: &Link<'_>, number: u64) -> Result<TaskAckV1, HiveError> {
    submit_via(
        link,
        number,
        "claude-3e",
        Submission {
            summary: "The budget cannot be found in this repo",
            evidence: vec![commit()],
            usage: None,
            outcome: TaskOutcome::Failed,
        },
        &submitted,
    )
}

struct Place {
    data: tempfile::TempDir,
    root: tempfile::TempDir,
}

impl Place {
    fn new() -> Self {
        Self {
            data: tempfile::tempdir().unwrap(),
            root: tempfile::tempdir().unwrap(),
        }
    }

    fn link(&self, fake: &Fake) -> Link<'_> {
        Link {
            data_dir: self.data.path(),
            vault_id: PROJECT,
            sealed: false,
            root: self.root.path(),
            via: Via::Origin(fake.origin.clone()),
            budget: BUDGET,
            deadline: None,
        }
    }
}

fn commit() -> EvidenceInput {
    EvidenceInput {
        kind: "commit".into(),
        reference: "9f01cf32a3".into(),
    }
}

/// Claim → update → renew → submit: every write carries the current claim,
/// the claim lives only in the 0600 cache, and the result ends it.
#[test]
fn a_claimed_task_is_updated_renewed_and_submitted_under_its_claim() {
    let fake = hive_for_task_7();
    let place = Place::new();
    let link = place.link(&fake);

    let granted = claim_via(&link, 7, "claude-3e", "session-1", Some(30), &admitted).unwrap();
    assert_eq!((granted.number, granted.minutes), (7, MAX_CLAIM_MINUTES));
    let held = claims::find(link.data_dir, PROJECT, 7).expect("claim recorded");
    assert_eq!(held.claim.fencing_token, 7);
    assert_eq!(held.pid, std::process::id());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(place.data.path().join("task-claims.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "the claim cache is private");
    }
    // The holder claiming again renews the claim: a fresh expiry, same token.
    let again = claim_via(&link, 7, "claude-3e", "session-1", None, &admitted).unwrap();
    assert_eq!(again.minutes, MAX_CLAIM_MINUTES);
    assert_eq!(fake.requests().len(), 2);

    update_via(&link, 7, "claude-3e", "Found the budget constant", None).unwrap();
    let renewed = renew_via(&link, 7, "claude-3e").unwrap();
    assert_eq!(renewed.claim.fencing_token, 7, "a renewal keeps the token");
    let usage = UsageV1 {
        tokens: 1200,
        usd_micros: None,
    };
    let ack = submit_as(
        &link,
        7,
        "claude-3e",
        "Budget is 3 retries",
        vec![commit()],
        Some(usage),
        &submitted,
    )
    .unwrap();
    assert_eq!(
        ack.state,
        lean_ctx_protocol::hive::tasks::TaskState::Submitted
    );
    assert!(
        claims::find(link.data_dir, PROJECT, 7).is_none(),
        "a result ends the claim"
    );

    let requests = fake.requests();
    let paths: Vec<&str> = requests.iter().map(|(_, path, _)| path.as_str()).collect();
    let tasks = format!("/vaults/{PROJECT}/tasks");
    assert_eq!(
        paths,
        [
            format!("{tasks}/7/claim"),
            format!("{tasks}/7/claim/renew"),
            format!("{tasks}/7/updates"),
            format!("{tasks}/7/claim/renew"),
            format!("{tasks}/7/result"),
        ]
    );
    let claim = &requests[0].2;
    assert_eq!(claim["agent"], "claude-3e");
    assert_eq!(claim["minutes"], 15);
    assert!(claim["session_ref"].as_str().unwrap().starts_with("ses_"));
    assert_eq!(requests[1].2["claim"]["fencing_token"], 7);
    assert_eq!(requests[2].2["claim"]["fencing_token"], 7);
    assert_eq!(requests[2].2["summary"], "Found the budget constant");
    assert_eq!(requests[4].2["claim"]["fencing_token"], 7);
    assert_eq!(
        requests[4].2["evidence"],
        json!([{"kind": "commit", "reference": "9f01cf32a3"}])
    );
    assert_eq!(requests[4].2["usage"]["tokens"], 1200);
    // Prod 1.x: the result carries no v5 field and no draft, and the
    // answer reads as a result waiting for its decision.
    assert!(requests[4].2.get("outcome").is_none());
    assert!(requests[4].2.get("draft").is_none());
    assert_eq!(
        render_result_ack(&ack),
        "Result submitted on task #7; a person or the review threshold decides on it."
    );
}

/// Hive v5: a result ends the task at once (`done`), with the same
/// request a 1.x Hive gets; knowledge never rides along as a draft.
#[test]
fn a_v5_result_completes_the_task_with_the_1x_request() {
    let fake = hive_for_task_7_on(true);
    let place = Place::new();
    let link = place.link(&fake);
    claim_via(&link, 7, "claude-3e", "s", None, &admitted).unwrap();
    let ack = submit_as(
        &link,
        7,
        "claude-3e",
        "Budget is 3 retries",
        vec![commit()],
        None,
        &submitted,
    )
    .unwrap();
    assert_eq!(ack.state, TaskState::Done);
    assert_eq!(
        render_result_ack(&ack),
        "Task #7 completed with this result."
    );
    let requests = fake.requests();
    let result = &requests.last().unwrap().2;
    assert!(result.get("outcome").is_none() && result.get("draft").is_none());
    assert_eq!(
        requests
            .iter()
            .filter(|(method, _, _)| method == "GET")
            .count(),
        0,
        "a completed result asks nothing first"
    );
    assert!(claims::find(link.data_dir, PROJECT, 7).is_none());
}

/// A failed result goes out as `outcome: failed` only to a Hive whose
/// task view carries the v5 `lifecycle`; it ends the task as failed.
#[test]
fn a_failed_result_is_sent_to_a_v5_hive() {
    let fake = hive_for_task_7_on(true);
    let place = Place::new();
    let link = place.link(&fake);
    claim_via(&link, 7, "claude-3e", "s", None, &admitted).unwrap();
    let ack = submit_failed(&link, 7).unwrap();
    assert_eq!(ack.state, TaskState::Failed);
    assert_eq!(
        render_result_ack(&ack),
        "Task #7 ended as failed with this result."
    );
    let requests = fake.requests();
    let (task, result_path) = (
        format!("/vaults/{PROJECT}/tasks/7"),
        format!("/vaults/{PROJECT}/tasks/7/result"),
    );
    let tail: Vec<(&str, &str)> = requests[1..]
        .iter()
        .map(|(method, path, _)| (method.as_str(), path.as_str()))
        .collect();
    assert_eq!(
        tail,
        [("GET", task.as_str()), ("POST", result_path.as_str())]
    );
    let result = &requests[2].2;
    assert_eq!(result["outcome"], "failed");
    assert!(result.get("draft").is_none());
    assert!(claims::find(link.data_dir, PROJECT, 7).is_none());
}

/// Prod 1.x knows no failed results (and denies the field): the failure is
/// refused here, nothing is posted, and the claim stays to be released.
#[test]
fn a_failed_result_is_refused_before_a_1x_hive() {
    let fake = hive_for_task_7();
    let place = Place::new();
    let link = place.link(&fake);
    claim_via(&link, 7, "claude-3e", "s", None, &admitted).unwrap();
    let refused = submit_failed(&link, 7).unwrap_err();
    assert!(
        matches!(&refused, HiveError::Refused(why) if why.contains("release task #7")),
        "{refused:?}"
    );
    assert!(
        fake.requests()
            .iter()
            .all(|(method, path, _)| method != "POST" || !path.ends_with("/result")),
        "no result was posted"
    );
    assert!(claims::find(link.data_dir, PROJECT, 7).is_some());
}

/// RB4: the worker gate decides before anything leaves. Without an opt-in,
/// outside its repos, over today's budget, after a stop or while paused,
/// no claim is requested and nothing is held; the refusal says why.
#[test]
fn the_worker_gate_refuses_before_any_claim_is_requested() {
    use crate::core::hive::worker::{Usage, WorkerState};
    let fake = hive_for_task_7();
    let place = Place::new();
    let link = place.link(&fake);
    let mut none = optin(&[PROJECT], true, 5);
    none.optin_id = None;
    let paused = WorkerState {
        paused: true,
        ..Default::default()
    };
    let spent = WorkerState {
        used: Usage {
            tasks: 2,
            ..Default::default()
        },
        ..Default::default()
    };
    for (case, gate, says) in [
        (
            "no opt-in",
            gate(none, Default::default()),
            "not enabled for this task",
        ),
        (
            "outside scope",
            gate(
                optin(&["0000000000000000000000000000000f"], true, 5),
                Default::default(),
            ),
            "not enabled for this task",
        ),
        (
            "over budget",
            gate(optin(&[PROJECT], true, 2), spent),
            "budget is used up",
        ),
        (
            "stopped",
            gate(optin(&[PROJECT], false, 5), Default::default()),
            "was stopped",
        ),
        (
            "paused",
            gate(optin(&[PROJECT], true, 5), paused),
            "is paused",
        ),
    ] {
        let refused = claim_via(&link, 7, "claude-3e", "s", None, &gate).unwrap_err();
        assert!(
            matches!(&refused, HiveError::Refused(why) if why.contains(says)),
            "{case}: {refused:?}"
        );
    }
    assert!(fake.requests().is_empty(), "no claim left the machine");
    assert!(claims::all(place.data.path()).is_empty());

    // A result the gate refuses is not sent and keeps the claim.
    claim_via(&link, 7, "claude-3e", "s", None, &admitted).unwrap();
    let refuse = |_: &TaskScope, _: &EvidenceSummary| Err(Refusal::Paused);
    assert!(submit_as(&link, 7, "claude-3e", "Done", vec![commit()], None, &refuse).is_err());
    assert_eq!(fake.requests().len(), 1, "only the claim was sent");
    assert!(claims::find(link.data_dir, PROJECT, 7).is_some());
}

/// Hive admits claims: without an opt-in, or with the budget used up, the
/// refusal is shown as it is and nothing is held.
#[test]
fn a_claim_hive_does_not_admit_is_not_held() {
    let fake = Fake::start(|_, path, _| {
        if path.contains("/8/") {
            (
                429,
                json!({"error": {"code": "rate_limited", "message": "x", "reason": "daily_budget", "number": 8, "retry_after_s": 3600}}),
            )
        } else {
            (
                403,
                json!({"error": {"code": "forbidden_right", "message": "x", "reason": "optin_scope", "number": 7}}),
            )
        }
    });
    let place = Place::new();
    let link = place.link(&fake);
    let error = claim_via(&link, 7, "claude-3e", "s", None, &admitted).unwrap_err();
    assert_eq!(
        error.to_string(),
        "task #7: this engine's opt-in does not cover this vault"
    );
    let budget = claim_via(&link, 8, "claude-3e", "s", None, &admitted).unwrap_err();
    assert_eq!(
        budget.to_string(),
        "task #8: today's task budget is used up (resets in 3600 s)"
    );
    assert_eq!(
        fake.requests()[0].2["optin_id"],
        "opt_1",
        "the claim names its opt-in"
    );
    assert!(claims::all(place.data.path()).is_empty());
}

/// A stale claim is forgotten; a refused claim names the task and reason.
#[test]
fn hive_refusals_end_or_keep_the_local_claim() {
    let fake = Fake::start(|_, path, _| match path.rsplit('/').next() {
        Some("claim") if path.contains("/8/") => (409, fixture("error.task_claimed.json")),
        Some("claim") => (200, claim_ack(7, 7, 10)),
        _ => (
            409,
            json!({"error": {"code": "invalid_state", "message": "x", "reason": "stale_claim", "number": 7}}),
        ),
    });
    let place = Place::new();
    let link = place.link(&fake);
    let refused = claim_via(&link, 8, "claude-3e", "s", None, &admitted).unwrap_err();
    assert_eq!(
        refused,
        HiveError::Task {
            refusal: TaskRefusal::Claimed,
            number: Some(7),
            detail: None
        }
    );
    claim_via(&link, 7, "claude-3e", "s", None, &admitted).unwrap();
    let stale = update_via(&link, 7, "claude-3e", "Still working on it", None).unwrap_err();
    assert!(matches!(
        stale,
        HiveError::Task {
            refusal: TaskRefusal::StaleClaim,
            ..
        }
    ));
    assert!(claims::find(link.data_dir, PROJECT, 7).is_none());
    assert_eq!(
        update_via(&link, 7, "claude-3e", "again", None).unwrap_err(),
        HiveError::Refused("this machine holds no claim on task #7".into())
    );
}

/// 24.10. (SERVER-2): a claim that lapsed while its agent worked made the
/// next update fail with "no longer the task's claim". The update claims
/// the task again and is sent once more; a refused re-claim is the answer.
#[test]
fn an_update_after_a_lapsed_claim_claims_again() {
    let stale = std::sync::atomic::AtomicBool::new(true);
    let fake = Fake::start(move |_, path, _| match path.rsplit('/').next() {
        Some("claim") => (200, claim_ack(7, 8, 10)),
        Some("updates") if stale.swap(false, std::sync::atomic::Ordering::SeqCst) => (
            409,
            json!({"error": {"code": "invalid_state", "message": "x", "reason": "stale_claim", "number": 7}}),
        ),
        Some("updates") => (200, ack(7, "claimed")),
        _ => (
            404,
            json!({"error": {"code": "not_found", "message": "no"}}),
        ),
    });
    let place = Place::new();
    let link = place.link(&fake);
    claim_via(&link, 7, "claude-3e", "s", None, &admitted).unwrap();
    let reclaimed = std::cell::Cell::new(false);
    let ack = update_or_reclaim_via(&link, 7, "claude-3e", "Still on it", None, || {
        reclaimed.set(true);
        claim_via(&link, 7, "claude-3e", "s", None, &admitted)
    })
    .unwrap();
    assert_eq!(ack.number, 7);
    assert!(reclaimed.get());
    let tokens: Vec<_> = fake
        .requests()
        .iter()
        .filter(|(_, path, _)| path.ends_with("/updates"))
        .map(|(_, _, body)| body["claim"]["fencing_token"].clone())
        .collect();
    assert_eq!(tokens, [json!(8), json!(8)]);

    let live = update_or_reclaim_via(&link, 7, "claude-3e", "x", None, || {
        Err(HiveError::Refused("taken".into()))
    });
    assert!(live.is_ok(), "a live claim never re-claims: {live:?}");
}

/// A summary or evidence the screening refuses is never sent; a file
/// anchor names only an admitted file that exists.
#[test]
fn outgoing_text_is_screened_and_anchors_name_admitted_files_only() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let fake = hive_for_task_7();
    let place = Place::new();
    let link = place.link(&fake);
    claim_via(&link, 7, "claude-3e", "s", None, &admitted).unwrap();
    let sent = fake.requests().len();
    let secret = update_via(
        &link,
        7,
        "claude-3e",
        &format!("Deploy with {AWS_KEY}"),
        None,
    )
    .unwrap_err();
    assert!(
        matches!(&secret, HiveError::Refused(why) if why.starts_with("the summary was not sent") && !why.contains(AWS_KEY)),
        "{secret:?}"
    );
    std::fs::write(place.root.path().join(".env"), "TOKEN=1\n").unwrap();
    let anchor = |reference: &str| EvidenceInput {
        kind: "file_anchor".into(),
        reference: reference.into(),
    };
    for refused in [".env", "src/missing.rs", "../outside.rs"] {
        assert!(
            matches!(
                submit_as(
                    &link,
                    7,
                    "claude-3e",
                    "Done",
                    vec![anchor(refused)],
                    None,
                    &submitted
                ),
                Err(HiveError::Refused(_))
            ),
            "{refused}"
        );
    }
    assert_eq!(fake.requests().len(), sent, "nothing left the machine");
    assert!(claims::find(link.data_dir, PROJECT, 7).is_some());

    std::fs::create_dir_all(place.root.path().join("src")).unwrap();
    std::fs::write(
        place.root.path().join("src/retry.rs"),
        "pub const BUDGET: u32 = 3;\n",
    )
    .unwrap();
    submit_as(
        &link,
        7,
        "claude-3e",
        "Budget is 3",
        vec![anchor("src/retry.rs")],
        None,
        &submitted,
    )
    .unwrap();
    let requests = fake.requests();
    let result = &requests.last().unwrap().2;
    assert_eq!(result["evidence"][0]["kind"], "file_anchor");
    assert_eq!(result["evidence"][0]["anchor"]["path"], "src/retry.rs");
    assert!(
        result["evidence"][0]["anchor"]["digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert!(claims::find(link.data_dir, PROJECT, 7).is_none());
}

#[test]
fn a_claim_is_released_with_its_reason() {
    let fake = hive_for_task_7();
    let place = Place::new();
    let link = place.link(&fake);
    claim_via(&link, 7, "claude-3e", "s", None, &admitted).unwrap();
    release_via(&link, 7, "claude-3e", Some("Out of scope")).unwrap();
    assert!(claims::find(link.data_dir, PROJECT, 7).is_none());
    assert_eq!(fake.requests().last().unwrap().2["reason"], "Out of scope");
}

/// What Hive sends reaches the agent fenced as foreign data (X5), one
/// complete wrapper per block, byte-stable for the same answer.
#[test]
fn listed_and_shown_tasks_reach_the_agent_fenced() {
    let fake = hive_for_task_7();
    let place = Place::new();
    let link = place.link(&fake);
    let list = list_via(&link, &ListFilter::default()).unwrap();
    let text = render_list(&list, 1);
    let title = "Explain the retry budget";
    let fence = text.find("<<foreign-data kind=task_body").expect("fenced");
    assert!(text.find(title).unwrap() > fence);
    assert!(text.contains("… 1 more"));
    assert!(!text.contains("Settle the retry budget"), "limit holds");
    assert_eq!(text, render_list(&list, 1), "deterministic");

    let view = show_via(&link, 7).unwrap();
    let shown = render_task(&view);
    assert!(shown.starts_with("Hive task #7 — claimed (agent) role=backend"));
    let fence = shown.find("<<foreign-data kind=task_body").unwrap();
    assert!(shown.find(title).unwrap() > fence);
    assert!(shown.find("Done when: A fact anchored").unwrap() > fence);
    let updates = shown
        .find("Updates:\n<<foreign-data kind=task_update")
        .unwrap();
    assert!(shown.find("Found the budget constant").unwrap() > updates);
    assert!(shown.contains("(by you, elsewhere)"));
    // Every fenced block is exactly one complete wrapper.
    let blocks = [
        &text[text.find("<<foreign-data").unwrap()..text.find("\n… ").unwrap()],
        &shown[fence..updates - 1],
        &shown[updates + "Updates:\n".len()..],
    ];
    for block in blocks {
        assert!(
            crate::core::hive::untrusted::is_wrapped(block),
            "not one wrapper: {block}"
        );
    }
}

/// Another project's answer is refused.
#[test]
fn an_answer_for_another_project_is_refused() {
    let fake = Fake::start(|_, _, _| {
        let mut list = fixture("task_list.response.json");
        list["vault_id"] = json!("0000000000000000000000000000000f");
        (200, list)
    });
    let place = Place::new();
    assert!(matches!(
        list_via(&place.link(&fake), &ListFilter::default()),
        Err(HiveError::Protocol(_))
    ));
}

#[test]
fn a_claim_is_renewed_after_half_its_minutes() {
    let now = chrono::Utc::now();
    let held = |minutes_left: i64| HeldClaim {
        vault_id: PROJECT.into(),
        number: 7,
        root: "/r".into(),
        agent: "a".into(),
        claim: lean_ctx_protocol::hive::tasks::ClaimRefV1 {
            lease_id: "l".into(),
            fencing_token: 1,
        },
        until: (now + chrono::Duration::minutes(minutes_left))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        minutes: 10,
        pid: 1,
    };
    assert!(!held(6).renew_due(now));
    assert!(held(4).renew_due(now));
}

#[test]
fn a_new_session_of_the_agent_adopts_its_unexpired_claims() {
    let place = Place::new();
    let data = place.data.path();
    let claim = |number, agent: &str, minutes| HeldClaim {
        vault_id: PROJECT.into(),
        number,
        root: "/r".into(),
        agent: agent.into(),
        claim: lean_ctx_protocol::hive::tasks::ClaimRefV1 {
            lease_id: "l".into(),
            fencing_token: 1,
        },
        until: stamp(minutes),
        minutes: 10,
        pid: 1,
    };
    claims::put(data, claim(1, "claude-a", 5)).unwrap();
    claims::put(data, claim(2, "claude-a", -1)).unwrap();
    claims::put(data, claim(3, "claude-b", 5)).unwrap();
    let adopted = claims::adopt(data, "claude-a", chrono::Utc::now()).unwrap();
    assert_eq!(adopted.iter().map(|c| c.number).collect::<Vec<_>>(), [1]);
    assert_eq!(claims::own(data).len(), 1);
    assert_eq!(claims::all(data).len(), 2, "the expired claim is dropped");
    claims::clear(data).unwrap();
    assert!(
        claims::all(data).is_empty(),
        "a worker stop forgets every claim"
    );
}

/// One proposal is one task: the key ignores the order of the facts; a
/// proposal that cannot pass validation is never queued.
#[test]
fn proposals_have_one_key_and_are_validated_before_queueing() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let proposal = |facts: [&str; 2]| QueuedProposal {
        kind: ProposalKind::Contradiction,
        stable_identity: "git:github.com/acme/payments".into(),
        root: "/r".into(),
        facts: facts.iter().map(|f| (*f).to_owned()).collect(),
        title: "Settle the retry budget".into(),
        goal: None,
        acceptance: "One fact states the budget".into(),
        role: None,
        session_ref: None,
    };
    let a = proposal(["f-1", "f-2"]);
    assert_eq!(
        a.idempotency_key(),
        proposal(["f-2", "f-1"]).idempotency_key()
    );
    assert_ne!(
        a.idempotency_key(),
        proposal(["f-1", "f-3"]).idempotency_key()
    );
    let place = Place::new();
    propose::enqueue(place.data.path(), &a).unwrap();
    let mut short = a.clone();
    short.title = "Fix".into();
    assert!(matches!(
        propose::enqueue(place.data.path(), &short),
        Err(HiveError::Refused(why)) if why.contains("title")
    ));
    let mut secret = a;
    secret.acceptance = format!("Rotate {AWS_KEY}");
    assert!(matches!(
        propose::enqueue(place.data.path(), &secret),
        Err(HiveError::Refused(why)) if why.starts_with("the acceptance was not sent")
    ));
    let queued = crate::core::hive::outbox::Outbox::at(place.data.path())
        .list(crate::core::hive::outbox::CHANNEL_TASKS);
    assert_eq!(queued.len(), 1);
}

/// C0 1.9 §4: a claim under an opt-in that changed since it was read
/// (`optin_changed`) reads the opt-in again and claims once more.
#[test]
fn a_changed_optin_is_read_again_and_the_claim_retried_once() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let claims_sent = Arc::new(AtomicUsize::new(0));
    let sent = Arc::clone(&claims_sent);
    let fake = Fake::start(move |_, path, _| {
        if !path.ends_with("/claim") {
            return (
                404,
                json!({"error": {"code": "not_found", "message": "no"}}),
            );
        }
        if sent.fetch_add(1, Ordering::SeqCst) == 0 {
            (
                409,
                json!({"error": {"code": "invalid_state", "message": "x", "reason": "optin_changed", "number": 7}}),
            )
        } else {
            (200, claim_ack(7, 1, 10))
        }
    });
    let place = Place::new();
    let reads = AtomicUsize::new(0);
    let gate = |scope: &TaskScope| {
        reads.fetch_add(1, Ordering::SeqCst);
        admitted(scope)
    };
    claim_via(&place.link(&fake), 7, "claude-3e", "s", None, &gate).unwrap();
    assert_eq!(reads.load(Ordering::SeqCst), 2, "the opt-in was read again");
    assert_eq!(claims_sent.load(Ordering::SeqCst), 2, "one retry, no more");
}

/// A stop of the opt-in on the web (`optin_inactive`) ends this machine's
/// claim: it is forgotten and never renewed again.
#[test]
fn a_stopped_optin_ends_the_local_claim() {
    let fake = Fake::start(|_, path, _| {
        if path.ends_with("/7/claim") {
            (200, claim_ack(7, 1, 10))
        } else {
            (
                409,
                json!({"error": {"code": "invalid_state", "message": "x", "reason": "optin_inactive"}}),
            )
        }
    });
    let place = Place::new();
    let link = place.link(&fake);
    claim_via(&link, 7, "claude-3e", "s", None, &admitted).unwrap();
    let stopped = renew_via(&link, 7, "claude-3e").unwrap_err();
    assert!(matches!(
        stopped,
        HiveError::Task {
            refusal: TaskRefusal::OptinInactive,
            ..
        }
    ));
    assert!(claims::find(link.data_dir, PROJECT, 7).is_none());
}

/// A claim belongs to the agent that took it: another agent on the same
/// machine neither gets it handed back nor writes under its fencing token.
#[test]
fn another_agent_never_uses_a_held_claim() {
    let fake = hive_for_task_7();
    let place = Place::new();
    let link = place.link(&fake);
    claim_via(&link, 7, "claude-a", "s", None, &admitted).unwrap();
    let theirs = |result: Result<_, HiveError>| matches!(result, Err(HiveError::Refused(why)) if why.contains("by another agent"));
    assert!(theirs(
        claim_via(&link, 7, "claude-b", "s", None, &admitted).map(|_| ())
    ));
    assert!(theirs(
        update_via(&link, 7, "claude-b", "Taking over", None).map(|_| ())
    ));
    assert!(theirs(release_via(&link, 7, "claude-b", None).map(|_| ())));
    assert!(theirs(
        submit_as(
            &link,
            7,
            "claude-b",
            "Done",
            vec![commit()],
            None,
            &submitted
        )
        .map(|_| ())
    ));
    assert_eq!(fake.requests().len(), 1, "only A's claim was sent");
    assert_eq!(
        claims::find(link.data_dir, PROJECT, 7).unwrap().agent,
        "claude-a"
    );
}

/// The claim's agent acts on it from any of its processes: SERVER-1's
/// `hive tasks submit 63 --agent <its id>` was refused because the daemon
/// (keeping the CLI claim alive) or its own session held it, while that
/// session's `ctx_tasks` looked in another vault. Another agent still gets
/// nothing (`another_agent_never_uses_a_held_claim`).
#[cfg(unix)]
#[test]
fn the_claims_agent_acts_on_it_while_another_of_its_processes_holds_it() {
    let fake = hive_for_task_7();
    let place = Place::new();
    let link = place.link(&fake);
    let held = |pid| HeldClaim {
        vault_id: PROJECT.into(),
        number: 7,
        root: "/r".into(),
        agent: "claude-3e".into(),
        claim: lean_ctx_protocol::hive::tasks::ClaimRefV1 {
            lease_id: "l".into(),
            fencing_token: 7,
        },
        until: stamp(5),
        minutes: 10,
        pid,
    };
    // The test's parent (the test runner) runs, as this user: the holder.
    claims::put(link.data_dir, held(std::os::unix::process::parent_id())).unwrap();
    let theirs = release_via(&link, 7, "claude-b", None).unwrap_err();
    assert!(
        matches!(&theirs, HiveError::Refused(why) if why.contains("by another agent")),
        "{theirs:?}"
    );
    assert!(fake.requests().is_empty(), "nothing was sent for claude-b");
    release_via(&link, 7, "claude-3e", None).unwrap();
    assert_eq!(fake.requests().len(), 1, "the release went out");
    assert!(claims::find(link.data_dir, PROJECT, 7).is_none());
}

/// A session working in another repository finds the claim its agent made
/// in the task's checkout by number; its own vault's claim comes first,
/// and two other vaults with that number are not guessed between.
#[test]
fn a_claim_is_found_from_a_session_in_another_repository() {
    let data = tempfile::tempdir().unwrap();
    let (here, task_repo, third) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let path = |dir: &tempfile::TempDir| dir.path().to_string_lossy().into_owned();
    let mut map = crate::core::hive::vault::VaultMap::default();
    for (vault, dir) in [
        ("v-here", &here),
        ("v-task", &task_repo),
        ("v-third", &third),
    ] {
        map.vaults.insert(
            vault.into(),
            crate::core::hive::vault::VaultRecord {
                vault_id: vault.into(),
                ns: "o".into(),
                name: vault.into(),
                sealed: false,
                roots: vec![path(dir)],
            },
        );
    }
    map.save(data.path()).unwrap();
    let claim_in = |vault: &str, dir: &tempfile::TempDir, agent: &str| HeldClaim {
        vault_id: vault.into(),
        number: 63,
        root: path(dir),
        agent: agent.into(),
        claim: lean_ctx_protocol::hive::tasks::ClaimRefV1 {
            lease_id: format!("l-{vault}"),
            fencing_token: 1,
        },
        until: stamp(5),
        minutes: 10,
        pid: 1,
    };
    let root_for = |agent: &str| {
        claim_root_in(data.path(), here.path(), 63, agent)
            .to_string_lossy()
            .into_owned()
    };

    claims::put(data.path(), claim_in("v-task", &task_repo, "server-1")).unwrap();
    assert_eq!(root_for("server-1"), path(&task_repo), "found by number");
    assert_eq!(root_for("claude-b"), path(&here), "not another agent's");

    claims::put(data.path(), claim_in("v-third", &third, "server-1")).unwrap();
    assert_eq!(
        root_for("server-1"),
        path(&here),
        "two candidates: no guess"
    );

    claims::put(data.path(), claim_in("v-here", &here, "server-1")).unwrap();
    assert_eq!(
        root_for("server-1"),
        path(&here),
        "this vault's claim first"
    );
}

/// Parallel writers (renew thread, tool calls, other processes) never lose
/// a claim: every read-modify-write holds the claims file lock.
#[test]
fn parallel_writers_keep_every_claim() {
    let place = Place::new();
    let data = place.data.path().to_path_buf();
    let writers: Vec<_> = (1..=8u64)
        .map(|number| {
            let data = data.clone();
            std::thread::spawn(move || {
                claims::put(
                    &data,
                    HeldClaim {
                        vault_id: PROJECT.into(),
                        number,
                        root: "/r".into(),
                        agent: "a".into(),
                        claim: lean_ctx_protocol::hive::tasks::ClaimRefV1 {
                            lease_id: format!("l{number}"),
                            fencing_token: 1,
                        },
                        until: stamp(5),
                        minutes: 10,
                        pid: 1,
                    },
                )
                .unwrap();
            })
        })
        .collect();
    for writer in writers {
        writer.join().unwrap();
    }
    let mut numbers: Vec<u64> = claims::all(&data).iter().map(|c| c.number).collect();
    numbers.sort_unstable();
    assert_eq!(numbers, (1..=8).collect::<Vec<_>>());
    assert!(data.join("task-claims.lock").exists());
}

/// lean-ctx #3: a claim made by the CLI outlives its `--minutes` while its
/// agent works. The CLI's process has ended; the relay's process renews
/// the claim at half-time without becoming its holder (never the daemon's
/// pid). An agent no longer at work keeps nothing alive, and a claim whose
/// holder still runs is its holder's to renew.
#[cfg(unix)]
#[test]
fn a_cli_claim_outlives_its_minutes_while_its_agent_works() {
    let fake = Fake::start(|method, path, body| {
        let tail = path
            .strip_prefix(&format!("/vaults/{PROJECT}/tasks"))
            .unwrap_or(path);
        match (method, tail) {
            ("POST", "/7/claim") => (200, claim_ack(7, 7, 10)),
            ("POST", "/7/claim/renew") => {
                let token = body["claim"]["fencing_token"].as_u64().unwrap();
                (200, claim_ack(7, token, 25))
            }
            _ => (
                404,
                json!({"error": {"code": "not_found", "message": "no"}}),
            ),
        }
    });
    let place = Place::new();
    let link = place.link(&fake);
    let data = place.data.path();
    claim_via(&link, 7, "claude-3e", "s", Some(10), &admitted).unwrap();
    // The claiming process (the CLI) has ended.
    let mut ended = std::process::Command::new("true").spawn().unwrap();
    ended.wait().unwrap();
    let mut held = claims::find(data, PROJECT, 7).unwrap();
    held.pid = ended.id();
    claims::put(data, held.clone()).unwrap();
    let claimed_until = held.until().unwrap();
    // Past half-time of its 10 minutes.
    let now = claimed_until - chrono::Duration::minutes(2);
    let renew = |held: &HeldClaim| {
        renew_via(&link, held.number, &held.agent).unwrap();
    };
    let renewals = || {
        fake.requests()
            .iter()
            .filter(|(_, path, _)| path.ends_with("/claim/renew"))
            .count()
    };

    assert_eq!(keep_orphaned(data, now, |_| false, renew), 0, "agent gone");
    assert_eq!(claims::find(data, PROJECT, 7).unwrap().pid, ended.id());
    assert_eq!(renewals(), 0);

    assert_eq!(keep_orphaned(data, now, |a| a == "claude-3e", renew), 1);
    let kept = claims::find(data, PROJECT, 7).unwrap();
    assert_eq!(kept.pid, ended.id(), "renewed, not taken over");
    assert_eq!(renewals(), 1);
    assert!(
        kept.until().unwrap() > claimed_until + chrono::Duration::minutes(10),
        "{} outlives {claimed_until}",
        kept.until
    );
    // Not yet half-time again: no second renewal.
    keep_orphaned(data, Utc::now(), |a| a == "claude-3e", renew);
    assert_eq!(renewals(), 1);

    // A claim whose holder still runs is its holder's to renew.
    let mut running = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let mut other = kept.clone();
    other.number = 8;
    other.pid = running.id();
    claims::put(data, other).unwrap();
    let touched = std::cell::RefCell::new(Vec::new());
    keep_orphaned(
        data,
        now,
        |_| true,
        |held| {
            touched.borrow_mut().push(held.number);
        },
    );
    assert!(!touched.borrow().contains(&8), "{:?}", touched.borrow());
    assert_eq!(claims::find(data, PROJECT, 8).unwrap().pid, running.id());
    running.kill().unwrap();
    running.wait().unwrap();
}

fn view_of(task: Value) -> TaskView {
    let task: TaskV1 = serde_json::from_value(task).unwrap();
    TaskView {
        update_summaries: task.updates.iter().map(|u| u.summary.clone()).collect(),
        task,
        content: None,
        held_until: None,
    }
}

/// A 1.x answer (no `lifecycle`, a result proposal) renders as before; a
/// v5 answer shows `failed`, a running claim, and the log seq of a result
/// that has no proposal (never "proposal #0").
#[test]
fn task_views_of_1x_and_v5_render_their_result() {
    let old = render_task(&view_of(fixture("task.done.response.json")));
    assert!(
        old.starts_with("Hive task #7 — done (agent) role=backend"),
        "{old}"
    );
    assert!(
        old.ends_with("\nResult: proposal #13 (3 evidence)"),
        "{old}"
    );

    let mut failed = fixture("task.done.response.json");
    failed["state"] = "failed".into();
    failed["lifecycle"] = "failed".into();
    failed["result"]["proposal"] = 0.into();
    failed["result"]["seq"] = 42.into();
    let shown = render_task(&view_of(failed.clone()));
    assert!(
        shown.starts_with("Hive task #7 — failed (agent)"),
        "{shown}"
    );
    assert!(
        shown.ends_with("\nResult: knowledge at log seq 42 (3 evidence)"),
        "{shown}"
    );
    assert!(!shown.contains("proposal #0"));
    assert_eq!(
        shown,
        render_task(&view_of(failed.clone())),
        "deterministic"
    );
    failed["result"].as_object_mut().unwrap().remove("seq");
    let bare = render_task(&view_of(failed));
    assert!(
        bare.ends_with("\nResult: recorded on the task (3 evidence)"),
        "{bare}"
    );

    let mut running = fixture("task.claimed.response.json");
    running["lifecycle"] = "running".into();
    let shown = render_task(&view_of(running));
    assert!(
        shown.starts_with("Hive task #7 — claimed running (agent)"),
        "{shown}"
    );

    let mut list = fixture("task_list.response.json");
    list["tasks"][0]["lifecycle"] = "running".into();
    list["tasks"][1]["lifecycle"] = "queued".into();
    let list: TaskListV1 = serde_json::from_value(list).unwrap();
    let text = render_list(&list, 5);
    assert!(text.contains("#7 claimed running ("), "{text}");
    assert!(text.contains("#9 open ("), "queued shows the state: {text}");
}
