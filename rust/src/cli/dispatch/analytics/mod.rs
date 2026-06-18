//! Analytics command family: gain, savings, billing, conformance and the
//! graph tools. Split by domain (GL#439) — each submodule owns exactly one
//! command surface; this hub re-exports the dispatch entry points.

mod billing;
mod finops;
mod gain;
mod graph;
mod learning;
mod savings;
mod spend;

pub(in crate::cli::dispatch) use billing::cmd_billing;
pub(in crate::cli::dispatch) use finops::cmd_finops;
pub(in crate::cli::dispatch) use gain::cmd_gain;
pub(in crate::cli::dispatch) use graph::{cmd_compact, cmd_graph, cmd_smells};
pub(in crate::cli::dispatch) use learning::cmd_learning;
pub(in crate::cli::dispatch) use savings::cmd_savings;
pub(in crate::cli::dispatch) use spend::cmd_spend;

use crate::core;

/// `lean-ctx conformance [--json]` — run the conformance & reproducibility
/// scorecard (EPIC 12.17) and exit non-zero if any check fails, so CI can gate.
pub(super) fn cmd_conformance(args: &[String]) {
    let card = core::conformance::run();

    if args.iter().any(|a| a == "--json") {
        match serde_json::to_string_pretty(&card.to_json()) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("conformance serialization failed: {e}");
                std::process::exit(1);
            }
        }
    } else {
        println!(
            "Conformance scorecard ({}/{} passed)",
            card.passed(),
            card.total()
        );
        for check in &card.checks {
            let mark = if check.passed { "ok" } else { "FAIL" };
            let detail = if check.detail.is_empty() {
                String::new()
            } else {
                format!(" — {}", check.detail)
            };
            println!("  [{mark}] {}/{}{detail}", check.category, check.name);
        }
    }

    if !card.all_passed() {
        std::process::exit(1);
    }
}
