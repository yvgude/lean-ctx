// SPDX-License-Identifier: Apache-2.0
//! Local trust of sealed places (SPEC-ENGINE §3.5.5): the server cannot read
//! sealed bodies, so each member engine computes the trust bucket itself
//! with exactly the CONTRACT §3.14.1 score and §3.14.2 buckets (integer
//! math, `vectors/trust.vectors.json`), only from folded log data.

use serde_json::Value;

use crate::core::hive::replica::fold::{Graph, Node};

/// The inputs of the §3.14.1 score (the field names of the C0 vectors).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inputs {
    pub provenance: String,
    pub evidence: String,
    pub person_accept: bool,
    pub reviewer_accept: bool,
    pub history_events: u64,
    pub history_reverts: u64,
    pub delivered_at_ms: Option<u64>,
    pub t_ref_ms: u64,
    pub contradicts_edges: u64,
    pub contradicts_open: u64,
    pub inbound_refs: u64,
    pub fresh_test_refs: u64,
    pub refs_gone: u64,
    pub refs_to_verify: u64,
    pub public_vault: bool,
    pub author_costly_signal: bool,
    pub trust_pin: Option<String>,
}

impl Default for Inputs {
    /// The `defaults` of the C0 vectors.
    fn default() -> Self {
        Self {
            provenance: "workspace".into(),
            evidence: "stated".into(),
            person_accept: false,
            reviewer_accept: false,
            history_events: 0,
            history_reverts: 0,
            delivered_at_ms: None,
            t_ref_ms: 0,
            contradicts_edges: 0,
            contradicts_open: 0,
            inbound_refs: 0,
            fresh_test_refs: 0,
            refs_gone: 0,
            refs_to_verify: 0,
            public_vault: false,
            author_costly_signal: true,
            trust_pin: None,
        }
    }
}

const DAY_MS: u64 = 86_400_000;

/// Score (0–100) and bucket (`high`, `normal`, `low`) of CONTRACT §3.14.
pub fn score(i: &Inputs) -> (i64, &'static str) {
    let base = match i.provenance.as_str() {
        "own" => 60,
        "workspace" => 55,
        "public" => 30,
        _ => 20,
    };
    let evidence = match i.evidence.as_str() {
        "observed" => 15,
        "imported" => 5,
        _ => 0,
    };
    let review = if i.person_accept {
        20
    } else if i.reviewer_accept {
        15
    } else {
        0
    };
    let (events, reverts) = (
        i64::try_from(i.history_events).unwrap_or(i64::MAX),
        i64::try_from(i.history_reverts).unwrap_or(i64::MAX),
    );
    let history = if events < 20 {
        0
    } else if reverts.saturating_mul(5) > events {
        -10
    } else {
        // round(10 * (1 - reverts/events)), halves up.
        (20 * (events - reverts) + events) / (2 * events)
    };
    let mut usage = 0;
    if let Some(at) = i.delivered_at_ms
        && i.t_ref_ms.saturating_sub(at) >= 30 * DAY_MS
        && i.contradicts_edges == 0
    {
        usage += 5;
    }
    if i.inbound_refs >= 3 {
        usage += 5;
    }
    let tests = if i.fresh_test_refs >= 1 { 5 } else { 0 };
    let anchors = if i.refs_gone >= 1 {
        -20
    } else if i.refs_to_verify >= 1 {
        -10
    } else {
        0
    };
    let contradiction = if i.contradicts_open >= 1 { -15 } else { 0 };
    let mut s = (base + evidence + review + history + usage + tests + anchors + contradiction)
        .clamp(0, 100);
    if i.public_vault && !i.author_costly_signal {
        s = s.min(39);
    }
    let bucket = match i.trust_pin.as_deref() {
        Some("high") => "high",
        Some("normal") => "normal",
        _ if s >= 70 => "high",
        _ if s >= 40 => "normal",
        _ => "low",
    };
    (s, bucket)
}

