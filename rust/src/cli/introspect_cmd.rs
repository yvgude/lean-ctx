//! `lean-ctx introspect` — report which cognition subsystems are wired and
//! actually active at runtime. Reads the shared activity registry persisted by
//! the running server (see [`crate::core::introspect`]).

use crate::core::{introspect, qubo_select};

pub(crate) fn cmd_introspect(args: &[String]) {
    let json = args.iter().any(|a| a == "--json");
    let sub = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .map_or("cognition", String::as_str);

    match sub {
        "cognition" => {
            if json {
                println!("{}", introspect::snapshot_json());
            } else {
                print!("{}", introspect::format_report());
            }
        }
        "qubo" => run_qubo_benchmark(),
        other => {
            eprintln!("Unknown introspect target: {other}");
            eprintln!("Usage: lean-ctx introspect <cognition|qubo> [--json]");
            std::process::exit(2);
        }
    }
}

/// `lean-ctx introspect qubo` — run the experimental QUBO-vs-greedy selection
/// benchmark (#10) on a deterministic synthetic problem and print the report.
/// This is a research spike: greedy remains the production default regardless.
fn run_qubo_benchmark() {
    let (items, budget) = qubo_select::synthetic_problem();
    let report = qubo_select::benchmark(&items, budget);
    println!("{}", report.format());
    if !qubo_select::is_enabled() {
        println!(
            "\nnote: QUBO is a benchmark spike and is NOT used for selection.\n\
             set LEAN_CTX_EXPERIMENTAL_QUBO=1 to opt into experiments."
        );
    }
}
