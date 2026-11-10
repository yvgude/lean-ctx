// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx knowledge remember|edit|relate|remove --json`: the knowledge
//! writes of a local reader (Hive Desktop) with a JSON result. Each runs the
//! `ctx_knowledge` action itself (memory policy, admission, protection,
//! locks unchanged) inside the released CLI run, then reads the store back
//! to say what happened. A write the gateway refused is `withheld`.

use serde::Serialize;

use lean_ctx::core::knowledge::{KnowledgeFact, ProjectKnowledge};
use lean_ctx::core::knowledge_relations::{
    KnowledgeEdgeKind, KnowledgeNodeRef, KnowledgeRelationGraph, parse_node_ref,
};
use lean_ctx_server::tools::ctx_knowledge;

use super::knowledge::{FactRow, local_rows};
use super::{Code, Failure};

#[derive(Debug, Serialize)]
pub(crate) struct Remembered {
    pub project_root: String,
    pub created: bool,
    pub fact: FactRow,
}

#[derive(Debug, Serialize)]
pub(crate) struct Edited {
    pub project_root: String,
    pub fact: FactRow,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct EdgeRow {
    pub from: String,
    pub to: String,
    pub relation: &'static str,
    pub created: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct Related {
    pub project_root: String,
    pub edge: EdgeRow,
}

#[derive(Debug, Serialize)]
pub(crate) struct Removed {
    pub project_root: String,
    pub id: String,
    pub removed: bool,
}

fn current<'a>(facts: &'a [KnowledgeFact], category: &str, key: &str) -> Option<&'a KnowledgeFact> {
    facts
        .iter()
        .filter(|f| f.is_current() && f.category == category && f.key == key)
        .max_by_key(|f| f.last_confirmed)
}

/// The `knowledge.list` row of the current fact `category/key`.
pub(crate) fn row_of(facts: &[KnowledgeFact], category: &str, key: &str) -> Option<FactRow> {
    let (rows, _) = local_rows(facts, true);
    rows.into_iter()
        .filter(|r| r.category == category && r.key == key)
        .max_by(|a, b| a.updated.cmp(&b.updated))
}

fn load(root: &str) -> Vec<KnowledgeFact> {
    ProjectKnowledge::load(root)
        .map(|k| k.facts)
        .unwrap_or_default()
}

/// `--id C/K` as a node.
fn node_arg(args: &[String], name: &str) -> Result<KnowledgeNodeRef, Failure> {
    super::flag(args, name)?
        .and_then(parse_node_ref)
        .ok_or_else(|| Failure::invalid(format!("{name} <category/key> is required")))
}

fn required<'a>(args: &'a [String], name: &str) -> Result<&'a str, Failure> {
    super::flag(args, name)?
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| Failure::invalid(format!("{name} is required")))
}

/// Run one `ctx_knowledge` action; a typed failure is `withheld`.
fn knowledge_action(
    root: &str,
    action: &str,
    category: Option<&str>,
    key: Option<&str>,
    value: Option<&str>,
    query: Option<&str>,
    confidence: Option<f32>,
) -> Result<String, Failure> {
    ctx_knowledge::handle_outcome(
        root,
        action,
        category,
        key,
        value,
        query,
        &super::super::knowledge_cmd::cli_session_id(),
        None,
        None,
        confidence,
        None,
        None,
    )
    .map_err(|reason| Failure::new(Code::Withheld, reason))
}

fn confidence_arg(args: &[String]) -> Result<Option<f32>, Failure> {
    super::flag(args, "--confidence")?
        .map(|raw| {
            raw.parse::<f32>()
                .ok()
                .filter(|c| (0.0..=1.0).contains(c))
                .ok_or_else(|| Failure::invalid("--confidence is 0.0-1.0"))
        })
        .transpose()
}

/// The fact `category/key` holds `value`'s write now: present and changed
/// since `before` (or already equal to it); else the reply says why not.
fn written(
    after: &[KnowledgeFact],
    before: Option<&KnowledgeFact>,
    category: &str,
    key: &str,
    reply: &str,
) -> Result<FactRow, Failure> {
    let now = current(after, category, key);
    let changed = match (before, now) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(b), Some(n)) => n.last_confirmed != b.last_confirmed || n.value != b.value,
    };
    if !changed {
        return Err(Failure::new(Code::Withheld, reply.trim().to_owned()));
    }
    row_of(after, category, key)
        .ok_or_else(|| Failure::new(Code::Internal, "the written fact is not readable"))
}

/// `knowledge remember --json --category C --key K --value V [--confidence X]`.
pub(crate) fn build_remember(args: &[String]) -> Result<Remembered, Failure> {
    let root = super::project_root(args)?;
    let category = required(args, "--category")?;
    let key = required(args, "--key")?;
    let value = required(args, "--value")?;
    let confidence = confidence_arg(args)?;
    let before = load(&root);
    let existed = current(&before, category, key).cloned();
    let reply = knowledge_action(
        &root,
        "remember",
        Some(category),
        Some(key),
        Some(value),
        None,
        confidence,
    )?;
    let fact = written(&load(&root), existed.as_ref(), category, key, &reply)?;
    Ok(Remembered {
        project_root: root,
        created: existed.is_none(),
        fact,
    })
}

