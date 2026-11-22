// SPDX-License-Identifier: Apache-2.0
//! Local Service v1 on the daemon socket (`docs/contracts/local-service-v1.md`):
//! the Local Read kinds as routes (`cli::local_read::routes`), and the live
//! event stream `GET /v1/local/events`. Served on the local IPC router only;
//! the socket's 0600 mode is the access control, as for every daemon route.

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::{
    Router,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::sse::{Event as SseEvent, KeepAlive, Sse},
    response::{IntoResponse, Response},
    routing::{MethodRouter, get, post},
};
use serde_json::{Value, json};
use tokio::time::Duration;

use super::AppState;
use crate::cli::local_read::{
    self, Code, Failure,
    routes::{self, Method, Route, VIEW_LIMIT, VIEW_MAX_LIMIT},
};

/// Concurrent event streams this daemon serves.
const MAX_STREAMS: usize = 16;
/// How often a stream looks at its sources.
const POLL: Duration = Duration::from_secs(1);
/// Journal events a stream looks at per poll (the newest).
const EVENT_WINDOW: usize = 32;

static STREAMS: AtomicUsize = AtomicUsize::new(0);

/// The HTTP status of an answer with the error `code`.
pub(crate) fn status_of(code: Option<Code>) -> StatusCode {
    match code {
        None => StatusCode::OK,
        Some(Code::InvalidArgument | Code::NoProject | Code::InvalidField) => {
            StatusCode::BAD_REQUEST
        }
        // LeanCTX Cloud's own refusals, kept for the reader (team chat);
        // `balance_empty` is a LeanCTX computer write the top-up cannot pay.
        Some(Code::PlanRequired | Code::BalanceEmpty) => StatusCode::PAYMENT_REQUIRED,
        Some(Code::NotFound | Code::NoVault | Code::NoComputer) => StatusCode::NOT_FOUND,
        // `sso_required` (engine billing): the person signs in with SSO in
        // the browser; refused like `forbidden`, named apart for the reader.
        // `policy_denied`: LeanCTX Cloud's policy refuses it, named apart.
        Some(
            Code::Forbidden
            | Code::Withheld
            | Code::SsoRequired
            | Code::EmailUnverified
            | Code::PolicyDenied
            | Code::AppOnly,
        ) => StatusCode::FORBIDDEN,
        // `knowledge verify` refused: the anchored code is gone, so the
        // fact has to be rewritten or removed, not confirmed; a local
        // task's state does not allow the write.
        // A LeanCTX computer that already exists or is busy conflicts too.
        Some(
            Code::AnchorGone
            | Code::Conflict
            | Code::InvalidState
            | Code::ComputerExists
            | Code::ComputerBusy,
        ) => StatusCode::CONFLICT,
        Some(
            Code::NotConnected
            | Code::Unavailable
            | Code::ComputerProviderUnavailable
            | Code::ComputerNotAvailable,
        ) => StatusCode::SERVICE_UNAVAILABLE,
        // Cloud rate-limits this instance and LeanCTX keeps no answer to serve.
        Some(Code::RateLimited) => StatusCode::TOO_MANY_REQUESTS,
        // The Cloud does not offer the feature: a refusal, not an outage.
        Some(Code::NotOffered) => StatusCode::NOT_IMPLEMENTED,
        Some(Code::Internal) => StatusCode::INTERNAL_SERVER_ERROR,
        // A billable LeanCTX computer write asked without `--yes`.
        Some(Code::ConfirmationRequired) => StatusCode::PRECONDITION_REQUIRED,
    }
}

/// A Local Read document as a response: the CLI's stdout bytes (document
/// and final `\n`).
fn document_response(status: StatusCode, document: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        format!("{document}\n"),
    )
        .into_response()
}

fn failure_response(failure: &Failure) -> Response {
    document_response(
        status_of(Some(failure.code)),
        &local_read::error_document(failure),
    )
}

/// The header that tells a socket reader the release point masked a value
/// in the body (the masked value stands in its place). Content-free.
pub(crate) const NOTICE_HEADER: &str = "x-leanctx-release-notice";

