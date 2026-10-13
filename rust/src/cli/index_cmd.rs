use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant, UNIX_EPOCH};

use crate::terminal_ui::ProgressIndicator;

pub(crate) fn cmd_index(args: &[String]) {
    let project_root = super::common::detect_project_root(args);
    let root = Path::new(&project_root);

    // #735: install the per-run filter overlay (repeatable --include/--exclude
    // globs, --no-gitignore/--respect-gitignore) before any builder runs, so
    // BM25 + graph + semantic + watch share the declared corpus for this run.
    install_filter_overlay(args);

    let sub = find_subcommand(args);
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
            // #790: activate memory guardian for CLI builds so graph/BM25 abort
            // checks actually fire (previously only started in daemon mode).
            crate::core::memory_guard::start_guard(std::sync::Arc::new(|level| {
                tracing::warn!(
                    "[index build] memory pressure: {level:?} — background tasks will throttle"
                );
                if level >= crate::core::memory_guard::PressureLevel::Hard {
                    crate::core::content_cache::clear();
                }
                crate::core::memory_guard::force_purge();
                true
            }));
            crate::core::index_orchestrator::ensure_all_background(&project_root);
            wait_graph_bm25_progress(&project_root);

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
            // #790: activate memory guardian for full builds too.
            crate::core::memory_guard::start_guard(std::sync::Arc::new(|level| {
                tracing::warn!("[index build-full] memory pressure: {level:?}");
                if level >= crate::core::memory_guard::PressureLevel::Hard {
                    crate::core::content_cache::clear();
                }
                crate::core::memory_guard::force_purge();
                true
            }));
            crate::core::interrupt::install_ctrlc_handler();
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
            wait_graph_bm25_progress(&project_root);

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
            wait_semantic_progress(&project_root);
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
            // #790: activate memory guardian for semantic builds too.
            crate::core::memory_guard::start_guard(std::sync::Arc::new(|level| {
                tracing::warn!("[index build-semantic] memory pressure: {level:?}");
                if level >= crate::core::memory_guard::PressureLevel::Hard {
                    crate::core::content_cache::clear();
                }
                crate::core::memory_guard::force_purge();
                true
            }));
            crate::core::interrupt::install_ctrlc_handler();
            // Build the dense embedding index on top of BM25.  If BM25 is not yet
            // built, build graph + BM25 first, then build semantic.
            let disk = crate::core::index_orchestrator::disk_status(&project_root);
            if !disk.bm25_index.exists {
                eprintln!("BM25 index not found — building graph + BM25 first ...");
                crate::core::index_orchestrator::ensure_all_background(&project_root);
                wait_graph_bm25_progress(&project_root);
            }

            wait_semantic_progress(&project_root);
            let sem = crate::core::index_orchestrator::semantic_summary(&project_root);
            match sem.state {
                "ready" => eprintln!("semantic index ready"),
                "failed" => eprintln!(
                    "semantic index failed: {}\n  finished files are kept — re-run `lean-ctx index build-semantic` to resume",
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
        Some("why") => {
            let Some(file) = positional_after(args, "why") else {
                eprintln!("Usage: lean-ctx index why <file> [--json] [--root <path>]");
                std::process::exit(2);
            };
            let coverage = crate::core::bm25_index::coverage::explain(root, file);
            if args.iter().any(|a| a == "--json") {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&coverage).unwrap_or_default()
                );
            } else {
                print!("{}", crate::core::bm25_index::coverage::render(&coverage));
            }
            if coverage.exclusion.is_some() {
                std::process::exit(1);
            }
        }
        _ => {
            println!(
                "Usage: lean-ctx index <status|build|build-full|build-graph|build-semantic|watch|why <file>> [--root <path>]\n\
                 Filter flags (#735, apply to this run; persist via [index] config):\n\
                   --exclude <glob>       drop matching files from the corpus (repeatable)\n\
                   --include <glob>       corpus = matching files only (repeatable)\n\
                   --no-gitignore         index .gitignore'd files too\n\
                   --respect-gitignore    honor .gitignore (default)\n\
                 Examples:\n\
                   lean-ctx index status\n\
                   lean-ctx index build              (graph + BM25 indexes)\n\
                   lean-ctx index build-full         (force rebuild all indexes)\n\
                   lean-ctx index build-full --exclude \"**/*.csv\" --exclude \"**/*.jsonl\"\n\
                   lean-ctx index build-semantic --include \"**/*.{{java,kt,ts}}\"\n\
                   lean-ctx index build-graph        (SQLite property graph for impact analysis)\n\
                   lean-ctx index build-semantic     (dense embedding index, builds BM25 first if needed)\n\
                   lean-ctx index watch\n\
                   lean-ctx index why src/Legacy.cs  (is this file indexed? if not, which rule dropped it)"
            );
        }
    }
}

