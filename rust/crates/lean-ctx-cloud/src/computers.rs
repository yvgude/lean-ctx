// SPDX-License-Identifier: Apache-2.0
//! LeanCTX Cloud computers (`/v1/computers`, cloud design `cloud-computer.md`
//! §4.3): list, create, start and stop the computers of the person and of
//! their teams, for `lean-ctx cloud computer …` and LeanCTX Code.
//!
//! The cloud enforces who may create or start a computer, its role, the free
//! hours, the balance and the trial; this module only passes the request on
//! and hands back what the cloud answered:
//! - `200`/`201`/`202` with a row (or `{computers: […]}`) → [`Answer::Done`];
//! - an error with a code (`402 balance_empty`, `402 plan_required`,
//!   `403 forbidden`, `404 no_computer`, `409 computer_exists`,
//!   `409 computer_busy`, `400`/`422 invalid_request`, `429`,
//!   `428 confirmation_required`, `503 computer_provider_unavailable`,
//!   `503 computer_not_available`) → [`Answer::Refused`], with the cloud's
//!   own message and `error.details`;
//! - `404` without a code, or `503 not_configured`, from a cloud that does
//!   not offer computers, and a list `200 {available: false}` (no provider
//!   yet) → [`Answer::NotOffered`].
//!
//! A create or a start costs money, so the CLI sends one only after the
//! person confirmed the terms (`--yes`), and always with `confirm: true`.
//! A cloud that still wants a confirmation answers
//! `428 confirmation_required` with the terms in `error.details`
//! (`{billing: free|billed, usd_per_hour, free_hours_left, topup_usd}`,
//! read with [`terms`]) and does nothing: a second guard.
//!
//! Every exchange goes through an [`Exchange`]: [`connected`] for the connected
//! instance (the session refreshes a refused token), [`on_client`] for one
//! [`CloudClient`].
//!
//! No code and no file content is ever sent: the bodies carry only the
//! request id, the name, the role, the team and the confirmation.

use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::CloudError;
use crate::config::CloudConfig;
use crate::state::CloudStore;
use crate::transport::CloudClient;

/// The cloud route (under `/v1`).
const PATH: &str = "/computers";
/// Largest answer read.
const MAX_ANSWER_BYTES: usize = 256 * 1024;
/// Most rows kept from a list.
pub const MAX_ROWS: usize = 64;
/// Longest cloud message passed on.
const MAX_MESSAGE_LEN: usize = 500;
/// Longest wait passed on (a day).
const RETRY_AFTER_CAP_S: u64 = 86_400;

/// The role the free Team hours apply to (owner, 18.11.).
pub const ROLE_WEAVE_HOST: &str = "weave_host";
/// The role of any other computer; the only one for a personal account.
pub const ROLE_GENERAL: &str = "general";
/// The roles a computer is created with; fixed at create.
pub const ROLES: [&str; 2] = [ROLE_GENERAL, ROLE_WEAVE_HOST];

/// The cloud refused the request; passed on as it said it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Refusal {
    /// HTTP status.
    pub status: u16,
    /// `error.code`, or `http_<status>` when the cloud sent none.
    pub code: String,
    /// `error.message`, when the cloud sent one (bounded).
    pub message: Option<String>,
    /// `error.next_step` (`top_up`), when sent.
    pub next_step: Option<String>,
    /// Seconds after which asking again may help (`429`).
    pub retry_after_s: Option<u64>,
    /// `error.details`, when it is an object (the terms of a `428`).
    pub details: Option<Map<String, Value>>,
    /// The specific reason: `error.reason`, else `error.details.reason`
    /// (a `409 invalid_state` with reason `computer_exists`).
    pub reason: Option<String>,
}

/// The terms of a billable write, from a `428 confirmation_required`
/// (`error.details`): one shape for the cloud, the CLI and LeanCTX Code.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Terms {
    /// `free` (the team's free `weave_host` hours) or `billed` (paid from
    /// the top-up, the only source of computer minutes); `None` when the
    /// cloud named neither.
    pub billing: Option<&'static str>,
    pub usd_per_hour: Option<f64>,
    pub free_hours_left: Option<f64>,
    /// What is left of the top-up, in USD.
    pub topup_usd: Option<f64>,
}