pub(crate) fn answer_response(answer: &local_read::Answer) -> Response {
    // G08: without a receipt for them, no document bytes are written.
    let Some(body) = answer.body() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let mut response = document_response(status_of(answer.failed), body);
    if let Some(notice) = answer
        .notice
        .as_deref()
        .and_then(|n| header::HeaderValue::from_str(n).ok())
    {
        response.headers_mut().insert(NOTICE_HEADER, notice);
    }
    response
}

async fn respond(
    state: AppState,
    route: &'static Route,
    segment: Option<String>,
    inputs: Vec<(String, Value)>,
) -> Response {
    let work =
        tokio::task::spawn_blocking(move || routes::answer(route, segment.as_deref(), &inputs));
    match tokio::time::timeout(state.timeout, work).await {
        Ok(Ok(answer)) => answer_response(&answer),
        Ok(Err(_)) => failure_response(&Failure::new(Code::Internal, "the read failed")),
        // A write may still complete after this answer: indeterminate.
        Err(_) => failure_response(&Failure::new(
            Code::Unavailable,
            "timed out; a write may still complete",
        )),
    }
}

fn query_inputs(pairs: Vec<(String, String)>) -> Vec<(String, Value)> {
    pairs
        .into_iter()
        .map(|(k, v)| (k, Value::String(v)))
        .collect()
}

/// The entries of a POST body, which must be one JSON object.
fn body_inputs(body: &Bytes) -> Result<Vec<(String, Value)>, Failure> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(Vec::new());
    }
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(map)) => Ok(map.into_iter().collect()),
        _ => Err(Failure::invalid("the body is one JSON object")),
    }
}

/// Whether a request to `route` may be answered: an app-only route
/// (`local_read::app_key`) only with LeanCTX Code's app key.
fn app_admitted(route: &Route, headers: &HeaderMap) -> bool {
    if !local_read::app_key::guards(route.kind) {
        return true;
    }
    let given = headers
        .get(local_read::app_key::HEADER)
        .map(header::HeaderValue::as_bytes);
    local_read::app_key::admits(given)
}

/// [`app_admitted`] off the async loop: the key file is read on a blocking
/// thread, so no file can stall the socket.
async fn app_admitted_off_loop(route: &'static Route, headers: &HeaderMap) -> bool {
    if !local_read::app_key::guards(route.kind) {
        return true;
    }
    let headers = headers.clone();
    tokio::task::spawn_blocking(move || app_admitted(route, &headers))
        .await
        .unwrap_or(false)
}

fn method_router(route: &'static Route) -> MethodRouter<AppState> {
    match (route.method, route.segment().is_some()) {
        (Method::Get, true) => get(
            move |State(state): State<AppState>,
                  Path(segment): Path<String>,
                  Query(q): Query<Vec<(String, String)>>| async move {
                respond(state, route, Some(segment), query_inputs(q)).await
            },
        ),
        (Method::Get, false) => get(
            move |State(state): State<AppState>, Query(q): Query<Vec<(String, String)>>| async move {
                respond(state, route, None, query_inputs(q)).await
            },
        ),
        (Method::Post, _) => post(
            move |State(state): State<AppState>, headers: HeaderMap, body: Bytes| async move {
                if !app_admitted_off_loop(route, &headers).await {
                    return failure_response(&local_read::app_key::refusal());
                }
                match body_inputs(&body) {
                    Ok(inputs) => respond(state, route, None, inputs).await,
                    Err(failure) => failure_response(&failure),
                }
            },
        ),
    }
}

async fn no_route() -> Response {
    failure_response(&Failure::new(
        Code::NotFound,
        "no such route (see /v1/local/capabilities)",
    ))
}

/// The Local Service routes, for the IPC router.
pub(crate) fn router() -> Router<AppState> {
    let mut by_path: BTreeMap<&'static str, MethodRouter<AppState>> = BTreeMap::new();
    for route in routes::all() {
        let handler = method_router(route);
        let merged = match by_path.remove(route.path) {
            Some(existing) => existing.merge(handler),
            None => handler,
        };
        by_path.insert(route.path, merged);
    }
    let mut router = Router::new()
        .route(routes::EVENTS_PATH, get(events))
        .route("/v1/local/{*rest}", axum::routing::any(no_route));
    for (path, handler) in by_path {
        router = router.route(path, handler);
    }
    router
}

// ---------------------------------------------------------------------------
// Live events

