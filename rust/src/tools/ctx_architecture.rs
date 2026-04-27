//! `ctx_architecture` — Graph-based architecture analysis tool.
//!
//! Discovers module clusters, dependency layers, entrypoints, cycles,
//! and structural patterns from the Property Graph.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

use crate::core::property_graph::CodeGraph;
use crate::core::tokens::count_tokens;

/// Dispatches architecture analysis actions (overview, clusters, layers, cycles, entrypoints, module).
pub fn handle(action: &str, path: Option<&str>, root: &str) -> String {
    match action {
        "overview" => handle_overview(root),
        "clusters" => handle_clusters(root),
        "layers" => handle_layers(root),
        "cycles" => handle_cycles(root),
        "entrypoints" => handle_entrypoints(root),
        "module" => handle_module(path, root),
        _ => "Unknown action. Use: overview, clusters, layers, cycles, entrypoints, module"
            .to_string(),
    }
}

fn open_graph(root: &str) -> Result<CodeGraph, String> {
    CodeGraph::open(Path::new(root)).map_err(|e| format!("Failed to open graph: {e}"))
}

struct GraphData {
    forward: HashMap<String, Vec<String>>,
    reverse: HashMap<String, Vec<String>>,
    all_files: HashSet<String>,
}

fn ensure_graph_built(root: &str) {
    let Ok(graph) = CodeGraph::open(Path::new(root)) else {
        return;
    };
    if graph.node_count().unwrap_or(0) == 0 {
        drop(graph);
        let result = crate::tools::ctx_impact::handle("build", None, root, None);
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

    Ok(GraphData {
        forward,
        reverse,
        all_files,
    })
}

fn handle_overview(root: &str) -> String {
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
    let layers = compute_layers(&data);
    let entrypoints = find_entrypoints(&data);
    let cycles = find_cycles(&data);

    let mut result = format!(
        "Architecture Overview ({} files, {} import edges)\n",
        data.all_files.len(),
        data.forward.values().map(std::vec::Vec::len).sum::<usize>()
    );

    result.push_str(&format!("\nClusters: {}\n", clusters.len()));
    for (i, cluster) in clusters.iter().enumerate().take(5) {
        let dir = common_prefix(&cluster.files);
        result.push_str(&format!(
            "  #{}: {} files ({})\n",
            i + 1,
            cluster.files.len(),
            dir
        ));
    }

    result.push_str(&format!("\nLayers: {}\n", layers.len()));
    for layer in &layers {
        result.push_str(&format!(
            "  L{}: {} files\n",
            layer.depth,
            layer.files.len()
        ));
    }

    result.push_str(&format!("\nEntrypoints: {}\n", entrypoints.len()));
    for ep in entrypoints.iter().take(10) {
        result.push_str(&format!("  {ep}\n"));
    }

    result.push_str(&format!("\nCycles: {}\n", cycles.len()));
    for cycle in cycles.iter().take(5) {
        result.push_str(&format!("  {}\n", cycle.join(" -> ")));
    }

    let tokens = count_tokens(&result);
    format!("{result}[ctx_architecture: {tokens} tok]")
}

fn handle_clusters(root: &str) -> String {
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
    let mut result = format!("Module Clusters ({}):\n", clusters.len());

    for (i, cluster) in clusters.iter().enumerate() {
        let dir = common_prefix(&cluster.files);
        result.push_str(&format!(
            "\n#{} — {} ({} files, {} internal edges)\n",
            i + 1,
            dir,
            cluster.files.len(),
            cluster.internal_edges
        ));
        for file in cluster.files.iter().take(15) {
            result.push_str(&format!("  {file}\n"));
        }
        if cluster.files.len() > 15 {
            result.push_str(&format!("  ... +{} more\n", cluster.files.len() - 15));
        }
    }

    let tokens = count_tokens(&result);
    format!("{result}[ctx_architecture clusters: {tokens} tok]")
}

fn handle_layers(root: &str) -> String {
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
    let mut result = format!("Dependency Layers ({}):\n", layers.len());

    for layer in &layers {
        result.push_str(&format!(
            "\nLayer {} ({} files):\n",
            layer.depth,
            layer.files.len()
        ));
        for file in layer.files.iter().take(20) {
            result.push_str(&format!("  {file}\n"));
        }
        if layer.files.len() > 20 {
            result.push_str(&format!("  ... +{} more\n", layer.files.len() - 20));
        }
    }

    let tokens = count_tokens(&result);
    format!("{result}[ctx_architecture layers: {tokens} tok]")
}

fn handle_cycles(root: &str) -> String {
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
        return "No dependency cycles found.".to_string();
    }

    let mut result = format!("Dependency Cycles ({}):\n", cycles.len());
    for (i, cycle) in cycles.iter().enumerate() {
        result.push_str(&format!("\n#{}: {}\n", i + 1, cycle.join(" -> ")));
    }

    let tokens = count_tokens(&result);
    format!("{result}[ctx_architecture cycles: {tokens} tok]")
}