/// The terms in `details`; anything malformed is `None`, never a guess.
#[must_use]
pub fn terms(details: Option<&Map<String, Value>>) -> Terms {
    let num = |k: &str| {
        details
            .and_then(|d| d.get(k))
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite() && *n >= 0.0)
    };
    Terms {
        billing: match details
            .and_then(|d| d.get("billing"))
            .and_then(Value::as_str)
        {
            Some("free") => Some("free"),
            Some("billed") => Some("billed"),
            _ => None,
        },
        usd_per_hour: num("usd_per_hour"),
        free_hours_left: num("free_hours_left"),
        // p37 names it `balance_usd` (top-up only, owner 19.11.).
        topup_usd: num("topup_usd").or_else(|| num("balance_usd")),
    }
}

/// What the cloud said.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer<T> {
    Done(T),
    Refused(Refusal),
    /// This cloud does not offer computers (older, or switched off).
    NotOffered,
}

/// `GET /v1/computers`, checked.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ComputerList {
    /// The rows, as the cloud sent them (malformed rows left out); each
    /// carries its scope's `balance` (§4.3 sends no top-level balance).
    pub computers: Vec<Map<String, Value>>,
}

/// `POST /v1/computers` body (`{ request_id, name, team?, role, confirm }`).
/// The cloud picks size and region and does not stop an idle computer, so
/// the CLI offers no size, region or idle stop: a computer runs, and is
/// billed, until it is stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CreateRequest {
    pub request_id: String,
    pub name: String,
    /// The team's org id; absent for a personal computer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    pub role: String,
    /// The person confirmed the terms (`--yes`). The CLI sends a create
    /// only when this is `true`; a cloud that sees `false` answers
    /// `428 confirmation_required` and creates nothing.
    pub confirm: bool,
}

/// A fresh request id: a retry of the same command passes it again with
/// `--request-id`, and the cloud answers the first answer.
#[must_use]
pub fn new_request_id() -> String {
    format!("req_{}", uuid::Uuid::new_v4().simple())
}

/// A request id the cloud accepts: 8-64 of `A-Z a-z 0-9 _ -`.
#[must_use]
pub fn valid_request_id(id: &str) -> bool {
    (8..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// A computer id as the cloud sends it (`cmp_…`), safe in a path.
#[must_use]
pub fn valid_id(id: &str) -> bool {
    id.starts_with("cmp_")
        && (5..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// One exchange with the cloud: method, path (under `/v1`) and body → the
/// HTTP status and the JSON answer.
pub type Exchange<'a> = &'a dyn Fn(&str, &str, Option<&Value>) -> Result<(u16, Value), CloudError>;

/// The exchanges of the connected instance: each one through its session,
/// which refreshes a refused token.
pub fn connected<'a>(
    store: &'a CloudStore,
    config: &'a CloudConfig,
) -> impl Fn(&str, &str, Option<&Value>) -> Result<(u16, Value), CloudError> + 'a {
    move |method, path, body| {
        let session = crate::scheduler::connected_session(store, config)?;
        session.with_client(|client| on_client(client)(method, path, body))
    }
}

/// The exchanges on `client`.
pub fn on_client(
    client: &CloudClient,
) -> impl Fn(&str, &str, Option<&Value>) -> Result<(u16, Value), CloudError> + '_ {
    move |method, path, body| {
        let (status, answer) = client.send_answer(method, path, body, MAX_ANSWER_BYTES)?;
        // A refused token goes through the session's refresh.
        if status == 401 {
            return Err(CloudError::from_response(status, &answer));
        }
        Ok((status, answer))
    }
}

/// `GET /v1/computers`.
pub fn list(send: Exchange<'_>) -> Result<Answer<ComputerList>, CloudError> {
    let (status, body) = send("GET", PATH, None)?;
    list_of(status, &body)
}

/// The list of `status` and `body`. `200 {available: false}` (no provider
/// yet, so no computers and no prices) is [`Answer::NotOffered`] like a
/// cloud without the route; a missing `available` means available.
fn list_of(status: u16, body: &Value) -> Result<Answer<ComputerList>, CloudError> {
    if (200..300).contains(&status) && body.get("available") == Some(&Value::Bool(false)) {
        return Ok(Answer::NotOffered);
    }
    classify(status, body, |body| {
        let rows = body
            .get("computers")
            .and_then(Value::as_array)
            .ok_or_else(|| CloudError::Protocol("computers: no `computers` list".into()))?;
        Ok(ComputerList {
            computers: rows.iter().filter_map(checked_row).take(MAX_ROWS).collect(),
        })
    })
}

/// `POST /v1/computers`: the new row (`provisioning`).
pub fn create(
    send: Exchange<'_>,
    request: &CreateRequest,
) -> Result<Answer<Map<String, Value>>, CloudError> {
    let body = serde_json::to_value(request)
        .map_err(|e| CloudError::Protocol(format!("encode request: {e}")))?;
    let (status, answer) = send("POST", PATH, Some(&body))?;
    classify(status, &answer, row_of)
}

/// `POST /v1/computers/{id}/start` or `/stop`: the row (`starting` or
/// `stopping`). `confirm` (start only) as in [`CreateRequest::confirm`].
pub fn transition(
    send: Exchange<'_>,
    id: &str,
    action: Transition,
    request_id: &str,
    confirm: bool,
) -> Result<Answer<Map<String, Value>>, CloudError> {
    if !valid_id(id) {
        return Err(CloudError::Protocol(format!("computers: bad id '{id}'")));
    }
    let path = format!("{PATH}/{id}/{}", action.as_str());
    let body = match action {
        Transition::Start => json!({ "request_id": request_id, "confirm": confirm }),
        Transition::Stop => json!({ "request_id": request_id }),
    };
    let (status, answer) = send("POST", &path, Some(&body))?;
    classify(status, &answer, row_of)
}

/// A start or a stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    Start,
    Stop,
}

