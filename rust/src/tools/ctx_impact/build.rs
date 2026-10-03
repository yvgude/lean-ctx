use super::{GRAPH_SOURCE_EXTS, open_graph};
use crate::core::git_util::{git_dirty, git_out};
use crate::core::property_graph::{CodeGraph, Edge, EdgeKind, Node};
use crate::core::tokens::count_tokens;
use crate::core::type_ref_edges::{DefIndex, ExtMethodIndex};
use crate::tools::graph_meta::{graph_summary, project_meta};
use crate::tools::output_format::OutputFormat;
use serde_json::json;
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Stdio;

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
/// Binds a callee name to its definition site in the caller's scope — the
/// same ladder as the call graph (`call_graph::rank_callee_def_file`): the
/// caller's own file, then exactly one imported file, then exactly one file
/// project-wide. Ambiguous names yield `None`; the previous fallback picked
/// the alphabetically first file and invented edges.
#[cfg_attr(not(feature = "embeddings"), allow(dead_code))]
fn resolve_call_callee_site(
    def_index: &DefIndex,
    callee: &str,
    caller_file: &str,
    imported: &[String],
) -> Option<(String, usize, usize, crate::core::semantic::EvidenceGrade)> {
    use crate::core::semantic::EvidenceGrade;
    let sites = def_index.get(callee)?;
    // The single definition of `callee` in `file`; several (other namespace,
    // other `impl` block) cannot be told apart without type information.
    let first_in = |file: &str| {
        let mut in_file = sites.iter().filter(|(f, ..)| f == file);
        let (f, _, ls, le) = in_file.next()?;
        in_file.next().is_none().then(|| (f.clone(), *ls, *le))
    };
    let mut files: Vec<&str> = sites.iter().map(|(f, ..)| f.as_str()).collect();
    files.sort_unstable();
    files.dedup();

    if files.contains(&caller_file) {
        // Defined in the caller's own file: bind it there, or — when that
        // file defines it several times — refuse rather than look elsewhere.
        let (f, ls, le) = first_in(caller_file)?;
        return Some((f, ls, le, EvidenceGrade::ResolvedStructural));
    }
    let in_scope: Vec<&str> = files
        .iter()
        .copied()
        .filter(|f| imported.iter().any(|i| i == f))
        .collect();
    if let [only] = in_scope.as_slice() {
        let (f, ls, le) = first_in(only)?;
        return Some((f, ls, le, EvidenceGrade::ResolvedStructural));
    }
    if let [only] = files.as_slice() {
        let (f, ls, le) = first_in(only)?;
        return Some((f, ls, le, EvidenceGrade::HeuristicStructural));
    }
    None
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

    for target_path in &targets {
        let Ok(target_id) = graph.upsert_node(&Node::file(target_path)) else {
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

        let Some((callee_file, c_line, c_end, grade)) =
            resolve_call_callee_site(def_index, &call.callee, rel_path, &targets)
        else {
            continue;
        };
        let evidence = crate::core::semantic::EdgeEvidence::new(
            grade,
            crate::core::semantic::EvidenceOrigin::ImpactIndex,
            None,
            1,
        );

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
        let _ = graph.upsert_edge_with_evidence(caller_id, callee_id, &EdgeKind::Calls, &evidence);
        total_edges += 1;

        if callee_file != rel_path {
            let Ok(callee_file_id) = graph.upsert_node(&Node::file(&callee_file)) else {
                continue;
            };
            let _ = graph.upsert_edge_with_evidence(
                file_node_id,
                callee_file_id,
                &EdgeKind::Calls,
                &evidence,
            );
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

pub(super) fn handle_build(root: &str, fmt: OutputFormat) -> String {
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

pub(super) fn handle_update(root: &str, fmt: OutputFormat) -> String {
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

#[cfg(test)]
mod tests {
    use super::resolve_call_callee_site;
    use crate::core::semantic::EvidenceGrade;
    use crate::core::type_ref_edges::DefIndex;

    /// Regression: an ambiguous callee used to bind to the alphabetically
    /// first defining file, inventing `calls` edges in the impact graph.
    #[test]
    fn callee_site_follows_caller_scope_and_never_guesses() {
        let mut index = DefIndex::new();
        index.insert(
            "save".into(),
            vec![("a.rs".into(), None, 3, 5), ("b.rs".into(), None, 7, 9)],
        );
        index.insert("only".into(), vec![("u.rs".into(), None, 1, 2)]);

        let none: [String; 0] = [];
        assert_eq!(
            resolve_call_callee_site(&index, "save", "c.rs", &none),
            None
        );
        assert_eq!(
            resolve_call_callee_site(&index, "save", "c.rs", &["b.rs".to_string()]),
            Some(("b.rs".into(), 7, 9, EvidenceGrade::ResolvedStructural))
        );
        assert_eq!(
            resolve_call_callee_site(&index, "save", "a.rs", &none),
            Some(("a.rs".into(), 3, 5, EvidenceGrade::ResolvedStructural))
        );
        assert_eq!(
            resolve_call_callee_site(&index, "only", "c.rs", &none),
            Some(("u.rs".into(), 1, 2, EvidenceGrade::HeuristicStructural))
        );

        // Two `Widget`s in the caller's own file (N1/N2 namespaces): refuse,
        // and never fall through to an imported file's `Widget`.
        index.insert(
            "Widget".into(),
            vec![
                ("w.cs".into(), Some("N1".into()), 3, 5),
                ("w.cs".into(), Some("N2".into()), 9, 11),
                ("lib.cs".into(), None, 1, 2),
            ],
        );
        assert_eq!(
            resolve_call_callee_site(&index, "Widget", "w.cs", &["lib.cs".to_string()]),
            None
        );
    }
}
