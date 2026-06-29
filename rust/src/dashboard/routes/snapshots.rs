//! `/api/snapshots` + `/api/snapshot` — the Context Time Machine **Replay**
//! surface (#1025).
//!
//! Read-only, project-scoped views over the append-only snapshot timeline:
//! - `GET /api/snapshots` lists the timeline (head + compact entries),
//! - `GET /api/snapshot?id=<prefix>` returns one full snapshot plus a
//!   server-computed `verify` verdict (signature + integrity).
//!
//! Both resolve the project the same way every other dashboard route does
//! ([`detect_project_root_for_dashboard`]) and never mutate state.

use serde_json::json;

use super::helpers::{detect_project_root_for_dashboard, extract_query_param};
use crate::core::context_snapshot;

pub(super) fn handle(
    path: &str,
    query_str: &str,
    _method: &str,
    _body: &str,
) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/snapshots" => Some(list()),
        "/api/snapshot" => Some(detail(query_str)),
        _ => None,
    }
}

/// Timeline for the active project: newest-last entries plus the current head.
fn list() -> (&'static str, &'static str, String) {
    let root = detect_project_root_for_dashboard();
    let hash = crate::core::project_hash::hash_project_root(&root);
    let entries = context_snapshot::load_entries(&root);
    let head = entries.last().map(|e| e.snapshot_id.clone());

    let payload = json!({
        "project_root": root,
        "project_hash": hash,
        "head": head,
        "count": entries.len(),
        "entries": entries,
    });
    json_response(&payload)
}

/// One full snapshot, resolved git-style by id prefix, with a verify verdict.
fn detail(query_str: &str) -> (&'static str, &'static str, String) {
    let root = detect_project_root_for_dashboard();
    let Some(prefix) = extract_query_param(query_str, "id").filter(|s| !s.trim().is_empty()) else {
        return error("400 Bad Request", "missing id query parameter");
    };

    let id = match context_snapshot::resolve_id(&root, prefix.trim()) {
        Ok(full) => full,
        Err(e) => return error("404 Not Found", &e),
    };
    let snap = match context_snapshot::read_snapshot(&root, &id) {
        Ok(s) => s,
        Err(e) => return error("404 Not Found", &e),
    };

    let payload = json!({
        "project_root": root,
        "verify": verify_label(&snap),
        "snapshot": snap,
    });
    json_response(&payload)
}

/// Stable wire label for the snapshot's trust state, mirroring the CLI verdicts.
fn verify_label(snap: &context_snapshot::ContextSnapshotV1) -> &'static str {
    if snap.signature.is_none() {
        return "unsigned";
    }
    match context_snapshot::verify_snapshot(snap) {
        Ok(true) => "verified",
        Ok(false) => "failed",
        Err(_) => "error",
    }
}

fn json_response(payload: &serde_json::Value) -> (&'static str, &'static str, String) {
    let body = serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string());
    ("200 OK", "application/json", body)
}

fn error(status: &'static str, msg: &str) -> (&'static str, &'static str, String) {
    (
        status,
        "application/json",
        json!({ "error": msg }).to_string(),
    )
}
