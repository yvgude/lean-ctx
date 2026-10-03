//! `lean-ctx roi` — print the recorded savings (ROI) report.
//!
//! A fully local, Local-Free surface over the signed savings ledger
//! ([`crate::core::savings_ledger::roi`]). It only *reads* the ledger and renders
//! a shareable, signature-backed summary of how many tokens — and how much money
//! — lean-ctx has saved on this machine. The signature makes the record
//! tamper-evident; the numbers themselves are local token counts (before vs.
//! after lean-ctx), not provider-billed usage, and the report says so. Producing your own ROI report is a local
//! capability, so it is never gated by a plan (the paid surface is the *team*
//! roll-up across many developers, not your own numbers).

use crate::core::savings_ledger::{RoiReport, roi_report};

/// What the ledger numbers are. Provider-measured savings exist only on the proxy
/// path with counterfactual metering (#701, `lean-ctx proxy status`).
const MEASUREMENT_BASIS: &str =
    "local token counts (before vs. after lean-ctx), not provider-billed usage";

/// Entry point for `lean-ctx roi [report] [--json|--md] [--export <path>]`.
pub(crate) fn cmd_roi(args: &[String]) {
    // `lean-ctx roi` and `lean-ctx roi report` are the same thing; drop a leading
    // `report` subcommand so both spellings work.
    let args: Vec<String> = args
        .iter()
        .filter(|a| a.as_str() != "report")
        .cloned()
        .collect();

    if args.iter().any(|a| matches!(a.as_str(), "-h" | "--help")) {
        print_usage();
        return;
    }

    let report = roi_report(crate::core::agent_identity::current_agent_id());

    if let Some(path) = arg_value(&args, "--export") {
        export_report(&report, &path);
        return;
    }

    if args.iter().any(|a| a == "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string())
        );
    } else if args
        .iter()
        .any(|a| matches!(a.as_str(), "--md" | "--markdown"))
    {
        println!("{}", format_markdown(&report));
    } else {
        println!("{}", format_human(&report));
    }
}

/// Write the report to a file; the format is inferred from the extension
/// (`.json` → JSON, anything else → Markdown), so `--export roi.md` and
/// `--export roi.json` both do the obvious thing.
fn export_report(report: &RoiReport, path: &str) {
    let is_json = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("json"));
    let body = if is_json {
        serde_json::to_string_pretty(report).unwrap_or_default()
    } else {
        format_markdown(report)
    };
    match std::fs::write(path, body) {
        Ok(()) => println!("ROI report written to {path}"),
        Err(e) => {
            eprintln!("Could not write {path}: {e}");
            std::process::exit(1);
        }
    }
}

/// Value following `flag` in `args`, if present (`--export roi.md` → `roi.md`).
fn arg_value(args: &[String], flag: &str) -> Option<String> {
    let pos = args.iter().position(|a| a == flag)?;
    args.get(pos + 1).cloned()
}

