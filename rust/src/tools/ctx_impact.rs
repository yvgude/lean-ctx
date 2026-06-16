//! `ctx_impact` — Graph-based impact analysis tool.
//!
//! Uses the SQLite-backed Property Graph to answer: "What breaks when file X changes?"
//! Performs BFS traversal of reverse import edges to find all transitively affected files.

use crate::core::property_graph::{CodeGraph, DependencyChain, Edge, EdgeKind, ImpactResult, Node};
use crate::core::tokens::count_tokens;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Stdio;

/// Extensions whose files become Property Graph source nodes. Must stay a subset
/// of `language_capabilities::is_indexable_ext` and align with the deep-query
/// extractors (`deep_queries::{type_defs, calls}`) so each language contributes
/// real symbol/import/call structure rather than bare file nodes.
const GRAPH_SOURCE_EXTS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "gd", "cs",
];

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

pub fn handle(
    action: &str,
    path: Option<&str>,
    root: &str,
    depth: Option<usize>,
    format: Option<&str>,
) -> String {
    let fmt = match parse_format(format) {
        Ok(f) => f,
        Err(e) => return e,
    };

    match action {
        "analyze" => handle_analyze(path, root, depth.unwrap_or(5), fmt),
        "diff" => handle_diff(root, depth.unwrap_or(5), fmt),
        "chain" => handle_chain(path, root, fmt),
        "build" => handle_build(root, fmt),
        "update" => handle_update(root, fmt),
        "status" => handle_status(root, fmt),
        _ => "Unknown action. Use: analyze, diff, chain, build, status, update".to_string(),
    }
}

fn open_graph(root: &str) -> Result<CodeGraph, String> {
    CodeGraph::open(root).map_err(|e| format!("Failed to open graph: {e}"))
}

/// Open the property graph for a *query*, rebuilding first when it cannot be
/// trusted: either empty (never built) or produced by an engine older than
/// [`crate::core::property_graph::GRAPH_ENGINE_VERSION`] — e.g. an upgraded
/// install whose graph predates the C#/Java `type_ref` edges (GH #398). The
/// rebuild is one-shot and idempotent: a fresh build stamps the current engine
/// version, so a healthy graph is returned without rebuilding.
fn open_graph_fresh(root: &str) -> Result<CodeGraph, String> {
    let graph = open_graph(root)?;
    let empty = graph.node_count().unwrap_or(0) == 0;
    let outdated = !empty && crate::core::property_graph::engine_outdated(root);
    if empty || outdated {
        drop(graph);
        let build_result = handle_build(root, OutputFormat::Text);
        tracing::info!(
            "Rebuilt property graph before impact query ({}): {}",
            if empty { "empty" } else { "engine outdated" },
            &build_result[..build_result.len().min(100)]
        );
        return open_graph(root);
    }
    Ok(graph)
}

fn handle_analyze(path: Option<&str>, root: &str, max_depth: usize, fmt: OutputFormat) -> String {
    let Some(target) = path else {
        return "path is required for 'analyze' action".to_string();
    };

    let graph = match open_graph_fresh(root) {
        Ok(g) => g,
        Err(e) => return e,
    };

    let rel_target = graph_target_key(target, root);

    if graph.node_count().unwrap_or(0) == 0 {
        return "Graph is empty after auto-build. No supported source files found.".to_string();
    }

    let impact = match graph.impact_analysis(&rel_target, max_depth) {
        Ok(r) => r,
        Err(e) => return format!("Impact analysis failed: {e}"),
    };

    format_impact(&impact, &rel_target, root, fmt)
}

fn format_impact(impact: &ImpactResult, target: &str, root: &str, fmt: OutputFormat) -> String {
    let mut sorted = impact.affected_files.clone();
    sorted.sort();

    let total = sorted.len();
    let limit = crate::core::budgets::IMPACT_AFFECTED_FILES_LIMIT.max(1);
    let truncated = total > limit;
    if truncated {
        sorted.truncate(limit);
    }

    match fmt {
        OutputFormat::Json => {
            let v = json!({
                "schema_version": crate::core::contracts::GRAPH_REPRODUCIBILITY_V1_SCHEMA_VERSION,
                "tool": "ctx_impact",
                "action": "analyze",
                "project": project_meta(root),
                "graph": graph_summary(root),
                "graph_meta": crate::core::property_graph::load_meta(root),
                "target": target,
                "max_depth_reached": impact.max_depth_reached,
                "edges_traversed": impact.edges_traversed,
                "affected_files_total": total,
                "affected_files": sorted,
                "truncated": truncated
            });
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
        }
        OutputFormat::Text => {
            if total == 0 {
                let result =
                    format!("No files depend on {target} (leaf node in the dependency graph).");
                let tokens = count_tokens(&result);
                return format!("{result}\n[ctx_impact: {tokens} tok]");
            }

            let mut result = format!(
                "Impact of changing {target}: {total} affected files (depth: {}, edges traversed: {})\n",
                impact.max_depth_reached, impact.edges_traversed
            );

            for file in &sorted {
                result.push_str(&format!("  {file}\n"));
            }
            if truncated {
                result.push_str(&format!("  ... +{} more\n", total - limit));
            }

            let tokens = count_tokens(&result);
            format!("{result}[ctx_impact: {tokens} tok]")
        }
    }
}

fn handle_diff(root: &str, max_depth: usize, fmt: OutputFormat) -> String {
    let changed = git_changed_files(root);
    if changed.is_empty() {
        return match fmt {
            OutputFormat::Json => {
                let v = json!({
                    "tool": "ctx_impact",
                    "action": "diff",
                    "changed_files": [],
                    "blast_radius": [],
                    "total_affected": 0
                });
                serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
            }
            OutputFormat::Text => "No uncommitted changes found.".to_string(),
        };
    }

    let graph = match open_graph_fresh(root) {
        Ok(g) => g,
        Err(e) => return e,
    };

    compute_diff_impact(&graph, &changed, root, max_depth, fmt)
}

fn git_changed_files(root: &str) -> Vec<String> {
    let output = std::process::Command::new("git")
        .args(["diff", "--name-only", "HEAD"])
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();

    let mut files: BTreeSet<String> = BTreeSet::new();

    if let Ok(o) = output
        && o.status.success()
    {
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                files.insert(trimmed.to_string());
            }
        }
    }

    let staged = std::process::Command::new("git")
        .args(["diff", "--name-only", "--cached"])
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();

    if let Ok(o) = staged
        && o.status.success()
    {
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                files.insert(trimmed.to_string());
            }
        }
    }

    let untracked = std::process::Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard"])
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();

    if let Ok(o) = untracked
        && o.status.success()
    {
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                files.insert(trimmed.to_string());
            }
        }
    }

    files.into_iter().collect()
}

