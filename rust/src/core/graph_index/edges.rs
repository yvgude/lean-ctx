//! Edge construction for the project graph index (import/module, co-change,
//! sibling, barrel and language-specific edges). Split out of `graph_index/mod.rs`.
//! `use super::*` re-imports the parent module’s types and helpers.

// Several edge-builders are called from test-only entry points (`tests.rs`);
// suppress dead-code since the call tree is compiled in test builds only.
#![allow(dead_code)]

use std::sync::Arc;

use rayon::prelude::*;

use super::{HashMap, IndexEdge, Path, ProjectIndex, import_resolver, normalize_project_root};

pub(crate) fn build_edges_cached(
    index: &mut ProjectIndex,
    content_cache: &HashMap<String, Arc<String>>,
) {
    build_edges_with_cache(index, content_cache);
    build_implicit_edges_with_cache(index, content_cache);
    build_cochange_edges(index);
    build_sibling_edges(index);
}

fn build_edges_with_cache(index: &mut ProjectIndex, content_cache: &HashMap<String, Arc<String>>) {
    index.edges.clear();

    if crate::core::memory_guard::abort_requested() {
        tracing::warn!("[graph_index: skipping edge-building due to memory pressure]");
        return;
    }

    let root = normalize_project_root(&index.project_root);
    let root_path = Path::new(&root);

    let mut file_paths: Vec<String> = index.files.keys().cloned().collect();
    file_paths.sort();

    let resolver_ctx =
        import_resolver::ResolverContext::new(root_path, file_paths.clone(), content_cache);

    const MAX_FILE_SIZE_FOR_EDGES: u64 = 2 * 1024 * 1024;

    // Each file's import resolution is independent — parallelise the outer
    // loop with rayon.  Per-file results are flat-merged and deduplicated.
    let import_edges: Vec<IndexEdge> = file_paths
        .par_iter()
        .flat_map(|rel_path| {
            let content = if let Some(cached) = content_cache.get(rel_path) {
                std::borrow::Cow::Borrowed(cached.as_str())
            } else {
                let abs_path = root_path.join(rel_path.trim_start_matches(['/', '\\']));
                if let Ok(meta) = abs_path.metadata()
                    && meta.len() > MAX_FILE_SIZE_FOR_EDGES
                {
                    return Vec::new();
                }
                match std::fs::read_to_string(&abs_path) {
                    Ok(c) => std::borrow::Cow::Owned(c),
                    Err(_) => return Vec::new(),
                }
            };

            let ext = Path::new(rel_path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");

            // Godot scenes carry their dependencies in `[ext_resource]` headers
            let (resolve_ext, imports) = if ext == "tscn" {
                (
                    "tscn",
                    crate::core::godot::scene::extract_scene_imports(&content),
                )
            } else {
                let resolve_ext = match ext {
                    "vue" | "svelte" => "ts",
                    _ => ext,
                };

                let analysis_content = if ext == "vue" || ext == "svelte" {
                    if let Some(script) =
                        crate::core::signatures_ts::sfc::extract_script_block(&content)
                    {
                        std::borrow::Cow::Owned(script)
                    } else {
                        content
                    }
                } else {
                    content
                };

                let imports =
                    crate::core::deep_queries::analyze(&analysis_content, resolve_ext).imports;
                (resolve_ext, imports)
            };

            if imports.is_empty() {
                return Vec::new();
            }

            let resolved =
                import_resolver::resolve_imports(&imports, rel_path, resolve_ext, &resolver_ctx);
            let mut file_edges: Vec<IndexEdge> = Vec::new();
            for r in resolved {
                if r.is_external {
                    continue;
                }
                if let Some(to) = r.resolved_path {
                    file_edges.push(IndexEdge {
                        from: rel_path.clone(),
                        to,
                        kind: "import".to_string(),
                        weight: 1.0,
                    });
                }
            }
            file_edges
        })
        .collect();

    index.edges.extend(import_edges);
    index.edges.sort_by(|a, b| {
        a.from
            .cmp(&b.from)
            .then_with(|| a.to.cmp(&b.to))
            .then_with(|| a.kind.cmp(&b.kind))
    });
    index
        .edges
        .dedup_by(|a, b| a.from == b.from && a.to == b.to && a.kind == b.kind);
}

// ---------------------------------------------------------------------------
// Layer 2: Implicit Language Edges (weight 0.8)
// ---------------------------------------------------------------------------

fn build_implicit_edges_with_cache(
    index: &mut ProjectIndex,
    content_cache: &HashMap<String, Arc<String>>,
) {
    let file_paths: Vec<String> = index.files.keys().cloned().collect();
    let file_set: std::collections::HashSet<&str> = file_paths.iter().map(String::as_str).collect();

    let existing_edges = std::mem::take(&mut index.edges);

    let (implicit_edges, csharp_edges): (Vec<IndexEdge>, Vec<IndexEdge>) = {
        let idx: &ProjectIndex = &*index;

        let parallel_edges: Vec<IndexEdge> = file_paths
            .par_iter()
            .flat_map(|file| {
                let ext = Path::new(file.as_str())
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("");

                let mut file_edges: Vec<IndexEdge> = Vec::new();
                match ext {
                    "rs" => {
                        collect_rust_mod_edges_cached(
                            file,
                            &file_set,
                            idx,
                            &mut file_edges,
                            content_cache,
                        );
                    }
                    "go" => collect_go_package_edges(file, &file_paths, &mut file_edges),
                    "py" => collect_python_init_edges(file, &file_paths, &mut file_edges),
                    "ts" | "js" | "tsx" | "jsx" => {
                        collect_barrel_edges_cached(
                            file,
                            &file_set,
                            idx,
                            &mut file_edges,
                            content_cache,
                        );
                    }
                    _ => {}
                }
                file_edges
            })
            .collect();

        // C# namespace cohesion is computed in a single pass over all `.cs` files
        // (grouping needs every file), rather than per-file inside the loop above.
        let mut cs_edges: Vec<IndexEdge> = Vec::new();
        collect_csharp_namespace_edges(&file_paths, idx, &mut cs_edges, content_cache);

        (parallel_edges, cs_edges)
    };

    // Restore the original edges and fold in the new ones.
    index.edges = existing_edges;
    index.edges.extend(implicit_edges);
    index.edges.extend(csharp_edges);
}

/// Link C# files that declare the same namespace so namespace-cohesive code
/// (including `partial` classes split across files) forms a connected component
/// even without a direct `using`. Files in a namespace are chained
/// deterministically (`a -> b -> c`), yielding `n-1` edges per group.
fn collect_csharp_namespace_edges(
    file_paths: &[String],
    index: &ProjectIndex,
    edges: &mut Vec<IndexEdge>,
    content_cache: &HashMap<String, Arc<String>>,
) {
    let mut by_namespace: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();

    for file in file_paths {
        if Path::new(file.as_str())
            .extension()
            .and_then(|e| e.to_str())
            != Some("cs")
        {
            continue;
        }

        let content = if let Some(cached) = content_cache.get(file) {
            std::borrow::Cow::Borrowed(cached.as_str())
        } else {
            let full_path = Path::new(&index.project_root).join(file);
            match std::fs::read_to_string(&full_path) {
                Ok(c) => std::borrow::Cow::Owned(c),
                Err(_) => continue,
            }
        };

        if let Some(namespace) = csharp_primary_namespace(&content) {
            by_namespace
                .entry(namespace)
                .or_default()
                .push(file.clone());
        }
    }

    for (_namespace, mut files) in by_namespace {
        files.sort();
        files.dedup();
        if files.len() < 2 {
            continue;
        }
        for pair in files.windows(2) {
            edges.push(IndexEdge {
                from: pair[0].clone(),
                to: pair[1].clone(),
                kind: "namespace".to_string(),
                weight: 0.6,
            });
        }
    }
}

/// First namespace declared in a C# file — block `namespace X.Y { }` or
/// file-scoped `namespace X.Y;`. Comment lines are skipped so a commented-out
/// declaration is never mistaken for the real one.
fn csharp_primary_namespace(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") || trimmed.starts_with('*') || trimmed.starts_with("/*") {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("namespace ") {
            let namespace: String = rest
                .chars()
                .take_while(|c| !c.is_whitespace() && *c != '{' && *c != ';')
                .collect();
            if !namespace.is_empty() {
                return Some(namespace);
            }
        }
    }
    None
}

fn collect_rust_mod_edges_cached(
    file: &str,
    file_set: &std::collections::HashSet<&str>,
    index: &ProjectIndex,
    edges: &mut Vec<IndexEdge>,
    content_cache: &HashMap<String, Arc<String>>,
) {
    if !index.files.contains_key(file) {
        return;
    }

    let content = if let Some(cached) = content_cache.get(file) {
        std::borrow::Cow::Borrowed(cached.as_str())
    } else {
        let full_path = Path::new(&index.project_root).join(file);
        match std::fs::read_to_string(&full_path) {
            Ok(c) => std::borrow::Cow::Owned(c),
            Err(_) => return,
        }
    };

    let dir = Path::new(file)
        .parent()
        .map(|p| p.to_string_lossy().to_string());

    for line in content.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("mod ") || trimmed.contains('{') {
            continue;
        }
        let mod_name = trimmed
            .trim_start_matches("mod ")
            .trim_start_matches("pub mod ")
            .trim_start_matches("pub(crate) mod ")
            .trim_end_matches(';')
            .trim();

        if mod_name.is_empty() || mod_name.contains(' ') {
            continue;
        }

        let candidates = if let Some(ref d) = dir {
            vec![
                format!("{d}/{mod_name}.rs"),
                format!("{d}/{mod_name}/mod.rs"),
            ]
        } else {
            vec![format!("{mod_name}.rs"), format!("{mod_name}/mod.rs")]
        };

        for candidate in candidates {
            if file_set.contains(candidate.as_str()) {
                edges.push(IndexEdge {
                    from: file.to_string(),
                    to: candidate,
                    kind: "module".to_string(),
                    weight: 0.8,
                });
                break;
            }
        }
    }
}

fn collect_go_package_edges(file: &str, file_paths: &[String], edges: &mut Vec<IndexEdge>) {
    let p = Path::new(file);
    if p.extension().and_then(|e| e.to_str()) != Some("go") {
        return;
    }
    if file.ends_with("_test.go") {
        return;
    }

    let Some(dir) = p.parent().map(|d| d.to_string_lossy().to_string()) else {
        return;
    };

    for other in file_paths {
        if other == file {
            continue;
        }
        let op = Path::new(other.as_str());
        if op.extension().and_then(|e| e.to_str()) != Some("go") {
            continue;
        }
        if other.ends_with("_test.go") {
            continue;
        }
        let other_dir = op
            .parent()
            .map(|d| d.to_string_lossy().to_string())
            .unwrap_or_default();
        if other_dir == dir {
            edges.push(IndexEdge {
                from: file.to_string(),
                to: other.clone(),
                kind: "package".to_string(),
                weight: 0.5,
            });
            break;
        }
    }
}

fn collect_python_init_edges(file: &str, file_paths: &[String], edges: &mut Vec<IndexEdge>) {
    let p = Path::new(file);
    if p.file_name().and_then(|n| n.to_str()) != Some("__init__.py") {
        return;
    }

    let Some(dir) = p.parent().map(|d| d.to_string_lossy().to_string()) else {
        return;
    };

    for other in file_paths {
        if other == file {
            continue;
        }
        let op = Path::new(other.as_str());
        if op.extension().and_then(|e| e.to_str()) != Some("py") {
            continue;
        }
        let other_dir = op
            .parent()
            .map(|d| d.to_string_lossy().to_string())
            .unwrap_or_default();
        if other_dir == dir {
            edges.push(IndexEdge {
                from: file.to_string(),
                to: other.clone(),
                kind: "module".to_string(),
                weight: 0.8,
            });
        }
    }
}

fn collect_barrel_edges_cached(
    file: &str,
    file_set: &std::collections::HashSet<&str>,
    index: &ProjectIndex,
    edges: &mut Vec<IndexEdge>,
    content_cache: &HashMap<String, Arc<String>>,
) {
    let basename = Path::new(file)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if basename != "index" {
        return;
    }

    let content = if let Some(cached) = content_cache.get(file) {
        std::borrow::Cow::Borrowed(cached.as_str())
    } else {
        let full_path = Path::new(&index.project_root).join(file);
        match std::fs::read_to_string(&full_path) {
            Ok(c) => std::borrow::Cow::Owned(c),
            Err(_) => return,
        }
    };

    let dir = Path::new(file)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    let ext = Path::new(file)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("ts");

    for line in content.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("export") || !trimmed.contains("from") {
            continue;
        }
        if let Some(from_pos) = trimmed.find("from") {
            let after = &trimmed[from_pos + 4..];
            let source = after
                .trim()
                .trim_start_matches(['\'', '"'])
                .trim_end_matches([';', '\'', '"'])
                .trim_end_matches(['\'', '"']);

            if source.starts_with("./") || source.starts_with("../") {
                let resolved = if dir.is_empty() {
                    source.trim_start_matches("./").to_string()
                } else {
                    format!("{dir}/{}", source.trim_start_matches("./"))
                };

                let candidates = vec![
                    format!("{resolved}.{ext}"),
                    format!("{resolved}/index.{ext}"),
                    resolved.clone(),
                ];

                for candidate in candidates {
                    if file_set.contains(candidate.as_str()) {
                        edges.push(IndexEdge {
                            from: file.to_string(),
                            to: candidate,
                            kind: "reexport".to_string(),
                            weight: 0.8,
                        });
                        break;
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Layer 3: Co-Change Edges (weight 0.5)
// ---------------------------------------------------------------------------

fn build_cochange_edges(index: &mut ProjectIndex) {
    let project_root = &index.project_root;

    let output = match std::process::Command::new("git")
        .args([
            "log",
            "--name-only",
            "--pretty=format:---",
            "--since=6 months",
            "--",
            ".",
        ])
        .current_dir(project_root)
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => return,
    };

    let file_set: std::collections::HashSet<&str> =
        index.files.keys().map(String::as_str).collect();

    let connected: std::collections::HashSet<&str> = index
        .edges
        .iter()
        .flat_map(|e| [e.from.as_str(), e.to.as_str()])
        .collect();

    // Parse commits into groups of files
    let mut cooccurrence: HashMap<(String, String), u32> = HashMap::new();
    let mut current_commit: Vec<&str> = Vec::new();

    for line in output.lines() {
        if line == "---" {
            if current_commit.len() >= 2 && current_commit.len() <= 20 {
                for i in 0..current_commit.len() {
                    for j in (i + 1)..current_commit.len() {
                        let a = current_commit[i];
                        let b = current_commit[j];
                        if !file_set.contains(a) || !file_set.contains(b) {
                            continue;
                        }
                        // Only add if at least one is currently isolated
                        if connected.contains(a) && connected.contains(b) {
                            continue;
                        }
                        let key = if a < b {
                            (a.to_string(), b.to_string())
                        } else {
                            (b.to_string(), a.to_string())
                        };
                        *cooccurrence.entry(key).or_insert(0) += 1;
                    }
                }
            }
            current_commit.clear();
        } else if !line.is_empty() {
            current_commit.push(line.trim());
        }
    }

    // Filter: min 5 shared commits
    let mut cochange_edges: Vec<IndexEdge> = cooccurrence
        .into_iter()
        .filter(|(_, count)| *count >= 5)
        .map(|((from, to), _)| IndexEdge {
            from,
            to,
            kind: "cochange".to_string(),
            weight: 0.5,
        })
        .collect();

    // Cap at 500 to prevent noise
    cochange_edges.sort_by(|a, b| a.from.cmp(&b.from).then_with(|| a.to.cmp(&b.to)));
    cochange_edges.truncate(500);

    index.edges.extend(cochange_edges);
}

// ---------------------------------------------------------------------------
// Layer 4: Sibling Edges (weight 0.2)
// ---------------------------------------------------------------------------

fn build_sibling_edges(index: &mut ProjectIndex) {
    let existing_edges = std::mem::take(&mut index.edges);
    let connected: std::collections::HashSet<&str> = existing_edges
        .iter()
        .flat_map(|e| [e.from.as_str(), e.to.as_str()])
        .collect();

    let file_paths: Vec<String> = index.files.keys().cloned().collect();

    // Each isolate (file not yet connected) searches for a sibling independently.
    let sibling_edges: Vec<IndexEdge> = file_paths
        .par_iter()
        .filter_map(|file| {
            if connected.contains(file.as_str()) {
                return None;
            }

            let ext = Path::new(file.as_str())
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            let dir = Path::new(file.as_str())
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();

            // Find one sibling with same extension
            for other in &file_paths {
                if other == file {
                    continue;
                }
                let other_ext = Path::new(other.as_str())
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("");
                let other_dir = Path::new(other.as_str())
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();

                if other_ext == ext && other_dir == dir {
                    return Some(IndexEdge {
                        from: file.clone(),
                        to: other.clone(),
                        kind: "sibling".to_string(),
                        weight: 0.2,
                    });
                }
            }
            None
        })
        .collect();

    index.edges = existing_edges;
    index.edges.extend(sibling_edges);
}
