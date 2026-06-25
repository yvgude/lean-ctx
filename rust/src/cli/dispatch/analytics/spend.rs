//! `lean-ctx spend` — measured provider spend (real model + billed tokens).
//!
//! Unlike `gain` (which reports request-side *savings* and prices them with a
//! resolved model), this reads the real per-model usage the proxy extracted from
//! provider responses (`proxy_usage.json`) and prices it with the shared table.
//! Only proxy-routed clients (Claude Code, Codex, Pi, Gemini CLI, `OpenCode`)
//! produce measured data; MCP-only IDEs bypass the proxy and stay estimated.

use crate::core::wrapped::format_tokens;
use crate::proxy::usage_meter::{self, ModelSpend};

pub(in crate::cli::dispatch) fn cmd_spend(args: &[String]) {
    if args
        .iter()
        .any(|a| a == "--help" || a == "-h" || a == "help")
    {
        print_help();
        return;
    }

    let rows = usage_meter::persisted_snapshot();

    if args.iter().any(|a| a == "--json") {
        let total: f64 = rows.iter().map(|r| r.cost_usd).sum();
        let payload = serde_json::json!({
            "source": "measured",
            "available": !rows.is_empty(),
            "total_usd": total,
            "per_model": rows,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string())
        );
        return;
    }

    print_table(&rows);
}

fn print_table(rows: &[ModelSpend]) {
    if rows.is_empty() {
        println!("No measured provider spend yet.");
        println!();
        println!(
            "  Measured cost comes from clients whose LLM traffic is routed through the\n  \
             lean-ctx proxy: Claude Code, Codex, Pi/forge, Gemini CLI, OpenCode.\n  \
             MCP-only IDEs (Cursor, Copilot, Windsurf, …) bypass the proxy — declare a\n  \
             model in [cost] to price them, and see estimated cost via `lean-ctx gain --cost`."
        );
        return;
    }

    println!("Measured provider spend  (real model + billed tokens)");
    println!();
    println!(
        "  {:<24} {:>7} {:>10} {:>10} {:>10} {:>12}",
        "Model", "Reqs", "Input", "Output", "Cache rd", "Cost (USD)"
    );
    println!("  {}", "-".repeat(78));

    let mut total = 0.0;
    for r in rows {
        total += r.cost_usd;
        let model = truncate(&r.model, 24);
        let flag = if r.pricing_estimated { " *" } else { "" };
        println!(
            "  {:<24} {:>7} {:>10} {:>10} {:>10} {:>12}{}",
            model,
            r.requests,
            format_tokens(r.input_tokens),
            format_tokens(r.output_tokens),
            format_tokens(r.cache_read_tokens),
            fmt_usd(r.cost_usd),
            flag,
        );
    }
    println!("  {}", "-".repeat(78));
    println!("  {:<24} {:>52}", "Total", fmt_usd(total));

    if rows.iter().any(|r| r.pricing_estimated) {
        println!();
        println!("  * pricing matched heuristically (no exact entry in the price table).");
    }
    println!();
    println!(
        "  Source: proxy-routed clients. MCP-only IDEs bypass the proxy —\n  \
         see `lean-ctx gain --cost` for their estimated cost."
    );
}

fn fmt_usd(v: f64) -> String {
    if v >= 1.0 {
        format!("${v:.2}")
    } else {
        format!("${v:.4}")
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

fn print_help() {
    println!("Usage: lean-ctx spend [--json]");
    println!();
    println!("Shows the measured provider bill: the real model and billed tokens");
    println!("(input, output, cache reads/writes, reasoning) that the lean-ctx proxy");
    println!("read from upstream responses, priced with the shared model table.");
    println!();
    println!("  --json   Emit machine-readable JSON instead of the table.");
    println!();
    println!("Measured data is produced only by proxy-routed clients (Claude Code,");
    println!("Codex, Pi, Gemini CLI, OpenCode). For estimated cost across all clients,");
    println!("use `lean-ctx gain --cost`.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_usd_switches_precision() {
        assert_eq!(fmt_usd(12.345), "$12.35");
        assert_eq!(fmt_usd(0.0123), "$0.0123");
    }

    #[test]
    fn truncate_long_model_names() {
        assert_eq!(truncate("short", 24), "short");
        let long = "a-really-really-long-model-id-that-overflows";
        let t = truncate(long, 24);
        assert_eq!(t.chars().count(), 24);
        assert!(t.ends_with('…'));
    }
}
