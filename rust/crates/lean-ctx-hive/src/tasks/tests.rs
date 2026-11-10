// SPDX-License-Identifier: Apache-2.0

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::propose::{ProposalKind, QueuedProposal};
use super::*;
use crate::TaskRefusal;
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
        concat!(env!("CARGO_MANIFEST_DIR"), "/../..")
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
            ("GET", "?state=open" | "" | "?priority=high" | "?state=open&priority=high") => {
                (200, fixture("task_list.response.json"))
            }
            _ => (
                404,
                json!({"error": {"code": "not_found", "message": "no"}}),
            ),
        }
    })
}

fn optin(vaults: &[&str], active: bool, tasks: u32) -> crate::worker::Optin {
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
    optin: crate::worker::Optin,
    state: crate::worker::WorkerState,
) -> impl Fn(&TaskScope) -> Result<Admission, Refusal> {
    move |scope| crate::worker::decide_claim(&optin, &Default::default(), &state, scope)
}

fn admitted(scope: &TaskScope) -> Result<Admission, Refusal> {
    gate(optin(&[PROJECT], true, 5), Default::default())(scope)
}

/// A claim tried again needs no fresh deadline against a local fake.
fn fresh() {}

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
            root: self.root.path(),
            via: Via::Origin(fake.origin.clone()),
            budget: BUDGET,
            deadline: std::cell::Cell::new(None),
            workers: None,
            presence: &test_presence,
            announce: None,
        }
    }
}

/// Every test agent is a live presence of this Instance in a reviewing
/// role, registered by its own process and held by the caller.
fn test_presence(agent: &str) -> Option<super::process::Presence> {
    Some(super::process::Presence {
        agent: agent.to_owned(),
        role: Some("code_reviewer".into()),
        app_session: None,
        process: Some(format!("proc-of-{agent}")),
        bound_here: true,
    })
}

/// Dogfood 03.11.: `hive tasks claim --agent mcp-…` sent the local bus id,
/// but Hive keeps presence under the worker the relay announced, so a task
/// with a role was refused "not on the bus". A local agent goes to Hive as
/// its worker; a worker id, or a name the book does not know, as given.
#[test]
fn a_local_agent_claims_as_the_worker_hive_knows() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let store = HiveStore::at(dir.path().join("c"), dir.path().join("d"));
    store
        .write_config(
            crate::state::INSTANCE_FILE,
            &crate::state::InstanceRecord {
                instance_id: "inst_a".into(),
                org_id: "org_a".into(),
                org_name: "Acme".into(),
                label: "mac".into(),
                url: "https://hive.example".into(),
                org_salt_id: "salt".into(),
                enrolled_at: "2026-10-24T00:00:00Z".into(),
                web_url: None,
                org_kind: None,
                company_name: None,
            },
        )
        .unwrap();
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_string_lossy().into_owned();
    let mut registry = lean_ctx::core::agents::AgentRegistry::new();
    let agent = registry
        .register("claude", Some("ops"), &root, None)
        .unwrap();
    // An agent outside any app session: `register` takes LEAN_CTX_SESSION_ID
    // from the environment (set in every Hive session), which would give it
    // a per-session worker instead (covered by
    // `a_session_env_gives_the_registered_agent_its_own_worker`).
    registry.agents[0].app_session_id = None;
    registry.save().unwrap();

    let place = Place::new();
    let fake = hive_for_task_7();
    let mut link = place.link(&fake);
    link.workers = Some(&store);
    let worker = crate::workers::workspace_worker_id("inst_a", "claude-code", "ops");
    assert_eq!(link.hive_agent(&agent), worker);
    assert_eq!(link.hive_agent(&worker), worker, "a worker id stays");
    assert_eq!(
        link.hive_agent("codex-reviewer"),
        "codex-reviewer",
        "unknown: as given"
    );
    link.workers = None;
    assert_eq!(link.hive_agent(&agent), agent, "no book: as given");

    // A task with a role, against a Hive that checks presence as prod does
    // (by the id the claim names): the claim is granted, kept locally under
    // the agent, and the verify names the worker too.
    let presence = worker.clone();
    let strict = Fake::start(move |method, path, body| {
        let tail = path
            .strip_prefix(&format!("/vaults/{PROJECT}/tasks"))
            .unwrap_or(path);
        match (method, tail) {
            ("POST", "/7/claim") if body["agent"] == presence.as_str() => {
                (200, claim_ack(7, 7, 10))
            }
            ("POST", "/7/claim") => (
                403,
                json!({"error": {"code": "forbidden_right", "reason": "not_permitted",
                                 "number": 7, "check": "presence", "role": "ops",
                                 "message": "not on the bus"}}),
            ),
            ("POST", "/7/verify") if body["worker_id"] == presence.as_str() => {
                (200, ack(7, "done"))
            }
            // The result came from another Instance: someone else's work.
            ("GET", "/7") => {
                let mut task = fixture("task.done.response.json");
                task["state"] = "submitted".into();
                task["result"]["by"]["mine"] = false.into();
                (200, task)
            }
            _ => (
                404,
                json!({"error": {"code": "not_found", "message": tail}}),
            ),
        }
    });
    let mut link = place.link(&strict);
    link.workers = Some(&store);
    let granted = claim_via(&link, 7, &agent, "s", None, &admitted, &fresh).unwrap();
    assert_eq!(granted.number, 7);
    let held = claims::find(place.data.path(), PROJECT, 7).unwrap();
    assert_eq!(held.agent, agent, "the local claim keeps the bus agent");
    let sent = strict.requests();
    assert_eq!(sent[0].2["agent"], worker.as_str(), "{sent:?}");
    verify_via(&link, 7, &agent, TaskVerdict::Accept, None).unwrap();
    // The same claim with the bus id, as before: refused for presence.
    link.workers = None;
    claims::remove(place.data.path(), PROJECT, 7).unwrap();
    let refused = claim_via(&link, 7, &agent, "s", None, &admitted, &fresh).unwrap_err();
    assert!(refused.to_string().contains("not on the bus"), "{refused}");
}

