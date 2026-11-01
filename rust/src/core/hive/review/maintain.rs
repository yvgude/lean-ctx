// SPDX-License-Identifier: Apache-2.0
//! Maintenance client, host mode (SPEC-ENGINE §3.15b, V9.4).
//!
//! `lean-ctx hive maintain <ws>/<vault> [--stale] [--contradictions]
//! [--dedupe] [--limit N]` and `… --stale --confirm <node_id>`.
//!
//! `sync_vault` runs the same `--stale` re-check after every capture, so
//! anchors are re-checked automatically (v5 2.2.1); the command remains for
//! `--confirm` and on-demand runs.
//!
//! Host mode runs only the engine-local deterministic task `anchor_changed`:
//! every live claim with anchors is re-checked against the member's own
//! checkout (`HEAD`), and a claim whose anchored bytes changed gets
//! `field.set anchor_state = to_verify`, one whose file or symbol is gone
//! `gone` — signed with the instance key as the place's maintenance worker,
//! provenance `workspace`. No model, no cost, no server task: host mode
//! leases no E42 kind. The judgment kinds (`--contradictions`, `--dedupe`)
//! belong to the place's review loop and are only pointed to here.
//!
//! The task never re-anchors on its own; `--confirm <node_id>` is the
//! person confirming the claim at the current code: one `field.set anchors`
//! with the new digests, the `HEAD` commit and `verified_at` (C0 2.0
//! `events/field.set.anchors.json`, RESOLUTIONS R121).
//!
//! Sealed places derive the same tasks from the decrypted replica (§3.5.5).
//! Retractions carry no engine-side threshold: a retraction the place holds
//! for review comes back `held`, stays pending and is counted "sent to
//! review" (§3.15b, CONTRACT §3.16).

use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};

use crate::core::hive::HiveError;
use crate::core::hive::capture::GATE_VERSION;
use crate::core::hive::capture::refs::{Checkout, Reference};
use crate::core::hive::events::build::{EVENT_SCHEMA, Place, Signer};
use crate::core::hive::events::hlc::{self, Stamp};
use crate::core::hive::events::{ids, sign};
use crate::core::hive::replica::Replica;
use crate::core::hive::replica::fold::Graph;

/// Role of the worker that writes maintenance events.
pub const MAINTENANCE_ROLE: &str = "maintenance";
/// `--limit` default and bounds.
pub const DEFAULT_LIMIT: usize = 50;
pub const MAX_LIMIT: usize = 200;

/// What a run was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// `<ws>/<vault>` as given.
    pub vault: String,
    pub stale: bool,
    pub contradictions: bool,
    pub dedupe: bool,
    pub limit: usize,
    /// `--stale --confirm <node_id>`: re-anchor this claim.
    pub confirm: Option<String>,
}

fn usage(what: &str) -> HiveError {
    HiveError::Config(format!(
        "{what}; usage: lean-ctx hive maintain <ws>/<vault> [--stale] [--contradictions] [--dedupe] [--limit N] | <ws>/<vault> --stale --confirm <node_id>"
    ))
}

/// Parse the arguments after `hive maintain`.
pub fn parse(args: &[String]) -> Result<Request, HiveError> {
    let mut vault = None;
    let mut req = Request {
        vault: String::new(),
        stale: false,
        contradictions: false,
        dedupe: false,
        limit: DEFAULT_LIMIT,
        confirm: None,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--stale" => req.stale = true,
            "--contradictions" => req.contradictions = true,
            "--dedupe" => req.dedupe = true,
            "--limit" => {
                req.limit = it
                    .next()
                    .and_then(|n| n.parse().ok())
                    .filter(|n| (1..=MAX_LIMIT).contains(n))
                    .ok_or_else(|| usage("--limit takes 1..=200"))?;
            }
            "--confirm" => {
                req.confirm = Some(
                    it.next()
                        .filter(|n| n.starts_with("n_"))
                        .cloned()
                        .ok_or_else(|| usage("--confirm takes a node id (n_…)"))?,
                );
            }
            other if other.starts_with("--") => return Err(usage(&format!("unknown {other}"))),
            other if vault.is_none() && other.split_once('/').is_some() => {
                vault = Some(other.to_string());
            }
            other => return Err(usage(&format!("unexpected {other:?}"))),
        }
    }
    req.vault = vault.ok_or_else(|| usage("name the vault"))?;
    if req.confirm.is_some() && (!req.stale || req.contradictions || req.dedupe) {
        return Err(usage("--confirm goes with --stale only"));
    }
    if !(req.stale || req.contradictions || req.dedupe) {
        (req.stale, req.contradictions, req.dedupe) = (true, true, true);
    }
    Ok(req)
}

