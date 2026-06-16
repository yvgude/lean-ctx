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
            println!("started");
        }
        Some("build-full") => {
            let bm25_path = crate::core::bm25_index::BM25Index::index_file_path(root);
            let _ = std::fs::remove_file(&bm25_path);
            if let Some(dir) = crate::core::graph_index::ProjectIndex::index_dir(&project_root) {
                let _ = std::fs::remove_file(dir.join("index.json.zst"));
                let _ = std::fs::remove_file(dir.join("index.json"));
                let _ = std::fs::remove_file(dir.join("call_graph.json.zst"));
                let _ = std::fs::remove_file(dir.join("graph.db"));
                let _ = std::fs::remove_file(dir.join("graph.meta.json"));
            }
            crate::core::index_orchestrator::ensure_all_background(&project_root);

            let started = std::time::Instant::now();
            let timeout = Duration::from_mins(5);
            eprint!("rebuilding indexes (graph + BM25 + call graph)");
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
            // "too large to persist" remedy) so the user is never left guessing why
            // semantic search stays cold (issue #249).
            let summary = crate::core::index_orchestrator::bm25_summary(&project_root);
            if let Some(note) = summary.note {
                eprintln!("  BM25: {note}");
            }
            if let Some(err) = summary.last_error {
                eprintln!("  BM25 error: {err}");
            }

            eprint!("rebuilding property graph");
            let result =
                crate::tools::ctx_impact::handle("build", None, &project_root, None, Some("text"));
            if result.contains("ERROR") {
                eprintln!(" {result}");
            } else {
                eprintln!(" done");
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
            let root_str = project_root.clone();
            let result =
                crate::tools::ctx_impact::handle("build", None, &root_str, None, Some("text"));
            println!("{result}");
        }
        Some("watch") => run_watcher(root),
        _ => {
            eprintln!(
                "Usage: lean-ctx index <status|build|build-full|build-graph|watch> [--root <path>]\n\
                 Examples:\n\
                   lean-ctx index status\n\
                   lean-ctx index build          (BM25 + JSON graph index)\n\
                   lean-ctx index build-full     (force rebuild all indexes)\n\
                   lean-ctx index build-graph    (SQLite property graph for impact analysis)\n\
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

    println!("  Project:     {project_root}");
    println!(
        "  Graph Index: {}",
        format_disk_line(&disk.graph_index, "files")
    );
    println!(
        "  BM25 Index:  {}",
        format_disk_line(&disk.bm25_index, "chunks")
    );
    println!(
        "  Code Graph:  {}",
        format_disk_line(&disk.code_graph, "nodes")
    );

    // Runtime semantic-index status (state/timing/why-stuck). This is the part
    // users asked for in #249 — knowing whether the index is working, how fast,
    // and if/why it failed, instead of an opaque "warming up" loop.
    let summary = crate::core::index_orchestrator::bm25_summary(project_root);
    let timing = match summary.elapsed_ms {
        Some(ms) if summary.state == "building" => format!(" ({:.1}s elapsed)", ms as f64 / 1000.0),
        Some(ms) => format!(" (built in {:.1}s)", ms as f64 / 1000.0),
        None => String::new(),
    };
    println!("  Semantic:    {}{timing}", summary.state);
    if let Some(note) = summary.note {
        println!("  Note:        {note}");
    }
    if let Some(err) = summary.last_error {
        println!("  Error:       {err}");
    }
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
