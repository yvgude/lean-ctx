// SPDX-License-Identifier: Apache-2.0
//! Knowledge states (Hive v5 CHECKLIST 2.2.1; C0 2.0
//! `enums/knowledge-states.json`, `vectors/knowledge_state.vectors.json`):
//! `verified | provisional | outdated | conflicting | rejected`, derived from
//! the folded replica on every read, never stored. Hive derives the same
//! state from its projection, so the web and the agent see one answer.

use std::collections::HashSet;

use serde_json::Value;

use crate::core::hive::replica::fold::{Graph, Node};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum State {
    Verified,
    Provisional,
    Outdated,
    Conflicting,
    Rejected,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Provisional => "provisional",
            Self::Outdated => "outdated",
            Self::Conflicting => "conflicting",
            Self::Rejected => "rejected",
        }
    }

    /// May an agent be given the node as current truth?
    pub fn served_as_truth(self) -> bool {
        matches!(self, Self::Verified | Self::Provisional)
    }
}

/// The folded facts the state is derived from (the C0 vector inputs).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inputs<'a> {
    pub retracted: bool,
    pub purged: bool,
    pub review_state: Option<&'a str>,
    pub evidence: Option<&'a str>,
    /// A decision's `status`.
    pub status: Option<&'a str>,
    pub anchor_state: Option<&'a str>,
    /// A live `supersedes` edge from a live node points at this one.
    pub superseded: bool,
    /// A live `contradicts` edge to a live node that is not superseded.
    pub contradicted: bool,
}

/// `(state, reason)`; the reason is `None` only for `verified`. A node
/// without a server review state folds as confirmed, as delivery does.
pub fn of_inputs(i: &Inputs<'_>) -> (State, Option<&'static str>) {
    if i.retracted || i.purged {
        return (State::Rejected, Some("retracted"));
    }
    if i.status == Some("reverted") {
        return (State::Rejected, Some("reverted"));
    }
    if i.superseded || i.status == Some("superseded") {
        return (State::Outdated, Some("superseded"));
    }
    match i.anchor_state {
        Some("gone") => return (State::Outdated, Some("anchor_gone")),
        Some("to_verify") => return (State::Outdated, Some("anchor_changed")),
        _ => {}
    }
    if i.contradicted {
        return (State::Conflicting, Some("contradicted"));
    }
    if i.review_state.is_some_and(|r| r != "confirmed") {
        return (State::Provisional, Some("unconfirmed"));
    }
    if i.evidence != Some("observed") {
        return (State::Provisional, Some("not_observed"));
    }
    (State::Verified, None)
}

fn live(graph: &Graph, id: &str) -> bool {
    graph
        .nodes
        .get(id)
        .is_some_and(|n| !n.retracted && !n.purged)
}

/// The edge-derived inputs of every node, built once per graph (one pass
/// over the edges, so delivery stays linear in the vault size).
#[derive(Debug, Default)]
pub struct Index {
    /// Targets of a live `supersedes` edge from a live node.
    superseded: HashSet<String>,
    /// Ends of a live `contradicts` edge whose other end is live and not
    /// superseded.
    contradicted: HashSet<String>,
}

impl Index {
    pub fn new(graph: &Graph) -> Self {
        let superseded: HashSet<String> = graph
            .edges
            .values()
            .filter(|e| e.kind == "supersedes" && !e.retracted && live(graph, &e.from))
            .map(|e| e.to.clone())
            .collect();
        let open = |id: &str| live(graph, id) && !superseded.contains(id);
        let mut contradicted = HashSet::new();
        for e in graph
            .edges
            .values()
            .filter(|e| e.kind == "contradicts" && !e.retracted)
        {
            if open(&e.to) {
                contradicted.insert(e.from.clone());
            }
            if open(&e.from) {
                contradicted.insert(e.to.clone());
            }
        }
        Self {
            superseded,
            contradicted,
        }
    }

    /// The state of `node_id` (`node` is its folded node).
    pub fn state(&self, node_id: &str, node: &Node) -> (State, Option<&'static str>) {
        let f = |k: &str| node.fields.get(k).and_then(Value::as_str);
        of_inputs(&Inputs {
            retracted: node.retracted,
            purged: node.purged,
            review_state: node.review_state.as_deref(),
            evidence: f("evidence"),
            status: (node.node_type == "decision")
                .then(|| f("status"))
                .flatten(),
            anchor_state: f("anchor_state"),
            superseded: self.superseded.contains(node_id),
            contradicted: self.contradicted.contains(node_id),
        })
    }
}

