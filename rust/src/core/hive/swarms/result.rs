// SPDX-License-Identifier: Apache-2.0
//! The signed events of an E26 result (CONTRACT §3.20.6, C0
//! `routes/swarm_result.v2.request.json`): one `node.add` of type `result`
//! on the claimed goal's node and one `edge.add result_of` from it to the
//! goal node, both in the swarm place and signed by the participant's swarm
//! alias key (R38; lead decision 25.10.: never the instance key).

use serde_json::{Value, json};

use crate::core::hive::HiveError;
use crate::core::hive::events::build::{EVENT_SCHEMA, Place, Signer};
use crate::core::hive::events::hlc::Stamp;
use crate::core::hive::events::{ids, sign};

/// What the participant reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultDraft {
    /// The claimed goal's swarm-log node (`n_…`).
    pub goal_node: String,
    pub summary: String,
    /// `{kind, ref, sha256}` entries (material the result names).
    pub artefacts: Vec<Value>,
    pub usage: Option<Usage>,
}

/// Self-reported usage (§3.14.2); `source` as CONTRACT defines it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Usage {
    pub source: String,
    pub tasks: u64,
    pub tokens: u64,
    pub usd_cents: u64,
}

/// The per-event identities; fresh in production, fixed in tests.
#[derive(Debug, Clone)]
pub struct Ids {
    pub result_event: String,
    pub edge_event: String,
    pub result_node: String,
    pub edge: String,
    pub nonces: [String; 2],
    pub stamps: [Stamp; 2],
}

impl Ids {
    /// Fresh ids, the two events one HLC step apart.
    pub fn fresh(first: Stamp, second: Stamp) -> Result<Ids, HiveError> {
        Ok(Ids {
            result_event: ids::new_event_id(first.wall_ms)?,
            edge_event: ids::new_event_id(second.wall_ms)?,
            result_node: ids::new_random_id("n")?,
            edge: ids::new_random_id("e")?,
            nonces: [sign::new_body_nonce()?, sign::new_body_nonce()?],
            stamps: [first, second],
        })
    }
}

fn local(e: impl std::fmt::Display) -> HiveError {
    HiveError::Local(e.to_string())
}

/// The capture block of a participant result: observed evidence that passed
/// the gate (`reasons_passed_hash` from the gate run).
pub struct Capture<'a> {
    pub gate_version: u32,
    pub reasons_passed_hash: &'a str,
}

/// The two signed events of one result.
pub fn events(
    draft: &ResultDraft,
    ids: &Ids,
    place: &Place,
    signer: &Signer<'_>,
    capture: &Capture<'_>,
    scanner_version: u32,
) -> Result<[Value; 2], HiveError> {
    let mut body = json!({
        "body_nonce": ids.nonces[0],
        "goal": draft.goal_node,
        "summary": draft.summary,
    });
    if !draft.artefacts.is_empty() {
        body["artefacts"] = Value::Array(draft.artefacts.clone());
    }
    if let Some(u) = &draft.usage {
        body["usage"] = json!({
            "source": u.source, "tasks": u.tasks, "tokens": u.tokens, "usd_cents": u.usd_cents,
        });
    }
    let common = |event_id: &str, stamp: Stamp, causal_base: u64| {
        json!({
            "schema": EVENT_SCHEMA,
            "event_id": event_id,
            "place": {"kind": place.kind, "id": place.id},
            "hlc": {"wall_ms": stamp.wall_ms, "counter": stamp.counter, "node": signer.hlc_node},
            "causal_base": causal_base,
            "actor": {"kind": "worker"},
            "worker_id": signer.worker_id,
            "provenance_claim": "workspace",
            "capture": {
                "gate_version": capture.gate_version,
                "evidence": "observed",
                "reasons_passed_hash": capture.reasons_passed_hash,
            },
            "screening": {
                "counts": {"forbidden": 0, "local_path": 0, "pii": 0, "redaction": 0, "secret": 0},
                "scanner_version": scanner_version,
            },
        })
    };
    let mut result = common(&ids.result_event, ids.stamps[0], place.causal_base);
    result["op"] = "node.add".into();
    result["node_type"] = "result".into();
    result["target"] = ids.result_node.clone().into();
    result["body_commit"] = sign::body_commit(&place.pk_id, &body)
        .map_err(local)?
        .into();
    result["body"] = body;
    sign::sign(&mut result, signer.key).map_err(local)?;

    let edge_body = json!({
        "body_nonce": ids.nonces[1],
        "from": ids.result_node,
        "kind": "result_of",
        "to": draft.goal_node,
    });
    // The edge follows the node in the same push.
    let mut edge = common(&ids.edge_event, ids.stamps[1], place.causal_base + 1);
    edge["op"] = "edge.add".into();
    edge["target"] = ids.edge.clone().into();
    edge["body_commit"] = sign::body_commit(&place.pk_id, &edge_body)
        .map_err(local)?
        .into();
    edge["body"] = edge_body;
    sign::sign(&mut edge, signer.key).map_err(local)?;
    Ok([result, edge])
}
