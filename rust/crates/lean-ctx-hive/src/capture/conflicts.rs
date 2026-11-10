// SPDX-License-Identifier: Apache-2.0
//! Concurrent edits of one shared fact across Instances (#286).
//!
//! A claim's node id comes from its text, so two Instances that give the
//! same fact (`category`, `key`) different values write two unlinked nodes.
//! Capture therefore gives every shared `fact` node a subject reference in
//! its `applies_to`: `subject:<id>`, a digest of (`category`, `key`) keyed
//! with the place id key `pk_id` (like the dedupe key: two engines of a
//! place reach the same id, nobody without the key can confirm a guessed
//! key, and no local key text leaves the machine).
//!
//! Every sync then folds the place and groups the live, not superseded
//! fact nodes by subject. Two nodes of one subject are linked, and which
//! link follows from the save history: each node's first `seq` in the log
//! and the `causal_base` of that save (how far its writer had folded when
//! it wrote the fact: the sync stamps fact saves with the position before
//! its own pull, so a value pulled in the same sync never counts as seen).
//!
//! * **Sequential edit**: the later save's writer had already folded the
//!   earlier one (`causal_base >= seq` of the earlier). The later value
//!   replaces it: its author queues `edge.add supersedes` from the newer
//!   node to the older one, so the older value is served as outdated and
//!   the newer as current.
//! * **Concurrent edit**: neither writer had seen the other's save, or the
//!   history of either is unknown. That is a conflict: one `edge.add
//!   contradicts` between them, and delivery serves both as
//!   `[conflicting]` on every Instance.
//!
//! The resolution is deterministic, the same on every Instance that folded
//! the same log: the edge id is a keyed digest of its kind and its pair, so
//! a second write names the same edge (the place refuses it, nothing
//! doubles), and exactly one Instance writes it (the replica's
//! `own_sources` know the authors): the newer node's author a
//! `supersedes`, the larger node's author a `contradicts`, which runs `from`
//! the larger node id `to` the smaller one. Others never race it.
//!
//! A node superseded in the place (or by a queued `supersedes`) is no
//! longer linked, and two nodes of one subject from the same checkout are
//! left to the capture lifecycle.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::HiveError;
use crate::events::hlc::{self, Stamp};
use crate::events::ids;
use crate::replica::Replica;
use crate::replica::fold::{Graph, Node};
use crate::replica::log::Save;
use crate::review::maintain::{Target, queue};

/// Prefix of the subject reference in a fact's `applies_to`.
pub const SUBJECT_PREFIX: &str = "subject:";
/// Domain of the subject digest.
const SUBJECT_DOMAIN: &[u8] = b"leanctx.hive.subject.v1";
/// Domain of a conflict edge id.
const EDGE_DOMAIN: &[u8] = b"leanctx.hive.contradicts.v1";
/// Domain of a sequential edit's `supersedes` edge id.
const SUPERSEDES_DOMAIN: &[u8] = b"leanctx.hive.supersedes.v1";
/// The most conflict edges one sync queues (the rest follow next sync).
pub const MAX_PER_SYNC: usize = 32;

/// The subject reference of the fact (`category`, `key`) in a place with
/// id key `pk_id`: `subject:` + 26 base32 characters.
pub fn subject_ref(pk_id: &[u8; 32], category: &str, key: &str) -> String {
    let mut h = blake3::Hasher::new_keyed(pk_id);
    h.update(SUBJECT_DOMAIN);
    h.update(&[0]);
    h.update(category.as_bytes());
    h.update(&[0]);
    h.update(key.as_bytes());
    format!(
        "{SUBJECT_PREFIX}{}",
        &ids::base32_lower(h.finalize().as_bytes())[..26]
    )
}

/// The subject reference a folded node carries, if any.
pub fn subject_of(node: &Node) -> Option<&str> {
    node.fields
        .get("applies_to")?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .find(|a| a.starts_with(SUBJECT_PREFIX))
}

