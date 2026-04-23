use std::collections::HashMap;

use crate::core::cache::SessionCache;
use crate::tools::{CrpMode, ToolCallRecord};

pub fn handle(cache: &SessionCache, tool_calls: &[ToolCallRecord], crp_mode: CrpMode) -> String {
    let cache_stats = cache.get_stats();
    let refs = cache.file_ref_map();

    let total_original: u64 = tool_calls.iter().map(|c| c.original_tokens as u64).sum();
    let total_saved: u64 = tool_calls.iter().map(|c| c.saved_tokens as u64).sum();
    let total_sent = total_original.saturating_sub(total_saved);
    let pct = if total_original > 0 {
        total_saved as f64 / total_original as f64 * 100.0
    } else {
        0.0
    };

    let mut out = Vec::new();
    let env_model = std::env::var("LEAN_CTX_MODEL")
        .or_else(|_| std::env::var("LCTX_MODEL"))
        .ok();
    let pricing = crate::core::gain::model_pricing::ModelPricing::load();
    let quote = pricing.quote(env_model.as_deref());

    if crp_mode.is_tdd() {
        out.push("§metrics".to_string());
        out.push("═".repeat(40));

        out.push(format!(
            "files:{} reads:{} hits:{} ({:.0}%)",
            cache_stats.files_tracked,
            cache_stats.total_reads,
            cache_stats.cache_hits,
            cache_stats.hit_rate()
        ));

        out.push(format!(
            "tok: {}→{} | saved:{} ({:.1}%)",
            format_tokens(total_original),
            format_tokens(total_sent),
            format_tokens(total_saved),
            pct
        ));

        let cost_saved = total_saved as f64 / 1_000_000.0 * quote.cost.input_per_m;
        let cost_without = total_original as f64 / 1_000_000.0 * quote.cost.input_per_m;
        out.push(format!(
            "cost: ${:.4}→${:.4} | -${:.4}",
            cost_without,
            cost_without - cost_saved,
            cost_saved
        ));
    } else {
        out.push("lean-ctx session metrics".to_string());
        out.push("═".repeat(50));

        out.push(format!(
            "Files tracked: {} | Reads: {} | Cache hits: {} ({:.0}%)",
            cache_stats.files_tracked,
            cache_stats.total_reads,
            cache_stats.cache_hits,
            cache_stats.hit_rate()
        ));

        out.push(format!(
            "Input tokens:  {} original → {} sent | {} saved ({:.1}%)",
            format_tokens(total_original),
            format_tokens(total_sent),
            format_tokens(total_saved),
            pct
        ));

        let cost_saved = total_saved as f64 / 1_000_000.0 * quote.cost.input_per_m;
        let cost_without = total_original as f64 / 1_000_000.0 * quote.cost.input_per_m;
        let cost_with = total_sent as f64 / 1_000_000.0 * quote.cost.input_per_m;
        out.push(format!(
            "Cost estimate: ${:.4} without → ${:.4} with lean-ctx | ${:.4} saved",
            cost_without, cost_with, cost_saved
        ));
    }

    if !tool_calls.is_empty() {
        out.push(String::new());

        let sep_w = if crp_mode.is_tdd() { 40 } else { 50 };
        if crp_mode.is_tdd() {
            out.push(format!(
                "{:<12} {:>4} {:>7} {:>7} {:>4}",
                "tool", "n", "orig", "saved", "%"
            ));
        } else {
            out.push("By Tool:".to_string());
            out.push(format!(
                "{:<14} {:>5}  {:>8}  {:>8}  {:>5}",
                "Tool", "Calls", "Original", "Saved", "Avg%"
            ));
        }
        out.push("─".repeat(sep_w));

        let mut by_tool: HashMap<&str, ToolStats> = HashMap::new();
        for call in tool_calls {
            let entry = by_tool.entry(&call.tool).or_default();
            entry.calls += 1;
            entry.original += call.original_tokens;
            entry.saved += call.saved_tokens;
        }

        let mut sorted: Vec<_> = by_tool
            .iter()
            .filter(|(_, ts)| ts.original > 0 || ts.saved > 0)
            .collect();
        sorted.sort_by_key(|x| std::cmp::Reverse(x.1.saved));

        for (tool, ts) in &sorted {
            let avg = if ts.original > 0 {
                ts.saved as f64 / ts.original as f64 * 100.0
            } else {
                0.0
            };
            if crp_mode.is_tdd() {
                out.push(format!(
                    "{:<12} {:>4} {:>7} {:>7} {:>3.0}%",
                    tool,
                    ts.calls,
                    format_tokens(ts.original as u64),
                    format_tokens(ts.saved as u64),
                    avg
                ));
            } else {
                out.push(format!(
                    "{:<14} {:>5}  {:>8}  {:>8}  {:>4.0}%",
                    tool,
                    ts.calls,
                    format_tokens(ts.original as u64),
                    format_tokens(ts.saved as u64),
                    avg
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
            sorted_modes.sort_by_key(|x| std::cmp::Reverse(x.1.saved));

            for (mode, ms) in &sorted_modes {
                if crp_mode.is_tdd() {
                    out.push(format!(
                        "{:<12} {:>4} {:>7}",
                        mode,
                        ms.calls,
                        format_tokens(ms.saved as u64)
                    ));
                } else {
                    out.push(format!(
                        "{:<14} {:>5}  {:>8}",
                        mode,
                        ms.calls,
                        format_tokens(ms.saved as u64)
                    ));
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
                out.push(format!(
                    "  {r}={short} [{}L {}t r:{}]",
                    entry.line_count, entry.original_tokens, entry.read_count
                ));
            } else {
                out.push(format!("  {r}={short}"));
            }
        }
    }

    let projected_session =
        total_saved as f64 / 1_000_000.0 * (quote.cost.input_per_m + quote.cost.output_per_m * 0.3);
    if projected_session > 0.001 {
        out.push(String::new());
        if crp_mode.is_tdd() {
            out.push(format!(
                "∴ session savings (incl. thinking): ${:.3}",
                projected_session
            ));
        } else {
            out.push(format!(
                "Projected session savings (incl. thinking): ${:.3}",
                projected_session
            ));
        }
    }

    let cep = compute_cep_compliance(cache, tool_calls);
    out.push(String::new());
    if crp_mode.is_tdd() {
        out.push("§CEP compliance".to_string());
    } else {
        out.push("CEP Compliance:".to_string());
    }
    out.push(format!(
        "  Cache utilization: {:.0}%  (hit rate for repeated files)",
        cep.cache_utilization * 100.0
    ));
    out.push(format!(
        "  Mode diversity:    {:.0}%  (using optimal modes per file)",
        cep.mode_diversity * 100.0
    ));
    out.push(format!(
        "  Compression rate:  {:.0}%  (overall token reduction)",
        cep.compression_rate * 100.0
    ));
    out.push(format!(
        "  CEP Score:         {:.0}/100",
        cep.overall_score * 100.0
    ));

    let complexity = crate::core::adaptive::classify_from_context(cache);
    out.push(format!("  Task complexity:   {:?}", complexity));

    out.join("\n")
}

struct CepCompliance {
    cache_utilization: f64,
    mode_diversity: f64,
    compression_rate: f64,
    overall_score: f64,
}

fn compute_cep_compliance(cache: &SessionCache, tool_calls: &[ToolCallRecord]) -> CepCompliance {
    let stats = cache.get_stats();

    let cache_utilization = stats.hit_rate() / 100.0;

    let modes_used: std::collections::HashSet<&str> = tool_calls
        .iter()
        .filter_map(|c| c.mode.as_deref())
        .collect();
    let possible_modes = crate::core::budgets::READ_MODE_COUNT;
    let mode_diversity = (modes_used.len() as f64 / possible_modes).min(1.0);

    let total_original: u64 = tool_calls.iter().map(|c| c.original_tokens as u64).sum();
    let total_saved: u64 = tool_calls.iter().map(|c| c.saved_tokens as u64).sum();
    let compression_rate = if total_original > 0 {
        total_saved as f64 / total_original as f64
    } else {
        0.0
    };

    let overall_score = cache_utilization * 0.3 + mode_diversity * 0.2 + compression_rate * 0.5;

    CepCompliance {
        cache_utilization,
        mode_diversity,
        compression_rate,
        overall_score,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cep_compliance_section_present_tdd() {
        let cache = SessionCache::new();
        let calls = vec![ToolCallRecord {
            tool: "ctx_read".to_string(),
            original_tokens: 1000,
            saved_tokens: 300,
            mode: Some("full".to_string()),
            duration_ms: 0,
            timestamp: String::new(),
        }];
        let output = handle(&cache, &calls, CrpMode::Tdd);
        assert!(
            output.contains("§CEP compliance"),
            "TDD output must contain CEP compliance section"
        );
        assert!(output.contains("Cache utilization:"));
        assert!(output.contains("Mode diversity:"));
        assert!(output.contains("Compression rate:"));
        assert!(output.contains("CEP Score:"));
        assert!(output.contains("Task complexity:"));
    }

    #[test]
    fn test_cep_compliance_section_present_normal() {
        let cache = SessionCache::new();
        let calls = vec![];
        let output = handle(&cache, &calls, CrpMode::Off);
        assert!(
            output.contains("CEP Compliance:"),
            "Normal output must contain CEP Compliance section"
        );
        assert!(output.contains("Task complexity:"));
    }

    #[test]
    fn test_cep_scores_zero_with_no_calls() {
        let cache = SessionCache::new();
        let calls = vec![];
        let output = handle(&cache, &calls, CrpMode::Tdd);
        assert!(output.contains("CEP Score:         0/100"));
        assert!(output.contains("Cache utilization: 0%"));
    }

    #[test]
    fn test_format_tokens_units() {
        assert_eq!(format_tokens(500), "500");
        assert_eq!(format_tokens(1500), "1.5K");
        assert_eq!(format_tokens(1_500_000), "1.5M");
    }
}
