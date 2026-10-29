// SPDX-License-Identifier: Apache-2.0
//! Review decisions in sealed places (SPEC-ENGINE §3.5.5 "Decision",
//! RESOLUTIONS R36): the server cannot read a sealed item, so a member's
//! engine binds its decision to the item it decrypted (`item_digest`,
//! `command_digests`) and posts E41 with the `consent_id` of the
//! `procedure_confirm` a maintainer+ answered on W50. The instance-key
//! signature binds the request to this engine; it proves no human.

use ed25519_dalek::{Signer as _, SigningKey};
use serde_json::{Value, json};

use crate::core::hive::HiveError;
use crate::core::hive::events::sign::key_id;
use lean_ctx_protocol::hive::{canonical_json, signing_input};

/// Signature domain of an E41 body.
pub const SEALED_DECISION_DOMAIN: &str = "leanctx.hive.sealed_decision.v1\n";

fn digest(value: &Value) -> Result<String, HiveError> {
    let bytes = canonical_json(value).map_err(|e| HiveError::Protocol(e.to_string()))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

/// `hex(blake3(canonical_json(command)))` per command, in body order.
pub fn command_digests(commands: &[Value]) -> Result<Vec<String>, HiveError> {
    commands.iter().map(digest).collect()
}

/// `hex(blake3(canonical_json({events, node_ids})))` over the envelope
/// digests of the item's events in item order: any new event on the item's
/// nodes changes it, so a decision never covers a newer version.
pub fn item_digest(envelope_digests: &[String], node_ids: &[String]) -> Result<String, HiveError> {
    digest(&json!({"events": envelope_digests, "node_ids": node_ids}))
}

/// What a person decided on W50 for one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Accept,
    Reject,
}

impl Answer {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Reject => "reject",
        }
    }
}

/// The decision that goes to E41.
pub struct Decision<'a> {
    pub item_digest: &'a str,
    pub command_digests: &'a [String],
    pub answer: Answer,
    pub reason_code: &'a str,
    pub consent_id: &'a str,
    pub approved_at: u64,
}

/// The signed E41 body: `channel` is always `web` (the W50 answer is the
/// only proof E41 accepts).
pub fn body(decision: &Decision<'_>, key: &SigningKey) -> Result<Value, HiveError> {
    let mut body = json!({
        "answer": decision.answer.as_str(),
        "approved_at": decision.approved_at,
        "channel": "web",
        "command_digests": decision.command_digests,
        "consent_id": decision.consent_id,
        "item_digest": decision.item_digest,
        "key_id": key_id(key.verifying_key().as_bytes()),
        "reason_code": decision.reason_code,
    });
    let input = signing_input(SEALED_DECISION_DOMAIN, &body)
        .map_err(|e| HiveError::Protocol(e.to_string()))?;
    body["signature"] = Value::String(hex::encode(key.sign(&input).to_bytes()));
    Ok(body)
}

/// Post E41 for review item `rv` (`204`). `review_item_changed` means the
/// item has a newer version: reload it and ask again.
pub fn post(
    session: &crate::core::hive::auth::Session,
    rv: &str,
    body: &Value,
) -> Result<(), HiveError> {
    check_rv(rv)?;
    session.with_client(|c| c.post(&format!("/reviews/{rv}/sealed-decision"), body))?;
    Ok(())
}

fn check_rv(rv: &str) -> Result<(), HiveError> {
    if rv.len() > 3 && rv.starts_with("rv_") && rv[3..].bytes().all(|b| b.is_ascii_alphanumeric()) {
        Ok(())
    } else {
        Err(HiveError::Config(format!("{rv} is not a review item id")))
    }
}

/// E41a: one review item as the server holds it (R134).
pub fn fetch(session: &crate::core::hive::auth::Session, rv: &str) -> Result<Value, HiveError> {
    check_rv(rv)?;
    session.with_client(|c| c.get(&format!("/reviews/{rv}")))
}

