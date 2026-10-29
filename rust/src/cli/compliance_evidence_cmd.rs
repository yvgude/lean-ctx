// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx compliance` CLI (GL #677) — the signed CISO compliance report.
//!
//! Subcommands:
//! - `report` — build, Ed25519-sign and export a report for a date range
//!   (signed JSON artifact, plus an optional CSV/PDF rendering). Framework
//!   coverage comes from the LeanCTX Enterprise runtime;
//! - `facts` — the projection the runtime assesses, i.e. all that leaves the engine;
//! - `verify` — offline-verify a report artifact (no license needed).

use std::path::{Path, PathBuf};

use crate::core::compliance_evidence::{self as compliance_report, render};
use crate::core::intelligence_runtime::compliance_frameworks;

pub(crate) fn cmd_compliance(args: &[String]) {
    match args.first().map(String::as_str) {
        Some("report") => cmd_report(&args[1..]),
        Some("facts") => cmd_facts(&args[1..]),
        Some("verify") => cmd_verify(&args[1..]),
        Some("-h" | "--help") | None => print_help(),
        Some(other) => {
            eprintln!("compliance: unknown subcommand '{other}'\n");
            print_help();
            std::process::exit(2);
        }
    }
}

fn print_help() {
    println!(
        "lean-ctx compliance — signed CISO compliance report (OWASP + frameworks + enforcement)\n\n\
USAGE:\n  \
lean-ctx compliance report --from <rfc3339> --to <rfc3339> \\\n    \
[--framework <id>]... [--pack <name|path>] \\\n    \
[--format json|csv|pdf|text] [--out <file>]\n  \
lean-ctx compliance facts [--framework <id>]... [--pack <name|path>]\n  \
lean-ctx compliance verify <report.json>\n\n\
Framework coverage (EU AI Act, ISO 42001, SOC 2) is LeanCTX Enterprise: the\n\
licensed runtime assesses the facts `compliance facts` prints — nothing else\n\
leaves the engine. `verify` checks a report offline and needs no license.\n\n\
The signed JSON artifact is always written and is the verifiable deliverable.\n\
--format csv|pdf additionally writes that human-readable rendering.\n\n\
EXAMPLES:\n  \
lean-ctx compliance report --from 2026-05-01T00:00:00Z --to 2026-06-01T00:00:00Z\n  \
lean-ctx compliance report --from 2026-05-01T00:00:00Z --to 2026-06-01T00:00:00Z \\\n    \
--framework eu-ai-act --pack .lean-ctx/policy.toml --format pdf --out q2-report.pdf\n  \
lean-ctx compliance verify ~/.local/share/lean-ctx/compliance/report-v1_*.json"
    );
}

fn cmd_report(args: &[String]) {
    let flag = |name: &str| -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|pos| args.get(pos + 1).cloned())
    };
    let multi = |name: &str| -> Vec<String> {
        args.iter()
            .enumerate()
            .filter(|(_, a)| a.as_str() == name)
            .filter_map(|(i, _)| args.get(i + 1).cloned())
            .collect()
    };

    let (Some(from), Some(to)) = (flag("--from"), flag("--to")) else {
        eprintln!("compliance report: --from and --to (RFC 3339) are required\n");
        print_help();
        std::process::exit(2);
    };

    let format = flag("--format").unwrap_or_else(|| "json".to_string());
    if !matches!(format.as_str(), "json" | "csv" | "pdf" | "text") {
        eprintln!("compliance report: --format must be one of json|csv|pdf|text");
        std::process::exit(2);
    }

    let spec = compliance_report::ReportSpec {
        from,
        to,
        frameworks: multi("--framework"),
        pack: flag("--pack"),
    };

    let mut report = match compliance_report::build(&spec, compliance_frameworks::assess) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("compliance report: {e}");
            std::process::exit(1);
        }
    };

    let agent_id = crate::core::agent_identity::current_agent_id();
    if let Err(e) = report.sign(agent_id) {
        eprintln!("compliance report: signing failed: {e}");
        std::process::exit(1);
    }

    if format == "text" {
        print!("{}", render::to_text(&report));
        return;
    }

    let (json_path, render_path) = match derive_paths(flag("--out").map(PathBuf::from), &format) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("compliance report: {e}");
            std::process::exit(1);
        }
    };

    let json_path = match compliance_report::write_artifact(&report, &json_path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("compliance report: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "Signed compliance report written to {}",
        json_path.display()
    );

    if let Some(render_path) = render_path {
        let bytes = match format.as_str() {
            "csv" => render::to_csv(&report).into_bytes(),
            "pdf" => compliance_report::pdf::to_pdf(&render::to_text(&report)),
            _ => unreachable!("text/json handled above"),
        };
        if let Err(e) = std::fs::write(&render_path, &bytes) {
            eprintln!("compliance report: write {}: {e}", render_path.display());
            std::process::exit(1);
        }
        println!("Rendering ({format}) written to {}", render_path.display());
    }

    println!(
        "  Period:      {} .. {}",
        report.period.from, report.period.to
    );
    println!(
        "  Blocked:     {}   Redacted: {}   Tool calls: {}",
        report.enforcement.blocked, report.enforcement.redacted, report.enforcement.tool_calls
    );
    println!(
        "  Chain:       {}",
        if report.audit.chain_valid {
            "valid (SHA-256 intact)"
        } else {
            "BROKEN"
        }
    );
    if let Some(pk) = &report.signer_public_key {
        println!("  Signer key:  {pk}");
    }
    println!(
        "\nVerify offline (no LeanCTX needed):  lean-ctx compliance verify {}",
        json_path.display()
    );
}

