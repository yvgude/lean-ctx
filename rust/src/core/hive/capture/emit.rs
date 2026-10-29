// SPDX-License-Identifier: Apache-2.0
//! Capture step 6 (SPEC-ENGINE §3.1.2): what the gate let through becomes
//! signed `node.add` events in the place's replica — queued as pending (the
//! offline queue the sync driver pushes), with the keyed dedupe key, the
//! encrypted MinHash signature and an `own_sources` row, so a later run
//! never sends the same claim twice and a purge can find the local text.

use serde_json::json;

use super::refs::Checkout;
use super::{CapturedFact, dedupe};
use crate::core::hive::HiveError;
use crate::core::hive::events::build::{self, Place, Signer};
use crate::core::hive::events::hlc::{self, Stamp};
use crate::core::hive::events::sealed_body::{self, SealKey};
use crate::core::hive::replica::Replica;
use crate::core::knowledge::NodeType;

/// What [`emit`] queued.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Emitted {
    /// `(event_id, node_id)` of every queued event.
    pub queued: Vec<(String, String)>,
    /// Claims the place already holds (exact key in the replica).
    pub duplicates: usize,
    /// Claims a purge removed (`purged_content`).
    pub purged: usize,
    /// `(event_id, to)` of every `derived_from` edge queued after its claim
    /// (v5 2.2.2, [`super::derived`]).
    pub derived: Vec<(String, String)>,
    /// The clock after the last stamp.
    pub clock: Stamp,
}

/// Where captured facts go: the place's replica, its key, the signer, and
/// the checkout's name in `own_sources` (never shared).
pub struct Target<'a> {
    pub replica: &'a Replica,
    pub place: &'a Place,
    pub signer: &'a Signer<'a>,
    pub project_hash: &'a str,
    /// A sealed place's epoch key: bodies leave only as `{ct, nonce}` under
    /// a per-event key (§3.3.4).
    pub seal: Option<&'a SealKey>,
}

/// Queue a signed `node.add` per captured fact.
pub fn emit(
    target: &Target<'_>,
    checkout: &mut Checkout,
    facts: &[CapturedFact],
    clock: Stamp,
    now_ms: u64,
) -> Result<Emitted, HiveError> {
    let Target {
        replica,
        place,
        signer,
        project_hash,
        seal,
    } = *target;
    let mut out = Emitted {
        clock,
        ..Emitted::default()
    };
    for fact in facts {
        let c = &fact.candidate;
        let dk = dedupe::exact_key(&place.pk_id, c.node_type, &c.statement);
        // A purged statement is never captured again (step 3c).
        if replica.is_purged(&dk)? {
            out.purged += 1;
            continue;
        }
        if c.node_type != NodeType::Procedure && replica.dedupe_node(&dk)?.is_some() {
            out.duplicates += 1;
            continue;
        }
        let (applies, anchors) = build::applies_and_anchors(&fact.refs, checkout);
        let body = build::claim_body(c, &applies, anchors, seal.is_some())?;
        out.clock = hlc::send(out.clock, now_ms);
        let (mut event, node_id) = build::node_add(c, &body, place, out.clock, signer)?;
        if let Some(seal) = seal {
            sealed_body::seal_event(&mut event, seal, signer.key)?;
        }
        let event_id = queue(replica, &event, out.clock)?;
        replica.add_dedupe_key(&dk, &node_id)?;
        replica.put_signature(&node_id, &dedupe::signature(&place.pk_id, &c.statement))?;
        replica.put_own_source(
            &node_id,
            "fact",
            &json!({
                "project_hash": project_hash,
                "category": fact.category,
                "key": fact.key,
                "value_digest": replica.value_digest(&fact.value),
            }),
        )?;
        // Provenance: the edges follow their claim in the same push, only
        // for nodes the body itself names (`node:<id>`, e.g. in
        // `applies_to`), the usage reference the server counts (v5 2.6.1).
        let cited = body.to_string();
        for to in fact
            .derived_from
            .iter()
            .filter(|to| cited.contains(&format!("node:{to}")))
        {
            out.clock = hlc::send(out.clock, now_ms);
            let mut edge = build::edge_add(
                "derived_from",
                &node_id,
                to,
                c.evidence.as_str(),
                place,
                out.clock,
                signer,
            )?;
            if let Some(seal) = seal {
                sealed_body::seal_event(&mut edge, seal, signer.key)?;
            }
            out.derived
                .push((queue(replica, &edge, out.clock)?, to.clone()));
        }
        out.queued.push((event_id, node_id));
    }
    Ok(out)
}