/// The line `--contradictions` / `--dedupe` print in host mode: judgment
/// tasks run in the place's review loop, never here (no E42 call).
pub fn handover_line(place: &str) -> String {
    format!("Judgment tasks run in the review loop: lean-ctx review attach {place}")
}

/// What a run did; printed as counts, never node texts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Counts {
    pub done: usize,
    pub to_verify: usize,
    pub gone: usize,
    pub merged: usize,
    pub retracted: usize,
    pub sent_to_review: usize,
    pub skipped: usize,
}

impl Counts {
    pub fn line(&self) -> String {
        format!(
            "done {} · to_verify {} · gone {} · merged {} · retracted {} · sent to review {} · skipped {}",
            self.done,
            self.to_verify,
            self.gone,
            self.merged,
            self.retracted,
            self.sent_to_review,
            self.skipped
        )
    }
}

/// Where a claim's anchors stand in the checkout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AnchorVerdict {
    Fresh,
    ToVerify,
    Gone,
}

impl AnchorVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::ToVerify => "to_verify",
            Self::Gone => "gone",
        }
    }
}

/// The bytes an anchor `ref` names at `HEAD`; `Err` for a ref that is not
/// a code anchor, `Ok(None)` when its file or symbol is gone.
fn anchored_bytes(checkout: &mut Checkout, anchor_ref: &str) -> Result<Option<Vec<u8>>, ()> {
    match crate::core::hive::capture::refs::parse(anchor_ref) {
        Some(Reference::File { path, .. }) => Ok(checkout.blob_bytes(&path).map(<[u8]>::to_vec)),
        Some(Reference::Symbol { path, symbol }) => Ok(checkout.symbol_span(&path, &symbol)),
        _ => Err(()),
    }
}

/// Recheck a claim's `anchors` (CodeAnchorV1 values) at the checkout: the
/// worst state of any code anchor, `None` without one.
///
/// An anchor is judged only when `HEAD` contains the commit it was taken
/// at (that commit or a descendant). An engine on an older commit, or on a
/// branch that diverged, cannot tell a change from code it has not seen
/// yet: its verdict is unknown, not outdated (dogfood 25.10.: two engines
/// on different commits kept marking each other's claims `to_verify`).
pub fn check_anchors(anchors: &[Value], checkout: &mut Checkout) -> Option<AnchorVerdict> {
    let mut worst = None;
    for anchor in anchors {
        let (Some(anchor_ref), Some(digest)) = (
            anchor.get("ref").and_then(Value::as_str),
            anchor.get("digest").and_then(Value::as_str),
        ) else {
            continue;
        };
        if let Some(commit) = anchor.get("commit").and_then(Value::as_str)
            && !checkout.head_contains(commit)
        {
            continue;
        }
        let Ok(bytes) = anchored_bytes(checkout, anchor_ref) else {
            continue;
        };
        let verdict = match bytes {
            None => AnchorVerdict::Gone,
            Some(b) if hex::encode(Sha256::digest(&b)) == digest => AnchorVerdict::Fresh,
            Some(_) => AnchorVerdict::ToVerify,
        };
        worst = worst.max(Some(verdict));
    }
    worst
}

