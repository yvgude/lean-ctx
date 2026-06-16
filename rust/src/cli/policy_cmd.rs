//! `lean-ctx policy` — Context Policy Packs v1 (GL #489).
//!
//! Subcommands:
//! * `policy list`             — built-in packs (+ the project pack if present)
//! * `policy show <name>`      — resolved effective policy (`--toml` for raw TOML)
//! * `policy validate [path]`  — lint a pack file (default `.lean-ctx/policy.toml`)
//! * `policy coverage [name]`  — automated partial CGB assessment (GL #426)

use std::path::{Path, PathBuf};

use crate::core::compliance;
use crate::core::policy::{self, PolicyPack, ResolvedPolicy, builtin, coverage};

/// Project-local pack location, relative to the working directory.
const PROJECT_PACK_PATH: &str = ".lean-ctx/policy.toml";

/// Entry point dispatched from `cli::dispatch`.
pub(crate) fn cmd_policy(args: &[String]) {
    match args.first().map(String::as_str) {
        Some("list") => cmd_list(),
        Some("show") => cmd_show(&args[1..]),
        Some("validate") => cmd_validate(&args[1..]),
        Some("coverage") => cmd_coverage(&args[1..]),
        Some("-h" | "--help") | None => print_help(),
        Some(other) => {
            eprintln!("policy: unknown subcommand '{other}'\n");
            print_help();
            std::process::exit(2);
        }
    }
}

fn print_help() {
    println!(
        "lean-ctx policy — context policy packs (governance presets as code)\n\n\
USAGE:\n\
  lean-ctx policy list                 List built-in packs (+ project pack)\n\
  lean-ctx policy show <name> [--toml] Show the resolved effective policy\n\
  lean-ctx policy validate [path]      Validate a pack file\n\
                                       (default: {PROJECT_PACK_PATH})\n\
  lean-ctx policy coverage [name]      Automated PARTIAL assessment against\n\
                                       the Context Governance Benchmark\n\
                                       [--benchmark cgb] [--json]\n\
  lean-ctx policy coverage --framework <eu-ai-act|iso42001|soc2> [pack]\n\
                                       Framework coverage report: mapping\n\
                                       matrix + live pack verification\n\
                                       (defaults to the reference pack)\n\n\
A pack pins governance expectations — default read mode, allowed/denied\n\
tools, redaction patterns, audit retention, context budget — in reviewable\n\
TOML with single inheritance (extends). Start from a built-in:\n\
  lean-ctx policy show baseline --toml > {PROJECT_PACK_PATH}\n\n\
Docs: docs/contracts/context-policy-packs-v1.md · docs/guides/policy-packs.md"
    );
}

// ── list ─────────────────────────────────────────────────────────────────────

fn cmd_list() {
    println!("Built-in policy packs:\n");
    for pack in builtin::all() {
        let extends = pack
            .extends
            .as_deref()
            .map(|p| format!(" (extends {p})"))
            .unwrap_or_default();
        println!("  {:<18} v{}{}", pack.name, pack.version, extends);
        println!("  {:<18} {}", "", pack.description);
    }
    match load_project_pack() {
        Some(Ok(pack)) => {
            println!("\nProject pack ({PROJECT_PACK_PATH}):\n");
            let extends = pack
                .extends
                .as_deref()
                .map(|p| format!(" (extends {p})"))
                .unwrap_or_default();
            println!("  {:<18} v{}{}", pack.name, pack.version, extends);
            println!("  {:<18} {}", "", pack.description);
        }
        Some(Err(e)) => {
            println!("\nProject pack ({PROJECT_PACK_PATH}): INVALID — {e}");
        }
        None => {
            println!("\nNo project pack. Create one from a built-in:");
            println!("  lean-ctx policy show baseline --toml > {PROJECT_PACK_PATH}");
        }
    }
}

/// The project pack, when `.lean-ctx/policy.toml` exists. `None` = no file.
fn load_project_pack() -> Option<Result<PolicyPack, policy::PolicyError>> {
    let path = PathBuf::from(PROJECT_PACK_PATH);
    path.exists().then(|| policy::parse_file(&path))
}

/// Resolve a pack argument — `project`, a `.toml` path or a built-in name —
/// exiting with a contextual message on failure (shared by show/coverage).
fn load_pack_arg(name: &str, ctx: &str) -> PolicyPack {
    let is_toml_path = Path::new(name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("toml"));
    if name == "project" || is_toml_path {
        let path = if name == "project" {
            PathBuf::from(PROJECT_PACK_PATH)
        } else {
            PathBuf::from(name)
        };
        match policy::parse_file(&path) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("{ctx}: {e}");
                std::process::exit(1);
            }
        }
    } else if let Some(p) = builtin::get(name) {
        p
    } else {
        eprintln!(
            "{ctx}: no pack named '{name}' (built-ins: {}; or pass a .toml path)",
            builtin::names().join(", ")
        );
        std::process::exit(1);
    }
}