/// The inputs a sealed engine derives for `node_id` from its folded graph.
// lean-ctx: author history (worker events and reverts within 90 days),
// delivery age and fresh test references need per-event worker ids and
// received_at, which the fold does not keep yet; they stay at the C0
// defaults (no effect on the score) until the fold carries them.
pub fn inputs(graph: &Graph, node_id: &str, node: &Node) -> Inputs {
    let f = |k: &str| node.fields.get(k).and_then(Value::as_str);
    let review = node.review_state.as_deref() == Some("confirmed");
    let mut i = Inputs {
        provenance: node
            .provenance
            .clone()
            .unwrap_or_else(|| "workspace".into()),
        evidence: f("evidence").unwrap_or("stated").to_string(),
        person_accept: review && f("reviewed_by") == Some("maintainer"),
        reviewer_accept: review && f("reviewed_by") == Some("reviewer"),
        trust_pin: f("trust_pin").map(str::to_string),
        ..Inputs::default()
    };
    for e in graph.edges.values() {
        let touches = e.from == node_id || e.to == node_id;
        match e.kind.as_str() {
            "contradicts" if touches => {
                i.contradicts_edges += 1;
                if !e.retracted {
                    i.contradicts_open += 1;
                }
            }
            "references" | "derived_from" if e.to == node_id && !e.retracted => {
                i.inbound_refs += 1;
            }
            "references" if e.from == node_id && !e.retracted => {
                let state = graph
                    .nodes
                    .get(&e.to)
                    .and_then(|n| n.fields.get("anchor_state"))
                    .and_then(Value::as_str);
                match state {
                    Some("gone") => i.refs_gone += 1,
                    Some("to_verify") => i.refs_to_verify += 1,
                    _ => {}
                }
            }
            _ => {}
        }
    }
    i
}

/// The local trust bucket of every live node of a sealed place.
pub fn buckets(graph: &Graph) -> std::collections::BTreeMap<String, &'static str> {
    graph
        .live_nodes()
        .map(|(id, node)| (id.clone(), score(&inputs(graph, id, node)).1))
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    fn apply(i: &mut Inputs, set: &Value) {
        for (k, v) in set.as_object().into_iter().flatten() {
            let n = || v.as_u64().unwrap_or(0);
            match k.as_str() {
                "provenance" => i.provenance = v.as_str().unwrap().into(),
                "evidence" => i.evidence = v.as_str().unwrap().into(),
                "person_accept" => i.person_accept = v.as_bool().unwrap(),
                "reviewer_accept" => i.reviewer_accept = v.as_bool().unwrap(),
                "history_events" => i.history_events = n(),
                "history_reverts" => i.history_reverts = n(),
                "delivered_at_ms" => i.delivered_at_ms = v.as_u64(),
                "t_ref_ms" => i.t_ref_ms = n(),
                "contradicts_edges" => i.contradicts_edges = n(),
                "contradicts_open" => i.contradicts_open = n(),
                "inbound_refs" => i.inbound_refs = n(),
                "fresh_test_refs" => i.fresh_test_refs = n(),
                "refs_gone" => i.refs_gone = n(),
                "refs_to_verify" => i.refs_to_verify = n(),
                "public_vault" => i.public_vault = v.as_bool().unwrap(),
                "author_costly_signal" => i.author_costly_signal = v.as_bool().unwrap(),
                "trust_pin" => i.trust_pin = v.as_str().map(str::to_string),
                other => panic!("unknown trust input {other}"),
            }
        }
    }

    #[test]
    fn the_score_matches_the_contract_vectors() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("crates/lean-ctx-protocol/tests/fixtures/hive-v2/vectors/trust.vectors.json");
        let v: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let mut defaults = Inputs::default();
        apply(&mut defaults, &v["defaults"]);
        let cases = v["cases"].as_array().unwrap();
        assert!(cases.len() >= 25);
        for case in cases {
            let mut i = defaults.clone();
            if let Some(set) = case.get("set") {
                apply(&mut i, set);
            }
            assert_eq!(
                score(&i),
                (
                    case["score"].as_i64().unwrap(),
                    case["bucket"].as_str().unwrap()
                ),
                "{}",
                case["name"]
            );
        }
    }
}
