use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant, UNIX_EPOCH};

use crate::core::config::{Config, IndexingMode};
use crate::core::index_pipeline::pipeline::IndexPipeline;

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
            let mode = resolve_mode(args);

            if mode == IndexingMode::Full {
                eprintln!("purging old index artifacts for full rebuild ...");
                // New pipeline: remove the unified SQLite database
                let vectors_dir = crate::core::index_namespace::vectors_dir(root);
                for name in &["code_index.db", "code_index.db-wal", "code_index.db-shm"] {
                    let p = vectors_dir.join(name);
                    if p.exists() {
                        let _ = std::fs::remove_file(&p);
                    }
                }
                // Legacy BM25 files
                let bm25_path = crate::core::bm25_index::BM25Index::index_file_path(root);
                let _ = std::fs::remove_file(&bm25_path);
                // Graph index dir (graph.db, graph.meta.json, legacy artifacts)
                crate::core::graph_index::purge_index(&project_root);
                // Embeddings (semantic search)
                let embedding_bin = vectors_dir.join("embeddings.bin");
                if embedding_bin.exists() {
                    let _ = std::fs::remove_file(&embedding_bin);
                }
                let embedding_json = vectors_dir.join("embeddings.json");
                if embedding_json.exists() {
                    let _ = std::fs::remove_file(&embedding_json);
                }
            }

            eprintln!("building indexes (mode: {}) ...", mode.label());
            let pipeline = match IndexPipeline::new(root.to_path_buf())
                .with_mode(mode)
                .build()
            {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("error configuring index build: {e}");
                    return;
                }
            };

            match pipeline.run() {
                Ok(report) => {
                    eprintln!("index build complete");
                    eprintln!("  mode:         {}", report.mode.label());
                    eprintln!(
                        "  files:        {} scanned, {} new, {} changed, {} deleted",
                        report.files_scanned,
                        report.files_new,
                        report.files_changed,
                        report.files_deleted
                    );
                    eprintln!(
                        "  graph:        {} nodes, {} edges",
                        report.nodes, report.edges
                    );
                    eprintln!("  BM25 chunks:  {}", report.chunks);
                    eprintln!("  elapsed:      {} ms", report.elapsed_ms);
                    eprintln!("  incremental:  {}", report.is_incremental);
                    if mode == IndexingMode::Full {
                        crate::core::graph_cache::invalidate(Some(&project_root));
                        if crate::daemon_client::notify_cache_clear() {
                            eprintln!("  Daemon read cache flushed.");
                        }
                    }
                }
                Err(e) => {
                    eprintln!("index build failed: {e}");
                }
            }
        }
        Some("watch") => run_watcher(root),
        _ => {
            eprintln!(
                "Usage: lean-ctx index <status|build|watch> [--root <path>] [--mode <full|moderate|fast>]\n\
                 Examples:\n\
                   lean-ctx index status\n\
                   lean-ctx index build                                (graph + BM25 + semantic indexes)\n\
                   lean-ctx index build --mode fast                    (fast indexing: skip heavy passes)\n\
                   lean-ctx index build --mode moderate                (moderate indexing)\n\
                   lean-ctx index build --mode full                    (force full rebuild)\n\
                   lean-ctx index watch"
            );
        }
    }
}

/// Parse `--mode <full|moderate|fast>` / `-m <full|moderate|fast>` from args.
/// Falls back to env var, then config default.
fn resolve_mode(args: &[String]) -> IndexingMode {
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        if (arg == "--mode" || arg == "-m")
            && let Some(val) = iter.next()
        {
            if let Some(mode) = IndexingMode::parse(val) {
                return mode;
            }
            eprintln!("warning: unknown mode '{val}', valid: full, moderate, fast");
        }
    }
    IndexingMode::effective(&Config::load())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileState {
    mtime_ms: u64,
    size_bytes: u64,
}

fn run_watcher(project_root: &Path) {
    let mode = IndexingMode::effective(&Config::load());
    eprintln!(
        "index watcher started (mode: {}, poll: 700ms, debounce: 900ms)",
        mode.label()
    );

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
    let mode = IndexingMode::effective(&Config::load());

    // Determine incremental readiness: all primary indices exist on disk.
    let incremental_ready = disk.graph_index.exists && disk.bm25_index.exists;

    println!("  Project:        {project_root}");
    println!("  Index Mode:     {}", mode.label());
    println!(
        "  Incremental:    {}",
        if incremental_ready {
            "ready (indices exist)"
        } else {
            "no (full rebuild needed)"
        }
    );
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
