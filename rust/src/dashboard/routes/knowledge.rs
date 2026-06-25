use serde::Deserialize;

use super::helpers::{detect_project_root_for_dashboard, json_err, json_ok};

pub(super) fn handle(
    path: &str,
    query_str: &str,
    method: &str,
    body: &str,
) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/knowledge/edit" if method.eq_ignore_ascii_case("POST") => {
            Some(post_knowledge_edit(body))
        }
        "/api/knowledge-relations/edit" if method.eq_ignore_ascii_case("POST") => {
            Some(post_knowledge_relations_edit(body))
        }
        _ => get_routes(path, query_str),
    }
}

fn get_routes(path: &str, _query_str: &str) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/knowledge" => {
            let project_root = detect_project_root_for_dashboard();
            let policy = crate::core::config::Config::load()
                .memory_policy_effective()
                .unwrap_or_default();
            let _ = crate::core::knowledge::ProjectKnowledge::migrate_legacy_empty_root(
                &project_root,
                &policy,
            );

            let mut knowledge =
                crate::core::knowledge::ProjectKnowledge::load_or_create(&project_root);
            if knowledge.facts.is_empty() {
                // Keep /api/knowledge fast: read the property graph if it is already
                // populated, but never force a full (re)build on this request path.
                let open = crate::core::graph_provider::open_best_effort(&project_root);
                let provider = open.as_ref().map(|o| &o.provider);
                if crate::core::knowledge_bootstrap::bootstrap_if_empty(
                    &mut knowledge,
                    &project_root,
                    provider,
                    &policy,
                ) {
                    let _ = knowledge.save();
                }
            }
            // Expose the store cap so the UI can say "newest N kept" instead
            // of presenting a capped list as the complete history (#492).
            let mut value =
                serde_json::to_value(&knowledge).unwrap_or_else(|_| serde_json::json!({}));
            if let Some(obj) = value.as_object_mut() {
                obj.insert(
                    "max_facts".to_string(),
                    serde_json::json!(policy.knowledge.max_facts),
                );
            }
            let json = serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string());
            Some(("200 OK", "application/json", json))
        }
        "/api/knowledge-relations" => {
            let project_root = detect_project_root_for_dashboard();
            let policy = crate::core::config::Config::load()
                .memory_policy_effective()
                .unwrap_or_default();

            let knowledge = crate::core::knowledge::ProjectKnowledge::load_or_create(&project_root);
            let graph = crate::core::knowledge_relations::KnowledgeRelationGraph::load_or_create(
                &knowledge.project_hash,
            );

            let current_ids: std::collections::HashSet<String> = knowledge
                .facts
                .iter()
                .filter(|f| f.is_current())
                .map(|f| format!("{}/{}", f.category, f.key))
                .collect();

            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut edges: Vec<serde_json::Value> = Vec::new();

            let mut push_edge = |from: String, to: String, kind: String, derived: bool| {
                if from.trim().is_empty() || to.trim().is_empty() || from == to {
                    return;
                }
                if !current_ids.contains(&from) || !current_ids.contains(&to) {
                    return;
                }
                let key = format!("{from}|{kind}|{to}");
                if !seen.insert(key) {
                    return;
                }
                edges.push(serde_json::json!({
                    "from": from,
                    "to": to,
                    "kind": kind,
                    "derived": derived,
                }));
            };

            // Explicit user-managed relations.
            for e in &graph.edges {
                push_edge(e.from.id(), e.to.id(), e.kind.as_str().to_string(), false);
            }

            // Derived: `supersedes` links (stored on facts).
            for f in knowledge.facts.iter().filter(|f| f.is_current()) {
                let Some(to) = f
                    .supersedes
                    .as_deref()
                    .and_then(crate::core::knowledge_relations::parse_node_ref)
                else {
                    continue;
                };
                let from = format!("{}/{}", f.category, f.key);
                push_edge(from, to.id(), "supersedes".to_string(), true);
            }

            // Derived: soft references in values like `category/key` or `category:key`.
            for f in knowledge.facts.iter().filter(|f| f.is_current()) {
                let from = format!("{}/{}", f.category, f.key);
                for raw in f.value.split_whitespace() {
                    let tok = raw.trim_matches(|c: char| {
                        !c.is_ascii_alphanumeric() && c != '/' && c != ':' && c != '_' && c != '-'
                    });
                    let Some(to) = crate::core::knowledge_relations::parse_node_ref(tok) else {
                        continue;
                    };
                    if to.id() == from {
                        continue;
                    }
                    push_edge(from.clone(), to.id(), "related_to".to_string(), true);
                }
            }

            // Derived: facts that mention the same *specific* entity (file path,
            // ticket id, CamelCase/snake_case symbol, proper noun). A
            // document-frequency window keeps only references shared by a small
            // number of facts, so generic tokens (project name, common words)
            // never create hub explosions — only meaningful, specific shared
            // references link two facts (IDF-style co-occurrence).
            {
                use std::collections::{HashMap, HashSet};
                const MIN_DF: usize = 2;
                const MAX_DF: usize = 8;

                let mut ref_to_facts: HashMap<String, Vec<String>> = HashMap::new();
                for f in knowledge.facts.iter().filter(|f| f.is_current()) {
                    let id = format!("{}/{}", f.category, f.key);
                    let local: HashSet<String> = extract_fact_references(&f.key, &f.value)
                        .into_iter()
                        .collect();
                    for r in local {
                        ref_to_facts.entry(r).or_default().push(id.clone());
                    }
                }

                // Deterministic iteration for stable edge ordering.
                let mut refs: Vec<(&String, &Vec<String>)> = ref_to_facts.iter().collect();
                refs.sort_by(|a, b| a.0.cmp(b.0));
                for (_tok, fact_ids) in refs {
                    let mut ids: Vec<&String> = fact_ids.iter().collect();
                    ids.sort();
                    ids.dedup();
                    if ids.len() < MIN_DF || ids.len() > MAX_DF {
                        continue;
                    }
                    for i in 0..ids.len() {
                        for j in (i + 1)..ids.len() {
                            // Canonical (sorted) direction so the symmetric
                            // relation is emitted once.
                            push_edge(
                                ids[i].clone(),
                                ids[j].clone(),
                                "shares_ref".to_string(),
                                true,
                            );
                        }
                    }
                }
            }

            let max_edges = policy.knowledge.max_facts.saturating_mul(8);
            if max_edges > 0 && edges.len() > max_edges {
                edges.truncate(max_edges);
            }

            let payload = serde_json::json!({
                "project_root": project_root,
                "project_hash": knowledge.project_hash,
                "edges": edges,
                "explicit_edges_total": graph.edges.len(),
            });
            let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
            Some(("200 OK", "application/json", json))
        }
        "/api/gotchas" => {
            let project_root = detect_project_root_for_dashboard();
            let store = crate::core::gotcha_tracker::GotchaStore::load(&project_root);
            let json = serde_json::to_string(&store).unwrap_or_else(|_| "{}".to_string());
            Some(("200 OK", "application/json", json))
        }
        _ => None,
    }
}