/// `knowledge edit --json --id C/K --value V`: a new value for a current fact.
pub(crate) fn build_edit(args: &[String]) -> Result<Edited, Failure> {
    let root = super::project_root(args)?;
    let node = node_arg(args, "--id")?;
    let value = required(args, "--value")?;
    let before = load(&root);
    let Some(existed) = current(&before, &node.category, &node.key).cloned() else {
        return Err(Failure::new(
            Code::NotFound,
            format!("no current fact {}", node.id()),
        ));
    };
    let reply = knowledge_action(
        &root,
        "remember",
        Some(&node.category),
        Some(&node.key),
        Some(value),
        None,
        Some(existed.confidence),
    )?;
    let fact = written(
        &load(&root),
        Some(&existed),
        &node.category,
        &node.key,
        &reply,
    )?;
    Ok(Edited {
        project_root: root,
        fact,
    })
}

fn has_edge(
    root: &str,
    from: &KnowledgeNodeRef,
    to: &KnowledgeNodeRef,
    kind: KnowledgeEdgeKind,
) -> bool {
    let Some(knowledge) = ProjectKnowledge::load(root) else {
        return false;
    };
    KnowledgeRelationGraph::load(&knowledge.project_hash).is_some_and(|graph| {
        graph
            .edges
            .iter()
            .any(|e| e.from == *from && e.to == *to && e.kind == kind)
    })
}

/// `knowledge relate --json --from C/K --to C/K [--relation R]`.
pub(crate) fn build_relate(args: &[String]) -> Result<Related, Failure> {
    let root = super::project_root(args)?;
    let from = node_arg(args, "--from")?;
    let to = node_arg(args, "--to")?;
    let relation = super::flag(args, "--relation")?.unwrap_or("related_to");
    let kind = KnowledgeEdgeKind::parse(relation).ok_or_else(|| {
        Failure::invalid("--relation is depends_on|related_to|supports|contradicts|supersedes")
    })?;
    let facts = load(&root);
    for node in [&from, &to] {
        if current(&facts, &node.category, &node.key).is_none() {
            return Err(Failure::new(
                Code::NotFound,
                format!("no current fact {}", node.id()),
            ));
        }
    }
    let existed = has_edge(&root, &from, &to, kind);
    let reply = knowledge_action(
        &root,
        "relate",
        Some(&from.category),
        Some(&from.key),
        Some(kind.as_str()),
        Some(&to.id()),
        None,
    )?;
    if !has_edge(&root, &from, &to, kind) {
        return Err(Failure::new(Code::Unavailable, reply.trim().to_owned()));
    }
    Ok(Related {
        project_root: root,
        edge: EdgeRow {
            from: from.id(),
            to: to.id(),
            relation: kind.as_str(),
            created: !existed,
        },
    })
}

/// `knowledge remove --json --id C/K`.
pub(crate) fn build_remove(args: &[String]) -> Result<Removed, Failure> {
    let root = super::project_root(args)?;
    let node = node_arg(args, "--id")?;
    let existed = current(&load(&root), &node.category, &node.key).is_some();
    if existed {
        knowledge_action(
            &root,
            "remove",
            Some(&node.category),
            Some(&node.key),
            None,
            None,
            None,
        )?;
    }
    let removed = existed && current(&load(&root), &node.category, &node.key).is_none();
    Ok(Removed {
        project_root: root,
        id: node.id(),
        removed,
    })
}

/// The document of the knowledge write `kind` (`knowledge.remembered` |
/// `knowledge.edited` | `knowledge.related` | `knowledge.removed`), for
/// `via`; it carries project text, so it leaves through the released run.
pub(crate) fn answer(kind: &str, args: &[String], via: super::Via) -> super::Answer {
    let root = match super::project_root(args) {
        Ok(root) => root,
        Err(failure) => return super::Answer::failure(&failure),
    };
    // Writes: the release point sees the request (the fact's text among it)
    // before anything is stored (G08). A repeated write is safe by its key.
    let fields: &[&str] = match kind {
        "knowledge.remembered" => &["--category", "--key", "--value"],
        "knowledge.edited" => &["--id", "--value"],
        "knowledge.related" => &["--from", "--to", "--relation"],
        _ => &["--id"],
    };
    let preview = super::request_preview(args, fields);
    let write = || -> Result<serde_json::Value, Failure> {
        let value = match kind {
            "knowledge.remembered" => serde_json::to_value(build_remember(args)?),
            "knowledge.edited" => serde_json::to_value(build_edit(args)?),
            "knowledge.related" => serde_json::to_value(build_relate(args)?),
            _ => serde_json::to_value(build_remove(args)?),
        };
        value.map_err(|e| Failure::new(super::Code::Internal, e.to_string()))
    };
    super::answer_released_write("ctx_knowledge", &root, kind, via, &preview, write)
}

/// `knowledge remember|edit|relate|remove --json …`.
pub(crate) fn run(action: &str, args: &[String]) {
    let kind = match action {
        "remember" => "knowledge.remembered",
        "edit" => "knowledge.edited",
        "relate" => "knowledge.related",
        _ => "knowledge.removed",
    };
    super::print(&answer(kind, args, super::Via::Cli));
}
