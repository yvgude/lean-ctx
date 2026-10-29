// SPDX-License-Identifier: Apache-2.0
//! Delivery of a place's knowledge into an agent's context (SPEC-ENGINE
//! §3.5): which folded nodes a host session may see, in which bucket, and a
//! deterministic rendering (#498) — the same replica and query give the
//! same bytes, whatever the clock, the scores or the trust revision say.

use serde_json::Value;

use crate::core::hive::replica::fold::{Graph, Node};
use crate::core::hive::untrusted::{UntrustedKind, wrap};

pub mod decide;
pub mod health;
pub mod knowledge_state;
pub mod trust;

/// Bucket a delivered node is shown in (§3.5.2), in delivery order. The
/// last two hold knowledge that is not current truth (v5 2.2.1): it is
/// delivered after everything else and labelled, never as a plain item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Bucket {
    High,
    Normal,
    Unconfirmed,
    /// An anchor changed or is gone: re-verify before relying on it.
    Outdated,
    /// An open contradiction with another live claim.
    Conflicting,
}

impl Bucket {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Normal => "normal",
            Self::Unconfirmed => "unconfirmed",
            Self::Outdated => "outdated",
            Self::Conflicting => "conflicting",
        }
    }
}

/// Node type order inside a bucket (§3.5.3).
fn type_rank(node_type: &str) -> u8 {
    match node_type {
        "gotcha" => 0,
        "decision" => 1,
        "procedure" => 2,
        "fact" => 3,
        "finding" => 4,
        "episode" => 5,
        _ => 6,
    }
}

/// One deliverable node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub node_id: String,
    pub node_type: String,
    pub bucket: Bucket,
    /// `low` trust sorts last within `normal`.
    pub low: bool,
    pub own: bool,
    pub text: String,
    /// Relevance tier 0 (best) – 3 for the query.
    pub tier: u8,
}

/// The text a node is delivered as: the statement, a procedure's title
/// with numbered steps, an episode's summary. Never truncated (R70).
fn text_of(node: &Node) -> Option<String> {
    let f = |k: &str| node.fields.get(k).and_then(Value::as_str);
    match node.node_type.as_str() {
        "procedure" => {
            let mut t = f("title")?.to_string();
            if let Some(steps) = node.fields.get("steps").and_then(Value::as_array) {
                for (i, s) in steps.iter().filter_map(Value::as_str).enumerate() {
                    t.push_str(&format!("\n  {}. {s}", i + 1));
                }
            }
            Some(t)
        }
        "episode" => f("summary").map(str::to_string),
        "gotcha" => f("statement").map(|s| format!("Gotcha: {s}")),
        "decision" => f("statement").map(|s| match f("chosen") {
            Some(chosen) => format!("{s} (chosen: {chosen})"),
            None => s.to_string(),
        }),
        _ => f("statement").map(str::to_string),
    }
}

/// Bucket of a node for a session of the owning workspace, from the server
/// fields and the trust bucket (absent until the trust job publishes one).
/// `None`: not delivered.
pub fn bucket_of(node: &Node, trust_bucket: Option<&str>) -> Option<(Bucket, bool)> {
    let own = node.provenance.as_deref() == Some("own");
    match node.review_state.as_deref() {
        Some("unconfirmed" | "reviewed_needs_person") => Some((Bucket::Unconfirmed, false)),
        Some("confirmed") | None => Some(match trust_bucket {
            Some("high") if !own => (Bucket::High, false),
            Some("low") => (Bucket::Normal, true),
            _ => (Bucket::Normal, false),
        }),
        Some(_) => None,
    }
}

/// Relevance tier of `text` for `query` terms: share of query terms the
/// text contains, cut at fixed quantiles (no raw scores in the output).
fn tier(text: &str, terms: &[String]) -> Option<u8> {
    if terms.is_empty() {
        return Some(0);
    }
    let lower = text.to_lowercase();
    let hits = terms.iter().filter(|t| lower.contains(t.as_str())).count();
    if hits == 0 {
        return None;
    }
    let share = hits * 100 / terms.len();
    Some(match share {
        100 => 0,
        67..=99 => 1,
        34..=66 => 2,
        _ => 3,
    })
}