#[derive(Deserialize)]
struct KnowledgeEditReq {
    action: String,
    fact_index: usize,
    #[serde(default)]
    value: Option<String>,
}

fn post_knowledge_edit(body: &str) -> (&'static str, &'static str, String) {
    let req: KnowledgeEditReq = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => {
            return (
                "400 Bad Request",
                "application/json",
                json_err(&format!("invalid JSON: {e}")),
            );
        }
    };
    let project_root = detect_project_root_for_dashboard();
    let policy = crate::core::config::Config::load()
        .memory_policy_effective()
        .unwrap_or_default();
    let _ =
        crate::core::knowledge::ProjectKnowledge::migrate_legacy_empty_root(&project_root, &policy);
    // Read-modify-write under the cross-process lock (#326/#594): the dashboard
    // shares the knowledge store with the daemon and MCP server.
    let result =
        crate::core::knowledge::ProjectKnowledge::mutate_locked(&project_root, |knowledge| {
            let current_idxs: Vec<usize> = knowledge
                .facts
                .iter()
                .enumerate()
                .filter(|(_, f)| f.is_current())
                .map(|(i, _)| i)
                .collect();
            let Some(&real_idx) = current_idxs.get(req.fact_index) else {
                return Err(("400 Bad Request", json_err("fact_index out of range")));
            };

            match req.action.as_str() {
                "archive" => {
                    knowledge.facts[real_idx].valid_until = Some(chrono::Utc::now());
                    knowledge.facts[real_idx].valid_from = knowledge.facts[real_idx]
                        .valid_from
                        .or(Some(knowledge.facts[real_idx].created_at));
                }
                "delete" => {
                    knowledge.facts.swap_remove(real_idx);
                }
                "update_confidence" => {
                    let Some(ref raw) = req.value else {
                        return Err((
                            "400 Bad Request",
                            json_err("value required for update_confidence"),
                        ));
                    };
                    let Ok(c) = raw.parse::<f32>() else {
                        return Err(("400 Bad Request", json_err("value must be a float")));
                    };
                    knowledge.facts[real_idx].confidence = c.clamp(0.0, 1.0);
                }
                _ => {
                    return Err(("400 Bad Request", json_err("unknown action")));
                }
            }

            knowledge.updated_at = chrono::Utc::now();
            Ok(())
        });

    match result {
        Ok((_, Ok(()))) => ("200 OK", "application/json", json_ok()),
        Ok((_, Err((status, body)))) => (status, "application/json", body),
        Err(e) => (
            "500 Internal Server Error",
            "application/json",
            json_err(&e),
        ),
    }
}