/// One `anchor_changed` task: the claim and the state it gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorTask {
    pub node_id: String,
    pub verdict: AnchorVerdict,
}

/// The engine-local `anchor_changed` tasks of a folded place, in node id
/// order, at most `limit`. Claims whose anchors still match, or that
/// already carry the state, are counted as skipped (`no_change`).
pub fn anchor_tasks(
    graph: &Graph,
    checkout: &mut Checkout,
    limit: usize,
) -> (Vec<AnchorTask>, usize) {
    let mut tasks = Vec::new();
    let mut skipped = 0;
    for (id, node) in graph.live_nodes() {
        let Some(anchors) = node.fields.get("anchors").and_then(Value::as_array) else {
            continue;
        };
        let Some(verdict) = check_anchors(anchors, checkout) else {
            continue;
        };
        let current = node.fields.get("anchor_state").and_then(Value::as_str);
        // Matching bytes never clear a mark on their own: engines on older
        // commits would flip it back and forth with engines on newer ones.
        // Only `--confirm` (a person, at the current code) writes `fresh`.
        if verdict == AnchorVerdict::Fresh || current == Some(verdict.as_str()) {
            skipped += 1;
            continue;
        }
        if tasks.len() == limit {
            break;
        }
        tasks.push(AnchorTask {
            node_id: id.clone(),
            verdict,
        });
    }
    (tasks, skipped)
}

/// The claim's anchors re-taken at `HEAD` for `--confirm` (same refs, new
/// digests, the `HEAD` commit, `verified_at`). `None` when a ref is gone
/// (a gone claim is retracted, not re-anchored) or there is no `HEAD`.
pub fn reanchored(
    anchors: &[Value],
    checkout: &mut Checkout,
    verified_at: u64,
) -> Option<Vec<Value>> {
    let commit = checkout.head()?;
    anchors
        .iter()
        .map(|anchor| {
            let anchor_ref = anchor.get("ref").and_then(Value::as_str)?;
            let bytes = anchored_bytes(checkout, anchor_ref).ok()??;
            Some(json!({
                "ref": anchor_ref,
                "digest": hex::encode(Sha256::digest(&bytes)),
                "commit": commit,
                "verified_at": verified_at,
            }))
        })
        .collect()
}

/// `reasons_passed_hash` of a maintenance event: the task that wrote it.
pub(super) fn reasons_hash(task: &str) -> String {
    blake3::hash(format!("leanctx.hive.maintenance.v1:{task}").as_bytes())
        .to_hex()
        .to_string()
}

/// Every field the engine writes with `field.set` (all through
/// [`field_set`], on claims that carry anchors). Each must be mutable in C0
/// `enums/node-types.json` for every node type with anchors, or the server
/// refuses the event (`target_state`); a guard test checks both.
pub const FIELD_SET_FIELDS: [&str; 2] = ["anchors", "anchor_state"];

/// A signed class-W `field.set` by the maintenance worker (provenance
/// `workspace`). `verified_at` goes with `field = anchors` only.
pub fn field_set(
    place: &Place,
    target: &str,
    field: &str,
    value: Value,
    verified_at: Option<u64>,
    stamp: Stamp,
    signer: &Signer<'_>,
) -> Result<Value, HiveError> {
    fn local(e: impl std::fmt::Display) -> HiveError {
        HiveError::Local(e.to_string())
    }
    let mut body = Map::new();
    body.insert("body_nonce".into(), Value::String(sign::new_body_nonce()?));
    body.insert("field".into(), Value::from(field));
    body.insert("value".into(), value);
    if let Some(at) = verified_at {
        body.insert("verified_at".into(), Value::from(at));
    }
    let body = Value::Object(body);
    let body_commit = sign::body_commit(&place.pk_id, &body).map_err(local)?;
    let mut event = json!({
        "schema": EVENT_SCHEMA,
        "event_id": ids::new_event_id(stamp.wall_ms)?,
        "place": {"kind": place.kind, "id": place.id},
        "op": "field.set",
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
            "reasons_passed_hash": reasons_hash(field),
        },
        "screening": {
            "counts": {"forbidden": 0, "local_path": 0, "pii": 0, "redaction": 0, "secret": 0},
            "scanner_version": crate::core::hive::knowledge::screening::SCANNER_VERSION,
        },
    });
    sign::sign(&mut event, signer.key).map_err(local)?;
    Ok(event)
}

