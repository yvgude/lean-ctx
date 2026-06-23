//! `lean-ctx graph|smells|compact` — the code-graph command surface and
//! transcript compaction.

use crate::{core, tools};

pub(in crate::cli::dispatch) fn cmd_graph(rest: &[String]) {
    // `--json` is positional-agnostic: strip it out and remember the choice so
    // positional parsing below stays simple.
    let want_json = rest.iter().any(|a| a == "--json");
    let fmt = if want_json { Some("json") } else { None };
    let filtered: Vec<String> = rest
        .iter()
        .filter(|a| a.as_str() != "--json")
        .cloned()
        .collect();
    let rest = &filtered[..];
    let sub = rest.first().map_or("build", std::string::String::as_str);
    match sub {
        "build" => {
            // `--force`/`-f` purges the cache for a from-scratch rebuild. A plain
            // `graph build` always rescans (incremental, hash-reused) so it reflects
            // the current source — `load_or_build` can otherwise return a cached
            // index until the staleness TTL elapses.
            let force = rest.iter().any(|a| a == "--force" || a == "-f");
            let root = rest
                .iter()
                .skip(1)
                .find(|a| !a.starts_with('-'))
                .cloned()
                .or_else(|| {
                    std::env::current_dir()
                        .ok()
                        .map(|p| p.to_string_lossy().to_string())
                })
                .unwrap_or_else(|| ".".to_string());
            if force {
                core::graph_index::purge_index(&root);
            }
            let handle = crate::core::index_pipeline::pipeline::IndexPipeline::new(
                std::path::PathBuf::from(&root),
            )
            .build()
            .expect("pipeline build failed");
            let (index, _) = handle.run_and_load().expect("pipeline run failed");
            println!(
                "Graph built: {} files, {} edges",
                index.files.len(),
                index.edges.len()
            );
            if force {
                // A forced rebuild must also drop cached *derivations* of the old
                // index/source: the in-process graph cache and — crucially — the
                // running daemon's read cache, which lives in another process and
                // would otherwise keep serving pre-rebuild ctx_read map/signatures
                // (#420).
                core::graph_cache::invalidate(Some(&root));
                if crate::daemon_client::notify_cache_clear() {
                    println!("Daemon read cache flushed — ctx_read re-derives map/signatures.");
                }
            }
        }
        "export-html" => {
            let mut root: Option<String> = None;
            let mut out: Option<String> = None;
            let mut max_nodes: usize = 2500;

            let args = &rest[1..];
            let mut i = 0usize;
            while i < args.len() {
                let a = args[i].as_str();
                if let Some(v) = a.strip_prefix("--root=") {
                    root = Some(v.to_string());
                } else if a == "--root" {
                    root = args.get(i + 1).cloned();
                    i += 1;
                } else if let Some(v) = a.strip_prefix("--out=") {
                    out = Some(v.to_string());
                } else if a == "--out" {
                    out = args.get(i + 1).cloned();
                    i += 1;
                } else if let Some(v) = a.strip_prefix("--max-nodes=") {
                    max_nodes = v.parse::<usize>().unwrap_or(0);
                } else if a == "--max-nodes" {
                    let v = args.get(i + 1).map_or("", String::as_str);
                    max_nodes = v.parse::<usize>().unwrap_or(0);
                    i += 1;
                }
                i += 1;
            }

            let root = root
                .or_else(|| {
                    std::env::current_dir()
                        .ok()
                        .map(|p| p.to_string_lossy().to_string())
                })
                .unwrap_or_else(|| ".".to_string());
            let Some(out) = out else {
                eprintln!(
                    "Usage: lean-ctx graph export-html --out <path> [--root <path>] [--max-nodes <n>]"
                );
                std::process::exit(1);
            };
            if max_nodes == 0 {
                eprintln!("--max-nodes must be >= 1");
                std::process::exit(1);
            }

            core::graph_export::export_graph_html(&root, std::path::Path::new(&out), max_nodes)
                .unwrap_or_else(|e| {
                    eprintln!("graph export failed: {e}");
                    std::process::exit(1);
                });
            println!("{out}");
        }
        "related" | "impact" | "symbol" | "context" | "status" | "neighbors" | "explain" => {
            let path_arg = if sub == "status" {
                None
            } else {
                rest.get(1).map(String::as_str)
            };
            let root_idx = if sub == "status" { 1 } else { 2 };
            let root = resolve_graph_root(rest.get(root_idx));
            println!(
                "{}",
                tools::ctx_graph::handle(
                    sub,
                    path_arg,
                    &root,
                    &mut core::cache::SessionCache::new(),
                    tools::CrpMode::Off,
                    None,
                    None,
                    None,
                    fmt,
                    None,
                )
            );
        }
        "parity" => {
            // #682.3: shadow-mode proof that the PropertyGraph reproduces
            // graph_index before the backend flip relies on it.
            let root = resolve_graph_root(rest.get(1));
            println!(
                "{}",
                tools::ctx_impact::handle("parity", None, &root, None, fmt)
            );
        }
        "path" => {
            let from = rest.get(1).map(String::as_str);
            let to = rest.get(2).map(String::as_str);
            let root = resolve_graph_root(rest.get(3));
            println!(
                "{}",
                tools::ctx_graph::handle(
                    sub,
                    from,
                    &root,
                    &mut core::cache::SessionCache::new(),
                    tools::CrpMode::Off,
                    None,
                    None,
                    to,
                    fmt,
                    None,
                )
            );
        }
        "diff" => {
            let since = rest.get(1).map(String::as_str);
            let root = resolve_graph_root(rest.get(2));
            println!(
                "{}",
                tools::ctx_graph::handle(
                    sub,
                    None,
                    &root,
                    &mut core::cache::SessionCache::new(),
                    tools::CrpMode::Off,
                    None,
                    None,
                    None,
                    fmt,
                    since,
                )
            );
        }
        // Team graph — Context-as-Code (GL#451): a committable, diffable,
        // mergeable snapshot of the property graph.
        "snapshot" | "import" | "check" => cmd_graph_snapshot(sub, &rest[1..]),
        _ => {
            eprintln!(
                "Usage:\n  \
                 lean-ctx graph build [path]\n  \
                 lean-ctx graph related <file>\n  \
                 lean-ctx graph impact <file|symbol>\n  \
                 lean-ctx graph symbol <name>\n  \
                 lean-ctx graph context <query>\n  \
                 lean-ctx graph neighbors <file> [--json]\n  \
                 lean-ctx graph path <from> <to> [--json]\n  \
                 lean-ctx graph explain <file> [--json]\n  \
                 lean-ctx graph diff [since-ref] [--json]\n  \
                 lean-ctx graph status\n  \
                 lean-ctx graph export-html --out <path> [--root <path>] [--max-nodes <n>]\n  \
                 lean-ctx graph snapshot [--out <path>]      (export committable team snapshot)\n  \
                 lean-ctx graph import <path>                (merge a teammate's snapshot)\n  \
                 lean-ctx graph check [path]                 (drift vs snapshot; exit 1 on drift)"
            );
            std::process::exit(1);
        }
    }
}

