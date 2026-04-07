use std::path::Path;

use crate::core::cache::SessionCache;
use crate::core::intent_engine;
use crate::core::tokens::count_tokens;
use crate::tools::CrpMode;

#[derive(Debug)]
enum Intent {
    FixBug { area: String },
    AddFeature { area: String },
    Refactor { area: String },
    Understand { area: String },
    Test { area: String },
    Config,
    Deploy,
    Unknown,
}

pub fn handle(
    cache: &mut SessionCache,
    query: &str,
    project_root: &str,
    crp_mode: CrpMode,
) -> String {
    let multi_intents = intent_engine::detect_multi_intent(query);
    let primary = &multi_intents[0];
    let briefing_header = intent_engine::format_briefing_header(primary);
    let complexity = intent_engine::classify_complexity(query, primary);

    let intent = classify_intent(query);
    let strategy = build_strategy(&intent, project_root);

    let file_context: Vec<(String, usize)> = strategy
        .iter()
        .filter(|(p, _)| Path::new(p).exists())
        .filter_map(|(p, _)| {
            std::fs::read_to_string(p)
                .ok()
                .map(|c| (p.clone(), c.lines().count()))
        })
        .collect();
    let briefing = crate::core::task_briefing::build_briefing(query, &file_context);
    let briefing_block = crate::core::task_briefing::format_briefing(&briefing);

    let mut result = Vec::new();
    result.push(briefing_block);
    result.push(briefing_header);
    result.push(format!(
        "Complexity: {} | {}",
        complexity.instruction_suffix().lines().next().unwrap_or(""),
        if multi_intents.len() > 1 {
            format!("{} sub-intents detected", multi_intents.len())
        } else {
            "single intent".to_string()
        }
    ));
    result.push(format!(
        "Strategy: {} files, modes: {}",
        strategy.len(),
        strategy
            .iter()
            .map(|(_, m)| m.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    ));

    if multi_intents.len() > 1 {
        result.push("Sub-intents:".to_string());
        for (i, sub) in multi_intents.iter().enumerate() {
            result.push(format!(
                "  {}. {} ({:.0}%)",
                i + 1,
                sub.task_type.as_str(),
                sub.confidence * 100.0
            ));
        }
    }

    result.push(String::new());

    for (path, mode) in &strategy {
        if !Path::new(path).exists() {
            continue;
        }
        let file_result = crate::tools::ctx_read::handle(cache, path, mode, crp_mode);
        result.push(file_result);
        result.push("---".to_string());
    }

    let output = result.join("\n");
    let tokens = count_tokens(&output);
    format!(
        "{output}\n\n[ctx_intent: {tokens} tok | complexity: {}]",
        complexity.instruction_suffix().lines().next().unwrap_or("")
    )
}

fn classify_intent(query: &str) -> Intent {
    let q = query.to_lowercase();

    let area = extract_area(&q);

    if q.contains("fix")
        || q.contains("bug")
        || q.contains("error")
        || q.contains("broken")
        || q.contains("crash")
        || q.contains("fail")
    {
        return Intent::FixBug { area };
    }
    if q.contains("add")
        || q.contains("create")
        || q.contains("implement")
        || q.contains("new")
        || q.contains("feature")
    {
        return Intent::AddFeature { area };
    }
    if q.contains("refactor")
        || q.contains("clean")
        || q.contains("restructure")
        || q.contains("rename")
        || q.contains("move")
    {
        return Intent::Refactor { area };
    }
    if q.contains("understand")
        || q.contains("how")
        || q.contains("what")
        || q.contains("explain")
        || q.contains("where")
    {
        return Intent::Understand { area };
    }
    if q.contains("test") || q.contains("spec") || q.contains("coverage") {
        return Intent::Test { area };
    }
    if q.contains("config") || q.contains("setup") || q.contains("env") || q.contains("install") {
        return Intent::Config;
    }
    if q.contains("deploy") || q.contains("release") || q.contains("publish") || q.contains("ship")
    {
        return Intent::Deploy;
    }

    Intent::Unknown
}

fn extract_area(query: &str) -> String {
    let keywords: Vec<&str> = query
        .split_whitespace()
        .filter(|w| {
            w.len() > 3
                && !matches!(
                    *w,
                    "the"
                        | "this"
                        | "that"
                        | "with"
                        | "from"
                        | "into"
                        | "have"
                        | "please"
                        | "could"
                        | "would"
                        | "should"
                )
        })
        .collect();

    let file_refs: Vec<&&str> = keywords
        .iter()
        .filter(|w| w.contains('.') || w.contains('/') || w.contains('\\'))
        .collect();

    if let Some(path) = file_refs.first() {
        return path.to_string();
    }

    let code_terms: Vec<&&str> = keywords
        .iter()
        .filter(|w| {
            w.chars().any(|c| c.is_uppercase())
                || w.contains('_')
                || matches!(
                    **w,
                    "auth"
                        | "login"
                        | "api"
                        | "database"
                        | "db"
                        | "server"
                        | "client"
                        | "user"
                        | "admin"
                        | "router"
                        | "handler"
                        | "middleware"
                        | "controller"
                        | "model"
                        | "view"
                        | "component"
                        | "service"
                        | "repository"
                        | "cache"
                        | "queue"
                        | "worker"
                )
        })
        .collect();

    if let Some(term) = code_terms.first() {
        return term.to_string();
    }

    keywords.last().unwrap_or(&"").to_string()
}

pub fn rank_by_heat(files: &mut [(String, String)], root: &str) {
    let index = crate::core::graph_index::load_or_build(root);
    if index.files.is_empty() {
        return;
    }

    let mut connection_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for edge in &index.edges {
        *connection_counts.entry(edge.from.clone()).or_default() += 1;
        *connection_counts.entry(edge.to.clone()).or_default() += 1;
    }

    let max_tokens = index
        .files
        .values()
        .map(|f| f.token_count)
        .max()
        .unwrap_or(1) as f64;
    let max_conn = connection_counts.values().max().copied().unwrap_or(1) as f64;

    files.sort_by(|a, b| {
        let heat_a = heat_score_for(&a.0, root, &index, &connection_counts, max_tokens, max_conn);
        let heat_b = heat_score_for(&b.0, root, &index, &connection_counts, max_tokens, max_conn);
        heat_b
            .partial_cmp(&heat_a)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

fn heat_score_for(
    path: &str,
    root: &str,
    index: &crate::core::graph_index::ProjectIndex,
    connections: &std::collections::HashMap<String, usize>,
    max_tokens: f64,
    max_conn: f64,
) -> f64 {
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .trim_start_matches('/');

    if let Some(entry) = index.files.get(rel) {
        let conn = connections.get(rel).copied().unwrap_or(0);
        let token_norm = entry.token_count as f64 / max_tokens;
        let conn_norm = conn as f64 / max_conn;
        token_norm * 0.4 + conn_norm * 0.6
    } else {
        0.0
    }
}

fn build_strategy(intent: &Intent, root: &str) -> Vec<(String, String)> {
    let mut files = Vec::new();
    let loaded = crate::core::graph_index::load_or_build(root);
    let graph = if loaded.files.is_empty() {
        None
    } else {
        Some(loaded)
    };

    match intent {
        Intent::FixBug { area } => {
            if let Some(paths) = find_files_for_area(area, root) {
                for path in paths.iter().take(3) {
                    files.push((path.clone(), "full".to_string()));
                }
                if let Some(ref idx) = graph {
                    enrich_with_graph(&mut files, &paths, idx, root, "map", 5);
                }
                for path in paths.iter().skip(3).take(5) {
                    if !files.iter().any(|(f, _)| f == path) {
                        files.push((path.clone(), "map".to_string()));
                    }
                }
            }
            if let Some(test_files) = find_test_files(area, root) {
                for path in test_files.iter().take(2) {
                    if !files.iter().any(|(f, _)| f == path) {
                        files.push((path.clone(), "signatures".to_string()));
                    }
                }
            }
        }
        Intent::AddFeature { area } => {
            if let Some(paths) = find_files_for_area(area, root) {
                for path in paths.iter().take(2) {
                    files.push((path.clone(), "signatures".to_string()));
                }
                if let Some(ref idx) = graph {
                    enrich_with_graph(&mut files, &paths, idx, root, "map", 5);
                }
                for path in paths.iter().skip(2).take(5) {
                    if !files.iter().any(|(f, _)| f == path) {
                        files.push((path.clone(), "map".to_string()));
                    }
                }
            }
        }
        Intent::Refactor { area } => {
            if let Some(paths) = find_files_for_area(area, root) {
                for path in paths.iter().take(5) {
                    files.push((path.clone(), "full".to_string()));
                }
                if let Some(ref idx) = graph {
                    enrich_with_graph(&mut files, &paths, idx, root, "full", 5);
                }
            }
        }
        Intent::Understand { area } => {
            if let Some(paths) = find_files_for_area(area, root) {
                for path in &paths {
                    files.push((path.clone(), "map".to_string()));
                }
                if let Some(ref idx) = graph {
                    enrich_with_graph(&mut files, &paths, idx, root, "map", 8);
                }
            }
        }
        Intent::Test { area } => {
            if let Some(test_files) = find_test_files(area, root) {
                for path in test_files.iter().take(3) {
                    files.push((path.clone(), "full".to_string()));
                }
            }
            if let Some(src_files) = find_files_for_area(area, root) {
                for path in src_files.iter().take(3) {
                    if !files.iter().any(|(f, _)| f == path) {
                        files.push((path.clone(), "signatures".to_string()));
                    }
                }
            }
        }
        Intent::Config => {
            for name in &[
                "Cargo.toml",
                "package.json",
                "pyproject.toml",
                "go.mod",
                "tsconfig.json",
                "docker-compose.yml",
            ] {
                let path = format!("{root}/{name}");
                if Path::new(&path).exists() {
                    files.push((path, "full".to_string()));
                }
            }
        }
        Intent::Deploy => {
            for name in &[
                "Dockerfile",
                "docker-compose.yml",
                "Makefile",
                ".github/workflows",
            ] {
                let path = format!("{root}/{name}");
                if Path::new(&path).exists() {
                    files.push((path, "full".to_string()));
                }
            }
        }
        Intent::Unknown => {}
    }

    rank_by_heat(&mut files, root);
    files
}

fn enrich_with_graph(
    files: &mut Vec<(String, String)>,
    seed_paths: &[String],
    index: &crate::core::graph_index::ProjectIndex,
    root: &str,
    mode: &str,
    max: usize,
) {
    let mut added = 0;
    for seed in seed_paths {
        let rel = seed
            .strip_prefix(root)
            .unwrap_or(seed)
            .trim_start_matches('/');

        for related in index.get_related(rel, 2) {
            if added >= max {
                return;
            }
            let abs = format!("{root}/{related}");
            if !files.iter().any(|(f, _)| *f == abs || *f == related) && Path::new(&abs).exists() {
                files.push((abs, mode.to_string()));
                added += 1;
            }
        }
    }
}

fn find_files_for_area(area: &str, root: &str) -> Option<Vec<String>> {
    let mut matches = Vec::new();
    let search_term = area.to_lowercase();

    ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .max_depth(Some(6))
        .build()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_some_and(|ft| ft.is_file()))
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_lowercase();
            name.contains(&search_term)
                || e.path()
                    .to_string_lossy()
                    .to_lowercase()
                    .contains(&search_term)
        })
        .take(10)
        .for_each(|e| {
            let path = e.path().to_string_lossy().to_string();
            if !matches.contains(&path) {
                matches.push(path);
            }
        });

    if matches.is_empty() {
        None
    } else {
        Some(matches)
    }
}

fn find_test_files(area: &str, root: &str) -> Option<Vec<String>> {
    let search_term = area.to_lowercase();
    let mut matches = Vec::new();

    ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .max_depth(Some(6))
        .build()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_some_and(|ft| ft.is_file()))
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_lowercase();
            (name.contains("test") || name.contains("spec"))
                && (name.contains(&search_term)
                    || e.path()
                        .to_string_lossy()
                        .to_lowercase()
                        .contains(&search_term))
        })
        .take(5)
        .for_each(|e| {
            matches.push(e.path().to_string_lossy().to_string());
        });

    if matches.is_empty() {
        None
    } else {
        Some(matches)
    }
}