#[derive(Deserialize)]
struct RelationEditReq {
    action: String,
    relation: RelationBody,
}

#[derive(Deserialize)]
struct RelationBody {
    from: String,
    to: String,
    kind: String,
}

fn post_knowledge_relations_edit(body: &str) -> (&'static str, &'static str, String) {
    let req: RelationEditReq = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => {
            return (
                "400 Bad Request",
                "application/json",
                json_err(&format!("invalid JSON: {e}")),
            );
        }
    };
    let Some(from) = crate::core::knowledge_relations::parse_node_ref(&req.relation.from) else {
        return (
            "400 Bad Request",
            "application/json",
            json_err("invalid relation.from"),
        );
    };
    let Some(to) = crate::core::knowledge_relations::parse_node_ref(&req.relation.to) else {
        return (
            "400 Bad Request",
            "application/json",
            json_err("invalid relation.to"),
        );
    };
    let Some(kind) = crate::core::knowledge_relations::KnowledgeEdgeKind::parse(&req.relation.kind)
    else {
        return (
            "400 Bad Request",
            "application/json",
            json_err("invalid relation.kind"),
        );
    };

    let project_root = detect_project_root_for_dashboard();
    let knowledge = crate::core::knowledge::ProjectKnowledge::load_or_create(&project_root);
    let mut graph = crate::core::knowledge_relations::KnowledgeRelationGraph::load_or_create(
        &knowledge.project_hash,
    );

    match req.action.as_str() {
        "add" => {
            let _ = graph.upsert_edge(from, to, kind, "dashboard");
            let max_edges = crate::core::config::Config::load()
                .memory_policy_effective()
                .unwrap_or_default()
                .knowledge
                .max_facts
                .saturating_mul(8);
            let _ = graph.enforce_cap(max_edges);
            if let Err(e) = graph.save() {
                return (
                    "500 Internal Server Error",
                    "application/json",
                    json_err(&e),
                );
            }
            ("200 OK", "application/json", json_ok())
        }
        "delete" => {
            let n = graph.remove_edge(&from, &to, Some(kind));
            if n == 0 {
                return (
                    "400 Bad Request",
                    "application/json",
                    json_err("no matching relation"),
                );
            }
            if let Err(e) = graph.save() {
                return (
                    "500 Internal Server Error",
                    "application/json",
                    json_err(&e),
                );
            }
            ("200 OK", "application/json", json_ok())
        }
        _ => (
            "400 Bad Request",
            "application/json",
            json_err("unknown action"),
        ),
    }
}

