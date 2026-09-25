//! `lean-ctx prove`: auditable E2E decision-loop evidence.

use std::{fs, path::Path};

use crate::core::integration_proof::{ProofResult, TaskProof, prove_decision_loop};

#[rustfmt::skip]
pub(crate) fn cmd_prove(args: &[String]) {
    if args.first().is_some_and(|a| a == "speed") {
        crate::cli::prove_speed::cmd_prove_speed(&args[1..]);
        return;
    }
    if args.iter().any(|arg| matches!(arg.as_str(), "-h" | "--help")) {
        usage();
        return;
    }
    let Some((format, output)) = parse(args) else {
        eprintln!("prove: expected --format table|json|markdown and an optional --output FILE");
        usage();
        std::process::exit(2);
    };
    let Some(report) = render(&prove_decision_loop(), format) else {
        unreachable!("validated proof format");
    };
    if let Some(path) = output && let Err(error) = fs::write(Path::new(path), &report) {
        eprintln!("prove: cannot write {path}: {error}");
        std::process::exit(1);
    }
    print!("{report}");
}

fn parse(args: &[String]) -> Option<(&str, Option<&str>)> {
    let mut format = "table";
    let mut output = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--format" => {
                format = args.get(index + 1)?.as_str();
                index += 2;
            }
            "--output" => {
                output = Some(args.get(index + 1)?.as_str());
                index += 2;
            }
            _ => return None,
        }
    }
    matches!(format, "table" | "json" | "markdown").then_some((format, output))
}

fn usage() {
    println!(
        "Generate auditable decision-loop evidence.\n\nUsage: lean-ctx prove [--format <table|json|markdown>] [--output FILE]\n       lean-ctx prove speed --suite FILE [--runs N]   (signed A/B latency proof; see `prove speed --help`)\n\nExamples:\n  lean-ctx prove\n  lean-ctx prove --format markdown --output proof.md\n  lean-ctx prove --format json"
    );
}

pub(crate) fn render(proof: &ProofResult, format: &str) -> Option<String> {
    match format {
        "json" => serde_json::to_string_pretty(proof)
            .ok()
            .map(|json| format!("{json}\n")),
        "table" => Some(human(proof, false)),
        "markdown" => Some(human(proof, true)),
        _ => None,
    }
}

#[rustfmt::skip]
fn human(proof: &ProofResult, markdown: bool) -> String {
    let mut out = if markdown {
        format!("# LeanCTX Decision Loop Proof\n\n{}\n\n| Task ID | Query | Intent | Complexity | References | Bundle candidates | Cost | Outcome | CPAO |\n|---|---|---|---|---:|---:|---:|---|---:|\n", summary(proof))
    } else {
        format!("LeanCTX Decision Loop Proof\n{}\n\n{:<12} {:<40} {:<12} {:<10} {:>4} {:>7} {:>8} {:<9} {:>8}\n{}\n", summary(proof), "TASK ID", "QUERY", "INTENT", "COMPLEXITY", "REFS", "BUNDLES", "COST", "OUTCOME", "CPAO", "-".repeat(132))
    };
    for task in &proof.tasks {
        out.push_str(&row(task, markdown));
    }
    out.push_str(&format!("\nEvidence chain: {} ({} stage records across {} tasks)\nBinary: lean-ctx v{}\nGold Set: {} tasks available for validation\n", if proof.evidence_chain_complete { "COMPLETE" } else { "INCOMPLETE" }, proof.evidence_ledger.items.len(), proof.tasks.len(), env!("CARGO_PKG_VERSION"), proof.tasks.len()));
    out
}

#[rustfmt::skip]
fn row(task: &TaskProof, markdown: bool) -> String {
    let (id, query, intent, complexity) = (short(&task.task_id, 12), short(&task.query, 40), short(&task.profile_intent, 12), short(&task.profile_complexity, 10));
    let (refs, bundles, cost, outcome, cpao) = (task.references_found.len(), task.bundle_candidates, task.cost_micros, if task.outcome_accepted { "accepted" } else { "rejected" }, cpao(task.cpao_micros));
    if markdown {
        format!("| {id} | {query} | {intent} | {complexity} | {refs} | {bundles} | {cost} | {outcome} | {cpao} |\n")
    } else {
        format!("{id:<12} {query:<40} {intent:<12} {complexity:<10} {refs:>4} {bundles:>7} {cost:>8} {outcome:<9} {cpao:>8}\n")
    }
}

#[rustfmt::skip]
fn summary(proof: &ProofResult) -> String { let accepted = proof.tasks.iter().filter(|task| task.outcome_accepted).count(); format!("Tasks: {} total, {accepted} accepted | CPAO: {} | evidence_chain_complete: {}", proof.tasks.len(), cpao(proof.aggregate_cpao_micros), proof.evidence_chain_complete) }

fn cpao(value: Option<u64>) -> String {
    value.map_or_else(|| "-".into(), |value| value.to_string())
}
fn short(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.into()
    } else {
        format!(
            "{}…",
            value
                .chars()
                .take(max.saturating_sub(1))
                .collect::<String>()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn parse_rejects_missing_values_and_unknown_options() {
        assert!(parse(&["--format".into()]).is_none());
        assert!(parse(&["--nope".into()]).is_none());
    }
}
