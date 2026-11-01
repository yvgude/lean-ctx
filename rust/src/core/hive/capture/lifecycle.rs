// SPDX-License-Identifier: Apache-2.0
//! The lifecycle of captured knowledge after its `node.add`: what an agent
//! does to a fact later reaches the vault, so other engines never keep
//! receiving knowledge its author has withdrawn.
//!
//! Capture (step 6, [`super::emit`]) records for every node it adds which
//! local fact it came from (`own_sources`: project, category, key, value
//! digest). Each sync compares those own nodes with the project's knowledge
//! store and queues, through the existing event builders:
//!
//! - **superseded**: the fact's value is no longer current and a current
//!   fact of the same category and key has its own node → `edge.add
//!   supersedes` from that node to the old one. The server derives the old
//!   node `outdated (superseded)`; it is not retracted (one or the other).
//! - **withdrawn**: no fact of the store carries the node's value any more
//!   and no successor has a node → `retract` with the fixed reason
//!   `withdrawn by author` (no content).
//! - **re-pinned**: the vault marked the claim `to_verify`/`gone`, while the
//!   local fact with the same value is current, not under review and its
//!   anchors match the code here (an agent re-pinned it) → the claim is
//!   re-anchored at `HEAD` and `anchor_state fresh`, the event `hive
//!   maintain --confirm` writes.
//!
//! The comparison reads the raw store, not a policy view: a fact a view
//! withholds still exists and is never retracted for that. A store that
//! cannot be read, or holds no facts while own nodes exist, plans nothing.
//! Only `fact` sources of this checkout count; another checkout of the same
//! vault owns its nodes. The server's retraction limits apply unchanged: a
//! held retraction stays pending, the engine sets no threshold of its own.
//!
//! Every action is idempotent: an edge or retraction already in the folded
//! place or still pending is not queued again, so a sync that ran before
//! changes nothing.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::refs::Checkout;
use crate::core::hive::HiveError;
use crate::core::hive::events::build;
use crate::core::hive::events::hlc::{self, Stamp};
use crate::core::hive::replica::Replica;
use crate::core::hive::replica::fold::Graph;
use crate::core::hive::review::{curate, maintain};

/// The reason of a lifecycle retraction: stable, and no fact content.
pub const WITHDRAWN_REASON: &str = "withdrawn by author";

/// An own node and the fact it was captured from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnNode {
    pub node_id: String,
    pub category: String,
    pub key: String,
    pub value_digest: String,
}

/// A fact of the raw store, as far as the lifecycle needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreFact {
    pub category: String,
    pub key: String,
    pub value_digest: String,
    /// Not superseded and not expired (`valid_until` unset).
    pub current: bool,
    /// Current, not under review, and every anchor matches this checkout.
    pub confirmed_here: bool,
}

/// What the vault shows of a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeView {
    pub live: bool,
    /// `anchor_state` is `to_verify` or `gone`.
    pub marked: bool,
    pub has_anchors: bool,
}

/// One lifecycle action, in the order it is queued.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Action {
    Supersede { old: String, new: String },
    Retract { node: String },
    Confirm { node: String },
}

/// What is already in the place or pending, so nothing is queued twice.
#[derive(Debug, Default)]
pub struct Present {
    /// `(from, to)` of live or pending `supersedes` edges.
    pub supersedes: BTreeSet<(String, String)>,
    /// Targets of pending `retract` events.
    pub retracting: BTreeSet<String>,
    /// Targets of pending `field.set anchors` events.
    pub confirming: BTreeSet<String>,
    /// Nodes queued by a pending `node.add` (this sync's captures): live for
    /// the plan, so a successor captured in the same run supersedes its
    /// predecessor instead of the predecessor being retracted.
    pub adding: BTreeSet<String>,
}

impl Present {
    /// [`node_view`] in `graph`, or a pending addition as a live node.
    pub fn view(&self, graph: &Graph, node_id: &str) -> Option<NodeView> {
        node_view(graph, node_id).or_else(|| {
            self.adding.contains(node_id).then_some(NodeView {
                live: true,
                marked: false,
                has_anchors: false,
            })
        })
    }
}

/// The `(category, key)` of every fact the knowledge journal of the
/// checkout at `root` records as removed.
pub fn removed_identities(root: &str) -> BTreeSet<(String, String)> {
    let Ok(dir) = crate::core::knowledge::knowledge_dir_of(root) else {
        return BTreeSet::new();
    };
    crate::core::knowledge::journal::rows_since(&dir, 0, usize::MAX)
        .into_iter()
        .filter(|r| r.op == "removed")
        .map(|r| (r.category, r.key))
        .collect()
}

