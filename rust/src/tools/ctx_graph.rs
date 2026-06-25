use std::collections::HashMap;
use std::path::Path;

use crate::core::graph_index;
use crate::core::graph_provider::{self, GraphProvider};
use crate::core::tokens::count_tokens;

#[allow(clippy::too_many_arguments)]
pub fn handle(
    action: &str,
    path: Option<&str>,
    root: &str,
    cache: &mut crate::core::cache::SessionCache,
    crp_mode: crate::tools::CrpMode,
    depth: Option<usize>,
    kind: Option<&str>,
    to: Option<&str>,
    format: Option<&str>,
    since: Option<&str>,
) -> String {
    match action {
        "build" => handle_build(root, format),
        "related" => handle_related(path, root),
        "symbol" => handle_symbol(path, root, cache, crp_mode),
        "impact" => handle_impact(path, root, format),
        "status" => handle_status(root),
        "enrich" => handle_enrich(root),
        "context" => handle_context_query(path, root),
        "diagram" => crate::tools::ctx_graph_diagram::handle(path, depth, kind, root),
        "neighbors" => crate::tools::ctx_graph_primitives::neighbors(path, root, depth, format),
        "path" => crate::tools::ctx_graph_primitives::shortest_path(path, to, root, format),
        "explain" => crate::tools::ctx_graph_primitives::explain(path, root, format),
        "diff" => crate::tools::ctx_graph_diff::diff(since, root, format),
        _ => "Unknown action. Use: build, related, symbol, impact, status, enrich, context, \
diagram, neighbors, path, explain, diff"
            .to_string(),
    }
}

fn handle_build(root: &str, format: Option<&str>) -> String {
    let handle =
        crate::core::index_pipeline::pipeline::IndexPipeline::new(std::path::PathBuf::from(root))
            .build()
            .expect("pipeline build failed");
    let (index, _) = handle.run_and_load().expect("pipeline run failed");

    if matches!(format, Some(f) if f.eq_ignore_ascii_case("json")) {
        let nodes_json: Vec<_> = index.files.values().map(|entry| {
            let name = entry.path.rsplit('/').next().unwrap_or(&entry.path);
            serde_json::json!({ "name": name, "file": entry.path })
        }).collect();

        let edges_json: Vec<_> = index
            .edges
            .iter()
            .map(|e| serde_json::json!({ "source": e.from, "target": e.to, "type": e.kind }))
            .collect();

        let val = serde_json::json!({
            "nodes": nodes_json,
            "edges": edges_json,
            "summary": {
                "files": index.file_count(),
                "symbols": index.symbol_count(),
                "edges": index.edge_count(),
            },
        });
        return serde_json::to_string_pretty(&val).unwrap_or_else(|_| "{}".to_string());
    }

    let mut by_lang: HashMap<&str, (usize, usize)> = HashMap::new();
    for entry in index.files.values() {
        let e = by_lang.entry(&entry.language).or_insert((0, 0));
        e.0 += 1;
        e.1 += entry.token_count;
    }

    let mut result = Vec::new();
    result.push(format!(
        "Project Graph: {} files, {} symbols, {} edges",
        index.file_count(),
        index.symbol_count(),
        index.edge_count()
    ));

    let mut langs: Vec<_> = by_lang.iter().collect();
    langs.sort_by_key(|(_, v)| std::cmp::Reverse(v.1));
    result.push("\nLanguages:".to_string());
    for (lang, (count, tokens)) in &langs {
        result.push(format!("  {lang}: {count} files, {tokens} tok"));
    }

    let mut import_counts: HashMap<&str, usize> = HashMap::new();
    for edge in &index.edges {
        if edge.kind == "import" {
            *import_counts.entry(&edge.to).or_insert(0) += 1;
        }
    }
    let mut hotspots: Vec<_> = import_counts.iter().collect();
    hotspots.sort_by_key(|x| std::cmp::Reverse(*x.1));

    if !hotspots.is_empty() {
        result.push(format!("\nMost imported ({}):", hotspots.len().min(10)));
        for (module, count) in hotspots.iter().take(10) {
            result.push(format!("  {module}: imported by {count} files"));
        }
    }

    if let Some(dir) = GraphProvider::index_dir(root) {
        result.push(format!(
            "\nIndex saved: {}",
            crate::core::protocol::shorten_path(&dir.to_string_lossy())
        ));
    }

    let output = result.join("\n");
    let tokens = count_tokens(&output);
    format!("{output}\n[ctx_graph build: {tokens} tok]")
}

