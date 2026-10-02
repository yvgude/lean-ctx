//! `lean-ctx output-savings` — report how much lean-ctx reduced *output* tokens.
//!
//! A fully local read over the measured cohort totals
//! ([`crate::proxy::output_savings`]). When a holdout
//! ([`crate::core::config::ProxyConfig::output_holdout_fraction`]) has produced
//! enough paired turns, this prints a real A/B reduction with a 95 % confidence
//! interval; otherwise it prints the model-based estimate as a band and tells the
//! user how to switch on the holdout to get a measured number.
//!
//! It also reports the input-compression holdout (#1905,
//! [`crate::proxy::compression_savings`]): measured or pending, never estimated.

use crate::proxy::compression_savings::{self, CompressionSavings};
use crate::proxy::output_savings::{self, Savings};

/// Entry point for `lean-ctx output-savings [--json]`.
pub(crate) fn cmd_output_savings(args: &[String]) {
    if args.iter().any(|a| matches!(a.as_str(), "-h" | "--help")) {
        print_usage();
        return;
    }

    let savings = output_savings::current();
    let compression = compression_savings::current();

    if args.iter().any(|a| a == "--json") {
        let mut v = output_savings::to_json(&savings);
        v["compression"] = compression_savings::to_json(&compression);
        println!(
            "{}",
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
        );
    } else {
        println!("{}", format_human(&savings));
        println!("{}", format_compression(&compression));
    }
}

/// The input-compression holdout section (#1905). Never shows an estimate:
/// without the holdout there is no measured baseline, and it says so.
fn format_compression(s: &CompressionSavings) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("lean-ctx — Input Compression (holdout)\n\n");
    match s {
        CompressionSavings::Measured(m) => {
            let _ = writeln!(
                out,
                "  Measured prompt reduction  {:.1}%  (95% CI {:.1}–{:.1}%)",
                m.reduction_pct, m.ci95_low_pct, m.ci95_high_pct
            );
            let _ = writeln!(
                out,
                "  Avg prompt/turn     {:.0} tok uncompressed → {:.0} tok compressed",
                m.control_avg, m.treatment_avg
            );
            let _ = writeln!(
                out,
                "  Sample              {} uncompressed turns · {} compressed turns",
                m.control_n, m.treatment_n
            );
        }
        CompressionSavings::Pending {
            control_n,
            treatment_n,
            needed,
        } => {
            let _ = writeln!(
                out,
                "  Holdout running — {control_n}/{needed} uncompressed, {treatment_n}/{needed} compressed turns."
            );
        }
        CompressionSavings::Off => {
            let _ = writeln!(
                out,
                "  Not measured. Forward a share of conversations uncompressed to get a real baseline:"
            );
            let _ = writeln!(
                out,
                "      lean-ctx config set proxy.compression_holdout 0.1   # 10% control"
            );
        }
    }
    let _ = write!(
        out,
        "  Answer quality      unknown — the holdout measures prompt size, not quality."
    );
    out
}

/// Human-readable, terminal-friendly summary.
fn format_human(s: &Savings) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "lean-ctx — Output Tokens Saved");
    let _ = writeln!(out);
    match s {
        Savings::Measured(m) => {
            let _ = writeln!(
                out,
                "  Measured reduction  {:.1}%  (95% CI {:.1}–{:.1}%)",
                m.reduction_pct, m.ci95_low_pct, m.ci95_high_pct
            );
            let _ = writeln!(
                out,
                "  Avg output/turn     {:.0} tok control → {:.0} tok shaped  (−{:.0} tok/turn)",
                m.control_avg, m.treatment_avg, m.tokens_saved_per_turn
            );
            let _ = writeln!(
                out,
                "  Sample              {} control turns · {} shaped turns",
                m.control_n, m.treatment_n
            );
            let _ = writeln!(out, "\n  Real A/B result from your output_holdout.");
        }
        Savings::Pending {
            control_n,
            treatment_n,
            needed,
        } => {
            let _ = writeln!(
                out,
                "  Holdout running — collecting paired turns ({control_n}/{needed} control, {treatment_n}/{needed} shaped)."
            );
            let _ = writeln!(
                out,
                "  A measured reduction with a 95% CI appears once both arms reach {needed} turns."
            );
        }
        Savings::Estimated {
            point_pct,
            low_pct,
            high_pct,
        } => {
            let _ = writeln!(
                out,
                "  Estimated reduction  ~{point_pct:.0}%  (band {low_pct:.0}–{high_pct:.0}%, model-based)"
            );
            let _ = writeln!(
                out,
                "\n  This is an estimate, not a measurement. To measure your real"
            );
            let _ = writeln!(out, "  output savings, enable a holdout control arm:");
            let _ = writeln!(
                out,
                "      lean-ctx config set proxy.output_holdout 0.1   # 10% control"
            );
        }
    }
    out
}

fn print_usage() {
    println!("Usage: lean-ctx output-savings [--json]");
    println!();
    println!("Report how much lean-ctx reduced LLM *output* tokens on this machine.");
    println!("  (no flags)  Human-readable summary (measured A/B or model estimate)");
    println!("  --json      Machine-readable JSON");
    println!();
    println!("Measured numbers require a holdout: lean-ctx config set proxy.output_holdout 0.1");
    println!("Input compression is measured the same way:  proxy.compression_holdout 0.1");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::output_savings::Measured;

    #[test]
    fn estimated_human_explains_how_to_measure() {
        let s = Savings::Estimated {
            point_pct: 33.3,
            low_pct: 21.7,
            high_pct: 45.0,
        };
        let out = format_human(&s);
        assert!(out.contains("Estimated reduction"));
        assert!(out.contains("output_holdout"), "tells user how to measure");
    }

    #[test]
    fn measured_human_shows_ci_and_sample() {
        let s = Savings::Measured(Measured {
            control_avg: 180.0,
            treatment_avg: 120.0,
            tokens_saved_per_turn: 60.0,
            reduction_pct: 33.3,
            ci95_low_pct: 28.0,
            ci95_high_pct: 38.6,
            control_n: 50,
            treatment_n: 450,
        });
        let out = format_human(&s);
        assert!(out.contains("Measured reduction"));
        assert!(out.contains("95% CI"));
        assert!(out.contains("50 control turns"));
    }

    #[test]
    fn pending_human_shows_progress() {
        let s = Savings::Pending {
            control_n: 5,
            treatment_n: 12,
            needed: 30,
        };
        let out = format_human(&s);
        assert!(out.contains("Holdout running"));
        assert!(out.contains("5/30"));
    }

    #[test]
    fn json_status_matches_variant() {
        let measured = Savings::Measured(Measured {
            control_avg: 180.0,
            treatment_avg: 120.0,
            tokens_saved_per_turn: 60.0,
            reduction_pct: 33.33,
            ci95_low_pct: 28.0,
            ci95_high_pct: 38.6,
            control_n: 50,
            treatment_n: 450,
        });
        let v = output_savings::to_json(&measured);
        assert_eq!(v["status"], "measured");
        assert_eq!(v["reduction_pct"], 33.33);
        assert_eq!(v["control_n"], 50);

        let est = Savings::Estimated {
            point_pct: 33.3,
            low_pct: 21.7,
            high_pct: 45.0,
        };
        assert_eq!(output_savings::to_json(&est)["status"], "estimated");
    }
}