/// Default snapshot location inside the repo: committable Context-as-Code.
fn default_snapshot_path(root: &str) -> std::path::PathBuf {
    std::path::Path::new(root)
        .join(".lean-ctx")
        .join("graph.snapshot")
}

fn cmd_graph_snapshot(sub: &str, args: &[String]) {
    let root = std::env::current_dir()
        .map_or_else(|_| ".".to_string(), |p| p.to_string_lossy().to_string());

    let graph = match core::property_graph::CodeGraph::open(&root) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("graph open failed: {e}");
            std::process::exit(1);
        }
    };

    match sub {
        "snapshot" => {
            let out_path = args
                .iter()
                .enumerate()
                .find_map(|(i, a)| {
                    a.strip_prefix("--out=")
                        .map(String::from)
                        .or_else(|| (a == "--out").then(|| args.get(i + 1).cloned()).flatten())
                })
                .map_or_else(|| default_snapshot_path(&root), std::path::PathBuf::from);

            let content = match core::property_graph::snapshot::export_snapshot(&graph) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("snapshot export failed: {e}");
                    std::process::exit(1);
                }
            };
            if let Some(parent) = out_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = crate::config_io::write_atomic_with_backup(&out_path, &content) {
                eprintln!("snapshot write failed: {e}");
                std::process::exit(1);
            }
            let lines = content.lines().count().saturating_sub(1);
            println!(
                "Graph snapshot written: {} ({lines} entries) — commit it to share with your team",
                out_path.display()
            );
        }
        "import" => {
            let Some(path) = args.iter().find(|a| !a.starts_with('-')) else {
                eprintln!("Usage: lean-ctx graph import <path>");
                std::process::exit(1);
            };
            let content = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("cannot read {path}: {e}");
                    std::process::exit(1);
                }
            };
            match core::property_graph::snapshot::import_snapshot(&graph, &content) {
                Ok(stats) => println!(
                    "Graph snapshot merged: {} nodes, {} edges ({} edges skipped — endpoints unknown locally)",
                    stats.nodes, stats.edges, stats.skipped_edges
                ),
                Err(e) => {
                    eprintln!("import failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        "check" => {
            let path = args
                .iter()
                .find(|a| !a.starts_with('-'))
                .map_or_else(|| default_snapshot_path(&root), std::path::PathBuf::from);
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("cannot read {}: {e}", path.display());
                    std::process::exit(1);
                }
            };
            match core::property_graph::snapshot::check_snapshot(&graph, &content) {
                Ok(report) => {
                    if report.in_sync() {
                        println!("Graph in sync with snapshot ({} entries)", report.common);
                    } else {
                        println!(
                            "Graph drift: {} only local, {} only in snapshot ({} common) — run 'lean-ctx graph snapshot' to refresh",
                            report.only_local, report.only_snapshot, report.common
                        );
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    eprintln!("check failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        _ => unreachable!("guarded by caller"),
    }
}

pub(in crate::cli::dispatch) fn cmd_smells(rest: &[String]) {
    let action = rest.first().map_or("summary", String::as_str);
    let rule = rest.iter().enumerate().find_map(|(i, a)| {
        if let Some(v) = a.strip_prefix("--rule=") {
            return Some(v.to_string());
        }
        if a == "--rule" {
            return rest.get(i + 1).cloned();
        }
        None
    });
    let path = rest.iter().enumerate().find_map(|(i, a)| {
        if let Some(v) = a.strip_prefix("--path=") {
            return Some(v.to_string());
        }
        if a == "--path" {
            return rest.get(i + 1).cloned();
        }
        None
    });
    let root = rest
        .iter()
        .enumerate()
        .find_map(|(i, a)| {
            if let Some(v) = a.strip_prefix("--root=") {
                return Some(v.to_string());
            }
            if a == "--root" {
                return rest.get(i + 1).cloned();
            }
            None
        })
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| ".".to_string());
    let fmt = if rest.iter().any(|a| a == "--json") {
        Some("json")
    } else {
        None
    };
    println!(
        "{}",
        tools::ctx_smells::handle(action, rule.as_deref(), path.as_deref(), &root, fmt)
    );
}

fn resolve_graph_root(arg: Option<&String>) -> String {
    arg.cloned()
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| ".".to_string())
}

pub(in crate::cli::dispatch) fn cmd_compact(rest: &[String]) {
    let target = rest.first().map_or_else(
        || {
            let home = dirs::home_dir().unwrap_or_default();
            let claude = home.join(".claude").join("projects");
            if claude.is_dir() {
                claude
            } else {
                let cursor = home.join(".cursor").join("agent-transcripts");
                if cursor.is_dir() {
                    cursor
                } else {
                    std::env::current_dir().unwrap_or_default()
                }
            }
        },
        std::path::PathBuf::from,
    );

    if !target.exists() {
        eprintln!("Path does not exist: {}", target.display());
        std::process::exit(1);
    }

    let result = if target.is_file() {
        core::transcript_compact::compact_file(&target)
    } else {
        core::transcript_compact::compact_directory(&target)
    };

    match result {
        Ok(stats) => {
            println!("Transcript compaction: {stats}");
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}