/// Hive task #27 (pounce 06.11.): a developer session's claim went to Hive
/// as its worker, Hive found no presence of that worker with role
/// developer, and the refusal never said whom it looked up. A claim
/// refused for presence now asks the relay to announce and tries once
/// more; a refusal that stands names the worker, the bus agent and role.
#[test]
fn a_presence_refusal_announces_retries_and_names_whom_hive_looked_up() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static ANNOUNCED: AtomicBool = AtomicBool::new(false);
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let store = HiveStore::at(dir.path().join("c"), dir.path().join("d"));
    store
        .write_config(
            crate::state::INSTANCE_FILE,
            &crate::state::InstanceRecord {
                instance_id: "inst_31750cccc601463389943a42312d720d".into(),
                org_id: "org_a".into(),
                org_name: "Acme".into(),
                label: "pounce".into(),
                url: "https://hive.example".into(),
                org_salt_id: "salt".into(),
                enrolled_at: "2026-10-24T00:00:00Z".into(),
                web_url: None,
                org_kind: None,
                company_name: None,
            },
        )
        .unwrap();
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_string_lossy().into_owned();
    let mut registry = lean_ctx::core::agents::AgentRegistry::new();
    let agent = registry
        .register("claude", Some("developer"), &root, None)
        .unwrap();
    registry.save().unwrap();
    let worker = crate::workers::workspace_worker_id(
        "inst_31750cccc601463389943a42312d720d",
        "claude-code",
        "developer",
    );

    // Hive as on pounce: the worker is present with role developer only
    // once the relay announced it.
    let refusal = format!(
        "Agent {worker} is not on the bus: this task needs it present with role developer. \
         Start its session (or the relay) and claim again."
    );
    let present = worker.clone();
    let hive = Fake::start(move |method, path, body| {
        let tail = path
            .strip_prefix(&format!("/vaults/{PROJECT}/tasks"))
            .unwrap_or(path);
        match (method, tail) {
            ("POST", "/7/claim")
                if body["agent"] == present.as_str() && ANNOUNCED.load(Ordering::SeqCst) =>
            {
                (200, claim_ack(7, 7, 10))
            }
            ("POST", "/7/claim") => (
                403,
                json!({"error": {"code": "forbidden_right", "reason": "not_permitted",
                                 "number": 7, "check": "presence", "role": "developer",
                                 "message": refusal}}),
            ),
            _ => (
                404,
                json!({"error": {"code": "not_found", "message": tail}}),
            ),
        }
    });
    let place = Place::new();
    let mut link = place.link(&hive);
    link.workers = Some(&store);

    // The refusal as it was (no relay to ask): it now names the worker
    // Hive looked up, the bus agent it stands for and the role.
    let refused = claim_via(&link, 7, &agent, "s", None, &admitted, &fresh).unwrap_err();
    let text = refused.to_string();
    assert!(
        text.starts_with(&format!("task #7: Agent {worker} is not on the bus")),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "Hive looked up {worker} (the worker of bus agent {agent}, role developer)."
        )),
        "{text}"
    );
    assert!(text.contains("[check: presence]"), "{text}");

    // No relay answers the request to announce: refused once more, and
    // the refusal says so; the claim was not sent twice.
    fn silent(_: Duration) -> Option<String> {
        None
    }
    link.announce = Some(silent);
    let before = hive.requests().len();
    let refused = claim_via(&link, 7, &agent, "s", None, &admitted, &fresh).unwrap_err();
    assert!(
        refused
            .to_string()
            .ends_with("No relay answered the request to announce."),
        "{refused}"
    );
    assert_eq!(hive.requests().len(), before + 1, "one claim, no retry");

    // The relay announces the worker: the claim is tried again and granted.
    fn relay(_: Duration) -> Option<String> {
        ANNOUNCED.store(true, Ordering::SeqCst);
        Some("announced 1 agent".into())
    }
    link.announce = Some(relay);
    let restarted = std::cell::Cell::new(false);
    let granted = claim_via(&link, 7, &agent, "s", None, &admitted, &|| {
        restarted.set(true);
    })
    .unwrap();
    assert_eq!(granted.number, 7);
    assert!(restarted.get(), "the retry gets a fresh deadline");
    let claims: Vec<_> = hive
        .requests()
        .into_iter()
        .filter(|(method, path, _)| method == "POST" && path.ends_with("/7/claim"))
        .collect();
    assert_eq!(claims.len(), 4, "{claims:?}");
    assert!(
        claims
            .iter()
            .all(|(_, _, body)| body["agent"] == worker.as_str())
    );
}