fn compute_diff_impact(
    graph: &CodeGraph,
    changed: &[String],
    root: &str,
    max_depth: usize,
    fmt: OutputFormat,
) -> String {
    let mut all_affected: BTreeSet<String> = BTreeSet::new();
    let mut per_file: Vec<(String, Vec<String>)> = Vec::new();

    for file in changed {
        let rel = graph_target_key(file, root);
        if let Ok(impact) = graph.impact_analysis(&rel, max_depth) {
            let mut affected: Vec<String> = impact
                .affected_files
                .into_iter()
                .filter(|f| !changed.contains(f))
                .collect();
            affected.sort();
            for a in &affected {
                all_affected.insert(a.clone());
            }
            if !affected.is_empty() {
                per_file.push((rel, affected));
            }
        }
    }

    match fmt {
        OutputFormat::Json => {
            let items: Vec<Value> = per_file
                .iter()
                .map(|(file, affected)| {
                    json!({
                        "changed_file": file,
                        "affected": affected,
                        "count": affected.len()
                    })
                })
                .collect();
            let v = json!({
                "tool": "ctx_impact",
                "action": "diff",
                "changed_files": changed,
                "blast_radius": items,
                "total_affected": all_affected.len()
            });
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
        }
        OutputFormat::Text => {
            let mut result = format!(
                "Diff Impact Analysis ({} changed files, {} blast radius)\n\n",
                changed.len(),
                all_affected.len()
            );
            result.push_str("Changed files:\n");
            for f in changed.iter().take(30) {
                result.push_str(&format!("  {f}\n"));
            }

            if !per_file.is_empty() {
                result.push_str("\nBlast radius:\n");
                for (file, affected) in per_file.iter().take(15) {
                    result.push_str(&format!("  {file} -> {} affected\n", affected.len()));
                    for a in affected.iter().take(10) {
                        result.push_str(&format!("    {a}\n"));
                    }
                    if affected.len() > 10 {
                        result.push_str(&format!("    ... +{} more\n", affected.len() - 10));
                    }
                }
            }

            let tokens = count_tokens(&result);
            format!("{result}\n[ctx_impact diff: {tokens} tok]")
        }
    }
}

fn handle_chain(path: Option<&str>, root: &str, fmt: OutputFormat) -> String {
    let Some(spec) = path else {
        return "path is required for 'chain' action (format: from_file->to_file)".to_string();
    };

    let (from, to) = match spec.split_once("->") {
        Some((f, t)) => (f.trim(), t.trim()),
        None => {
            return format!(
                "Invalid chain spec '{spec}'. Use format: from_file->to_file\n\
                 Example: src/server.rs->src/core/config.rs"
            );
        }
    };

    let graph = match open_graph_fresh(root) {
        Ok(g) => g,
        Err(e) => return e,
    };

    let rel_from = graph_target_key(from, root);
    let rel_to = graph_target_key(to, root);

    match graph.dependency_chain(&rel_from, &rel_to) {
        Ok(Some(chain)) => format_chain(&chain, root, fmt),
        Ok(None) => match fmt {
            OutputFormat::Json => {
                let v = json!({
                    "schema_version": crate::core::contracts::GRAPH_REPRODUCIBILITY_V1_SCHEMA_VERSION,
                    "tool": "ctx_impact",
                    "action": "chain",
                    "project": project_meta(root),
                    "graph": graph_summary(root),
                    "graph_meta": crate::core::property_graph::load_meta(root),
                    "from": rel_from,
                    "to": rel_to,
                    "found": false
                });
                serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
            }
            OutputFormat::Text => {
                let result = format!("No dependency path from {rel_from} to {rel_to}");
                let tokens = count_tokens(&result);
                format!("{result}\n[ctx_impact chain: {tokens} tok]")
            }
        },
        Err(e) => format!("Chain analysis failed: {e}"),
    }
}

fn format_chain(chain: &DependencyChain, root: &str, fmt: OutputFormat) -> String {
    match fmt {
        OutputFormat::Json => {
            let v = json!({
                "schema_version": crate::core::contracts::GRAPH_REPRODUCIBILITY_V1_SCHEMA_VERSION,
                "tool": "ctx_impact",
                "action": "chain",
                "project": project_meta(root),
                "graph": graph_summary(root),
                "graph_meta": crate::core::property_graph::load_meta(root),
                "found": true,
                "depth": chain.depth,
                "path": chain.path
            });
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
        }
        OutputFormat::Text => {
            let mut result = format!("Dependency chain (depth {}):\n", chain.depth);
            for (i, step) in chain.path.iter().enumerate() {
                if i > 0 {
                    result.push_str("  -> ");
                } else {
                    result.push_str("  ");
                }
                result.push_str(step);
                result.push('\n');
            }
            let tokens = count_tokens(&result);
            format!("{result}[ctx_impact chain: {tokens} tok]")
        }
    }
}

fn graph_target_key(path: &str, root: &str) -> String {
    let rel = crate::core::graph_index::graph_relative_key(path, root);
    let rel_key = crate::core::graph_index::graph_match_key(&rel);
    if rel_key.is_empty() {
        crate::core::graph_index::graph_match_key(path)
    } else {
        rel_key
    }
}

fn walk_supported_sources(root_path: &Path) -> (Vec<String>, Vec<(String, String, String)>) {
    let walker = ignore::WalkBuilder::new(root_path)
        .hidden(true)
        .git_ignore(true)
        .require_git(false)
        .filter_entry(crate::core::walk_filter::keep_entry)
        .build();

    let mut file_paths: Vec<String> = Vec::new();
    let mut file_contents: Vec<(String, String, String)> = Vec::new();

    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }

        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

        if !GRAPH_SOURCE_EXTS.contains(&ext) {
            continue;
        }

        // Canonical `/` separators: graph node keys must be platform-stable
        // so queries like `impact_analysis("Models/Engine.cs")` match the
        // same node on Windows (output determinism, #498).
        let rel_path = path
            .strip_prefix(root_path)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");

        file_paths.push(rel_path.clone());

        if let Ok(content) = std::fs::read_to_string(path) {
            file_contents.push((rel_path, content, ext.to_string()));
        }
    }

    file_paths.sort();
    file_paths.dedup();
    file_contents.sort_by(|a, b| a.0.cmp(&b.0));
    (file_paths, file_contents)
}

