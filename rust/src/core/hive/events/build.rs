// SPDX-License-Identifier: Apache-2.0
//! Building signed C0 2.0 events from what the capture gate let through
//! (SPEC-ENGINE §3.1.2 step 6, §3.3.1).
//!
//! A claim becomes one `node.add` whose target is its keyed node id. Its
//! valid references go into `applies_to` (contract form, at most 5 × 120
//! characters); every `file:`/`sym:` entry gets a code anchor with the
//! sha256 of the anchored bytes at `HEAD` and that commit, as the contract
//! requires for facts.

use ed25519_dalek::SigningKey;
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};

use super::{hlc::Stamp, ids, sign};
use crate::core::hive::HiveError;
use crate::core::hive::capture::refs::{Checkout, Reference};
use crate::core::hive::capture::{CaptureCandidate, GATE_VERSION, dedupe};
use crate::core::knowledge::NodeType;

pub const EVENT_SCHEMA: &str = "leanctx.hive.event/v2";
/// `applies_to` limits of a v4 claim body.
const MAX_APPLIES_TO: usize = 5;
const MAX_APPLIES_TO_CHARS: usize = 120;
/// Anchors per node.
const MAX_ANCHORS: usize = 16;

/// The place an event is written to.
#[derive(Debug, Clone)]
pub struct Place {
    /// `vault` or `swarm`.
    pub kind: String,
    pub id: String,
    /// The place id key (`id_salt`, or the sealed `pk_id`): node ids and
    /// body commitments.
    pub pk_id: [u8; 32],
    /// Last server `seq` folded here.
    pub causal_base: u64,
}

/// Who signs: the instance key (workspace places) or an alias key, with the
/// matching HLC node and worker id. Production paths build it with
/// [`Signer::workspace`] or [`Signer::alias`], never field by field: the
/// maintain CLI once set `hlc.node` to the raw `inst_<hex>` id and Hive
/// refused every event (`invalid_event shape /hlc/node`, dogfood 25.10.).
pub struct Signer<'a> {
    pub key: &'a SigningKey,
    pub hlc_node: String,
    pub worker_id: String,
}

impl<'a> Signer<'a> {
    /// A workspace place's signer: the instance key, and as `hlc.node` the
    /// instance id as C0 2.0 names it (the hyphenated UUID; a 1.x enrolment
    /// stores `inst_<32 hex>`, which the event schema refuses).
    pub fn workspace(key: &'a SigningKey, instance_id: &str, worker_id: &str) -> Self {
        Self {
            key,
            hlc_node: crate::core::hive::workers::canonical_instance_id(instance_id),
            worker_id: worker_id.to_string(),
        }
    }

    /// An alias signer (public vaults, swarms, R38): the alias key of
    /// `place_ref` and its keyed 16-hex `hlc.node`.
    pub fn alias(
        alias_key: &'a SigningKey,
        keys: &crate::core::hive::instance_key::InstanceKeys,
        place_ref: &str,
        worker_id: &str,
    ) -> Self {
        Self {
            key: alias_key,
            hlc_node: crate::core::hive::workers::alias_hlc_node(keys, place_ref),
            worker_id: worker_id.to_string(),
        }
    }
}

fn local(e: impl std::fmt::Display) -> HiveError {
    HiveError::Local(e.to_string())
}

/// `reasons_passed_hash`: which gate version and rules a node passed.
pub fn reasons_passed_hash(node_type: NodeType) -> String {
    let rules = if node_type == NodeType::Procedure {
        "G1,G10,G11,G12,procedure"
    } else {
        "G1,G2,G3,G4,G5,G6,G7,G8,G9,G10,G11,G12"
    };
    blake3::hash(format!("leanctx.hive.capture.v{GATE_VERSION}:{rules}").as_bytes())
        .to_hex()
        .to_string()
}