/// The query of the event stream; unknown keys are refused like on the
/// routes.
#[derive(Debug, Default)]
struct EventsQuery {
    project_root: Option<String>,
    types: Option<String>,
    /// `message` events of every project (the person's console).
    all: bool,
    /// A bounded view subscription (`messages`) and its parameters.
    view: Option<String>,
    limit: Option<String>,
    thread: Option<String>,
    from: Option<String>,
    to: Option<String>,
    cursor: Option<String>,
}

fn events_query(pairs: Vec<(String, String)>) -> Result<EventsQuery, Failure> {
    let mut q = EventsQuery::default();
    for (key, value) in pairs {
        match key.as_str() {
            "project_root" => q.project_root = Some(value),
            "types" => q.types = Some(value),
            "all" => {
                q.all = match value.as_str() {
                    "true" => true,
                    "false" => false,
                    _ => return Err(Failure::invalid("all is true or false")),
                }
            }
            "view" => q.view = Some(value),
            "limit" => q.limit = Some(value),
            "thread" => q.thread = Some(value),
            "from" => q.from = Some(value),
            "to" => q.to = Some(value),
            "cursor" => q.cursor = Some(value),
            other => return Err(Failure::invalid(format!("unknown parameter {other}"))),
        }
    }
    Ok(q)
}

/// How far before its cursor a delta snapshot starts, so a message a slower
/// writer stamped just before the cursor still arrives (readers keep rows by
/// `id`, so a repeated row is harmless).
const CURSOR_GRACE_MS: i64 = 2_000;
/// The cursor format this service writes; any other is refused.
const CURSOR_PREFIX: &str = "m1.";

/// A bounded subscription to what one view shows: at most `limit` rows,
/// filtered on the server, resumed from `cursor` with a snapshot.
#[derive(Debug, Clone)]
pub(crate) struct View {
    limit: usize,
    thread: Option<String>,
    from: Option<String>,
    to: Option<String>,
    cursor: Option<lean_ctx_time::UtcDateTime>,
}

impl View {
    fn json(&self) -> Value {
        json!({
            "name": "messages",
            "limit": self.limit,
            "thread": self.thread,
            "from": self.from,
            "to": self.to,
        })
    }
}

/// The view a stream subscribes to, if any. The view parameters are refused
/// without `view`, and `view=messages` needs a project or `all` (messages
/// carry agent text).
fn view_of(q: &EventsQuery) -> Result<Option<View>, Failure> {
    let Some(name) = q.view.as_deref() else {
        if q.limit.is_some()
            || q.thread.is_some()
            || q.from.is_some()
            || q.to.is_some()
            || q.cursor.is_some()
        {
            return Err(Failure::invalid(
                "limit, thread, from, to and cursor need view",
            ));
        }
        return Ok(None);
    };
    if !routes::VIEWS.contains(&name) {
        return Err(Failure::invalid(format!("unknown view {name}")));
    }
    if q.project_root.is_none() && !q.all {
        return Err(Failure::new(
            Code::NoProject,
            "view=messages needs project_root or all=true",
        ));
    }
    let limit = match q.limit.as_deref() {
        None => VIEW_LIMIT,
        Some(raw) => raw
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=VIEW_MAX_LIMIT).contains(n))
            .ok_or_else(|| Failure::invalid(format!("limit is 1 to {VIEW_MAX_LIMIT}")))?,
    };
    let cursor = q.cursor.as_deref().map(parse_cursor).transpose()?;
    let named = |v: &Option<String>| v.clone().filter(|s| !s.trim().is_empty());
    Ok(Some(View {
        limit,
        thread: named(&q.thread),
        from: named(&q.from),
        to: named(&q.to),
        cursor,
    }))
}

fn parse_cursor(raw: &str) -> Result<lean_ctx_time::UtcDateTime, Failure> {
    raw.strip_prefix(CURSOR_PREFIX)
        .and_then(|micros| micros.parse::<i64>().ok())
        .and_then(lean_ctx_time::UtcDateTime::from_timestamp_micros)
        .ok_or_else(|| Failure::invalid("cursor is one this service sent"))
}