/// Per-file analysis borrowed from the walked contents: (path, content, ext, analysis).
type AnalyzedFile<'a> = (
    &'a str,
    &'a str,
    &'a str,
    crate::core::deep_queries::DeepAnalysis,
);

/// Definition sites per symbol name: `name -> [(file, line_start, line_end)]`.
type DefIndex = std::collections::HashMap<String, Vec<(String, usize, usize)>>;

/// Analyze every walked source file once (parallel) and build the global
/// symbol-definition index `type name -> [(file, line_start, line_end)]`.
/// Shared by full build and incremental update on both builder paths.
fn analyze_all(file_contents: &[(String, String, String)]) -> (Vec<AnalyzedFile<'_>>, DefIndex) {
    use rayon::prelude::*;
    let per_file: Vec<AnalyzedFile<'_>> = file_contents
        .par_iter()
        .map(|(p, c, e)| {
            (
                p.as_str(),
                c.as_str(),
                e.as_str(),
                crate::core::deep_queries::analyze(c.as_str(), e.as_str()),
            )
        })
        .collect();

    let mut def_index = DefIndex::new();
    for (p, _, _, analysis) in &per_file {
        for t in &analysis.types {
            def_index.entry(t.name.clone()).or_default().push((
                (*p).to_string(),
                t.line,
                t.end_line,
            ));
        }
    }

    (per_file, def_index)
}

/// Definition sites of types this file *uses* (field/param/base/generic/cast),
/// resolved against the project-wide definition index: `(defining_file,
/// type_name, line_start, line_end)`. This is what connects C#/Java
/// same-namespace consumers that have no import statement (GH #398).
///
/// A name defined in more than `MAX_DEF_SITES` files is considered too
/// generic to attribute (e.g. `Config` in a monorepo) and is skipped —
/// recall for the common case without flooding the graph with noise.
fn type_ref_targets(
    def_index: &DefIndex,
    type_uses: &[crate::core::deep_queries::TypeUse],
    rel_path: &str,
) -> Vec<(String, String, usize, usize)> {
    const MAX_DEF_SITES: usize = 3;

    let mut targets: Vec<(String, String, usize, usize)> = Vec::new();
    for type_use in type_uses {
        let Some(sites) = def_index.get(&type_use.name) else {
            continue;
        };
        // Defined in this very file -> self-reference, not a dependency.
        let mut sites: Vec<&(String, usize, usize)> =
            sites.iter().filter(|(f, _, _)| f != rel_path).collect();
        sites.sort_unstable();
        sites.dedup();
        if sites.is_empty() || sites.len() > MAX_DEF_SITES {
            continue;
        }
        targets.extend(
            sites
                .into_iter()
                .map(|(f, ls, le)| (f.clone(), type_use.name.clone(), *ls, *le)),
        );
    }
    targets.sort();
    targets.dedup();
    targets
}

/// Insert `TypeRef` edges for every resolved type usage:
/// - file -> defining file (drives `impact_analysis` blast radius),
/// - file -> defined type symbol (clears the symbol from `dead_code`,
///   whose query already exempts `type_ref` targets).
fn insert_type_ref_edges(
    graph: &CodeGraph,
    file_node_id: i64,
    rel_path: &str,
    type_uses: &[crate::core::deep_queries::TypeUse],
    def_index: &DefIndex,
) -> usize {
    let mut added = 0usize;
    for (target_file, type_name, line_start, line_end) in
        type_ref_targets(def_index, type_uses, rel_path)
    {
        let Ok(target_id) = graph.upsert_node(&Node::file(&target_file)) else {
            continue;
        };
        let _ = graph.upsert_edge(&Edge::new(file_node_id, target_id, EdgeKind::TypeRef));
        added += 1;

        let sym_node = Node::symbol(
            &type_name,
            &target_file,
            crate::core::property_graph::NodeKind::Symbol,
        )
        .with_lines(line_start, line_end);
        if let Ok(sym_id) = graph.upsert_node(&sym_node) {
            let _ = graph.upsert_edge(&Edge::new(file_node_id, sym_id, EdgeKind::TypeRef));
            added += 1;
        }
    }
    added
}

fn normalize_git_path(line: &str) -> String {
    line.trim().replace('\\', "/")
}

fn git_diff_name_only_lines(project_root: &Path, args: &[&str]) -> Option<Vec<String>> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(project_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    Some(
        s.lines()
            .map(normalize_git_path)
            .filter(|l| !l.is_empty())
            .collect(),
    )
}

fn collect_git_changed_paths(project_root: &Path, last_git_head: &str) -> Option<BTreeSet<String>> {
    let range = format!("{last_git_head}..HEAD");
    let mut set: BTreeSet<String> = BTreeSet::new();
    for line in git_diff_name_only_lines(project_root, &["diff", "--name-only", &range])? {
        set.insert(line);
    }
    for line in git_diff_name_only_lines(project_root, &["diff", "--name-only"])? {
        set.insert(line);
    }
    for line in git_diff_name_only_lines(project_root, &["diff", "--name-only", "--cached"])? {
        set.insert(line);
    }
    Some(set)
}

#[cfg(feature = "embeddings")]
fn enclosing_symbol_name_for_line(
    types: &[crate::core::deep_queries::TypeDef],
    line: usize,
) -> String {
    let mut best: Option<(&crate::core::deep_queries::TypeDef, usize)> = None;
    for t in types {
        if line >= t.line && line <= t.end_line {
            let span = t.end_line.saturating_sub(t.line);
            match best {
                None => best = Some((t, span)),
                Some((_, prev_span)) => {
                    if span < prev_span {
                        best = Some((t, span));
                    }
                }
            }
        }
    }
    best.map_or_else(|| "<module>".to_string(), |(t, _)| t.name.clone())
}

#[cfg(feature = "embeddings")]
fn resolve_call_callee_site(
    def_index: &DefIndex,
    callee: &str,
    caller_file: &str,
) -> Option<(String, usize, usize)> {
    let sites = def_index.get(callee)?;
    for (f, ls, le) in sites {
        if f == caller_file {
            return Some((f.clone(), *ls, *le));
        }
    }
    let mut sorted: Vec<(String, usize, usize)> = sites.clone();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    sorted.into_iter().next()
}

