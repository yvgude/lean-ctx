// SPDX-License-Identifier: Apache-2.0
//! Marking a contradiction and retracting a claim on a C0 2.0 place (v5
//! BAUPLAN §5.1: conflicting and rejected knowledge is never served as
//! truth).
//!
//! `lean-ctx knowledge contradict <node> <other-node>` queues one signed
//! `edge.add` of kind `contradicts` (C0 `events/edge.add.json`, `edge_body`:
//! kind, from, to, body_nonce; no reason member). `lean-ctx knowledge retract
//! <node> --reason <text>` queues one signed `retract` (C0
//! `events/retract.json`, `reason_body`: reason 2–500 chars, body_nonce).
//! Both are class-W events of this instance's workspace worker, signed with
//! the shared vault signer, provenance claim `workspace` (the server only
//! keeps `own` for an engine's own nodes), and queued in the replica like
//! captures: offline is fine, the next push sends them.
//!
//! The checks mirror the server's fold (hive-server `place/push.rs`): both
//! ends of an edge and a retraction's target must be live nodes; anyone
//! with the vault write right may retract (no author rule), and the server
//! holds a large retraction for review (`held`). The retraction reason is
//! screened before it is queued; a blocked text is never sent.

use serde_json::{Value, json};

use super::maintain::{Target, queue, reasons_hash};
use crate::core::hive::HiveError;
use crate::core::hive::capture::GATE_VERSION;
use crate::core::hive::events::build::{EVENT_SCHEMA, Place, Signer};
use crate::core::hive::events::hlc::{self, Stamp};
use crate::core::hive::events::{ids, sign};
use crate::core::hive::knowledge::screening::Screen;
use crate::core::hive::replica::fold::Graph;

/// C0 `reason_body.reason` length bounds.
const REASON_MIN: usize = 2;
const REASON_MAX: usize = 500;

fn local(e: impl std::fmt::Display) -> HiveError {
    HiveError::Local(e.to_string())
}

/// A signed class-W event of `op` on `target` with `body` (provenance claim
/// `workspace`, as every curation by a person through the engine).
fn event(
    place: &Place,
    op: &str,
    target: &str,
    body: &Value,
    task: &str,
    stamp: Stamp,
    signer: &Signer<'_>,
) -> Result<Value, HiveError> {
    let body_commit = sign::body_commit(&place.pk_id, body).map_err(local)?;
    let mut event = json!({
        "schema": EVENT_SCHEMA,
        "event_id": ids::new_event_id(stamp.wall_ms)?,
        "place": {"kind": place.kind, "id": place.id},
        "op": op,
        "target": target,
        "body": body,
        "body_commit": body_commit,
        "hlc": {"wall_ms": stamp.wall_ms, "counter": stamp.counter, "node": signer.hlc_node},
        "causal_base": place.causal_base,
        "actor": {"kind": "worker"},
        "worker_id": signer.worker_id,
        "provenance_claim": "workspace",
        "capture": {
            "gate_version": GATE_VERSION,
            "evidence": "observed",
            "reasons_passed_hash": reasons_hash(task),
        },
        "screening": {
            "counts": {"forbidden": 0, "local_path": 0, "pii": 0, "redaction": 0, "secret": 0},
            "scanner_version": crate::core::hive::knowledge::screening::SCANNER_VERSION,
        },
    });
    sign::sign(&mut event, signer.key).map_err(local)?;
    Ok(event)
}

/// `edge.add contradicts` from `from` to `to`.
pub fn contradicts_edge(
    place: &Place,
    from: &str,
    to: &str,
    stamp: Stamp,
    signer: &Signer<'_>,
) -> Result<Value, HiveError> {
    let body = json!({
        "body_nonce": sign::new_body_nonce()?,
        "from": from,
        "kind": "contradicts",
        "to": to,
    });
    let edge_id = ids::new_random_id("e")?;
    event(
        place,
        "edge.add",
        &edge_id,
        &body,
        "contradicts",
        stamp,
        signer,
    )
}

