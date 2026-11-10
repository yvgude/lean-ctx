// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx knowledge related <id> --json` and `lean-ctx knowledge graph
//! --json` (Local Read v1, `docs/contracts/local-read-v1.md`): the project's
//! knowledge as nodes and typed edges, for Hive Desktop.
//!
//! Nodes use the ids `knowledge list` prints (`category/key`, vault knowledge
//! `hive/<address>/<node>`), the same read-noise rule and timestamps; code
//! nodes use the Hive graph's ids (`code:<path>`, `code:<path>#<symbol>`).
//! Edge kinds are the Hive relation and graph types: recorded relations as
//! stored in `relations.json`, plus edges derived from the store on every
//! read (never stored, so they cannot go stale): `supersedes` along a key's
//! archived versions, `contradicts` between disagreeing current facts on one
//! key, `applies_to` from a fact to the code it is anchored to, `contains`
//! from a file to a symbol in it. Same store → same bytes.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use lean_ctx::core::knowledge::{KnowledgeFact, fact_version_id_v1, string_similarity};
use lean_ctx::core::knowledge_relations::{KnowledgeEdgeKind, KnowledgeRelationGraph};
use serde::Serialize;

use super::knowledge_shared::{current_ids, is_read_noise, rfc3339, version_id};
use super::local_read::knowledge::{FactRow, vault_rows};
use super::local_read::{self, Code, Failure};

const RELATED_DEPTH_DEFAULT: usize = 1;
const RELATED_DEPTH_MAX: usize = 3;
const RELATED_LIMIT_DEFAULT: usize = 200;
const RELATED_LIMIT_MAX: usize = 1000;
const GRAPH_LIMIT_DEFAULT: usize = 500;
const GRAPH_LIMIT_MAX: usize = 2000;
/// Values at least this similar are one claim reworded, not a contradiction
/// (the threshold `ProjectKnowledge::check_contradiction` uses).
const SAME_CLAIM_SIMILARITY: f32 = 0.8;
/// Category prefix of a relation end that names a vault node by vault id
/// (`lean_ctx_hive::migrate::REPLICA_NODE_CATEGORY`).
const REPLICA_CATEGORY: &str = "vault:";

/// Derived edge kinds of the Hive graph view (`hive::graph::GRAPH_EDGE_TYPES`).
const APPLIES_TO: &str = "applies_to";
const CONTAINS: &str = "contains";

#[derive(Debug, Clone, Serialize, PartialEq)]
struct Node {
    id: String,
    node_kind: &'static str,
    label: String,
    category: Option<String>,
    key: Option<String>,
    version: Option<String>,
    state: Option<&'static str>,
    text: Option<String>,
    path: Option<String>,
    symbol: Option<String>,
    distance: Option<usize>,
}

impl Node {
    fn fact(id: String, fact: &KnowledgeFact, state: &'static str) -> Self {
        Self {
            id,
            node_kind: "fact",
            label: fact.key.clone(),
            category: Some(fact.category.clone()),
            key: Some(fact.key.clone()),
            version: Some(fact_version_id_v1(fact)),
            state: Some(state),
            text: Some(fact.value.clone()),
            path: None,
            symbol: None,
            distance: None,
        }
    }

    fn vault(row: &FactRow) -> Self {
        Self {
            id: row.id.clone(),
            node_kind: "fact",
            label: row.key.clone(),
            category: Some(row.category.clone()),
            key: Some(row.key.clone()),
            version: None,
            state: Some("current"),
            text: Some(row.text.clone()),
            path: None,
            symbol: None,
            distance: None,
        }
    }

    fn missing(id: String, category: &str, key: &str) -> Self {
        Self {
            id,
            node_kind: "fact",
            label: key.to_string(),
            category: Some(category.to_string()),
            key: Some(key.to_string()),
            version: None,
            state: Some("missing"),
            text: None,
            path: None,
            symbol: None,
            distance: None,
        }
    }