fn row_time(row: &local_read::bus::MessageRow) -> Option<lean_ctx_time::UtcDateTime> {
    lean_ctx_time::OffsetDateTime::parse_from_rfc3339(&row.timestamp)
        .ok()
        .map(lean_ctx_time::OffsetDateTime::to_utc)
}

/// The resume point just after `row`.
fn cursor_of(row: &local_read::bus::MessageRow) -> Option<String> {
    row_time(row).map(|at| format!("{CURSOR_PREFIX}{}", at.timestamp_micros()))
}

/// The `snapshot` of a view over `rows` (its selection: oldest first, the
/// newest `limit + 1`): the rows newer than the reader's cursor when they
/// fit the bound (`delta`), else the newest `limit` (`full`, which replaces
/// what the reader holds).
fn snapshot(view: &View, rows: &[local_read::bus::MessageRow], resume: bool) -> Value {
    let since = view
        .cursor
        .filter(|_| resume)
        .map(|at| at - lean_ctx_time::Duration::milliseconds(CURSOR_GRACE_MS));
    let newer: Vec<&local_read::bus::MessageRow> = match since {
        Some(since) => rows
            .iter()
            .filter(|r| row_time(r).is_none_or(|at| at > since))
            .collect(),
        None => Vec::new(),
    };
    let (mode, kept, truncated): (&str, Vec<&local_read::bus::MessageRow>, bool) =
        if since.is_some() && newer.len() <= view.limit {
            ("delta", newer, false)
        } else {
            let skip = rows.len().saturating_sub(view.limit);
            ("full", rows.iter().skip(skip).collect(), skip > 0)
        };
    let cursor = kept.last().and_then(|r| cursor_of(r)).or_else(|| {
        view.cursor
            .filter(|_| mode == "delta")
            .map(|at| format!("{CURSOR_PREFIX}{}", at.timestamp_micros()))
    });
    json!({
        "view": "messages",
        "mode": mode,
        "rows": kept,
        "cursor": cursor,
        "truncated": truncated,
    })
}