/// `retract` of `target` with `reason`.
pub fn retract_event(
    place: &Place,
    target: &str,
    reason: &str,
    stamp: Stamp,
    signer: &Signer<'_>,
) -> Result<Value, HiveError> {
    let body = json!({"body_nonce": sign::new_body_nonce()?, "reason": reason});
    event(place, "retract", target, &body, "retract", stamp, signer)
}

fn fold(target: &Target<'_>) -> Result<Graph, HiveError> {
    let mut graph = Graph::default();
    graph
        .fold(&target.replica.fold_events()?)
        .map_err(|e| HiveError::Protocol(e.to_string()))?;
    Ok(graph)
}

fn live(graph: &Graph, node_id: &str) -> Result<(), HiveError> {
    match graph.nodes.get(node_id) {
        Some(n) if !n.retracted && !n.purged => Ok(()),
        Some(_) => Err(HiveError::Config(format!(
            "{node_id} is already retracted or purged in this vault"
        ))),
        None => Err(HiveError::Config(format!(
            "{node_id}: no such node in this vault's replica (run `lean-ctx hive sync` first)"
        ))),
    }
}

/// The events already queued and not yet folded, so a repeated command
/// queues nothing twice. A sealed place's queued bodies are ciphertext, so
/// there only the folded state dedupes.
fn pending(target: &Target<'_>) -> Result<Vec<Value>, HiveError> {
    Ok(target
        .replica
        .pending()?
        .iter()
        .filter_map(|(_, bytes)| serde_json::from_slice::<Value>(bytes).ok())
        .collect())
}

fn same_pair(body: &Value, a: &str, b: &str) -> bool {
    let (from, to) = (body["from"].as_str(), body["to"].as_str());
    (from == Some(a) && to == Some(b)) || (from == Some(b) && to == Some(a))
}

/// Queue `a contradicts b`. Returns `false` when the two are already marked
/// (folded or queued) and nothing was queued.
pub fn queue_contradiction(
    target: &Target<'_>,
    a: &str,
    b: &str,
    now_ms: u64,
) -> Result<bool, HiveError> {
    if a == b {
        return Err(HiveError::Config(
            "a node does not contradict itself".into(),
        ));
    }
    let graph = fold(target)?;
    live(&graph, a)?;
    live(&graph, b)?;
    let folded = graph.edges.values().any(|e| {
        e.kind == "contradicts"
            && !e.retracted
            && ((e.from == a && e.to == b) || (e.from == b && e.to == a))
    });
    let queued = pending(target)?.iter().any(|e| {
        e["op"] == "edge.add" && e["body"]["kind"] == "contradicts" && same_pair(&e["body"], a, b)
    });
    if folded || queued {
        return Ok(false);
    }
    let stamp = hlc::send(Stamp::default(), now_ms);
    let event = contradicts_edge(target.place, a, b, stamp, target.signer)?;
    queue(target, event, stamp)?;
    Ok(true)
}

/// Queue the retraction of `node` with a screened `reason`. Returns `false`
/// when a retraction of it is already queued.
pub fn queue_retraction(
    target: &Target<'_>,
    node: &str,
    reason: &str,
    screen: &mut Screen,
    now_ms: u64,
) -> Result<bool, HiveError> {
    let reason = reason.trim();
    let chars = reason.chars().count();
    if !(REASON_MIN..=REASON_MAX).contains(&chars) {
        return Err(HiveError::Config(format!(
            "--reason needs {REASON_MIN} to {REASON_MAX} characters"
        )));
    }
    screen.check(reason).map_err(|why| {
        HiveError::Refused(format!("screened: the reason was not queued ({why})"))
    })?;
    let graph = fold(target)?;
    live(&graph, node)?;
    if pending(target)?
        .iter()
        .any(|e| e["op"] == "retract" && e["target"] == node)
    {
        return Ok(false);
    }
    let stamp = hlc::send(Stamp::default(), now_ms);
    let event = retract_event(target.place, node, reason, stamp, target.signer)?;
    queue(target, event, stamp)?;
    Ok(true)
}

#[cfg(test)]
#[path = "curate_tests.rs"]
mod tests;
