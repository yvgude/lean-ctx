//! `lean-ctx doctor lint-context` (#960) — surfaces the injected-context linter.
//!
//! Reports low-signal / duplicate lines in lean-ctx's own injected context (rules
//! block + advertised tool descriptions). Exits non-zero when an Error-level
//! finding is present, so it doubles as a CI gate alongside the `cargo test` guard.

use crate::core::context_lint::{Severity, error_count, lint_injected_context};

const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const GREEN: &str = "\x1b[32m";
const RST: &str = "\x1b[0m";

pub(super) fn run_lint_context(json: bool) -> i32 {
    let findings = lint_injected_context();
    let errors = error_count(&findings);

    if json {
        let rows: Vec<_> = findings
            .iter()
            .map(|f| {
                serde_json::json!({
                    "severity": match f.severity { Severity::Error => "error", Severity::Warn => "warn" },
                    "kind": format!("{:?}", f.kind),
                    "source": f.source,
                    "detail": f.detail,
                })
            })
            .collect();
        let out = serde_json::json!({ "findings": rows, "error_count": errors });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return i32::from(errors > 0);
    }

    println!("{BOLD}Injected-context lint{RST}");
    println!(
        "{DIM}Every injected line must earn its tokens: when/why + the non-obvious gotcha.{RST}\n"
    );

    if findings.is_empty() {
        println!("  {GREEN}✓ no findings — injected context is high-signal{RST}");
        return 0;
    }

    for f in &findings {
        let (tag, color) = match f.severity {
            Severity::Error => ("ERROR", RED),
            Severity::Warn => (" WARN", YELLOW),
        };
        println!("  {color}{tag}{RST} {DIM}{}{RST}  {}", f.source, f.detail);
    }
    println!();
    if errors > 0 {
        println!("  {RED}{errors} error finding(s) — these gate CI{RST}");
    } else {
        println!(
            "  {GREEN}✓ no gating errors{RST} {DIM}({} warning(s)){RST}",
            findings.len()
        );
    }
    i32::from(errors > 0)
}