fn handle_related(path: Option<&str>, root: &str) -> String {
    let Some(target) = path else {
        return "path is required for 'related' action".to_string();
    };

    let Some(open) = graph_provider::open_or_build(root) else {
        return "No graph index found. Run ctx_graph with action='build' first.".to_string();
    };

    let rel_target = graph_index::graph_relative_key(target, root);

    let related = open.provider.related(&rel_target, 2);
    if related.is_empty() {
        return format!(
            "No related files found for {}",
            crate::core::protocol::shorten_path(target)
        );
    }

    let mut result = format!(
        "Files related to {} ({}):\n",
        crate::core::protocol::shorten_path(target),
        related.len()
    );
    for r in &related {
        result.push_str(&format!("  {}\n", crate::core::protocol::shorten_path(r)));
    }

    let tokens = count_tokens(&result);
    format!("{result}[ctx_graph related: {tokens} tok]")
}

fn handle_symbol(
    path: Option<&str>,
    root: &str,
    cache: &mut crate::core::cache::SessionCache,
    crp_mode: crate::tools::CrpMode,
) -> String {
    let Some(spec) = path else {
        return "path is required for 'symbol' action (format: <file>::<symbol>, or a bare <symbol> name)".to_string();
    };

    let Some(open) = graph_provider::open_or_build(root) else {
        return "No graph index found. Run ctx_graph with action='build' first.".to_string();
    };

    // Bare symbol name (no `::`): resolve against the symbol table so GDScript
    // (and every other language) symbols are reachable without a file qualifier
    // (#314).
    let Some((file_part, symbol_name)) = spec.split_once("::") else {
        return resolve_bare_symbol(&open.provider, spec, root, cache, crp_mode);
    };

    let rel_file = graph_index::graph_relative_key(file_part, root);

    let key = format!("{rel_file}::{symbol_name}");
    let Some(symbol) = open.provider.get_symbol(&key) else {
        let available = open
            .provider
            .find_symbols(symbol_name, Some(&rel_file), None);
        if available.is_empty() {
            return format!(
                "Symbol '{symbol_name}' not found in {rel_file}. Run ctx_graph action='build' to update the index."
            );
        }
        let names: Vec<String> = available
            .iter()
            .take(10)
            .map(|s| format!("{}::{}", s.file, s.name))
            .collect();
        return format!(
            "Symbol '{symbol_name}' not found in {rel_file}.\nAvailable symbols:\n  {}",
            names.join("\n  ")
        );
    };

    let abs_path = if Path::new(file_part).is_absolute() {
        file_part.to_string()
    } else {
        Path::new(root)
            .join(rel_file.trim_start_matches(['/', '\\']))
            .to_string_lossy()
            .to_string()
    };

    render_symbol_snippet(&symbol, &abs_path, &rel_file, cache, crp_mode)
}