    fn code(path: &str, symbol: Option<&str>) -> Self {
        Self {
            id: match symbol {
                Some(symbol) => format!("code:{path}#{symbol}"),
                None => format!("code:{path}"),
            },
            node_kind: "code",
            label: symbol.unwrap_or(path).to_string(),
            category: None,
            key: None,
            version: None,
            state: None,
            text: None,
            path: Some(path.to_string()),
            symbol: symbol.map(str::to_string),
            distance: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct Edge {
    from: String,
    to: String,
    kind: &'static str,
    origin: &'static str,
    count: Option<u32>,
    strength: Option<f64>,
    last_seen: Option<String>,
}

impl Edge {
    fn derived(from: &str, to: &str, kind: &'static str) -> Self {
        Self {
            from: from.to_string(),
            to: to.to_string(),
            kind,
            origin: "derived",
            count: None,
            strength: None,
            last_seen: None,
        }
    }
}

/// The vault linked to the checkout, as `knowledge list` reads it.
struct Vault {
    vault_id: String,
    address: String,
    rows: Vec<FactRow>,
}

/// The whole graph of one store: nodes by id, edges sorted by
/// `(from, kind, to)` with one edge per triple.
#[derive(Debug, Default)]
struct View {
    nodes: BTreeMap<String, Node>,
    edges: Vec<Edge>,
    excluded_noise: usize,
}

/// Add the code nodes of one anchor and the edge `from` → it.
fn anchor_edges(
    view: &mut View,
    derived: &mut Vec<Edge>,
    from: &str,
    path: &str,
    symbol: Option<&str>,
) {
    let file = Node::code(path, None);
    let target = match symbol {
        Some(symbol) => {
            let node = Node::code(path, Some(symbol));
            derived.push(Edge::derived(&file.id, &node.id, CONTAINS));
            let id = node.id.clone();
            view.nodes.entry(id.clone()).or_insert(node);
            id
        }
        None => file.id.clone(),
    };
    view.nodes.entry(file.id.clone()).or_insert(file);
    derived.push(Edge::derived(from, &target, APPLIES_TO));
}

/// Build the graph of `facts`, the linked `vault` and the recorded `relations`.
fn build_view(
    facts: &[KnowledgeFact],
    vault: Option<&Vault>,
    relations: Option<&KnowledgeRelationGraph>,
    include_noise: bool,
) -> View {
    let mut view = View::default();
    let mut excluded_keys: BTreeSet<(&str, &str)> = BTreeSet::new();

    // Current facts per key; the local one (else the newest) keeps the
    // `category/key` id `knowledge list` prints, the others get `@version`.
    let mut current: BTreeMap<(&str, &str), Vec<&KnowledgeFact>> = BTreeMap::new();
    let mut versions: HashMap<String, &KnowledgeFact> = HashMap::new();
    for fact in facts {
        if !include_noise && is_read_noise(fact) {
            if fact.is_current() {
                view.excluded_noise += 1;
                excluded_keys.insert((&fact.category, &fact.key));
            }
            continue;
        }
        versions.insert(fact_version_id_v1(fact), fact);
        if fact.is_current() {
            current
                .entry((&fact.category, &fact.key))
                .or_default()
                .push(fact);
        }
    }

    let mut primary: HashMap<(&str, &str), String> = HashMap::new();
    let mut id_of_version: HashMap<String, String> = HashMap::new();
    let mut current_nodes: Vec<(String, &KnowledgeFact)> = Vec::new();
    let all_current: Vec<&KnowledgeFact> = current.values().flatten().copied().collect();
    for (fact, id) in all_current.iter().zip(current_ids(&all_current)) {
        if id == format!("{}/{}", fact.category, fact.key) {
            primary.insert((&fact.category, &fact.key), id.clone());
        }
        id_of_version.insert(fact_version_id_v1(fact), id.clone());
        view.nodes
            .insert(id.clone(), Node::fact(id.clone(), fact, "current"));
        current_nodes.push((id, fact));
    }

    let mut derived: Vec<Edge> = Vec::new();

    // supersedes: follow each current fact's chain of replaced versions.
    for (id, fact) in &current_nodes {
        let mut from = id.clone();
        let mut next = fact.supersedes.as_ref();
        let mut seen = BTreeSet::new();
        while let Some(version) = next {
            if !seen.insert(version) {
                break;
            }
            let Some(old) = versions.get(version) else {
                break;
            };
            if old.is_current() {
                break;
            }
            let to = version_id(old);
            view.nodes
                .entry(to.clone())
                .or_insert_with(|| Node::fact(to.clone(), old, "superseded"));
            derived.push(Edge::derived(
                &from,
                &to,
                KnowledgeEdgeKind::Supersedes.as_str(),
            ));
            from = to;
            next = old.supersedes.as_ref();
        }
    }

    // contradicts: current facts on one key whose values disagree, newer → older.
    for group in current.values() {
        for (i, a) in group.iter().enumerate() {
            for b in &group[i + 1..] {
                if a.value.to_lowercase() == b.value.to_lowercase()
                    || string_similarity(&a.value, &b.value) > SAME_CLAIM_SIMILARITY
                {
                    continue;
                }
                let rank = |f: &KnowledgeFact| (f.created_at, fact_version_id_v1(f));
                let (newer, older) = if rank(a) >= rank(b) { (a, b) } else { (b, a) };
                let id = |f: &KnowledgeFact| id_of_version[&fact_version_id_v1(f)].clone();
                derived.push(Edge::derived(
                    &id(newer),
                    &id(older),
                    KnowledgeEdgeKind::Contradicts.as_str(),
                ));
            }
        }
    }

    // applies_to / contains: current facts to the code they are anchored to.
    for (id, fact) in &current_nodes {
        for anchor in &fact.anchors {
            anchor_edges(
                &mut view,
                &mut derived,
                id,
                &anchor.path,
                anchor.symbol.as_deref(),
            );
        }
    }

    // The linked vault's knowledge, with its anchors.
    if let Some(vault) = vault {
        for row in &vault.rows {
            view.nodes
                .entry(row.id.clone())
                .or_insert_with(|| Node::vault(row));
            for anchor in &row.anchors {
                anchor_edges(
                    &mut view,
                    &mut derived,
                    &row.id,
                    &anchor.path,
                    anchor.symbol.as_deref(),
                );
            }
        }
    }

    // Recorded relations, by `category/key`; a relation to a vault node by
    // vault id reads as that node's `knowledge list` id. An end that names
    // no node becomes a `missing` node only when its edge is kept, and an
    // edge is kept only when its other end is a real node: a relation
    // between two forgotten facts, or one to excluded noise, leaves no
    // dangling node behind to inflate the counts.
    let mut recorded: Vec<Edge> = Vec::new();
    let mut missing: BTreeMap<String, Node> = BTreeMap::new();
    for edge in relations.map(|g| g.edges.as_slice()).unwrap_or_default() {
        let endpoint = |category: &str, key: &str| -> Option<(String, bool)> {
            if let Some(id) = primary.get(&(category, key)) {
                return Some((id.clone(), true));
            }
            if excluded_keys.contains(&(category, key)) {
                return None;
            }
            let id = match (category.strip_prefix(REPLICA_CATEGORY), vault) {
                (Some(vault_id), Some(v)) if vault_id == v.vault_id => {
                    format!("hive/{}/{key}", v.address)
                }
                _ => format!("{category}/{key}"),
            };
            let known = view.nodes.contains_key(&id);
            Some((id, known))
        };
        let (Some((from, from_known)), Some((to, to_known))) = (
            endpoint(&edge.from.category, &edge.from.key),
            endpoint(&edge.to.category, &edge.to.key),
        ) else {
            continue;
        };
        if from == to || !(from_known || to_known) {
            continue;
        }
        for (id, known, end) in [(&from, from_known, &edge.from), (&to, to_known, &edge.to)] {
            if !known {
                missing
                    .entry(id.clone())
                    .or_insert_with(|| Node::missing(id.clone(), &end.category, &end.key));
            }
        }
        recorded.push(Edge {
            from,
            to,
            kind: edge.kind.as_str(),
            origin: "recorded",
            count: Some(edge.count),
            strength: Some(edge.strength),
            last_seen: edge.last_seen.map(rfc3339),
        });
    }
    view.nodes.extend(missing);

    // One edge per triple; a recorded edge wins over a derived twin.
    let mut by_triple: BTreeMap<(String, &'static str, String), Edge> = BTreeMap::new();
    for edge in derived.into_iter().chain(recorded) {
        let triple = (edge.from.clone(), edge.kind, edge.to.clone());
        if by_triple
            .get(&triple)
            .is_none_or(|kept| kept.origin != "recorded")
        {
            by_triple.insert(triple, edge);
        }
    }
    view.edges = by_triple.into_values().collect();
    view
}

#[derive(Serialize)]
struct GraphBody<'a> {
    project_root: &'a str,
    total_nodes: usize,
    total_edges: usize,
    truncated: bool,
    excluded_noise: usize,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

#[derive(Serialize)]
struct RelatedBody<'a> {
    project_root: &'a str,
    root: &'a str,
    depth: usize,
    direction: Direction,
    total_nodes: usize,
    total_edges: usize,
    truncated: bool,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

/// Which edges `related` follows from its root: `out` = what the root
/// points at (what it depends on, supersedes, applies to), `in` = what
/// points at the root (what depends on it), `both` = the two walks
/// together, each keeping its direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Direction {
    In,
    Out,
    Both,
}

impl Direction {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "in" => Some(Self::In),
            "out" => Some(Self::Out),
            "both" => Some(Self::Both),
            _ => None,
        }
    }
}

/// Breadth-first distances from `root` along `adjacent`, up to `depth`.
fn walk<'s>(
    adjacent: &HashMap<&'s str, BTreeSet<&'s str>>,
    root: &'s str,
    depth: usize,
) -> BTreeMap<&'s str, usize> {
    let mut distance: BTreeMap<&str, usize> = BTreeMap::from([(root, 0)]);
    let mut queue = VecDeque::from([root]);
    while let Some(id) = queue.pop_front() {
        let d = distance[id];
        if d == depth {
            continue;
        }
        for &next in adjacent.get(id).into_iter().flatten() {
            if !distance.contains_key(next) {
                distance.insert(next, d + 1);
                queue.push_back(next);
            }
        }
    }
    distance
}

