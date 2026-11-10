// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx knowledge list --json`: the project's knowledge for a local
//! reader — local facts, gotchas, and the knowledge of the vault linked to
//! this checkout (from its local replica, with the state Hive derives too).
//! Automatic read-noise is left out unless `--include-noise` asks for it.

use serde::Serialize;
use serde_json::Value;

use lean_ctx::core::knowledge::{
    FactOrigin, FactReviewReason, FactReviewState, KnowledgeArchetype, KnowledgeFact, NodeType,
};
use lean_ctx_hive::delivery::knowledge_state::{Index, State};
use lean_ctx_hive::replica::fold::{Graph, Node};

use super::{Failure, rfc3339};

const DEFAULT_LIMIT: usize = 200;
const MAX_LIMIT: usize = 1000;
/// Vault node types listed as knowledge.
const VAULT_TYPES: [&str; 5] = ["fact", "decision", "gotcha", "procedure", "reference"];

#[derive(Debug, Serialize)]
pub(crate) struct KnowledgeList {
    pub project_root: String,
    pub total: usize,
    pub returned: usize,
    pub truncated: bool,
    pub excluded_noise: usize,
    /// Facts the release point withheld from this answer (#1298); they are
    /// not in `facts` and not counted in `returned`.
    pub withheld: usize,
    pub facts: Vec<FactRow>,
}

/// The facts of `knowledge.list` are released one by one: a fact the
/// release would withhold is left out, counted in `withheld`, and
/// `returned` counts the facts that are delivered.
pub(crate) const RECORDS: crate::cli::common::Records = crate::cli::common::Records {
    path: &["facts"],
    report: Some(report_withheld),
};

