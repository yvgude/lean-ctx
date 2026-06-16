use crate::dashboard::routes::helpers::detect_project_root_for_dashboard;

pub(super) fn get_route(
    path: &str,
    query_str: &str,
) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/heatmap" => Some(heatmap()),
        "/api/graph" => Some(graph()),
        "/api/graph/enrich" => Some(enrich()),
        "/api/graph/stats" => Some(stats()),
        "/api/graph-files" => Some(graph_files()),
        "/api/routes" => Some(routes(query_str)),
        _ => None,
    }
}

fn heatmap() -> (&'static str, &'static str, String) {
    let project_root = detect_project_root_for_dashboard();
    let Some(open) = crate::core::graph_provider::open_or_build(&project_root) else {
        return ("200 OK", "application/json", "[]".to_string());
    };
    let entries = build_heatmap_json(&open.provider);
    ("200 OK", "application/json", entries)
}

fn graph() -> (&'static str, &'static str, String) {
    let root = detect_project_root_for_dashboard();
    let Some(open) = crate::core::graph_provider::open_or_build(&root) else {
        return (
            "200 OK",
            "application/json",
            "{\"error\":\"no graph\"}".to_string(),
        );
    };
    let gp = &open.provider;

    // Community assignment (stable ids from the hardened Leiden engine). Shared
    // with the call-graph tab via the same provider, so colours stay consistent.
    let community = crate::core::community::detect_communities_for_provider(gp, &root);
    let community_map = community.assignment_min_size(2);
    let community_count = community
        .communities
        .iter()
        .filter(|c| c.files.len() >= 2)
        .count();

    let all_edges = gp.edges();
    let mut edge_stats: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for edge in &all_edges {
        *edge_stats.entry(edge.kind.as_str()).or_default() += 1;
    }
    let connected: std::collections::HashSet<&str> = all_edges
        .iter()
        .flat_map(|e| [e.from.as_str(), e.to.as_str()])
        .collect();
    let file_count = gp.file_count();
    let isolated_count = file_count - connected.len().min(file_count);

    // Graphify-style static analyses over the *real* directed dependency edges
    // (import / reexport) — God-Nodes (most connected) and import cycles.
    let god_nodes = crate::core::graph_analysis::compute_god_nodes(&all_edges, 12);
    let import_cycles = crate::core::graph_analysis::find_import_cycles(&all_edges, 20);
    let bridges = crate::core::graph_analysis::compute_bridge_centrality(&all_edges, 10);
    let surprising_connections =
        crate::core::graph_analysis::find_surprising_connections(&all_edges, &community_map, 10);

    // Per-community cohesion (module quality): internal vs external edge ratio,
    // already computed by the community engine. Surfaced for the dashboard.
    let mut community_cohesion: Vec<serde_json::Value> = community
        .communities
        .iter()
        .filter(|c| c.files.len() >= 2)
        .map(|c| {
            serde_json::json!({
                "id": c.id,
                "files": c.files.len(),
                "cohesion": (c.cohesion * 1000.0).round() / 1000.0,
                "internal_edges": c.internal_edges,
                "external_edges": c.external_edges,
            })
        })
        .collect();
    community_cohesion.sort_by(|a, b| {
        b["files"]
            .as_u64()
            .unwrap_or(0)
            .cmp(&a["files"].as_u64().unwrap_or(0))
    });
    community_cohesion.truncate(12);

    let files: Vec<serde_json::Value> = gp
        .file_paths()
        .iter()
        .filter_map(|p| {
            gp.get_file_entry(p).map(|f| {
                serde_json::json!({
                    "path": f.path,
                    "language": f.language,
                    "token_count": f.token_count,
                    "line_count": f.line_count,
                    "exports": f.exports,
                    "summary": f.summary,
                    "community": community_map.get(&f.path),
                })
            })
        })
        .collect();

    let mut edges: Vec<serde_json::Value> = all_edges
        .iter()
        .map(|e| {
            serde_json::json!({
                "from": e.from,
                "to": e.to,
                "kind": e.kind,
                "weight": e.weight,
                "confidence": (crate::core::graph_analysis::edge_confidence(&e.kind, e.weight)
                    * 1000.0)
                    .round()
                    / 1000.0,
            })
        })
        .collect();

    // Traversal overlay (#289): co-access edges learned from real sessions,
    // drawn on top of the static graph. Restricted to files present in the graph
    // (skip deleted/unindexed files) and capped so the view stays readable. The
    // static analyses above intentionally run on structural edges only, so this
    // overlay never skews god-nodes / cycles / orphan-rate.
    let co_access_edges: Vec<(String, String, f64)> = {
        let in_graph: std::collections::HashSet<String> = gp.file_paths().into_iter().collect();
        crate::core::cooccurrence::export_edges(&root, 0.15, 250)
            .into_iter()
            .filter(|(a, b, _)| in_graph.contains(a) && in_graph.contains(b))
            .collect()
    };
    if !co_access_edges.is_empty() {
        *edge_stats.entry("co_access").or_default() += co_access_edges.len();
        edges.extend(co_access_edges.iter().map(|(from, to, weight)| {
            serde_json::json!({
                "from": from,
                "to": to,
                "kind": "co_access",
                "weight": weight,
                "confidence": (crate::core::graph_analysis::edge_confidence("co_access", *weight)
                    * 1000.0)
                    .round()
                    / 1000.0,
            })
        }));
    }

    // When the graph is empty, explain *why*: a project built mostly from
    // graph-unsupported languages (e.g. Lua/Luau, #360) would otherwise leave the
    // dashboard stuck on an unhelpful "run index build" hint that never helps.
    let graph_support = if files.is_empty() {
        let unsupported: Vec<serde_json::Value> =
            crate::core::language_capabilities::scan_unsupported_source_languages(&root, 5000)
                .into_iter()
                .map(|(language, file_count)| {
                    serde_json::json!({ "language": language, "files": file_count })
                })
                .collect();
        Some(serde_json::json!({
            "supported_languages": crate::core::language_capabilities::graph_supported_language_names(),
            "unsupported_present": unsupported,
        }))
    } else {
        None
    };

    // Realized per-language coverage when the provider is index-backed (real
    // symbol/import counts); fall back to capability-only flags otherwise.
    let language_matrix = match gp.as_graph_index() {
        Some(index) => super::capability_matrix::realized_from_index(index, None),
        None => crate::core::language_capabilities::language_capability_matrix(gp.file_paths()),
    };

    let val = serde_json::json!({
        "project_root": super::project_basename(&root),
        "project_root_full": root,
        "files": files,
        "edges": edges,
        "edge_stats": edge_stats,
        "isolated_node_count": isolated_count,
        "orphan_rate": if file_count > 0 {
            (isolated_count as f64 / file_count as f64 * 100.0).round() / 100.0
        } else {
            0.0
        },
        "graph_support": graph_support,
        "language_matrix": language_matrix,
        "community_count": community_count,
        "god_nodes": god_nodes,
        "import_cycles": import_cycles,
        "bridge_nodes": bridges.nodes,
        "betweenness_sampled": bridges.sampled,
        "surprising_connections": surprising_connections,
        "community_cohesion": community_cohesion,
    });
    let json = serde_json::to_string(&val)
        .unwrap_or_else(|_| "{\"error\":\"failed to serialize\"}".to_string());
    ("200 OK", "application/json", json)
}