/// E41a: the place's items waiting for a person, oldest first (all pages).
pub fn waiting(
    session: &crate::core::hive::auth::Session,
    place_ref: &str,
) -> Result<Vec<Value>, HiveError> {
    let mut out = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..100 {
        let mut path = format!("/places/{place_ref}/reviews?state=needs_person&limit=100");
        if let Some(a) = &after {
            path.push_str(&format!("&after={a}"));
        }
        let page = session.with_client(|c| c.get(&path))?;
        out.extend(page["items"].as_array().cloned().unwrap_or_default());
        match page["next"].as_str() {
            Some(next) if check_rv(next).is_ok() => after = Some(next.to_string()),
            _ => break,
        }
    }
    Ok(out)
}

/// What this engine decrypted for an item, bound to the server's version.
#[derive(Debug, Clone, PartialEq)]
pub struct Prepared {
    pub item_digest: String,
    pub command_digests: Vec<String>,
    /// `(node_id, node_type, text fields)` of the item's nodes.
    pub nodes: Vec<(String, String, Value)>,
    /// The procedure commands, in body order.
    pub commands: Vec<Value>,
}

/// Bind `item` (E41a) to the local replica: the envelopes of its events
/// (`envelope_of`) give the same `item_digest`, and the commands of its
/// folded nodes give the same `command_digests`. A mismatch means the
/// replica is behind or the item changed: sync and ask again.
pub fn prepare(
    item: &Value,
    envelope_of: &dyn Fn(&str) -> Option<Value>,
    graph: &crate::core::hive::replica::fold::Graph,
) -> Result<Prepared, HiveError> {
    let strings = |k: &str| -> Vec<String> {
        item[k]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    };
    let node_ids = strings("node_ids");
    let mut envelopes = Vec::new();
    for id in strings("events") {
        let envelope = envelope_of(&id).ok_or_else(|| {
            HiveError::Refused(format!(
                "event {id} of this item is not in the replica yet; run `lean-ctx hive sync`"
            ))
        })?;
        envelopes.push(
            crate::core::hive::events::sign::envelope_digest(&envelope)
                .map_err(HiveError::Protocol)?,
        );
    }
    let digest = item_digest(&envelopes, &node_ids)?;
    if item["item_digest"].as_str() != Some(digest.as_str()) {
        return Err(HiveError::Refused(
            "review_item_changed: the replica holds another version of this item; sync and ask again"
                .into(),
        ));
    }
    let mut nodes = Vec::new();
    let mut commands = Vec::new();
    for id in &node_ids {
        let node = graph.nodes.get(id).ok_or_else(|| {
            HiveError::Refused(format!(
                "node {id} is not folded here; run `lean-ctx hive sync`"
            ))
        })?;
        if let Some(list) = node.fields.get("commands").and_then(Value::as_array) {
            commands.extend(list.iter().cloned());
        }
        nodes.push((
            id.clone(),
            node.node_type.clone(),
            Value::Object(node.fields.clone()),
        ));
    }
    let command_digests = command_digests(&commands)?;
    if command_digests != strings("command_digests") {
        return Err(HiveError::Refused(
            "review_item_changed: the item's commands differ from the decrypted ones".into(),
        ));
    }
    Ok(Prepared {
        item_digest: digest,
        command_digests,
        nodes,
        commands,
    })
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

    fn strings(v: &Value) -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn item_digests_match_the_contract_vectors() {
        let v = fixture("vectors/review_item.vectors.json");
        for case in v["cases"].as_array().unwrap() {
            let got = item_digest(
                &strings(&case["envelope_digests"]),
                &strings(&case["node_ids"]),
            )
            .unwrap();
            assert_eq!(got, case["item_digest"].as_str().unwrap());
            let item = fixture(case["fixture"].as_str().unwrap());
            assert_eq!(item["item_digest"], case["item_digest"]);
            assert_eq!(item["command_digests"], case["command_digests"]);
        }
    }

    #[test]
    fn the_signed_body_matches_the_contract_vector() {
        let v = fixture("vectors/sealed_decision.vectors.json");
        let keys = fixture("vectors/signing.vectors.json");
        let seed: [u8; 32] = hex::decode(keys["keys"]["instance_a"]["seed_hex"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let key = SigningKey::from_bytes(&seed);
        for case in v["cases"].as_array().unwrap() {
            let want = fixture(case["fixture"].as_str().unwrap());
            let digests = strings(&want["command_digests"]);
            let got = body(
                &Decision {
                    item_digest: want["item_digest"].as_str().unwrap(),
                    command_digests: &digests,
                    answer: match want["answer"].as_str().unwrap() {
                        "accept" => Answer::Accept,
                        _ => Answer::Reject,
                    },
                    reason_code: want["reason_code"].as_str().unwrap(),
                    consent_id: want["consent_id"].as_str().unwrap(),
                    approved_at: want["approved_at"].as_u64().unwrap(),
                },
                &key,
            )
            .unwrap();
            assert_eq!(got, want);
            let mut unsigned = got.clone();
            unsigned.as_object_mut().unwrap().remove("signature");
            assert_eq!(
                String::from_utf8(signing_input(SEALED_DECISION_DOMAIN, &unsigned).unwrap())
                    .unwrap(),
                case["signing_input"].as_str().unwrap()
            );
        }
    }

    #[test]
    fn a_procedure_command_digest_is_its_canonical_blake3() {
        let event = fixture("events/node.add.procedure.json");
        let commands = event["body"]["commands"].as_array().unwrap();
        let got = command_digests(commands).unwrap();
        assert_eq!(got.len(), commands.len());
        assert_eq!(got, command_digests(commands).unwrap(), "deterministic");
        assert!(got.iter().all(|d| d.len() == 64));
    }

    #[test]
    fn prepare_binds_the_e41a_item_to_the_replica() {
        use crate::core::hive::replica::fold::{FoldEvent, Graph};
        let item = fixture("routes/review_item.get.response.json");
        let event = fixture("events/node.add.procedure.sealed.json");
        let sealed = fixture("vectors/sealed_event.vectors.json");
        let plain = sealed["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["fixture"] == "events/node.add.procedure.sealed.json")
            .unwrap()["plaintext_body"]
            .clone();
        let node_id = item["node_ids"][0].as_str().unwrap().to_string();
        let mut graph = Graph::default();
        graph
            .fold(&[FoldEvent {
                seq: 1,
                event_id: event["event_id"].as_str().unwrap().into(),
                op: "node.add".into(),
                target: Some(node_id.clone()),
                node_type: Some("procedure".into()),
                body: Some(plain),
                sig_invalid: false,
                provenance: Some("workspace".into()),
                review_state: Some("needs_person".into()),
            }])
            .unwrap();
        let id = event["event_id"].as_str().unwrap().to_string();
        let envelopes = |e: &str| (e == id).then(|| event.clone());
        let p = prepare(&item, &envelopes, &graph).unwrap();
        assert_eq!(p.item_digest, item["item_digest"].as_str().unwrap());
        assert_eq!(p.command_digests, strings(&item["command_digests"]));
        assert_eq!(p.commands.len(), 1);
        assert_eq!(p.nodes[0].0, node_id);

        // Not synced yet, or another version: refused before any consent.
        let none = |_: &str| None;
        assert!(
            prepare(&item, &none, &graph)
                .unwrap_err()
                .to_string()
                .contains("hive sync")
        );
        let mut changed = item.clone();
        changed["item_digest"] = json!("ab".repeat(32));
        assert!(
            prepare(&changed, &envelopes, &graph)
                .unwrap_err()
                .to_string()
                .contains("review_item_changed")
        );
        assert!(check_rv("rv_../x").is_err() && check_rv("rv_").is_err());
    }

    /// The sealed item binds what the engine decrypted: the envelope of the
    /// sealed procedure event and the commands of its plaintext body.
    #[test]
    fn the_sealed_item_binds_the_decrypted_procedure() {
        let item = fixture("review/item.sealed.json");
        let event = fixture("events/node.add.procedure.sealed.json");
        let envelope = crate::core::hive::events::sign::envelope_digest(&event).unwrap();
        let got = item_digest(&[envelope], &strings(&item["node_ids"])).unwrap();
        assert_eq!(got, item["item_digest"].as_str().unwrap());
        let sealed = fixture("vectors/sealed_event.vectors.json");
        let case = sealed["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["fixture"] == "events/node.add.procedure.sealed.json")
            .expect("sealed procedure case");
        let commands = case["plaintext_body"]["commands"].as_array().unwrap();
        assert_eq!(
            command_digests(commands).unwrap(),
            strings(&item["command_digests"])
        );
    }
}