fn report_withheld(document: &mut Value, withheld: usize) {
    let returned = document
        .get("facts")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    if let Some(fields) = document.as_object_mut() {
        fields.insert("withheld".into(), withheld.into());
        fields.insert("returned".into(), returned.into());
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct FactRow {
    pub id: String,
    #[serde(rename = "type")]
    pub fact_type: &'static str,
    pub category: String,
    pub key: String,
    pub text: String,
    pub status: &'static str,
    pub origin: &'static str,
    pub confidence: Option<f32>,
    pub anchors: Vec<AnchorRow>,
    pub source: Option<SourceRow>,
    pub review: Option<&'static str>,
    pub created: Option<String>,
    pub updated: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct AnchorRow {
    pub path: String,
    pub symbol: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct SourceRow {
    pub kind: &'static str,
    #[serde(rename = "ref")]
    pub reference: String,
}

/// What a reader asked for.
#[derive(Debug, Clone, Default)]
pub(crate) struct Query {
    /// Lowercase categories or types; empty = all.
    pub categories: Vec<String>,
    /// Lowercase substring over id and text.
    pub text: Option<String>,
    pub limit: usize,
    pub include_noise: bool,
}

pub(crate) use super::super::knowledge_shared::is_read_noise;

fn fact_type(f: &KnowledgeFact) -> &'static str {
    if let Some(node_type) = f.capture.node_type {
        return match node_type {
            NodeType::Decision => "decision",
            NodeType::Gotcha => "gotcha",
            NodeType::Procedure => "procedure",
            NodeType::Fact | NodeType::Finding => "fact",
        };
    }
    if matches!(
        f.category.to_lowercase().as_str(),
        "reference" | "references" | "ref" | "refs"
    ) {
        return "reference";
    }
    match KnowledgeArchetype::infer_from_category(&f.category) {
        KnowledgeArchetype::Decision => "decision",
        KnowledgeArchetype::Gotcha => "gotcha",
        KnowledgeArchetype::Workflow => "procedure",
        _ => "fact",
    }
}

fn review_of(f: &KnowledgeFact) -> Option<&'static str> {
    let review = f.review.as_ref()?;
    Some(match (review.state, review.reason) {
        (FactReviewState::Contradicted, _) => "contradicted",
        (FactReviewState::ToVerify, FactReviewReason::AnchorMissing) => "gone",
        (FactReviewState::ToVerify, _) => "to_verify",
    })
}

/// One local fact as a row under `id` (from
/// [`knowledge_shared::current_ids`](super::super::knowledge_shared::current_ids)).
/// Facts a 1.x vault revision copied into the project knowledge read
/// `origin: vault`.
pub(crate) fn fact_row(f: &KnowledgeFact, id: String) -> FactRow {
    let vault = f
        .imported_from
        .as_deref()
        .and_then(lean_ctx::core::knowledge::import_tag::import_tag_source)
        .map(|tag| {
            let tag = tag.trim_start_matches('@');
            tag.rsplit_once("@r")
                .map_or(tag, |(address, _)| address)
                .to_owned()
        });
    let review = review_of(f);
    let (origin, status, source) = if let Some(address) = vault {
        let status = match review {
            Some("contradicted") => "conflicting",
            Some(_) => "outdated",
            None => "provisional",
        };
        let source = SourceRow {
            kind: "vault",
            reference: address,
        };
        ("vault", status, Some(source))
    } else {
        let origin = match &f.origin {
            FactOrigin::Local => "local",
            FactOrigin::Provider(_) => "provider",
            FactOrigin::Derived(_) => "derived",
            FactOrigin::Unverified => "unverified",
        };
        let source = match (&f.origin, f.imported_from.as_deref()) {
            (FactOrigin::Provider(p), _) => Some(SourceRow {
                kind: "provider",
                reference: format!("{}/{}", p.provider, p.resource),
            }),
            (_, Some(from)) => Some(SourceRow {
                kind: "import",
                reference: from.to_owned(),
            }),
            _ if !f.source_session.is_empty() => Some(SourceRow {
                kind: "session",
                reference: f.source_session.clone(),
            }),
            _ => None,
        };
        (origin, "local", source)
    };
    FactRow {
        id,
        fact_type: fact_type(f),
        category: f.category.clone(),
        key: f.key.clone(),
        text: f.value.clone(),
        status,
        origin,
        confidence: Some(f.confidence),
        anchors: f
            .anchors
            .iter()
            .map(|a| AnchorRow {
                path: a.path.clone(),
                symbol: a.symbol.clone(),
            })
            .collect(),
        source,
        review,
        created: Some(rfc3339(f.created_at)),
        updated: Some(rfc3339(f.last_confirmed)),
    }
}

/// One gotcha of the gotcha store as a row (`type: gotcha`).
pub(crate) fn gotcha_row(g: &lean_ctx::core::gotcha_tracker::Gotcha) -> FactRow {
    FactRow {
        id: format!("gotcha/{}", g.id),
        fact_type: "gotcha",
        category: "gotcha".into(),
        key: g.id.clone(),
        text: if g.resolution.trim().is_empty() {
            g.trigger.clone()
        } else {
            format!("{} — {}", g.trigger, g.resolution)
        },
        status: "local",
        origin: "local",
        confidence: Some(g.confidence),
        anchors: Vec::new(),
        source: g.session_ids.last().map(|s| SourceRow {
            kind: "session",
            reference: s.clone(),
        }),
        review: None,
        created: Some(rfc3339(g.first_seen)),
        updated: Some(rfc3339(g.last_seen)),
    }
}

fn node_text(node: &Node) -> Option<String> {
    let f = |k: &str| node.fields.get(k).and_then(Value::as_str);
    match node.node_type.as_str() {
        "procedure" => {
            let mut text = f("title").or_else(|| f("statement"))?.to_owned();
            if let Some(steps) = node.fields.get("steps").and_then(Value::as_array) {
                for (i, step) in steps.iter().filter_map(Value::as_str).enumerate() {
                    text.push_str(&format!("\n{}. {step}", i + 1));
                }
            }
            Some(text)
        }
        "decision" => f("statement").map(|s| match f("chosen") {
            Some(chosen) => format!("{s} (chosen: {chosen})"),
            None => s.to_owned(),
        }),
        _ => f("statement").map(str::to_owned),
    }
}

/// The knowledge states of a vault graph: its edges and the fact links the
/// replica's save history resolves, so two Instances' concurrent values of
/// one fact read `conflicting` as soon as both are here (#286).
pub(crate) fn vault_states(graph: &Graph, replica: &lean_ctx_hive::replica::Replica) -> Index {
    match replica.first_saves() {
        Ok(saves) => Index::with_saves(graph, &saves),
        Err(_) => Index::new(graph),
    }
}

/// The knowledge nodes of a vault graph as rows: rejected and superseded
/// nodes are left out (they are no longer knowledge), the rest carry the
/// state Hive derives from the same log.
pub(crate) fn vault_rows(graph: &Graph, states: &Index, address: &str) -> Vec<FactRow> {
    let category = format!("hive/{address}");
    graph
        .live_nodes()
        .filter(|(_, node)| VAULT_TYPES.contains(&node.node_type.as_str()))
        .filter_map(|(id, node)| {
            let status = match states.state(id, node) {
                (State::Rejected, _) | (State::Outdated, Some("superseded")) => return None,
                (State::Verified, _) => "verified",
                (State::Provisional, _) => "provisional",
                (State::Outdated, _) => "outdated",
                (State::Conflicting, _) => "conflicting",
            };
            let text = node_text(node)?;
            let anchors = node
                .fields
                .get("anchors")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|a| {
                    Some(AnchorRow {
                        path: a.get("path")?.as_str()?.to_owned(),
                        symbol: a.get("symbol").and_then(Value::as_str).map(str::to_owned),
                    })
                })
                .collect();
            let review = match node.fields.get("anchor_state").and_then(Value::as_str) {
                Some("to_verify") => Some("to_verify"),
                Some("gone") => Some("gone"),
                _ if status == "conflicting" => Some("contradicted"),
                _ => None,
            };
            let fact_type = VAULT_TYPES
                .into_iter()
                .find(|t| *t == node.node_type)
                .unwrap_or("fact");
            Some(FactRow {
                id: format!("{category}/{id}"),
                fact_type,
                category: category.clone(),
                key: id.clone(),
                text,
                status,
                origin: "vault",
                confidence: None,
                anchors,
                source: Some(SourceRow {
                    kind: "vault",
                    reference: address.to_owned(),
                }),
                review,
                created: None,
                updated: None,
            })
        })
        .collect()
}

/// Filter, order (newest `updated` first, undated last, then by id) and
/// cap `rows`; `excluded_noise` is counted by the caller.
pub(crate) fn select(
    project_root: &str,
    mut rows: Vec<FactRow>,
    query: &Query,
    excluded_noise: usize,
) -> KnowledgeList {
    rows.retain(|r| {
        (query.categories.is_empty()
            || query
                .categories
                .iter()
                .any(|c| r.category.eq_ignore_ascii_case(c) || r.fact_type == c))
            && query.text.as_deref().is_none_or(|needle| {
                r.id.to_lowercase().contains(needle) || r.text.to_lowercase().contains(needle)
            })
    });
    rows.sort_by(|a, b| {
        b.updated
            .is_some()
            .cmp(&a.updated.is_some())
            .then_with(|| b.updated.cmp(&a.updated))
            .then_with(|| a.id.cmp(&b.id))
    });
    let total = rows.len();
    rows.truncate(query.limit);
    KnowledgeList {
        project_root: project_root.to_owned(),
        total,
        returned: rows.len(),
        truncated: total > rows.len(),
        excluded_noise,
        withheld: 0,
        facts: rows,
    }
}

/// The current facts of `facts` as rows, with unique ids, and how many
/// read-noise facts were left out.
pub(crate) fn local_rows(facts: &[KnowledgeFact], include_noise: bool) -> (Vec<FactRow>, usize) {
    let mut noise = 0usize;
    let current: Vec<&KnowledgeFact> = facts
        .iter()
        .filter(|f| f.is_current())
        .filter(|f| {
            let skip = !include_noise && is_read_noise(f);
            noise += usize::from(skip);
            !skip
        })
        .collect();
    let ids = super::super::knowledge_shared::current_ids(&current);
    let rows = current
        .into_iter()
        .zip(ids)
        .map(|(f, id)| fact_row(f, id))
        .collect();
    (rows, noise)
}

/// Every row of the project (before filters) and the read-noise left out.
fn collect(project_root: &str, include_noise: bool) -> (Vec<FactRow>, usize) {
    let mut rows = Vec::new();
    let mut noise = 0usize;
    if let Some(k) = lean_ctx::core::knowledge::ProjectKnowledge::load(project_root) {
        (rows, noise) = local_rows(&k.facts, include_noise);
    }
    let now = chrono::Utc::now();
    let gotchas = lean_ctx::core::gotcha_tracker::GotchaStore::load(project_root);
    rows.extend(
        gotchas
            .gotchas
            .iter()
            .filter(|g| g.expires_at.is_none_or(|at| at > now))
            .map(gotcha_row),
    );
    // The linked vault's knowledge, when this machine holds its replica;
    // without one the local knowledge stands alone.
    if let Ok(vault) = super::docs::open_linked(project_root)
        && let Ok(events) = vault.replica.fold_events()
    {
        let mut graph = Graph::default();
        if graph.fold(&events).is_ok() {
            rows.extend(vault_rows(
                &graph,
                &vault_states(&graph, &vault.replica),
                &vault.address,
            ));
        }
    }
    (rows, noise)
}

fn query_of(args: &[String]) -> Result<Query, Failure> {
    Ok(Query {
        categories: super::flags(args, "--category")?
            .into_iter()
            .map(str::to_lowercase)
            .collect(),
        text: super::flag(args, "--query")?.map(str::to_lowercase),
        limit: super::limit(args, DEFAULT_LIMIT, MAX_LIMIT)?,
        include_noise: args.iter().any(|a| a == "--include-noise"),
    })
}

/// `knowledge list --json [--project-root P] [--category C]… [--query Q]
/// [--limit N] [--include-noise]`.
pub(crate) fn run(args: &[String]) {
    if !super::wants_json(args) {
        super::fail(&Failure::invalid(
            "knowledge list needs --json (the text listing is `knowledge recall`)",
        ));
    }
    let (root, query) = match parse(args) {
        Ok(pair) => pair,
        Err(failure) => super::fail(&failure),
    };
    super::print(&super::answer_released_records(
        "ctx_knowledge",
        &root,
        "knowledge.list",
        super::Via::Cli,
        RECORDS,
        || build(&root, &query),
    ));
}

/// The document of `knowledge list --json`, for `via`.
#[cfg_attr(not(feature = "http-server"), allow(dead_code))]
pub(crate) fn answer(args: &[String], via: super::Via) -> super::Answer {
    let (root, query) = match parse(args) {
        Ok(pair) => pair,
        Err(failure) => return super::Answer::failure(&failure),
    };
    super::answer_released_records(
        "ctx_knowledge",
        &root,
        "knowledge.list",
        via,
        RECORDS,
        || build(&root, &query),
    )
}

/// The project and the selection `args` name.
fn parse(args: &[String]) -> Result<(String, Query), Failure> {
    Ok((super::project_root(args)?, query_of(args)?))
}

fn build(root: &str, query: &Query) -> Result<KnowledgeList, Failure> {
    let (rows, noise) = collect(root, query.include_noise);
    Ok(select(root, rows, query, noise))
}