fn edges_among(edges: &[Edge], kept: &BTreeSet<&str>) -> Vec<Edge> {
    edges
        .iter()
        .filter(|e| kept.contains(e.from.as_str()) && kept.contains(e.to.as_str()))
        .cloned()
        .collect()
}

/// The `knowledge.graph` body: under the cap the best connected nodes.
fn graph_body<'a>(view: &View, project_root: &'a str, limit: usize) -> GraphBody<'a> {
    let mut degree: HashMap<&str, usize> = HashMap::new();
    for e in &view.edges {
        *degree.entry(e.from.as_str()).or_default() += 1;
        *degree.entry(e.to.as_str()).or_default() += 1;
    }
    let mut ranked: Vec<&Node> = view.nodes.values().collect();
    ranked.sort_by_key(|n| {
        (
            std::cmp::Reverse(degree.get(n.id.as_str()).copied().unwrap_or(0)),
            n.state != Some("current"),
            n.id.as_str(),
        )
    });
    ranked.truncate(limit);
    let kept: BTreeSet<&str> = ranked.iter().map(|n| n.id.as_str()).collect();
    GraphBody {
        project_root,
        total_nodes: view.nodes.len(),
        total_edges: view.edges.len(),
        truncated: kept.len() < view.nodes.len(),
        excluded_noise: view.excluded_noise,
        nodes: view
            .nodes
            .values()
            .filter(|n| kept.contains(n.id.as_str()))
            .cloned()
            .collect(),
        edges: edges_among(&view.edges, &kept),
    }
}