/// One event ready to send: its type and its `data` line, a finished Local
/// Read document. A released one is exactly the bytes its receipt binds
/// (with the line's final `\n`).
pub(crate) type Emitted = (&'static str, String);

fn plain(kind: &'static str, data: &Value) -> Emitted {
    (kind, local_read::document(&format!("event.{kind}"), data))
}

/// Releases the stream's slot when the client goes away.
struct Slot;

impl Drop for Slot {
    fn drop(&mut self) {
        STREAMS.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A file's change marker (modified time, length).
type Marker = Option<(std::time::SystemTime, u64)>;

fn marker(path: Option<&std::path::Path>) -> Marker {
    let meta = std::fs::metadata(path?).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// What one stream has already told its client.
pub(crate) struct Tail {
    project: Option<String>,
    all: bool,
    types: Vec<&'static str>,
    started: bool,
    seen_events: HashSet<u64>,
    registry_path: Option<std::path::PathBuf>,
    registry: Marker,
    seen_messages: HashSet<String>,
    active_agents: Option<usize>,
    presence_path: Option<std::path::PathBuf>,
    presence: Marker,
    view: Option<View>,
}

impl Tail {
    /// The stream subscribes to `view`: its first poll yields the view's
    /// `snapshot`, and `message` events follow the view's filter and bound.
    pub(crate) fn with_view(mut self, view: Option<View>) -> Self {
        self.view = view;
        self
    }

    pub(crate) fn new(project: Option<String>, all: bool, types: Vec<&'static str>) -> Self {
        let registry_path = lean_ctx::core::data_dir::lean_ctx_data_dir()
            .ok()
            .map(|dir| dir.join("agents").join("registry.json"));
        let presence_path = lean_ctx_cloud::CloudStore::open()
            .ok()
            .map(|store| store.data_dir().join("bus").join("presence.json"));
        Self {
            project,
            all,
            types,
            started: false,
            seen_events: HashSet::new(),
            registry_path,
            registry: None,
            seen_messages: HashSet::new(),
            active_agents: None,
            presence_path,
            presence: None,
            view: None,
        }
    }

    fn wants(&self, kind: &str) -> bool {
        self.types.contains(&kind)
    }

    /// The `hello` event's data.
    pub(crate) fn hello(&self) -> Value {
        json!({
            "project_root": self.project,
            "all": self.all,
            "types": self.types,
            "view": self.view.as_ref().map(View::json),
            "service": {
                "version": env!("CARGO_PKG_VERSION"),
                "build": lean_ctx::core::integrity::build().to_json(),
            },
        })
    }

    /// The events since the last poll. The first poll only takes note of the
    /// current state: the stream is live, not a replay.
    pub(crate) fn poll(&mut self) -> Vec<Emitted> {
        let mut out = Vec::new();
        self.poll_journal(&mut out);
        self.poll_registry(&mut out);
        let presence = marker(self.presence_path.as_deref());
        if self.started && presence != self.presence && self.wants("presence_changed") {
            out.push(plain(
                "presence_changed",
                &json!({ "route": "/v1/local/presence" }),
            ));
        }
        self.presence = presence;
        self.started = true;
        out
    }

    /// The project a released event is released under (none for `all`).
    fn release_root(&self) -> String {
        if self.all {
            String::new()
        } else {
            self.project.clone().unwrap_or_default()
        }
    }

    /// `data` released through the CLI tool run of `tool` for the socket,
    /// as a route answer is (redaction, gateway, ONE transport receipt for
    /// the exact document sent). A withheld event is sent as its type with
    /// the `withheld` error document as data (that is what was receipted);
    /// without a stored receipt nothing is sent. An event in which a value
    /// was masked is followed by a `release_notice` when the stream asked
    /// for that type.
    pub(crate) fn push_released(
        &self,
        out: &mut Vec<Emitted>,
        tool: &str,
        kind: &'static str,
        data: Value,
    ) {
        let answer = local_read::answer_released(
            tool,
            &self.release_root(),
            &format!("event.{kind}"),
            local_read::Via::Socket,
            || Ok(data),
        );
        let Some(text) = answer.body() else {
            return;
        };
        out.push((kind, text.to_owned()));
        if let Some(notice) = answer.notice
            && self.wants("release_notice")
        {
            out.push(plain(
                "release_notice",
                &json!({ "event": kind, "notice": notice }),
            ));
        }
    }

    fn poll_journal(&mut self, out: &mut Vec<Emitted>) {
        if !(self.wants("tool_call") || self.wants("activity") || self.wants("knowledge_changed")) {
            return;
        }
        let project = self.project.as_deref().map(std::path::Path::new);
        let events = lean_ctx::core::events::load_served_events(EVENT_WINDOW, project);
        let window: HashSet<u64> = events.iter().map(|e| e.id).collect();
        for event in events {
            if !self.started || self.seen_events.contains(&event.id) {
                continue;
            }
            let Some((kind, mut data)) = journal_event(&event) else {
                continue;
            };
            if !self.wants(kind) {
                continue;
            }
            if carries_free_text(kind, &data) {
                // A policy reason, a warning's detail, an agent's action, a
                // fact key: through the release point like any project text.
                if kind == "knowledge_changed" {
                    data["project_root"] = json!(self.project);
                }
                self.push_released(out, "ctx_session", kind, data);
            } else {
                // Tool names, modes, paths the gate admitted, numbers.
                out.push(plain(kind, &data));
            }
        }
        // The window is the memory: ids that left it are not served again.
        self.seen_events = window;
    }

    fn poll_registry(&mut self, out: &mut Vec<Emitted>) {
        let now = marker(self.registry_path.as_deref());
        if self.started && now == self.registry {
            return;
        }
        self.registry = now;
        let registry = lean_ctx::core::agents::AgentRegistry::load().unwrap_or_default();
        let active = registry.list_active(self.project.as_deref()).len();
        if self.started && self.active_agents != Some(active) && self.wants("agents_changed") {
            out.push(plain("agents_changed", &json!({ "active": active })));
        }
        self.active_agents = Some(active);
        // Messages carry agent text: only for a named project or the
        // person's console (`all`), released as the `agent messages` read is.
        if self.project.is_none() && !self.all {
            return;
        }
        let view = self.view.clone();
        // A view selects one row past its bound, to tell whether it holds
        // everything newer than a cursor.
        let selection = local_read::bus::Selection {
            project_root: if self.all { None } else { self.project.clone() },
            all: self.all,
            since: None,
            to: view.as_ref().and_then(|v| v.to.clone()),
            from: view.as_ref().and_then(|v| v.from.clone()),
            thread: view.as_ref().and_then(|v| v.thread.clone()),
            limit: view.as_ref().map_or(200, |v| v.limit + 1),
        };
        let rows = local_read::bus::select(
            &registry.scratchpad,
            &selection,
            lean_ctx_time::UtcDateTime::now(),
        );
        let current: HashSet<String> = rows.iter().map(|r| r.id.clone()).collect();
        if let Some(view) = &view {
            self.poll_view(out, view, &rows);
        } else if self.started && self.wants("message") {
            for row in rows {
                if self.seen_messages.contains(&row.id) {
                    continue;
                }
                if let Ok(data) = serde_json::to_value(&row) {
                    self.push_released(out, "ctx_agent", "message", data);
                }
            }
        }
        self.seen_messages = current;
    }

    /// A view's events over its selection `rows`: on the first poll the
    /// `snapshot` (resumed from the reader's cursor when it can be), after
    /// that each new row as a `message` event with its `cursor`, or a fresh
    /// full `snapshot` when more rows arrived than the view holds.
    fn poll_view(&self, out: &mut Vec<Emitted>, view: &View, rows: &[local_read::bus::MessageRow]) {
        if !self.started {
            self.push_released(out, "ctx_agent", "snapshot", snapshot(view, rows, true));
            return;
        }
        if !self.wants("message") {
            return;
        }
        let new: Vec<&local_read::bus::MessageRow> = rows
            .iter()
            .filter(|r| !self.seen_messages.contains(&r.id))
            .collect();
        if new.len() > view.limit {
            self.push_released(out, "ctx_agent", "snapshot", snapshot(view, rows, false));
            return;
        }
        for row in new {
            let Ok(mut data) = serde_json::to_value(row) else {
                continue;
            };
            data["cursor"] = json!(cursor_of(row));
            self.push_released(out, "ctx_agent", "message", data);
        }
    }
}

/// Whether a journal event carries free text and so passes the release
/// point. One release costs about 2 ms under protection (3.5 ms with 16
/// streams contending for the receipt store, measured 2026-11-05), once per
/// stream: the per-tool-call kinds without free text (`tool_call`,
/// `cache_hit`, `compression`, threshold changes) are served through the
/// observation gate only; the rare ones with free text are released.
fn carries_free_text(kind: &str, data: &Value) -> bool {
    match kind {
        "tool_call" => false,
        "activity" => !matches!(
            data["activity"].as_str(),
            Some("cache_hit" | "compression" | "threshold_shift" | "threshold_adapted")
        ),
        _ => true,
    }
}

/// The stream event of one journal event, by its kind.
fn journal_event(event: &lean_ctx::core::events::LeanCtxEvent) -> Option<(&'static str, Value)> {
    use lean_ctx::core::events::EventKind;
    let at = event.timestamp.clone();
    Some(match &event.kind {
        EventKind::ToolCall {
            tool,
            tokens_original,
            tokens_saved,
            mode,
            duration_ms,
            path,
        } => (
            "tool_call",
            json!({
                "at": at, "event_id": event.id, "tool": tool, "mode": mode, "path": path,
                "duration_ms": duration_ms, "tokens_original": tokens_original,
                "tokens_saved": tokens_saved,
            }),
        ),
        EventKind::KnowledgeUpdate {
            category,
            key,
            action,
        } => (
            "knowledge_changed",
            json!({
                "at": at, "event_id": event.id, "route": "/v1/local/knowledge",
                "category": category, "key": key, "action": action,
            }),
        ),
        other => {
            let Value::Object(mut detail) = serde_json::to_value(other).ok()? else {
                return None;
            };
            let name = detail
                .remove("type")
                .and_then(|t| t.as_str().map(snake_case))?;
            (
                "activity",
                json!({ "at": at, "event_id": event.id, "activity": name, "detail": detail }),
            )
        }
    })
}

fn snake_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// The requested event types, sorted; all when none are named.
pub(crate) fn event_types(raw: Option<&str>) -> Result<Vec<&'static str>, Failure> {
    let Some(raw) = raw.filter(|r| !r.trim().is_empty()) else {
        return Ok(routes::EVENT_TYPES.to_vec());
    };
    let mut out = Vec::new();
    for name in raw.split(',').map(str::trim).filter(|n| !n.is_empty()) {
        let known = routes::EVENT_TYPES
            .iter()
            .find(|t| **t == name)
            .ok_or_else(|| Failure::invalid(format!("unknown event type {name}")))?;
        if !out.contains(known) {
            out.push(*known);
        }
    }
    out.sort_unstable();
    Ok(out)
}

fn sse_event(seq: u64, kind: &str, data: &str) -> SseEvent {
    SseEvent::default()
        .id(seq.to_string())
        .event(kind)
        .data(data)
}

async fn events(Query(pairs): Query<Vec<(String, String)>>) -> Response {
    let q = match events_query(pairs) {
        Ok(q) => q,
        Err(failure) => return failure_response(&failure),
    };
    let types = match event_types(q.types.as_deref()) {
        Ok(types) => types,
        Err(failure) => return failure_response(&failure),
    };
    let project = match q.project_root.as_deref() {
        None => None,
        Some(raw) => match routes::project_dir(raw) {
            Ok(root) => Some(root),
            Err(failure) => return failure_response(&failure),
        },
    };
    let view = match view_of(&q) {
        Ok(view) => view,
        Err(failure) => return failure_response(&failure),
    };
    let all = q.all;
    if STREAMS.fetch_add(1, Ordering::SeqCst) >= MAX_STREAMS {
        STREAMS.fetch_sub(1, Ordering::SeqCst);
        return failure_response(&Failure::new(
            Code::Unavailable,
            format!("at most {MAX_STREAMS} event streams"),
        ));
    }
    let slot = Slot;
    let Ok((tail, first)) = tokio::task::spawn_blocking(move || {
        let mut tail = Tail::new(project, all, types).with_view(view);
        // The first poll only takes note of the state; with a view it
        // yields the view's snapshot.
        let first = tail.poll();
        (tail, first)
    })
    .await
    else {
        return failure_response(&Failure::new(Code::Internal, "stream start failed"));
    };
    let hello = plain("hello", &tail.hello());
    let pending: std::collections::VecDeque<Emitted> =
        std::iter::once(hello).chain(first).collect();
    let stream = futures::stream::unfold(
        (Some(tail), pending, 0_u64, slot),
        |(mut tail, mut pending, mut seq, slot)| async move {
            loop {
                if let Some((kind, data)) = pending.pop_front() {
                    seq += 1;
                    let event = sse_event(seq, kind, &data);
                    return Some((
                        Ok::<_, std::convert::Infallible>(event),
                        (tail, pending, seq, slot),
                    ));
                }
                tokio::time::sleep(POLL).await;
                let mut current = tail.take()?;
                let (back, found) = tokio::task::spawn_blocking(move || {
                    let found = current.poll();
                    (current, found)
                })
                .await
                .ok()?;
                tail = Some(back);
                pending.extend(found);
            }
        },
    );
    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response()
}

#[cfg(test)]
mod app_only_tests {
    use super::{HeaderMap, Route, StatusCode, app_admitted, failure_response, local_read};

    fn send_route() -> &'static Route {
        crate::cli::local_read::routes::all()
            .find(|r| r.kind == local_read::decisions_part::SEND)
            .expect("routed")
    }

    #[test]
    fn send_to_agents_without_the_app_key_is_403_app_only() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let route = send_route();
        let mut headers = HeaderMap::new();
        assert!(!app_admitted(route, &headers), "no header, no key file");
        headers.insert(
            local_read::app_key::HEADER,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                .parse()
                .unwrap(),
        );
        assert!(!app_admitted(route, &headers), "a header but no key file");
        let response = failure_response(&local_read::app_key::refusal());
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let document: serde_json::Value =
            serde_json::from_str(&local_read::error_document(&local_read::app_key::refusal()))
                .unwrap();
        assert_eq!(document["error"]["code"], "app_only");
        // Every other route is answered as before.
        let create = crate::cli::local_read::routes::all()
            .find(|r| r.kind == local_read::decisions_part::CREATE)
            .unwrap();
        assert!(app_admitted(create, &HeaderMap::new()));
    }

    #[test]
    fn send_to_agents_with_the_held_app_key_is_admitted() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        // A test key: 64 lowercase hex characters.
        let key = "ab".repeat(32);
        let file = local_read::app_key::path().expect("data dir");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, &key).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let mut headers = HeaderMap::new();
        headers.insert(local_read::app_key::HEADER, key.parse().unwrap());
        assert!(app_admitted(send_route(), &headers));
        headers.insert(
            local_read::app_key::HEADER,
            key.replace('a', "c").parse().unwrap(),
        );
        assert!(!app_admitted(send_route(), &headers), "another key");
    }
}

