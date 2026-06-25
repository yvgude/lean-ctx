//! `ctx_architecture` — Graph-based architecture analysis tool.
//!
//! Discovers module clusters, dependency layers, entrypoints, cycles,
//! and structural patterns from the Property Graph.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::core::property_graph::CodeGraph;
use crate::core::tokens::count_tokens;
use serde_json::{Value, json};

/// Dispatches architecture analysis actions (overview, clusters, layers, cycles, entrypoints, module).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutputFormat {
    Text,
    Json,
}

fn parse_format(format: Option<&str>) -> Result<OutputFormat, String> {
    let f = format.unwrap_or("text").trim().to_lowercase();
    match f.as_str() {
        "text" => Ok(OutputFormat::Text),
        "json" => Ok(OutputFormat::Json),
        _ => Err("Error: format must be text|json".to_string()),
    }
}

pub fn handle(action: &str, path: Option<&str>, root: &str, format: Option<&str>) -> String {
    let fmt = match parse_format(format) {
        Ok(f) => f,
        Err(e) => return e,
    };

    match action {
        "overview" => handle_overview(root, fmt),
        "clusters" => handle_clusters(root, fmt),
        "communities" => handle_communities(root, fmt),
        "layers" => handle_layers(root, fmt),
        "cycles" => handle_cycles(root, fmt),
        "entrypoints" => handle_entrypoints(root, fmt),
        "hotspots" => handle_hotspots(root, fmt),
        "health" => handle_health(root, fmt),
        "module" => handle_module(path, root, fmt),
        _ => "Unknown action. Use: overview, clusters, communities, layers, cycles, entrypoints, hotspots, health, module"
            .to_string(),
    }
}

fn open_graph(root: &str) -> Result<CodeGraph, String> {
    CodeGraph::open(root).map_err(|e| format!("Failed to open graph: {e}"))
}

struct GraphData {
    forward: HashMap<String, Vec<String>>,
    reverse: HashMap<String, Vec<String>>,
    all_files: HashSet<String>,
}

fn ensure_graph_built(root: &str) {
    let Ok(graph) = CodeGraph::open(root) else {
        return;
    };
    if graph.node_count().unwrap_or(0) == 0 {
        drop(graph);
        let result = crate::tools::ctx_impact::handle("build", None, root, None, None);
        tracing::info!(
            "Auto-built graph for architecture: {}",
            &result[..result.len().min(100)]
        );
    }
}

fn load_graph_data(graph: &CodeGraph) -> Result<GraphData, String> {
    let nodes = graph.node_count().map_err(|e| format!("{e}"))?;
    if nodes == 0 {
        return Err(
            "Graph is empty after auto-build. No supported source files found.".to_string(),
        );
    }

    let conn = &graph.connection();
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT n_src.file_path, n_tgt.file_path
         FROM edges e
         JOIN nodes n_src ON e.source_id = n_src.id
         JOIN nodes n_tgt ON e.target_id = n_tgt.id
         WHERE e.kind = 'imports'
           AND n_src.kind = 'file' AND n_tgt.kind = 'file'
           AND n_src.file_path != n_tgt.file_path",
        )
        .map_err(|e| format!("{e}"))?;

    let mut forward: HashMap<String, Vec<String>> = HashMap::new();
    let mut reverse: HashMap<String, Vec<String>> = HashMap::new();
    let mut all_files: HashSet<String> = HashSet::new();

    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| format!("{e}"))?;

    for row in rows {
        let (src, tgt) = row.map_err(|e| format!("{e}"))?;
        all_files.insert(src.clone());
        all_files.insert(tgt.clone());
        forward.entry(src.clone()).or_default().push(tgt.clone());
        reverse.entry(tgt).or_default().push(src);
    }

    let mut file_stmt = conn
        .prepare("SELECT DISTINCT file_path FROM nodes WHERE kind = 'file'")
        .map_err(|e| format!("{e}"))?;
    let file_rows = file_stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| format!("{e}"))?;
    for f in file_rows.flatten() {
        all_files.insert(f);
    }

    for deps in forward.values_mut() {
        deps.sort();
        deps.dedup();
    }
    for deps in reverse.values_mut() {
        deps.sort();
        deps.dedup();
    }

    Ok(GraphData {
        forward,
        reverse,
        all_files,
    })
}