impl Transition {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
        }
    }
}

/// The answer of `status` and `body`; `done` reads a success body.
fn classify<T>(
    status: u16,
    body: &Value,
    done: impl FnOnce(&Value) -> Result<T, CloudError>,
) -> Result<Answer<T>, CloudError> {
    if (200..300).contains(&status) {
        return done(body).map(Answer::Done);
    }
    let code = body["error"]["code"].as_str().unwrap_or_default();
    if (status == 404 && (code.is_empty() || code == "not_found"))
        || (status == 503 && code == "not_configured")
    {
        return Ok(Answer::NotOffered);
    }
    if code.is_empty() && !matches!(status, 400 | 402 | 403 | 409 | 422 | 428 | 429 | 503) {
        return Err(CloudError::from_response(status, body));
    }
    let text = |key: &str| {
        body["error"][key]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| bounded(s, MAX_MESSAGE_LEN))
    };
    let details = body["error"]["details"].as_object().cloned();
    let reason = body["error"]["reason"]
        .as_str()
        .or_else(|| details.as_ref()?.get("reason")?.as_str())
        .map(|r| bounded(r, 64));
    Ok(Answer::Refused(Refusal {
        status,
        code: if code.is_empty() {
            format!("http_{status}")
        } else {
            bounded(code, 64)
        },
        message: text("message"),
        next_step: text("next_step"),
        retry_after_s: body["error"]["retry_after_s"]
            .as_u64()
            .map(|s| s.min(RETRY_AFTER_CAP_S)),
        details,
        reason,
    }))
}

/// `POST /v1/computers/{id}/start` body for `lean-ctx run --cloud`
/// (contract §35). A run is billed from the first minute (free hours are
/// only for `weave_host`), so this body exists only after the person
/// confirmed the terms (`--yes`, or yes at the prompt): `confirm` is
/// always `true`. Carries no code, no task text, no file content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunStartRequest {
    pub request_id: String,
    /// Always `run`: the cloud bills it from the first minute.
    pub purpose: &'static str,
    /// Always `true`: an unconfirmed paid start is never sent.
    pub confirm: bool,
}

impl RunStartRequest {
    /// The body of a start the person confirmed.
    #[must_use]
    pub fn confirmed(request_id: String) -> Self {
        Self {
            request_id,
            purpose: "run",
            confirm: true,
        }
    }
}

/// The terms of a run on computer `row`, from the list row alone (no
/// request to the cloud): billed from the first minute, at the row's price
/// (else its balance's), with what is left of the top-up as listed
/// (`topup_usd`). `purpose: "run"` is always `billed`: the free hours are a
/// team's `weave_host`'s, never a run's. A malformed value is `None`, never
/// a guess.
#[must_use]
pub fn run_terms(row: &Map<String, Value>) -> Terms {
    let price = row
        .get("price")
        .and_then(|p| p.get("usd_per_hour"))
        .or_else(|| row.get("balance").and_then(|b| b.get("usd_per_hour")));
    let mut details = Map::new();
    details.insert("billing".into(), "billed".into());
    if let Some(price) = price {
        details.insert("usd_per_hour".into(), price.clone());
    }
    if let Some(usd) = row.get("balance").and_then(|b| b.get("usd")) {
        details.insert("topup_usd".into(), usd.clone());
    }
    terms(Some(&details))
}