/// Where a run writes: the place's replica, key and signer, and for a
/// sealed place the epoch key its events are sealed with (per-event content
/// key, §3.3.4; `place.pk_id` is then the sealed `pk_id`).
pub struct Target<'a> {
    pub replica: &'a Replica,
    pub place: &'a Place,
    pub signer: &'a Signer<'a>,
    pub seal: Option<&'a crate::core::hive::events::sealed_body::SealKey>,
}

/// Queue a built class-W event as pending, sealed for a sealed place
/// (maintenance, curation, and the capture lifecycle's retract, supersedes
/// and confirm events).
pub(crate) fn queue(target: &Target<'_>, mut event: Value, stamp: Stamp) -> Result<(), HiveError> {
    if let Some(seal) = target.seal {
        // A sealed event always carries `flags` (signed; C0 event schema);
        // a maintenance field.set never adds commands.
        event["flags"] = json!({ "commands": false });
        crate::core::hive::events::sealed_body::seal_event(&mut event, seal, target.signer.key)?;
    }
    let id = event["event_id"]
        .as_str()
        .ok_or_else(|| HiveError::Local("built event without id".into()))?
        .to_string();
    let bytes = serde_json::to_vec(&event).map_err(|e| HiveError::Local(e.to_string()))?;
    target
        .replica
        .add_pending(&id, stamp.wall_ms, stamp.counter, None, &bytes)
}

/// Run the host-mode deterministic part (`--stale`): fold the replica,
/// derive the `anchor_changed` tasks against `checkout` and queue one
/// signed `field.set anchor_state` per task as pending (the sync push
/// sends them). With `confirm`, queue the re-anchoring of that claim
/// instead. Never contacts the server.
pub fn run_stale(
    target: &Target<'_>,
    checkout: &mut Checkout,
    req: &Request,
    now_ms: u64,
) -> Result<Counts, HiveError> {
    run_stale_at(target, checkout, req, now_ms, Stamp::default()).map(|(counts, _)| counts)
}

