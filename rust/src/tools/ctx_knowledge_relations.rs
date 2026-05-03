use crate::core::knowledge::ProjectKnowledge;
use crate::core::knowledge_relations::{
    format_mermaid, parse_node_ref, KnowledgeEdge, KnowledgeEdgeKind, KnowledgeNodeRef,
    KnowledgeRelationGraph,
};

fn load_policy_or_error() -> Result<crate::core::memory_policy::MemoryPolicy, String> {
    crate::core::config::Config::load()
        .memory_policy_effective()
        .map_err(|e| {
            let path = crate::core::config::Config::path().map_or_else(
                || "~/.lean-ctx/config.toml".to_string(),
                |p| p.display().to_string(),
            );
            format!("Error: invalid memory policy: {e}\nFix: edit {path}")
        })
}

fn ensure_current_fact_exists(knowledge: &ProjectKnowledge, node: &KnowledgeNodeRef) -> bool {
    knowledge
        .facts
        .iter()
        .any(|f| f.is_current() && f.category == node.category && f.key == node.key)
}

fn parse_kind_or_default(value: Option<&str>) -> Result<KnowledgeEdgeKind, String> {
    let kind_str = value.unwrap_or("related_to");
    KnowledgeEdgeKind::parse(kind_str).ok_or_else(|| {
        "Error: relation kind must be one of depends_on|related_to|supports|contradicts|supersedes"
            .to_string()
    })
}

fn parse_target_or_error(query: Option<&str>) -> Result<KnowledgeNodeRef, String> {
    let Some(q) = query else {
        return Err("Error: query is required and must be 'category/key'".to_string());
    };
    parse_node_ref(q).ok_or_else(|| "Error: query must be 'category/key'".to_string())
}

fn parse_direction(query: Option<&str>) -> &'static str {
    match query.unwrap_or("all").trim().to_lowercase().as_str() {
        "in" | "incoming" => "in",
        "out" | "outgoing" => "out",
        _ => "all",
    }
}

fn derived_supersedes_edges(
    knowledge: &ProjectKnowledge,
    focus: &KnowledgeNodeRef,
) -> Vec<KnowledgeEdge> {
    let mut out = Vec::new();
    let focus_id = focus.id();

    for f in knowledge.facts.iter().filter(|f| f.is_current()) {
        if f.category == focus.category && f.key == focus.key {
            if let Some(s) = &f.supersedes {
                if let Some(to) = parse_node_ref(s) {
                    out.push(KnowledgeEdge {
                        from: focus.clone(),
                        to,
                        kind: KnowledgeEdgeKind::Supersedes,
                        created_at: f.created_at,
                        last_seen: None,
                        count: 0,
                        source_session: f.source_session.clone(),
                    });
                }
            }
        } else if f.supersedes.as_deref() == Some(&focus_id) {
            out.push(KnowledgeEdge {
                from: KnowledgeNodeRef::new(&f.category, &f.key),
                to: focus.clone(),
                kind: KnowledgeEdgeKind::Supersedes,
                created_at: f.created_at,
                last_seen: None,
                count: 0,
                source_session: f.source_session.clone(),
            });
        }
    }

    out
}

pub fn handle_relate(
    project_root: &str,
    category: Option<&str>,
    key: Option<&str>,
    value: Option<&str>,
    query: Option<&str>,
    session_id: &str,
) -> String {
    let Some(cat) = category else {
        return "Error: category is required for relate".to_string();
    };
    let Some(k) = key else {
        return "Error: key is required for relate".to_string();
    };

    let from = KnowledgeNodeRef::new(cat, k);
    let to = match parse_target_or_error(query) {
        Ok(n) => n,
        Err(e) => return e,
    };
    let kind = match parse_kind_or_default(value) {
        Ok(k) => k,
        Err(e) => return e,
    };

    let policy = match load_policy_or_error() {
        Ok(p) => p,
        Err(e) => return e,
    };

    let knowledge = ProjectKnowledge::load_or_create(project_root);
    if !ensure_current_fact_exists(&knowledge, &from) {
        return format!(
            "Error: no current fact exists for [{}] {}. Use ctx_knowledge remember first.",
            from.category, from.key
        );
    }
    if !ensure_current_fact_exists(&knowledge, &to) {
        return format!(
            "Error: no current fact exists for [{}] {}. Use ctx_knowledge remember first.",
            to.category, to.key
        );
    }

    let mut graph = KnowledgeRelationGraph::load_or_create(&knowledge.project_hash);
    let created = graph.upsert_edge(from.clone(), to.clone(), kind, session_id);
    let max_edges = policy.knowledge.max_facts.saturating_mul(8);
    let capped = graph.enforce_cap(max_edges);

    match graph.save() {
        Ok(()) => {
            let verb = if created { "added" } else { "reinforced" };
            let mut out = format!(
                "Relation {verb}: {} -({})-> {}",
                from.id(),
                kind.as_str(),
                to.id()
            );
            if capped {
                out.push_str(&format!(" (note: capped to {max_edges} edges)"));
            }
            out
        }
        Err(e) => format!(
            "Relation recorded but save failed: {e} ({} -({})-> {})",
            from.id(),
            kind.as_str(),
            to.id()
        ),
    }
}