/// The `knowledge.related` body, or `None` when `root` is no node.
fn related_body<'a>(
    view: &View,
    project_root: &'a str,
    root: &'a str,
    depth: usize,
    direction: Direction,
    limit: usize,
) -> Option<RelatedBody<'a>> {
    view.nodes.get(root)?;
    let mut outgoing: HashMap<&str, BTreeSet<&str>> = HashMap::new();
    let mut incoming: HashMap<&str, BTreeSet<&str>> = HashMap::new();
    for e in &view.edges {
        outgoing
            .entry(e.from.as_str())
            .or_default()
            .insert(e.to.as_str());
        incoming
            .entry(e.to.as_str())
            .or_default()
            .insert(e.from.as_str());
    }
    // Each walk keeps its direction for the whole path, so `both` is the
    // union of what the root reaches and what reaches the root, never a
    // zig-zag through a shared neighbour (`a → x ← b` does not relate a to b).
    let mut distance = match direction {
        Direction::Out | Direction::Both => walk(&outgoing, root, depth),
        Direction::In => walk(&incoming, root, depth),
    };
    if direction == Direction::Both {
        for (id, d) in walk(&incoming, root, depth) {
            let kept = distance.entry(id).or_insert(d);
            *kept = (*kept).min(d);
        }
    }
    let reached: BTreeSet<&str> = distance.keys().copied().collect();
    let mut order: Vec<(&str, usize)> = distance.iter().map(|(&id, &d)| (id, d)).collect();
    order.sort_by_key(|&(id, d)| (d, id));
    order.truncate(limit.max(1));
    let kept: BTreeSet<&str> = order.iter().map(|(id, _)| *id).collect();
    Some(RelatedBody {
        project_root,
        root,
        depth,
        direction,
        total_nodes: reached.len(),
        total_edges: edges_among(&view.edges, &reached).len(),
        truncated: kept.len() < reached.len(),
        nodes: order
            .iter()
            .map(|&(id, d)| Node {
                distance: Some(d),
                ..view.nodes[id].clone()
            })
            .collect(),
        edges: edges_among(&view.edges, &kept),
    })
}

// ---- command line -----------------------------------------------------------

/// The linked vault's knowledge, when this machine holds its replica;
/// without one the local knowledge stands alone (as in `knowledge list`).
fn linked_vault(project_root: &str) -> Option<Vault> {
    let linked = local_read::docs::open_linked(project_root).ok()?;
    let store = lean_ctx_hive::HiveStore::open().ok()?;
    let vault_id = lean_ctx_hive::vault::VaultMap::load(store.data_dir())
        .for_root(project_root)?
        .vault_id
        .clone();
    let events = linked.replica.fold_events().ok()?;
    let mut graph = lean_ctx_hive::replica::fold::Graph::default();
    graph.fold(&events).ok()?;
    let states = local_read::knowledge::vault_states(&graph, &linked.replica);
    Some(Vault {
        vault_id,
        rows: vault_rows(&graph, &states, &linked.address),
        address: linked.address,
    })
}