/// The state of one node of `graph` (builds the index; use [`Index`] for
/// many nodes).
pub fn of(graph: &Graph, node_id: &str, node: &Node) -> (State, Option<&'static str>) {
    Index::new(graph).state(node_id, node)
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value, json};

    use super::*;
    use crate::core::hive::replica::fold::Edge;

    const VECTORS: &str = include_str!(
        "../../../../crates/lean-ctx-protocol/tests/fixtures/hive-v2/vectors/knowledge_state.vectors.json"
    );

    #[test]
    fn the_c0_knowledge_state_vectors_hold() {
        let doc: Value = serde_json::from_str(VECTORS).unwrap();
        let defaults = doc["defaults"].as_object().unwrap();
        let mut seen = std::collections::BTreeSet::new();
        for c in doc["cases"].as_array().unwrap() {
            let name = c["name"].as_str().unwrap();
            let mut m: Map<String, Value> = defaults.clone();
            if let Some(Value::Object(set)) = c.get("set") {
                for (k, v) in set {
                    assert!(m.contains_key(k), "{name}: unknown input {k}");
                    m.insert(k.clone(), v.clone());
                }
            }
            let b = |k: &str| m[k].as_bool().unwrap();
            let (state, reason) = of_inputs(&Inputs {
                retracted: b("retracted"),
                purged: b("purged"),
                review_state: m["review_state"].as_str(),
                evidence: m["evidence"].as_str(),
                status: m["status"].as_str(),
                anchor_state: m["anchor_state"].as_str(),
                superseded: b("superseded"),
                contradicted: b("contradicted"),
            });
            assert_eq!(state.as_str(), c["state"], "{name}");
            assert_eq!(
                reason.map_or(Value::Null, Value::from),
                c["reason"],
                "{name}"
            );
            seen.insert(state);
        }
        assert_eq!(seen.len(), 5, "a case per state");
    }

    fn node(fields: &Value) -> Node {
        Node {
            node_type: "fact".into(),
            fields: fields.as_object().cloned().unwrap(),
            review_state: Some("confirmed".into()),
            ..Node::default()
        }
    }

    fn edge(kind: &str, from: &str, to: &str) -> Edge {
        Edge {
            kind: kind.into(),
            from: from.into(),
            to: to.into(),
            ..Edge::default()
        }
    }

    #[test]
    fn the_graph_inputs_follow_live_edges() {
        let mut g = Graph::default();
        for id in ["n_a", "n_b", "n_c"] {
            g.nodes.insert(
                id.into(),
                node(&json!({"statement": id, "evidence": "observed"})),
            );
        }
        let state = |g: &Graph, id: &str| of(g, id, &g.nodes[id]).0;
        assert_eq!(state(&g, "n_a"), State::Verified);
        g.edges
            .insert("e_1".into(), edge("contradicts", "n_a", "n_b"));
        assert_eq!(state(&g, "n_a"), State::Conflicting);
        assert_eq!(state(&g, "n_b"), State::Conflicting);
        // c supersedes b: b is outdated, the contradiction with it is moot.
        g.edges
            .insert("e_2".into(), edge("supersedes", "n_c", "n_b"));
        assert_eq!(state(&g, "n_b"), State::Outdated);
        assert_eq!(state(&g, "n_a"), State::Verified);
        // A retracted successor supersedes nothing.
        g.nodes.get_mut("n_c").unwrap().retracted = true;
        assert_eq!(state(&g, "n_b"), State::Conflicting);
        // The anchor re-check makes a claim outdated, `fresh` clears it.
        g.nodes
            .get_mut("n_a")
            .unwrap()
            .fields
            .insert("anchor_state".into(), json!("to_verify"));
        assert_eq!(
            of(&g, "n_a", &g.nodes["n_a"]),
            (State::Outdated, Some("anchor_changed"))
        );
        g.nodes
            .get_mut("n_a")
            .unwrap()
            .fields
            .insert("anchor_state".into(), json!("fresh"));
        assert_eq!(state(&g, "n_a"), State::Conflicting);
    }

    /// Retrieval never serves outdated, conflicting or rejected knowledge
    /// as current truth: rejected and superseded leave delivery, the rest
    /// come last with a label.
    #[test]
    fn delivery_marks_or_drops_what_is_not_current_truth() {
        use crate::core::hive::delivery::{Bucket, items, render};
        let mut g = Graph::default();
        let mut add = |id: &str, statement: &str, extra: Value| {
            let mut f = json!({"statement": statement, "evidence": "observed"});
            for (k, v) in extra.as_object().unwrap() {
                f[k] = v.clone();
            }
            let mut n = node(&f);
            n.provenance = Some("workspace".into());
            g.nodes.insert(id.into(), n);
        };
        add("n_ok", "Retries back off with jitter.", json!({}));
        add("n_old", "Retries use a fixed delay.", json!({}));
        add("n_new", "Retries back off with full jitter.", json!({}));
        add(
            "n_moved",
            "The retry cap lives in retry.rs.",
            json!({"anchor_state": "to_verify"}),
        );
        add("n_x", "Retries are unlimited.", json!({}));
        add("n_y", "Retries stop after five tries.", json!({}));
        g.edges
            .insert("e_s".into(), edge("supersedes", "n_new", "n_old"));
        g.edges
            .insert("e_c".into(), edge("contradicts", "n_x", "n_y"));
        let no_trust = |_: &str| -> Option<String> { None };
        let list = items(&g, &no_trust, "");
        let got: Vec<(&str, Bucket)> = list
            .iter()
            .map(|i| (i.node_id.as_str(), i.bucket))
            .collect();
        assert_eq!(
            got,
            vec![
                ("n_new", Bucket::Normal),
                ("n_ok", Bucket::Normal),
                ("n_moved", Bucket::Outdated),
                ("n_x", Bucket::Conflicting),
                ("n_y", Bucket::Conflicting),
            ]
        );
        let out = render("acme/billing", &list, 1_500);
        assert!(
            !out.contains("fixed delay"),
            "superseded is not delivered: {out}"
        );
        assert!(out.contains("[outdated: re-verify] fact"), "{out}");
        assert_eq!(out.matches("[conflicting] fact").count(), 2, "{out}");
        assert_eq!(
            out,
            render("acme/billing", &items(&g, &no_trust, ""), 1_500)
        );
    }
}
