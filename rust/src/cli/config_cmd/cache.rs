// SPDX-License-Identifier: Apache-2.0

use super::{load_mcp_live_stats, mcp_cache_stats_lines, prune_bm25_caches, prune_graph_caches};

pub fn cmd_cache(args: &[String]) {
    if let Err(error) = try_cmd_cache(args) {
        eprintln!("CLI cache operation failed: {error}");
        std::process::exit(1);
    }
}

fn try_cmd_cache(args: &[String]) -> std::io::Result<()> {
    use crate::core::cli_cache;
    match args.first().map(std::string::String::as_str) {
        Some("clear") => {
            let count = cli_cache::clear()?;
            println!("Cleared {count} cached entries.");
        }
        Some("reset") => {
            let project_flag = args.get(1).map(std::string::String::as_str) == Some("--project");
            if project_flag {
                let root =
                    crate::core::session::SessionState::load_latest().and_then(|s| s.project_root);
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
            if args.len() < 2 {
                eprintln!("Usage: lean-ctx cache invalidate <path>");
                std::process::exit(1);
            }
            cli_cache::invalidate(&args[1])?;
            println!("Invalidated cache for {}", args[1]);
        }
        Some("prune") => {
            let bm25 = prune_bm25_caches();
            let graph = prune_graph_caches();
            // Enforce the archive TTL + on-disk size budget alongside the index
            // caches so a manual prune reclaims the (often largest) store too (#417).
            let archive_before = crate::core::archive::disk_usage_bytes()
                + crate::core::archive_fts::db_size_bytes();
            let archive_removed = crate::core::archive::cleanup();
            let _ = crate::core::archive_fts::enforce_cap();
            let archive_after = crate::core::archive::disk_usage_bytes()
                + crate::core::archive_fts::db_size_bytes();
            let archive_freed = archive_before.saturating_sub(archive_after);

            // Reclaim knowledge stores whose project_root was deleted (removed
            // worktrees, thrown-away projects): they can never be written again,
            // so their per-store eviction cap can never self-heal — pure bloat (#615).
            let orphans = crate::core::knowledge::maintenance::prune_orphaned_stores();

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