#[cfg(feature = "embeddings")]
fn index_graph_file_embeddings(
    graph: &CodeGraph,
    rel_path: &str,
    ext: &str,
    analysis: &crate::core::deep_queries::DeepAnalysis,
    resolver_ctx: &crate::core::import_resolver::ResolverContext,
    def_index: &DefIndex,
) -> (usize, usize) {
    let mut total_nodes = 0usize;
    let mut total_edges = 0usize;

    let Ok(file_node_id) = graph.upsert_node(&Node::file(rel_path)) else {
        return (0, 0);
    };
    total_nodes += 1;

    for type_def in &analysis.types {
        let sym_node = Node::symbol(
            &type_def.name,
            rel_path,
            crate::core::property_graph::NodeKind::Symbol,
        )
        .with_lines(type_def.line, type_def.end_line);
        if let Ok(sym_id) = graph.upsert_node(&sym_node) {
            total_nodes += 1;
            let _ = graph.upsert_edge(&Edge::new(file_node_id, sym_id, EdgeKind::Defines));
            total_edges += 1;
            if type_def.is_exported {
                let _ = graph.upsert_edge(&Edge::new(sym_id, file_node_id, EdgeKind::Exports));
                total_edges += 1;
            }
        }
    }

    let resolved = crate::core::import_resolver::resolve_imports(
        &analysis.imports,
        rel_path,
        ext,
        resolver_ctx,
    );

    let mut targets: Vec<String> = resolved
        .into_iter()
        .filter(|imp| !imp.is_external)
        .filter_map(|imp| imp.resolved_path)
        .collect();
    targets.sort();
    targets.dedup();

    for target_path in targets {
        let Ok(target_id) = graph.upsert_node(&Node::file(&target_path)) else {
            continue;
        };
        let _ = graph.upsert_edge(&Edge::new(file_node_id, target_id, EdgeKind::Imports));
        total_edges += 1;
    }

    for call in &analysis.calls {
        let caller_name = enclosing_symbol_name_for_line(&analysis.types, call.line);
        let mut caller_node = Node::symbol(
            &caller_name,
            rel_path,
            crate::core::property_graph::NodeKind::Symbol,
        );
        if let Some(t) = analysis.types.iter().find(|t| t.name == caller_name) {
            caller_node = caller_node.with_lines(t.line, t.end_line);
        }
        let Ok(caller_id) = graph.upsert_node(&caller_node) else {
            continue;
        };
        total_nodes += 1;

        let Some((callee_file, c_line, c_end)) =
            resolve_call_callee_site(def_index, &call.callee, rel_path)
        else {
            continue;
        };

        let callee_node = Node::symbol(
            &call.callee,
            &callee_file,
            crate::core::property_graph::NodeKind::Symbol,
        )
        .with_lines(c_line, c_end);
        let Ok(callee_id) = graph.upsert_node(&callee_node) else {
            continue;
        };
        total_nodes += 1;
        let _ = graph.upsert_edge(&Edge::new(caller_id, callee_id, EdgeKind::Calls));
        total_edges += 1;

        if callee_file != rel_path {
            let Ok(callee_file_id) = graph.upsert_node(&Node::file(&callee_file)) else {
                continue;
            };
            let _ = graph.upsert_edge(&Edge::new(file_node_id, callee_file_id, EdgeKind::Calls));
            total_edges += 1;
        }
    }

    // Type-usage edges close the same-namespace gap (C#/Java, GH #398):
    // a file consuming a project type without importing it still depends on
    // the defining file.
    total_edges += insert_type_ref_edges(
        graph,
        file_node_id,
        rel_path,
        &analysis.type_uses,
        def_index,
    );

    (total_nodes, total_edges)
}

#[cfg(not(feature = "embeddings"))]
fn index_graph_file_minimal(
    graph: &CodeGraph,
    rel_path: &str,
    content: &str,
    ext: &str,
    analysis: &crate::core::deep_queries::DeepAnalysis,
    resolver_ctx: &crate::core::import_resolver::ResolverContext,
    def_index: &DefIndex,
) -> (usize, usize) {
    let Ok(file_node_id) = graph.upsert_node(&Node::file(rel_path)) else {
        return (0, 0);
    };
    let mut total_nodes = 1usize;
    let mut total_edges = 0usize;

    let resolved = crate::core::import_resolver::resolve_imports(
        &analysis.imports,
        rel_path,
        ext,
        resolver_ctx,
    );

    let mut targets: Vec<String> = resolved
        .into_iter()
        .filter(|imp| !imp.is_external)
        .filter_map(|imp| imp.resolved_path)
        .filter(|p| p != rel_path)
        .collect();
    targets.sort();
    targets.dedup();

    for target_path in targets {
        let Ok(target_id) = graph.upsert_node(&Node::file(&target_path)) else {
            continue;
        };
        total_nodes += 1;
        let _ = graph.upsert_edge(&Edge::new(file_node_id, target_id, EdgeKind::Imports));
        total_edges += 1;
    }

    for type_def in &analysis.types {
        if type_def.is_exported {
            let sym_node = Node::symbol(
                &type_def.name,
                rel_path,
                crate::core::property_graph::NodeKind::Symbol,
            )
            .with_lines(type_def.line, type_def.end_line);
            if let Ok(sym_id) = graph.upsert_node(&sym_node) {
                total_nodes += 1;
                let _ = graph.upsert_edge(&Edge::new(file_node_id, sym_id, EdgeKind::Defines));
                let _ = graph.upsert_edge(&Edge::new(sym_id, file_node_id, EdgeKind::Exports));
                total_edges += 2;
            }
        }
    }

    // Same-namespace type consumption (C#/Java, GH #398) — see the
    // embeddings-path counterpart in `index_graph_file_embeddings`.
    total_edges += insert_type_ref_edges(
        graph,
        file_node_id,
        rel_path,
        &analysis.type_uses,
        def_index,
    );

    let exports: Vec<String> = analysis
        .types
        .iter()
        .filter(|t| t.is_exported)
        .map(|t| t.name.clone())
        .collect();
    let line_count = content.lines().count();
    let token_count = crate::core::tokens::count_tokens(content);
    let hash = {
        use md5::{Digest, Md5};
        let mut h = Md5::new();
        h.update(content.as_bytes());
        format!("{:x}", h.finalize())
    };
    let _ = graph.upsert_file_catalog(&crate::core::property_graph::FileCatalogEntry {
        path: rel_path.to_string(),
        hash,
        language: ext.to_string(),
        line_count,
        token_count,
        exports,
        summary: String::new(),
    });

    (total_nodes, total_edges)
}

