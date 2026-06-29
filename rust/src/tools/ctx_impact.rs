//! `ctx_impact` — Graph-based impact analysis tool.
//!
//! Uses the SQLite-backed Property Graph to answer: "What breaks when file X changes?"
//! Performs BFS traversal of reverse import edges to find all transitively affected files.

use crate::core::property_graph::{CodeGraph, DependencyChain, Edge, EdgeKind, ImpactResult, Node};
use crate::core::tokens::count_tokens;
use crate::core::type_ref_edges::{DefIndex, ExtMethodIndex};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Stdio;

use crate::core::git_util::{git_dirty, git_out};
use crate::tools::graph_meta::{graph_summary, project_meta};
use crate::tools::output_format::{OutputFormat, parse_format};

/// Extensions whose files become Property Graph source nodes. Must stay a subset
/// of `language_capabilities::is_indexable_ext` and align with the deep-query
/// extractors (`deep_queries::{type_defs, calls}`) so each language contributes
/// real symbol/import/call structure rather than bare file nodes.
const GRAPH_SOURCE_EXTS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "gd", "cs", "kt", "kts",
];

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
        "parity" => handle_parity(root, fmt),
        _ => "Unknown action. Use: analyze, diff, chain, build, status, update, parity".to_string(),
    }
}

/// Shadow-mode parity proof (#682.3): build an in-memory PropertyGraph from the
/// current graph_index and quantify whether PG reproduces everything the
/// facade exposes (symbols, edges, dependencies) before any backend flip.
fn handle_parity(root: &str, fmt: OutputFormat) -> String {
    // Compare the *fresh extractor* output (a real graph_index scan, built
    // in-memory from the file walk + signature extraction) against a
    // PropertyGraph populated from it — the genuine "mirror is lossless"
    // invariant. Loading the persisted index would be circular since #696 C4
    // (it is itself materialized from the PG), yielding a meaningless trivially
    // lossless result, so always rescan to keep this a real proof.
    let index = crate::core::graph_index::scan_with_content_cache(root).0;

    let report = match crate::core::graph_parity::compare(&index) {
        Ok(r) => r,
        Err(e) => return format!("Parity comparison failed: {e}"),
    };

    match fmt {
        OutputFormat::Json => {
            let v = json!({
                "tool": "ctx_impact",
                "action": "parity",
                "lossless": report.is_lossless(),
                "files": report.files,
                "symbols": { "gi": report.symbol_count_gi, "pg": report.symbol_count_pg,
                             "matched": report.symbols_matched, "checked": report.symbols_checked },
                "edges": { "gi": report.edge_count_gi, "pg": report.edge_count_pg,
                           "superset": report.edge_pairs_lossless },
                "dependencies": { "lossless": report.dependencies_lossless,
                                  "checked": report.files_checked, "extra": report.dependencies_extra },
                "dependents": { "lossless": report.dependents_lossless, "checked": report.files_checked },
                "divergences": report.divergences,
            });
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
        }
        OutputFormat::Text => {
            let body = crate::core::graph_parity::format_report(&report);
            let tokens = count_tokens(&body);
            format!("{body}\n[ctx_impact parity: {tokens} tok]")
        }
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

    if graph.node_count().unwrap_or(0) == 0 {
        return "Graph is empty after auto-build. No supported source files found.".to_string();
    }

    let rel_target = graph_target_key(target, root);

    // 1) Direct file-node match — the documented contract (a file path).
    if graph.get_node_by_path(&rel_target).ok().flatten().is_some() {
        let impact = match graph.impact_analysis(&rel_target, max_depth) {
            Ok(r) => r,
            Err(e) => return format!("Impact analysis failed: {e}"),
        };
        return format_impact(&impact, &rel_target, root, fmt);
    }

    // 2) Symbol-name fallback (GH #398): callers — and LLMs — routinely ask for
    //    the impact of a *class/type* by name (`ctx_impact analyze ArcPoint`)
    //    rather than its file path. Resolve the bare name to the file(s) that
    //    define it and report their combined blast radius, instead of the
    //    misleading "leaf node" answer a non-file target produced before.
    let symbol = symbol_query_name(target);
    if !symbol.is_empty()
        && let Ok(def_files) = graph.resolve_symbol_def_files(&symbol)
        && !def_files.is_empty()
    {
        return analyze_symbol(&graph, &symbol, &def_files, root, max_depth, fmt);
    }

    // 3) Neither a file nor a known symbol: an actionable diagnostic beats a
    //    false "no impact".
    analyze_unresolved(&graph, target, &rel_target, root, fmt)
}

/// Reduce a user-supplied target to a bare symbol name for the #398 fallback:
/// drop any directory prefix and a single trailing source extension, so
/// `Models/ArcPoint.cs`, `ArcPoint.cs` and `ArcPoint` all query `ArcPoint`.
/// Returns an empty string for inputs that cannot name a single symbol
/// (namespace separators, generics, globs, whitespace) — those would only
/// produce bogus matches.
fn symbol_query_name(target: &str) -> String {
    let base = target.rsplit(['/', '\\']).next().unwrap_or(target).trim();
    let stem = base
        .rsplit_once('.')
        .filter(|(_, ext)| GRAPH_SOURCE_EXTS.contains(ext))
        .map_or(base, |(s, _)| s);
    if stem.is_empty()
        || stem.contains(|c: char| {
            c.is_whitespace() || matches!(c, '.' | ':' | '*' | '<' | '>' | '(' | ')' | '/' | '\\')
        })
    {
        return String::new();
    }
    stem.to_string()
}

/// Combined blast radius of every file that defines `symbol` (GH #398
/// symbol-name fallback). The defining files are what changes, so they are
/// excluded from the affected set; the resolved files are surfaced so the
/// answer stays transparent. Output is sorted + capped for determinism (#498).
fn analyze_symbol(
    graph: &CodeGraph,
    symbol: &str,
    def_files: &[String],
    root: &str,
    max_depth: usize,
    fmt: OutputFormat,
) -> String {
    let mut affected: BTreeSet<String> = BTreeSet::new();
    let mut max_depth_reached = 0usize;
    let mut edges_traversed = 0usize;
    for f in def_files {
        if let Ok(r) = graph.impact_analysis(f, max_depth) {
            max_depth_reached = max_depth_reached.max(r.max_depth_reached);
            edges_traversed += r.edges_traversed;
            affected.extend(r.affected_files);
        }
    }
    // The definers are the thing being changed, not impacted by it.
    for f in def_files {
        affected.remove(f);
    }

    let mut sorted: Vec<String> = affected.into_iter().collect();
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
                "target": symbol,
                "resolved_from": "symbol",
                "defined_in": def_files,
                "max_depth_reached": max_depth_reached,
                "edges_traversed": edges_traversed,
                "affected_files_total": total,
                "affected_files": sorted,
                "truncated": truncated
            });
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
        }
        OutputFormat::Text => {
            let defined = def_files.join(", ");
            if total == 0 {
                let result = format!(
                    "No files depend on {symbol} (defined in {defined}); it is a leaf in the dependency graph."
                );
                let tokens = count_tokens(&result);
                return format!("{result}\n[ctx_impact: {tokens} tok]");
            }
            let mut result = format!(
                "Impact of changing {symbol} (defined in {defined}): {total} affected files \
                 (depth: {max_depth_reached}, edges traversed: {edges_traversed})\n"
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

/// Diagnostic for an `analyze` target that matched neither a file node nor a
/// symbol. Replaces the old silent "leaf node" answer — indistinguishable from
/// a real leaf — with the indexed counts and a concrete next step (GH #398).
fn analyze_unresolved(
    graph: &CodeGraph,
    target: &str,
    rel_target: &str,
    root: &str,
    fmt: OutputFormat,
) -> String {
    let files = graph.file_node_count().unwrap_or(0);
    let symbols = graph.symbol_count().unwrap_or(0);
    match fmt {
        OutputFormat::Json => {
            let v = json!({
                "tool": "ctx_impact",
                "action": "analyze",
                "project": project_meta(root),
                "graph": graph_summary(root),
                "target": target,
                "resolved": false,
                "indexed_files": files,
                "indexed_symbols": symbols,
                "hint": "Target is neither an indexed file path nor a known symbol. Pass a path relative to the project root, or rebuild with action='build'."
            });
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
        }
        OutputFormat::Text => {
            let result = format!(
                "'{target}' is not a known file or symbol in the graph \
                 ({files} files, {symbols} symbols indexed).\n  \
                 - As a file: pass a path relative to the project root (looked up '{rel_target}').\n  \
                 - As a class/type: check the spelling, or run ctx_impact action='build' to (re)index."
            );
            let tokens = count_tokens(&result);
            format!("{result}\n[ctx_impact: {tokens} tok]")
        }
    }
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
    let rel = crate::core::index_paths::graph_relative_key(path, root);
    let rel_key = crate::core::index_paths::graph_match_key(&rel);
    if rel_key.is_empty() {
        crate::core::index_paths::graph_match_key(path)
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

/// Analyze every walked source file once (parallel) and build the global
/// symbol-definition index and the extension-method index. Shared by full
/// build and incremental update on both builder paths.
fn analyze_all(
    file_contents: &[(String, String, String)],
) -> (Vec<AnalyzedFile<'_>>, DefIndex, ExtMethodIndex) {
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

    // Single source of truth for the #398 indexes (shared with the graph_index
    // mirror so both builders resolve identical type-usage edges).
    let def_index =
        crate::core::type_ref_edges::build_def_index(per_file.iter().map(|(p, _, _, a)| (*p, a)));
    let ext_method_index = crate::core::type_ref_edges::build_ext_method_index(
        per_file.iter().map(|(p, _, _, a)| (*p, a)),
    );

    (per_file, def_index, ext_method_index)
}

/// Insert `TypeRef` edges for every resolved type usage:
/// - file -> defining file (drives `impact_analysis` blast radius; the
///   `graph_index` mirror produces the identical file edge via
///   [`crate::core::type_ref_edges::cross_file_type_edges`] so a reindex cannot
///   drop it — GH #398),
/// - file -> defined type symbol (clears the symbol from `dead_code`, whose
///   query already exempts `type_ref` targets; symbol-level edges live only on
///   this builder path).
fn insert_type_ref_edges(
    graph: &CodeGraph,
    file_node_id: i64,
    rel_path: &str,
    type_uses: &[crate::core::deep_queries::TypeUse],
    def_index: &DefIndex,
    scope: &crate::core::type_ref_edges::ResolveScope,
) -> usize {
    let mut added = 0usize;
    for (target_file, type_name, line_start, line_end) in
        crate::core::type_ref_edges::type_ref_targets(
            def_index,
            type_uses,
            rel_path,
            &scope.visible_ns,
            scope.allow_global_fallback,
        )
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

/// Insert `TypeRef` edges for resolved C# extension-method calls: a
/// `value.Foo()` call links the consuming file to the file that defines the
/// `this`-parameter method `Foo`. Mirrors `insert_type_ref_edges` (file +
/// symbol edge). Resolution is by method name alone, so the same self-filter
/// and a failsafe cap keep it bounded; the index only ever holds genuine
/// extension methods, which keeps the name space small and distinct.
fn insert_ext_method_edges(
    graph: &CodeGraph,
    file_node_id: i64,
    rel_path: &str,
    calls: &[crate::core::deep_queries::CallSite],
    ext_method_index: &ExtMethodIndex,
) -> usize {
    let mut added = 0usize;
    for (target_file, method_name, line_start, line_end) in
        crate::core::type_ref_edges::ext_method_targets(ext_method_index, calls, rel_path)
    {
        let Ok(target_id) = graph.upsert_node(&Node::file(&target_file)) else {
            continue;
        };
        let _ = graph.upsert_edge(&Edge::new(file_node_id, target_id, EdgeKind::TypeRef));
        added += 1;

        let sym_node = Node::symbol(
            &method_name,
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
    for (f, _ns, ls, le) in sites {
        if f == caller_file {
            return Some((f.clone(), *ls, *le));
        }
    }
    let mut sorted: Vec<(String, usize, usize)> = sites
        .iter()
        .map(|(f, _ns, ls, le)| (f.clone(), *ls, *le))
        .collect();
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
    ext_method_index: &ExtMethodIndex,
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

    // Type-usage edges close the same-namespace gap (C#/Java/Go/Kotlin,
    // GH #398): a file consuming a project type without importing it still
    // depends on the defining file. Scope is per-language (namespace-aware for
    // C#/Kotlin, directory-strict for Go).
    let scope = crate::core::type_ref_edges::resolve_scope(rel_path, ext, analysis);
    total_edges += insert_type_ref_edges(
        graph,
        file_node_id,
        rel_path,
        &analysis.type_uses,
        def_index,
        &scope,
    );
    // Extension-method calls (`value.Foo()`) depend on the defining file too.
    total_edges += insert_ext_method_edges(
        graph,
        file_node_id,
        rel_path,
        &analysis.calls,
        ext_method_index,
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
    ext_method_index: &ExtMethodIndex,
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

    // Same-namespace type consumption (C#/Java/Go/Kotlin, GH #398) — see the
    // embeddings-path counterpart in `index_graph_file_embeddings`.
    let scope = crate::core::type_ref_edges::resolve_scope(rel_path, ext, analysis);
    total_edges += insert_type_ref_edges(
        graph,
        file_node_id,
        rel_path,
        &analysis.type_uses,
        def_index,
        &scope,
    );
    total_edges += insert_ext_method_edges(
        graph,
        file_node_id,
        rel_path,
        &analysis.calls,
        ext_method_index,
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
        crate::core::agent_identity::hex_encode(&h.finalize())
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

    let (per_file, def_index, ext_method_index) = analyze_all(&file_contents);

    #[cfg(feature = "embeddings")]
    for (rel_path, _content, ext, analysis) in &per_file {
        let (n, e) = index_graph_file_embeddings(
            &graph,
            rel_path,
            ext,
            analysis,
            &resolver_ctx,
            &def_index,
            &ext_method_index,
        );
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
            &ext_method_index,
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
            project_root: crate::core::graph_index::normalize_project_root(root),
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

    let (per_file, def_index, ext_method_index) = analyze_all(&file_contents);

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
                &ext_method_index,
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
                &ext_method_index,
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
            project_root: crate::core::graph_index::normalize_project_root(root),
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

#[cfg(test)]
#[path = "ctx_impact_tests.rs"]
mod tests;