/// Extracts specific, linkable references from a fact (file paths, ticket ids,
/// backticked code spans, CamelCase / `snake_case` identifiers, proper nouns).
/// Generic words are filtered out downstream via a document-frequency window.
/// Returned tokens are lowercased so `Stripe`/`stripe` unify when matching.
fn extract_fact_references(key: &str, value: &str) -> Vec<String> {
    let text = format!("{key} {value}");
    let mut out: Vec<String> = Vec::new();
    for raw in text.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | '`' | '!' | '?' | ':'
            )
    }) {
        // Keep characters meaningful for identifiers/paths/tickets.
        let tok = raw.trim_matches(|c: char| {
            !c.is_ascii_alphanumeric() && !matches!(c, '/' | '.' | '_' | '#' | '-')
        });
        if tok.len() < 4 {
            continue;
        }
        if is_linkable_reference(tok) {
            out.push(tok.to_ascii_lowercase());
        }
    }
    out
}

/// True when a token is *specific* enough to meaningfully link two facts.
fn is_linkable_reference(tok: &str) -> bool {
    // Ticket reference: #123
    if let Some(rest) = tok.strip_prefix('#') {
        return !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit());
    }
    // Path-like: contains a separator and looks substantial.
    if tok.contains('/') && (tok.contains('.') || tok.len() >= 6) {
        return true;
    }
    // File with an extension: foo.rs, bar.tsx, config.yaml
    if let Some((stem, ext)) = tok.rsplit_once('.')
        && stem.len() >= 2
        && (1..=5).contains(&ext.len())
        && ext.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return true;
    }
    // CamelCase identifier: a lower->Upper transition (ProjectIndex, CallGraph).
    let bytes = tok.as_bytes();
    let camel = bytes
        .windows(2)
        .any(|w| w[0].is_ascii_lowercase() && w[1].is_ascii_uppercase());
    if camel && tok.chars().all(|c| c.is_ascii_alphanumeric()) {
        return true;
    }
    // snake_case identifier with an underscore.
    if tok.len() >= 5
        && tok.contains('_')
        && tok.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return true;
    }
    // Proper noun: Capitalised word with a lowercase tail (Stripe, Webhook,
    // Leiden). All-caps acronyms are excluded — they are usually generic and
    // get filtered by the document-frequency window anyway.
    if tok.len() >= 4
        && bytes[0].is_ascii_uppercase()
        && tok.chars().all(|c| c.is_ascii_alphabetic())
        && tok.chars().any(|c| c.is_ascii_lowercase())
    {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_specific_references() {
        assert!(is_linkable_reference("#256"));
        assert!(is_linkable_reference("rust/src/core/community.rs"));
        assert!(is_linkable_reference("callgraph.rs"));
        assert!(is_linkable_reference("ProjectIndex"));
        assert!(is_linkable_reference("detect_communities"));
        assert!(is_linkable_reference("Stripe"));
    }

    #[test]
    fn rejects_generic_tokens() {
        assert!(!is_linkable_reference("the"));
        assert!(!is_linkable_reference("with"));
        assert!(!is_linkable_reference("#")); // bare hash, no number
        assert!(!is_linkable_reference("#ab")); // non-numeric ticket
        assert!(!is_linkable_reference("lower")); // plain lowercase word
        assert!(!is_linkable_reference("ALLCAPS")); // not a Capitalised proper noun
    }

    #[test]
    fn extracts_and_lowercases() {
        let refs = extract_fact_references(
            "deployment/stripe",
            "Stripe webhook verified in `billing_edge.rs`, see #256",
        );
        assert!(refs.contains(&"stripe".to_string()));
        assert!(refs.contains(&"billing_edge.rs".to_string()));
        assert!(refs.contains(&"#256".to_string()));
        // generic lowercase words are not collected
        assert!(!refs.contains(&"webhook".to_string()));
        assert!(!refs.contains(&"verified".to_string()));
    }

    #[test]
    fn shared_reference_links_two_facts() {
        // Two facts mentioning the same file should both surface that ref.
        let a = extract_fact_references("arch/a", "see core/community.rs for Leiden");
        let b = extract_fact_references("arch/b", "core/community.rs hardening pass");
        let sa: std::collections::HashSet<_> = a.into_iter().collect();
        let sb: std::collections::HashSet<_> = b.into_iter().collect();
        let shared: Vec<_> = sa.intersection(&sb).collect();
        assert!(shared.contains(&&"core/community.rs".to_string()));
    }
}
