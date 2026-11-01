//! `/api/architecture` — a Markdown architecture report built from the *same*
//! real graph signals the Dependencies tab uses (communities, god-nodes, import
//! cycles, bridges, cohesion). No mock data: every section reflects the indexed
//! project. The dashboard renders the Markdown and offers a `.md` download.

use crate::dashboard::routes::helpers::detect_project_root_for_dashboard;
use std::collections::HashMap;
use std::fmt::Write as _;

pub(super) fn get_route(
    path: &str,
    _query_str: &str,
) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/architecture" => Some(architecture()),
        _ => None,
    }
}

fn architecture() -> (&'static str, &'static str, String) {
    let root = detect_project_root_for_dashboard();
    let project = super::project_basename(&root);
    let open = match crate::core::graph_coordinator::get_or_start_build(&root) {
        Ok(open) => open,
        // The building answer, plus the report fields a reader renders.
        Err(progress) => {
            let mut val = serde_json::to_value(&progress).unwrap_or_default();
            if let Some(obj) = val.as_object_mut() {
                obj.insert("project".into(), project.clone().into());
                obj.insert(
                    "markdown".into(),
                    format!("# Architecture Report — {project}\n\n_The project index is being built; this report follows when it is ready._\n").into(),
                );
            }
            return ("202 Accepted", "application/json", val.to_string());
        }
    };
    let gp = &open.provider;

    // Memoize the (expensive) report keyed by a cheap graph fingerprint, so
    // repeated dashboard loads don't re-run community detection, betweenness,
    // cycle search and surprising-connection scans on an unchanged graph.
    let fingerprint = super::analysis_cache::fingerprint(gp);
    let cache_key = format!("architecture:{root}");
    let json = super::analysis_cache::cached_or_compute(&cache_key, &fingerprint, || {
        compute_report(gp, &root, &project)
    });
    ("200 OK", "application/json", json)
}

fn compute_report(
    gp: &crate::core::graph_provider::GraphProvider,
    root: &str,
    project: &str,
) -> String {
    let community = crate::core::community::detect_communities_for_provider(gp, root);
    let community_map = community.assignment_min_size(2);
    let all_edges = gp.edges();
    let file_count = gp.file_count();

    let mut edge_stats: HashMap<&str, usize> = HashMap::new();
    for e in &all_edges {
        *edge_stats.entry(e.kind.as_str()).or_default() += 1;
    }
    let connected: std::collections::HashSet<&str> = all_edges
        .iter()
        .flat_map(|e| [e.from.as_str(), e.to.as_str()])
        .collect();
    let isolated = file_count - connected.len().min(file_count);

    let god_nodes = crate::core::graph_analysis::compute_god_nodes(&all_edges, 10);
    let import_cycles = crate::core::graph_analysis::find_import_cycles(&all_edges, 20);
    let bridges = crate::core::graph_analysis::compute_bridge_centrality(&all_edges, 10);
    let surprising =
        crate::core::graph_analysis::find_surprising_connections(&all_edges, &community_map, 10);

    // Language breakdown from file entries.
    let mut lang_counts: HashMap<String, usize> = HashMap::new();
    for p in gp.file_paths() {
        if let Some(f) = gp.get_file_entry(&p) {
            *lang_counts.entry(f.language.clone()).or_default() += 1;
        }
    }
    let mut langs: Vec<(String, usize)> = lang_counts.into_iter().collect();
    langs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let community_count = community
        .communities
        .iter()
        .filter(|c| c.files.len() >= 2)
        .count();
    let mut comms: Vec<&crate::core::community::Community> = community
        .communities
        .iter()
        .filter(|c| c.files.len() >= 2)
        .collect();
    comms.sort_by_key(|c| std::cmp::Reverse(c.files.len()));

    let md = render_markdown(
        project,
        file_count,
        &all_edges,
        &edge_stats,
        isolated,
        community_count,
        community.modularity,
        &langs,
        &comms,
        &god_nodes,
        &bridges,
        &import_cycles,
        &surprising,
    );

    let val = serde_json::json!({
        "project": project,
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "file_count": file_count,
        "edge_count": all_edges.len(),
        "community_count": community_count,
        "markdown": md,
    });
    val.to_string()
}