fn load_view(project_root: &str, include_noise: bool) -> View {
    let knowledge = lean_ctx::core::knowledge::ProjectKnowledge::load(project_root);
    let hash = knowledge.as_ref().map_or_else(
        || lean_ctx::core::project_hash::hash_project_root(project_root),
        |k| k.project_hash.clone(),
    );
    let relations = KnowledgeRelationGraph::load(&hash);
    let facts = knowledge
        .as_ref()
        .map(|k| k.facts.as_slice())
        .unwrap_or_default();
    build_view(
        facts,
        linked_vault(project_root).as_ref(),
        relations.as_ref(),
        include_noise,
    )
}

/// Flags every graph command takes; `--contract` is the Local Read opt-in.
const COMMON_FLAGS: [&str; 6] = [
    "--json",
    "--project-root",
    "--root",
    "--limit",
    "--include-noise",
    "--contract",
];
/// Flags only `related` takes.
const RELATED_FLAGS: [&str; 2] = ["--depth", "--direction"];

/// The arguments after `subcommand` that are neither a flag nor a flag's
/// value (the shared Local Read rule, so `--contract local-read/1` is never
/// read as an id). A flag the command does not take is `invalid_argument`
/// rather than silently ignored (`graph --depth 2` has no depth).
fn operands<'a>(
    args: &'a [String],
    subcommand: &str,
    own: &[&str],
) -> Result<Vec<&'a str>, Failure> {
    for arg in args.iter().filter(|a| a.starts_with("--")) {
        let name = arg.split_once('=').map_or(arg.as_str(), |(name, _)| name);
        if !COMMON_FLAGS.contains(&name) && !own.contains(&name) {
            return Err(Failure::invalid(format!(
                "knowledge {subcommand} does not take {name}"
            )));
        }
    }
    let mut rest = local_read::positionals(args).into_iter();
    // The subcommand is the first operand (`knowledge [--flags] related …`).
    if rest.next() != Some(subcommand) {
        return Err(Failure::invalid(format!(
            "usage: lean-ctx knowledge {subcommand}"
        )));
    }
    Ok(rest.collect())
}

fn direction(args: &[String]) -> Result<Direction, Failure> {
    match local_read::flag(args, "--direction")? {
        None => Ok(Direction::Both),
        Some(raw) => {
            Direction::parse(raw).ok_or_else(|| Failure::invalid("--direction is in, out or both"))
        }
    }
}

fn depth(args: &[String]) -> Result<usize, Failure> {
    match local_read::flag(args, "--depth")? {
        None => Ok(RELATED_DEPTH_DEFAULT),
        Some(raw) => raw
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=RELATED_DEPTH_MAX).contains(n))
            .ok_or_else(|| Failure::invalid(format!("--depth is 1-{RELATED_DEPTH_MAX}"))),
    }
}

/// Print the document (`--json`) or, for a person, the edges as lines and a
/// failure as text on stderr.
fn emit<T: Serialize>(
    args: &[String],
    command: &str,
    kind: &str,
    build: impl FnOnce(&str) -> Result<T, Failure>,
) {
    let json = local_read::wants_json(args);
    let root = match local_read::project_root(args) {
        Ok(root) => root,
        Err(failure) if json => local_read::fail(&failure),
        Err(failure) => fail_text(command, &failure),
    };
    if json {
        return local_read::emit_released("ctx_knowledge", &root, kind, || build(&root));
    }
    match build(&root).and_then(|body| {
        serde_json::to_value(&body).map_err(|e| Failure::new(Code::Internal, e.to_string()))
    }) {
        Ok(body) => {
            let text = super::common::CliToolRun::new("ctx_knowledge", &root, None)
                .human_facing()
                .deliver_text(|| edge_lines(&body));
            super::common::print_released(&text);
        }
        Err(failure) => fail_text(command, &failure),
    }
}

fn fail_text(command: &str, failure: &Failure) -> ! {
    eprintln!("lean-ctx knowledge {command}: {}", failure.message);
    std::process::exit(failure.code.exit_status());
}