/// The actions that bring the vault in line with the store, in node id
/// order (deterministic).
///
/// `removed` are the `(category, key)` identities the knowledge journal
/// records as removed. An empty store withdraws only those: a store that
/// was reset or could not be read never withdraws everything.
pub fn plan(
    own: &[OwnNode],
    store: &[StoreFact],
    removed: &BTreeSet<(String, String)>,
    node: impl Fn(&str) -> Option<NodeView>,
    present: &Present,
) -> Vec<Action> {
    if store.is_empty() && removed.is_empty() {
        return Vec::new();
    }
    let mut by_identity: BTreeMap<(&str, &str), Vec<&StoreFact>> = BTreeMap::new();
    for fact in store {
        by_identity
            .entry((fact.category.as_str(), fact.key.as_str()))
            .or_default()
            .push(fact);
    }
    // The own live node of each (category, key, value digest).
    let mut node_of: BTreeMap<(&str, &str, &str), &str> = BTreeMap::new();
    for n in own {
        if node(&n.node_id).is_some_and(|v| v.live) {
            node_of.insert(
                (n.category.as_str(), n.key.as_str(), n.value_digest.as_str()),
                n.node_id.as_str(),
            );
        }
    }
    let mut actions = Vec::new();
    for n in own {
        let Some(view) = node(&n.node_id).filter(|v| v.live) else {
            continue;
        };
        let facts = by_identity
            .get(&(n.category.as_str(), n.key.as_str()))
            .map(Vec::as_slice)
            .unwrap_or_default();
        let same = facts.iter().find(|f| f.value_digest == n.value_digest);
        if let Some(fact) = same.filter(|f| f.current) {
            if view.marked
                && view.has_anchors
                && fact.confirmed_here
                && !present.confirming.contains(&n.node_id)
            {
                actions.push(Action::Confirm {
                    node: n.node_id.clone(),
                });
            }
            continue;
        }
        // The node's value is not current any more (superseded, expired or
        // removed). A current successor with its own node supersedes it.
        let successor = facts
            .iter()
            .filter(|f| f.current && f.value_digest != n.value_digest)
            .find_map(|f| {
                node_of.get(&(f.category.as_str(), f.key.as_str(), f.value_digest.as_str()))
            });
        if let Some(&new) = successor {
            if !present
                .supersedes
                .contains(&(new.to_owned(), n.node_id.clone()))
            {
                actions.push(Action::Supersede {
                    old: n.node_id.clone(),
                    new: new.to_owned(),
                });
            }
        } else {
            let journaled = removed.contains(&(n.category.clone(), n.key.clone()));
            if (store.is_empty() && !journaled) || present.retracting.contains(&n.node_id) {
                continue;
            }
            actions.push(Action::Retract {
                node: n.node_id.clone(),
            });
        }
    }
    actions.sort();
    actions.dedup();
    actions
}

/// The own `fact` nodes of the checkout `project_hash`, from `own_sources`.
pub fn own_nodes(replica: &Replica, project_hash: &str) -> Result<Vec<OwnNode>, HiveError> {
    Ok(replica
        .own_sources()?
        .into_iter()
        .filter(|(_, kind, _)| kind == "fact")
        .filter_map(|(node_id, _, source)| {
            if source.get("project_hash")?.as_str()? != project_hash {
                return None;
            }
            Some(OwnNode {
                node_id,
                category: source.get("category")?.as_str()?.to_owned(),
                key: source.get("key")?.as_str()?.to_owned(),
                value_digest: source.get("value_digest")?.as_str()?.to_owned(),
            })
        })
        .collect())
}

/// The vault's view of `node_id` in `graph`.
pub fn node_view(graph: &Graph, node_id: &str) -> Option<NodeView> {
    let node = graph.nodes.get(node_id)?;
    Some(NodeView {
        live: !node.retracted && !node.purged,
        marked: matches!(
            node.fields.get("anchor_state").and_then(Value::as_str),
            Some("to_verify" | "gone")
        ),
        has_anchors: node
            .fields
            .get("anchors")
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty()),
    })
}