fn handle_entrypoints(root: &str) -> String {
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
    let mut result = format!(
        "Entrypoints ({} — files with no dependents):\n",
        entrypoints.len()
    );
    for ep in &entrypoints {
        let dep_count = data.forward.get(ep).map_or(0, std::vec::Vec::len);
        result.push_str(&format!("  {ep} (imports {dep_count} files)\n"));
    }

    let tokens = count_tokens(&result);
    format!("{result}[ctx_architecture entrypoints: {tokens} tok]")
}

fn handle_module(path: Option<&str>, root: &str) -> String {
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

    let module_files: Vec<&String> = data
        .all_files
        .iter()
        .filter(|f| f.starts_with(prefix))
        .collect();

    if module_files.is_empty() {
        return format!("No files found in module path '{prefix}'");
    }

    let file_set: HashSet<&str> = module_files.iter().map(|f| f.as_str()).collect();

    let mut internal_edges = 0;
    let mut external_imports: Vec<String> = Vec::new();
    let mut external_dependents: Vec<String> = Vec::new();

    for file in &module_files {
        if let Some(deps) = data.forward.get(*file) {
            for dep in deps {
                if file_set.contains(dep.as_str()) {
                    internal_edges += 1;
                } else {
                    external_imports.push(format!("{file} -> {dep}"));
                }
            }
        }
        if let Some(revs) = data.reverse.get(*file) {
            for rev in revs {
                if !file_set.contains(rev.as_str()) {
                    external_dependents.push(format!("{rev} -> {file}"));
                }
            }
        }
    }

    let mut result = format!(
        "Module '{prefix}' ({} files, {} internal edges)\n",
        module_files.len(),
        internal_edges
    );

    result.push_str("\nFiles:\n");
    for f in &module_files {
        result.push_str(&format!("  {f}\n"));
    }

    if !external_imports.is_empty() {
        result.push_str(&format!(
            "\nExternal imports ({}):\n",
            external_imports.len()
        ));
        for imp in external_imports.iter().take(15) {
            result.push_str(&format!("  {imp}\n"));
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
    }

    let tokens = count_tokens(&result);
    format!("{result}[ctx_architecture module: {tokens} tok]")
}

// ---------------------------------------------------------------------------
// Algorithms
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Cluster {
    files: Vec<String>,
    internal_edges: usize,
}

fn compute_clusters(data: &GraphData) -> Vec<Cluster> {
    let mut dir_groups: HashMap<String, Vec<String>> = HashMap::new();
    for file in &data.all_files {
        let dir = file.rsplitn(2, '/').last().unwrap_or("").to_string();
        dir_groups.entry(dir).or_default().push(file.clone());
    }

    let mut clusters: Vec<Cluster> = Vec::new();
    for files in dir_groups.values() {
        if files.len() < 2 {
            continue;
        }
        let file_set: HashSet<&str> = files.iter().map(std::string::String::as_str).collect();
        let mut internal = 0;
        for file in files {
            if let Some(deps) = data.forward.get(file) {
                for dep in deps {
                    if file_set.contains(dep.as_str()) {
                        internal += 1;
                    }
                }
            }
        }

        let mut sorted = files.clone();
        sorted.sort();
        clusters.push(Cluster {
            files: sorted,
            internal_edges: internal,
        });
    }

    clusters.sort_by_key(|x| std::cmp::Reverse(x.files.len()));
    clusters
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
            .filter(|(_, &depth)| depth == d)
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

    for start in &data.all_files {
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

    cycles.sort_by_key(std::vec::Vec::len);
    cycles.truncate(20);
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

fn common_prefix(files: &[String]) -> String {
    if files.is_empty() {
        return String::new();
    }
    if files.len() == 1 {
        return files[0]
            .rsplitn(2, '/')
            .last()
            .unwrap_or(&files[0])
            .to_string();
    }

    let parts: Vec<Vec<&str>> = files.iter().map(|f| f.split('/').collect()).collect();
    let min_len = parts.iter().map(std::vec::Vec::len).min().unwrap_or(0);

    let mut common = Vec::new();
    for i in 0..min_len {
        let segment = parts[0][i];
        if parts.iter().all(|p| p[i] == segment) {
            common.push(segment);
        } else {
            break;
        }
    }

    if common.is_empty() {
        "(root)".to_string()
    } else {
        common.join("/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_prefix_single() {
        let files = vec!["src/core/cache.rs".to_string()];
        assert_eq!(common_prefix(&files), "src/core");
    }

    #[test]
    fn common_prefix_multiple() {
        let files = vec![
            "src/core/cache.rs".to_string(),
            "src/core/config.rs".to_string(),
            "src/core/session.rs".to_string(),
        ];
        assert_eq!(common_prefix(&files), "src/core");
    }

    #[test]
    fn common_prefix_different_dirs() {
        let files = vec![
            "src/tools/ctx_read.rs".to_string(),
            "src/core/cache.rs".to_string(),
        ];
        assert_eq!(common_prefix(&files), "src");
    }

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
        let result = handle("invalid", None, "/tmp");
        assert!(result.contains("Unknown action"));
    }
}