/// What a link between two values of one fact says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// Concurrent values: both are served as conflicting.
    Contradicts,
    /// A sequential edit: `from` (newer) replaces `to` (older).
    Supersedes,
}

impl Kind {
    /// The edge kind in the event body.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Contradicts => "contradicts",
            Self::Supersedes => "supersedes",
        }
    }
}

/// One link edge to write.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Link {
    pub edge_id: String,
    pub kind: Kind,
    /// `contradicts`: the larger node id of the pair; `supersedes`: the
    /// newer node.
    pub from: String,
    pub to: String,
}

fn link_id(pk_id: &[u8; 32], domain: &[u8], from: &str, to: &str) -> String {
    let mut h = blake3::Hasher::new_keyed(pk_id);
    h.update(domain);
    h.update(&[0]);
    h.update(from.as_bytes());
    h.update(&[0]);
    h.update(to.as_bytes());
    let bytes: [u8; 16] = h.finalize().as_bytes()[..16].try_into().expect("16 bytes");
    ids::random_id("e", &bytes)
}

/// The deterministic conflict edge between `a` and `b` in a place with id
/// key `pk_id`.
pub fn contradiction(pk_id: &[u8; 32], a: &str, b: &str) -> Link {
    let (from, to) = if a > b { (a, b) } else { (b, a) };
    Link {
        edge_id: link_id(pk_id, EDGE_DOMAIN, from, to),
        kind: Kind::Contradicts,
        from: from.to_owned(),
        to: to.to_owned(),
    }
}

/// The deterministic `supersedes` edge from the newer value `new` to the
/// older value `old` in a place with id key `pk_id`.
pub fn supersession(pk_id: &[u8; 32], new: &str, old: &str) -> Link {
    Link {
        edge_id: link_id(pk_id, SUPERSEDES_DOMAIN, new, old),
        kind: Kind::Supersedes,
        from: new.to_owned(),
        to: old.to_owned(),
    }
}

/// How two values of one fact relate, from their save history: `Some((new,
/// old))` when the later save's writer had folded the earlier one, `None`
/// for concurrent values or an unknown history (a conflict).
fn sequential<'a>(
    saves: &BTreeMap<String, Save>,
    a: &'a str,
    b: &'a str,
) -> Option<(&'a str, &'a str)> {
    let (sa, sb) = (saves.get(a)?, saves.get(b)?);
    let ((new, s_new), (old, s_old)) = if sa.seq > sb.seq {
        ((a, sa), (b, sb))
    } else {
        ((b, sb), (a, sa))
    };
    (s_new.seq != s_old.seq && s_new.causal_base >= s_old.seq).then_some((new, old))
}

/// A fact node this Instance authored (`own_sources`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Own {
    /// The checkout that captured it.
    pub project_hash: String,
    /// Its subject, from the source's category and key (nodes captured
    /// before subjects were written carry none in their body).
    pub subject: Option<String>,
}

/// What [`plan`] reads besides the folded graph.
#[derive(Debug, Default)]
pub struct Inputs {
    /// This Instance's own fact nodes.
    pub own: BTreeMap<String, Own>,
    /// Targets of `edge.add` events queued and not yet folded.
    pub queued_edges: BTreeSet<String>,
    /// Nodes a queued `supersedes` edge supersedes.
    pub queued_superseded: BTreeSet<String>,
    /// Each node's first save in the log ([`Replica::first_saves`]); a node
    /// without one has an unknown history.
    pub saves: BTreeMap<String, Save>,
}