/// `applies_to` and the code anchors for a candidate's valid references.
pub fn applies_and_anchors(refs: &[String], checkout: &mut Checkout) -> (Vec<String>, Vec<Value>) {
    let head = checkout.head();
    let mut applies = Vec::new();
    let mut anchors = Vec::new();
    for raw in refs {
        if applies.len() == MAX_APPLIES_TO {
            break;
        }
        let Some(r) = crate::core::hive::capture::refs::parse(raw) else {
            continue;
        };
        let text = r.to_contract();
        if text.chars().count() > MAX_APPLIES_TO_CHARS {
            continue;
        }
        let (anchor_ref, bytes) = match &r {
            Reference::File { path, .. } => (
                format!("file:{path}"),
                checkout.blob_bytes(path).map(<[u8]>::to_vec),
            ),
            Reference::Symbol { path, symbol } => {
                (text.clone(), checkout.symbol_span(path, symbol))
            }
            _ => {
                applies.push(text);
                continue;
            }
        };
        // A code entry without an anchor would break the contract rule
        // (anchors required for file:/sym: entries); such an entry is left
        // out rather than shared unanchored.
        let (Some(bytes), Some(commit)) = (bytes, head.clone()) else {
            continue;
        };
        let applies_text = if matches!(r, Reference::File { .. }) {
            anchor_ref.clone()
        } else {
            text
        };
        if !applies.contains(&applies_text) {
            applies.push(applies_text);
        }
        if anchors.len() < MAX_ANCHORS
            && !anchors
                .iter()
                .any(|a: &Value| a["ref"] == anchor_ref.as_str())
        {
            anchors.push(json!({
                "ref": anchor_ref,
                "digest": hex::encode(Sha256::digest(&bytes)),
                "commit": commit,
            }));
        }
    }
    (applies, anchors)
}

/// The body of a claim node (`fact`, `decision`, `gotcha`, `finding`) or a
/// procedure.
pub fn claim_body(
    c: &CaptureCandidate,
    applies_to: &[String],
    anchors: Vec<Value>,
    sealed: bool,
) -> Result<Value, HiveError> {
    let mut body = Map::new();
    body.insert("body_nonce".into(), Value::String(sign::new_body_nonce()?));
    body.insert("evidence".into(), Value::from(c.evidence.as_str()));
    match c.node_type {
        NodeType::Procedure => {
            body.insert("title".into(), Value::from(c.statement.clone()));
            body.insert("steps".into(), json!(c.steps));
            if !c.commands.is_empty() {
                body.insert(
                    "commands".into(),
                    serde_json::to_value(&c.commands).map_err(local)?,
                );
            }
        }
        _ => {
            body.insert("statement".into(), Value::from(c.statement.clone()));
        }
    }
    // C0 `decision_body` requires `status`; a captured decision is in force
    // (`superseded`/`reverted` come later as their own events). Without it
    // Hive refused every captured decision (`invalid_event shape
    // /body/status`), so a vault never received one.
    if c.node_type == NodeType::Decision {
        body.insert("status".into(), Value::from("active"));
    }
    if c.node_type == NodeType::Fact {
        if !applies_to.is_empty() {
            body.insert("applies_to".into(), json!(applies_to));
        }
        body.insert(
            "sensitivity".into(),
            Value::from(
                if sealed && c.sensitivity > crate::core::sensitivity::SensitivityLevel::Public {
                    "internal"
                } else {
                    "public"
                },
            ),
        );
    }
    if matches!(
        c.node_type,
        NodeType::Fact | NodeType::Gotcha | NodeType::Procedure
    ) && !anchors.is_empty()
    {
        body.insert("anchors".into(), Value::Array(anchors));
    }
    Ok(Value::Object(body))
}