fn handle_build(root: &str, fmt: OutputFormat) -> String {
    let t0 = std::time::Instant::now();
    let root_path = Path::new(root);

    let graph = match open_graph(root) {
        Ok(g) => g,
        Err(e) => return e,
    };

    let incremental_hint: Option<&'static str> = {
        let nodes_ok = graph.node_count().unwrap_or(0) > 0;
        let has_head = crate::core::property_graph::load_meta(root)
            .and_then(|m| m.git_head)
            .is_some_and(|s| !s.is_empty());
        if nodes_ok && has_head {
            Some(
                "Hint: Graph already indexed — for faster refresh, use ctx_impact action='update' \
                 to apply incremental git-based updates instead of a full rebuild.",
            )
        } else {
            None
        }
    };

    if let Err(e) = graph.clear() {
        return format!("Failed to clear graph: {e}");
    }

    let (file_paths, file_contents) = walk_supported_sources(root_path);

    let cs_contents: std::collections::HashMap<String, String> = file_contents
        .iter()
        .filter(|(_, _, e)| e.eq_ignore_ascii_case("cs"))
        .map(|(p, c, _)| (p.clone(), c.clone()))
        .collect();
    let resolver_ctx = crate::core::import_resolver::ResolverContext::new(
        root_path,
        file_paths.clone(),
        &cs_contents,
    );

    let mut total_nodes = 0usize;
    let mut total_edges = 0usize;

    let (per_file, def_index) = analyze_all(&file_contents);

    #[cfg(feature = "embeddings")]
    for (rel_path, _content, ext, analysis) in &per_file {
        let (n, e) =
            index_graph_file_embeddings(&graph, rel_path, ext, analysis, &resolver_ctx, &def_index);
        total_nodes += n;
        total_edges += e;
    }

    #[cfg(not(feature = "embeddings"))]
    for (rel_path, content, ext, analysis) in &per_file {
        let (n, e) = index_graph_file_minimal(
            &graph,
            rel_path,
            content,
            ext,
            analysis,
            &resolver_ctx,
            &def_index,
        );
        total_nodes += n;
        total_edges += e;
    }

    let build_time_ms = t0.elapsed().as_millis() as u64;

    let db_display = graph.db_path().display();
    let mut result = format!(
        "Graph built: {total_nodes} nodes, {total_edges} edges from {} files\n\
         Stored at: {db_display}\n\
         Build time: {build_time_ms}ms",
        file_contents.len(),
    );
    if let Some(h) = incremental_hint {
        result.push('\n');
        result.push_str(h);
    }

    let _ = crate::core::property_graph::write_meta(
        root,
        &crate::core::property_graph::PropertyGraphMetaV1 {
            schema_version: 1,
            engine_version: crate::core::property_graph::GRAPH_ENGINE_VERSION,
            built_with: env!("CARGO_PKG_VERSION").to_string(),
            built_at: chrono::Utc::now().to_rfc3339(),
            git_head: git_out(root_path, &["rev-parse", "--short", "HEAD"]),
            git_dirty: Some(git_dirty(root_path)),
            nodes: graph.node_count().ok(),
            edges: graph.edge_count().ok(),
            files_indexed: Some(file_contents.len()),
            build_time_ms: Some(build_time_ms),
        },
    );

    let tokens = count_tokens(&result);
    match fmt {
        OutputFormat::Json => {
            let mut v = serde_json::json!({
                "schema_version": crate::core::contracts::GRAPH_REPRODUCIBILITY_V1_SCHEMA_VERSION,
                "tool": "ctx_impact",
                "action": "build",
                "project": project_meta(root),
                "graph": graph_summary(root),
                "graph_meta": crate::core::property_graph::load_meta(root),
                "indexed_files": file_contents.len(),
                "nodes": total_nodes,
                "edges": total_edges,
                "build_time_ms": build_time_ms,
                "db_path": graph.db_path().display().to_string()
            });
            if let Some(h) = incremental_hint {
                v.as_object_mut()
                    .map(|m| m.insert("incremental_hint".to_string(), json!(h)));
            }
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
        }
        OutputFormat::Text => format!("{result}\n[ctx_impact build: {tokens} tok]"),
    }
}

fn handle_update(root: &str, fmt: OutputFormat) -> String {
    let t0 = std::time::Instant::now();
    let root_path = Path::new(root);

    let graph = match open_graph(root) {
        Ok(g) => g,
        Err(e) => return e,
    };

    if graph.node_count().unwrap_or(0) == 0 {
        return handle_build(root, fmt);
    }

    let Some(meta) = crate::core::property_graph::load_meta(root) else {
        return handle_build(root, fmt);
    };

    let Some(last_git_head) = meta.git_head.filter(|s| !s.is_empty()) else {
        return handle_build(root, fmt);
    };

    let Some(changed) = collect_git_changed_paths(root_path, &last_git_head) else {
        return handle_build(root, fmt);
    };

    let changed_count = changed.len();
    let (file_paths, file_contents) = walk_supported_sources(root_path);
    let cs_contents: std::collections::HashMap<String, String> = file_contents
        .iter()
        .filter(|(_, _, e)| e.eq_ignore_ascii_case("cs"))
        .map(|(p, c, _)| (p.clone(), c.clone()))
        .collect();
    let resolver_ctx = crate::core::import_resolver::ResolverContext::new(
        root_path,
        file_paths.clone(),
        &cs_contents,
    );

    let (per_file, def_index) = analyze_all(&file_contents);

    let mut total_nodes = 0usize;
    let mut total_edges = 0usize;

    for rel_path in &changed {
        let p = Path::new(rel_path);
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        let supported = GRAPH_SOURCE_EXTS.contains(&ext);
        let abs = root_path.join(rel_path);

        if !abs.exists() {
            if supported {
                let _ = graph.remove_file_nodes(rel_path);
            }
            continue;
        }

        if !supported {
            continue;
        }

        if let Err(e) = graph.remove_file_nodes(rel_path) {
            return format!("Failed to remove old nodes for {rel_path}: {e}");
        }

        let Some((_, _content, ext_owned, analysis)) =
            per_file.iter().find(|(p, _, _, _)| *p == rel_path)
        else {
            continue;
        };

        #[cfg(feature = "embeddings")]
        {
            let (n, e) = index_graph_file_embeddings(
                &graph,
                rel_path,
                ext_owned,
                analysis,
                &resolver_ctx,
                &def_index,
            );
            total_nodes += n;
            total_edges += e;
        }

        #[cfg(not(feature = "embeddings"))]
        {
            let (n, e) = index_graph_file_minimal(
                &graph,
                rel_path,
                _content,
                ext_owned,
                analysis,
                &resolver_ctx,
                &def_index,
            );
            total_nodes += n;
            total_edges += e;
        }
    }

    let elapsed_ms = t0.elapsed().as_millis() as u64;

    let _ = crate::core::property_graph::write_meta(
        root,
        &crate::core::property_graph::PropertyGraphMetaV1 {
            schema_version: 1,
            engine_version: crate::core::property_graph::GRAPH_ENGINE_VERSION,
            built_with: env!("CARGO_PKG_VERSION").to_string(),
            built_at: chrono::Utc::now().to_rfc3339(),
            git_head: git_out(root_path, &["rev-parse", "--short", "HEAD"]),
            git_dirty: Some(git_dirty(root_path)),
            nodes: graph.node_count().ok(),
            edges: graph.edge_count().ok(),
            files_indexed: Some(file_contents.len()),
            build_time_ms: Some(elapsed_ms),
        },
    );

    let summary = format!(
        "Incremental update: {changed_count} files changed, {total_nodes} nodes updated, {total_edges} edges added ({elapsed_ms}ms)"
    );

    let tokens = count_tokens(&summary);
    match fmt {
        OutputFormat::Json => {
            let v = json!({
                "schema_version": crate::core::contracts::GRAPH_REPRODUCIBILITY_V1_SCHEMA_VERSION,
                "tool": "ctx_impact",
                "action": "update",
                "project": project_meta(root),
                "graph": graph_summary(root),
                "graph_meta": crate::core::property_graph::load_meta(root),
                "git_range_from": last_git_head,
                "files_changed_reported": changed_count,
                "nodes_added": total_nodes,
                "edges_added": total_edges,
                "update_time_ms": elapsed_ms,
                "db_path": graph.db_path().display().to_string()
            });
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
        }
        OutputFormat::Text => format!("{summary}\n[ctx_impact update: {tokens} tok]"),
    }
}

