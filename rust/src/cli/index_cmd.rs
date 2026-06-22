use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant, UNIX_EPOCH};

pub(crate) fn cmd_index(args: &[String]) {
    let project_root = super::common::detect_project_root(args);
    let root = Path::new(&project_root);

    let sub = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .map(String::as_str);
    match sub {
        Some("status") => {
            let json_flag = args.iter().any(|a| a == "--json");
            if json_flag {
                println!(
                    "{}",
                    crate::core::index_orchestrator::status_json(&project_root)
                );
            } else {
                print_human_status(&project_root);
            }
        }
        Some("build") => {
            crate::core::index_orchestrator::ensure_all_background(&project_root);

            let started = std::time::Instant::now();
            let timeout = Duration::from_mins(5);
            eprint!("building indexes (graph + BM25)");
            loop {
                std::thread::sleep(Duration::from_millis(500));
                let status = crate::core::index_orchestrator::status_json(&project_root);
                if !status.contains("\"building\"") {
                    break;
                }
                eprint!(".");
                if started.elapsed() > timeout {
                    eprintln!(" timeout (background build continues)");
                    return;
                }
            }
            eprintln!(" done");

            // Surface the BM25 build outcome so the operator knows the index
            // state (issue #249).
            let summary = crate::core::index_orchestrator::bm25_summary(&project_root);
            if let Some(note) = summary.note {
                eprintln!("  BM25: {note}");
            }
            if let Some(err) = summary.last_error {
                eprintln!("  BM25 error: {err}");
            }
        }
        Some("build-full") => {
            let bm25_path = crate::core::bm25_index::BM25Index::index_file_path(root);
            let _ = std::fs::remove_file(&bm25_path);
            // #696 C4: purge the property graph (graph.db + wal/shm + meta) and
            // any retired JSON/call-graph artifacts so the rebuild starts clean.
            crate::core::graph_index::purge_index(&project_root);
            // Purge old embeddings so the full rebuild starts from scratch and
            // does not re-use stale vectors from a different model or project
            // state.
            let vectors_dir = crate::core::index_namespace::vectors_dir(root);
            let embedding_bin = vectors_dir.join("embeddings.bin");
            if embedding_bin.exists() {
                let _ = std::fs::remove_file(&embedding_bin);
            }
            let embedding_json = vectors_dir.join("embeddings.json");
            if embedding_json.exists() {
                let _ = std::fs::remove_file(&embedding_json);
            }
            crate::core::index_orchestrator::ensure_all_background(&project_root);

            let started = std::time::Instant::now();
            let timeout = Duration::from_mins(5);
            eprintln!("rebuilding indexes (graph + BM25)");
            loop {
                std::thread::sleep(Duration::from_millis(500));
                let status = crate::core::index_orchestrator::status_json(&project_root);
                if !status.contains("\"building\"") {
                    break;
                }
                eprint!(".");
                if started.elapsed() > timeout {
                    eprintln!(" timeout (background build continues)");
                    return;
                }
            }
            eprintln!(" done");

            // Surface the BM25 build outcome (chunk count + persisted size, or the
            // "too large to persist" remedy) so the operator is never left guessing.
            let summary = crate::core::index_orchestrator::bm25_summary(&project_root);
            if let Some(note) = summary.note {
                eprintln!("  BM25: {note}");
            }
            if let Some(err) = summary.last_error {
                eprintln!("  BM25 error: {err}");
            }

            // The property graph was already mirrored from the graph_index
            // extractor inside ensure_all_background above (#682.2).
            eprintln!("property graph mirrored from graph_index during index build");

            // Build semantic (dense embedding) index on top of the fresh BM25.
            eprintln!("building semantic (dense embedding) index ...");
            crate::core::index_orchestrator::build_semantic(&project_root);
            let sem = crate::core::index_orchestrator::semantic_summary(&project_root);
            match sem.state {
                "ready" => eprintln!("  semantic index ready"),
                "failed" => eprintln!(
                    "  semantic index failed: {}",
                    sem.last_error.unwrap_or_else(|| String::from("unknown"))
                ),
                _ => {
                    if let Some(note) = sem.note {
                        eprintln!("  semantic: {note}");
                    }
                }
            }

            // build-full is an explicit "make everything fresh". Drop the in-process
            // graph cache and flush the running daemon's read cache too, so ctx_read
            // map/signatures don't keep serving pre-rebuild output from the daemon's
            // long-lived SessionCache in another process (#420).
            crate::core::graph_cache::invalidate(Some(&project_root));
            if crate::daemon_client::notify_cache_clear() {
                eprintln!("  Daemon read cache flushed — ctx_read re-derives on next read.");
            }
        }
        Some("build-graph") => {
            // #682.1: mirror the proven graph_index extractor into the property
            // graph (complete symbols + file_catalog).
            match crate::core::graph_provider::build_property_graph(&project_root) {
                Ok(()) => match crate::core::property_graph::CodeGraph::open(&project_root) {
                    Ok(g) => println!(
                        "property graph built from graph_index: {} nodes, {} edges, {} files",
                        g.node_count().unwrap_or(0),
                        g.edge_count().unwrap_or(0),
                        g.file_catalog_count().unwrap_or(0),
                    ),
                    Err(_) => println!("property graph built from graph_index"),
                },
                Err(e) => eprintln!("property graph build failed: {e}"),
            }
        }
        Some("build-semantic") => {
            // Build the dense embedding index on top of BM25.  If BM25 is not yet
            // built, build graph + BM25 first, then build semantic.
            let disk = crate::core::index_orchestrator::disk_status(&project_root);
            if !disk.bm25_index.exists {
                eprintln!("BM25 index not found — building graph + BM25 first ...");
                crate::core::index_orchestrator::ensure_all_background(&project_root);
                let started = std::time::Instant::now();
                let timeout = Duration::from_mins(5);
                loop {
                    std::thread::sleep(Duration::from_millis(500));
                    let status = crate::core::index_orchestrator::status_json(&project_root);
                    if !status.contains("\"building\"") {
                        break;
                    }
                    eprint!(".");
                    if started.elapsed() > timeout {
                        eprintln!(" timeout");
                        return;
                    }
                }
                eprintln!(" done");
            }

            eprintln!("building semantic (dense embedding) index ...");
            crate::core::index_orchestrator::build_semantic(&project_root);
            let sem = crate::core::index_orchestrator::semantic_summary(&project_root);
            match sem.state {
                "ready" => eprintln!("semantic index ready"),
                "failed" => eprintln!(
                    "semantic index failed: {}",
                    sem.last_error.unwrap_or_else(|| String::from("unknown"))
                ),
                _ => {
                    eprintln!("semantic index not available");
                    if let Some(ref note) = sem.note {
                        eprintln!("  reason: {note}");
                    }
                }
            }
        }
        Some("watch") => run_watcher(root),
        _ => {
            eprintln!(
                "Usage: lean-ctx index <status|build|build-full|build-graph|build-semantic|watch> [--root <path>]\n\
                 Examples:\n\
                   lean-ctx index status\n\
                   lean-ctx index build              (graph + BM25 indexes)\n\
                   lean-ctx index build-full         (force rebuild all indexes)\n\
                   lean-ctx index build-graph        (SQLite property graph for impact analysis)\n\
                   lean-ctx index build-semantic     (dense embedding index, builds BM25 first if needed)\n\
                   lean-ctx index watch"
            );
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileState {
    mtime_ms: u64,
    size_bytes: u64,
}

fn run_watcher(project_root: &Path) {
    let hash = crate::core::index_namespace::namespace_hash(project_root);
    let lock_name = format!("index-watch-{}", &hash[..8.min(hash.len())]);
    let Some(lock) = crate::core::startup_guard::try_acquire_lock(
        &lock_name,
        Duration::from_millis(800),
        Duration::from_secs(8),
    ) else {
        eprintln!("index watcher already running");
        return;
    };

    let mut last = snapshot_code_files(project_root);
    let mut pending: Option<Instant> = None;
    let poll = Duration::from_millis(700);
    let debounce = Duration::from_millis(900);

    loop {
        lock.touch();
        std::thread::sleep(poll);

        let cur = snapshot_code_files(project_root);
        if cur != last {
            last = cur;
            pending = Some(Instant::now());
            continue;
        }

        if let Some(t) = pending
            && t.elapsed() >= debounce
        {
            crate::core::index_orchestrator::ensure_all_background(
                project_root.to_string_lossy().as_ref(),
            );
            pending = None;
        }
    }
}

fn snapshot_code_files(project_root: &Path) -> HashMap<String, FileState> {
    let walker = ignore::WalkBuilder::new(project_root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .require_git(false)
        .filter_entry(crate::core::walk_filter::keep_entry)
        .build();

    let mut out: HashMap<String, FileState> = HashMap::new();
    for entry in walker.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.components().any(|c| c.as_os_str() == ".git") {
            continue;
        }
        if !crate::core::ingestion::is_ingestible(path) {
            continue;
        }
        let Ok(meta) = path.metadata() else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        let Some(mtime_ms) = modified
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|d| d.as_millis() as u64)
        else {
            continue;
        };

        let rel = path
            .strip_prefix(project_root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        if rel.is_empty() {
            continue;
        }

        out.insert(
            rel,
            FileState {
                mtime_ms,
                size_bytes: meta.len(),
            },
        );
    }
    out
}

fn print_human_status(project_root: &str) {
    let disk = crate::core::index_orchestrator::disk_status(project_root);

    println!("  Project:        {project_root}");
    println!(
        "  Graph Index:    {}",
        format_disk_line(&disk.graph_index, "files")
    );
    println!(
        "  BM25 Index:     {}",
        format_disk_line(&disk.bm25_index, "chunks")
    );
    println!(
        "  Code Graph:     {}",
        format_disk_line(&disk.code_graph, "nodes")
    );
    println!(
        "  Semantic Index: {}",
        format_disk_line(&disk.semantic_index, "vectors")
    );
}

fn format_disk_line(ds: &crate::core::index_orchestrator::DiskStatus, count_label: &str) -> String {
    if !ds.exists {
        return "not built".to_string();
    }
    let mut parts = vec!["ready".to_string()];
    if let Some(count) = ds.file_count {
        parts.push(format!("{count} {count_label}"));
    }
    if let Some(bytes) = ds.size_bytes {
        parts.push(format_bytes(bytes));
    }
    if let Some(ref t) = ds.modified_at {
        parts.push(format!("built {t}"));
    }
    format!("({})", parts.join(", "))
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}
