// SPDX-License-Identifier: Apache-2.0
//! Provenance of captured claims (Hive v5 CHECKLIST 2.2.2, 2.6.1): a claim
//! that names a vault node itself (`node:<id>` reference) builds on it, and
//! its `node.add` is followed by a `derived_from` edge to that node. The
//! edge is ordinary vault content: signed, queued with the claim (offline
//! too) and pushed by the next sync.
//!
//! Only an explicit reference counts. The server counts verified
//! cross-engine reuse from these edges, and only where the claim's own body
//! carries the `node:` reference, so a guessed link (shared words with
//! something delivered earlier) is no provenance and is never written.
//!
//! Not telemetry: there are no counters, no usage pings and no local
//! delivery log; only the edges reach the vault, as the claim's provenance.

use super::CapturedFact;
use crate::core::hive::replica::fold::Graph;

/// Fill each claim's `derived_from` with the live nodes it names itself
/// (`node:<id>` references), once each, in reference order.
pub fn link(facts: &mut [CapturedFact], graph: &Graph) {
    let live = |id: &str| {
        graph
            .nodes
            .get(id)
            .is_some_and(|n| !n.retracted && !n.purged)
    };
    for f in facts.iter_mut() {
        let mut named: Vec<String> = Vec::new();
        for id in f.refs.iter().filter_map(|r| r.strip_prefix("node:")) {
            if live(id) && !named.iter().any(|n| n == id) {
                named.push(id.to_string());
            }
        }
        f.derived_from = named;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::core::hive::capture::CaptureCandidate;
    use crate::core::hive::replica::fold::Node;
    use crate::core::knowledge::{EvidenceLevel, NodeType};
    use crate::core::sensitivity::SensitivityLevel;

    fn node(statement: &str) -> Node {
        Node {
            node_type: "fact".into(),
            fields: json!({"statement": statement, "applies_to": []})
                .as_object()
                .unwrap()
                .clone(),
            ..Node::default()
        }
    }

    fn claim(statement: &str, refs: &[&str]) -> CapturedFact {
        CapturedFact {
            candidate: CaptureCandidate {
                node_type: NodeType::Fact,
                statement: statement.into(),
                steps: vec![],
                commands: vec![],
                refs: vec![],
                evidence: EvidenceLevel::Observed,
                sensitivity: SensitivityLevel::Public,
            },
            refs: refs.iter().map(ToString::to_string).collect(),
            category: "arch".into(),
            key: "k".into(),
            value: statement.into(),
            derived_from: vec![],
        }
    }

    #[test]
    fn only_nodes_a_claim_names_itself_become_provenance() {
        let mut g = Graph::default();
        g.nodes.insert(
            "n_lock".into(),
            node("Ledger close takes the advisory lock 42 before it reads balances."),
        );
        g.nodes.insert(
            "n_retry".into(),
            node("Payment retries back off exponentially, capped at 64 seconds."),
        );
        let mut gone = node("Ledger balances are read without any advisory lock.");
        gone.retracted = true;
        g.nodes.insert("n_gone".into(), gone);
        let mut facts = vec![
            // Shares most words with n_lock but names nothing: no link (a
            // guess is no provenance, v5 2.6.1).
            claim(
                "The nightly export waits for advisory lock 42 so the ledger close reads stable balances.",
                &["file:src/ledger.rs"],
            ),
            // Names two live nodes (one twice), a retracted and an unknown
            // one: the live ones, once each, in reference order.
            claim(
                "The nightly export waits for the ledger lock and retries with backoff.",
                &[
                    "node:n_retry",
                    "node:n_gone",
                    "node:n_lock",
                    "node:n_retry",
                    "node:n_unknown",
                ],
            ),
        ];
        link(&mut facts, &g);
        assert!(facts[0].derived_from.is_empty());
        assert_eq!(facts[1].derived_from, ["n_retry", "n_lock"]);
        // Deterministic: the same claims and graph give the same links.
        let mut again = facts.clone();
        link(&mut again, &g);
        assert_eq!(again, facts);
    }
}