fn handle_status(root: &str, fmt: OutputFormat) -> String {
    let graph = match open_graph(root) {
        Ok(g) => g,
        Err(e) => return e,
    };

    let nodes = graph.node_count().unwrap_or(0);
    let edges = graph.edge_count().unwrap_or(0);

    if nodes == 0 {
        return match fmt {
            OutputFormat::Json => {
                let v = json!({
                    "schema_version": crate::core::contracts::GRAPH_REPRODUCIBILITY_V1_SCHEMA_VERSION,
                    "tool": "ctx_impact",
                    "action": "status",
                    "project": project_meta(root),
                    "graph": graph_summary(root),
                    "freshness": "empty",
                    "hint": "Run ctx_impact action='build' to index."
                });
                serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
            }
            OutputFormat::Text => {
                "Graph is empty. Run ctx_impact action='build' to index.".to_string()
            }
        };
    }

    let root_path = Path::new(root);
    let meta = crate::core::property_graph::load_meta(root);
    let current_head = git_out(root_path, &["rev-parse", "--short", "HEAD"]);
    let current_dirty = git_dirty(root_path);
    let stale = meta.as_ref().is_some_and(|m| {
        let head_mismatch = match (m.git_head.as_ref(), current_head.as_ref()) {
            (Some(a), Some(b)) => a != b,
            _ => false,
        };
        let dirty_mismatch = match (m.git_dirty, Some(current_dirty)) {
            (Some(a), Some(b)) => a != b,
            _ => false,
        };
        head_mismatch || dirty_mismatch
    });
    let freshness = if stale { "stale" } else { "fresh" };

    match fmt {
        OutputFormat::Json => {
            let v = json!({
                "schema_version": crate::core::contracts::GRAPH_REPRODUCIBILITY_V1_SCHEMA_VERSION,
                "tool": "ctx_impact",
                "action": "status",
                "project": project_meta(root),
                "graph": graph_summary(root),
                "freshness": freshness,
                "meta": meta
            });
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
        }
        OutputFormat::Text => {
            let db_display = graph.db_path().display();
            let mut out =
                format!("Property Graph: {nodes} nodes, {edges} edges\nStored: {db_display}");
            if stale {
                out.push_str("\nWARNING: graph looks stale (git HEAD / dirty mismatch). Run ctx_impact action='build' to refresh.");
            }
            out
        }
    }
}

