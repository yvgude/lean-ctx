// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx inspect`: why the Context Gateway delivered, changed or withheld
//! what it did in the latest rounds of this project. Reads only stored,
//! digest-verified Decision Receipts — never content, never a guess.

use std::io::Write;

use lean_ctx_protocol::context_gateway::{
    ContextDecisionReceiptV1, ContextDecisionV1, CoverageKindV1, DetectorSignalV1,
};

use crate::core::context_admission::receipt_store::{self, LoadError, StoredReceipt};

const DEFAULT_LAST: usize = 1;
const MAX_LAST: usize = receipt_store::LATEST_CAPACITY;

fn usage() {
    println!(
        "Why the Context Gateway delivered, changed or withheld what it did.\n\n\
         Shows the latest Decision Receipts of this project (newest first), each\n\
         verified against the digest it is stored under. Receipts hold decisions,\n\
         counts and digests only — never content.\n\n\
         Usage: lean-ctx inspect [--last <N>] [--project <DIR> | --proxy] [--json]\n\
         \x20      lean-ctx inspect --task <TASK_ID> [--project-id <ID>] [--tenant-id <ID>] [--json]\n\n\
         Options:\n  \
           --last <N>       show the N newest receipts (default 1, max {MAX_LAST})\n  \
           --project <DIR>  project root (default: the current project)\n  \
           --proxy          requests the BYOK proxy forwarded to a model provider\n  \
           --task <ID>      one task's lineage: plan, deliveries, cost, outcome\n  \
           --project-id <ID> the task's project (default: the current project root)\n  \
           --tenant-id <ID> the task's tenant, for organization-admitted tasks\n  \
           --json           machine-readable output\n"
    );
}

struct Options {
    last: usize,
    project: Option<String>,
    task: Option<String>,
    project_id: Option<lean_ctx_protocol::ProjectId>,
    tenant_id: Option<lean_ctx_protocol::TenantId>,
    json: bool,
}