/// A signed `node.add` for a gated candidate. Returns the event and its
/// target node id.
pub fn node_add(
    c: &CaptureCandidate,
    body: &Value,
    place: &Place,
    stamp: Stamp,
    signer: &Signer<'_>,
) -> Result<(Value, String), HiveError> {
    let target = if c.node_type == NodeType::Procedure {
        ids::new_random_id("n")?
    } else {
        ids::claim_node_id(&dedupe::exact_key(&place.pk_id, c.node_type, &c.statement))
    };
    let body_commit = sign::body_commit(&place.pk_id, body).map_err(local)?;
    let mut event = json!({
        "schema": EVENT_SCHEMA,
        "event_id": ids::new_event_id(stamp.wall_ms)?,
        "place": {"kind": place.kind, "id": place.id},
        "op": "node.add",
        "target": target,
        "node_type": c.node_type.as_str(),
        "body": body,
        "body_commit": body_commit,
        "hlc": {"wall_ms": stamp.wall_ms, "counter": stamp.counter, "node": signer.hlc_node},
        "causal_base": place.causal_base,
        "actor": {"kind": "worker"},
        "worker_id": signer.worker_id,
        "provenance_claim": "own",
        "capture": {
            "gate_version": GATE_VERSION,
            "evidence": c.evidence.as_str(),
            "reasons_passed_hash": reasons_passed_hash(c.node_type),
        },
        "screening": {
            "counts": {"forbidden": 0, "local_path": 0, "pii": 0, "redaction": 0, "secret": 0},
            "scanner_version": crate::core::hive::knowledge::screening::SCANNER_VERSION,
        },
    });
    if c.node_type == NodeType::Procedure {
        event["flags"] = json!({"commands": !c.commands.is_empty()});
    }
    sign::sign(&mut event, signer.key).map_err(local)?;
    Ok((event, target))
}

