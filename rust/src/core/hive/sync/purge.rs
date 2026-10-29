// SPDX-License-Identifier: Apache-2.0
//! Purges on the engine (SPEC-ENGINE §3.4.6, V4.7): after a fold, every
//! node a purge removed leaves the dedupe index (its keys move to
//! `purge_deny`, so the text is never captured again), its own local text is
//! removed from the project knowledge, and each tombstone gets one signed
//! acknowledgement for the place (E9 / `purge_acks[]` on E5).

use ed25519_dalek::{Signer as _, SigningKey};
use serde_json::{Value, json};

use crate::core::hive::HiveError;
use crate::core::hive::events::sign::key_id;
use crate::core::hive::replica::Replica;
use crate::core::hive::replica::fold::Graph;
use lean_ctx_protocol::hive::signing_input;

/// Signature domain of a purge acknowledgement.
pub const PURGE_ACK_DOMAIN: &str = "leanctx.hive.purge_ack.v1\n";

/// A signed acknowledgement that this engine applied tombstone `seq` of
/// `purge_id` in `place` at `applied_at` (ms).
pub fn ack(
    place: &str,
    purge_id: &str,
    seq: u64,
    applied_at: u64,
    key: &SigningKey,
) -> Result<Value, HiveError> {
    let mut ack = json!({
        "applied_at": applied_at,
        "key_id": key_id(key.verifying_key().as_bytes()),
        "place": place,
        "purge_id": purge_id,
        "seq": seq,
    });
    let input =
        signing_input(PURGE_ACK_DOMAIN, &ack).map_err(|e| HiveError::Protocol(e.to_string()))?;
    ack["signature"] = Value::String(hex::encode(key.sign(&input).to_bytes()));
    Ok(ack)
}

/// What [`apply`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Applied {
    /// Nodes whose dedupe keys moved to `purge_deny`.
    pub denied: usize,
    /// Tombstones newly acknowledged (pending until the place accepts).
    pub new_acks: usize,
}

/// Apply what `graph` purged to the replica's indexes and queue one ack per
/// tombstone not acknowledged yet (its first `applied_at` is kept).
pub fn apply(replica: &mut Replica, graph: &Graph, now_ms: u64) -> Result<Applied, HiveError> {
    let mut done = Applied::default();
    for (node_id, purge) in &graph.purged_nodes {
        replica.deny_node(node_id, &purge.reason)?;
        done.denied += 1;
    }
    let mut acks = replica.purge_acks()?;
    for (purge_id, seq) in &graph.tombstones {
        let key = (purge_id.clone(), *seq);
        if acks.sent.contains(&key)
            || acks
                .pending
                .iter()
                .any(|(p, s, _)| p == purge_id && s == seq)
        {
            continue;
        }
        acks.pending.push((purge_id.clone(), *seq, now_ms));
        done.new_acks += 1;
    }
    if done.new_acks > 0 {
        replica.set_purge_acks(&acks)?;
    }
    Ok(done)
}

/// The pending acks of `replica`, signed for `place`.
pub fn signed_pending(
    replica: &Replica,
    place: &str,
    key: &SigningKey,
) -> Result<Vec<Value>, HiveError> {
    replica
        .purge_acks()?
        .pending
        .iter()
        .map(|(purge_id, seq, applied_at)| ack(place, purge_id, *seq, *applied_at, key))
        .collect()
}

/// The place accepted these acks: they move from pending to sent.
pub fn mark_sent(replica: &Replica, acks: &[Value]) -> Result<(), HiveError> {
    if acks.is_empty() {
        return Ok(());
    }
    let mut state = replica.purge_acks()?;
    for a in acks {
        let (Some(p), Some(s)) = (a["purge_id"].as_str(), a["seq"].as_u64()) else {
            continue;
        };
        state.pending.retain(|(pp, ss, _)| !(pp == p && *ss == s));
        state.sent.insert((p.to_string(), s));
    }
    replica.set_purge_acks(&state)
}

