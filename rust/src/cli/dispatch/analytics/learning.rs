//! `lean-ctx learning` — inspect, export and merge the adaptive learning
//! state (#550, VIS-4): LITM placement calibration (#539). Bundles are secret-free (extensions,
//! profiles and aggregate numbers only) and merge idempotently.

use crate::core;

pub(in crate::cli::dispatch) fn cmd_learning(rest: &[String]) {
    let action = rest.first().map_or("status", String::as_str);
    match action {
        "export" => {
            let bundle = core::learning_sync::export_bundle();
            let json = match serde_json::to_string_pretty(&bundle) {
                Ok(j) => j,
                Err(e) => {
                    eprintln!("Export failed: {e}");
                    std::process::exit(1);
                }
            };
            match rest.get(1).map(String::as_str) {
                None | Some("-") => println!("{json}"),
                Some(path) => {
                    if let Err(e) = std::fs::write(path, &json) {
                        eprintln!("Cannot write {path}: {e}");
                        std::process::exit(1);
                    }
                    println!(
                        "Learning bundle written to {path} ({} litm profile(s)).",
                        bundle.litm.per_profile.len()
                    );
                }
            }
        }
        "import" => {
            let Some(src) = rest.get(1) else {
                eprintln!("Usage: lean-ctx learning import <file|->");
                std::process::exit(1);
            };
            let json = if src == "-" {
                use std::io::Read;
                let mut buf = String::new();
                if let Err(e) = std::io::stdin().read_to_string(&mut buf) {
                    eprintln!("Cannot read stdin: {e}");
                    std::process::exit(1);
                }
                buf
            } else {
                match std::fs::read_to_string(src) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("Cannot read {src}: {e}");
                        std::process::exit(1);
                    }
                }
            };
            match core::learning_sync::import_bundle(&json) {
                Ok(report) => println!(
                    "Merged learning bundle: {} litm profile(s). \
                     Max-counters — re-importing is a no-op.",
                    report.litm_profiles
                ),
                Err(e) => {
                    eprintln!("Import failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        "status" | "" => {
            let litm = core::litm_calibration::report();
            let efficacy = core::efficacy::report();
            println!("Adaptive learning state\n{}", "=".repeat(40));
            println!("\nLITM placement calibration:");
            if litm.is_empty() {
                println!("  (calibrating — accumulates as wakeup facts get recalled)");
            } else {
                for l in litm {
                    println!("  {l}");
                }
            }
            if !efficacy.is_empty() {
                println!("\nEfficacy:");
                for l in efficacy {
                    println!("  {l}");
                }
            }
            println!("\nShare with your team: lean-ctx learning export team.json");
        }
        _ => {
            eprintln!("Usage: lean-ctx learning [status|export [file]|import <file|->]");
            std::process::exit(1);
        }
    }
}