#[allow(clippy::too_many_arguments)]
fn render_markdown(
    project: &str,
    file_count: usize,
    all_edges: &[crate::core::graph_provider::EdgeInfo],
    edge_stats: &HashMap<&str, usize>,
    isolated: usize,
    community_count: usize,
    modularity: f64,
    langs: &[(String, usize)],
    comms: &[&crate::core::community::Community],
    god_nodes: &[crate::core::graph_analysis::GodNode],
    bridges: &crate::core::graph_analysis::BridgeCentrality,
    import_cycles: &[crate::core::graph_analysis::ImportCycle],
    surprising: &[crate::core::graph_analysis::SurprisingConnection],
) -> String {
    let base = |p: &str| -> String { p.rsplit('/').next().unwrap_or(p).to_string() };
    let orphan_rate = if file_count > 0 {
        (isolated as f64 / file_count as f64 * 1000.0).round() / 10.0
    } else {
        0.0
    };
    let dep_edges = edge_stats.get("import").copied().unwrap_or(0)
        + edge_stats.get("reexport").copied().unwrap_or(0);

    let mut md = String::new();
    let _ = writeln!(md, "# Architecture Report — {project}");
    let _ = writeln!(md);
    let _ = writeln!(
        md,
        "_{file_count} files · {} edges · {community_count} modules_",
        all_edges.len()
    );
    let _ = writeln!(md);

    // Overview.
    let _ = writeln!(md, "## Overview");
    let _ = writeln!(md);
    let _ = writeln!(md, "- **Files:** {file_count}");
    let _ = writeln!(md, "- **Dependency edges (import/reexport):** {dep_edges}");
    let _ = writeln!(md, "- **Total edges:** {}", all_edges.len());
    let _ = writeln!(
        md,
        "- **Modules (communities ≥2):** {community_count} · modularity {modularity:.3}"
    );
    let _ = writeln!(
        md,
        "- **Isolated files:** {isolated} ({orphan_rate}% orphan rate)"
    );
    let _ = writeln!(md);

    // Edge composition.
    if !edge_stats.is_empty() {
        let _ = writeln!(md, "### Edge composition");
        let _ = writeln!(md);
        let _ = writeln!(md, "| Kind | Count | Confidence |");
        let _ = writeln!(md, "|------|------:|-----------:|");
        let mut kinds: Vec<(&&str, &usize)> = edge_stats.iter().collect();
        kinds.sort_by(|a, b| b.1.cmp(a.1));
        for (kind, count) in kinds {
            let conf = crate::core::graph_analysis::edge_confidence(kind, 1.0);
            let _ = writeln!(md, "| {kind} | {count} | {conf:.2} |");
        }
        let _ = writeln!(md);
    }

    // Languages.
    if !langs.is_empty() {
        let _ = writeln!(md, "## Languages");
        let _ = writeln!(md);
        let _ = writeln!(md, "| Language | Files |");
        let _ = writeln!(md, "|----------|------:|");
        for (lang, count) in langs.iter().take(15) {
            let _ = writeln!(md, "| {lang} | {count} |");
        }
        let _ = writeln!(md);
    }

    // Modules.
    if !comms.is_empty() {
        let _ = writeln!(md, "## Modules (communities)");
        let _ = writeln!(md);
        let _ = writeln!(
            md,
            "Largest cohesive groups of files. Cohesion = internal / total edges."
        );
        let _ = writeln!(md);
        let _ = writeln!(md, "| # | Files | Cohesion | Internal | External |");
        let _ = writeln!(md, "|---|------:|---------:|---------:|---------:|");
        for c in comms.iter().take(12) {
            let _ = writeln!(
                md,
                "| {} | {} | {:.2} | {} | {} |",
                c.id,
                c.files.len(),
                c.cohesion,
                c.internal_edges,
                c.external_edges
            );
        }
        let _ = writeln!(md);
    }

    // God-Nodes.
    if !god_nodes.is_empty() {
        let _ = writeln!(md, "## God-Nodes (most connected)");
        let _ = writeln!(md);
        let _ = writeln!(
            md,
            "Files with the highest dependency degree — likely refactor or review hotspots."
        );
        let _ = writeln!(md);
        let _ = writeln!(md, "| File | Degree | In | Out |");
        let _ = writeln!(md, "|------|-------:|---:|----:|");
        for g in god_nodes {
            let _ = writeln!(
                md,
                "| `{}` | {} | {} | {} |",
                base(&g.path),
                g.degree,
                g.in_degree,
                g.out_degree
            );
        }
        let _ = writeln!(md);
    }

    // Bridges.
    if !bridges.nodes.is_empty() {
        let _ = writeln!(md, "## Bridges (betweenness centrality)");
        let _ = writeln!(md);
        let _ = writeln!(
            md,
            "Files that sit on many shortest paths — changes here ripple widely."
        );
        if bridges.sampled {
            let _ = writeln!(md);
            let _ = writeln!(
                md,
                "_Estimated from a sample of {} of {} nodes (large graph); the \
                 relative ranking is preserved._",
                bridges.sources_used, bridges.total_nodes
            );
        }
        let _ = writeln!(md);
        let _ = writeln!(md, "| File | Betweenness |");
        let _ = writeln!(md, "|------|------------:|");
        for b in &bridges.nodes {
            let _ = writeln!(md, "| `{}` | {:.2} |", base(&b.path), b.betweenness);
        }
        let _ = writeln!(md);
    }

    // Import cycles.
    let _ = writeln!(md, "## Import cycles");
    let _ = writeln!(md);
    if import_cycles.is_empty() {
        let _ = writeln!(md, "None — the dependency graph is acyclic. ✓");
    } else {
        let _ = writeln!(md, "{} cycle(s) detected:", import_cycles.len());
        let _ = writeln!(md);
        for (i, c) in import_cycles.iter().enumerate() {
            let names: Vec<String> = c.files.iter().map(|f| base(f)).collect();
            let _ = writeln!(md, "{}. ({} files) {}", i + 1, c.size, names.join(" → "));
        }
    }
    let _ = writeln!(md);

    // Surprising connections.
    if !surprising.is_empty() {
        let _ = writeln!(md, "## Surprising connections");
        let _ = writeln!(md);
        let _ = writeln!(
            md,
            "Unexpected couplings (low neighbour overlap, often cross-module)."
        );
        let _ = writeln!(md);
        let _ = writeln!(md, "| A | B | Score |");
        let _ = writeln!(md, "|---|---|------:|");
        for s in surprising {
            let _ = writeln!(
                md,
                "| `{}` | `{}` | {:.2} |",
                base(&s.from),
                base(&s.to),
                s.score
            );
        }
        let _ = writeln!(md);
    }

    md
}