/// Resolve a bare symbol name (no `<file>::` qualifier) against the symbol table.
/// One unambiguous hit renders the snippet; otherwise the candidates are listed
/// so the caller can disambiguate with `<file>::<symbol>` (#314).
fn resolve_bare_symbol(
    provider: &GraphProvider,
    name: &str,
    root: &str,
    cache: &mut crate::core::cache::SessionCache,
    crp_mode: crate::tools::CrpMode,
) -> String {
    let matches = provider.find_symbols(name, None, None);
    if matches.is_empty() {
        return format!(
            "Symbol '{name}' not found. Run ctx_graph action='build' to update the index."
        );
    }

    let name_lower = name.to_lowercase();
    let exact: Vec<&graph_provider::SymbolInfo> = matches
        .iter()
        .filter(|s| s.name.to_lowercase() == name_lower)
        .collect();

    if let [only] = exact.as_slice() {
        let abs_path = Path::new(root)
            .join(only.file.trim_start_matches(['/', '\\']))
            .to_string_lossy()
            .to_string();
        return render_symbol_snippet(only, &abs_path, &only.file, cache, crp_mode);
    }

    // Several identically-named symbols, or only substring hits → list them.
    let shortlist: Vec<&graph_provider::SymbolInfo> = if exact.is_empty() {
        matches.iter().collect()
    } else {
        exact
    };
    let mut lines = vec![format!(
        "Symbol '{name}' matches {} entries — pick one with `<file>::{name}`:",
        shortlist.len()
    )];
    for s in shortlist.iter().take(15) {
        lines.push(format!(
            "  {}::{} ({}, {}:{})",
            crate::core::protocol::shorten_path(&s.file),
            s.name,
            s.kind,
            s.start_line,
            s.end_line
        ));
    }
    lines.join("\n")
}

/// Render a symbol's source snippet with the standard token-savings footer.
/// Shared by the qualified (`<file>::<symbol>`) and bare-name resolution paths.
fn render_symbol_snippet(
    symbol: &graph_provider::SymbolInfo,
    abs_path: &str,
    rel_display: &str,
    cache: &mut crate::core::cache::SessionCache,
    crp_mode: crate::tools::CrpMode,
) -> String {
    let content = match std::fs::read_to_string(abs_path) {
        Ok(c) => c,
        Err(e) => return format!("Cannot read {abs_path}: {e}"),
    };

    let lines: Vec<&str> = content.lines().collect();
    let start = symbol.start_line.saturating_sub(1);
    let end = symbol.end_line.min(lines.len());

    if start >= lines.len() {
        return crate::tools::ctx_read::handle(cache, abs_path, "full", crp_mode);
    }

    let mut result = format!(
        "{}::{} ({}:{}-{})\n",
        crate::core::protocol::shorten_path(rel_display),
        symbol.name,
        symbol.kind,
        symbol.start_line,
        symbol.end_line
    );

    for (i, line) in lines[start..end].iter().enumerate() {
        result.push_str(&format!("{:>4}|{}\n", start + i + 1, line));
    }

    let tokens = count_tokens(&result);
    let full_tokens = count_tokens(&content);
    let saved = full_tokens.saturating_sub(tokens);
    let pct = if full_tokens > 0 {
        (saved as f64 / full_tokens as f64 * 100.0).round() as usize
    } else {
        0
    };

    format!("{result}[ctx_graph symbol: {tokens} tok (full file: {full_tokens} tok, -{pct}%)]")
}

