use std::collections::HashMap;

use crate::core::cache::SessionCache;
use crate::tools::{CrpMode, ToolCallRecord};

const COST_PER_1M_INPUT: f64 = 15.0;
const COST_PER_1M_OUTPUT: f64 = 75.0;

pub fn handle(cache: &SessionCache, tool_calls: &[ToolCallRecord], crp_mode: CrpMode) -> String {
    let stats = cache.get_stats();
    let refs = cache.file_ref_map();

    let mut out = Vec::new();

    if crp_mode.is_tdd() {
        out.push("§metrics".to_string());
        out.push("═".repeat(40));

        out.push(format!(
            "files:{} reads:{} hits:{} ({:.0}%)",
            stats.files_tracked, stats.total_reads, stats.cache_hits, stats.hit_rate()
        ));

        let saved = stats.tokens_saved();
        let pct = stats.savings_percent();
        out.push(format!(
            "tok: {}→{} | saved:{} ({:.1}%)",
            format_tokens(stats.total_original_tokens),
            format_tokens(stats.total_sent_tokens),
            format_tokens(saved),
            pct
        ));

        let cost_saved = saved as f64 / 1_000_000.0 * COST_PER_1M_INPUT;
        let cost_without = stats.total_original_tokens as f64 / 1_000_000.0 * COST_PER_1M_INPUT;
        out.push(format!("cost: ${:.4}→${:.4} | -${:.4}", cost_without, cost_without - cost_saved, cost_saved));
    } else {
        out.push("lean-ctx session metrics".to_string());
        out.push("═".repeat(50));

        out.push(format!(
            "Files tracked: {} | Reads: {} | Cache hits: {} ({:.0}%)",
            stats.files_tracked, stats.total_reads, stats.cache_hits, stats.hit_rate()
        ));

        let saved = stats.tokens_saved();
        let pct = stats.savings_percent();
        out.push(format!(
            "Input tokens:  {} original → {} sent | {} saved ({:.1}%)",
            format_tokens(stats.total_original_tokens),
            format_tokens(stats.total_sent_tokens),
            format_tokens(saved),
            pct
        ));

        let cost_saved = saved as f64 / 1_000_000.0 * COST_PER_1M_INPUT;
        let cost_without = stats.total_original_tokens as f64 / 1_000_000.0 * COST_PER_1M_INPUT;
        let cost_with = stats.total_sent_tokens as f64 / 1_000_000.0 * COST_PER_1M_INPUT;
        out.push(format!(
            "Cost estimate: ${:.4} without → ${:.4} with lean-ctx | ${:.4} saved",
            cost_without, cost_with, cost_saved
        ));
    }

    if !tool_calls.is_empty() {
        out.push(String::new());

        let sep_w = if crp_mode.is_tdd() { 40 } else { 50 };
        if crp_mode.is_tdd() {
            out.push(format!("{:<12} {:>4} {:>7} {:>7} {:>4}", "tool", "n", "orig", "saved", "%"));
        } else {
            out.push("By Tool:".to_string());
            out.push(format!("{:<14} {:>5}  {:>8}  {:>8}  {:>5}", "Tool", "Calls", "Original", "Saved", "Avg%"));
        }
        out.push("─".repeat(sep_w));

        let mut by_tool: HashMap<&str, ToolStats> = HashMap::new();
        for call in tool_calls {
            let entry = by_tool.entry(&call.tool).or_default();
            entry.calls += 1;
            entry.original += call.original_tokens;
            entry.saved += call.saved_tokens;
        }

        let mut sorted: Vec<_> = by_tool.iter().collect();
        sorted.sort_by(|a, b| b.1.saved.cmp(&a.1.saved));

        for (tool, ts) in &sorted {
            let avg = if ts.original > 0 { ts.saved as f64 / ts.original as f64 * 100.0 } else { 0.0 };
            if crp_mode.is_tdd() {
                out.push(format!(
                    "{:<12} {:>4} {:>7} {:>7} {:>3.0}%",
                    tool, ts.calls, format_tokens(ts.original as u64), format_tokens(ts.saved as u64), avg
                ));
            } else {
                out.push(format!(
                    "{:<14} {:>5}  {:>8}  {:>8}  {:>4.0}%",
                    tool, ts.calls, format_tokens(ts.original as u64), format_tokens(ts.saved as u64), avg
                ));
            }
        }

        let mut by_mode: HashMap<&str, ModeStats> = HashMap::new();
        for call in tool_calls {
            if let Some(ref mode) = call.mode {
                let entry = by_mode.entry(mode).or_default();
                entry.calls += 1;
                entry.saved += call.saved_tokens;
            }
        }

        if !by_mode.is_empty() {
            out.push(String::new());
            if crp_mode.is_tdd() {
                out.push(format!("{:<12} {:>4} {:>7}", "mode", "n", "saved"));
            } else {
                out.push("By Mode:".to_string());
                out.push(format!("{:<14} {:>5}  {:>8}", "Mode", "Calls", "Saved"));
            }
            out.push("─".repeat(if crp_mode.is_tdd() { 28 } else { 30 }));

            let mut sorted_modes: Vec<_> = by_mode.iter().collect();
            sorted_modes.sort_by(|a, b| b.1.saved.cmp(&a.1.saved));

            for (mode, ms) in &sorted_modes {
                if crp_mode.is_tdd() {
                    out.push(format!("{:<12} {:>4} {:>7}", mode, ms.calls, format_tokens(ms.saved as u64)));
                } else {
                    out.push(format!("{:<14} {:>5}  {:>8}", mode, ms.calls, format_tokens(ms.saved as u64)));
                }
            }
        }
    }

    if !refs.is_empty() {
        out.push(String::new());
        if crp_mode.is_tdd() {
            out.push("§refs:".to_string());
        } else {
            out.push("File Refs:".to_string());
        }
        let mut ref_list: Vec<_> = refs.iter().collect();
        ref_list.sort_by_key(|(_, r)| (*r).clone());
        for (path, r) in &ref_list {
            let short = crate::core::protocol::shorten_path(path);
            if let Some(entry) = cache.get(path) {
                out.push(format!("  {r}={short} [{}L {}t r:{}]", entry.line_count, entry.original_tokens, entry.read_count));
            } else {
                out.push(format!("  {r}={short}"));
            }
        }
    }

    let saved = stats.tokens_saved();
    let projected_session = saved as f64 / 1_000_000.0 * (COST_PER_1M_INPUT + COST_PER_1M_OUTPUT * 0.3);
    if projected_session > 0.001 {
        out.push(String::new());
        if crp_mode.is_tdd() {
            out.push(format!("∴ session savings (incl. thinking): ${:.3}", projected_session));
        } else {
            out.push(format!("Projected session savings (incl. thinking): ${:.3}", projected_session));
        }
    }

    out.join("\n")
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        format!("{n}")
    }
}

#[derive(Default)]
struct ToolStats {
    calls: u32,
    original: usize,
    saved: usize,
}

#[derive(Default)]
struct ModeStats {
    calls: u32,
    saved: usize,
}