/// The deliverable items of `graph` for `query` (all when empty), in the
/// order of §3.5.3: bucket → node type → relevance tier → node id.
pub fn items(graph: &Graph, trust: &dyn Fn(&str) -> Option<String>, query: &str) -> Vec<Item> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(str::to_lowercase)
        .filter(|t| t.chars().count() >= 2)
        .collect();
    let states = knowledge_state::Index::new(graph);
    let mut out: Vec<Item> = graph
        .live_nodes()
        .filter_map(|(id, node)| {
            // Rejected and superseded knowledge leaves delivery (SPEC-ENGINE
            // §3.4 edge table); outdated and conflicting go last, labelled.
            let (bucket, low) = match states.state(id, node) {
                (knowledge_state::State::Rejected, _)
                | (knowledge_state::State::Outdated, Some("superseded")) => return None,
                (knowledge_state::State::Outdated, _) => (Bucket::Outdated, false),
                (knowledge_state::State::Conflicting, _) => (Bucket::Conflicting, false),
                _ => bucket_of(node, trust(id).as_deref())?,
            };
            let text = text_of(node)?;
            Some(Item {
                node_id: id.clone(),
                node_type: node.node_type.clone(),
                bucket,
                low,
                own: node.provenance.as_deref() == Some("own"),
                tier: tier(&text, &terms)?,
                text,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        (a.bucket, a.low, type_rank(&a.node_type), a.tier, &a.node_id).cmp(&(
            b.bucket,
            b.low,
            type_rank(&b.node_type),
            b.tier,
            &b.node_id,
        ))
    });
    out
}

/// Render items for the agent within `budget_tokens` (an item that does
/// not fit is skipped whole). Header `vault @<ns>/<name> · <d>`, `d` a
/// digest of the delivered (node id, bucket) pairs; other members' text is
/// fenced as data.
///
/// Every item whose id follows the reference grammar carries its
/// `node:<id>` (v5 2.6.1), and the section says once how to cite it: a
/// claim that names the id in its refs gets a `derived_from` edge
/// (`capture::derived`), the only evidence of verified reuse (BAUPLAN
/// §5.3). The hint line is counted against the budget with the first
/// citable item; a section without one carries no hint.
pub fn render(vault: &str, items: &[Item], budget_tokens: usize) -> String {
    let source = format!("vault:{vault}");
    let mut lines = Vec::new();
    let mut delivered: Vec<(String, &str)> = Vec::new();
    let mut used = 0;
    let mut cited = false;
    for item in items {
        // Only an id the capture path accepts back is shown: a malformed
        // target from a foreign event is neither citable nor rendered
        // outside the data fence.
        let citable = matches!(
            crate::core::hive::capture::refs::parse(&format!("node:{}", item.node_id)),
            Some(crate::core::hive::capture::refs::Reference::Node(_))
        );
        let id = if citable {
            format!(" node:{}", item.node_id)
        } else {
            String::new()
        };
        let label = match item.bucket {
            Bucket::Unconfirmed => "[unconfirmed] ",
            Bucket::Outdated => "[outdated: re-verify] ",
            Bucket::Conflicting => "[conflicting] ",
            _ if item.low => "[low trust] ",
            _ => "",
        };
        let body = if item.own {
            item.text.clone()
        } else {
            wrap(UntrustedKind::Knowledge, &source, &item.text)
        };
        let badge = if item.own { "" } else { " (from workspace)" };
        let line = format!("- {label}{}{id}{badge}: {body}", item.node_type);
        let mut cost = crate::core::tokens::count_tokens(&line);
        if citable && !cited {
            cost += crate::core::tokens::count_tokens(CITE_HINT);
        }
        if used + cost > budget_tokens {
            continue;
        }
        used += cost;
        cited |= citable;
        lines.push(line);
        delivered.push((item.node_id.clone(), item.bucket.as_str()));
    }
    if lines.is_empty() {
        return String::new();
    }
    delivered.sort();
    let mut h = blake3::Hasher::new();
    for (id, bucket) in &delivered {
        h.update(id.as_bytes());
        h.update(&[0]);
        h.update(bucket.as_bytes());
        h.update(&[0]);
    }
    let digest = &h.finalize().to_hex()[..8];
    let hint = if cited {
        format!("{CITE_HINT}\n")
    } else {
        String::new()
    };
    format!("vault @{vault} · {digest}\n{hint}{}\n", lines.join("\n"))
}

/// How an agent cites a delivered item (once per section, ~15 tokens).
pub const CITE_HINT: &str = "Building on an item? Put its node:<id> in remember refs.";

/// Knowledge section budget per call (SPEC-ENGINE §3.5.1: hard max).
pub const MAX_SECTION_TOKENS: usize = 1_500;

/// The machine replica key, read once per process (keychain or file).
fn master_key(config_dir: &std::path::Path) -> Option<&'static [u8; 32]> {
    static MK: std::sync::OnceLock<Option<[u8; 32]>> = std::sync::OnceLock::new();
    MK.get_or_init(|| {
        crate::core::hive::replica::crypt::master_key(config_dir, false)
            .ok()
            .map(|(k, _)| *k)
    })
    .as_ref()
}