/// Queue a built event as pending; its event id.
fn queue(replica: &Replica, event: &serde_json::Value, clock: Stamp) -> Result<String, HiveError> {
    let event_id = event["event_id"]
        .as_str()
        .ok_or_else(|| HiveError::Local("built event without id".into()))?
        .to_string();
    let bytes = serde_json::to_vec(event).map_err(|e| HiveError::Local(e.to_string()))?;
    replica.add_pending(&event_id, clock.wall_ms, clock.counter, None, &bytes)?;
    Ok(event_id)
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::SigningKey;

    use super::*;
    use crate::core::hive::capture::CaptureCandidate;
    use crate::core::knowledge::EvidenceLevel;
    use crate::core::sensitivity::SensitivityLevel;

    #[test]
    fn captured_facts_queue_once_as_signed_events() {
        let repo = crate::core::hive::capture::tests::repo();
        let data = tempfile::tempdir().unwrap();
        let (replica, _) = Replica::open(data.path(), "pl_emit", &[3; 32]).unwrap();
        let key = SigningKey::from_bytes(&[0x49; 32]);
        let place = Place {
            kind: "vault".into(),
            id: "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01".into(),
            pk_id: [9; 32],
            causal_base: 0,
        };
        let signer = Signer {
            key: &key,
            hlc_node: "6f2b9c4e-1a3d-4e5f-8a7b-0c1d2e3f4a5b".into(),
            worker_id: "wkr_vk4oelpv5lzjmev44hun".into(),
        };
        let fact = CapturedFact {
            candidate: CaptureCandidate {
                node_type: NodeType::Fact,
                statement: "The relay drops frames when the link is slow.".into(),
                steps: vec![],
                commands: vec![],
                refs: vec![],
                evidence: EvidenceLevel::Observed,
                sensitivity: SensitivityLevel::Public,
            },
            refs: vec!["file:src/lib.rs".into()],
            category: "arch".into(),
            key: "relay".into(),
            value: "The relay drops frames when the link is slow.".into(),
            derived_from: vec![],
        };
        let mut checkout = Checkout::new(repo.path());
        let start = Stamp {
            wall_ms: 1_792_800_000_000,
            counter: 0,
        };
        let target = Target {
            replica: &replica,
            place: &place,
            signer: &signer,
            project_hash: "ph",
            seal: None,
        };
        let first = emit(
            &target,
            &mut checkout,
            std::slice::from_ref(&fact),
            start,
            1_792_800_000_000,
        )
        .unwrap();
        assert_eq!(first.queued.len(), 1);
        assert_eq!(
            first.clock,
            Stamp {
                wall_ms: 1_792_800_000_000,
                counter: 1
            }
        );
        let pending = replica.pending().unwrap();
        assert_eq!(pending.len(), 1);
        let event: serde_json::Value = serde_json::from_slice(&pending[0].1).unwrap();
        assert!(crate::core::hive::events::sign::verify(
            &event,
            key.verifying_key().as_bytes()
        ));
        assert_eq!(event["body"]["anchors"][0]["ref"], "file:src/lib.rs");
        let (kind, source) = replica.own_source(&first.queued[0].1).unwrap().unwrap();
        assert_eq!(kind, "fact");
        assert_ne!(
            source["value_digest"],
            fact.value.as_str(),
            "only a keyed digest"
        );

        // The same claim again: already held, nothing queued.
        let again = emit(
            &target,
            &mut checkout,
            &[fact],
            first.clock,
            1_792_800_000_000,
        )
        .unwrap();
        assert_eq!((again.queued.len(), again.duplicates), (0, 1));
        assert_eq!(replica.pending().unwrap().len(), 1);
    }

    /// v5 2.2.2, 2.6.1: a claim that names vault nodes is followed by one
    /// signed `derived_from` edge per node its body cites, in the same push;
    /// a node the body does not name gets no edge.
    #[test]
    fn a_derived_claim_queues_its_provenance_edges_after_the_node() {
        let repo = crate::core::hive::capture::tests::repo();
        let data = tempfile::tempdir().unwrap();
        let (replica, _) = Replica::open(data.path(), "pl_derive", &[3; 32]).unwrap();
        let key = SigningKey::from_bytes(&[0x49; 32]);
        let place = Place {
            kind: "vault".into(),
            id: "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01".into(),
            pk_id: [9; 32],
            causal_base: 4,
        };
        let signer = Signer {
            key: &key,
            hlc_node: "6f2b9c4e-1a3d-4e5f-8a7b-0c1d2e3f4a5b".into(),
            worker_id: "wkr_vk4oelpv5lzjmev44hun".into(),
        };
        let statement = "The nightly export waits for advisory lock 42 before the ledger close.";
        let fact = CapturedFact {
            candidate: CaptureCandidate {
                node_type: NodeType::Fact,
                statement: statement.into(),
                steps: vec![],
                commands: vec![],
                refs: vec![],
                evidence: EvidenceLevel::Stated,
                sensitivity: SensitivityLevel::Public,
            },
            refs: vec![
                "file:src/lib.rs".into(),
                "node:n_aaaaaaaaaaaaaaaaaaaaaacaa4".into(),
            ],
            category: "arch".into(),
            key: "export".into(),
            value: statement.into(),
            derived_from: vec![
                "n_aaaaaaaaaaaaaaaaaaaaaacaa4".into(),
                "n_aaaaaaaaaaaaaaaaaaaaaacab4".into(),
            ],
        };
        let target = Target {
            replica: &replica,
            place: &place,
            signer: &signer,
            project_hash: "ph",
            seal: None,
        };
        let out = emit(
            &target,
            &mut Checkout::new(repo.path()),
            &[fact],
            Stamp::default(),
            1_792_800_000_000,
        )
        .unwrap();
        assert_eq!(out.queued.len(), 1);
        let node_id = &out.queued[0].1;
        assert_eq!(
            out.derived
                .iter()
                .map(|(_, to)| to.as_str())
                .collect::<Vec<_>>(),
            ["n_aaaaaaaaaaaaaaaaaaaaaacaa4"],
            "cab4 is not named in the body"
        );
        let pending: Vec<serde_json::Value> = replica
            .pending()
            .unwrap()
            .iter()
            .map(|(_, b)| serde_json::from_slice(b).unwrap())
            .collect();
        let ops: Vec<&str> = pending.iter().map(|e| e["op"].as_str().unwrap()).collect();
        assert_eq!(ops, ["node.add", "edge.add"]);
        // The usage reference the server checks: the claim body names it.
        assert!(
            pending[0]["body"]["applies_to"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a == "node:n_aaaaaaaaaaaaaaaaaaaaaacaa4"),
            "{}",
            pending[0]["body"]
        );
        for (edge, to) in pending[1..].iter().zip(&out.derived) {
            assert!(crate::core::hive::events::sign::verify(
                edge,
                key.verifying_key().as_bytes()
            ));
            assert_eq!(edge["body"]["kind"], "derived_from");
            assert_eq!(edge["body"]["from"], node_id.as_str());
            assert_eq!(edge["body"]["to"], to.1.as_str());
            assert_eq!(edge["provenance_claim"], "own");
            assert_eq!(edge["capture"]["evidence"], "stated");
            assert_eq!(edge["causal_base"], 4);
            assert!(
                edge["target"]
                    .as_str()
                    .is_some_and(|t| t.starts_with("e_") && t.len() == 28),
                "{edge}"
            );
        }
    }
}