fn file_path_to_module_prefixes(
    rel_path: &str,
    project_root: &str,
    provider: &GraphProvider,
) -> Vec<String> {
    let rel_path_slash = graph_index::graph_match_key(rel_path);
    let without_ext = rel_path_slash
        .strip_suffix(".rs")
        .or_else(|| rel_path_slash.strip_suffix(".ts"))
        .or_else(|| rel_path_slash.strip_suffix(".tsx"))
        .or_else(|| rel_path_slash.strip_suffix(".js"))
        .or_else(|| rel_path_slash.strip_suffix(".py"))
        .or_else(|| rel_path_slash.strip_suffix(".kt"))
        .or_else(|| rel_path_slash.strip_suffix(".kts"))
        .or_else(|| rel_path_slash.strip_suffix(".gd"))
        .unwrap_or(&rel_path_slash);

    let module_path = without_ext
        .strip_prefix("src/")
        .unwrap_or(without_ext)
        .replace('/', "::");

    let module_path = if module_path.ends_with("::mod") {
        module_path
            .strip_suffix("::mod")
            .unwrap_or(&module_path)
            .to_string()
    } else {
        module_path
    };

    let crate_name = std::fs::read_to_string(Path::new(project_root).join("Cargo.toml"))
        .or_else(|_| std::fs::read_to_string(Path::new(project_root).join("package.json")))
        .ok()
        .and_then(|c| {
            c.lines()
                .find(|l| l.contains("\"name\"") || l.starts_with("name"))
                .and_then(|l| l.split('"').nth(1))
                .map(|n| n.replace('-', "_"))
        })
        .unwrap_or_default();

    let mut prefixes = vec![
        format!("crate::{module_path}"),
        format!("super::{module_path}"),
        module_path.clone(),
    ];
    if !crate_name.is_empty() {
        prefixes.insert(0, format!("{crate_name}::{module_path}"));
    }

    let ext = Path::new(rel_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    if ext == "gd" {
        // GDScript import edges resolve to the target script's project-relative
        // path (e.g. "actors/Player.gd"), not a Rust-style module path, so the
        // path itself is the key that `graph impact` must match on (#314).
        prefixes.push(rel_path_slash.clone());
    }
    if matches!(ext, "kt" | "kts") {
        let abs_path = Path::new(project_root).join(rel_path.trim_start_matches(['/', '\\']));
        if let Ok(content) = std::fs::read_to_string(abs_path)
            && let Some(package_name) = content.lines().map(str::trim).find_map(|line| {
                line.strip_prefix("package ")
                    .map(|rest| rest.trim().trim_end_matches(';').to_string())
            })
        {
            prefixes.push(package_name.clone());
            if let Some(entry) = provider.get_file_entry(rel_path) {
                for export in &entry.exports {
                    prefixes.push(format!("{package_name}.{export}"));
                }
            }
            if let Some(file_stem) = Path::new(rel_path).file_stem().and_then(|s| s.to_str()) {
                prefixes.push(format!("{package_name}.{file_stem}"));
            }
        }
    }

    prefixes.sort();
    prefixes.dedup();
    prefixes
}

fn edge_matches_file(edge_to: &str, module_prefixes: &[String]) -> bool {
    module_prefixes.iter().any(|prefix| {
        edge_to == *prefix
            || edge_to.starts_with(&format!("{prefix}::"))
            || edge_to.starts_with(&format!("{prefix},"))
    })
}

fn handle_impact(path: Option<&str>, root: &str, format: Option<&str>) -> String {
    let Some(target) = path else {
        return "path is required for 'impact' action".to_string();
    };

    let Some(open) = graph_provider::open_or_build(root) else {
        return "No graph index found. Run ctx_graph with action='build' first.".to_string();
    };
    let gp = &open.provider;

    let rel_target = graph_index::graph_relative_key(target, root);
    let module_prefixes = file_path_to_module_prefixes(&rel_target, root, gp);

    // Direct importers come from two complementary lookups, merged + deduped:
    //   1. `dependents(rel_target)` — the import resolver records edges keyed by
    //      the target's project-relative *file path*, so this is the primary,
    //      backend-agnostic match (works for Rust/TS/JS/Python/…).
    //   2. `edge_matches_file` over module-path prefixes — additionally catches
    //      edges keyed by a module/symbol path rather than a file (Rust `mod`,
    //      Kotlin package, barrel re-exports), which the file-path match misses.
    let mut direct: Vec<String> = gp.dependents(&rel_target);
    for e in gp.edges_by_kind("import") {
        if edge_matches_file(&e.to, &module_prefixes) && !direct.contains(&e.from) {
            direct.push(e.from);
        }
    }
    direct.retain(|d| *d != rel_target);

    let mut all_dependents: Vec<String> = direct.clone();
    for d in &direct {
        for dep in gp.dependents(d) {
            if !all_dependents.contains(&dep) && dep != rel_target {
                all_dependents.push(dep);
            }
        }
    }

    if all_dependents.is_empty() {
        if matches!(format, Some(f) if f.eq_ignore_ascii_case("json")) {
            let name = rel_target.rsplit('/').next().unwrap_or(&rel_target);
            return serde_json::json!({
                "nodes": [{ "name": name, "file": rel_target }],
                "edges": [],
                "impact": { "target": rel_target, "direct": 0, "total": 0 },
            }).to_string();
        }
        return format!(
            "No files depend on {}",
            crate::core::protocol::shorten_path(target)
        );
    }

    if matches!(format, Some(f) if f.eq_ignore_ascii_case("json")) {
        let mut all_set: std::collections::HashSet<String> = std::collections::HashSet::new();
        all_set.insert(rel_target.clone());
        for d in &all_dependents {
            all_set.insert(d.clone());
        }

        let nodes_json: Vec<_> =
            {
                let mut sorted: Vec<_> = all_set.iter().collect();
                sorted.sort();
                sorted.iter().map(|n| {
                let name = n.rsplit('/').next().unwrap_or(n);
                serde_json::json!({ "name": name, "file": n })
            }).collect()
            };

        let edges_json: Vec<_> = {
            let all_edges = gp.edges();
            let deps_set: std::collections::HashSet<&str> =
                all_dependents.iter().map(|s| s.as_str()).collect();
            all_edges
                .iter()
                .filter(|e| deps_set.contains(e.from.as_str()) && e.to == rel_target)
                .map(|e| serde_json::json!({ "source": e.from, "target": e.to, "type": e.kind }))
                .collect()
        };

        let indirect: Vec<&String> = all_dependents
            .iter()
            .filter(|d| !direct.contains(*d))
            .collect();
        let val = serde_json::json!({
            "nodes": nodes_json,
            "edges": edges_json,
            "impact": { "target": rel_target, "direct": direct.len(), "total": all_dependents.len() },
            "direct": direct,
            "indirect": indirect,
        });
        return serde_json::to_string_pretty(&val).unwrap_or_else(|_| "{}".to_string());
    }

    let mut result = format!(
        "Impact of {} ({} dependents):\n",
        crate::core::protocol::shorten_path(target),
        all_dependents.len()
    );

    if !direct.is_empty() {
        result.push_str(&format!("\nDirect ({}):\n", direct.len()));
        for d in &direct {
            result.push_str(&format!("  {}\n", crate::core::protocol::shorten_path(d)));
        }
    }

    let indirect: Vec<&String> = all_dependents
        .iter()
        .filter(|d| !direct.contains(d))
        .collect();
    if !indirect.is_empty() {
        result.push_str(&format!("\nIndirect ({}):\n", indirect.len()));
        for d in &indirect {
            result.push_str(&format!("  {}\n", crate::core::protocol::shorten_path(d)));
        }
    }

    let tokens = count_tokens(&result);
    format!("{result}[ctx_graph impact: {tokens} tok]")
}

fn handle_status(root: &str) -> String {
    let Some(open) = graph_provider::open_best_effort(root) else {
        return "No graph index. Run ctx_graph action='build' to create one.".to_string();
    };
    let gp = &open.provider;

    let file_paths = gp.file_paths();
    let mut by_lang: HashMap<String, usize> = HashMap::new();
    let mut total_tokens = 0usize;
    for path in &file_paths {
        if let Some(entry) = gp.get_file_entry(path) {
            *by_lang.entry(entry.language).or_insert(0) += 1;
            total_tokens += entry.token_count;
        }
    }

    let mut langs: Vec<_> = by_lang.iter().collect();
    langs.sort_by_key(|item| std::cmp::Reverse(*item.1));
    let lang_summary: String = langs
        .iter()
        .take(5)
        .map(|(l, c)| format!("{l}:{c}"))
        .collect::<Vec<_>>()
        .join(" ");

    format!(
        "Graph: {} files, {} symbols, {} edges ({:?}) | {} tok total\nLast scan: {}\nLanguages: {lang_summary}\nStored: {}",
        gp.file_count(),
        gp.symbol_count(),
        gp.edge_count().unwrap_or(0),
        open.source,
        total_tokens,
        gp.last_scan(),
        GraphProvider::index_dir(root)
            .map(|d| d.to_string_lossy().to_string())
            .unwrap_or_default()
    )
}

fn resolve_node_name(graph: &crate::core::property_graph::CodeGraph, node_id: i64) -> String {
    let conn = graph.connection();
    conn.query_row(
        "SELECT name FROM nodes WHERE id = ?1",
        rusqlite::params![node_id],
        |row| row.get::<_, String>(0),
    )
    .unwrap_or_else(|_| format!("node#{node_id}"))
}

fn handle_enrich(root: &str) -> String {
    let graph = match crate::core::property_graph::CodeGraph::open(root) {
        Ok(g) => g,
        Err(e) => return format!("Failed to open graph: {e}"),
    };

    match crate::core::graph_enricher::enrich_graph(&graph, Path::new(root), 500) {
        Ok(stats) => {
            let node_count = graph.node_count().unwrap_or(0);
            let edge_count = graph.edge_count().unwrap_or(0);
            format!(
                "Graph enriched.\n{}\nTotal: {node_count} nodes, {edge_count} edges",
                stats.format_summary()
            )
        }
        Err(e) => format!("Enrichment failed: {e}"),
    }
}

fn handle_context_query(query: Option<&str>, root: &str) -> String {
    let Some(query) = query else {
        return "Usage: ctx_graph action=context path=\"<query or file path>\"".to_string();
    };

    let graph = match crate::core::property_graph::CodeGraph::open(root) {
        Ok(g) => g,
        Err(e) => return format!("Failed to open graph: {e}"),
    };

    let gp = graph_provider::open_or_build(root);
    let mut result = Vec::new();

    if let Ok(Some(node)) = graph.get_node_by_path(query) {
        result.push(format!("## Context for `{query}`\n"));

        if let Some(node_id) = node.id {
            let edges_out = graph.edges_from(node_id).unwrap_or_default();
            let edges_in = graph.edges_to(node_id).unwrap_or_default();

            let mut tests: Vec<String> = Vec::new();
            let mut commits: Vec<String> = Vec::new();
            let mut knowledge: Vec<String> = Vec::new();
            let mut imports: Vec<String> = Vec::new();
            let mut dependents: Vec<String> = Vec::new();

            for edge in &edges_out {
                let target = resolve_node_name(&graph, edge.target_id);
                match edge.kind {
                    crate::core::property_graph::EdgeKind::TestedBy => tests.push(target),
                    crate::core::property_graph::EdgeKind::ChangedIn => commits.push(target),
                    crate::core::property_graph::EdgeKind::MentionedIn => {
                        knowledge.push(target);
                    }
                    crate::core::property_graph::EdgeKind::Imports => imports.push(target),
                    _ => {}
                }
            }

            for edge in &edges_in {
                let source = resolve_node_name(&graph, edge.source_id);
                if edge.kind == crate::core::property_graph::EdgeKind::Imports {
                    dependents.push(source);
                }
            }

            if !tests.is_empty() {
                result.push(format!("**Tests ({}):** {}", tests.len(), tests.join(", ")));
            }
            if !commits.is_empty() {
                result.push(format!(
                    "**Recent commits ({}):** {}",
                    commits.len(),
                    commits
                        .iter()
                        .take(5)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if !knowledge.is_empty() {
                result.push(format!(
                    "**Knowledge ({}):** {}",
                    knowledge.len(),
                    knowledge.join(", ")
                ));
            }
            if !imports.is_empty() {
                result.push(format!(
                    "**Imports ({}):** {}",
                    imports.len(),
                    imports
                        .iter()
                        .take(10)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if !dependents.is_empty() {
                result.push(format!(
                    "**Depended on by ({}):** {}",
                    dependents.len(),
                    dependents
                        .iter()
                        .take(10)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }

            if let Ok(impact) = graph.impact_analysis(query, 3)
                && !impact.affected_files.is_empty()
            {
                result.push(format!(
                    "**Impact radius:** {} files within 3 hops",
                    impact.affected_files.len()
                ));
            }
        }
    } else {
        result.push(format!("## Search: `{query}`\n"));

        // Symbol names (e.g. GDScript `_ready`, or any function/type) live in the
        // GraphIndex, not the PropertyGraph node table, so resolve them here — a
        // bare concept query should return hits instead of "nothing found" (#314).
        let mut symbols = gp
            .as_ref()
            .map(|o| o.provider.find_symbols(query, None, None))
            .unwrap_or_default();
        let q_lower = query.to_lowercase();
        symbols.sort_by(|a, b| {
            (a.name.to_lowercase() != q_lower)
                .cmp(&(b.name.to_lowercase() != q_lower))
                .then_with(|| a.file.cmp(&b.file))
                .then_with(|| a.start_line.cmp(&b.start_line))
        });
        if !symbols.is_empty() {
            result.push(format!("**Symbols ({}):**", symbols.len()));
            for s in symbols.iter().take(15) {
                result.push(format!(
                    "  - {}::{} ({}, {}:{})",
                    crate::core::protocol::shorten_path(&s.file),
                    s.name,
                    s.kind,
                    s.start_line,
                    s.end_line
                ));
            }
        }

        let related = gp
            .as_ref()
            .map(|o| o.provider.related(query, 2))
            .unwrap_or_default();
        if !related.is_empty() {
            result.push(format!("**Related files ({}):**", related.len()));
            for f in related.iter().take(15) {
                result.push(format!("  - {f}"));
            }
        }

        if symbols.is_empty() && related.is_empty() {
            result.push("No matching nodes found in graph.".to_string());
        }
    }

    result.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_edge_matches_file_crate_prefix() {
        let prefixes = vec![
            "lean_ctx::core::cache".to_string(),
            "crate::core::cache".to_string(),
            "super::core::cache".to_string(),
            "core::cache".to_string(),
        ];
        assert!(edge_matches_file(
            "lean_ctx::core::cache::SessionCache",
            &prefixes
        ));
        assert!(edge_matches_file(
            "crate::core::cache::SessionCache",
            &prefixes
        ));
        assert!(edge_matches_file("crate::core::cache", &prefixes));
        assert!(!edge_matches_file(
            "lean_ctx::core::config::Config",
            &prefixes
        ));
        assert!(!edge_matches_file("crate::core::cached_reader", &prefixes));
    }

    #[test]
    fn test_file_path_to_module_prefixes_rust() {
        let gp =
            GraphProvider::GraphIndex(crate::core::graph_index::ProjectIndex::new("/nonexistent"));
        let prefixes = file_path_to_module_prefixes("src/core/cache.rs", "/nonexistent", &gp);
        assert!(prefixes.contains(&"crate::core::cache".to_string()));
        assert!(prefixes.contains(&"core::cache".to_string()));
    }

    #[test]
    fn test_file_path_to_module_prefixes_mod_rs() {
        let gp =
            GraphProvider::GraphIndex(crate::core::graph_index::ProjectIndex::new("/nonexistent"));
        let prefixes = file_path_to_module_prefixes("src/core/mod.rs", "/nonexistent", &gp);
        assert!(prefixes.contains(&"crate::core".to_string()));
        assert!(!prefixes.iter().any(|p| p.contains("mod")));
    }

    #[test]
    fn test_file_path_to_module_prefixes_gd_uses_path() {
        // GDScript import edges store the resolved project-relative path, so the
        // path itself must be among the prefixes `graph impact` matches on (#314).
        let gp =
            GraphProvider::GraphIndex(crate::core::graph_index::ProjectIndex::new("/nonexistent"));
        let prefixes = file_path_to_module_prefixes("actors/Player.gd", "/nonexistent", &gp);
        assert!(
            prefixes.contains(&"actors/Player.gd".to_string()),
            "got: {prefixes:?}"
        );
    }

    #[test]
    fn test_edge_matches_file_gd_path() {
        let prefixes = vec!["actors/Base.gd".to_string()];
        assert!(edge_matches_file("actors/Base.gd", &prefixes));
        assert!(!edge_matches_file("actors/Enemy.gd", &prefixes));
    }
}

/// End-to-end GDScript graph coverage on a real Godot fixture (#314): four `.gd`
/// scripts with `_ready` definitions and `res://` import edges, exercised through
/// the public graph actions (context / impact / bare symbol).
#[cfg(test)]
mod gdscript_p0_tests {
    use super::*;

    fn write(root: &std::path::Path, rel: &str, content: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    /// Minimal Godot project: `Player`/`Enemy` extend `Base`, `main` preloads
    /// `Player`; every script defines `_ready`. Returns (tempdir guard, root).
    fn godot_fixture() -> (tempfile::TempDir, String) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.to_string_lossy().to_string());

        let proj = tmp.path().join("game");
        std::fs::create_dir_all(&proj).unwrap();
        write(
            &proj,
            "project.godot",
            "[application]\nconfig/name=\"Fixture\"\n",
        );
        write(
            &proj,
            "actors/Base.gd",
            "extends Node\n\nfunc _ready():\n\tpass\n",
        );
        write(
            &proj,
            "actors/Player.gd",
            "extends \"res://actors/Base.gd\"\n\nfunc _ready():\n\tprint(\"player\")\n",
        );
        write(
            &proj,
            "actors/Enemy.gd",
            "extends \"res://actors/Base.gd\"\n\nfunc _ready():\n\tprint(\"enemy\")\n",
        );
        write(
            &proj,
            "main.gd",
            "const Player = preload(\"res://actors/Player.gd\")\n\nfunc _ready():\n\tprint(\"main\")\n",
        );

        (tmp, proj.to_string_lossy().to_string())
    }

    #[test]
    fn context_resolves_gdscript_symbol() {
        let _lock = crate::core::data_dir::test_env_lock();
        let (_tmp, root) = godot_fixture();
        let _ = handle_build(&root, None);
        let out = handle_context_query(Some("_ready"), &root);
        assert!(
            out.contains("_ready"),
            "context should surface _ready symbols: {out}"
        );
        assert!(!out.contains("No matching nodes found"), "got: {out}");
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }

    #[test]
    fn impact_lists_gdscript_dependents() {
        let _lock = crate::core::data_dir::test_env_lock();
        let (_tmp, root) = godot_fixture();
        let _ = handle_build(&root, None);
        let out = handle_impact(Some("actors/Base.gd"), &root, None);
        assert!(
            out.contains("Player.gd"),
            "Base.gd dependents should include Player: {out}"
        );
        assert!(
            out.contains("Enemy.gd"),
            "Base.gd dependents should include Enemy: {out}"
        );
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }

    #[test]
    fn bare_symbol_resolves_gdscript() {
        let _lock = crate::core::data_dir::test_env_lock();
        let (_tmp, root) = godot_fixture();
        let _ = handle_build(&root, None);
        let mut cache = crate::core::cache::SessionCache::new();
        let out = handle_symbol(
            Some("_ready"),
            &root,
            &mut cache,
            crate::tools::CrpMode::Off,
        );
        // Four `_ready` defs → a disambiguation list (or a snippet), never the
        // pre-#314 "Invalid symbol spec" / "not found" errors.
        assert!(out.contains("_ready"), "bare symbol should resolve: {out}");
        assert!(!out.contains("Invalid symbol spec"), "got: {out}");
        assert!(!out.to_lowercase().contains("not found"), "got: {out}");
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }
}