#[cfg(test)]
mod error_status_tests {
    use super::{Code, StatusCode, status_of};

    /// Every error code; the match fails to compile when a code is added
    /// without being listed here.
    const ALL: [Code; 27] = [
        Code::InvalidArgument,
        Code::NoProject,
        Code::NotConnected,
        Code::NoVault,
        Code::NotFound,
        Code::Forbidden,
        Code::Withheld,
        Code::AnchorGone,
        Code::Conflict,
        Code::Unavailable,
        Code::RateLimited,
        Code::PlanRequired,
        Code::InvalidState,
        Code::InvalidField,
        Code::SsoRequired,
        Code::NotOffered,
        Code::EmailUnverified,
        Code::PolicyDenied,
        Code::Internal,
        Code::NoComputer,
        Code::BalanceEmpty,
        Code::ComputerExists,
        Code::ComputerBusy,
        Code::ComputerProviderUnavailable,
        Code::ComputerNotAvailable,
        Code::ConfirmationRequired,
        Code::AppOnly,
    ];

    const fn listed(code: Code) {
        match code {
            Code::InvalidArgument
            | Code::NoProject
            | Code::NotConnected
            | Code::NoVault
            | Code::NotFound
            | Code::Forbidden
            | Code::Withheld
            | Code::AnchorGone
            | Code::Conflict
            | Code::Unavailable
            | Code::RateLimited
            | Code::PlanRequired
            | Code::InvalidState
            | Code::InvalidField
            | Code::SsoRequired
            | Code::NotOffered
            | Code::EmailUnverified
            | Code::PolicyDenied
            | Code::Internal
            | Code::NoComputer
            | Code::BalanceEmpty
            | Code::ComputerExists
            | Code::ComputerBusy
            | Code::ComputerProviderUnavailable
            | Code::ComputerNotAvailable
            | Code::ConfirmationRequired
            | Code::AppOnly => {}
        }
    }

