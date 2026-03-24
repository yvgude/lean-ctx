use std::path::Path;

use crate::core::compressor;
use crate::core::entropy;
use crate::core::signatures;
use crate::core::symbol_map::{self, SymbolMap};
use crate::core::tokens::count_tokens;
use crate::tools::CrpMode;

pub fn handle(path: &str, crp_mode: CrpMode) -> String {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return format!("ERROR: {e}"),
    };

    let short = crate::core::protocol::shorten_path(path);
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    let line_count = content.lines().count();
    let raw_tokens = count_tokens(&content);
    let analysis = entropy::analyze_entropy(&content);
    let entropy_result = entropy::entropy_compress(&content);

    let sigs = signatures::extract_signatures(&content, ext);
    let sig_output: String = sigs
        .iter()
        .map(|s| {
            if crp_mode.is_tdd() {
                s.to_tdd()
            } else {
                s.to_compact()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let sig_tokens = count_tokens(&sig_output);

    let aggressive = compressor::aggressive_compress(&content);
    let agg_tokens = count_tokens(&aggressive);

    let cache_tokens = 13usize;

    let mut sections = Vec::new();
    sections.push(format!("ANALYSIS: {short} ({line_count}L, {raw_tokens} tok)\n"));

    sections.push("Entropy Distribution:".to_string());
    sections.push(format!("  H̄ = {:.1} bits/char", analysis.avg_entropy));
    sections.push(format!(
        "  Low-entropy (H<2.0): {} lines ({:.0}%)",
        analysis.low_entropy_count,
        if analysis.total_lines > 0 {
            analysis.low_entropy_count as f64 / analysis.total_lines as f64 * 100.0
        } else {
            0.0
        }
    ));
    sections.push(format!(
        "  High-entropy (H>4.0): {} lines ({:.0}%)",
        analysis.high_entropy_count,
        if analysis.total_lines > 0 {
            analysis.high_entropy_count as f64 / analysis.total_lines as f64 * 100.0
        } else {
            0.0
        }
    ));

    sections.push(String::new());
    sections.push("Strategy Comparison:".to_string());
    sections.push(format_strategy("raw", raw_tokens, raw_tokens));
    sections.push(format_strategy("aggressive", agg_tokens, raw_tokens));

    let sig_label = if crp_mode.is_tdd() { "signatures (tdd)" } else { "signatures" };
    sections.push(format_strategy(sig_label, sig_tokens, raw_tokens));

    sections.push(format_strategy("entropy", entropy_result.compressed_tokens, raw_tokens));

    if crp_mode.is_tdd() {
        let mut sym = SymbolMap::new();
        let idents = symbol_map::extract_identifiers(&content, ext);
        for ident in &idents {
            sym.register(ident);
        }
        let tdd_agg = sym.apply(&aggressive);
        let tdd_table = sym.format_table();
        let tdd_agg_tokens = count_tokens(&tdd_agg) + count_tokens(&tdd_table);
        sections.push(format_strategy("aggressive + §MAP", tdd_agg_tokens, raw_tokens));
    }

    sections.push(format_strategy("cache hit", cache_tokens, raw_tokens));

    sections.push(String::new());

    let mut strategies = vec![
        ("signatures", sig_tokens),
        ("entropy", entropy_result.compressed_tokens),
        ("aggressive", agg_tokens),
    ];
    if crp_mode.is_tdd() {
        strategies.push(("signatures (tdd)", sig_tokens));
    }
    let best = strategies.iter().min_by_key(|(_, t)| *t).unwrap();
    sections.push(format!("Recommendation: {} (best first-read savings)", best.0));

    sections.join("\n")
}

fn format_strategy(name: &str, tokens: usize, baseline: usize) -> String {
    if tokens >= baseline {
        format!("  {name:<24} {tokens:>6} tok  —")
    } else {
        let pct = ((baseline - tokens) as f64 / baseline as f64 * 100.0).round() as usize;
        format!("  {name:<24} {tokens:>6} tok  -{pct}%")
    }
}
