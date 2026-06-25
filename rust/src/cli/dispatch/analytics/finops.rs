//! `lean-ctx finops` — export ledger costs/savings for `FinOps` platforms
//! (GL #402): `CloudZero` CBF, Vantage custom provider, FOCUS 1.2 CSV.

use std::path::PathBuf;

use crate::core::finops_export::{self, DateRange, aliases::ProjectAliases};

pub(in crate::cli::dispatch) fn cmd_finops(args: &[String]) {
    let action = args.first().map_or("export", String::as_str);
    if action == "--help" || action == "-h" || action == "help" {
        print_help();
        return;
    }
    if action != "export" {
        eprintln!("Unknown finops action `{action}`.");
        print_help();
        std::process::exit(1);
    }

    let target = flag(args, "--target=").unwrap_or_else(|| "focus".to_string());
    let range = DateRange {
        from: flag(args, "--from="),
        to: flag(args, "--to="),
    };
    let out_path = flag(args, "--out=");
    let do_upload = args.iter().any(|a| a == "--upload");

    let mut rows = finops_export::aggregate(&range);
    if rows.is_empty() {
        eprintln!(
            "No ledger events in range. The savings ledger is the data source — \
             run some sessions first (`lean-ctx ledger verify` shows the chain)."
        );
        std::process::exit(1);
    }

    // #668: opt-in, export-time only `repo_hash -> readable name` showback. The
    // ledger and signed batch stay privacy-preserving; unmapped hashes pass
    // through unchanged.
    let aliases = ProjectAliases::load(flag(args, "--aliases=").map(PathBuf::from).as_deref());
    if !aliases.is_empty() {
        aliases.apply(&mut rows);
        eprintln!("Applied {} project alias(es) for showback.", aliases.len());
    }
    let (days, first, last) = (
        rows.len(),
        rows.first().map(|r| r.date.clone()).unwrap_or_default(),
        rows.last().map(|r| r.date.clone()).unwrap_or_default(),
    );

    let csv = match target.as_str() {
        "focus" | "csv" => finops_export::focus::to_csv(&rows),
        "cbf" | "cloudzero" => finops_export::cbf::to_csv(&rows),
        "vantage" => finops_export::vantage::to_csv(&rows),
        other => {
            eprintln!("Unknown target `{other}` (focus|cbf|vantage).");
            std::process::exit(1);
        }
    };

    match &out_path {
        Some(path) => {
            if let Err(e) = std::fs::write(path, &csv) {
                eprintln!("Failed to write {path}: {e}");
                std::process::exit(1);
            }
            eprintln!(
                "Wrote {} rows ({days} day-groups, {first}..{last}) to {path}",
                csv.lines().count() - 1
            );
        }
        None => print!("{csv}"),
    }

    if do_upload {
        let result = match target.as_str() {
            "cbf" | "cloudzero" => {
                // One Stream drop per month — replace_drop keeps it idempotent.
                let mut months: Vec<String> =
                    rows.iter().map(|r| r.date[..7].to_string()).collect();
                months.sort();
                months.dedup();
                months
                    .iter()
                    .map(|month| {
                        let month_rows: Vec<_> = rows
                            .iter()
                            .filter(|r| r.date.starts_with(month.as_str()))
                            .cloned()
                            .collect();
                        finops_export::cbf::upload(&month_rows, month)
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map(|msgs| msgs.join("\n"))
            }
            "vantage" => finops_export::vantage::upload(&csv).map(|msg| {
                format!(
                    "{msg}\nNote: Vantage uploads are additive — delete the previous \
                     dataset for {first}..{last} in Settings → Integrations before re-sending."
                )
            }),
            _ => Err(
                "--upload supports --target=cbf or --target=vantage (FOCUS is file-only)".into(),
            ),
        };
        match result {
            Ok(msg) => eprintln!("{msg}"),
            Err(e) => {
                eprintln!("Upload failed: {e}");
                std::process::exit(1);
            }
        }
    }
}

fn flag(args: &[String], prefix: &str) -> Option<String> {
    args.iter()
        .find_map(|a| a.strip_prefix(prefix))
        .map(str::to_string)
}

fn print_help() {
    println!(
        "Usage: lean-ctx finops export [--target=focus|cbf|vantage] [--from=YYYY-MM-DD] [--to=YYYY-MM-DD] [--out=FILE] [--aliases=FILE] [--upload]"
    );
    println!();
    println!("Exports daily cost/savings rows derived from the hash-chained savings");
    println!("ledger (verified numbers; model price pinned per event at record time).");
    println!();
    println!("Showback names (--aliases, #668): the ledger only stores a truncated repo");
    println!("hash (never a path). An opt-in TOML map turns those into readable project");
    println!("names at export time only — the ledger/signed batch stay untouched:");
    println!("  default: <config_dir>/finops-aliases.toml  (or env LEAN_CTX_FINOPS_ALIASES)");
    println!("  format:  [projects]\\n  <repo_hash> = \"Team name\"");
    println!("  unmapped hashes fall back to the hash.");
    println!();
    println!("Targets:");
    println!("  focus    FOCUS 1.2 CSV (FinOps Foundation spec) — generic, file-only");
    println!("  cbf      CloudZero Common Bill Format; --upload posts per-month");
    println!("           AnyCost Stream drops (replace_drop = idempotent re-runs)");
    println!("           env: CLOUDZERO_API_KEY, CLOUDZERO_CONNECTION_ID");
    println!("  vantage  Vantage custom-provider CSV; --upload posts multipart");
    println!("           env: VANTAGE_API_TOKEN, VANTAGE_INTEGRATION_TOKEN");
    println!();
    println!("Savings appear as Credit (FOCUS/Vantage) or Discount (CBF) rows with");
    println!("negative cost — Usage spend stays clean for budgeting.");
    println!("Docs: docs/integrations/finops.md");
}