/// Group digits with thousands separators: `1234567` → `1,234,567`.
fn fmt_int(n: u64) -> String {
    let digits = n.to_string();
    let len = digits.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Like [`fmt_int`] but for signed values — the net-after-injection figure can
/// legitimately go negative on a short, non-cached run.
fn fmt_signed(n: i64) -> String {
    if n < 0 {
        format!("-{}", fmt_int(n.unsigned_abs()))
    } else {
        fmt_int(n.unsigned_abs())
    }
}

/// Short, signed-chain provenance suffix, e.g. `signed (a1b2c3…)`.
fn provenance(report: &RoiReport) -> String {
    let chain = if report.chain_valid {
        "valid"
    } else {
        "BROKEN"
    };
    let signed = if report.signed { "signed" } else { "unsigned" };
    match (report.signed, &report.signer_public_key) {
        (true, Some(key)) => {
            let short = key.get(..16).unwrap_or(key);
            format!("chain {chain}, {signed} ({short}…)")
        }
        _ => format!("chain {chain}, {signed}"),
    }
}

/// Human-readable, terminal-friendly report.
fn format_human(report: &RoiReport) -> String {
    use std::fmt::Write as _;
    if report.total_events == 0 {
        return "lean-ctx ROI: no savings recorded yet.\n\
                Use lean-ctx (ctx_read / ctx_search / …) for a while, then run `lean-ctx roi` again."
            .to_string();
    }

    let mut s = String::new();
    let _ = writeln!(s, "lean-ctx — Recorded Savings (ROI)");
    let _ = writeln!(
        s,
        "Period {} · generated {}",
        report.period, report.created_at
    );
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "  Net tokens saved   {}",
        fmt_int(report.net_saved_tokens)
    );
    if report.turns > 0 {
        let _ = writeln!(
            s,
            "  After injection    {}  ({} tok/turn × {} turns)",
            fmt_signed(report.net_after_overhead_tokens),
            fmt_int(report.injected_overhead_tokens_per_turn),
            fmt_int(report.turns),
        );
    }
    let _ = writeln!(s, "  Estimated $ saved  ${:.2}", report.saved_usd);
    let _ = writeln!(
        s,
        "  Events             {}",
        fmt_int(report.total_events as u64)
    );
    let _ = writeln!(
        s,
        "  Avg per event      {:.0} tok  (${:.4})",
        report.avg_saved_tokens_per_event, report.avg_saved_usd_per_event
    );

    if !report.top_models.is_empty() {
        let _ = writeln!(s, "\n  Top models");
        for (model, tokens, usd) in report.top_models.iter().take(5) {
            let _ = writeln!(s, "    {model:<24} {:>15} tok  ${usd:.2}", fmt_int(*tokens));
        }
    }
    if !report.top_tools.is_empty() {
        let _ = writeln!(s, "\n  Top tools");
        for (tool, tokens) in report.top_tools.iter().take(5) {
            let _ = writeln!(s, "    {tool:<24} {:>15} tok", fmt_int(*tokens));
        }
    }

    let _ = writeln!(s, "\n  Basis         {MEASUREMENT_BASIS}");
    let _ = writeln!(s, "  Verification  {}", provenance(report));
    let _ = writeln!(s, "  Share it      lean-ctx roi --export roi.md");
    s
}

/// Markdown report — the shareable artifact (manager / finance / README ready).
fn format_markdown(report: &RoiReport) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(s, "# lean-ctx — Recorded Savings (ROI)\n");
    let _ = writeln!(
        s,
        "- **Net tokens saved:** {}",
        fmt_int(report.net_saved_tokens)
    );
    if report.turns > 0 {
        let _ = writeln!(
            s,
            "- **Net after injection:** {}  ({} tok/turn × {} turns)",
            fmt_signed(report.net_after_overhead_tokens),
            fmt_int(report.injected_overhead_tokens_per_turn),
            fmt_int(report.turns),
        );
    }
    let _ = writeln!(s, "- **Estimated $ saved:** ${:.2}", report.saved_usd);
    let _ = writeln!(s, "- **Events:** {}", fmt_int(report.total_events as u64));
    let _ = writeln!(s, "- **Period:** {}", report.period);
    let _ = writeln!(s, "- **Generated:** {}", report.created_at);
    let _ = writeln!(s, "- **Basis:** {MEASUREMENT_BASIS}");
    let _ = writeln!(s, "- **Verification:** {}", provenance(report));

    if !report.top_models.is_empty() {
        let _ = writeln!(
            s,
            "\n## Top models\n\n| Model | Tokens saved | $ saved |\n|---|--:|--:|"
        );
        for (model, tokens, usd) in report.top_models.iter().take(10) {
            let _ = writeln!(s, "| {model} | {} | ${usd:.2} |", fmt_int(*tokens));
        }
    }
    if !report.top_tools.is_empty() {
        let _ = writeln!(s, "\n## Top tools\n\n| Tool | Tokens saved |\n|---|--:|");
        for (tool, tokens) in report.top_tools.iter().take(10) {
            let _ = writeln!(s, "| {tool} | {} |", fmt_int(*tokens));
        }
    }

    let _ = writeln!(
        s,
        "\n_Generated by lean-ctx — numbers derived from a local, Ed25519-signed savings ledger._"
    );
    s
}

