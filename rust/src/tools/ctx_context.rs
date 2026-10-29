use crate::core::cache::SessionCache;
use crate::tools::CrpMode;

pub fn handle_status(cache: &SessionCache, turn_count: usize, crp_mode: CrpMode) -> String {
    let entries = cache.get_all_entries();
    let mut result = Vec::new();

    result.push(format!("Multi-turn context (turn {turn_count}):"));
    result.push(format!("  Cached files: {}", entries.len()));

    let total_tokens: usize = entries.iter().map(|(_, e)| e.original_tokens).sum();
    let total_reads: u32 = entries.iter().map(|(_, e)| e.read_count()).sum();
    result.push(format!("  Total original tokens: {total_tokens}"));
    result.push(format!("  Total reads: {total_reads}"));

    let frequent: Vec<_> = entries.iter().filter(|(_, e)| e.read_count() > 1).collect();
    if !frequent.is_empty() {
        result.push(format!("\n  Frequently accessed ({}):", frequent.len()));
        for (path, entry) in &frequent {
            result.push(format!(
                "    {} ({}x, {} tok)",
                crate::core::protocol::shorten_path(path),
                entry.read_count(),
                entry.original_tokens
            ));
        }
    }

    let mode_label = match crp_mode {
        CrpMode::Off => "off",
        CrpMode::Compact => "compact",
        CrpMode::Tdd => "tdd",
    };
    result.push(format!("\n  CRP mode: {mode_label}"));

    let complexity = crate::core::adaptive::classify_from_context(cache);
    result.push(format!("\n  {}", complexity.encoded_suffix()));

    let hints = generate_prefill_hints(cache);
    if !hints.is_empty() {
        result.push("\nSMART HINTS:".to_string());
        for hint in &hints {
            result.push(format!("  → {hint}"));
        }
    }

    result.join("\n")
}

fn generate_prefill_hints(cache: &SessionCache) -> Vec<String> {
    let entries = cache.get_all_entries();
    let mut hints = Vec::new();

    let read_heavy: Vec<_> = entries
        .iter()
        .filter(|(_, e)| e.read_count() >= 3 && e.original_tokens > 500)
        .collect();
    for (path, entry) in &read_heavy {
        let short = crate::core::protocol::shorten_path(path);
        hints.push(format!(
            "{short} read {}x ({} tok) — consider mode=map for future reads",
            entry.read_count(),
            entry.original_tokens
        ));
    }

    let large: Vec<_> = entries
        .iter()
        .filter(|(_, e)| e.original_tokens > 2000 && e.read_count() <= 1)
        .collect();
    for (path, entry) in &large {
        let short = crate::core::protocol::shorten_path(path);
        hints.push(format!(
            "{short} is large ({} tok) — consider mode=signatures or aggressive",
            entry.original_tokens
        ));
    }

    let stale_count = entries.iter().filter(|(_, e)| e.read_count() == 1).count();
    if stale_count > 5 {
        hints.push(format!(
            "{stale_count} files read only once — ctx_control(action=\"evict\", targets=…) frees their context"
        ));
    }

    hints.truncate(5);
    hints
}