/// The folded graph of `vault_id`'s replica; `None` when there is none
/// (nothing is created here).
fn folded(store: &crate::core::hive::HiveStore, vault_id: &str) -> Option<Graph> {
    let dir = crate::core::hive::replica::store::place_dir(store.data_dir(), vault_id);
    if !dir.join("replica.db").exists() {
        return None;
    }
    let mk = master_key(store.config_dir())?;
    let (replica, _) =
        crate::core::hive::replica::Replica::open(store.data_dir(), vault_id, mk).ok()?;
    let mut graph = Graph::default();
    // lean-ctx: folds the stored log per call; switch to the materialised
    // nodes table once replicas reach ~10k events.
    graph.fold(&replica.fold_events().ok()?).ok()?;
    Some(graph)
}

/// `hive status --health`: the local figures of every sealed vault of
/// this machine with a replica (§3.5.5; never sent), in vault order.
pub fn sealed_health_lines(store: &crate::core::hive::HiveStore) -> Vec<String> {
    let map = crate::core::hive::vault::VaultMap::load(store.data_dir());
    let mut lines: Vec<String> = map
        .vaults
        .values()
        .filter(|r| r.sealed)
        .filter_map(|r| {
            let graph = folded(store, &r.vault_id)?;
            Some(format!(
                "Health @{}/{} (sealed, local): {}",
                r.ns,
                r.name,
                health::of(&graph).render()
            ))
        })
        .collect();
    lines.sort();
    lines
}

/// The vault section a recall in `project_root` delivers (§3.5.1): the
/// bound vault's replica, deterministic and within budget. `None` when the
/// checkout is not bound or has no replica yet (nothing is created here).
pub fn for_project(project_root: &str, query: &str) -> Option<String> {
    let store = crate::core::hive::HiveStore::open().ok()?;
    let root = std::path::Path::new(project_root);
    let stable = lean_ctx_protocol::hive::vault::stable_identity(
        &crate::core::hive::vault::project_identity(root)?,
    );
    let record = crate::core::hive::vault::VaultMap::load(store.data_dir())
        .vaults
        .get(&stable)?
        .clone();
    let graph = folded(&store, &record.vault_id)?;
    // Sealed places: the server cannot score what it cannot read, so the
    // trust bucket is computed here from the folded log (§3.5.5).
    let local = record.sealed.then(|| trust::buckets(&graph));
    let trust_of = |id: &str| {
        local
            .as_ref()
            .and_then(|b| b.get(id))
            .map(|b| (*b).to_string())
    };
    let list = items(&graph, &trust_of, query);
    let out = render(
        &format!("{}/{}", record.ns, record.name),
        &list,
        MAX_SECTION_TOKENS,
    );
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests;
