// SPDX-License-Identifier: Apache-2.0
use serde::Serialize;

use lean_ctx::core::provenance::{CheckpointRecord, ProvenanceRecord, ProvenanceTracker};

use super::helpers::detect_project_root_for_dashboard;

const RECENT_RECORD_LIMIT: usize = 50;

#[derive(Serialize)]
struct ProvenanceResponse {
    records: Vec<ProvenanceRecord>,
    checkpoints: Vec<CheckpointRecord>,
}

pub(super) fn handle(
    path: &str,
    _query: &str,
    method: &str,
    _body: &str,
) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/provenance" if method.eq_ignore_ascii_case("GET") => {
            let root = detect_project_root_for_dashboard();
            let response = ProvenanceTracker::new(root)
                .map(|tracker| ProvenanceResponse {
                    records: tracker
                        .store()
                        .query_recent_file_touches(RECENT_RECORD_LIMIT),
                    checkpoints: recent_checkpoints(&tracker),
                })
                .unwrap_or_else(|_| ProvenanceResponse {
                    records: Vec::new(),
                    checkpoints: Vec::new(),
                });
            let body = serde_json::to_string(&response)
                .unwrap_or_else(|_| r#"{"records":[],"checkpoints":[]}"#.to_string());
            Some(("200 OK", "application/json", body))
        }
        "/api/provenance" => Some((
            "405 Method Not Allowed",
            "application/json",
            r#"{"error":"method not allowed"}"#.to_string(),
        )),
        _ => None,
    }
}

fn recent_checkpoints(tracker: &ProvenanceTracker) -> Vec<CheckpointRecord> {
    let mut checkpoints = tracker.store().query_checkpoints(None);
    checkpoints.sort_by_key(|cp| std::cmp::Reverse(cp.observed_at));
    checkpoints.truncate(RECENT_RECORD_LIMIT);
    checkpoints
}

#[cfg(test)]
mod tests {
    use super::handle;

    #[test]
    fn provenance_route_rejects_non_get_requests() {
        let response = handle("/api/provenance", "", "POST", "").expect("route response");
        assert_eq!(response.0, "405 Method Not Allowed");
    }
}