/// The link edges this Instance writes for `graph`, in a stable order
/// (at most [`MAX_PER_SYNC`]).
pub fn plan(graph: &Graph, pk_id: &[u8; 32], inputs: &Inputs) -> Vec<Link> {
    let live = |id: &str| {
        graph
            .nodes
            .get(id)
            .is_some_and(|n| !n.retracted && !n.purged)
    };
    let mut superseded: BTreeSet<&str> = graph
        .edges
        .values()
        .filter(|e| e.kind == "supersedes" && !e.retracted && live(&e.from))
        .map(|e| e.to.as_str())
        .collect();
    superseded.extend(inputs.queued_superseded.iter().map(String::as_str));
    let marked: BTreeSet<(&str, &str)> = graph
        .edges
        .values()
        .filter(|e| e.kind == "contradicts" && !e.retracted)
        .flat_map(|e| {
            [
                (e.from.as_str(), e.to.as_str()),
                (e.to.as_str(), e.from.as_str()),
            ]
        })
        .collect();
    let mut by_subject: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (id, node) in graph.live_nodes() {
        if node.node_type != "fact" || superseded.contains(id.as_str()) {
            continue;
        }
        let subject =
            subject_of(node).or_else(|| inputs.own.get(id).and_then(|o| o.subject.as_deref()));
        if let Some(subject) = subject {
            by_subject.entry(subject).or_default().push(id);
        }
    }
    let mut out = Vec::new();
    for ids in by_subject.values().filter(|ids| ids.len() > 1) {
        // `live_nodes` iterates in id order: `ids` is sorted.
        for (i, to) in ids.iter().enumerate() {
            for from in &ids[i + 1..] {
                if marked.contains(&(*from, *to)) {
                    continue;
                }
                // Two values from one checkout: the lifecycle supersedes.
                if let (Some(a), Some(b)) = (inputs.own.get(*from), inputs.own.get(*to))
                    && a.project_hash == b.project_hash
                {
                    continue;
                }
                // A sequential edit: the newer node's author supersedes;
                // otherwise the larger node's author marks the conflict.
                let (writer, link) = match sequential(&inputs.saves, from, to) {
                    Some((new, old)) => (new, supersession(pk_id, new, old)),
                    None => (*from, contradiction(pk_id, from, to)),
                };
                if inputs.own.contains_key(writer) && !inputs.queued_edges.contains(&link.edge_id) {
                    out.push(link);
                }
            }
        }
    }
    out.sort();
    out.truncate(MAX_PER_SYNC);
    out
}

/// The links between the values of one fact that every Instance derives on
/// read, from the folded graph and the save history alone: the same
/// resolution as [`plan`], before (or without) its edge reaching this
/// replica. Without it, an Instance that pulled both values of a fact
/// served them as two plain claims until the one Instance the resolution
/// names had written its edge and this one had pulled it (#286).
///
/// A node is grouped by the subject in its body (nodes captured before
/// subjects are left to their author's edge). Sequential edits come
/// first: the older value is superseded, then the open values left in a
/// group contradict each other pairwise. Pairs a folded `contradicts` or
/// `supersedes` edge already links, retracted or not, are skipped first,
/// as in [`plan`]. Returns `(kind, from, to)` in a stable order.
pub fn derived(graph: &Graph, saves: &BTreeMap<String, Save>) -> Vec<(Kind, String, String)> {
    let live = |id: &str| {
        graph
            .nodes
            .get(id)
            .is_some_and(|n| !n.retracted && !n.purged)
    };
    let superseded: BTreeSet<&str> = graph
        .edges
        .values()
        .filter(|e| e.kind == "supersedes" && !e.retracted && live(&e.from))
        .map(|e| e.to.as_str())
        .collect();
    // A pair any folded link edge joins is decided, either way round and
    // even when retracted: a curator's retraction is a decision too, and
    // deriving the link again would undo it.
    let decided: BTreeSet<(&str, &str)> = graph
        .edges
        .values()
        .filter(|e| e.kind == "contradicts" || e.kind == "supersedes")
        .flat_map(|e| {
            [
                (e.from.as_str(), e.to.as_str()),
                (e.to.as_str(), e.from.as_str()),
            ]
        })
        .collect();
    let mut by_subject: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (id, node) in graph.live_nodes() {
        if node.node_type != "fact" || superseded.contains(id.as_str()) {
            continue;
        }
        if let Some(subject) = subject_of(node) {
            by_subject.entry(subject).or_default().push(id);
        }
    }
    let mut out = Vec::new();
    for ids in by_subject.values().filter(|ids| ids.len() > 1) {
        // Same order as `plan`: a decided pair is skipped first, then a
        // sequential edit supersedes, otherwise the pair contradicts.
        let mut old: BTreeSet<&str> = BTreeSet::new();
        let mut open: Vec<(&str, &str)> = Vec::new();
        for (i, to) in ids.iter().enumerate() {
            for from in &ids[i + 1..] {
                if decided.contains(&(*from, *to)) {
                    continue;
                }
                match sequential(saves, from, to) {
                    Some((new, older)) => {
                        old.insert(older);
                        out.push((Kind::Supersedes, new.to_owned(), older.to_owned()));
                    }
                    None => open.push((*from, *to)),
                }
            }
        }
        // A value superseded here no longer conflicts.
        out.extend(
            open.into_iter()
                .filter(|(from, to)| !old.contains(from) && !old.contains(to))
                .map(|(from, to)| (Kind::Contradicts, from.to_owned(), to.to_owned())),
        );
    }
    out.sort();
    out
}