fn edge_lines(body: &serde_json::Value) -> String {
    let edges = body["edges"].as_array().cloned().unwrap_or_default();
    if edges.is_empty() {
        return "No relations.".to_string();
    }
    edges
        .iter()
        .map(|e| {
            let s = |k: &str| e[k].as_str().unwrap_or_default().to_string();
            format!("{} -[{}]-> {}", s("from"), s("kind"), s("to"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The id `related` asks about, its depth, direction and node cap.
fn related_args(args: &[String]) -> Result<(&str, usize, Direction, usize), Failure> {
    const USAGE: &str = "usage: lean-ctx knowledge related <id> --json";
    let id = match operands(args, "related", &RELATED_FLAGS)?.as_slice() {
        [id] if !id.trim().is_empty() => id.trim(),
        [] | [_] => return Err(Failure::invalid(USAGE)),
        [_, extra, ..] => return Err(Failure::invalid(format!("unexpected argument {extra}"))),
    };
    Ok((
        id,
        depth(args)?,
        direction(args)?,
        local_read::limit(args, RELATED_LIMIT_DEFAULT, RELATED_LIMIT_MAX)?,
    ))
}

/// `lean-ctx knowledge related <id> [--json] [--depth N] [--direction in|out|both]
/// [--limit N] [--include-noise]`.
pub(crate) fn cmd_related(args: &[String]) {
    emit(args, "related", "knowledge.related", |root| {
        let (id, depth, direction, limit) = related_args(args)?;
        let view = load_view(root, args.iter().any(|a| a == "--include-noise"));
        related_body(&view, root, id, depth, direction, limit)
            .map(|body| serde_json::to_value(body).unwrap_or_default())
            .ok_or_else(|| Failure::new(Code::NotFound, format!("no knowledge node {id}")))
    });
}

/// `lean-ctx knowledge graph [--json] [--limit N] [--include-noise]`.
pub(crate) fn cmd_graph(args: &[String]) {
    emit(args, "graph", "knowledge.graph", |root| {
        if let Some(extra) = operands(args, "graph", &[])?.first() {
            return Err(Failure::invalid(format!("unexpected argument {extra}")));
        }
        let limit = local_read::limit(args, GRAPH_LIMIT_DEFAULT, GRAPH_LIMIT_MAX)?;
        let view = load_view(root, args.iter().any(|a| a == "--include-noise"));
        Ok(serde_json::to_value(graph_body(&view, root, limit)).unwrap_or_default())
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use lean_ctx::core::knowledge::anchors::CodeAnchor;
    use lean_ctx::core::knowledge::{FactOrigin, ProjectKnowledge};
    use lean_ctx::core::knowledge_relations::KnowledgeNodeRef;
    use lean_ctx::core::memory_policy::MemoryPolicy;
    use serde_json::Value;

    fn store() -> (impl Sized, ProjectKnowledge) {
        let data = lean_ctx::core::data_dir::isolated_data_dir();
        let community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
        let root = tempfile::tempdir().unwrap();
        let k = ProjectKnowledge::new(root.path().to_str().unwrap());
        ((root, community, data), k)
    }

    fn anchor(path: &str, symbol: Option<&str>) -> CodeAnchor {
        CodeAnchor {
            path: path.into(),
            symbol: symbol.map(Into::into),
            digest: format!("sha256:{}", "0".repeat(64)),
            lang: None,
        }
    }

    fn graph(view: &View) -> Value {
        serde_json::to_value(graph_body(view, "/p", 500)).unwrap()
    }

    fn edges(doc: &Value) -> Vec<(String, String, String, String)> {
        doc["edges"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                let s = |k: &str| e[k].as_str().unwrap().to_string();
                (s("from"), s("kind"), s("to"), s("origin"))
            })
            .collect()
    }

    /// The capture rules: an update supersedes, a disagreeing current fact
    /// of another origin contradicts, anchors are applied to (a file
    /// contains its symbol), recorded relations keep their stored kind,
    /// a relation to a vault node by vault id reads as its list id, and
    /// names a missing fact; noise stays out unless asked for.
    #[test]
    fn capture_rules_and_graph_shape() {
        let (_guard, mut k) = store();
        let policy = MemoryPolicy::default();
        k.remember(
            "architecture",
            "auth",
            "Sessions in a cookie",
            "s1",
            0.9,
            &policy,
        );
        k.remember(
            "architecture",
            "auth",
            "JWT RS256 validated in middleware",
            "s2",
            0.9,
            &policy,
        );
        k.remember(
            "api",
            "rate-limit",
            "100 requests per minute",
            "s1",
            0.8,
            &policy,
        );
        let mut other = k
            .facts
            .iter()
            .find(|f| f.key == "rate-limit")
            .unwrap()
            .clone();
        other.origin = FactOrigin::Unverified;
        other.value = "Unlimited for internal callers".into();
        other.created_at += chrono::Duration::seconds(5);
        k.facts.push(other);
        let auth = k
            .facts
            .iter_mut()
            .find(|f| f.key == "auth" && f.is_current())
            .unwrap();
        auth.anchors = vec![
            anchor("src/auth/mw.rs", Some("validate")),
            anchor("src/auth/mod.rs", None),
        ];
        let mut noise = k.facts[0].clone();
        noise.category = "auto:read".into();
        noise.valid_until = None;
        k.facts.push(noise);
        let vault = Vault {
            vault_id: "v_123".into(),
            address: "acme/app".into(),
            rows: Vec::new(),
        };
        let mut relations = KnowledgeRelationGraph::new("test");
        relations.upsert_edge(
            KnowledgeNodeRef::new("architecture", "auth"),
            KnowledgeNodeRef::new("api", "rate-limit"),
            KnowledgeEdgeKind::DependsOn,
            "s1",
        );
        relations.upsert_edge(
            KnowledgeNodeRef::new("api", "rate-limit"),
            KnowledgeNodeRef::new("vault:v_123", "n_limits"),
            KnowledgeEdgeKind::Supports,
            "s1",
        );

        let view = build_view(&k.facts, Some(&vault), Some(&relations), false);
        let doc = graph(&view);
        assert_eq!(doc["excluded_noise"], 1);
        assert_eq!(doc["truncated"], false);

        let old = k
            .facts
            .iter()
            .find(|f| f.key == "auth" && !f.is_current())
            .unwrap();
        let old = format!("architecture/auth@{}", fact_version_id_v1(old));
        let second = view
            .nodes
            .keys()
            .find(|id| id.starts_with("api/rate-limit@"))
            .expect("a second current fact on a key gets the @version id")
            .clone();
        let e = |from: &str, kind: &str, to: &str, origin: &str| {
            (from.into(), kind.into(), to.into(), origin.into())
        };
        assert_eq!(
            edges(&doc),
            vec![
                e(
                    "api/rate-limit",
                    "supports",
                    "hive/acme/app/n_limits",
                    "recorded"
                ),
                e(&second, "contradicts", "api/rate-limit", "derived"),
                e(
                    "architecture/auth",
                    "applies_to",
                    "code:src/auth/mod.rs",
                    "derived"
                ),
                e(
                    "architecture/auth",
                    "applies_to",
                    "code:src/auth/mw.rs#validate",
                    "derived"
                ),
                e(
                    "architecture/auth",
                    "depends_on",
                    "api/rate-limit",
                    "recorded"
                ),
                e("architecture/auth", "supersedes", &old, "derived"),
                e(
                    "code:src/auth/mw.rs",
                    "contains",
                    "code:src/auth/mw.rs#validate",
                    "derived"
                ),
            ]
        );
        let node = |id: &str| view.nodes[id].clone();
        assert_eq!(node(&old).state, Some("superseded"));
        assert_eq!(node("hive/acme/app/n_limits").state, Some("missing"));
        assert_eq!(node("code:src/auth/mw.rs#validate").node_kind, "code");
        assert_eq!(
            node("code:src/auth/mw.rs#validate").path.as_deref(),
            Some("src/auth/mw.rs")
        );
        let recorded = &doc["edges"][4];
        assert_eq!(recorded["count"], 1);
        assert!(recorded["last_seen"].as_str().unwrap().ends_with('Z'));

        let with_noise = build_view(&k.facts, None, Some(&relations), true);
        assert_eq!(with_noise.excluded_noise, 0);
        assert!(
            with_noise
                .nodes
                .keys()
                .any(|id| id.starts_with("auto:read/"))
        );
    }

    /// The document read from disk is the same bytes on every read.
    #[test]
    fn graph_document_is_byte_stable() {
        let (_guard, mut k) = store();
        let policy = MemoryPolicy::default();
        k.remember("a", "one", "first claim about storage", "s1", 0.9, &policy);
        k.remember("a", "one", "a different claim entirely", "s2", 0.9, &policy);
        k.facts[1].anchors = vec![anchor("src/x.rs", Some("f"))];
        k.save().unwrap();
        KnowledgeRelationGraph::mutate_locked(&k.project_root, &k.project_hash, |g| {
            g.upsert_edge(
                KnowledgeNodeRef::new("a", "one"),
                KnowledgeNodeRef::new("b", "two"),
                KnowledgeEdgeKind::RelatedTo,
                "s1",
            );
            Some(())
        })
        .unwrap();
        let read = || {
            let view = load_view(&k.project_root, false);
            local_read::document("knowledge.graph", &graph_body(&view, &k.project_root, 500))
        };
        let first = read();
        assert!(first.contains("\"supersedes\"") && first.contains("\"related_to\""));
        assert_eq!(read(), first);
    }

    /// `related` follows `--direction`: `in` answers "what depends on X",
    /// `out` "what X depends on", `both` the two walks without a zig-zag
    /// through a shared neighbour; totals are before the cap, an unknown
    /// id answers `None`, and relations naming two forgotten facts leave no
    /// dangling node in the counts.
    #[test]
    fn related_direction_depth_cap_and_dangling_ends() {
        let (_guard, mut k) = store();
        let policy = MemoryPolicy::default();
        for (key, value) in [
            ("one", "storage is postgres"),
            ("two", "the api caches responses"),
            ("three", "dashboards poll every minute"),
            ("four", "backups run nightly"),
            ("five", "exports are written as csv"),
        ] {
            k.remember("a", key, value, "s1", 0.9, &policy);
        }
        let mut relations = KnowledgeRelationGraph::new("test");
        let mut depends = |from: (&str, &str), to: (&str, &str)| {
            relations.upsert_edge(
                KnowledgeNodeRef::new(from.0, from.1),
                KnowledgeNodeRef::new(to.0, to.1),
                KnowledgeEdgeKind::DependsOn,
                "s1",
            );
        };
        depends(("a", "two"), ("a", "one"));
        depends(("a", "three"), ("a", "two"));
        depends(("a", "one"), ("a", "four"));
        depends(("a", "five"), ("a", "four"));
        depends(("a", "one"), ("x", "forgotten"));
        depends(("x", "gone"), ("x", "also-gone"));
        let view = build_view(&k.facts, None, Some(&relations), false);

        // Five facts and the one forgotten end of a kept relation; the
        // relation between two forgotten facts adds nothing.
        let doc = graph(&view);
        assert_eq!(doc["total_nodes"], 6);
        assert_eq!(doc["total_edges"], 5);
        assert_eq!(view.nodes["x/forgotten"].state, Some("missing"));
        assert!(!view.nodes.contains_key("x/gone"));

        let ids = |direction: Direction| -> Vec<(String, usize)> {
            related_body(&view, "/p", "a/one", 3, direction, 200)
                .unwrap()
                .nodes
                .into_iter()
                .map(|n| (n.id, n.distance.unwrap()))
                .collect()
        };
        let owned = |v: &[(&str, usize)]| -> Vec<(String, usize)> {
            v.iter().map(|&(id, d)| (id.to_string(), d)).collect()
        };
        assert_eq!(
            ids(Direction::In),
            owned(&[("a/one", 0), ("a/two", 1), ("a/three", 2)])
        );
        assert_eq!(
            ids(Direction::Out),
            owned(&[("a/one", 0), ("a/four", 1), ("x/forgotten", 1)])
        );
        assert_eq!(
            ids(Direction::Both),
            owned(&[
                ("a/one", 0),
                ("a/four", 1),
                ("a/two", 1),
                ("x/forgotten", 1),
                ("a/three", 2),
            ])
        );
        let shallow = related_body(&view, "/p", "a/one", 1, Direction::In, 200).unwrap();
        assert_eq!(shallow.total_nodes, 2);

        let capped = related_body(&view, "/p", "a/one", 3, Direction::Both, 2).unwrap();
        assert_eq!(capped.total_nodes, 5);
        assert_eq!(capped.total_edges, 4);
        assert!(capped.truncated);
        assert_eq!(capped.nodes.len(), 2);

        assert!(related_body(&view, "/p", "a/missing", 1, Direction::Both, 200).is_none());
    }

    /// `--contract local-read/1` is the opt-in, never the id; `--depth=N`
    /// counts; a flag the command does not take, a second id or a bad
    /// direction is `invalid_argument`.
    #[test]
    fn related_and_graph_arguments() {
        let args = |v: &[&str]| -> Vec<String> { v.iter().map(|s| (*s).to_string()).collect() };
        let ok = args(&[
            "--contract",
            "local-read/1",
            "related",
            "--json",
            "a/one",
            "--depth=2",
            "--direction",
            "in",
        ]);
        assert_eq!(
            related_args(&ok).unwrap(),
            ("a/one", 2, Direction::In, RELATED_LIMIT_DEFAULT)
        );
        let defaults = args(&["related", "a/one", "--contract", "local-read/1"]);
        assert_eq!(
            related_args(&defaults).unwrap(),
            ("a/one", 1, Direction::Both, RELATED_LIMIT_DEFAULT)
        );
        for bad in [
            &["related", "a/one", "a/two"][..],
            &["related", "--contract", "local-read/1"],
            &["related", "a/one", "--direction", "up"],
            &["related", "a/one", "--depth", "9"],
            &["related", "a/one", "--dept", "2"],
        ] {
            let failure = related_args(&args(bad)).unwrap_err();
            assert_eq!(failure.code, Code::InvalidArgument, "{bad:?}");
        }
        assert!(operands(&args(&["graph", "--depth", "2"]), "graph", &[]).is_err());
        assert_eq!(
            operands(
                &args(&["--contract", "local-read/1", "graph"]),
                "graph",
                &[]
            )
            .unwrap(),
            Vec::<&str>::new()
        );
    }
}
