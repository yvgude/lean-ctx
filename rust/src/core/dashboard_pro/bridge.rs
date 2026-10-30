// SPDX-License-Identifier: Apache-2.0
//! One cockpit analysis in two engine phases around the cockpit's own
//! computation: `prepare` checks the request and collects its inputs,
//! `finish` checks the cockpit's answer and returns the body, a file, or the
//! executed plans.

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use super::collect::{self, RouteSource};
use super::effects::{self, Ctx, Notifier, Outcome};
use super::request::{self, BridgeRequest, Op};
use super::wire::{self, GuardEvent};
use super::{ProError, ProResult, ProjectScope, watch};

/// One analysis frame: requests and answers are at most 1 MiB.
pub(crate) const MAX_FRAME_BYTES: usize = 1 << 20;

pub(crate) struct Env<'a> {
    pub(crate) scope: &'a ProjectScope,
    pub(crate) routes: &'a dyn RouteSource,
    pub(crate) notifier: &'a dyn Notifier,
    pub(crate) now: DateTime<Utc>,
}

/// An HTTP answer for the cockpit.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Reply {
    pub(crate) status: u16,
    pub(crate) body: Value,
}

impl Reply {
    fn ok(body: Value) -> Self {
        Self { status: 200, body }
    }

    fn error(error: &ProError) -> Self {
        Self {
            status: error.status,
            body: super::error_body(error),
        }
    }
}

/// `POST /api/dashboard-analysis/inputs` `{op, method, query, body}` →
/// `{request}` for the cockpit's computation. `alerts.evaluate` answers
/// `{skip: true}` when there is nothing to evaluate, else also
/// `guard_cursor`, which the cockpit hands back to `apply`.
pub(crate) fn handle_inputs(env: &Env<'_>, raw: &str) -> Reply {
    let request = match request::parse(raw).and_then(|r| scoped(env, r)) {
        Ok(request) => request,
        Err(error) => return Reply::error(&error),
    };
    let (guard_new, guard_cursor) = if request.op == Op::AlertsEvaluate {
        match watch::pending(env.scope) {
            Ok(Some((events, cursor))) => (Some(events), Some(cursor)),
            Ok(None) => return Reply::ok(json!({ "skip": true })),
            Err(error) => return Reply::error(&error),
        }
    } else {
        (None, None)
    };
    match prepare(env, &request, guard_new) {
        Ok(prepared) => {
            let mut body = json!({ "request": prepared });
            if let Some(cursor) = guard_cursor {
                body["guard_cursor"] = json!(cursor);
            }
            Reply::ok(body)
        }
        Err(error) => Reply::error(&error),
    }
}

/// `POST /api/dashboard-analysis/apply` `{op, method, query, body, response,
/// guard_cursor?}` → `{status, body | file | applied}`; a refusal keeps its
/// status and answers `{error}`.
pub(crate) fn handle_apply(env: &Env<'_>, raw: &str) -> Reply {
    if raw.len() > MAX_FRAME_BYTES + request::MAX_REQUEST_BYTES {
        return Reply::error(&ProError::new(413, "Request too large"));
    }
    let Ok(Value::Object(mut object)) = serde_json::from_str::<Value>(raw) else {
        return Reply::error(&ProError::bad("Invalid JSON request"));
    };
    let response = object.remove("response");
    let guard_cursor = object.remove("guard_cursor");
    let request = match request::from_object(object).and_then(|r| scoped(env, r)) {
        Ok(request) => request,
        Err(error) => return Reply::error(&error),
    };
    let Some(Ok(response)) = response.map(serde_json::from_value::<wire::Response>) else {
        return Reply::error(&ProError::bad(
            "The analysis answer is missing or unreadable",
        ));
    };
    let cursor = match (request.op, guard_cursor.as_ref().map(Value::as_u64)) {
        (Op::AlertsEvaluate, Some(Some(cursor))) => Some(cursor),
        (Op::AlertsEvaluate, _) => {
            return Reply::error(&ProError::bad("alerts.evaluate needs its guard_cursor"));
        }
        (_, Some(_)) => return Reply::error(&ProError::bad("Unknown request field")),
        (_, None) => None,
    };
    let mut body = match finish(env, &request, response) {
        Ok(body) => body,
        Err(error) => return Reply::error(&error),
    };
    // The cursor moves only once the evaluated events were answered.
    if let Some(cursor) = cursor
        && let Err(error) = watch::advance(env.scope, cursor)
    {
        return Reply::error(&error);
    }
    body["status"] = json!(200);
    Reply::ok(body)
}

/// The device scope reads every project's stores; it serves only ops that
/// read device-wide stores, whichever route built it.
fn scoped(env: &Env<'_>, request: BridgeRequest) -> ProResult<BridgeRequest> {
    if env.scope.is_device() && !request.op.device_wide() {
        return Err(ProError::bad(
            "Choose a project: this view reads one project's data",
        ));
    }
    Ok(request)
}