/// [`Inputs`] from the replica: own fact sources, the save history and
/// the pending queue.
pub fn inputs(replica: &Replica, pk_id: &[u8; 32]) -> Result<Inputs, HiveError> {
    let mut inputs = Inputs {
        saves: replica.first_saves()?,
        ..Inputs::default()
    };
    for (node_id, kind, source) in replica.own_sources()? {
        if kind != "fact" {
            continue;
        }
        let text = |k: &str| source.get(k).and_then(Value::as_str);
        let subject = match (text("category"), text("key")) {
            (Some(category), Some(key)) => Some(subject_ref(pk_id, category, key)),
            _ => None,
        };
        inputs.own.insert(
            node_id,
            Own {
                project_hash: text("project_hash").unwrap_or_default().to_owned(),
                subject,
            },
        );
    }
    for (_, bytes) in replica.pending()? {
        let Ok(event) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        if event["op"] != "edge.add" {
            continue;
        }
        if let Some(target) = event["target"].as_str() {
            inputs.queued_edges.insert(target.to_owned());
        }
        if event["body"]["kind"] == "supersedes"
            && let Some(to) = event["body"]["to"].as_str()
        {
            inputs.queued_superseded.insert(to.to_owned());
        }
    }
    Ok(inputs)
}

/// Queue the signed link edges of `planned` (the sync push sends them),
/// continuing `clock`. Returns how many it queued and the clock.
pub fn queue_all(
    target: &Target<'_>,
    planned: &[Link],
    clock: Stamp,
    now_ms: u64,
) -> Result<(usize, Stamp), HiveError> {
    let mut clock = clock;
    for c in planned {
        clock = hlc::send(clock, now_ms);
        let event = match c.kind {
            Kind::Contradicts => crate::review::curate::contradicts_edge_at(
                target.place,
                &c.edge_id,
                &c.from,
                &c.to,
                clock,
                target.signer,
            )?,
            // Written like the capture lifecycle's supersedes: by the
            // newer value's author.
            Kind::Supersedes => crate::events::build::edge_add_at(
                &c.edge_id,
                c.kind.as_str(),
                &c.from,
                &c.to,
                "observed",
                target.place,
                clock,
                target.signer,
            )?,
        };
        queue(target, &event, clock)?;
    }
    Ok((planned.len(), clock))
}

/// Fold the replica, plan and queue this Instance's conflict edges.
pub fn detect(
    target: &Target<'_>,
    pk_id: &[u8; 32],
    clock: Stamp,
    now_ms: u64,
) -> Result<(usize, Stamp), HiveError> {
    let mut graph = Graph::default();
    graph
        .fold(&target.replica.fold_events()?)
        .map_err(|e| HiveError::Protocol(e.to_string()))?;
    let planned = plan(&graph, pk_id, &inputs(target.replica, pk_id)?);
    queue_all(target, &planned, clock, now_ms)
}

#[cfg(test)]
#[path = "conflicts_tests.rs"]
mod tests;
