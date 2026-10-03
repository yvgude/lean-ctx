use super::{GRAPH_SOURCE_EXTS, build, open_graph};
use crate::core::git_util::{git_dirty, git_out};
use crate::core::property_graph::{CodeGraph, DependencyChain, ImpactResult};
use crate::core::tokens::count_tokens;
use crate::tools::graph_meta::{graph_summary, project_meta};
use crate::tools::output_format::{OutputFormat, parse_format};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Stdio;

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
        "build" => build::handle_build(root, fmt),
        "update" => build::handle_update(root, fmt),
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
        let build_result = build::handle_build(root, OutputFormat::Text);
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
            // Bounded like `affected_files`: only the returned files' marks.
            let weak: Vec<&String> = sorted
                .iter()
                .filter(|f| impact.weak_files.binary_search(f).is_ok())
                .collect();
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
                "weak_files_total": impact.weak_files.len(),
                "weak_files": weak,
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
                if impact.weak_files.binary_search(file).is_ok() {
                    result.push_str(&format!("  {file}  (name match only)\n"));
                } else {
                    result.push_str(&format!("  {file}\n"));
                }
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
#[path = "../ctx_impact_tests.rs"]
mod tests;