fn parse(args: &[String]) -> Result<Option<Options>, String> {
    let mut options = Options {
        last: DEFAULT_LAST,
        project: None,
        task: None,
        project_id: None,
        tenant_id: None,
        json: false,
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--json" => options.json = true,
            "--last" => {
                let value = args.next().ok_or("--last needs a number")?;
                options.last = value
                    .parse::<usize>()
                    .ok()
                    .filter(|n| (1..=MAX_LAST).contains(n))
                    .ok_or(format!("--last must be between 1 and {MAX_LAST}"))?;
            }
            "--project" => {
                options.project = Some(args.next().ok_or("--project needs a directory")?.clone());
            }
            "--proxy" => {
                options.project =
                    Some(crate::core::context_admission::egress::PROXY_RECEIPT_KEY.to_owned());
            }
            "--task" => {
                let value = args.next().ok_or("--task needs a task id")?;
                lean_ctx_protocol::TaskId::new(value.clone())
                    .map_err(|_| "--task is not a valid task id".to_owned())?;
                options.task = Some(value.clone());
            }
            "--project-id" => {
                let value = args.next().ok_or("--project-id needs an id")?;
                options.project_id = Some(
                    lean_ctx_protocol::ProjectId::new(value.clone())
                        .map_err(|_| "--project-id is not a valid project id".to_owned())?,
                );
            }
            "--tenant-id" => {
                let value = args.next().ok_or("--tenant-id needs an id")?;
                options.tenant_id = Some(
                    lean_ctx_protocol::TenantId::new(value.clone())
                        .map_err(|_| "--tenant-id is not a valid tenant id".to_owned())?,
                );
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if options.task.is_none() && (options.project_id.is_some() || options.tenant_id.is_some()) {
        return Err("--project-id and --tenant-id only apply with --task".to_owned());
    }
    Ok(Some(options))
}

fn current_project_root() -> String {
    crate::core::config::Config::find_project_root()
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|dir| dir.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| ".".to_owned())
}

pub(crate) fn cmd_inspect(args: &[String]) {
    let options = match parse(args) {
        Ok(Some(options)) => options,
        Ok(None) => {
            usage();
            return;
        }
        Err(error) => {
            eprintln!("lean-ctx inspect: {error}");
            usage();
            std::process::exit(2);
        }
    };
    if let Some(task) = &options.task {
        // MCP tasks carry the session's project root as their project id.
        let Some(project) = options
            .project_id
            .clone()
            .or_else(|| lean_ctx_protocol::ProjectId::new(current_project_root()).ok())
        else {
            eprintln!("lean-ctx inspect: specify --project-id for this task");
            std::process::exit(2);
        };
        let scope = crate::core::context_store::task_scope(options.tenant_id.as_ref(), &project);
        let lineage = crate::core::context_store::lineage::load(&scope, task);
        let mut out = std::io::stdout().lock();
        let _ = if options.json {
            writeln!(
                out,
                "{}",
                serde_json::to_string_pretty(&lineage).unwrap_or_else(|_| "{}".to_owned())
            )
        } else {
            write!(out, "{}", render_lineage(&lineage))
        };
        return;
    }
    let project = options.project.clone().unwrap_or_else(current_project_root);
    let receipts = receipt_store::latest(&project, options.last);
    let mut out = std::io::stdout().lock();
    let _ = if options.json {
        writeln!(out, "{}", render_json(&receipts))
    } else {
        write!(
            out,
            "{}{}",
            coverage_header(),
            render_human(&project, &receipts)
        )
    };
}

/// One line on what the gateway can see on this machine, so a receipt is
/// never read as covering more than it does (G7).
fn coverage_header() -> String {
    let Some(home) = dirs::home_dir() else {
        return String::new();
    };
    let coverage = crate::doctor::configured_host_coverage(&home);
    if coverage.is_empty() {
        return String::new();
    }
    let hosts = coverage
        .iter()
        .map(|host| format!("{} {}", host.host, host.level.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    format!("Gateway coverage on this machine: {hosts} (details: lean-ctx doctor)\n\n")
}

fn render_json(receipts: &[(String, Result<StoredReceipt, LoadError>)]) -> String {
    let items: Vec<serde_json::Value> = receipts
        .iter()
        .map(|(hex, loaded)| match loaded {
            Ok(stored) => serde_json::json!({
                "digest": stored.digest.as_str(),
                "verified": true,
                "receipt": stored.receipt,
            }),
            Err(error) => serde_json::json!({
                "digest": format!("sha256:{hex}"),
                "verified": false,
                "error": load_error(error),
            }),
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::json!({ "receipts": items }))
        .unwrap_or_else(|_| "{}".to_owned())
}

fn load_error(error: &LoadError) -> &'static str {
    match error {
        LoadError::Missing => "missing",
        LoadError::Unreadable => "unreadable",
        LoadError::Tampered => "tampered",
    }
}

fn short(hex: &str) -> &str {
    &hex[..hex.len().min(12)]
}

pub(crate) fn render_human(
    project: &str,
    receipts: &[(String, Result<StoredReceipt, LoadError>)],
) -> String {
    if receipts.is_empty() {
        return format!(
            "No Context Gateway receipts for {project} yet.\n\
             Receipts are written when an agent reads sources through lean-ctx.\n"
        );
    }
    let mut text = String::new();
    for (index, (hex, loaded)) in receipts.iter().enumerate() {
        if index > 0 {
            text.push('\n');
        }
        match loaded {
            Ok(stored) => render_receipt(&mut text, short(stored.digest.hex()), &stored.receipt),
            Err(error) => text.push_str(&format!(
                "Receipt {} — NOT VERIFIED ({}): not shown\n",
                short(hex),
                load_error(error)
            )),
        }
    }
    text
}

pub(crate) fn render_lineage(
    lineage: &crate::core::context_store::lineage::TaskLineageV1,
) -> String {
    let mut text = format!("Task {} — outcome {}\n", lineage.task_id, lineage.outcome);
    if let Some(scope) = &lineage.scope {
        text.push_str(&format!("  scope      {scope}\n"));
    }
    if let Some(error) = &lineage.ledger_error {
        text.push_str(&format!(
            "  ledger     NOT VERIFIED ({error}): steps not shown\n"
        ));
    }
    for step in &lineage.steps {
        let fields: Vec<String> = step
            .fields
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        text.push_str(&format!(
            "  #{:<4} {:<26} {}\n",
            step.sequence,
            step.kind,
            fields.join(" ")
        ));
    }
    for delivery in &lineage.deliveries {
        let hex = delivery.digest.trim_start_matches("sha256:");
        match &delivery.summary {
            Some(s) => text.push_str(&format!(
                "  delivery {} (verified) {} to {} · {} inspected · {} delivered · {} withheld · \
                 {} redacted · {} → {} tokens\n",
                short(hex),
                s.outcome,
                s.destination,
                s.inspected,
                s.delivered,
                s.withheld,
                s.redactions,
                s.tokens_original,
                s.tokens_delivered,
            )),
            None => text.push_str(&format!(
                "  delivery {} — NOT VERIFIED ({}): not shown\n",
                short(hex),
                delivery.error.unwrap_or("unknown")
            )),
        }
    }
    if lineage.gaps.is_empty() {
        text.push_str("  chain      complete: plan → delivery → outcome\n");
    } else {
        text.push_str(&format!("  gaps       {}\n", lineage.gaps.join(", ")));
    }
    text
}

fn serde_name<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn render_receipt(text: &mut String, digest: &str, receipt: &ContextDecisionReceiptV1) {
    let s = receipt.sources;
    let sec = receipt.security;
    let policy = receipt
        .policy
        .as_ref()
        .map_or_else(|| "none".to_owned(), |p| short(p.digest.hex()).to_owned());
    let destination = &receipt.destination;
    let target = destination.model.as_ref().map_or_else(
        || destination.provider.as_str().to_owned(),
        |model| format!("{} / {}", destination.provider.as_str(), model.as_str()),
    );
    text.push_str(&format!(
        "Context Gateway receipt {digest} (verified)\n\
         \x20 outcome    {} · mode {} · policy {policy}\n\
         \x20 to         {target} ({})\n\
         \x20 objects    {} inspected · {} delivered · {} withheld\n\
         \x20 security   {} value(s) redacted · {} injection signal(s) · {} not fully inspected\n\
         \x20 tokens     {} original → {} delivered\n",
        serde_name(&receipt.outcome),
        serde_name(&receipt.mode),
        serde_name(&destination.locality),
        s.inspected,
        s.selected,
        s.blocked,
        sec.redactions,
        sec.injection_signals,
        sec.incomplete_coverage,
        receipt.tokens.original,
        receipt.tokens.delivered,
    ));
    if let Some(metric) = crate::core::context_admission::hud::compact_metric(receipt) {
        text.push_str(&format!("  hud        {metric}\n"));
    }
    let itemized = receipt.decisions.len();
    if (s.inspected as usize) > itemized {
        text.push_str(&format!(
            "  ({itemized} decisions itemized, notable ones first; all {} are counted above)\n",
            s.inspected
        ));
    }
    for (index, decision) in receipt.decisions.iter().enumerate() {
        render_decision(text, index + 1, decision);
    }
}

fn render_decision(text: &mut String, number: usize, decision: &ContextDecisionV1) {
    text.push_str(&format!(
        "  {number}. {} · object {}\n",
        serde_name(&decision.disposition),
        short(decision.object.hex())
    ));
    if decision.reason_codes.is_empty() {
        text.push_str("       inspected completely, nothing found\n");
    }
    for reason in &decision.reason_codes {
        text.push_str(&format!(
            "       {} — {}\n",
            reason.as_str(),
            explain(reason.as_str())
        ));
    }
    let detectors: Vec<String> = decision.signals.iter().filter_map(detector_note).collect();
    if !detectors.is_empty() {
        text.push_str(&format!("       detectors: {}\n", detectors.join(", ")));
    }
}

/// Only what is worth a reader's attention: findings and gaps.
fn detector_note(signal: &DetectorSignalV1) -> Option<String> {
    let name = signal
        .detector
        .id
        .as_str()
        .trim_start_matches("builtin.")
        .to_owned();
    let coverage = match signal.coverage.kind {
        CoverageKindV1::Complete | CoverageKindV1::NotRequired => None,
        other => Some(format!(
            "{} ({} of {} bytes)",
            serde_name(&other),
            signal.coverage.bytes_inspected,
            signal.coverage.bytes_total
        )),
    };
    match (signal.evidence_count, coverage) {
        (0, None) => None,
        (0, Some(coverage)) => Some(format!("{name} {coverage}")),
        (n, None) => Some(format!("{name} {n} found")),
        (n, Some(coverage)) => Some(format!("{name} {n} found, {coverage}")),
    }
}

/// A plain sentence for each reason code the gateway emits.
pub(crate) fn explain(code: &str) -> String {
    let fixed = match code {
        "secret.redacted" => "credentials were masked before caching and compression",
        "secret.flagged" => "credentials were detected and delivered (secrets = \"warn\")",
        "secret.blocked" => "withheld: credentials found and secrets = \"block\"",
        "pii.redacted" => "checksum-valid personal data was masked",
        "pii.flagged" => "personal data was detected and delivered (pii = \"warn\")",
        "pii.blocked" => "withheld: personal data found and pii = \"block\"",
        "injection.flagged" => "prompt-injection pattern found; delivered and marked untrusted",
        "injection.redacted" => "the lines carrying a prompt-injection pattern were replaced",
        "injection.blocked" => "withheld: prompt-injection pattern and injection = \"block\"",
        "classification.flagged" => "a classification marking raised the sensitivity",
        "classification.blocked" => {
            "withheld: classification marking and classification = \"block\""
        }
        "coverage.partial" => "a detector inspected only part of the source (inspection budget)",
        "coverage.budget_exhausted" => "the inspection budget ended before the source did",
        "coverage.unsupported_media" => "no detector can read this medium (e.g. an image)",
        "detector.timed_out" => "a detector ran out of time; the source is not proven clean",
        "detector.invalid_pattern" => {
            "a configured custom pattern is invalid; that detector failed"
        }
        "redaction.incomplete" => "withheld: masking left a detectable value behind",
        "semantic.unavailable" => "the licensed semantic detector was unavailable (advisory)",
        "semantic.partial" => "the semantic detector inspected only part of the source (advisory)",
        "gateway.disabled" => "the Context Gateway is switched off",
        "destination.remote_restricted" => {
            "withheld: restricted content never goes to a remote model"
        }
        "egress.sealed_content" => {
            "the provider signed or encrypted this content, so it could not be masked"
        }
        "egress.opaque_payload" => "the proxy could not read this request body (opaque)",
        _ => "",
    };
    if !fixed.is_empty() {
        return fixed.to_owned();
    }
    if let Some(rest) = code.strip_prefix("semantic.") {
        return format!("semantic detector: {}", rest.replace(['.', '_'], " "));
    }
    "see docs/contracts/context-gateway-v1.md".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reason_the_gateway_emits_has_a_sentence() {
        for code in [
            "secret.redacted",
            "secret.flagged",
            "secret.blocked",
            "pii.redacted",
            "pii.flagged",
            "pii.blocked",
            "injection.flagged",
            "injection.redacted",
            "injection.blocked",
            "classification.flagged",
            "classification.blocked",
            "coverage.partial",
            "coverage.budget_exhausted",
            "coverage.unsupported_media",
            "detector.timed_out",
            "detector.invalid_pattern",
            "redaction.incomplete",
            "semantic.unavailable",
            "semantic.partial",
            "gateway.disabled",
            "destination.remote_restricted",
            "egress.sealed_content",
            "egress.opaque_payload",
        ] {
            assert!(
                !explain(code).starts_with("see docs"),
                "{code} needs an explanation"
            );
        }
        assert_eq!(
            explain("semantic.prompt_injection.flagged"),
            "semantic detector: prompt injection flagged"
        );
    }

    #[test]
    fn arguments_are_validated() {
        assert!(parse(&["--last".into(), "0".into()]).is_err());
        assert!(parse(&["--bogus".into()]).is_err());
        assert!(matches!(parse(&["--help".into()]), Ok(None)));
        let options = parse(&["--last".into(), "3".into(), "--json".into()])
            .expect("valid arguments")
            .expect("not help");
        assert_eq!((options.last, options.json), (3, true));
        assert!(parse(&["--task".into()]).is_err());
        assert!(parse(&["--task".into(), String::new()]).is_err());
        let options = parse(&["--task".into(), "task-1".into()])
            .expect("valid arguments")
            .expect("not help");
        assert_eq!(options.task.as_deref(), Some("task-1"));
        let options = parse(&[
            "--task".into(),
            "task-1".into(),
            "--project-id".into(),
            "project-a".into(),
        ])
        .expect("valid arguments")
        .expect("not help");
        assert_eq!(
            options
                .project_id
                .map(|project| project.as_str().to_owned()),
            Some("project-a".to_owned())
        );
        assert!(
            parse(&["--project-id".into(), "project-a".into()]).is_err(),
            "a scope without a task"
        );
    }

    #[test]
    fn a_lineage_names_its_gaps() {
        let lineage = crate::core::context_store::lineage::build(
            "task-1",
            Ok(Vec::new()),
            crate::core::context_store::lineage::LedgerScope::Verified,
            &crate::core::context_admission::receipt_store::TaskReceipts {
                entries: Vec::new(),
                index_complete: true,
            },
        );
        let text = render_lineage(&lineage);
        assert!(text.starts_with("Task task-1 — outcome unknown"), "{text}");
        assert!(text.contains("gaps       no_plan_recorded"), "{text}");
    }

    #[test]
    fn an_empty_project_says_so() {
        assert!(render_human("/p", &[]).contains("No Context Gateway receipts"));
    }

    #[test]
    fn unverifiable_receipts_are_named_not_shown() {
        let text = render_human("/p", &[("ab".repeat(32), Err(LoadError::Tampered))]);
        assert!(text.contains("NOT VERIFIED (tampered)"), "{text}");
    }
}