/// A signed `edge.add` `from` a claim this engine captured `to` a node it
/// builds on (v5 2.2.2, [`crate::core::hive::capture::derived`]): written
/// like the claim itself (provenance `own`, the claim's evidence).
pub fn edge_add(
    kind: &str,
    from: &str,
    to: &str,
    evidence: &str,
    place: &Place,
    stamp: Stamp,
    signer: &Signer<'_>,
) -> Result<Value, HiveError> {
    let body = json!({
        "body_nonce": sign::new_body_nonce()?,
        "from": from,
        "kind": kind,
        "to": to,
    });
    let body_commit = sign::body_commit(&place.pk_id, &body).map_err(local)?;
    let mut event = json!({
        "schema": EVENT_SCHEMA,
        "event_id": ids::new_event_id(stamp.wall_ms)?,
        "place": {"kind": place.kind, "id": place.id},
        "op": "edge.add",
        "target": ids::new_random_id("e")?,
        "body": body,
        "body_commit": body_commit,
        "hlc": {"wall_ms": stamp.wall_ms, "counter": stamp.counter, "node": signer.hlc_node},
        "causal_base": place.causal_base,
        "actor": {"kind": "worker"},
        "worker_id": signer.worker_id,
        "provenance_claim": "own",
        "capture": {
            "gate_version": GATE_VERSION,
            "evidence": evidence,
            "reasons_passed_hash": blake3::hash(
                format!("leanctx.hive.capture.v{GATE_VERSION}:edge:{kind}").as_bytes()
            )
            .to_hex()
            .to_string(),
        },
        "screening": {
            "counts": {"forbidden": 0, "local_path": 0, "pii": 0, "redaction": 0, "secret": 0},
            "scanner_version": crate::core::hive::knowledge::screening::SCANNER_VERSION,
        },
    });
    sign::sign(&mut event, signer.key).map_err(local)?;
    Ok(event)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::knowledge::EvidenceLevel;
    use crate::core::sensitivity::SensitivityLevel;

    #[test]
    fn a_claim_becomes_a_signed_node_add_with_anchors() {
        let dir = crate::core::hive::capture::tests::repo();
        let mut checkout = Checkout::new(dir.path());
        let c = CaptureCandidate {
            node_type: NodeType::Fact,
            statement: "The relay drops frames when the link is slow.".into(),
            steps: vec![],
            commands: vec![],
            refs: vec![],
            evidence: EvidenceLevel::Observed,
            sensitivity: SensitivityLevel::Public,
        };
        let refs = vec![
            "file:src/lib.rs".to_string(),
            "sym:src/lib.rs::relay".to_string(),
            "https://docs.rs/blake3/".to_string(),
        ];
        let (applies, anchors) = applies_and_anchors(&refs, &mut checkout);
        assert_eq!(applies, refs);
        assert_eq!(anchors.len(), 2);
        let content = std::fs::read(dir.path().join("src/lib.rs")).unwrap();
        assert_eq!(anchors[0]["digest"], hex::encode(Sha256::digest(&content)));
        assert_eq!(anchors[0]["commit"].as_str().unwrap().len(), 40);

        let body = claim_body(&c, &applies, anchors, false).unwrap();
        let key = SigningKey::from_bytes(&[0x49; 32]);
        let place = Place {
            kind: "vault".into(),
            id: "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01".into(),
            pk_id: [0; 32],
            causal_base: 7,
        };
        let signer = Signer {
            key: &key,
            hlc_node: "6f2b9c4e-1a3d-4e5f-8a7b-0c1d2e3f4a5b".into(),
            worker_id: "wkr_vk4oelpv5lzjmev44hun".into(),
        };
        let (event, target) = node_add(
            &c,
            &body,
            &place,
            Stamp {
                wall_ms: 1_792_800_001_000,
                counter: 0,
            },
            &signer,
        )
        .unwrap();
        assert_eq!(event["target"], target.as_str());
        assert!(target.starts_with("n_") && target.len() == 28);
        assert!(sign::verify(&event, key.verifying_key().as_bytes()));
        assert_eq!(
            event["body_commit"],
            sign::body_commit(&place.pk_id, &event["body"]).unwrap()
        );
        assert_eq!(event["event_id"].as_str().unwrap().len(), 26);
        // Same statement, same place key → same node on every engine.
        assert_eq!(
            target,
            ids::claim_node_id(&dedupe::exact_key(
                &[0; 32],
                NodeType::Fact,
                "the relay drops frames when the link is slow"
            ))
        );
    }

    /// Every claim body the engine builds has exactly the shape C0
    /// `event.schema.json` names for its node type: each required member,
    /// no member outside `properties`, enum members within their enum. A
    /// captured decision without `status` was refused by Hive
    /// (`invalid_event shape /body/status`, live 26.10.).
    #[test]
    fn every_claim_body_has_its_c0_shape() {
        let path = format!(
            "{}/crates/lean-ctx-protocol/tests/fixtures/hive-v2/schemas/event.schema.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let schema: Value = serde_json::from_slice(&std::fs::read(&path).expect(&path)).unwrap();
        for (node_type, def) in [
            (NodeType::Fact, "fact_body"),
            (NodeType::Decision, "decision_body"),
            (NodeType::Gotcha, "gotcha_body"),
            (NodeType::Finding, "finding_body"),
            (NodeType::Procedure, "procedure_body"),
        ] {
            let c = CaptureCandidate {
                node_type,
                statement: "Releases are tagged from main after the smoke test passes.".into(),
                steps: vec!["Run the smoke test against staging.".into()],
                commands: vec![],
                refs: vec![],
                evidence: EvidenceLevel::Stated,
                sensitivity: SensitivityLevel::Public,
            };
            let body = claim_body(&c, &["file:docs/deploy.md".into()], Vec::new(), false).unwrap();
            let def = &schema["$defs"][def];
            let props = def["properties"].as_object().unwrap();
            for required in def["required"].as_array().unwrap() {
                let name = required.as_str().unwrap();
                assert!(body.get(name).is_some(), "{node_type:?}: {name} missing");
            }
            for (name, value) in body.as_object().unwrap() {
                let prop = props
                    .get(name)
                    .unwrap_or_else(|| panic!("{node_type:?}: {name} not in C0"));
                if let Some(allowed) = prop["enum"].as_array() {
                    assert!(allowed.contains(value), "{node_type:?}: {name} = {value}");
                }
            }
        }
    }
}
