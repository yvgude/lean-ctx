// SPDX-License-Identifier: Apache-2.0

use super::{load_mcp_live_stats, mcp_cache_stats_lines, prune_bm25_caches, prune_graph_caches};

pub fn cmd_cache(args: &[String]) {
    if let Err(error) = try_cmd_cache(args) {
        eprintln!("CLI cache operation failed: {error}");
        std::process::exit(1);
    }
}

fn try_cmd_cache(args: &[String]) -> std::io::Result<()> {
    use lean_ctx::core::cli_cache;
    match args.first().map(std::string::String::as_str) {
        Some("clear") => {
            let count = cli_cache::clear()?;
            println!("Cleared {count} cached entries.");
        }
        Some("reset") => {
            let project_flag = args.get(1).map(std::string::String::as_str) == Some("--project");
            if project_flag {
                let root = lean_ctx::core::session::SessionState::load_latest()
                    .and_then(|s| s.project_root);
                if let Some(root) = root {
                    let count = cli_cache::clear_project(&root)?;
                    println!("Reset {count} cache entries for project: {root}");
                } else {
                    eprintln!("No active project root found. Start a session first.");
                    std::process::exit(1);
                }
            } else {
                let count = cli_cache::clear()?;
                println!("Reset all {count} cache entries.");
            }
        }
        Some("stats") => {
            let (hits, reads, entries) = cli_cache::stats()?;
            let rate = if reads > 0 {
                (hits as f64 / reads as f64 * 100.0).round() as u32
            } else {
                0
            };
            println!("CLI Cache Stats (lean-ctx read / lean-ctx grep):");
            println!("  Entries:   {entries}");
            println!("  Reads:     {reads}");
            println!("  Hits:      {hits}");
            println!("  Hit Rate:  {rate}%");

            if let Some(value) = load_mcp_live_stats() {
                println!();
                for line in mcp_cache_stats_lines(&value) {
                    println!("{line}");
                }
            } else {
                println!();
                println!("MCP Session Cache: no data yet (start a session with your AI editor)");
            }
        }
        Some("invalidate") => {
            let Some(path) = invalidate_target(&args[1..]) else {
                eprintln!("Usage: lean-ctx cache invalidate [--] <path>");
                std::process::exit(1);
            };
            cli_cache::invalidate(path)?;
            println!("Invalidated cache for {path}");
        }
        Some("prune") => {
            let bm25 = prune_bm25_caches();
            let graph = prune_graph_caches();
            // Enforce the archive TTL + on-disk size budget alongside the index
            // caches so a manual prune reclaims the (often largest) store too (#417).
            let archive_before = lean_ctx::core::archive::disk_usage_bytes()
                + lean_ctx::core::archive_fts::db_size_bytes();
            let archive_removed = lean_ctx::core::archive::cleanup();
            let _ = lean_ctx::core::archive_fts::enforce_cap();
            let archive_after = lean_ctx::core::archive::disk_usage_bytes()
                + lean_ctx::core::archive_fts::db_size_bytes();
            let archive_freed = archive_before.saturating_sub(archive_after);

            // Reclaim knowledge stores whose project_root was deleted (removed
            // worktrees, thrown-away projects): they can never be written again,
            // so their per-store eviction cap can never self-heal — pure bloat (#615).
            let orphans = lean_ctx::core::knowledge::maintenance::prune_orphaned_stores();

            let removed = bm25.removed + graph.removed + archive_removed + orphans.removed as u32;
            let failed = bm25.failed + graph.failed;
            let freed =
                bm25.bytes_freed + graph.bytes_freed + archive_freed + orphans.reclaimed_bytes;
            println!(
                "Pruned {} entries, freed {:.1} MB (BM25: {}, graphs: {}, archive: {}, orphaned stores: {}, failed: {})",
                removed,
                freed as f64 / 1_048_576.0,
                bm25.removed,
                graph.removed,
                archive_removed,
                orphans.removed,
                failed,
            );
        }
        _ => {
            let (hits, reads, entries) = cli_cache::stats()?;
            let rate = if reads > 0 {
                (hits as f64 / reads as f64 * 100.0).round() as u32
            } else {
                0
            };
            println!("CLI File Cache: {entries} entries, {hits}/{reads} hits ({rate}%)");
            println!();
            println!("Subcommands:");
            println!("  cache stats       Show detailed stats");
            println!("  cache clear       Clear all cached entries");
            println!("  cache reset       Reset all cache (or --project for current project only)");
            println!("  cache invalidate  Remove specific file from cache");
            println!(
                "  cache prune       Reclaim BM25 + graph indexes, archive, and orphaned knowledge stores"
            );
        }
    }
    Ok(())
}

/// The path after `cache invalidate`; a leading `--` ends the options, so
/// `cache invalidate -- -h` names a file called `-h` (#2230).
fn invalidate_target(rest: &[String]) -> Option<&str> {
    match rest {
        [sep, path, ..] if sep == "--" => Some(path),
        [sep] if sep == "--" => None,
        [path, ..] => Some(path),
        [] => None,
    }
}

#[cfg(test)]
mod tests {
    use super::invalidate_target;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn invalidate_target_honours_end_of_options_2230() {
        assert_eq!(invalidate_target(&args(&["--", "-h"])), Some("-h"));
        assert_eq!(invalidate_target(&args(&["src/a.rs"])), Some("src/a.rs"));
        assert_eq!(invalidate_target(&args(&["--"])), None);
        assert_eq!(invalidate_target(&args(&[])), None);
    }
}