fn handle_overview(root: &str, fmt: OutputFormat) -> String {
    ensure_graph_built(root);

    let graph = match open_graph(root) {
        Ok(g) => g,
        Err(e) => return e,
    };

    let data = match load_graph_data(&graph) {
        Ok(d) => d,
        Err(e) => return e,
    };

    let clusters = compute_clusters(&data);
    let packages = compute_packages(&data);
    let entrypoints = find_entrypoints(&data);

    // Hotspot computation (compact)
    let pr_input = crate::core::pagerank::PageRankInput {
        files: data.all_files.clone(),
        forward: data.forward.clone(),
    };
    let pagerank = crate::core::pagerank::compute(&pr_input, 0.85, 30);

    let mut hotspots: Vec<(String, f64, f64)> = pagerank
        .iter()
        .map(|(file, &rank)| {
            let in_edges = data.reverse.get(file).map_or(0, Vec::len);
            let out_edges = data.forward.get(file).map_or(0, Vec::len);
            let score = rank * 0.4 + (in_edges + out_edges) as f64 * 0.01 * 0.6;
            (file.clone(), score, rank)
        })
        .collect();
    hotspots.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let files_total = data.all_files.len();
    let import_edges = data.forward.values().map(std::vec::Vec::len).sum::<usize>();

    let clusters_total = clusters.len();
    let clusters_limit = crate::core::budgets::ARCHITECTURE_OVERVIEW_CLUSTERS_LIMIT.max(1);
    let clusters_truncated = clusters_total > clusters_limit;

    let packages_total = packages.len();
    let packages_limit = crate::core::budgets::ARCHITECTURE_OVERVIEW_PACKAGES_LIMIT.max(1);
    let packages_truncated = packages_total > packages_limit;

    let entrypoints_total = entrypoints.len();
    let entrypoints_limit = crate::core::budgets::ARCHITECTURE_OVERVIEW_ENTRYPOINTS_LIMIT.max(1);
    let entrypoints_truncated = entrypoints_total > entrypoints_limit;

    let hotspots_limit = 5usize;

    match fmt {
        OutputFormat::Json => {
            let clusters_json: Vec<Value> = clusters
                .iter()
                .take(clusters_limit)
                .map(|c| {
                    json!({
                        "name": c.name,
                        "members": c.members.len(),
                        "cohesion": (c.cohesion * 1000.0).round() / 1000.0
                    })
                })
                .collect();

            let packages_json: Vec<Value> = packages
                .iter()
                .take(packages_limit)
                .map(|p| {
                    json!({
                        "name": p.name,
                        "node_count": p.node_count,
                        "fan_in": p.fan_in,
                        "fan_out": p.fan_out
                    })
                })
                .collect();

            let entrypoints_json: Vec<Value> = entrypoints
                .iter()
                .take(entrypoints_limit)
                .map(|ep| {
                    let imports = data.forward.get(ep).map_or(0, std::vec::Vec::len);
                    json!({ "file": ep, "imports": imports })
                })
                .collect();

            let hotspots_json: Vec<Value> = hotspots
                .iter()
                .take(hotspots_limit)
                .map(|(file, score, rank)| {
                    json!({
                        "file": file,
                        "score": (score * 1000.0).round() / 1000.0,
                        "rank": (rank * 10000.0).round() / 10000.0
                    })
                })
                .collect();

            let v = json!({
                "files": files_total,
                "edges": import_edges,
                "clusters": clusters_json,
                "packages": packages_json,
                "entrypoints": entrypoints_json,
                "hotspots": hotspots_json
            });
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
        }
        OutputFormat::Text => {
            let mut result = format!(
                "Architecture Overview ({files_total} files, {import_edges} import edges)
"
            );

            result.push_str(&format!("
Clusters: {clusters_total}
"));
            for cluster in clusters.iter().take(clusters_limit) {
                result.push_str(&format!(
                    "  {:<30} {:>4} members  cohesion {:.0}%\n",
                    cluster.name,
                    cluster.members.len(),
                    cluster.cohesion * 100.0
                ));
            }
            if clusters_truncated {
                result.push_str(&format!(
                    "  ... +{} more\n",
                    clusters_total - clusters_limit
                ));
            }

            result.push_str(&format!("
Packages: {packages_total}
"));
            for pkg in packages.iter().take(packages_limit) {
                result.push_str(&format!(
                    "  {:<30} {:>4} files  fan_in={:<3} fan_out={:<3}\n",
                    pkg.name, pkg.node_count, pkg.fan_in, pkg.fan_out
                ));
            }
            if packages_truncated {
                result.push_str(&format!("  ... +{} more\n", packages_total - packages_limit));
            }

            result.push_str(&format!("
Entrypoints: {entrypoints_total}
"));
            for ep in entrypoints.iter().take(entrypoints_limit) {
                let dep_count = data.forward.get(ep).map_or(0, std::vec::Vec::len);
                result.push_str(&format!("  {ep} (imports {dep_count} files)\n"));
            }
            if entrypoints_truncated {
                result.push_str(&format!(
                    "  ... +{} more\n",
                    entrypoints_total - entrypoints_limit
                ));
            }

            result.push_str(&format!("
Hotspots (top {}):\n", hotspots_limit));
            for (file, score, _rank) in hotspots.iter().take(hotspots_limit) {
                result.push_str(&format!("  {score:.3}  {file}\n"));
            }
            if hotspots.len() > hotspots_limit {
                result.push_str(&format!("  ... +{} more\n", hotspots.len() - hotspots_limit));
            }

            let tokens = count_tokens(&result);
            format!("{result}[ctx_architecture: {tokens} tok]")
        }
    }
}

fn handle_clusters(root: &str, fmt: OutputFormat) -> String {
    ensure_graph_built(root);
    let graph = match open_graph(root) {
        Ok(g) => g,
        Err(e) => return e,
    };

    let data = match load_graph_data(&graph) {
        Ok(d) => d,
        Err(e) => return e,
    };

    let clusters = compute_clusters(&data);
    let total = clusters.len();
    let limit = crate::core::budgets::ARCHITECTURE_CLUSTERS_LIMIT.max(1);
    let file_limit = crate::core::budgets::ARCHITECTURE_CLUSTER_FILES_LIMIT.max(1);
    let truncated = total > limit;

    match fmt {
        OutputFormat::Json => {
            let items: Vec<Value> = clusters
                .iter()
                .take(limit)
                .map(|c| {
                    let members_total = c.members.len();
                    let members_truncated = members_total > file_limit;
                    let mut members = c.members.clone();
                    if members_truncated {
                        members.truncate(file_limit);
                    }
                    json!({
                        "name": c.name,
                        "files": members,
                        "file_count": members_total,
                        "cohesion": (c.cohesion * 1000.0).round() / 1000.0,
                        "files_truncated": members_truncated
                    })
                })
                .collect();
            let v = json!({
                "total": total,
                "clusters": items,
                "truncated": truncated
            });
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
        }
        OutputFormat::Text => {
            let mut result = format!("Module Clusters ({total}):\n");

            for (i, cluster) in clusters.iter().take(limit).enumerate() {
                result.push_str(&format!(
                    "\n#{} — {} ({} members, cohesion {:.0}%)\n",
                    i + 1,
                    cluster.name,
                    cluster.members.len(),
                    cluster.cohesion * 100.0
                ));
                for file in cluster.members.iter().take(file_limit) {
                    result.push_str(&format!("  {file}\n"));
                }
                if cluster.members.len() > file_limit {
                    result.push_str(&format!(
                        "  ... +{} more\n",
                        cluster.members.len() - file_limit
                    ));
                }
            }
            if truncated {
                result.push_str(&format!("\n... +{} more clusters\n", total - limit));
            }

            let tokens = count_tokens(&result);
            format!("{result}[ctx_architecture clusters: {tokens} tok]")
        }
    }
}

fn handle_communities(root: &str, fmt: OutputFormat) -> String {
    ensure_graph_built(root);
    let graph = match open_graph(root) {
        Ok(g) => g,
        Err(e) => return e,
    };

    let result = crate::core::community::detect_communities_stable(graph.connection(), root);

    match fmt {
        OutputFormat::Json => {
            let comms: Vec<Value> = result
                .communities
                .iter()
                .take(30)
                .map(|c| {
                    json!({
                        "id": c.id,
                        "file_count": c.files.len(),
                        "files": c.files.iter().take(20).collect::<Vec<_>>(),
                        "internal_edges": c.internal_edges,
                        "external_edges": c.external_edges,
                        "cohesion": (c.cohesion * 100.0).round() / 100.0,
                    })
                })
                .collect();
            let v = json!({
                "modularity": (result.modularity * 1000.0).round() / 1000.0,
                "node_count": result.node_count,
                "edge_count": result.edge_count,
                "community_count": result.communities.len(),
                "communities": comms
            });
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
        }
        OutputFormat::Text => {
            let mut out = format!(
                "Community Detection (Louvain) — {} communities, modularity {:.3}\n\n",
                result.communities.len(),
                result.modularity
            );
            for c in result.communities.iter().take(20) {
                out.push_str(&format!(
                    "  Community #{}: {} files, cohesion {:.0}%, {} internal / {} external edges\n",
                    c.id,
                    c.files.len(),
                    c.cohesion * 100.0,
                    c.internal_edges,
                    c.external_edges
                ));
                for f in c.files.iter().take(10) {
                    out.push_str(&format!("    {f}\n"));
                }
                if c.files.len() > 10 {
                    out.push_str(&format!("    ... +{} more\n", c.files.len() - 10));
                }
            }
            if result.communities.len() > 20 {
                out.push_str(&format!(
                    "\n  ... +{} more communities\n",
                    result.communities.len() - 20
                ));
            }
            let tokens = count_tokens(&out);
            format!("{out}\n[ctx_architecture communities: {tokens} tok]")
        }
    }
}

fn handle_layers(root: &str, fmt: OutputFormat) -> String {
    ensure_graph_built(root);
    let graph = match open_graph(root) {
        Ok(g) => g,
        Err(e) => return e,
    };

    let data = match load_graph_data(&graph) {
        Ok(d) => d,
        Err(e) => return e,
    };

    let layers = compute_layers(&data);
    let total = layers.len();
    let limit = crate::core::budgets::ARCHITECTURE_LAYERS_LIMIT.max(1);
    let file_limit = crate::core::budgets::ARCHITECTURE_LAYER_FILES_LIMIT.max(1);
    let truncated = total > limit;

    match fmt {
        OutputFormat::Json => {
            let items: Vec<Value> = layers
                .iter()
                .take(limit)
                .map(|l| {
                    let files_total = l.files.len();
                    let files_truncated = files_total > file_limit;
                    let mut files = l.files.clone();
                    if files_truncated {
                        files.truncate(file_limit);
                    }
                    json!({
                        "depth": l.depth,
                        "file_count": files_total,
                        "files": files,
                        "files_truncated": files_truncated
                    })
                })
                .collect();
            let v = json!({
                "total": total,
                "layers": items,
                "truncated": truncated
            });
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
        }
        OutputFormat::Text => {
            let mut result = format!("Dependency Layers ({total}):\n");

            for layer in layers.iter().take(limit) {
                result.push_str(&format!(
                    "\nLayer {} ({} files):\n",
                    layer.depth,
                    layer.files.len()
                ));
                for file in layer.files.iter().take(file_limit) {
                    result.push_str(&format!("  {file}\n"));
                }
                if layer.files.len() > file_limit {
                    result.push_str(&format!("  ... +{} more\n", layer.files.len() - file_limit));
                }
            }
            if truncated {
                result.push_str(&format!("\n... +{} more layers\n", total - limit));
            }

            let tokens = count_tokens(&result);
            format!("{result}[ctx_architecture layers: {tokens} tok]")
        }
    }
}

fn handle_cycles(root: &str, fmt: OutputFormat) -> String {
    ensure_graph_built(root);
    let graph = match open_graph(root) {
        Ok(g) => g,
        Err(e) => return e,
    };

    let data = match load_graph_data(&graph) {
        Ok(d) => d,
        Err(e) => return e,
    };

    let cycles = find_cycles(&data);
    if cycles.is_empty() {
        return match fmt {
            OutputFormat::Json => {
                let v = json!({
                    "total": 0,
                    "cycles": []
                });
                serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
            }
            OutputFormat::Text => "No dependency cycles found.".to_string(),
        };
    }

    let total = cycles.len();
    let limit = crate::core::budgets::ARCHITECTURE_CYCLES_LIMIT.max(1);
    let truncated = total > limit;

    match fmt {
        OutputFormat::Json => {
            let items: Vec<Value> = cycles
                .iter()
                .take(limit)
                .map(|c| json!({ "path": c, "hops": c.len().saturating_sub(1) }))
                .collect();
            let v = json!({
                "total": total,
                "cycles": items,
                "truncated": truncated
            });
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
        }
        OutputFormat::Text => {
            let mut result = format!("Dependency Cycles ({total}):\n");
            for (i, cycle) in cycles.iter().take(limit).enumerate() {
                result.push_str(&format!("\n#{}: {}\n", i + 1, cycle.join(" -> ")));
            }
            if truncated {
                result.push_str(&format!("\n... +{} more cycles\n", total - limit));
            }

            let tokens = count_tokens(&result);
            format!("{result}[ctx_architecture cycles: {tokens} tok]")
        }
    }
}

fn handle_entrypoints(root: &str, fmt: OutputFormat) -> String {
    ensure_graph_built(root);
    let graph = match open_graph(root) {
        Ok(g) => g,
        Err(e) => return e,
    };

    let data = match load_graph_data(&graph) {
        Ok(d) => d,
        Err(e) => return e,
    };

    let entrypoints = find_entrypoints(&data);
    let total = entrypoints.len();
    let limit = crate::core::budgets::ARCHITECTURE_ENTRYPOINTS_LIMIT.max(1);
    let truncated = total > limit;

    match fmt {
        OutputFormat::Json => {
            let items: Vec<Value> = entrypoints
                .iter()
                .take(limit)
                .map(|ep| {
                    let imports = data.forward.get(ep).map_or(0, std::vec::Vec::len);
                    json!({ "file": ep, "imports": imports })
                })
                .collect();
            let v = json!({
                "total": total,
                "entrypoints": items,
                "truncated": truncated
            });
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
        }
        OutputFormat::Text => {
            let mut result = format!("Entrypoints ({total} — files with no dependents):\n");
            for ep in entrypoints.iter().take(limit) {
                let dep_count = data.forward.get(ep).map_or(0, std::vec::Vec::len);
                result.push_str(&format!("  {ep} (imports {dep_count} files)\n"));
            }
            if truncated {
                result.push_str(&format!("  ... +{} more\n", total - limit));
            }

            let tokens = count_tokens(&result);
            format!("{result}[ctx_architecture entrypoints: {tokens} tok]")
        }
    }
}

fn handle_hotspots(root: &str, fmt: OutputFormat) -> String {
    ensure_graph_built(root);
    let graph = match open_graph(root) {
        Ok(g) => g,
        Err(e) => return e,
    };

    let data = match load_graph_data(&graph) {
        Ok(d) => d,
        Err(e) => return e,
    };

    let pr_input = crate::core::pagerank::PageRankInput {
        files: data.all_files.clone(),
        forward: data.forward.clone(),
    };
    let pagerank = crate::core::pagerank::compute(&pr_input, 0.85, 30);
    let cfg = crate::core::smells::SmellConfig::default();
    let findings = crate::core::smells::scan_all(graph.connection(), &cfg);

    let mut smell_count: HashMap<String, usize> = HashMap::new();
    for f in &findings {
        *smell_count.entry(f.file_path.clone()).or_default() += 1;
    }

    let mut hotspots: Vec<(String, f64, f64, usize, usize)> = pagerank
        .iter()
        .map(|(file, &rank)| {
            let in_edges = data.reverse.get(file).map_or(0, Vec::len);
            let out_edges = data.forward.get(file).map_or(0, Vec::len);
            let smells = smell_count.get(file).copied().unwrap_or(0);
            let score = rank * 0.4
                + (in_edges + out_edges) as f64 * 0.01 * 0.3
                + smells as f64 * 0.05 * 0.3;
            (file.clone(), score, rank, in_edges + out_edges, smells)
        })
        .collect();

    hotspots.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let limit = 30;
    match fmt {
        OutputFormat::Json => {
            let items: Vec<Value> = hotspots
                .iter()
                .take(limit)
                .map(|(file, score, rank, edges, smells)| {
                    json!({
                        "file": file,
                        "score": (score * 1000.0).round() / 1000.0,
                        "pagerank": (rank * 10000.0).round() / 10000.0,
                        "edges": edges,
                        "smells": smells
                    })
                })
                .collect();
            let v = json!({
                "total_files": data.all_files.len(),
                "hotspots": items
            });
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
        }
        OutputFormat::Text => {
            let mut result = format!(
                "Hotspots ({} files analyzed)\n\n  {:<50} {:>8} {:>8} {:>6} {:>6}\n",
                data.all_files.len(),
                "File",
                "Score",
                "PageRank",
                "Edges",
                "Smells"
            );
            result.push_str(&format!("  {}\n", "-".repeat(82)));
            for (file, score, rank, edges, smells) in hotspots.iter().take(limit) {
                let display = if file.len() > 48 {
                    // Suffix cut must land on a char boundary — multibyte
                    // paths panic on byte-indexed slicing (GitHub #386).
                    let mut start = file.len() - 45;
                    while start < file.len() && !file.is_char_boundary(start) {
                        start += 1;
                    }
                    format!("...{}", &file[start..])
                } else {
                    file.clone()
                };
                result.push_str(&format!(
                    "  {display:<50} {score:>8.3} {rank:>8.4} {edges:>6} {smells:>6}\n"
                ));
            }
            if hotspots.len() > limit {
                result.push_str(&format!("\n  ... +{} more\n", hotspots.len() - limit));
            }
            let tokens = count_tokens(&result);
            format!("{result}\n[ctx_architecture hotspots: {tokens} tok]")
        }
    }
}

fn handle_health(root: &str, fmt: OutputFormat) -> String {
    ensure_graph_built(root);
    let graph = match open_graph(root) {
        Ok(g) => g,
        Err(e) => return e,
    };

    let data = match load_graph_data(&graph) {
        Ok(d) => d,
        Err(e) => return e,
    };

    let communities = crate::core::community::detect_communities_stable(graph.connection(), root);
    let cfg = crate::core::smells::SmellConfig::default();
    let findings = crate::core::smells::scan_all(graph.connection(), &cfg);
    let summary = crate::core::smells::summarize(&findings);
    let cycles = find_cycles(&data);
    let layers = compute_layers(&data);

    let total_smells: usize = summary.iter().map(|s| s.findings).sum();
    let files = data.all_files.len();
    let edges = data.forward.values().map(Vec::len).sum::<usize>();

    let smell_density = if files > 0 {
        total_smells as f64 / files as f64
    } else {
        0.0
    };
    let avg_cohesion = if communities.communities.is_empty() {
        0.0
    } else {
        communities
            .communities
            .iter()
            .map(|c| c.cohesion)
            .sum::<f64>()
            / communities.communities.len() as f64
    };

    let health_score = compute_health_score(
        smell_density,
        avg_cohesion,
        communities.modularity,
        cycles.len(),
        files,
    );

    let grade = match health_score {
        s if s >= 90.0 => "A",
        s if s >= 80.0 => "B",
        s if s >= 65.0 => "C",
        s if s >= 50.0 => "D",
        _ => "F",
    };

    match fmt {
        OutputFormat::Json => {
            let smell_items: Vec<Value> = summary
                .iter()
                .filter(|s| s.findings > 0)
                .map(|s| json!({"rule": s.rule, "findings": s.findings}))
                .collect();
            let v = json!({
                "health_score": (health_score * 10.0).round() / 10.0,
                "grade": grade,
                "files": files,
                "edges": edges,
                "total_smells": total_smells,
                "smell_density": (smell_density * 100.0).round() / 100.0,
                "modularity": (communities.modularity * 1000.0).round() / 1000.0,
                "avg_cohesion": (avg_cohesion * 100.0).round() / 100.0,
                "communities": communities.communities.len(),
                "cycles": cycles.len(),
                "layers": layers.len(),
                "smells_by_rule": smell_items
            });
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
        }
        OutputFormat::Text => {
            let mut result = format!(
                "Architecture Health Report\n\n  Score:       {health_score:.0}/100 (Grade: {grade})\n  Files:       {files}\n  Edges:       {edges}\n"
            );
            result.push_str(&format!(
                "  Communities: {} (modularity {:.3}, avg cohesion {:.0}%)\n",
                communities.communities.len(),
                communities.modularity,
                avg_cohesion * 100.0
            ));
            result.push_str(&format!(
                "  Cycles:      {}\n  Layers:      {}\n  Smells:      {} (density {:.2}/file)\n",
                cycles.len(),
                layers.len(),
                total_smells,
                smell_density
            ));

            if total_smells > 0 {
                result.push_str("\n  Smell breakdown:\n");
                for s in &summary {
                    if s.findings > 0 {
                        result.push_str(&format!("    {:<25} {:>3}\n", s.rule, s.findings));
                    }
                }
            }

            let tokens = count_tokens(&result);
            format!("{result}\n[ctx_architecture health: {tokens} tok]")
        }
    }
}

fn compute_health_score(
    smell_density: f64,
    avg_cohesion: f64,
    modularity: f64,
    cycle_count: usize,
    file_count: usize,
) -> f64 {
    let smell_penalty = (smell_density * 10.0).min(30.0);
    let cohesion_bonus = avg_cohesion * 20.0;
    let modularity_bonus = modularity.max(0.0) * 30.0;
    let cycle_penalty = (cycle_count as f64 * 5.0).min(20.0);
    let size_factor = if file_count > 1000 { 0.95 } else { 1.0 };

    let raw =
        (50.0 + cohesion_bonus + modularity_bonus - smell_penalty - cycle_penalty) * size_factor;
    raw.clamp(0.0, 100.0)
}

fn handle_module(path: Option<&str>, root: &str, fmt: OutputFormat) -> String {
    let Some(target) = path else {
        return "path is required for 'module' action".to_string();
    };

    ensure_graph_built(root);
    let graph = match open_graph(root) {
        Ok(g) => g,
        Err(e) => return e,
    };

    let data = match load_graph_data(&graph) {
        Ok(d) => d,
        Err(e) => return e,
    };

    let canon_root = crate::core::pathutil::safe_canonicalize(std::path::Path::new(root))
        .map_or_else(|_| root.to_string(), |p| p.to_string_lossy().to_string());
    let canon_target = crate::core::pathutil::safe_canonicalize(std::path::Path::new(target))
        .map_or_else(|_| target.to_string(), |p| p.to_string_lossy().to_string());
    let root_slash = if canon_root.ends_with('/') {
        canon_root.clone()
    } else {
        format!("{canon_root}/")
    };
    let rel = canon_target
        .strip_prefix(&root_slash)
        .or_else(|| canon_target.strip_prefix(&canon_root))
        .unwrap_or(&canon_target)
        .trim_start_matches('/');

    let prefix = if rel.contains('/') {
        rel.rsplitn(2, '/').last().unwrap_or(rel)
    } else {
        rel
    };

    let mut module_files: Vec<String> = data
        .all_files
        .iter()
        .filter(|f| f.starts_with(prefix))
        .cloned()
        .collect();
    module_files.sort();

    if module_files.is_empty() {
        return format!("No files found in module path '{prefix}'");
    }

    let file_set: HashSet<&str> = module_files
        .iter()
        .map(std::string::String::as_str)
        .collect();

    let mut internal_edges = 0;
    let mut external_imports: Vec<String> = Vec::new();
    let mut external_dependents: Vec<String> = Vec::new();

    for file in &module_files {
        if let Some(deps) = data.forward.get(file) {
            for dep in deps {
                if file_set.contains(dep.as_str()) {
                    internal_edges += 1;
                } else {
                    external_imports.push(format!("{file} -> {dep}"));
                }
            }
        }
        if let Some(revs) = data.reverse.get(file) {
            for rev in revs {
                if !file_set.contains(rev.as_str()) {
                    external_dependents.push(format!("{rev} -> {file}"));
                }
            }
        }
    }

    external_imports.sort();
    external_imports.dedup();
    external_dependents.sort();
    external_dependents.dedup();

    let files_total = module_files.len();
    let file_limit = crate::core::budgets::ARCHITECTURE_MODULE_FILES_LIMIT.max(1);
    let files_truncated = files_total > file_limit;

    match fmt {
        OutputFormat::Json => {
            let files: Vec<String> = module_files.iter().take(file_limit).cloned().collect();

            let ext_limit = 50usize;
            let ext_imports_total = external_imports.len();
            let ext_dependents_total = external_dependents.len();
            let imports_truncated = ext_imports_total > ext_limit;
            let dependents_truncated = ext_dependents_total > ext_limit;
            let imports: Vec<String> = external_imports.iter().take(ext_limit).cloned().collect();
            let dependents: Vec<String> = external_dependents
                .iter()
                .take(ext_limit)
                .cloned()
                .collect();

            let v = json!({
                "module_prefix": prefix,
                "file_count": files_total,
                "internal_edges": internal_edges,
                "files": files,
                "files_truncated": files_truncated,
                "external_imports_total": ext_imports_total,
                "external_imports": imports,
                "external_imports_truncated": imports_truncated,
                "external_dependents_total": ext_dependents_total,
                "external_dependents": dependents,
                "external_dependents_truncated": dependents_truncated
            });
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
        }
        OutputFormat::Text => {
            let mut result = format!(
                "Module '{prefix}' ({files_total} files, {internal_edges} internal edges)\n"
            );

            result.push_str("\nFiles:\n");
            for f in module_files.iter().take(file_limit) {
                result.push_str(&format!("  {f}\n"));
            }
            if files_truncated {
                result.push_str(&format!("  ... +{} more\n", files_total - file_limit));
            }

            if !external_imports.is_empty() {
                result.push_str(&format!(
                    "\nExternal imports ({}):\n",
                    external_imports.len()
                ));
                for imp in external_imports.iter().take(15) {
                    result.push_str(&format!("  {imp}\n"));
                }
                if external_imports.len() > 15 {
                    result.push_str(&format!("  ... +{} more\n", external_imports.len() - 15));
                }
            }

            if !external_dependents.is_empty() {
                result.push_str(&format!(
                    "\nExternal dependents ({}):\n",
                    external_dependents.len()
                ));
                for dep in external_dependents.iter().take(15) {
                    result.push_str(&format!("  {dep}\n"));
                }
                if external_dependents.len() > 15 {
                    result.push_str(&format!("  ... +{} more\n", external_dependents.len() - 15));
                }
            }

            let tokens = count_tokens(&result);
            format!("{result}[ctx_architecture module: {tokens} tok]")
        }
    }
}

// ---------------------------------------------------------------------------
// Algorithms
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Cluster {
    name: String,
    members: Vec<String>,
    cohesion: f64,
}

#[derive(Debug)]
struct Package {
    name: String,
    node_count: usize,
    fan_in: usize,
    fan_out: usize,
}

fn compute_clusters(data: &GraphData) -> Vec<Cluster> {
    let mut dir_groups: HashMap<String, Vec<String>> = HashMap::new();
    for file in &data.all_files {
        let dir = file.rsplitn(2, '/').last().unwrap_or("").to_string();
        dir_groups.entry(dir).or_default().push(file.clone());
    }

    let mut clusters: Vec<Cluster> = Vec::new();
    for (dir_name, files) in &dir_groups {
        if files.len() < 2 {
            continue;
        }
        let file_set: HashSet<&str> = files.iter().map(std::string::String::as_str).collect();
        let mut internal = 0;
        let mut external = 0;
        for file in files {
            if let Some(deps) = data.forward.get(file) {
                for dep in deps {
                    if file_set.contains(dep.as_str()) {
                        internal += 1;
                    } else {
                        external += 1;
                    }
                }
            }
        }

        let total = internal + external;
        let cohesion = if total > 0 {
            internal as f64 / total as f64
        } else {
            1.0
        };

        let mut sorted = files.clone();
        sorted.sort();
        clusters.push(Cluster {
            name: dir_name.clone(),
            members: sorted,
            cohesion,
        });
    }

    clusters.sort_by(|a, b| {
        b.members
            .len()
            .cmp(&a.members.len())
            .then_with(|| a.members[0].cmp(&b.members[0]))
    });
    clusters
}

fn compute_packages(data: &GraphData) -> Vec<Package> {
    // Group files by parent directory
    let mut dir_groups: HashMap<String, Vec<String>> = HashMap::new();
    for file in &data.all_files {
        let dir = file.rsplitn(2, '/').last().unwrap_or("").to_string();
        dir_groups.entry(dir).or_default().push(file.clone());
    }

    // Map each file to its package directory
    let file_to_pkg: HashMap<&str, &str> = data
        .all_files
        .iter()
        .map(|f| {
            let dir = f.rsplitn(2, '/').last().unwrap_or("");
            (f.as_str(), dir)
        })
        .collect();

    let mut packages: Vec<Package> = Vec::new();
    for (pkg_name, files) in &dir_groups {
        let mut fan_in_pkgs: HashSet<&str> = HashSet::new();
        let mut fan_out_pkgs: HashSet<&str> = HashSet::new();

        for file in files {
            // Fan-out: which packages does this package import from?
            if let Some(deps) = data.forward.get(file) {
                for dep in deps {
                    if let Some(&dep_pkg) = file_to_pkg.get(dep.as_str()) {
                        if dep_pkg != pkg_name.as_str() {
                            fan_out_pkgs.insert(dep_pkg);
                        }
                    }
                }
            }

            // Fan-in: which packages import from this package?
            if let Some(dependents) = data.reverse.get(file) {
                for dep in dependents {
                    if let Some(&dep_pkg) = file_to_pkg.get(dep.as_str()) {
                        if dep_pkg != pkg_name.as_str() {
                            fan_in_pkgs.insert(dep_pkg);
                        }
                    }
                }
            }
        }

        packages.push(Package {
            name: pkg_name.clone(),
            node_count: files.len(),
            fan_in: fan_in_pkgs.len(),
            fan_out: fan_out_pkgs.len(),
        });
    }

    packages.sort_by(|a, b| {
        b.node_count
            .cmp(&a.node_count)
            .then_with(|| a.name.cmp(&b.name))
    });
    packages
}

struct Layer {
    depth: usize,
    files: Vec<String>,
}

fn compute_layers(data: &GraphData) -> Vec<Layer> {
    let leaf_files: HashSet<&String> = data
        .all_files
        .iter()
        .filter(|f| data.forward.get(*f).is_none_or(std::vec::Vec::is_empty))
        .collect();

    let mut depth_map: HashMap<String, usize> = HashMap::new();
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();

    for leaf in &leaf_files {
        depth_map.insert((*leaf).clone(), 0);
        queue.push_back(((*leaf).clone(), 0));
    }

    while let Some((file, depth)) = queue.pop_front() {
        if let Some(dependents) = data.reverse.get(&file) {
            for dep in dependents {
                let new_depth = depth + 1;
                let current = depth_map.get(dep).copied().unwrap_or(0);
                if new_depth > current {
                    depth_map.insert(dep.clone(), new_depth);
                    queue.push_back((dep.clone(), new_depth));
                }
            }
        }
    }

    for file in &data.all_files {
        depth_map.entry(file.clone()).or_insert(0);
    }

    let max_depth = depth_map.values().copied().max().unwrap_or(0);
    let mut layers: Vec<Layer> = Vec::new();
    for d in 0..=max_depth {
        let mut files: Vec<String> = depth_map
            .iter()
            .filter(|&(_, &depth)| depth == d)
            .map(|(f, _)| f.clone())
            .collect();
        if !files.is_empty() {
            files.sort();
            layers.push(Layer { depth: d, files });
        }
    }

    layers
}

fn find_entrypoints(data: &GraphData) -> Vec<String> {
    let mut entrypoints: Vec<String> = data
        .all_files
        .iter()
        .filter(|f| !data.reverse.contains_key(*f))
        .cloned()
        .collect();
    entrypoints.sort();
    entrypoints
}

fn find_cycles(data: &GraphData) -> Vec<Vec<String>> {
    let mut cycles: Vec<Vec<String>> = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();

    let mut starts: Vec<&String> = data.all_files.iter().collect();
    starts.sort();
    for start in starts {
        if visited.contains(start) {
            continue;
        }

        let mut stack: Vec<String> = Vec::new();
        let mut on_stack: HashSet<String> = HashSet::new();
        dfs_cycles(
            start,
            &data.forward,
            &mut stack,
            &mut on_stack,
            &mut visited,
            &mut cycles,
        );
    }

    cycles.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    cycles.truncate(crate::core::budgets::ARCHITECTURE_CYCLES_LIMIT.max(1));
    cycles
}

fn dfs_cycles(
    node: &str,
    graph: &HashMap<String, Vec<String>>,
    stack: &mut Vec<String>,
    on_stack: &mut HashSet<String>,
    visited: &mut HashSet<String>,
    cycles: &mut Vec<Vec<String>>,
) {
    if on_stack.contains(node) {
        let cycle_start = stack.iter().position(|n| n == node).unwrap_or(0);
        let mut cycle: Vec<String> = stack[cycle_start..].to_vec();
        cycle.push(node.to_string());
        cycles.push(cycle);
        return;
    }

    if visited.contains(node) {
        return;
    }

    on_stack.insert(node.to_string());
    stack.push(node.to_string());

    if let Some(deps) = graph.get(node) {
        for dep in deps {
            dfs_cycles(dep, graph, stack, on_stack, visited, cycles);
        }
    }

    stack.pop();
    on_stack.remove(node);
    visited.insert(node.to_string());
}



#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entrypoints_no_dependents() {
        let mut forward: HashMap<String, Vec<String>> = HashMap::new();
        forward.insert("main.rs".to_string(), vec!["lib.rs".to_string()]);

        let all_files: HashSet<String> = ["main.rs", "lib.rs"]
            .iter()
            .map(std::string::ToString::to_string)
            .collect();

        let data = GraphData {
            forward,
            reverse: {
                let mut r = HashMap::new();
                r.insert("lib.rs".to_string(), vec!["main.rs".to_string()]);
                r
            },
            all_files,
        };

        let eps = find_entrypoints(&data);
        assert_eq!(eps, vec!["main.rs"]);
    }

    #[test]
    fn layers_simple_chain() {
        let mut forward: HashMap<String, Vec<String>> = HashMap::new();
        forward.insert("a.rs".to_string(), vec!["b.rs".to_string()]);
        forward.insert("b.rs".to_string(), vec!["c.rs".to_string()]);

        let mut reverse: HashMap<String, Vec<String>> = HashMap::new();
        reverse.insert("b.rs".to_string(), vec!["a.rs".to_string()]);
        reverse.insert("c.rs".to_string(), vec!["b.rs".to_string()]);

        let all_files: HashSet<String> = ["a.rs", "b.rs", "c.rs"]
            .iter()
            .map(std::string::ToString::to_string)
            .collect();

        let data = GraphData {
            forward,
            reverse,
            all_files,
        };

        let layers = compute_layers(&data);
        assert!(layers.len() >= 2);

        let layer0 = layers.iter().find(|l| l.depth == 0).unwrap();
        assert!(layer0.files.contains(&"c.rs".to_string()));

        let layer2 = layers.iter().find(|l| l.depth == 2).unwrap();
        assert!(layer2.files.contains(&"a.rs".to_string()));
    }

    #[test]
    fn cycles_detection() {
        let mut forward: HashMap<String, Vec<String>> = HashMap::new();
        forward.insert("a.rs".to_string(), vec!["b.rs".to_string()]);
        forward.insert("b.rs".to_string(), vec!["a.rs".to_string()]);

        let all_files: HashSet<String> = ["a.rs", "b.rs"]
            .iter()
            .map(std::string::ToString::to_string)
            .collect();

        let data = GraphData {
            forward,
            reverse: HashMap::new(),
            all_files,
        };

        let cycles = find_cycles(&data);
        assert!(!cycles.is_empty());
    }

    #[test]
    fn handle_unknown() {
        let result = handle("invalid", None, "/tmp", None);
        assert!(result.contains("Unknown action"));
    }
}