// ── show ─────────────────────────────────────────────────────────────────────

fn cmd_show(args: &[String]) {
    let Some(name) = args.first().filter(|a| !a.starts_with('-')) else {
        eprintln!(
            "policy show: missing pack name (one of: {})",
            builtin::names().join(", ")
        );
        std::process::exit(2);
    };
    let as_toml = args.iter().any(|a| a == "--toml");

    let pack = load_pack_arg(name, "policy show");

    if as_toml {
        // Raw, copyable pack definition (not the resolved view) — the natural
        // starting point for an org-specific pack.
        match toml::to_string_pretty(&pack) {
            Ok(t) => print!("{t}"),
            Err(e) => {
                eprintln!("policy show: failed to render TOML: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    match policy::resolve(&pack) {
        Ok(resolved) => print_resolved(&resolved),
        Err(e) => {
            eprintln!("policy show: {e}");
            std::process::exit(1);
        }
    }
}

fn print_resolved(r: &ResolvedPolicy) {
    println!("{} v{} — {}", r.name, r.version, r.description);
    if !r.chain.is_empty() {
        println!("inherits: {}", r.chain.join(" -> "));
    }
    println!();
    println!(
        "  default_read_mode    {}",
        r.default_read_mode.as_deref().unwrap_or("(engine default)")
    );
    match &r.allow_tools {
        Some(allow) => println!("  allow_tools          {}", allow.join(", ")),
        None => println!("  allow_tools          (all tools allowed)"),
    }
    if r.deny_tools.is_empty() {
        println!("  deny_tools           (none)");
    } else {
        println!("  deny_tools           {}", r.deny_tools.join(", "));
    }
    println!(
        "  max_context_tokens   {}",
        r.max_context_tokens
            .map_or("(unbounded)".to_string(), |v| v.to_string())
    );
    println!(
        "  audit_retention_days {}",
        r.audit_retention_days
            .map_or("(unspecified)".to_string(), |v| v.to_string())
    );
    if r.redaction.is_empty() {
        println!("  redaction            (none)");
    } else {
        println!("  redaction            {} patterns:", r.redaction.len());
        for (name, pattern) in &r.redaction {
            println!("    {name:<22} {pattern}");
        }
    }
}

// ── validate ─────────────────────────────────────────────────────────────────

fn cmd_validate(args: &[String]) {
    let path = args
        .first()
        .map_or_else(|| PathBuf::from(PROJECT_PACK_PATH), PathBuf::from);
    if !Path::new(&path).exists() {
        eprintln!("policy validate: {} not found", path.display());
        std::process::exit(1);
    }
    match policy::parse_file(&path).and_then(|p| policy::resolve(&p)) {
        Ok(resolved) => {
            println!(
                "OK — {} v{} validates and resolves ({} redaction patterns, {} denied tools)",
                resolved.name,
                resolved.version,
                resolved.redaction.len(),
                resolved.deny_tools.len()
            );
        }
        Err(e) => {
            eprintln!("INVALID — {e}");
            std::process::exit(1);
        }
    }
}

// ── coverage ─────────────────────────────────────────────────────────────────

/// `policy coverage [name|path|project] [--benchmark cgb] [--json]` —
/// automated PARTIAL assessment of a pack against the CGB spec. Prints
/// per-check evidence and an honesty line; never a maturity grade.
fn cmd_coverage(args: &[String]) {
    if let Some(pos) = args.iter().position(|a| a == "--benchmark") {
        let bench = args.get(pos + 1).map(String::as_str);
        if bench != Some("cgb") {
            eprintln!(
                "policy coverage: unknown benchmark '{}' (supported: cgb)",
                bench.unwrap_or("")
            );
            std::process::exit(2);
        }
    }
    let framework = args.iter().position(|a| a == "--framework").map(|pos| {
        match args.get(pos + 1).map(String::as_str) {
            Some(name) if compliance::get(name).is_some() => name.to_string(),
            other => {
                eprintln!(
                    "policy coverage: unknown framework '{}' (supported: {})",
                    other.unwrap_or(""),
                    compliance::names().join(", ")
                );
                std::process::exit(2);
            }
        }
    });
    let as_json = args.iter().any(|a| a == "--json");
    // First positional = pack; skip flags and their values.
    let mut name = None;
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "--benchmark" || arg == "--framework" {
            skip_next = true;
        } else if !arg.starts_with("--") {
            name = Some(arg.clone());
            break;
        }
    }

    // Framework mode: pack optional (defaults to the mapping's reference
    // pack so the report is the audit-conversation artifact out of the box).
    if let Some(fw) = framework {
        let mapping = compliance::get(&fw).expect("validated above");
        let pack_arg = name.unwrap_or_else(|| mapping.reference_pack.clone());
        let pack = load_pack_arg(&pack_arg, "policy coverage");
        let resolved = match policy::resolve(&pack) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("policy coverage: {e}");
                std::process::exit(1);
            }
        };
        render_framework_report(mapping, &resolved, as_json);
        return;
    }

    let pack_arg = name.unwrap_or_else(|| {
        if Path::new(PROJECT_PACK_PATH).exists() {
            "project".to_string()
        } else {
            eprintln!(
                "policy coverage: no pack given and no {PROJECT_PACK_PATH} — pass a built-in ({}) or a .toml path",
                builtin::names().join(", ")
            );
            std::process::exit(2);
        }
    });

    let pack = load_pack_arg(&pack_arg, "policy coverage");
    let resolved = match policy::resolve(&pack) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("policy coverage: {e}");
            std::process::exit(1);
        }
    };

    let checks = coverage::assess(&resolved);
    let summary = coverage::summarize(&checks);

    if as_json {
        let doc = serde_json::json!({
            "benchmark": coverage::BENCHMARK_ID,
            "pack": { "name": resolved.name, "version": resolved.version, "chain": resolved.chain },
            "checks": checks,
            "summary": summary,
            "disclaimer": "automated partial assessment — full grading requires the manual CGB assessment",
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&doc).expect("serializable")
        );
        return;
    }

    println!(
        "CGB coverage — automated PARTIAL assessment ({})",
        coverage::BENCHMARK_ID
    );
    println!(
        "pack: {} v{}{}\n",
        resolved.name,
        resolved.version,
        if resolved.chain.is_empty() {
            String::new()
        } else {
            format!(" (inherits {})", resolved.chain.join(" -> "))
        }
    );
    for check in &checks {
        let status = match check.status {
            coverage::CheckStatus::Pass => "PASS        ",
            coverage::CheckStatus::Fail => "FAIL        ",
            coverage::CheckStatus::Inconclusive => "INCONCLUSIVE",
        };
        println!(
            "  {:<8} {:<28} {}  {}",
            check.control, check.title, status, check.detail
        );
    }
    println!(
        "\n{} pass · {} fail · {} inconclusive — touches {} of {} CGB controls.",
        summary.pass,
        summary.fail,
        summary.inconclusive,
        summary.controls_covered,
        summary.controls_total
    );
    println!(
        "This is partial evidence, NOT a grade. Full assessment: spec assessment/TEMPLATE.md\n\
(LeanCTX's own: docs/compliance/cgb-self-assessment.md)."
    );
    if summary.fail > 0 {
        std::process::exit(1);
    }
}