fn print_usage() {
    println!("Usage: lean-ctx roi [--json | --md] [--export <path>]");
    println!();
    println!("Print the recorded savings (ROI) report from your local signed ledger.");
    println!("  (no flags)        Human-readable summary");
    println!("  --json            Machine-readable JSON (the RoiReport)");
    println!("  --md, --markdown  Markdown (shareable)");
    println!("  --export <path>   Write to a file (.json → JSON, else Markdown)");
    println!();
    println!("Team roll-up across developers (opt-in): lean-ctx savings team");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(events: usize) -> RoiReport {
        RoiReport {
            period: "all".to_string(),
            created_at: "2026-06-09T00:00:00Z".to_string(),
            lean_ctx_version: "test".to_string(),
            agent_id: "agent-1".to_string(),
            last_entry_hash: "deadbeef".to_string(),
            chain_valid: true,
            signed: true,
            signer_public_key: Some("0123456789abcdef0123456789abcdef".to_string()),
            total_events: events,
            saved_tokens: 1_234_567,
            net_saved_tokens: 1_234_000,
            saved_usd: 3.70,
            avg_saved_tokens_per_event: 1000.0,
            avg_saved_usd_per_event: 0.003,
            top_models: vec![("gpt-5".to_string(), 1_000_000, 3.0)],
            top_tools: vec![("ctx_read".to_string(), 900_000)],
            // An observed run: 1,200 tok/turn × 20 turns = 24,000 tax → 1,210,000 net.
            injected_overhead_tokens_per_turn: 1_200,
            turns: 20,
            injected_overhead_total_tokens: 24_000,
            net_after_overhead_tokens: 1_210_000,
        }
    }

    #[test]
    fn fmt_int_groups_thousands() {
        assert_eq!(fmt_int(0), "0");
        assert_eq!(fmt_int(999), "999");
        assert_eq!(fmt_int(1_000), "1,000");
        assert_eq!(fmt_int(1_234_567), "1,234,567");
    }

    #[test]
    fn human_report_shows_headline_numbers() {
        let out = format_human(&sample(4));
        assert!(out.contains("Recorded Savings (ROI)"));
        assert!(out.contains("1,234,000"), "net tokens with separators");
        assert!(out.contains("$3.70"), "dollar headline");
        assert!(out.contains("ctx_read"), "top tool");
        assert!(out.contains("signed"), "provenance");
        // A signature proves the record was not altered, not that a provider billed
        // these savings: every shareable form states the basis and never says
        // "verified savings".
        for report in [out, format_markdown(&sample(4))] {
            assert!(report.contains(MEASUREMENT_BASIS), "{report}");
            assert!(
                !report.to_lowercase().contains("verified savings"),
                "{report}"
            );
        }
    }

    #[test]
    fn human_report_shows_injection_net_when_turns_observed() {
        let out = format_human(&sample(4));
        assert!(out.contains("After injection"), "injection line present");
        assert!(out.contains("1,210,000"), "net after injection");
        assert!(out.contains("1,200 tok/turn × 20 turns"), "tax breakdown");
    }

    #[test]
    fn reports_hide_injection_line_without_observed_turns() {
        let mut r = sample(4);
        r.turns = 0;
        r.injected_overhead_total_tokens = 0;
        r.net_after_overhead_tokens = r.net_saved_tokens as i64;
        let human = format_human(&r);
        let md = format_markdown(&r);
        assert!(!human.contains("After injection"), "human: {human}");
        assert!(!md.contains("Net after injection"), "md: {md}");
    }

    #[test]
    fn fmt_signed_handles_negative_net() {
        assert_eq!(fmt_signed(-1_234_000), "-1,234,000");
        assert_eq!(fmt_signed(0), "0");
        assert_eq!(fmt_signed(2_500), "2,500");
    }

    #[test]
    fn empty_ledger_is_friendly_not_blank() {
        let out = format_human(&sample(0));
        assert!(out.contains("no savings recorded yet"));
    }

    #[test]
    fn markdown_report_has_heading_and_tables() {
        let md = format_markdown(&sample(4));
        assert!(md.starts_with("# lean-ctx — Recorded Savings (ROI)"));
        assert!(md.contains("| Model | Tokens saved | $ saved |"));
        assert!(md.contains("| Tool | Tokens saved |"));
        assert!(md.contains("Ed25519-signed"));
        assert!(
            md.contains("**Net after injection:** 1,210,000"),
            "markdown injection line: {md}"
        );
    }

    #[test]
    fn provenance_includes_short_signer_key() {
        let p = provenance(&sample(1));
        assert!(p.contains("chain valid"));
        assert!(p.contains("signed"));
        assert!(p.contains("0123456789abcdef…"), "short signer key suffix");
    }

    #[test]
    fn arg_value_reads_flag_argument() {
        let args = vec!["--export".to_string(), "roi.md".to_string()];
        assert_eq!(arg_value(&args, "--export").as_deref(), Some("roi.md"));
        assert_eq!(arg_value(&args, "--missing"), None);
    }
}
