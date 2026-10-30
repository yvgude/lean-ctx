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
lean-ctx compliance verify <report.json> [--pubkey <hex>]\n\n\
Framework coverage (EU AI Act, ISO 42001, SOC 2) is LeanCTX Enterprise: the\n\
licensed runtime assesses the facts `compliance facts` prints — nothing else\n\
leaves the engine. `verify` checks a report offline and needs no license; it\n\
checks the signer against --pubkey or this installation's key and reports a\n\
report signed by any other key as SELF-SIGNED / UNPINNED (exit 3).\n\n\
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

/// The signer `verify` expects: `--pubkey <hex>` received out of band, else
/// this installation's own report key (the key `report` signs with).
fn expected_signer(args: &[String]) -> Option<(String, &'static str)> {
    if let Some(pk) = args
        .iter()
        .position(|a| a == "--pubkey")
        .and_then(|pos| args.get(pos + 1))
    {
        return Some((pk.clone(), "--pubkey"));
    }
    let agent_id = crate::core::agent_identity::current_agent_id();
    crate::core::agent_identity::get_stored_public_key(agent_id)
        .ok()
        .map(|key| {
            (
                crate::core::agent_identity::hex_encode(&key.to_bytes()),
                "this installation",
            )
        })
}

fn cmd_verify(args: &[String]) {
    let pubkey_value = args.iter().position(|a| a == "--pubkey").map(|pos| pos + 1);
    let Some(path) = args
        .iter()
        .enumerate()
        .find(|(i, a)| !a.starts_with("--") && Some(*i) != pubkey_value)
        .map(|(_, a)| a)
    else {
        eprintln!("compliance verify: a report JSON path is required\n");
        print_help();
        std::process::exit(2);
    };
    if pubkey_value.is_some_and(|i| args.get(i).is_none()) {
        eprintln!("compliance verify: --pubkey needs a hex public key");
        std::process::exit(2);
    }
    let report = match compliance_report::load_artifact(Path::new(path)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("compliance verify: {e}");
            std::process::exit(1);
        }
    };
    let expected = expected_signer(args);
    let explicit = expected.as_ref().is_some_and(|(_, src)| *src == "--pubkey");
    let mut result = match &expected {
        Some((pk, _)) => report.verify_signer(pk),
        None => report.verify(),
    };
    // A report from another installation is not invalid, only unpinned:
    // only an explicit --pubkey makes a different signer a failure.
    if !explicit && !result.signature_valid && expected.is_some() {
        let self_check = report.verify();
        if self_check.signature_valid {
            result = self_check;
        }
    }
    if result.signature_valid && !result.signer_pinned {
        println!(
            "SELF-SIGNED / UNPINNED — the signature matches the key embedded in the report, \
             which proves integrity only: anyone can re-sign an edited report"
        );
        if let Some(pk) = &result.signer_public_key {
            println!("  Signer key: {pk}");
        }
        println!("  Pass --pubkey <hex> with the producer's key to verify who signed it.");
        std::process::exit(3);
    }
    if result.signature_valid {
        let source = expected.as_ref().map_or("", |(_, src)| *src);
        println!(
            "VALID — signature verifies against the expected signer ({source}; Ed25519, offline)"
        );
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