/// `policy coverage --framework <id> [pack]` — the audit-conversation
/// artifact (GL #424): mapping matrix + live pack verification.
fn render_framework_report(
    mapping: &compliance::FrameworkMapping,
    resolved: &ResolvedPolicy,
    as_json: bool,
) {
    let report = compliance::report(mapping, Some(resolved));

    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("serializable")
        );
        if report.summary.not_enforced > 0 {
            std::process::exit(1);
        }
        return;
    }

    println!("{} — coverage report", report.title);
    println!(
        "framework pin: {} (pinned {}, semi-annual review)\npack: {}\n",
        report.version_pin,
        report.pinned_on,
        report.pack.as_deref().unwrap_or("-")
    );
    for row in &report.rows {
        let status = match row.status {
            compliance::RowStatus::Enforced => "ENFORCED    ",
            compliance::RowStatus::EngineGuarantee => "ENGINE      ",
            compliance::RowStatus::NotEnforced => "NOT-ENFORCED",
            compliance::RowStatus::NotVerified => "NOT-VERIFIED",
            compliance::RowStatus::Gap => "GAP         ",
        };
        println!(
            "  {:<18} {:<14} {}  {}",
            row.id, row.clause, status, row.detail
        );
    }
    let s = &report.summary;
    println!(
        "\n{} of {} controls technically enforced ({} pack-verified, {} engine guarantees) · {} documented gaps{}",
        s.enforced + s.engine_guarantee,
        s.controls_total,
        s.enforced,
        s.engine_guarantee,
        s.gaps,
        if s.not_enforced > 0 {
            format!(" · {} NOT enforced by this pack", s.not_enforced)
        } else {
            String::new()
        }
    );
    println!("\n{}", report.disclaimer);
    if s.not_enforced > 0 {
        std::process::exit(1);
    }
}