fn enrich() -> (&'static str, &'static str, String) {
    let root = detect_project_root_for_dashboard();
    let project_path = std::path::Path::new(&root);
    let result = match crate::core::property_graph::CodeGraph::open(&root) {
        Ok(graph) => match crate::core::graph_enricher::enrich_graph(&graph, project_path, 500) {
            Ok(stats) => {
                let nc = graph.node_count().unwrap_or(0);
                let ec = graph.edge_count().unwrap_or(0);
                serde_json::json!({
                    "commits_indexed": stats.commits_indexed,
                    "tests_indexed": stats.tests_indexed,
                    "knowledge_indexed": stats.knowledge_indexed,
                    "edges_created": stats.edges_created,
                    "total_nodes": nc,
                    "total_edges": ec,
                })
            }
            Err(e) => {
                tracing::warn!("graph enrich error: {e}");
                serde_json::json!({"error": "enrichment_failed"})
            }
        },
        Err(e) => {
            tracing::warn!("graph open error: {e}");
            serde_json::json!({"error": "graph_unavailable"})
        }
    };
    let json = serde_json::to_string(&result).unwrap_or_else(|_| "{}".to_string());
    ("200 OK", "application/json", json)
}

fn stats() -> (&'static str, &'static str, String) {
    let root = detect_project_root_for_dashboard();
    let result = match crate::core::graph_provider::open_best_effort(&root) {
        Some(open) => {
            let nc = open.provider.node_count().unwrap_or(0);
            let ec = open.provider.edge_count().unwrap_or(0);
            match open.source {
                crate::core::graph_provider::GraphProviderSource::PropertyGraph => {
                    serde_json::json!({
                        "source": "property_graph",
                        "node_count": nc,
                        "edge_count": ec,
                    })
                }
                crate::core::graph_provider::GraphProviderSource::GraphIndex => {
                    serde_json::json!({
                        "source": "graph_index",
                        "node_count": nc,
                        "edge_count": ec,
                    })
                }
            }
        }
        _ => {
            serde_json::json!({
                "source": "none",
                "node_count": 0,
                "edge_count": 0,
            })
        }
    };
    let json = serde_json::to_string(&result).unwrap_or_else(|_| "{}".to_string());
    ("200 OK", "application/json", json)
}