/// What the folded place and the pending queue already hold.
pub fn present(graph: &Graph, replica: &Replica) -> Result<Present, HiveError> {
    let mut out = Present::default();
    for edge in graph.edges.values() {
        if edge.kind == "supersedes" && !edge.retracted {
            out.supersedes.insert((edge.from.clone(), edge.to.clone()));
        }
    }
    for (_, bytes) in replica.pending()? {
        let Ok(event) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        let target = event["target"].as_str().unwrap_or_default().to_owned();
        match event["op"].as_str() {
            Some("retract") => {
                out.retracting.insert(target);
            }
            Some("field.set") if event["body"]["field"] == "anchors" => {
                out.confirming.insert(target);
            }
            Some("node.add") => {
                out.adding.insert(target);
            }
            Some("edge.add") if event["body"]["kind"] == "supersedes" => {
                if let (Some(from), Some(to)) =
                    (event["body"]["from"].as_str(), event["body"]["to"].as_str())
                {
                    out.supersedes.insert((from.to_owned(), to.to_owned()));
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// What [`queue`] queued.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Queued {
    pub superseded: usize,
    pub retracted: usize,
    pub confirmed: usize,
    pub clock: Stamp,
}

/// Queue the signed events of `actions` as pending (the sync push sends
/// them), continuing `clock`. A confirm whose anchored code is gone is
/// skipped: a gone claim is withdrawn, not re-anchored.
pub fn queue(
    target: &maintain::Target<'_>,
    graph: &Graph,
    checkout: &mut Checkout,
    actions: &[Action],
    clock: Stamp,
    now_ms: u64,
) -> Result<Queued, HiveError> {
    let mut out = Queued {
        clock,
        ..Queued::default()
    };
    for action in actions {
        match action {
            Action::Supersede { old, new } => {
                out.clock = hlc::send(out.clock, now_ms);
                let event = build::edge_add(
                    "supersedes",
                    new,
                    old,
                    "observed",
                    target.place,
                    out.clock,
                    target.signer,
                )?;
                maintain::queue(target, event, out.clock)?;
                out.superseded += 1;
            }
            Action::Retract { node } => {
                out.clock = hlc::send(out.clock, now_ms);
                let event = curate::retract_event(
                    target.place,
                    node,
                    WITHDRAWN_REASON,
                    out.clock,
                    target.signer,
                )?;
                maintain::queue(target, event, out.clock)?;
                out.retracted += 1;
            }
            Action::Confirm { node } => {
                let Some(anchors) = graph
                    .nodes
                    .get(node)
                    .and_then(|n| n.fields.get("anchors"))
                    .and_then(Value::as_array)
                else {
                    continue;
                };
                let Some(fresh) = maintain::reanchored(anchors, checkout, now_ms) else {
                    continue;
                };
                out.clock = hlc::send(out.clock, now_ms);
                let event = maintain::field_set(
                    target.place,
                    node,
                    "anchors",
                    Value::Array(fresh),
                    Some(now_ms),
                    out.clock,
                    target.signer,
                )?;
                maintain::queue(target, event, out.clock)?;
                out.clock = hlc::send(out.clock, now_ms);
                let event = maintain::field_set(
                    target.place,
                    node,
                    "anchor_state",
                    Value::from("fresh"),
                    None,
                    out.clock,
                    target.signer,
                )?;
                maintain::queue(target, event, out.clock)?;
                out.confirmed += 1;
            }
        }
    }
    Ok(out)
}

/// The raw store's facts of `project_root` for [`plan`], with value digests
/// keyed like `own_sources` (`replica.value_digest`). `None` when the store
/// cannot be read.
pub fn store_facts(replica: &Replica, project_root: &str) -> Option<Vec<StoreFact>> {
    let knowledge =
        crate::core::knowledge::ProjectKnowledge::load_for_checked_capture(project_root).ok()?;
    let root = std::path::Path::new(project_root);
    Some(
        knowledge
            .facts
            .iter()
            .map(|fact| {
                let current = fact.valid_until.is_none();
                let anchors_here = !fact.anchors.is_empty()
                    && crate::core::knowledge::anchors::review(root, &fact.anchors)
                        .iter()
                        .all(|(_, state)| {
                            *state == crate::core::knowledge::anchors::AnchorState::Current
                        });
                StoreFact {
                    category: fact.category.clone(),
                    key: fact.key.clone(),
                    value_digest: replica.value_digest(&fact.value),
                    current,
                    confirmed_here: current && fact.review.is_none() && anchors_here,
                }
            })
            .collect(),
    )
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod tests;