pub fn handle_unrelate(
    project_root: &str,
    category: Option<&str>,
    key: Option<&str>,
    value: Option<&str>,
    query: Option<&str>,
) -> String {
    let Some(cat) = category else {
        return "Error: category is required for unrelate".to_string();
    };
    let Some(k) = key else {
        return "Error: key is required for unrelate".to_string();
    };

    let from = KnowledgeNodeRef::new(cat, k);
    let to = match parse_target_or_error(query) {
        Ok(n) => n,
        Err(e) => return e,
    };
    let kind = if let Some(v) = value {
        match KnowledgeEdgeKind::parse(v) {
            Some(k) => Some(k),
            None => {
                return "Error: relation kind must be one of depends_on|related_to|supports|contradicts|supersedes".to_string();
            }
        }
    } else {
        None
    };

    let knowledge = ProjectKnowledge::load_or_create(project_root);
    let mut graph = KnowledgeRelationGraph::load_or_create(&knowledge.project_hash);
    let removed = graph.remove_edge(&from, &to, kind);

    if removed == 0 {
        return format!("No matching relation found: {} -> {}", from.id(), to.id());
    }

    match graph.save() {
        Ok(()) => format!("Relation removed ({removed}): {} -> {}", from.id(), to.id()),
        Err(e) => format!(
            "Relation removed ({removed}) but save failed: {e} ({} -> {})",
            from.id(),
            to.id()
        ),
    }
}

pub fn handle_relations(
    project_root: &str,
    category: Option<&str>,
    key: Option<&str>,
    value: Option<&str>,
    query: Option<&str>,
) -> String {
    let Some(cat) = category else {
        return "Error: category is required for relations".to_string();
    };
    let Some(k) = key else {
        return "Error: key is required for relations".to_string();
    };

    let focus = KnowledgeNodeRef::new(cat, k);
    let dir = parse_direction(query);
    let kind_filter = match value {
        Some(v) => match KnowledgeEdgeKind::parse(v) {
            Some(k) => Some(k),
            None => {
                return "Error: relation kind must be one of depends_on|related_to|supports|contradicts|supersedes".to_string();
            }
        },
        None => None,
    };

    let knowledge = ProjectKnowledge::load_or_create(project_root);
    let graph = KnowledgeRelationGraph::load_or_create(&knowledge.project_hash);

    let mut edges: Vec<&KnowledgeEdge> = graph
        .edges
        .iter()
        .filter(|e| match dir {
            "in" => e.to == focus,
            "out" => e.from == focus,
            _ => e.from == focus || e.to == focus,
        })
        .filter(|e| kind_filter.is_none_or(|k| e.kind == k))
        .collect();

    edges.sort_by(|a, b| {
        a.kind
            .as_str()
            .cmp(b.kind.as_str())
            .then_with(|| a.from.category.cmp(&b.from.category))
            .then_with(|| a.from.key.cmp(&b.from.key))
            .then_with(|| a.to.category.cmp(&b.to.category))
            .then_with(|| a.to.key.cmp(&b.to.key))
            .then_with(|| b.count.cmp(&a.count))
            .then_with(|| b.last_seen.cmp(&a.last_seen))
            .then_with(|| b.created_at.cmp(&a.created_at))
    });

    let derived = derived_supersedes_edges(&knowledge, &focus);
    let mut derived_filtered: Vec<KnowledgeEdge> = derived
        .into_iter()
        .filter(|e| match dir {
            "in" => e.to == focus,
            "out" => e.from == focus,
            _ => e.from == focus || e.to == focus,
        })
        .filter(|e| kind_filter.is_none_or(|k| e.kind == k))
        .collect();
    derived_filtered.sort_by(|a, b| {
        a.kind
            .as_str()
            .cmp(b.kind.as_str())
            .then_with(|| a.from.category.cmp(&b.from.category))
            .then_with(|| a.from.key.cmp(&b.from.key))
            .then_with(|| a.to.category.cmp(&b.to.category))
            .then_with(|| a.to.key.cmp(&b.to.key))
    });

    let mut seen = std::collections::HashSet::<(String, String, KnowledgeEdgeKind)>::new();
    for e in &edges {
        let _ = seen.insert((e.from.id(), e.to.id(), e.kind));
    }
    let derived_filtered: Vec<_> = derived_filtered
        .into_iter()
        .filter(|e| seen.insert((e.from.id(), e.to.id(), e.kind)))
        .collect();

    if edges.is_empty() && derived_filtered.is_empty() {
        return format!("No relations for {}.", focus.id());
    }

    let mut out = Vec::new();
    out.push(format!("Relations for {} (dir={dir}):", focus.id()));
    for e in edges {
        let arrow = if e.from == focus { "->" } else { "<-" };
        let other = if e.from == focus { &e.to } else { &e.from };
        out.push(format!(
            "  {arrow} {} {} (count={}, last_seen={})",
            e.kind.as_str(),
            other.id(),
            e.count.max(1),
            e.last_seen
                .map_or_else(|| "n/a".to_string(), |t| t.format("%Y-%m-%d").to_string(),)
        ));
    }
    for e in derived_filtered {
        let arrow = if e.from == focus { "->" } else { "<-" };
        let other = if e.from == focus { &e.to } else { &e.from };
        out.push(format!(
            "  {arrow} {} {} (derived)",
            e.kind.as_str(),
            other.id()
        ));
    }
    out.join("\n")
}