/// Remove the local text a purge took out of the place: facts recorded in
/// `own_sources` whose current value still has the recorded digest.
/// Returns how many local facts were removed.
pub fn purge_local_store(
    replica: &Replica,
    graph: &Graph,
    project_root: &str,
) -> Result<usize, HiveError> {
    let mut targets = Vec::new();
    for node_id in graph.purged_nodes.keys() {
        if let Some((kind, source)) = replica.own_source(node_id)?
            && kind == "fact"
            && let (Some(category), Some(key), Some(digest)) = (
                source["category"].as_str(),
                source["key"].as_str(),
                source["value_digest"].as_str(),
            )
        {
            targets.push((category.to_string(), key.to_string(), digest.to_string()));
        }
    }
    if targets.is_empty() {
        return Ok(0);
    }
    let (_, removed) = crate::core::knowledge::ProjectKnowledge::mutate_locked(project_root, |k| {
        let before = k.facts.len();
        k.facts.retain(|f| {
            !targets.iter().any(|(c, key, d)| {
                f.category == *c && f.key == *key && replica.value_digest(&f.value) == *d
            })
        });
        let removed = before - k.facts.len();
        if removed > 0 {
            k.rebuild_index();
        }
        removed
    })
    .map_err(HiveError::Local)?;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn fixture(rel: &str) -> Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("crates/lean-ctx-protocol/tests/fixtures/hive-v2")
            .join(rel);
        serde_json::from_slice(&std::fs::read(path).expect("fixture")).expect("json")
    }

    #[test]
    fn acks_match_the_contract_vector() {
        let v = fixture("vectors/purge_ack.vectors.json");
        let keys = fixture("vectors/signing.vectors.json");
        let seed: [u8; 32] = hex::decode(keys["keys"]["instance_a"]["seed_hex"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let key = SigningKey::from_bytes(&seed);
        for case in v["cases"].as_array().unwrap() {
            let want = fixture(case["fixture"].as_str().unwrap());
            let got = ack(
                want["place"].as_str().unwrap(),
                want["purge_id"].as_str().unwrap(),
                want["seq"].as_u64().unwrap(),
                want["applied_at"].as_u64().unwrap(),
                &key,
            )
            .unwrap();
            assert_eq!(got, want);
            let mut unsigned = got.clone();
            unsigned.as_object_mut().unwrap().remove("signature");
            assert_eq!(
                String::from_utf8(signing_input(PURGE_ACK_DOMAIN, &unsigned).unwrap()).unwrap(),
                case["signing_input"].as_str().unwrap()
            );
        }
    }

    #[test]
    fn a_purge_denies_the_node_and_acks_once() {
        use crate::core::hive::replica::fold::FoldEvent;
        let data = tempfile::tempdir().unwrap();
        let (mut replica, _) = Replica::open(data.path(), "pl_purge", &[2; 32]).unwrap();
        let dk = [5u8; 32];
        replica.add_dedupe_key(&dk, "n_a").unwrap();
        replica
            .put_signature(
                "n_a",
                &crate::core::hive::capture::dedupe::signature(&[1; 32], "x"),
            )
            .unwrap();
        let mut graph = Graph::default();
        let add = FoldEvent {
            seq: 1,
            event_id: "E1".into(),
            op: "node.add".into(),
            target: Some("n_a".into()),
            node_type: Some("fact".into()),
            body: Some(json!({"statement": "Secret claim."})),
            sig_invalid: false,
            provenance: Some("own".into()),
            review_state: Some("confirmed".into()),
        };
        let purge = FoldEvent {
            seq: 2,
            event_id: "E2".into(),
            op: "purge".into(),
            target: None,
            node_type: None,
            body: Some(json!({"purge_id": "prg_x", "reason": "leaked_secret", "targets": ["E1"]})),
            sig_invalid: false,
            provenance: None,
            review_state: None,
        };
        graph.fold(&[add, purge]).unwrap();

        let done = apply(&mut replica, &graph, 1000).unwrap();
        assert_eq!(
            done,
            Applied {
                denied: 1,
                new_acks: 1
            }
        );
        assert!(replica.is_purged(&dk).unwrap(), "never captured again");
        assert!(replica.dedupe_node(&dk).unwrap().is_none());
        assert!(replica.signatures().unwrap().is_empty());

        // A second apply keeps the first applied_at and adds nothing.
        assert_eq!(apply(&mut replica, &graph, 2000).unwrap().new_acks, 0);
        let key = SigningKey::from_bytes(&[0x49; 32]);
        let acks = signed_pending(&replica, "vault:x", &key).unwrap();
        assert_eq!(acks.len(), 1);
        assert_eq!(acks[0]["applied_at"], 1000);
        mark_sent(&replica, &acks).unwrap();
        assert!(
            signed_pending(&replica, "vault:x", &key)
                .unwrap()
                .is_empty()
        );
        assert_eq!(apply(&mut replica, &graph, 3000).unwrap().new_acks, 0);
    }
}
