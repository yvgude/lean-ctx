use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant, UNIX_EPOCH};

pub fn cmd_index(args: &[String]) {
    let project_root = super::common::detect_project_root(args);
    let root = Path::new(&project_root);

    let sub = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .map(String::as_str);
    match sub {
        Some("status") => {
            println!(
                "{}",
                crate::core::index_orchestrator::status_json(&project_root)
            );
        }
        Some("build") => {
            crate::core::index_orchestrator::ensure_all_background(&project_root);
            println!("started");
        }
        Some("build-full") => {
            crate::core::index_orchestrator::ensure_full_background(&project_root);
            println!("started");
        }
        Some("watch") => run_watcher(root),
        _ => {
            eprintln!(
                "Usage: lean-ctx index <status|build|build-full|watch> [--root <path>]\n\
                 Examples:\n\
                   lean-ctx index status\n\
                   lean-ctx index build\n\
                   lean-ctx index build-full\n\
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

        if let Some(t) = pending {
            if t.elapsed() >= debounce {
                crate::core::index_orchestrator::ensure_all_background(
                    project_root.to_string_lossy().as_ref(),
                );
                pending = None;
            }
        }
    }
}

fn snapshot_code_files(project_root: &Path) -> HashMap<String, FileState> {
    let walker = ignore::WalkBuilder::new(project_root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
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
        if !is_code_file(path) {
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

fn is_code_file(path: &Path) -> bool {
    crate::core::vector_index::is_code_file(path)
}