/// Resolve the JSON artifact path and the optional rendering path from `--out`
/// and the chosen format. The signed JSON is always produced; a csv/pdf
/// rendering lands beside it with the matching extension (so `--out q2.pdf
/// --format pdf` yields `q2.pdf` + the verifiable `q2.json`).
fn derive_paths(out: Option<PathBuf>, format: &str) -> Result<(PathBuf, Option<PathBuf>), String> {
    let render_ext = match format {
        "csv" => Some("csv"),
        "pdf" => Some("pdf"),
        _ => None,
    };
    match out {
        Some(out) if format == "json" => Ok((out, None)),
        Some(out) => Ok((
            out.with_extension("json"),
            render_ext.map(|ext| out.with_extension(ext)),
        )),
        None => {
            let json = compliance_report::default_artifact_path()?;
            let render = render_ext.map(|ext| json.with_extension(ext));
            Ok((json, render))
        }
    }
}

fn cmd_facts(args: &[String]) {
    let flag = |name: &str| -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|pos| args.get(pos + 1).cloned())
    };
    let frameworks: Vec<String> = args
        .iter()
        .enumerate()
        .filter(|(_, a)| a.as_str() == "--framework")
        .filter_map(|(i, _)| args.get(i + 1).cloned())
        .collect();
    let resolved = match compliance_report::resolve_pack(flag("--pack").as_deref()) {
        Ok(resolved) => resolved,
        Err(e) => {
            eprintln!("compliance facts: {e}");
            std::process::exit(1);
        }
    };
    let facts = compliance_report::facts::facts(&frameworks, Some(&resolved));
    match serde_json::to_string_pretty(&facts) {
        Ok(json) => println!("{json}"),
        Err(e) => {
            eprintln!("compliance facts: {e}");
            std::process::exit(1);
        }
    }
}

fn cmd_verify(args: &[String]) {
    let Some(path) = args.first() else {
        eprintln!("compliance verify: a report JSON path is required\n");
        print_help();
        std::process::exit(2);
    };
    let report = match compliance_report::load_artifact(Path::new(path)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("compliance verify: {e}");
            std::process::exit(1);
        }
    };
    let result = report.verify();
    if result.signature_valid {
        println!("VALID — signature verifies (Ed25519, offline)");
        if let Some(pk) = &result.signer_public_key {
            println!("  Signer key: {pk}");
        }
        println!(
            "  Period:     {} .. {}",
            report.period.from, report.period.to
        );
        println!(
            "  Blocked:    {}   Redacted: {}",
            report.enforcement.blocked, report.enforcement.redacted
        );
        println!("  Audit head: {}", report.audit.head_hash);
    } else {
        eprintln!(
            "INVALID — {}",
            result
                .error
                .as_deref()
                .unwrap_or("signature does not verify")
        );
        std::process::exit(1);
    }
}