/// `POST /v1/computers/{id}/start` for a confirmed run (see
/// [`RunStartRequest`]): the row (`starting`, or `running` when it already
/// ran), or the cloud's refusal.
pub fn start_for_run(
    send: Exchange<'_>,
    id: &str,
    request: &RunStartRequest,
) -> Result<Answer<Map<String, Value>>, CloudError> {
    if !valid_id(id) {
        return Err(CloudError::Protocol(format!("computers: bad id '{id}'")));
    }
    if !request.confirm {
        return Err(CloudError::Protocol(
            "computers: a run start is sent only when confirmed".into(),
        ));
    }
    let path = format!("{PATH}/{id}/start");
    let body = serde_json::to_value(request)
        .map_err(|e| CloudError::Protocol(format!("encode request: {e}")))?;
    let (status, answer) = send("POST", &path, Some(&body))?;
    classify(status, &answer, row_of)
}

fn bounded(text: &str, max: usize) -> String {
    text.chars().filter(|c| !c.is_control()).take(max).collect()
}

/// A row with a string `id`, `name` and `state`; anything else is left out.
fn checked_row(row: &Value) -> Option<Map<String, Value>> {
    let object = row.as_object()?;
    let text = |k: &str| object.get(k).and_then(Value::as_str);
    let id = text("id")?;
    if !valid_id(id) || text("name")?.is_empty() || text("state")?.is_empty() {
        return None;
    }
    Some(object.clone())
}

/// A write's answer: the row itself, or `{computer: row}`.
fn row_of(body: &Value) -> Result<Map<String, Value>, CloudError> {
    checked_row(body.get("computer").unwrap_or(body))
        .ok_or_else(|| CloudError::Protocol("computers: the answer is not a computer row".into()))
}