/// [`run_stale`] continuing the HLC `clock` of events queued before it in
/// the same run (the automatic re-check in `sync_vault`); returns the clock
/// after the last event it queued.
pub fn run_stale_at(
    target: &Target<'_>,
    checkout: &mut Checkout,
    req: &Request,
    now_ms: u64,
    clock: Stamp,
) -> Result<(Counts, Stamp), HiveError> {
    let mut graph = Graph::default();
    graph
        .fold(&target.replica.fold_events()?)
        .map_err(|e| HiveError::Protocol(e.to_string()))?;
    let mut counts = Counts::default();
    let mut clock = clock;
    if let Some(node_id) = &req.confirm {
        let node = graph
            .live_nodes()
            .find(|(id, _)| *id == node_id)
            .map(|(_, n)| n)
            .ok_or_else(|| HiveError::Config(format!("{node_id}: no live claim here")))?;
        let anchors = node
            .fields
            .get("anchors")
            .and_then(Value::as_array)
            .filter(|a| !a.is_empty())
            .ok_or_else(|| HiveError::Config(format!("{node_id}: the claim has no anchors")))?;
        let Some(fresh) = reanchored(anchors, checkout, now_ms) else {
            return Err(HiveError::Config(format!(
                "{node_id}: an anchored file or symbol is gone; it cannot be confirmed at this code"
            )));
        };
        clock = hlc::send(clock, now_ms);
        let event = field_set(
            target.place,
            node_id,
            "anchors",
            Value::Array(fresh),
            Some(now_ms),
            clock,
            target.signer,
        )?;
        queue(target, event, clock)?;
        counts.done = 1;
        // A claim the re-check had marked is current again once confirmed.
        if matches!(
            node.fields.get("anchor_state").and_then(Value::as_str),
            Some("to_verify" | "gone")
        ) {
            clock = hlc::send(clock, now_ms);
            let event = field_set(
                target.place,
                node_id,
                "anchor_state",
                Value::from(AnchorVerdict::Fresh.as_str()),
                None,
                clock,
                target.signer,
            )?;
            queue(target, event, clock)?;
        }
        return Ok((counts, clock));
    }
    // A state already queued and not yet folded is not queued twice.
    let queued: std::collections::HashSet<(String, String)> = target
        .replica
        .pending()?
        .iter()
        .filter_map(|(_, bytes)| serde_json::from_slice::<Value>(bytes).ok())
        .filter(|e| e["op"] == "field.set" && e["body"]["field"] == "anchor_state")
        .filter_map(|e| {
            Some((
                e["target"].as_str()?.to_string(),
                e["body"]["value"].as_str()?.to_string(),
            ))
        })
        .collect();
    let (tasks, skipped) = anchor_tasks(&graph, checkout, req.limit);
    counts.skipped = skipped;
    for task in tasks {
        if queued.contains(&(task.node_id.clone(), task.verdict.as_str().to_string())) {
            counts.skipped += 1;
            continue;
        }
        clock = hlc::send(clock, now_ms);
        let event = field_set(
            target.place,
            &task.node_id,
            "anchor_state",
            Value::from(task.verdict.as_str()),
            None,
            clock,
            target.signer,
        )?;
        queue(target, event, clock)?;
        counts.done += 1;
        match task.verdict {
            AnchorVerdict::ToVerify => counts.to_verify += 1,
            AnchorVerdict::Gone => counts.gone += 1,
            AnchorVerdict::Fresh => {}
        }
    }
    Ok((counts, clock))
}

/// A host-mode run: `--stale` queues the deterministic events, the judgment
/// kinds only print where they run (no E42 call in host mode, §3.15b), and
/// with `api` the place's pending events are pushed (E5). Every pushed
/// event goes out; the server alone holds large retractions, which stay
/// pending and count as sent to review. Returns the counts and the lines
/// to print.
pub fn run_host(
    target: &Target<'_>,
    checkout: &mut Checkout,
    req: &Request,
    api: Option<(&dyn crate::core::hive::sync::driver::PlaceApi, &str)>,
    now_ms: u64,
) -> Result<(Counts, Vec<String>), HiveError> {
    let mut lines = Vec::new();
    let mut counts = if req.stale {
        run_stale(target, checkout, req, now_ms)?
    } else {
        Counts::default()
    };
    if req.contradictions || req.dedupe {
        lines.push(handover_line(&req.vault));
    }
    if req.contradictions {
        // C0 2.0 places: a person marks a contradiction directly.
        lines.push(
            "Mark a contradiction you found: lean-ctx knowledge contradict <node> <other-node>"
                .into(),
        );
    }
    if let Some((api, place_ref)) = api
        && !target.replica.pending()?.is_empty()
    {
        let push =
            crate::core::hive::sync::driver::push_pending(api, target.replica, place_ref, &[])?;
        counts.sent_to_review += push.held;
        if !push.failed.is_empty() {
            lines.push(format!(
                "{} event(s) refused: {}",
                push.failed.len(),
                push.failed
                    .iter()
                    .map(|(_, why)| why.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    lines.push(counts.line());
    Ok((counts, lines))
}

#[cfg(test)]
#[path = "maintain_tests.rs"]
pub(super) mod tests;