/// The first positional argument after the subcommand `sub`, skipping flags
/// and the values of value-taking flags (see [`find_subcommand`]).
fn positional_after<'a>(args: &'a [String], sub: &str) -> Option<&'a str> {
    let mut seen_sub = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if INDEX_VALUE_FLAGS.contains(&a.as_str()) {
            let _ = it.next();
            continue;
        }
        if a.starts_with("--") {
            continue;
        }
        if seen_sub {
            return Some(a.as_str());
        }
        seen_sub = a == sub;
    }
    None
}

/// Wait for graph + BM25 with a shared progress indicator.
///
/// Uses [`progress_view`] (typed) — no JSON scrape. No deadline: the build runs
/// on a thread of this process, so returning early would exit and kill it
/// before the index is persisted.
fn wait_graph_bm25_progress(project_root: &str) {
    let mut progress = ProgressIndicator::new("indexes");
    loop {
        let view = crate::core::index_orchestrator::progress_view(project_root);
        if !view.graph_bm25_active() {
            progress.finish("indexes (graph + BM25) done (search may still be warming)");
            return;
        }
        apply_graph_bm25_progress(&mut progress, &view);
        progress.tick();
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Build semantic on a worker thread while showing progress.
///
/// Waits for the worker without a deadline: the build runs in this process,
/// so returning early would exit and kill it. A CPU-only build of a large
/// corpus can take hours; Ctrl-C stops it and the checkpointed files are kept.
fn wait_semantic_progress(project_root: &str) {
    let root = project_root.to_string();
    let handle = std::thread::spawn(move || {
        crate::core::index_orchestrator::build_semantic(&root);
    });
    let mut progress = ProgressIndicator::new("semantic");
    loop {
        if handle.is_finished() {
            let _ = handle.join();
            progress.finish("semantic done");
            return;
        }
        let view = crate::core::index_orchestrator::progress_view(project_root);
        apply_semantic_progress(&mut progress, &view);
        progress.tick();
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn apply_graph_bm25_progress(
    progress: &mut ProgressIndicator,
    view: &crate::core::index_orchestrator::ProgressView,
) {
    if !view.graph_run_done {
        progress.set_label("graph");
        progress.indeterminate();
        return;
    }
    // BM25 phase (or graph→BM25 handoff).
    progress.set_label("BM25");
    if view.bm25.total > 0 {
        progress.set(view.bm25.done, view.bm25.total);
    } else {
        progress.indeterminate();
    }
}

fn apply_semantic_progress(
    progress: &mut ProgressIndicator,
    view: &crate::core::index_orchestrator::ProgressView,
) {
    if view.semantic.total > 0 {
        progress.set(view.semantic.done, view.semantic.total);
    } else {
        progress.indeterminate();
    }
}

/// First non-flag token, skipping the values of value-taking flags — so
/// `index build --exclude "**/*.csv"` resolves the subcommand `build`, not the
/// glob (#735).
/// Flags of `lean-ctx index` that consume the next argument.
const INDEX_VALUE_FLAGS: [&str; 4] = ["--exclude", "--include", "--root", "--project-root"];

fn find_subcommand(args: &[String]) -> Option<&str> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if INDEX_VALUE_FLAGS.contains(&a.as_str()) {
            let _ = it.next();
            continue;
        }
        if a.starts_with("--") {
            continue;
        }
        return Some(a.as_str());
    }
    None
}

/// Parse the #735 filter flags and install the per-run overlay. No-op when no
/// filter flag is present, so config-only runs take the config path.
fn install_filter_overlay(args: &[String]) {
    let (include, exclude, respect_gitignore) = parse_filter_flags(args);
    if !include.is_empty() || !exclude.is_empty() || respect_gitignore.is_some() {
        crate::core::index_filter::set_cli_overlay(include, exclude, respect_gitignore);
    }
}

/// Pure #735 flag parser: `--exclude` / `--include` are repeatable and accept
/// both `--flag value` and `--flag=value` forms; the last of `--no-gitignore`
/// / `--respect-gitignore` wins.
fn parse_filter_flags(args: &[String]) -> (Vec<String>, Vec<String>, Option<bool>) {
    let mut include: Vec<String> = Vec::new();
    let mut exclude: Vec<String> = Vec::new();
    let mut respect_gitignore: Option<bool> = None;

    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--exclude" => {
                if let Some(v) = it.next() {
                    exclude.push(v.clone());
                } else {
                    eprintln!("--exclude requires a glob argument");
                }
            }
            "--include" => {
                if let Some(v) = it.next() {
                    include.push(v.clone());
                } else {
                    eprintln!("--include requires a glob argument");
                }
            }
            "--no-gitignore" => respect_gitignore = Some(false),
            "--respect-gitignore" => respect_gitignore = Some(true),
            other => {
                if let Some(v) = other.strip_prefix("--exclude=") {
                    exclude.push(v.to_string());
                } else if let Some(v) = other.strip_prefix("--include=") {
                    include.push(v.to_string());
                }
            }
        }
    }

    (include, exclude, respect_gitignore)
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
    // #735: the watcher observes the same declared corpus as the builders it
    // triggers — a change to an excluded file must not cause rebuild churn.
    let filter = crate::core::index_filter::IndexFileFilter::effective();
    let walker = ignore::WalkBuilder::new(project_root)
        .hidden(true)
        .git_ignore(filter.respect_gitignore)
        .git_global(filter.respect_gitignore)
        .git_exclude(filter.respect_gitignore)
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
        if filter.is_excluded(&rel.replace('\\', "/")) {
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

/// Read the daemon's live session project_root by loading the latest persisted session.
fn daemon_session_project_root() -> Option<String> {
    crate::core::session::SessionState::load_latest().and_then(|s| s.project_root)
}

fn print_human_status(project_root: &str) {
    let disk = crate::core::index_orchestrator::disk_status(project_root);

    println!("  Project:        {project_root}");
    // #1437: surface the daemon session root when it differs from the CLI root
    // so users can diagnose the "status says ready but queries fail" mismatch.
    if let Some(daemon_root) = daemon_session_project_root() {
        // Compare forms, not spellings: `G:/x` and `\\?\G:\x` are one root (#2049).
        let same_root = |a: &str, b: &str| {
            let form = |p: &str| {
                let p = crate::core::pathutil::strip_verbatim_str(p).unwrap_or_else(|| p.into());
                crate::core::pathutil::normalize_tool_path(&p)
            };
            let (a, b) = (form(a), form(b));
            if cfg!(windows) {
                a.eq_ignore_ascii_case(&b)
            } else {
                a == b
            }
        };
        if !same_root(&daemon_root, project_root) {
            println!(
                "  ⚠ Daemon root:  {daemon_root} (differs from CLI — dense queries use this root)"
            );
        }
    }
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
    // #735: surface the active corpus filter so a filtered index is always
    // recognizable. Absent for the default (unfiltered) config, keeping the
    // default output byte-identical.
    if let Some(summary) = crate::core::index_filter::IndexFileFilter::effective().summary() {
        println!("  Index Filters:  {summary}");
    }
}

fn format_disk_line(ds: &crate::core::index_orchestrator::DiskStatus, count_label: &str) -> String {
    if !ds.exists {
        return "not built".to_string();
    }
    if let Some(total) = ds.partial_of {
        return format!(
            "(partial, {}/{total} {count_label} — resume: lean-ctx index build-semantic)",
            ds.file_count.unwrap_or(0)
        );
    }
    // #1914: a persisted index that holds nothing is not "ready" — searches
    // against it return nothing, and the user must be able to tell.
    let state = if ds.file_count == Some(0) {
        "empty"
    } else {
        "ready"
    };
    let mut parts = vec![state.to_string()];
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn empty_index_is_not_reported_ready() {
        use crate::core::index_orchestrator::DiskStatus;
        let empty = DiskStatus {
            exists: true,
            size_bytes: Some(26),
            file_count: Some(0),
            modified_at: None,
            partial_of: None,
        };
        assert_eq!(
            format_disk_line(&empty, "chunks"),
            "(empty, 0 chunks, 26 B)"
        );
        let built = DiskStatus {
            file_count: Some(1200),
            size_bytes: Some(9_437_184),
            ..empty
        };
        assert_eq!(
            format_disk_line(&built, "chunks"),
            "(ready, 1200 chunks, 9.0 MB)"
        );
    }

    #[test]
    fn interrupted_semantic_build_is_reported_partial_with_resume_hint() {
        use crate::core::index_orchestrator::DiskStatus;
        let partial = DiskStatus {
            exists: true,
            size_bytes: Some(4096),
            file_count: Some(1200),
            modified_at: None,
            partial_of: Some(5000),
        };
        assert_eq!(
            format_disk_line(&partial, "vectors"),
            "(partial, 1200/5000 vectors — resume: lean-ctx index build-semantic)"
        );
    }

    #[test]
    fn subcommand_found_before_flags() {
        assert_eq!(
            find_subcommand(&args(&["build", "--root", "/x"])),
            Some("build")
        );
        assert_eq!(find_subcommand(&args(&["status"])), Some("status"));
    }

    #[test]
    fn subcommand_skips_filter_flag_values() {
        // The glob value must never be mistaken for the subcommand (#735).
        assert_eq!(
            find_subcommand(&args(&["--exclude", "**/*.csv", "build-full"])),
            Some("build-full")
        );
        assert_eq!(
            find_subcommand(&args(&["build-semantic", "--include", "**/*.java"])),
            Some("build-semantic")
        );
        assert_eq!(
            find_subcommand(&args(&["--root", "/x", "--no-gitignore", "watch"])),
            Some("watch")
        );
    }

    #[test]
    fn why_takes_the_file_after_the_subcommand() {
        assert_eq!(
            positional_after(&args(&["why", "src/a.cs"]), "why"),
            Some("src/a.cs")
        );
        assert_eq!(
            positional_after(&args(&["--root", "/x", "why", "--json", "b.rs"]), "why"),
            Some("b.rs")
        );
        assert_eq!(positional_after(&args(&["why", "--json"]), "why"), None);
        assert_eq!(find_subcommand(&args(&["why", "b.rs"])), Some("why"));
    }

    #[test]
    fn subcommand_none_for_flag_only_args() {
        assert_eq!(find_subcommand(&args(&["--exclude", "**/*.csv"])), None);
        assert_eq!(find_subcommand(&[]), None);
    }

    #[test]
    fn filter_flags_repeatable_and_both_forms() {
        let (include, exclude, gitignore) = parse_filter_flags(&args(&[
            "build-full",
            "--exclude",
            "**/*.csv",
            "--exclude=**/*.jsonl",
            "--include",
            "**/*.rs",
            "--include=**/*.ts",
        ]));
        assert_eq!(exclude, vec!["**/*.csv", "**/*.jsonl"]);
        assert_eq!(include, vec!["**/*.rs", "**/*.ts"]);
        assert_eq!(gitignore, None);
    }

    #[test]
    fn gitignore_flags_last_one_wins() {
        let (_, _, gitignore) =
            parse_filter_flags(&args(&["build", "--no-gitignore", "--respect-gitignore"]));
        assert_eq!(gitignore, Some(true));
        let (_, _, gitignore) = parse_filter_flags(&args(&["build", "--no-gitignore"]));
        assert_eq!(gitignore, Some(false));
    }

    #[test]
    fn no_filter_flags_yields_empty_overlay_inputs() {
        let (include, exclude, gitignore) = parse_filter_flags(&args(&["build", "--root", "/x"]));
        assert!(include.is_empty());
        assert!(exclude.is_empty());
        assert_eq!(gitignore, None);
    }
}