/// Who pays a start, from the row the cloud answered (§3.3), in the words
/// of [`Terms::billing`]: a team's `weave_host` computer with free hours
/// left starts on them (`free`); a personal computer never does (the free
/// hours are the team's) and a `general` one never does: they are `billed`
/// (paid from the top-up). A team's `weave_host` whose row carries no
/// readable `balance.free_hours_left` is `None`: unknown, never guessed.
/// The cloud decides; this only names it.
#[must_use]
pub fn start_basis(row: &Map<String, Value>) -> Option<&'static str> {
    let team = row
        .get("scope")
        .and_then(|s| s.get("kind"))
        .and_then(Value::as_str)
        == Some("team");
    if !team || row.get("role").and_then(Value::as_str) != Some(ROLE_WEAVE_HOST) {
        return Some("billed");
    }
    let free = row
        .get("balance")
        .and_then(|b| b.get("free_hours_left"))
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && *n >= 0.0)?;
    Some(if free > 0.0 { "free" } else { "billed" })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, name: &str, state: &str) -> Value {
        json!({ "id": id, "name": name, "state": state, "role": "weave_host",
                "balance": { "usd": 12.4, "state": "ok", "free_hours_left": 494.8, "usd_per_hour": 0.09 } })
    }

    #[test]
    fn a_list_keeps_well_formed_rows() {
        let body = json!({
            "computers": [row("cmp_1", "weave-host", "running"), { "name": "x" }, row("bad id", "y", "running")]
        });
        let Answer::Done(list) = classify(200, &body, |b| {
            Ok(ComputerList {
                computers: b["computers"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(checked_row)
                    .collect(),
            })
        })
        .expect("ok") else {
            panic!("not done");
        };
        assert_eq!(list.computers.len(), 1);
        assert_eq!(list.computers[0]["name"], "weave-host");
        assert_eq!(list.computers[0]["balance"]["state"], "ok");
    }

    #[test]
    fn refusals_pass_the_cloud_code_and_message() {
        let body = json!({ "error": { "code": "balance_empty", "message": "Top up to start.", "next_step": "top_up" } });
        let Answer::Refused(refusal) = classify(402, &body, row_of).expect("refusal") else {
            panic!("not refused");
        };
        assert_eq!(refusal.code, "balance_empty");
        assert_eq!(refusal.status, 402);
        assert_eq!(refusal.message.as_deref(), Some("Top up to start."));
        assert_eq!(refusal.next_step.as_deref(), Some("top_up"));
        let busy = json!({ "error": { "code": "computer_busy" } });
        let Answer::Refused(refusal) = classify(409, &busy, row_of).expect("refusal") else {
            panic!("not refused");
        };
        assert_eq!(refusal.code, "computer_busy");
        assert!(refusal.message.is_none());
        let limited = json!({ "error": { "code": "rate_limited", "retry_after_s": 999_999 } });
        let Answer::Refused(refusal) = classify(429, &limited, row_of).expect("refusal") else {
            panic!("not refused");
        };
        assert_eq!(refusal.retry_after_s, Some(RETRY_AFTER_CAP_S));
        let bare = classify(422, &Value::Null, row_of).expect("refusal");
        assert!(matches!(bare, Answer::Refused(r) if r.code == "http_422"));
        let exists = json!({ "error": { "code": "invalid_state", "details": { "reason": "computer_exists" } } });
        let Answer::Refused(refusal) = classify(409, &exists, row_of).expect("refusal") else {
            panic!("not refused");
        };
        assert_eq!(refusal.reason.as_deref(), Some("computer_exists"));
    }

    #[test]
    fn a_confirmation_carries_the_terms() {
        let body = json!({ "error": { "code": "confirmation_required", "message": "Confirm.",
            "details": { "billing": "free", "usd_per_hour": 0.09, "free_hours_left": 494.8, "topup_usd": 12.4 } } });
        let Answer::Refused(refusal) = classify(428, &body, row_of).expect("refusal") else {
            panic!("not refused");
        };
        assert_eq!(refusal.code, "confirmation_required");
        let t = terms(refusal.details.as_ref());
        assert_eq!(t.billing, Some("free"));
        assert_eq!(t.usd_per_hour, Some(0.09));
        assert_eq!(t.free_hours_left, Some(494.8));
        assert_eq!(t.topup_usd, Some(12.4));
        let odd = json!({ "billing": "maybe", "usd_per_hour": -1.0, "topup_usd": "12" });
        let t = terms(odd.as_object());
        assert_eq!(
            t,
            Terms {
                billing: None,
                usd_per_hour: None,
                free_hours_left: None,
                topup_usd: None
            }
        );
        let p37 = json!({ "billing": "billed", "balance_usd": 3.5 });
        assert_eq!(terms(p37.as_object()).topup_usd, Some(3.5));
        assert_eq!(terms(None).billing, None);
    }

    #[test]
    fn a_run_start_body_carries_no_code_and_no_task() {
        let body =
            serde_json::to_value(RunStartRequest::confirmed("req_12345678".into())).expect("json");
        let mut keys: Vec<&str> = body
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["confirm", "purpose", "request_id"]);
        assert_eq!(body["purpose"], "run");
        assert_eq!(body["confirm"], true);
    }

    #[test]
    fn run_terms_come_from_the_list_row() {
        let row = json!({ "id": "cmp_1", "name": "dev", "state": "stopped",
            "price": { "usd_per_hour": 0.09 },
            "balance": { "usd": 12.4, "free_hours_left": 494.8, "usd_per_hour": 0.08 } });
        let t = run_terms(row.as_object().expect("row"));
        assert_eq!(
            t.billing,
            Some("billed"),
            "a run is billed from the first minute"
        );
        assert_eq!(t.usd_per_hour, Some(0.09));
        assert_eq!(t.topup_usd, Some(12.4));
        assert_eq!(
            t.free_hours_left, None,
            "free hours are a weave_host's, never a run's"
        );
        let bare = json!({ "id": "cmp_1", "name": "dev", "state": "stopped",
            "balance": { "usd_per_hour": 0.09, "usd": "x" } });
        let t = run_terms(bare.as_object().expect("row"));
        assert_eq!(t.usd_per_hour, Some(0.09));
        assert_eq!(t.topup_usd, None);
    }

    #[test]
    fn an_unconfirmed_run_start_is_never_sent() {
        let client = crate::transport::CloudClient::new("http://127.0.0.1:9").expect("client");
        let mut request = RunStartRequest::confirmed("req_12345678".into());
        request.confirm = false;
        let error = start_for_run(&on_client(&client), "cmp_1", &request).expect_err("refused");
        assert!(error.to_string().contains("only when confirmed"), "{error}");
    }

    #[test]
    fn a_cloud_without_computers_is_not_offered() {
        assert_eq!(
            classify(404, &Value::Null, row_of).expect("ok"),
            Answer::NotOffered
        );
        let off = json!({ "error": { "code": "not_configured" } });
        assert_eq!(classify(503, &off, row_of).expect("ok"), Answer::NotOffered);
        let gone = json!({ "error": { "code": "no_computer" } });
        assert!(
            matches!(classify(404, &gone, row_of).expect("ok"), Answer::Refused(r) if r.code == "no_computer")
        );
        assert!(classify(500, &Value::Null, row_of).is_err());
    }

    #[test]
    fn a_list_without_a_provider_is_not_offered() {
        let off = json!({ "available": false, "computers": [] });
        assert_eq!(list_of(200, &off).expect("ok"), Answer::NotOffered);
        let on = json!({ "available": true, "computers": [row("cmp_1", "weave-host", "running")] });
        assert!(
            matches!(list_of(200, &on).expect("ok"), Answer::Done(l) if l.computers.len() == 1)
        );
        let older = json!({ "computers": [] });
        assert!(matches!(list_of(200, &older).expect("ok"), Answer::Done(_)));
        let write = json!({ "error": { "code": "computer_not_available", "message": "x" } });
        assert!(
            matches!(classify(503, &write, row_of).expect("ok"), Answer::Refused(r) if r.code == "computer_not_available")
        );
    }

    #[test]
    fn a_write_answer_is_the_row_or_wrapped_row() {
        let plain = row_of(&row("cmp_1", "weave-host", "starting")).expect("row");
        assert_eq!(plain["state"], "starting");
        let wrapped =
            row_of(&json!({ "computer": row("cmp_2", "w", "provisioning") })).expect("row");
        assert_eq!(wrapped["id"], "cmp_2");
        assert!(row_of(&json!({ "ok": true })).is_err());
    }

    #[test]
    fn the_start_basis_follows_scope_role_and_free_hours() {
        let row = |kind: &str, role: &str, free: Option<f64>| {
            let mut row = json!({ "id": "cmp_1", "name": "w", "state": "stopped",
                "scope": { "kind": kind }, "role": role });
            if let Some(free) = free {
                row["balance"] = json!({ "usd": 0.0, "free_hours_left": free });
            }
            row.as_object().cloned().expect("object")
        };
        let basis = |kind, role, free| start_basis(&row(kind, role, free));
        assert_eq!(basis("team", "weave_host", Some(1.5)), Some("free"));
        assert_eq!(basis("team", "weave_host", Some(0.0)), Some("billed"));
        assert_eq!(basis("team", "general", Some(400.0)), Some("billed"));
        assert_eq!(
            basis("team", "weave_host", None),
            None,
            "no balance: the free hours are unknown, never guessed"
        );
        assert_eq!(basis("team", "weave_host", Some(f64::NAN)), None);
        assert_eq!(basis("team", "weave_host", Some(-1.0)), None);
        assert_eq!(
            basis("personal", "weave_host", Some(400.0)),
            Some("billed"),
            "personal computers never run on the team's free hours"
        );
    }

    #[test]
    fn ids_and_request_ids_are_checked() {
        assert!(valid_id("cmp_01HZX"));
        assert!(!valid_id("cmp_../x"));
        assert!(!valid_id("dev"));
        assert!(valid_request_id(&new_request_id()));
        assert!(!valid_request_id("short"));
        assert!(!valid_request_id("req_with space_xx"));
        let body = serde_json::to_value(CreateRequest {
            request_id: "req_12345678".into(),
            name: "weave-host".into(),
            team: None,
            role: ROLE_WEAVE_HOST.into(),
            confirm: true,
        })
        .expect("json");
        assert_eq!(body["confirm"], true);
        assert!(
            body.get("team").is_none(),
            "a personal create sends no team"
        );
        let keys: Vec<&str> = body
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys.len(),
            4,
            "only request_id, name, role and confirm: {keys:?}"
        );
        let body = serde_json::to_value(CreateRequest {
            request_id: "req_12345678".into(),
            name: "dev".into(),
            team: Some("org_acme".into()),
            role: "general".into(),
            confirm: true,
        })
        .expect("json");
        assert_eq!(
            body,
            json!({"request_id": "req_12345678", "name": "dev",
                "team": "org_acme",
                "role": "general", "confirm": true})
        );
    }
}