fn project_meta(root: &str) -> Value {
    let root_hash = crate::core::project_hash::hash_project_root(root);
    let identity_hash = crate::core::project_hash::project_identity(root)
        .as_deref()
        .map(crate::core::hasher::hash_str);

    let root_path = Path::new(root);
    json!({
        "project_root_hash": root_hash,
        "project_identity_hash": identity_hash,
        "git": {
            "head": git_out(root_path, &["rev-parse", "--short", "HEAD"]),
            "branch": git_out(root_path, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "dirty": git_dirty(root_path)
        }
    })
}

fn graph_summary(project_root: &str) -> Value {
    let graph_dir = crate::core::property_graph::graph_dir(project_root);
    let db_path = graph_dir.join("graph.db");
    let db_path_display = db_path.display().to_string();
    if !db_path.exists() {
        return json!({
            "exists": false,
            "db_path": db_path_display,
            "nodes": null,
            "edges": null
        });
    }
    match crate::core::property_graph::CodeGraph::open(project_root) {
        Ok(g) => json!({
            "exists": true,
            "db_path": g.db_path().display().to_string(),
            "nodes": g.node_count().ok(),
            "edges": g.edge_count().ok()
        }),
        Err(_) => json!({
            "exists": true,
            "db_path": db_path_display,
            "nodes": null,
            "edges": null
        }),
    }
}

fn git_dirty(project_root: &Path) -> bool {
    let out = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(project_root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() => !o.stdout.is_empty(),
        _ => false,
    }
}

fn git_out(project_root: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(project_root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let s = s.trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_impact_empty() {
        let impact = ImpactResult {
            root_file: "a.rs".to_string(),
            affected_files: vec![],
            max_depth_reached: 0,
            edges_traversed: 0,
        };
        let result = format_impact(&impact, "a.rs", "/tmp", OutputFormat::Text);
        assert!(result.contains("No files depend on"));
    }

    #[test]
    fn format_impact_with_files() {
        let impact = ImpactResult {
            root_file: "a.rs".to_string(),
            affected_files: vec!["b.rs".to_string(), "c.rs".to_string()],
            max_depth_reached: 2,
            edges_traversed: 3,
        };
        let result = format_impact(&impact, "a.rs", "/tmp", OutputFormat::Text);
        assert!(result.contains("2 affected files"));
        assert!(result.contains("b.rs"));
        assert!(result.contains("c.rs"));
    }

    #[test]
    fn format_chain_display() {
        let chain = DependencyChain {
            path: vec!["a.rs".to_string(), "b.rs".to_string(), "c.rs".to_string()],
            depth: 2,
        };
        let result = format_chain(&chain, "/tmp", OutputFormat::Text);
        assert!(result.contains("depth 2"));
        assert!(result.contains("a.rs"));
        assert!(result.contains("-> b.rs"));
        assert!(result.contains("-> c.rs"));
    }

    #[test]
    fn handle_missing_path() {
        let result = handle("analyze", None, "/tmp", None, None);
        assert!(result.contains("path is required"));
    }

    #[test]
    fn handle_invalid_chain_spec() {
        let result = handle("chain", Some("no_arrow_here"), "/tmp", None, None);
        assert!(result.contains("Invalid chain spec"));
    }

    #[test]
    fn handle_unknown_action() {
        let result = handle("invalid", None, "/tmp", None, None);
        assert!(result.contains("Unknown action"));
    }

    /// GH #398: the TypeRef target resolution — unique definers connect,
    /// self-references are skipped, over-generic names (>3 definers) are
    /// dropped, and output is sorted + deduped (determinism, #498).
    #[test]
    fn type_ref_targets_resolution_rules() {
        let mut def_index: std::collections::HashMap<String, Vec<(String, usize, usize)>> =
            std::collections::HashMap::new();
        def_index.insert("Engine".into(), vec![("Models/Engine.cs".into(), 1, 5)]);
        def_index.insert("Motor".into(), vec![("Services/Motor.cs".into(), 1, 9)]);
        def_index.insert(
            "Config".into(),
            vec![
                ("a/Config.cs".into(), 1, 2),
                ("b/Config.cs".into(), 1, 2),
                ("c/Config.cs".into(), 1, 2),
                ("d/Config.cs".into(), 1, 2),
            ],
        );

        let uses = |names: &[&str]| -> Vec<crate::core::deep_queries::TypeUse> {
            names
                .iter()
                .map(|n| crate::core::deep_queries::TypeUse {
                    name: (*n).to_string(),
                    line: 1,
                })
                .collect()
        };

        // Unique definer in another file -> edge target with symbol site.
        assert_eq!(
            type_ref_targets(&def_index, &uses(&["Engine"]), "Services/Motor.cs"),
            vec![("Models/Engine.cs".to_string(), "Engine".to_string(), 1, 5)]
        );
        // Using one's own type -> no self edge.
        assert!(type_ref_targets(&def_index, &uses(&["Motor"]), "Services/Motor.cs").is_empty());
        // Defined in 4 files -> too generic, skipped.
        assert!(type_ref_targets(&def_index, &uses(&["Config"]), "Services/Motor.cs").is_empty());
        // Unknown / external types -> nothing.
        assert!(type_ref_targets(&def_index, &uses(&["String"]), "x.cs").is_empty());
        // Duplicate uses collapse into one sorted target list.
        assert_eq!(
            type_ref_targets(&def_index, &uses(&["Engine", "Engine"]), "x.cs"),
            vec![("Models/Engine.cs".to_string(), "Engine".to_string(), 1, 5)]
        );
    }

    #[test]
    fn graph_target_key_normalizes_windows_styles() {
        let target = graph_target_key(r"C:/repo/src/main.rs", r"C:\repo");
        let expected = if cfg!(windows) {
            "src/main.rs"
        } else {
            "C:/repo/src/main.rs"
        };
        assert_eq!(target, expected);
    }

    /// End-to-end regression for GH #365: build the property graph from real
    /// Python sources and assert that a class which is imported + instantiated
    /// cross-file is NOT reported as `dead_code`. This exercises the *builder*
    /// (symbol-level `Calls` edge for class instantiation), not just the SQL
    /// rule covered by the synthetic test in `core::smells`. The unused class
    /// must still be flagged so the test cannot pass vacuously.
    #[cfg(feature = "embeddings")]
    #[test]
    fn dead_code_builder_does_not_flag_instantiated_python_class() {
        // The property-graph DB path is derived from `LEAN_CTX_DATA_DIR`
        // (`graph_dir`), so a concurrent test that mutates that env var between
        // our `build` and `open` would point them at different directories and
        // yield an empty graph. Serialize on the shared lock that every other
        // data-dir-mutating test already uses.
        let _env = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("models")).unwrap();
        std::fs::write(
            root.join("models/engine.py"),
            "class Engine:\n    def __init__(self, power):\n        self.power = power\n\n\n\
             class Pipeline:\n    def __init__(self, cfg):\n        self.cfg = cfg\n\n\n\
             class UnusedOrphan:\n    pass\n",
        )
        .unwrap();
        std::fs::write(
            root.join("app.py"),
            "from models.engine import Engine, Pipeline\n\n\
             engine = Engine(power=100)\npipeline = Pipeline(cfg={})\n",
        )
        .unwrap();

        let root_str = root.to_string_lossy().to_string();
        let out = handle("build", None, &root_str, None, Some("text"));
        assert!(!out.contains("ERROR"), "graph build failed: {out}");

        let graph =
            crate::core::property_graph::CodeGraph::open(&root_str).expect("open property graph");
        let findings = crate::core::smells::scan_rule(
            graph.connection(),
            "dead_code",
            &crate::core::smells::SmellConfig::default(),
        );
        let dead: Vec<String> = findings.iter().filter_map(|f| f.symbol.clone()).collect();

        assert!(
            !dead.iter().any(|s| s == "Engine"),
            "instantiated class `Engine` must not be dead_code; findings: {dead:?}"
        );
        assert!(
            !dead.iter().any(|s| s == "Pipeline"),
            "instantiated class `Pipeline` must not be dead_code; findings: {dead:?}"
        );
        assert!(
            dead.iter().any(|s| s == "UnusedOrphan"),
            "never-referenced class `UnusedOrphan` should still be flagged (non-vacuous); \
             findings: {dead:?}"
        );
    }

    /// End-to-end regression for GH #398: C# files in the same namespace use
    /// each other's types **without any `using` directive**, and dependency
    /// injection means the type is often never `new`-ed by its consumer. With
    /// import- and call-edges only, the consumed class is a false-negative
    /// leaf node. Type-usage edges (`TypeRef`) must connect consumer -> definer
    /// so impact analysis reports the real blast radius.
    #[cfg(feature = "embeddings")]
    #[test]
    fn csharp_same_namespace_type_use_is_not_a_leaf() {
        let _env = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("Models")).unwrap();
        std::fs::create_dir_all(root.join("Services")).unwrap();

        // The small class under change — no consumer imports it via `using`.
        std::fs::write(
            root.join("Models/Engine.cs"),
            "namespace App.Core;\n\n\
             public class Engine\n{\n    public int Power { get; set; }\n}\n",
        )
        .unwrap();
        // DI-style consumer: field + constructor parameter, never `new Engine()`.
        std::fs::write(
            root.join("Services/Motor.cs"),
            "namespace App.Core;\n\n\
             public class Motor\n{\n    private readonly Engine _engine;\n\n\
             \x20   public Motor(Engine engine)\n    {\n        _engine = engine;\n    }\n}\n",
        )
        .unwrap();
        // Inheritance consumer in a nested namespace part, also without `using`.
        std::fs::write(
            root.join("Services/TurboEngine.cs"),
            "namespace App.Core;\n\n\
             public class TurboEngine : Engine\n{\n    public int Boost { get; set; }\n}\n",
        )
        .unwrap();
        // Unrelated file: must NOT appear in the blast radius (non-vacuous).
        std::fs::write(
            root.join("Services/Logger.cs"),
            "namespace App.Core;\n\n\
             public class Logger\n{\n    public void Log(string msg) { }\n}\n",
        )
        .unwrap();

        let root_str = root.to_string_lossy().to_string();
        let out = handle("build", None, &root_str, None, Some("text"));
        assert!(!out.contains("ERROR"), "graph build failed: {out}");

        let graph =
            crate::core::property_graph::CodeGraph::open(&root_str).expect("open property graph");
        let impact = graph
            .impact_analysis("Models/Engine.cs", 5)
            .expect("impact analysis");

        assert!(
            impact
                .affected_files
                .contains(&"Services/Motor.cs".to_string()),
            "DI consumer (field + ctor param, no using, no new) must be affected; got: {:?}",
            impact.affected_files
        );
        assert!(
            impact
                .affected_files
                .contains(&"Services/TurboEngine.cs".to_string()),
            "subclass (base_list, no using) must be affected; got: {:?}",
            impact.affected_files
        );
        assert!(
            !impact
                .affected_files
                .contains(&"Services/Logger.cs".to_string()),
            "unrelated file must NOT be affected; got: {:?}",
            impact.affected_files
        );

        // Same root cause, second symptom: a class consumed only as a type
        // (DI) was flagged `dead_code` because nothing ever *called* it. The
        // symbol-level TypeRef edge must clear it; the genuinely unreferenced
        // Logger keeps the rule honest.
        let findings = crate::core::smells::scan_rule(
            graph.connection(),
            "dead_code",
            &crate::core::smells::SmellConfig::default(),
        );
        let dead: Vec<String> = findings.iter().filter_map(|f| f.symbol.clone()).collect();
        assert!(
            !dead.iter().any(|s| s == "Engine"),
            "type-consumed class `Engine` must not be dead_code; findings: {dead:?}"
        );
        assert!(
            dead.iter().any(|s| s == "Logger"),
            "never-referenced class `Logger` should still be flagged (non-vacuous); \
             findings: {dead:?}"
        );
    }

    /// Regression for GH #398's upgrade path: the v3.8.3 `type_ref` fix only
    /// helps if an *existing* graph is rebuilt after upgrading. A graph built by
    /// an older engine keeps `node_count > 0`, so without an engine-version gate
    /// `analyze` silently serves it — leaving the C# same-namespace consumer a
    /// false-negative leaf. We build a correct graph, stamp its meta back to
    /// engine version 0 (simulating a pre-`type_ref` build), and assert the next
    /// query self-heals: it rebuilds, surfaces the consumer, and re-stamps the
    /// current engine version.
    #[cfg(feature = "embeddings")]
    #[test]
    fn stale_engine_graph_is_rebuilt_before_query() {
        let _env = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("Models")).unwrap();
        std::fs::create_dir_all(root.join("Services")).unwrap();

        std::fs::write(
            root.join("Models/Engine.cs"),
            "namespace App.Core;\n\n\
             public class Engine\n{\n    public int Power { get; set; }\n}\n",
        )
        .unwrap();
        // DI-style consumer: field + constructor parameter, no `using`, no `new`.
        std::fs::write(
            root.join("Services/Motor.cs"),
            "namespace App.Core;\n\n\
             public class Motor\n{\n    private readonly Engine _engine;\n\n\
             \x20   public Motor(Engine engine)\n    {\n        _engine = engine;\n    }\n}\n",
        )
        .unwrap();

        let root_str = root.to_string_lossy().to_string();

        // Build a correct graph, then simulate a graph produced by an engine
        // that predates `type_ref` by stamping its meta back to version 0.
        let out = handle("build", None, &root_str, None, Some("text"));
        assert!(!out.contains("ERROR"), "graph build failed: {out}");
        let mut meta = crate::core::property_graph::load_meta(&root_str).expect("meta after build");
        assert_eq!(
            meta.engine_version,
            crate::core::property_graph::GRAPH_ENGINE_VERSION,
            "a fresh build must stamp the current engine version"
        );
        meta.engine_version = 0;
        crate::core::property_graph::write_meta(&root_str, &meta).expect("downgrade meta");
        assert!(
            crate::core::property_graph::engine_outdated(&root_str),
            "downgraded graph must read as outdated"
        );

        // The query path must transparently rebuild the stale graph.
        let analysis = handle(
            "analyze",
            Some("Models/Engine.cs"),
            &root_str,
            None,
            Some("text"),
        );
        assert!(
            analysis.contains("Services/Motor.cs"),
            "stale graph must be rebuilt so the DI consumer surfaces; got: {analysis}"
        );
        let healed =
            crate::core::property_graph::load_meta(&root_str).expect("meta after self-heal");
        assert_eq!(
            healed.engine_version,
            crate::core::property_graph::GRAPH_ENGINE_VERSION,
            "self-heal must re-stamp the current engine version"
        );
    }
}
