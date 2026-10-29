// SPDX-License-Identifier: Apache-2.0
//! Health figures of a sealed place, computed locally (SPEC-ENGINE §3.5.5
//! "Health"): the server cannot read sealed bodies, so it reports `null`
//! for the four percentages; the engine derives them from its folded
//! replica with the CONTRACT §3.25 definitions. They are never sent.

use std::collections::BTreeMap;

use serde_json::Value;

use super::trust;
use crate::core::hive::replica::fold::Graph;

/// Node types the figures count (claim types + procedures).
const COUNTED: [&str; 5] = ["fact", "decision", "gotcha", "finding", "procedure"];

/// The §3.25 figures over active nodes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Health {
    pub active: usize,
    pub evidenced: usize,
    pub stale: usize,
    pub contradicted: usize,
    pub unconfirmed: usize,
    /// Local trust bucket → count.
    pub buckets: BTreeMap<&'static str, usize>,
}

/// Integer percent of `part` in `whole`, halves up; `None` without nodes.
pub fn percent(part: usize, whole: usize) -> Option<usize> {
    (whole > 0).then(|| (part * 200 + whole) / (2 * whole))
}

/// The figures of `graph`; a pure function of the folded log (#498).
pub fn of(graph: &Graph) -> Health {
    let mut h = Health::default();
    for (id, node) in graph.live_nodes() {
        if !COUNTED.contains(&node.node_type.as_str()) {
            continue;
        }
        let inputs = trust::inputs(graph, id, node);
        h.active += 1;
        h.evidenced +=
            usize::from(node.fields.get("evidence").and_then(Value::as_str) == Some("observed"));
        h.stale += usize::from(inputs.refs_gone + inputs.refs_to_verify > 0);
        h.contradicted += usize::from(inputs.contradicts_open > 0);
        h.unconfirmed += usize::from(
            node.review_state
                .as_deref()
                .is_some_and(|s| s != "confirmed"),
        );
        *h.buckets.entry(trust::score(&inputs).1).or_default() += 1;
    }
    h
}

impl Health {
    /// `Evidenced 50% · Stale 0% · Contradicted 25% · Unconfirmed 0% · trust high 1, normal 3`.
    pub fn render(&self) -> String {
        let p = |n| percent(n, self.active).map_or("–".to_string(), |v| format!("{v}%"));
        let buckets: Vec<String> = self
            .buckets
            .iter()
            .map(|(b, n)| format!("{b} {n}"))
            .collect();
        format!(
            "{} nodes · Evidenced {} · Stale {} · Contradicted {} · Unconfirmed {} · trust {}",
            self.active,
            p(self.evidenced),
            p(self.stale),
            p(self.contradicted),
            p(self.unconfirmed),
            if buckets.is_empty() {
                "–".to_string()
            } else {
                buckets.join(", ")
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::core::hive::replica::fold::FoldEvent;

    fn add(seq: u64, id: &str, node_type: &str, body: Value, review: &str) -> FoldEvent {
        FoldEvent {
            seq,
            event_id: format!("E{seq:025}"),
            op: "node.add".into(),
            target: Some(id.into()),
            node_type: Some(node_type.into()),
            body: Some(body),
            sig_invalid: false,
            provenance: Some("workspace".into()),
            review_state: Some(review.into()),
        }
    }

    #[test]
    fn figures_follow_the_contract_definitions() {
        let mut g = Graph::default();
        g.fold(&[
            add(
                1,
                "n_a",
                "fact",
                json!({"statement": "a", "evidence": "observed"}),
                "confirmed",
            ),
            add(2, "n_b", "fact", json!({"statement": "b"}), "unconfirmed"),
            add(3, "n_c", "gotcha", json!({"statement": "c"}), "confirmed"),
            add(
                4,
                "n_d",
                "episode",
                json!({"summary": "not counted"}),
                "confirmed",
            ),
        ])
        .unwrap();
        let h = of(&g);
        assert_eq!(h.active, 3);
        assert_eq!(
            (h.evidenced, h.unconfirmed, h.stale, h.contradicted),
            (1, 1, 0, 0)
        );
        assert_eq!(percent(h.evidenced, h.active), Some(33));
        assert_eq!(percent(1, 2), Some(50));
        assert_eq!(percent(0, 0), None);
        assert_eq!(h.buckets.values().sum::<usize>(), 3);
        assert_eq!(of(&g), h, "deterministic");
        assert!(h.render().starts_with("3 nodes · Evidenced 33% · Stale 0%"));
        assert_eq!(
            Health::default().render(),
            "0 nodes · Evidenced – · Stale – · Contradicted – · Unconfirmed – · trust –"
        );
    }
}