pub fn handle_relations_diagram(
    project_root: &str,
    category: Option<&str>,
    key: Option<&str>,
    value: Option<&str>,
    query: Option<&str>,
) -> String {
    let Some(cat) = category else {
        return "Error: category is required for relations_diagram".to_string();
    };
    let Some(k) = key else {
        return "Error: key is required for relations_diagram".to_string();
    };

    let focus = KnowledgeNodeRef::new(cat, k);
    let dir = parse_direction(query);
    let kind_filter = match value {
        Some(v) => match KnowledgeEdgeKind::parse(v) {
            Some(k) => Some(k),
            None => {
                return "Error: relation kind must be one of depends_on|related_to|supports|contradicts|supersedes".to_string();
            }
        },
        None => None,
    };

    let knowledge = ProjectKnowledge::load_or_create(project_root);
    let graph = KnowledgeRelationGraph::load_or_create(&knowledge.project_hash);

    let mut edges: Vec<KnowledgeEdge> = graph
        .edges
        .iter()
        .filter(|e| match dir {
            "in" => e.to == focus,
            "out" => e.from == focus,
            _ => e.from == focus || e.to == focus,
        })
        .filter(|e| kind_filter.is_none_or(|k| e.kind == k))
        .cloned()
        .collect();

    let derived = derived_supersedes_edges(&knowledge, &focus);
    let derived_filtered = derived
        .into_iter()
        .filter(|e| match dir {
            "in" => e.to == focus,
            "out" => e.from == focus,
            _ => e.from == focus || e.to == focus,
        })
        .filter(|e| kind_filter.is_none_or(|k| e.kind == k))
        .collect::<Vec<_>>();

    let mut seen = std::collections::HashSet::<(String, String, KnowledgeEdgeKind)>::new();
    edges.retain(|e| seen.insert((e.from.id(), e.to.id(), e.kind)));
    for e in derived_filtered {
        if seen.insert((e.from.id(), e.to.id(), e.kind)) {
            edges.push(e);
        }
    }

    edges.sort_by(|a, b| {
        a.kind
            .as_str()
            .cmp(b.kind.as_str())
            .then_with(|| a.from.category.cmp(&b.from.category))
            .then_with(|| a.from.key.cmp(&b.from.key))
            .then_with(|| a.to.category.cmp(&b.to.category))
            .then_with(|| a.to.key.cmp(&b.to.key))
    });

    format_mermaid(&edges)
}