/// Collects the op's inputs into one bounded analysis request.
pub(crate) fn prepare(
    env: &Env<'_>,
    request: &BridgeRequest,
    guard_new: Option<Vec<GuardEvent>>,
) -> ProResult<wire::Request> {
    let collected = collect::inputs(request, env.scope, env.routes, env.now, guard_new)?;
    let mut payload = wire::Request {
        schema: wire::SCHEMA.to_string(),
        op: request.op.name().to_string(),
        now_utc: env.now.to_rfc3339(),
        project: env.scope.project(),
        query: request.query.clone(),
        body: Value::Object(request.body.clone()),
        inputs: collected.inputs,
    };
    let size = match request.op {
        Op::WorkAnalysis => fit_newest_episodes(&mut payload, MAX_FRAME_BYTES)?,
        // Knowledge has no fact limit: a review reads the newest facts that
        // fit; `facts_total` still counts them all.
        Op::MemoryAnalyze | Op::MemorySnapshot => {
            fit_newest(&mut payload, "facts", MAX_FRAME_BYTES)?
        }
        _ => encoded_len(&payload)?,
    };
    if size > MAX_FRAME_BYTES {
        return Err(ProError::new(
            413,
            "This view needs more data than one analysis request can carry; narrow the period or filters, or consolidate memory first",
        ));
    }
    Ok(payload)
}

fn encoded_len(payload: &wire::Request) -> ProResult<usize> {
    serde_json::to_vec(payload)
        .map(|bytes| bytes.len())
        .map_err(|_| ProError::internal("The request could not be encoded"))
}

/// A long-lived project's sessions outgrow one frame: the work analysis then
/// reads the newest sessions that fit, dropping the oldest a tenth at a time.
/// Returns the encoded size it ends with.
pub(crate) fn fit_newest_episodes(payload: &mut wire::Request, max: usize) -> ProResult<usize> {
    fit_newest(payload, "episodes", max)
}

/// Drops the oldest tenth of the oldest-first list input `key` until the
/// request fits `max` bytes. Returns the encoded size it ends with.
pub(crate) fn fit_newest(payload: &mut wire::Request, key: &str, max: usize) -> ProResult<usize> {
    let mut size = encoded_len(payload)?;
    while size > max {
        let Some(Value::Array(items)) = payload.inputs.get_mut(key) else {
            break;
        };
        if items.is_empty() {
            break;
        }
        let drop = items.len().div_ceil(10);
        items.drain(..drop);
        size = encoded_len(payload)?;
    }
    Ok(size)
}

/// Checks the cockpit's answer and executes its plans; returns `{body}`,
/// `{file}` or `{applied, body}`.
pub(crate) fn finish(
    env: &Env<'_>,
    request: &BridgeRequest,
    response: wire::Response,
) -> ProResult<Value> {
    match response.status {
        200 => {}
        400 | 404 | 409 => {
            let message = response
                .error
                .map(|e| e.message)
                .filter(|m| super::plain_text(m, 500))
                .unwrap_or_else(|| "The request was refused".into());
            return Err(ProError::new(response.status, message));
        }
        _ => return Err(ProError::new(502, "The analysis answer is unreadable")),
    }
    if let Some(plans) = response.plans.filter(|p| !p.is_empty()) {
        let plans = effects::parse_plans(request.op, plans)?;
        let ctx = Ctx {
            scope: env.scope,
            request,
            now: env.now,
            notifier: env.notifier,
        };
        let mut applied = Vec::new();
        for outcome in effects::apply(&ctx, plans)? {
            match outcome {
                Outcome::Applied(readback) => applied.push(readback),
                Outcome::File(file) => return Ok(json!({ "file": file })),
            }
        }
        return Ok(json!({ "applied": applied, "body": response.body }));
    }
    if request.op.is_write() {
        // A write answered without a plan only reports (e.g. a preview).
        return Ok(json!({ "body": response.body }));
    }
    if let Some(file) = response.file {
        if !super::plain_text(&file.name, 120)
            || !super::plain_text(&file.mime, 100)
            || file.name.contains(['/', '\\'])
        {
            return Err(ProError::new(502, "The analysis file is refused"));
        }
        return Ok(json!({ "file": file }));
    }
    Ok(json!({ "body": response.body }))
}

/// The HTTP status line of a reply.
pub(crate) fn status_line(status: u16) -> &'static str {
    match status {
        200 => "200 OK",
        400 => "400 Bad Request",
        404 => "404 Not Found",
        405 => "405 Method Not Allowed",
        409 => "409 Conflict",
        413 => "413 Payload Too Large",
        502 => "502 Bad Gateway",
        _ => "500 Internal Server Error",
    }
}