fn graph_files() -> (&'static str, &'static str, String) {
    let root = detect_project_root_for_dashboard();
    let Some(open) = crate::core::graph_provider::open_or_build(&root) else {
        return ("200 OK", "application/json", "{\"files\":[]}".to_string());
    };
    let gp = &open.provider;
    let mut files: Vec<serde_json::Value> = gp
        .file_paths()
        .iter()
        .filter_map(|p| {
            gp.get_file_entry(p).map(|f| {
                serde_json::json!({
                    "path": f.path,
                    "language": f.language,
                    "token_count": f.token_count,
                    "line_count": f.line_count,
                })
            })
        })
        .collect();
    files.sort_by(|a, b| {
        b["token_count"]
            .as_u64()
            .unwrap_or(0)
            .cmp(&a["token_count"].as_u64().unwrap_or(0))
    });
    files.truncate(500);
    let json = serde_json::json!({ "files": files, "project_root_full": root });
    let out = serde_json::to_string(&json).unwrap_or_else(|_| "{\"files\":[]}".to_string());
    ("200 OK", "application/json", out)
}

fn routes(_query_str: &str) -> (&'static str, &'static str, String) {
    let root = detect_project_root_for_dashboard();
    let Some(open) = crate::core::graph_provider::open_or_build(&root) else {
        return ("200 OK", "application/json", "{\"routes\":[]}".to_string());
    };
    let gp = &open.provider;
    let file_paths = gp.file_paths();

    let files_map: std::collections::HashMap<String, crate::core::graph_index::FileEntry> =
        file_paths
            .iter()
            .filter_map(|p| {
                gp.get_file_entry(p).map(|f| {
                    (
                        p.clone(),
                        crate::core::graph_index::FileEntry {
                            path: f.path,
                            hash: f.hash,
                            language: f.language,
                            line_count: f.line_count,
                            token_count: f.token_count,
                            exports: f.exports,
                            summary: f.summary,
                        },
                    )
                })
            })
            .collect();

    let routes = crate::core::route_extractor::extract_routes_from_project(&root, &files_map);
    let route_candidate_count = file_paths
        .iter()
        .filter(|p| {
            std::path::Path::new(p.as_str())
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| {
                    matches!(e, "js" | "ts" | "py" | "rs" | "java" | "rb" | "go" | "kt")
                })
        })
        .count();
    let payload = serde_json::json!({
        "routes": routes,
        "indexed_file_count": file_paths.len(),
        "route_candidate_count": route_candidate_count,
    });
    let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{\"routes\":[]}".to_string());
    ("200 OK", "application/json", json)
}

fn build_heatmap_json(gp: &crate::core::graph_provider::GraphProvider) -> String {
    let all_edges = gp.edges();
    let mut connection_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for edge in &all_edges {
        *connection_counts.entry(edge.from.clone()).or_default() += 1;
        *connection_counts.entry(edge.to.clone()).or_default() += 1;
    }

    let paths = gp.file_paths();
    let mut max_tokens = 1usize;
    for path in &paths {
        if let Some(entry) = gp.get_file_entry(path) {
            max_tokens = max_tokens.max(entry.token_count);
        }
    }
    let max_tokens = max_tokens as f64;
    let max_connections = connection_counts.values().max().copied().unwrap_or(1) as f64;

    let mut entries: Vec<serde_json::Value> = paths
        .iter()
        .filter_map(|p| {
            gp.get_file_entry(p).map(|f| {
                let connections = connection_counts.get(&f.path).copied().unwrap_or(0);
                let token_norm = f.token_count as f64 / max_tokens;
                let conn_norm = connections as f64 / max_connections;
                let heat = token_norm * 0.4 + conn_norm * 0.6;
                serde_json::json!({
                    "path": f.path,
                    "tokens": f.token_count,
                    "connections": connections,
                    "language": f.language,
                    "heat": (heat * 100.0).round() / 100.0,
                })
            })
        })
        .collect();

    entries.sort_by(|a, b| {
        b["heat"]
            .as_f64()
            .unwrap_or(0.0)
            .partial_cmp(&a["heat"].as_f64().unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    serde_json::to_string(&entries).unwrap_or_else(|_| "[]".to_string())
}