    fn contract(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../docs/contracts")
            .join(name);
        // A Windows checkout may hold the document with CRLF line endings.
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
            .replace("\r\n", "\n")
    }

    #[test]
    fn every_error_code_is_in_the_closed_list_and_the_status_table() {
        let read = contract("local-read-v1.md");
        let closed = read
            .lines()
            .find(|l| l.trim_start().starts_with("`code` ∈ "))
            .expect("local-read-v1 §1 closed code list");
        let service = contract("local-service-v1.md");
        let start = service
            .find("- **Errors:**")
            .expect("local-service-v1 Errors");
        let rest = &service[start + 2..];
        let table = &rest[..rest.find("\n- **").unwrap_or(rest.len())];
        for code in ALL {
            listed(code);
            let word = format!("`{}`", code.as_str());
            assert!(closed.contains(&word), "local-read-v1 §1 lacks {word}");
            let status: StatusCode = status_of(Some(code));
            let arrow = format!("→ {}", status.as_u16());
            assert!(
                table
                    .split(';')
                    .any(|clause| clause.contains(&word) && clause.contains(&arrow)),
                "local-service-v1 Errors lacks {word} {arrow}"
            );
        }
    }

    /// Not offered reaches the socket as its own refusal: `501` with the
    /// `not_offered` document, never the `503` of an outage.
    #[test]
    fn not_offered_is_a_501_refusal_on_the_socket() {
        use crate::cli::local_read::{Answer, Failure};
        let failure = Failure::new(Code::NotOffered, "not on this cloud");
        let response = super::answer_response(&Answer::failure(&failure));
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        let bytes =
            futures::executor::block_on(axum::body::to_bytes(response.into_body(), 1 << 16))
                .expect("body");
        let doc: serde_json::Value = serde_json::from_slice(&bytes).expect("a document");
        assert_eq!(doc["error"]["code"], "not_offered");
    }
}