/// Task #21 with process v1 §3.1 (5!189): on one Instance, a worker
/// session and a separate reviewer session (both Claude Code, even with the
/// same role) are two agents on Hive and two sessions locally. Both checks
/// of checker ≠ author run on a verify: the Instance's own
/// (`checker_not_author`, by the per-session identity of the claim and the
/// submit) before anything is sent, and Hive's `self_verify` (by the
/// `wkr_` the verdict names, per session via `session_role`). They agree:
/// the reviewer session passes both, the worker's own session and a second
/// presence of it (a subagent) are refused by both. Without an app session
/// two agents share the `(host, role)` worker, so Hive still refuses what
/// the Instance lets pass: the stricter check holds. The Hive here keeps
/// the result's agent as prod does (the id the claim named) and refuses a
/// verify by it.
#[test]
fn a_separate_session_verifies_and_the_same_session_cannot() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let store = HiveStore::at(dir.path().join("c"), dir.path().join("d"));
    store
        .write_config(
            crate::state::INSTANCE_FILE,
            &crate::state::InstanceRecord {
                instance_id: "inst_a".into(),
                org_id: "org_a".into(),
                org_name: "Acme".into(),
                label: "pounce".into(),
                url: "https://hive.example".into(),
                org_salt_id: "salt".into(),
                enrolled_at: "2026-11-07T00:00:00Z".into(),
                web_url: None,
                org_kind: None,
                company_name: None,
            },
        )
        .unwrap();
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_string_lossy().into_owned();
    let mut registry = lean_ctx::core::agents::AgentRegistry::new();
    let worker = registry
        .register("claude", Some("coder"), &root, None)
        .unwrap();
    let template = registry.agents[0].clone();
    let presence = |id: &str, role: &str, app_session: Option<&str>| {
        let mut entry = template.clone();
        id.clone_into(&mut entry.agent_id);
        entry.role = Some(role.to_owned());
        entry.app_session_id = app_session.map(str::to_owned);
        entry
    };
    registry.agents[0] = presence(&worker, "coder", Some("hive-session-worker"));
    // The reviewer runs in its own app session with the same role.
    let reviewer = "mcp-2-reviewer".to_owned();
    registry
        .agents
        .push(presence(&reviewer, "coder", Some("hive-session-reviewer")));
    // A second presence of the worker's own app session (a subagent).
    let same = "mcp-3-same-session".to_owned();
    registry
        .agents
        .push(presence(&same, "coder", Some("hive-session-worker")));
    // Two sessions without an app session id share one worker, as before.
    let plain_a = "mcp-4-plain".to_owned();
    let plain_b = "mcp-5-plain".to_owned();
    registry.agents.push(presence(&plain_a, "coder", None));
    registry.agents.push(presence(&plain_b, "coder", None));
    registry.save().unwrap();
    // Each presence live, held by this process, registered by its own
    // process, in the app session the registry has for it.
    let sessions: Vec<(String, Option<String>)> = registry
        .agents
        .iter()
        .map(|a| (a.agent_id.clone(), a.app_session_id.clone()))
        .collect();
    let lookup = move |agent: &str| {
        let (id, app_session) = sessions.iter().find(|(id, _)| id == agent)?;
        Some(super::process::Presence {
            agent: id.clone(),
            role: Some("coder".into()),
            app_session: app_session.clone(),
            process: Some(format!("proc-of-{id}")),
            bound_here: true,
        })
    };

    let place = Place::new();
    let result_agent: Arc<Mutex<Option<String>>> = Arc::default();
    let seen = Arc::clone(&result_agent);
    let hive = Fake::start(move |method, path, body| {
        let tail = path
            .strip_prefix(&format!("/vaults/{PROJECT}/tasks"))
            .unwrap_or(path);
        let by = seen.lock().unwrap().clone();
        match (method, tail) {
            ("POST", "/7/claim") => {
                *seen.lock().unwrap() = body["agent"].as_str().map(str::to_owned);
                (200, claim_ack(7, 7, 10))
            }
            ("POST", "/7/result") => (200, ack(7, "submitted")),
            // The submitted result, by this Instance as the worker the
            // claim named.
            ("GET", "/7") => {
                let mut task = fixture("task.done.response.json");
                task["state"] = "submitted".into();
                task["result"]["by"]["agent"] = by.unwrap_or_default().into();
                (200, task)
            }
            ("POST", "/7/verify") if by.as_deref() == body["worker_id"].as_str() => (
                403,
                json!({"error": {"code": "forbidden_right", "reason": "self_verify",
                                 "number": 7, "message": "the worker that sent a result does not verify it"}}),
            ),
            ("POST", "/7/verify") => (200, ack(7, "done")),
            _ => (
                404,
                json!({"error": {"code": "not_found", "message": tail}}),
            ),
        }
    });
    let mut link = place.link(&hive);
    link.workers = Some(&store);
    link.presence = &lookup;

    let worker_id = link.hive_agent(&worker);
    let reviewer_id = link.hive_agent(&reviewer);
    assert!(crate::bus::address::is_worker_id(&worker_id), "{worker_id}");
    assert!(
        crate::bus::address::is_worker_id(&reviewer_id),
        "{reviewer_id}"
    );
    assert_ne!(worker_id, reviewer_id, "two sessions, two agents");
    assert_ne!(
        worker_id,
        crate::workers::workspace_worker_id("inst_a", "claude-code", "coder"),
        "never the shared machine worker"
    );
    assert_eq!(link.hive_agent(&same), worker_id, "one session, one agent");
    assert_eq!(link.hive_agent(&worker), worker_id, "stable per session");
    // The id Hive recomputes from the role the relay registers.
    let role = crate::workers::session_role("coder", Some("hive-session-worker"));
    assert_eq!(
        worker_id,
        crate::workers::workspace_worker_id("inst_a", "claude-code", &role)
    );
    assert_eq!(
        link.hive_agent(&plain_a),
        link.hive_agent(&plain_b),
        "without an app session: the (host, role) worker"
    );

    // The worker session claims and submits the result.
    claim_via(&link, 7, &worker, "s", None, &admitted, &fresh).unwrap();
    assert_eq!(hive.requests()[0].2["agent"], worker_id.as_str());
    submit_as(&link, 7, &worker, "Done", vec![commit()], None, &submitted).unwrap();
    let traced = claims::trace(place.data.path(), PROJECT, 7).unwrap();
    assert_eq!(
        traced.submitted_by.and_then(|s| s.session).as_deref(),
        Some("hive-session-worker"),
        "the author's per-session identity"
    );

    // Each check on its own: the Instance's (nothing is sent) and Hive's
    // (the verdict as the worker the agent is on Hive).
    let local = |agent: &str| -> Result<(), HiveError> {
        let task = link.call(|api| api.show(7))?;
        let checker = super::process::checker_identity(
            agent,
            (link.presence)(agent),
            task.process.is_some(),
        )?;
        let trace = claims::trace(place.data.path(), PROJECT, 7);
        super::process::checker_not_author(&task, &checker, trace.as_ref())
    };
    let on_hive = |agent: &str| -> Result<TaskAckV1, HiveError> {
        let request = TaskVerifyRequestV1 {
            schema: TASK_VERIFY_SCHEMA.into(),
            worker_id: link.hive_agent(agent),
            verdict: TaskVerdict::Accept,
            reason: None,
        };
        link.call(|api| api.verify(7, &request, &format!("direct-{agent}")))
    };
    let self_verify = |error: &HiveError| {
        matches!(
            error,
            HiveError::Task {
                refusal: TaskRefusal::SelfVerify,
                ..
            }
        )
    };
    for agent in [&worker, &same] {
        let mine = local(agent).unwrap_err();
        assert!(
            mine.to_string().starts_with("self_check"),
            "{agent}: {mine}"
        );
        let hive_says = on_hive(agent).unwrap_err();
        assert!(self_verify(&hive_says), "{agent}: {hive_says}");
    }
    local(&reviewer).unwrap();
    on_hive(&reviewer).unwrap();

    // Through `verify`: the same session is refused here before anything
    // is sent; the separate session passes the Instance and Hive.
    let sent_before = hive.requests().len();
    for agent in [&worker, &same] {
        let refused = verify_via(&link, 7, agent, TaskVerdict::Accept, None).unwrap_err();
        assert!(
            refused.to_string().starts_with("self_check"),
            "{agent}: {refused}"
        );
    }
    let ack = verify_via(&link, 7, &reviewer, TaskVerdict::Accept, None).unwrap();
    assert_eq!(ack.number, 7);
    let verifies: Vec<_> = hive.requests()[sent_before..]
        .iter()
        .filter(|(_, path, _)| path.ends_with("/7/verify"))
        .map(|(_, _, body)| body["worker_id"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(verifies, [reviewer_id], "only the reviewer's verdict left");

    // Without an app session: another process is another session here, but
    // both are the one `(host, role)` worker on Hive, which refuses it.
    claim_via(&link, 7, &plain_a, "s", None, &admitted, &fresh).unwrap();
    submit_as(&link, 7, &plain_a, "Done", vec![commit()], None, &submitted).unwrap();
    local(&plain_b).unwrap();
    let refused = verify_via(&link, 7, &plain_b, TaskVerdict::Accept, None).unwrap_err();
    assert!(self_verify(&refused), "{refused}");
    let refused = verify_via(&link, 7, &plain_a, TaskVerdict::Accept, None).unwrap_err();
    assert!(refused.to_string().starts_with("self_check"), "{refused}");
}

/// #21: an agent registered inside an app session (LEAN_CTX_SESSION_ID set,
/// as in every Hive session) goes to Hive as its own per-session worker,
/// not the shared (host, role) worker.
#[test]
fn a_session_env_gives_the_registered_agent_its_own_worker() {
    const SESSION: &str = "hive-session-env-21";
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let store = HiveStore::at(dir.path().join("c"), dir.path().join("d"));
    store
        .write_config(
            crate::state::INSTANCE_FILE,
            &crate::state::InstanceRecord {
                instance_id: "inst_a".into(),
                org_id: "org_a".into(),
                org_name: "Acme".into(),
                label: "mac".into(),
                url: "https://hive.example".into(),
                org_salt_id: "salt".into(),
                enrolled_at: "2026-10-24T00:00:00Z".into(),
                web_url: None,
                org_kind: None,
                company_name: None,
            },
        )
        .unwrap();
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_string_lossy().into_owned();
    let mut registry = lean_ctx::core::agents::AgentRegistry::new();
    let session_of = |registry: &lean_ctx::core::agents::AgentRegistry, id: &str| {
        registry
            .agents
            .iter()
            .find(|a| a.agent_id == id)
            .and_then(|a| a.app_session_id.clone())
    };
    let shared = crate::workers::workspace_worker_id("inst_a", "claude-code", "ops");
    let own = crate::workers::workspace_worker_id(
        "inst_a",
        "claude-code",
        &crate::workers::session_role("ops", Some(SESSION)),
    );
    assert_ne!(own, shared);
    let place = Place::new();
    let fake = hive_for_task_7();
    let mut link = place.link(&fake);
    link.workers = Some(&store);

    // Inside the app session (scoped; `_data` holds the env lock throughout).
    {
        let _session = lean_ctx::test_env::VarGuard::set("LEAN_CTX_SESSION_ID", SESSION);
        let agent = registry
            .register("claude", Some("ops"), &root, None)
            .unwrap();
        assert_eq!(session_of(&registry, &agent).as_deref(), Some(SESSION));
        registry.save().unwrap();
        assert_eq!(link.hive_agent(&agent), own, "per-session worker");
    }
    // Without a session the agent shares the (host, role) worker:
    // `a_local_agent_claims_as_the_worker_hive_knows`.
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

    let granted = claim_via(
        &link,
        7,
        "claude-3e",
        "session-1",
        Some(30),
        &admitted,
        &fresh,
    )
    .unwrap();
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
    let again = claim_via(&link, 7, "claude-3e", "session-1", None, &admitted, &fresh).unwrap();
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
            // A result reads the task first: a process task runs G6.
            format!("{tasks}/7"),
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
    assert_eq!(requests[5].2["claim"]["fencing_token"], 7);
    assert_eq!(
        requests[5].2["evidence"],
        json!([{"kind": "commit", "reference": "9f01cf32a3"}])
    );
    assert_eq!(requests[5].2["usage"]["tokens"], 1200);
    // Prod 1.x: the result carries no v5 field and no draft, and the
    // answer reads as a result waiting for its decision.
    assert!(requests[5].2.get("outcome").is_none());
    assert!(requests[5].2.get("draft").is_none());
    assert!(
        requests[5].2.get("gate_record").is_none(),
        "no process, no G6 record"
    );
    assert_eq!(
        render_result_ack(&ack),
        "Result submitted on task #7; another agent checks it before it is done."
    );
}

/// Hive v5: a result ends the task at once (`done`), with the same
/// request a 1.x Hive gets; knowledge never rides along as a draft.
#[test]
fn a_v5_result_completes_the_task_with_the_1x_request() {
    let fake = hive_for_task_7_on(true);
    let place = Place::new();
    let link = place.link(&fake);
    claim_via(&link, 7, "claude-3e", "s", None, &admitted, &fresh).unwrap();
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
        1,
        "a completed result reads the task once (a process task runs G6)"
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
    claim_via(&link, 7, "claude-3e", "s", None, &admitted, &fresh).unwrap();
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
    claim_via(&link, 7, "claude-3e", "s", None, &admitted, &fresh).unwrap();
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
    use crate::worker::{Usage, WorkerState};
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
        let refused = claim_via(&link, 7, "claude-3e", "s", None, &gate, &fresh).unwrap_err();
        assert!(
            matches!(&refused, HiveError::Refused(why) if why.contains(says)),
            "{case}: {refused:?}"
        );
    }
    assert!(fake.requests().is_empty(), "no claim left the machine");
    assert!(claims::all(place.data.path()).is_empty());

    // A result the gate refuses is not sent and keeps the claim.
    claim_via(&link, 7, "claude-3e", "s", None, &admitted, &fresh).unwrap();
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
    let error = claim_via(&link, 7, "claude-3e", "s", None, &admitted, &fresh).unwrap_err();
    assert_eq!(
        error.to_string(),
        "task #7: this instance's opt-in does not cover this vault"
    );
    let budget = claim_via(&link, 8, "claude-3e", "s", None, &admitted, &fresh).unwrap_err();
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
    let refused = claim_via(&link, 8, "claude-3e", "s", None, &admitted, &fresh).unwrap_err();
    assert_eq!(
        refused,
        HiveError::Task {
            refusal: TaskRefusal::Claimed,
            number: Some(7),
            detail: None
        }
    );
    claim_via(&link, 7, "claude-3e", "s", None, &admitted, &fresh).unwrap();
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
    claim_via(&link, 7, "claude-3e", "s", None, &admitted, &fresh).unwrap();
    let reclaimed = std::cell::Cell::new(false);
    let ack = update_or_reclaim_via(&link, 7, "claude-3e", "Still on it", None, || {
        reclaimed.set(true);
        claim_via(&link, 7, "claude-3e", "s", None, &admitted, &fresh)
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
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let fake = hive_for_task_7();
    let place = Place::new();
    let link = place.link(&fake);
    claim_via(&link, 7, "claude-3e", "s", None, &admitted, &fresh).unwrap();
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
    claim_via(&link, 7, "claude-3e", "s", None, &admitted, &fresh).unwrap();
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
    let text = render_listing(&list, 1);
    let title = "Explain the retry budget";
    let fence = text.find("<<foreign-data kind=task_body").expect("fenced");
    assert!(text.find(title).unwrap() > fence);
    assert!(text.contains("… 1 more"));
    assert!(!text.contains("Settle the retry budget"), "limit holds");
    assert_eq!(text, render_listing(&list, 1), "deterministic");

    let view = show_cached(&link, 7).unwrap().value;
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
            crate::untrusted::is_wrapped(block),
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
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
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
        local_number: None,
    };
    let a = proposal(["f-1", "f-2"]);
    // Two local tasks of one title brought on link stay two vault tasks.
    let local = |n| QueuedProposal {
        kind: ProposalKind::Agent,
        local_number: n,
        ..proposal(["f-1", "f-2"])
    };
    assert_ne!(
        local(Some(1)).idempotency_key(),
        local(Some(2)).idempotency_key()
    );
    assert_ne!(
        local(Some(1)).idempotency_key(),
        local(None).idempotency_key()
    );
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
    let queued = crate::outbox::Outbox::at(place.data.path()).list(crate::outbox::CHANNEL_TASKS);
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
    claim_via(&place.link(&fake), 7, "claude-3e", "s", None, &gate, &fresh).unwrap();
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
    claim_via(&link, 7, "claude-3e", "s", None, &admitted, &fresh).unwrap();
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
    claim_via(&link, 7, "claude-a", "s", None, &admitted, &fresh).unwrap();
    let theirs = |result: Result<_, HiveError>| matches!(result, Err(HiveError::Refused(why)) if why.contains("by another agent"));
    assert!(theirs(
        claim_via(&link, 7, "claude-b", "s", None, &admitted, &fresh).map(|_| ())
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
    let mut map = crate::vault::VaultMap::default();
    for (vault, dir) in [
        ("v-here", &here),
        ("v-task", &task_repo),
        ("v-third", &third),
    ] {
        map.vaults.insert(
            vault.into(),
            crate::vault::VaultRecord {
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
    claim_via(&link, 7, "claude-3e", "s", Some(10), &admitted, &fresh).unwrap();
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
        task,
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

/// k2 E2E: a claim whose session ended is given back by the daemon, so
/// another agent can take the task; one whose agent still works is not;
/// an expired claim Hive refuses is dropped here.
#[cfg(unix)]
#[test]
fn the_daemon_gives_back_claims_nobody_works_on() {
    let fake = Fake::start(|method, path, body| {
        let tail = path
            .strip_prefix(&format!("/vaults/{PROJECT}/tasks"))
            .unwrap_or(path);
        match (method, tail) {
            ("POST", "/7/claim") => (200, claim_ack(7, 7, 10)),
            ("POST", "/7/release") => {
                assert_eq!(body["reason"], ABANDONED_REASON);
                (200, ack(7, "open"))
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
    claim_via(&link, 7, "claude-3e", "s", Some(10), &admitted, &fresh).unwrap();
    let mut ended = std::process::Command::new("true").spawn().unwrap();
    ended.wait().unwrap();
    let mut held = claims::find(data, PROJECT, 7).unwrap();
    held.pid = ended.id();
    claims::put(data, held.clone()).unwrap();
    let now = held.until().unwrap() - chrono::Duration::minutes(5);
    let release = |held: &HeldClaim| {
        release_via(&link, held.number, &held.agent, Some(ABANDONED_REASON)).map(|_| ())
    };
    let releases = || {
        fake.requests()
            .iter()
            .filter(|(_, path, _)| path.ends_with("/release"))
            .count()
    };

    // Its agent still works here: the claim stays (keep_cli_claims renews it).
    assert_eq!(
        give_back_abandoned(data, now, |a| a == "claude-3e", release),
        0
    );
    assert!(claims::find(data, PROJECT, 7).is_some());
    assert_eq!(releases(), 0);

    // The session ended: given back with the reason, gone from the file.
    assert_eq!(give_back_abandoned(data, now, |_| false, release), 1);
    assert!(claims::find(data, PROJECT, 7).is_none());
    assert_eq!(releases(), 1);

    // An expired claim Hive refuses (here: no route for #8) is dropped.
    let mut expired = held.clone();
    expired.number = 8;
    expired.until = (now - chrono::Duration::minutes(30)).to_rfc3339();
    claims::put(data, expired).unwrap();
    assert_eq!(give_back_abandoned(data, now, |_| true, release), 0);
    assert!(claims::find(data, PROJECT, 8).is_none(), "expired: dropped");
}

/// Hive for priority writes on task 7: views carry `fields_version`
/// `version()`, a `PATCH` at another base answers `409 stale_base`.
fn hive_for_fields(version: Arc<Mutex<u32>>, bump_first: bool) -> Fake {
    let bumped = Arc::new(Mutex::new(!bump_first));
    Fake::start(move |method, path, body| {
        let tasks = format!("/vaults/{PROJECT}/tasks");
        let tail = path.strip_prefix(&tasks).unwrap_or(path);
        match (method, tail) {
            ("GET", "/7") => {
                let mut task = fixture("task.claimed.response.json");
                task["fields_version"] = json!(*version.lock().expect("lock"));
                (200, task)
            }
            ("PATCH", "/7") => {
                let mut current = version.lock().expect("lock");
                let mut bumped = bumped.lock().expect("lock");
                if !*bumped {
                    // Another person wrote between our read and our write.
                    *bumped = true;
                    *current += 1;
                }
                if body["base_version"] != json!(*current) {
                    return (
                        409,
                        json!({"error": {"code": "invalid_state", "reason": "stale_base",
                            "message": "stale", "number": 7}}),
                    );
                }
                *current += 1;
                (200, ack(7, "claimed"))
            }
            _ => (
                404,
                json!({"error": {"code": "not_found", "message": "no"}}),
            ),
        }
    })
}

/// C0 2.2 E47: a priority write is a `PATCH` at the task's
/// `fields_version` with exactly the fields that change.
#[test]
fn a_priority_write_patches_at_the_fields_version() {
    let version = Arc::new(Mutex::new(3));
    let fake = hive_for_fields(Arc::clone(&version), false);
    let place = Place::new();
    let change = fields::FieldsChange {
        priority: Some(lean_ctx_protocol::hive::tasks::TaskPriority::High),
        labels: Some(vec!["core".into(), "p1".into()]),
    };
    let ack = fields::set_fields_via(&place.link(&fake), 7, &change).expect("ok");
    assert_eq!(ack.number, 7);
    let patch = fake
        .requests()
        .into_iter()
        .find(|(method, _, _)| method == "PATCH")
        .expect("a PATCH");
    assert_eq!(
        patch.2,
        json!({"schema": "leanctx.hive.task-fields/v1", "base_version": 3,
            "priority": "high", "labels": ["core", "p1"]})
    );
    assert_eq!(*version.lock().expect("lock"), 4);
}

/// `409 stale_base`: the task is read again and the write sent once more.
#[test]
fn a_stale_base_reads_the_task_again_and_retries_once() {
    let version = Arc::new(Mutex::new(1));
    let fake = hive_for_fields(Arc::clone(&version), true);
    let place = Place::new();
    let change = fields::FieldsChange {
        priority: Some(lean_ctx_protocol::hive::tasks::TaskPriority::Normal),
        labels: None,
    };
    fields::set_fields_via(&place.link(&fake), 7, &change).expect("ok");
    let patches: Vec<Value> = fake
        .requests()
        .into_iter()
        .filter(|(method, _, _)| method == "PATCH")
        .map(|(_, _, body)| body)
        .collect();
    let bases: Vec<&Value> = patches.iter().map(|body| &body["base_version"]).collect();
    assert_eq!(bases, vec![&json!(1), &json!(2)]);
    assert!(
        patches.iter().all(|body| body.get("labels").is_none()),
        "labels are not sent when unchanged: {patches:?}"
    );
}

/// A wrong value is refused before any request, with what is allowed.
#[test]
fn a_wrong_priority_or_label_is_refused_before_any_request() {
    let error = fields::parse_priority("urgent")
        .expect_err("refused")
        .to_string();
    assert!(error.contains("priority is high or normal"), "{error}");
    assert!(fields::parse_priority("High").is_err());
    assert_eq!(
        fields::parse_priority("high").expect("ok"),
        lean_ctx_protocol::hive::tasks::TaskPriority::High
    );
    let error = fields::parse_labels("ok,Bad Label")
        .expect_err("refused")
        .to_string();
    assert!(error.contains("[a-z0-9-]"), "{error}");
    assert!(fields::parse_labels("a,a").is_err());
    assert_eq!(
        fields::parse_labels(" core , p1 ").expect("ok"),
        vec!["core".to_owned(), "p1".to_owned()]
    );
    assert_eq!(fields::parse_labels("").expect("ok"), Vec::<String>::new());
    let fake = Fake::start(|_, _, _| (500, json!({})));
    let place = Place::new();
    let nothing = fields::FieldsChange::default();
    assert!(fields::set_fields_via(&place.link(&fake), 7, &nothing).is_err());
    assert!(fake.requests().is_empty(), "nothing went to Hive");
}

/// Lists carry the order of work: high first, then by number.
#[test]
fn a_listing_shows_high_priority_first_then_by_number() {
    let fake = Fake::start(|_, _, _| {
        let mut list = fixture("task_list.response.json");
        list["tasks"][1]["priority"] = json!("high");
        list["tasks"][1]["labels"] = json!(["core"]);
        (200, list)
    });
    let place = Place::new();
    let list = list_via(&place.link(&fake), &ListFilter::default()).expect("ok");
    let order: Vec<u64> = list.list.tasks.iter().map(|t| t.number).collect();
    assert_eq!(order, vec![9, 7]);
    let text = render_listing(&list, 10);
    assert!(text.contains("#9 high open"), "{text}");
    assert!(text.contains("[core]"), "{text}");
}

/// Review of #12: Hive answers the newest 100 tasks by number, so an older
/// high task fell off the page. The high tasks are fetched with the same
/// filters and merged in, each task once, before the sort.
#[test]
fn an_older_high_task_beyond_a_full_page_still_comes_first() {
    let fake = Fake::start(|_, path, _| {
        let row = fixture("task_list.response.json")["tasks"][1].clone();
        let task = |number: u64, priority: &str| {
            let mut task = row.clone();
            task["number"] = json!(number);
            task["priority"] = json!(priority);
            task
        };
        let mut list = fixture("task_list.response.json");
        list["tasks"] = if path.contains("priority=high") {
            json!([task(150, "high"), task(5, "high")])
        } else {
            let mut page: Vec<Value> = (101..=200).rev().map(|n| task(n, "normal")).collect();
            page[50] = task(150, "high");
            json!(page)
        };
        (200, list)
    });
    let place = Place::new();
    let filter = ListFilter {
        state: Some(TaskState::Open),
        ..ListFilter::default()
    };
    let list = list_via(&place.link(&fake), &filter).expect("ok");
    let order: Vec<u64> = list.list.tasks.iter().map(|t| t.number).collect();
    assert_eq!(order.len(), 101, "each task once");
    assert_eq!(&order[..3], &[5, 150, 101]);
    let paths: Vec<String> = fake.requests().into_iter().map(|r| r.1).collect();
    assert!(
        paths
            .iter()
            .any(|p| p.contains("state=open") && p.contains("priority=high")),
        "{paths:?}"
    );
}

/// Review of #12 (follow-up): the high answer is a page too. With 101 high
/// tasks Hive sends the newest 100, so #1 is missing: the listing is marked
/// partial, in its rows and its text, never silently incomplete.
#[test]
fn a_full_high_page_marks_the_listing_partial() {
    let fake = Fake::start(|_, _, _| {
        let row = fixture("task_list.response.json")["tasks"][1].clone();
        let mut list = fixture("task_list.response.json");
        // Hive: ORDER BY number DESC LIMIT 100, the priority filter or not.
        let page: Vec<Value> = (2..=101)
            .rev()
            .map(|number| {
                let mut task = row.clone();
                task["number"] = json!(number);
                task["priority"] = json!("high");
                task
            })
            .collect();
        list["tasks"] = json!(page);
        (200, list)
    });
    let place = Place::new();
    let listing = list_via(&place.link(&fake), &ListFilter::default()).expect("ok");
    assert!(listing.partial);
    assert_eq!(listing.list.tasks.len(), 100);
    let text = render_listing(&listing, 10);
    assert!(text.ends_with(PARTIAL_NOTE), "{text}");
    let json = serde_json::to_value(&listing).expect("json");
    assert_eq!(json["partial"], json!(true));

    // A whole answer is not partial, and says nothing.
    let small = hive_for_task_7();
    let whole = list_via(&place.link(&small), &ListFilter::default()).expect("ok");
    assert!(!whole.partial);
    assert!(!render_listing(&whole, 10).contains("partial"));
}

/// No Hive serves `POST …/tasks/{n}/amend` yet: a vault amend checks the
/// request and the task (one read) and refuses honestly, sending no write;
/// someone else's result is refused as not theirs.
#[test]
fn a_vault_amend_fails_closed_until_hive_serves_the_route() {
    let place = Place::new();
    let fake = Fake::start(|method, path, _| {
        let mut done = fixture("task.done.response.json");
        done["verified_by"] = json!({"kind": "instance", "label": "ada-laptop",
                                     "agent": "rev-1", "mine": true});
        match (
            method,
            path.strip_prefix(&format!("/vaults/{PROJECT}/tasks")),
        ) {
            ("GET", Some("/7")) => (200, done),
            _ => (
                404,
                json!({"error": {"code": "not_found", "message": path}}),
            ),
        }
    });
    let link = place.link(&fake);
    let merge = || AmendInput {
        merge_commit: Some("6aded74fcf".into()),
        ..AmendInput::default()
    };
    let refused = amend_via(&link, 7, "claude-3e", merge()).unwrap_err();
    assert!(
        refused.to_string().contains("not supported by Hive yet"),
        "{refused}"
    );
    assert!(refused.to_string().contains(AMEND_ROUTE), "{refused}");
    let other = amend_via(&link, 7, "codex-reviewer", merge()).unwrap_err();
    assert!(other.to_string().contains("not_submitter"), "{other}");
    // Never the reviewer, even as a Queen session.
    let queen_reviewer = AmendInput {
        queen: true,
        ..merge()
    };
    let reviewer = amend_via(&link, 7, "rev-1", queen_reviewer).unwrap_err();
    assert!(reviewer.to_string().contains("not_merger"), "{reviewer}");
    // A Queen session that did not review passes the rule (and then meets
    // the missing route).
    let queen = AmendInput {
        queen: true,
        ..merge()
    };
    let unsupported = amend_via(&link, 7, "queen-1", queen).unwrap_err();
    assert!(
        unsupported
            .to_string()
            .contains("not supported by Hive yet"),
        "{unsupported}"
    );
    // A done task takes only a merge record.
    let replace = AmendInput {
        evidence: vec![commit()],
        ..AmendInput::default()
    };
    let wrong = amend_via(&link, 7, "claude-3e", replace).unwrap_err();
    assert!(wrong.to_string().contains("record_merge_only"), "{wrong}");
    assert!(
        fake.requests().iter().all(|(method, _, _)| method == "GET"),
        "no write left the machine: {:?}",
        fake.requests()
    );
}

/// A Hive with process task 7 (risk M): its view carries what G6 needs; a
/// result with a G6 record makes it `submitted` by this Instance (as the
/// machine's shared worker), and the stored record comes back in the view.
fn hive_with_process_task(spec: &str) -> Fake {
    hive_with_process(
        json!({"risk": "M", "spec_commit": spec, "locked_paths": super::gate::tests::locked(),
        "test_command": "test -f tests/refund.rs", "features": ["F1"]}),
    )
}

/// The same Hive with a given process: an M task needs a real cargo run,
/// since only a run that writes the harness report passes (review r5 R5-3).
fn hive_with_process(process: Value) -> Fake {
    let view = Arc::new(Mutex::new({
        let mut task = fixture("task.claimed.response.json");
        task["process"] = process;
        task
    }));
    Fake::start(move |method, path, body| {
        let tail = path
            .strip_prefix(&format!("/vaults/{PROJECT}/tasks"))
            .unwrap_or(path);
        match (method, tail) {
            ("POST", "/7/claim") => (200, claim_ack(7, 7, 10)),
            ("GET", "/7") => (200, view.lock().unwrap().clone()),
            ("POST", "/7/result") => {
                let mut task = view.lock().unwrap();
                let obj = task.as_object_mut().unwrap();
                obj.remove("claim");
                obj.insert("state".into(), "submitted".into());
                obj.insert(
                    "result".into(),
                    json!({"by": {"kind": "instance", "label": "pounce",
                                  "agent": "wkr_ic7yo2evmqb6zhrg6v23", "mine": true},
                           "at": "2026-11-06T02:00:00Z", "proposal": 0,
                           "evidence": body["evidence"].as_array().unwrap().iter()
                               .map(|e| json!({"kind": e["kind"], "reference": e["reference"], "verified": false}))
                               .collect::<Vec<_>>()}),
                );
                // Hive stores the Instance's part with what only it adds:
                // the contract shape, `ci` for the commit, a pass.
                let ci = super::process::tests::green_ci(&body["gate_record"]["artifact_commit"]);
                task["process"]["g6"] =
                    super::process::tests::as_hive_record(&body["gate_record"], Some(ci));
                (200, ack(7, "submitted"))
            }
            ("POST", "/7/verify") => (200, ack(7, "done")),
            _ => (
                404,
                json!({"error": {"code": "not_found", "message": tail}}),
            ),
        }
    })
}

fn process_link<'a>(data: &'a Path, root: &'a Path, fake: &Fake) -> Link<'a> {
    Link {
        data_dir: data,
        vault_id: PROJECT,
        root,
        via: Via::Origin(fake.origin.clone()),
        budget: BUDGET,
        deadline: std::cell::Cell::new(None),
        workers: None,
        presence: &test_presence,
        announce: None,
    }
}

fn posted(fake: &Fake, tail: &str) -> Vec<Value> {
    let path = format!("/vaults/{PROJECT}/tasks{tail}");
    fake.requests()
        .into_iter()
        .filter(|(method, p, _)| method == "POST" && *p == path)
        .map(|(_, _, body)| body)
        .collect()
}

/// Process v1 end to end: the claim notes its start commit, the submit
/// runs G6 in the Instance and sends the green record, and a verify on the
/// same Instance is refused for the session that did the work, refused
/// for the shared machine worker, and allowed for another session.
#[test]
#[cfg_attr(
    windows,
    ignore = "the G6 gate run is Unix-only (passwd home, rustup PATH)"
)]
fn a_process_task_goes_through_g6_and_another_session_verifies_it() {
    let (repo, spec) = super::gate::tests::cargo_repo();
    let fake = hive_with_process(json!({"risk": "M", "spec_commit": spec,
        "locked_paths": super::gate::tests::cargo_locked(),
        "test_command": super::gate::tests::cargo_command(), "features": ["F1"]}));
    let data = tempfile::tempdir().unwrap();
    let link = process_link(data.path(), &repo.root, &fake);
    claim_via(&link, 7, "mcp-100-worker", "s", None, &admitted, &fresh).unwrap();
    let trace = claims::trace(data.path(), PROJECT, 7).unwrap();
    assert_eq!(trace.start_commit.as_deref(), Some(spec.as_str()));

    repo.write("src/lib.rs", "pub fn refund() -> bool {\n    true\n}\n");
    let head = repo.commit("build");
    let evidence = vec![EvidenceInput {
        kind: "commit".into(),
        reference: head.clone(),
    }];
    let ack = submit_as(
        &link,
        7,
        "mcp-100-worker",
        "Refund done",
        evidence,
        None,
        &submitted,
    )
    .unwrap();
    assert_eq!(ack.state, TaskState::Submitted);
    let result = &posted(&fake, "/7/result")[0];
    let record = &result["gate_record"];
    assert_eq!(record["result"], "blocked", "the Instance does not read CI");
    assert_eq!(record["missing"], json!(["ci_pipeline"]));
    assert_eq!(record["artifact_commit"], head.as_str());
    assert_eq!(record["rerun"]["commit"], head.as_str());
    assert_eq!(record["locked_paths"]["ok"], true);
    assert_eq!(record["locked_paths"]["spec_commit"], spec.as_str());

    // The author's own session: refused here, nothing sent.
    let own = verify_via(&link, 7, "mcp-100-worker", TaskVerdict::Accept, None).unwrap_err();
    assert!(own.to_string().starts_with("self_check"), "{own}");
    // The machine's shared worker is no per-session identity.
    let shared = verify_via(
        &link,
        7,
        "wkr_ic7yo2evmqb6zhrg6v23",
        TaskVerdict::Accept,
        None,
    )
    .unwrap_err();
    assert!(
        shared.to_string().starts_with("no per-session identity"),
        "{shared}"
    );
    assert!(posted(&fake, "/7/verify").is_empty());
    // The submit recorded this test process's app session, if it has one;
    // the worker's own process runs under its own app session.
    let traced = claims::trace(data.path(), PROJECT, 7).unwrap();
    assert_eq!(
        traced.submitted_by.as_ref().map(|s| s.agent.as_str()),
        Some("mcp-100-worker")
    );
    assert_eq!(traced.submitted_commit.as_deref(), Some(head.as_str()));
    assert_eq!(
        traced
            .submitted_by
            .as_ref()
            .and_then(|s| s.process.as_deref()),
        Some("proc-of-mcp-100-worker"),
        "the submitter's registering process, from the local registry"
    );
    // Another session on the same Instance checks it.
    let ack = verify_via(&link, 7, "mcp-200-reviewer", TaskVerdict::Accept, None).unwrap();
    assert_eq!(ack.state, TaskState::Done);
    assert_eq!(posted(&fake, "/7/verify").len(), 1);
}

/// A changed locked test refuses the submit before anything is sent: the
/// claim stays, Hive never sees a result.
#[test]
fn a_process_submit_with_changed_locked_paths_sends_nothing() {
    let (repo, spec) = super::gate::tests::spec_repo();
    let fake = hive_with_process_task(&spec);
    let data = tempfile::tempdir().unwrap();
    let link = process_link(data.path(), &repo.root, &fake);
    claim_via(&link, 7, "mcp-100-worker", "s", None, &admitted, &fresh).unwrap();
    repo.write(
        "tests/refund.rs",
        "#[test] fn refunds() { /* weakened */ }\n",
    );
    let head = repo.commit("weaken the locked test");
    let evidence = vec![EvidenceInput {
        kind: "commit".into(),
        reference: head,
    }];
    let refused = submit_as(
        &link,
        7,
        "mcp-100-worker",
        "Done",
        evidence,
        None,
        &submitted,
    )
    .unwrap_err()
    .to_string();
    assert!(refused.contains("locked_paths_changed"), "{refused}");
    assert!(refused.contains("tests/refund.rs modified"), "{refused}");
    assert!(posted(&fake, "/7/result").is_empty());
    assert!(
        claims::find(data.path(), PROJECT, 7).is_some(),
        "the claim stays"
    );
    // Nothing was submitted, so no verify can pass either.
    let none = verify_via(&link, 7, "mcp-200-reviewer", TaskVerdict::Accept, None).unwrap_err();
    assert!(none.to_string().contains("no submitted result"), "{none}");
}
