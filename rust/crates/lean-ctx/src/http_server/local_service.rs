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
    http::{StatusCode, header},
    response::sse::{Event as SseEvent, KeepAlive, Sse},
    response::{IntoResponse, Response},
    routing::{MethodRouter, get, post},
};
use serde_json::{Value, json};
use tokio::time::Duration;

use super::AppState;
use crate::cli::local_read::{
    self, Code, Failure,
    routes::{self, Method, Route},
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
        Some(Code::InvalidArgument | Code::NoProject) => StatusCode::BAD_REQUEST,
        Some(Code::NotFound | Code::NoVault) => StatusCode::NOT_FOUND,
        Some(Code::Forbidden | Code::Withheld) => StatusCode::FORBIDDEN,
        // `knowledge verify` refused: the anchored code is gone, so the
        // fact has to be rewritten or removed, not confirmed; a local
        // task's state does not allow the write.
        Some(Code::AnchorGone | Code::Conflict) => StatusCode::CONFLICT,
        Some(Code::NotConnected | Code::Unavailable) => StatusCode::SERVICE_UNAVAILABLE,
        // Hive rate-limits this instance and LeanCTX keeps no answer to serve.
        Some(Code::RateLimited) => StatusCode::TOO_MANY_REQUESTS,
        Some(Code::Internal) => StatusCode::INTERNAL_SERVER_ERROR,
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
            move |State(state): State<AppState>, body: Bytes| async move {
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
            other => return Err(Failure::invalid(format!("unknown parameter {other}"))),
        }
    }
    Ok(q)
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
}

impl Tail {
    pub(crate) fn new(project: Option<String>, all: bool, types: Vec<&'static str>) -> Self {
        let registry_path = lean_ctx::core::data_dir::lean_ctx_data_dir()
            .ok()
            .map(|dir| dir.join("agents").join("registry.json"));
        let presence_path = lean_ctx_hive::HiveStore::open()
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
        let selection = local_read::bus::Selection {
            project_root: if self.all { None } else { self.project.clone() },
            all: self.all,
            since: None,
            to: None,
            from: None,
            thread: None,
            limit: 200,
        };
        let rows = local_read::bus::select(&registry.scratchpad, &selection, chrono::Utc::now());
        let current: HashSet<String> = rows.iter().map(|r| r.id.clone()).collect();
        if self.started && self.wants("message") {
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
    let all = q.all;
    if STREAMS.fetch_add(1, Ordering::SeqCst) >= MAX_STREAMS {
        STREAMS.fetch_sub(1, Ordering::SeqCst);
        return failure_response(&Failure::new(
            Code::Unavailable,
            format!("at most {MAX_STREAMS} event streams"),
        ));
    }
    let slot = Slot;
    let Ok(tail) = tokio::task::spawn_blocking(move || {
        let mut tail = Tail::new(project, all, types);
        let _ = tail.poll();
        tail
    })
    .await
    else {
        return failure_response(&Failure::new(Code::Internal, "stream start failed"));
    };
    let hello = plain("hello", &tail.hello());
    let pending: std::collections::VecDeque<Emitted> = std::iter::once(hello).collect();
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
