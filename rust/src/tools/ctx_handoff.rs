use std::path::Path;

use crate::core::handoff_ledger::HandoffLedgerV1;

pub fn format_created(path: &Path, ledger: &HandoffLedgerV1) -> String {
    let wf = ledger.workflow.as_ref().map_or_else(
        || "none".to_string(),
        |w| format!("{}@{}", w.spec.name, w.current),
    );
    format!(
        // #1592: both digests are BLAKE3. The on-disk ledger keeps its historical
        // `*_md5` field names; what a reader sees is labelled for what it is.
        "ctx_handoff create\n path: {}\n blake3: {}\n manifest_blake3: {}\n workflow: {}\n evidence_keys: {}\n curated_refs: {}\n knowledge_facts: {}",
        path.display(),
        ledger.content_md5,
        ledger.manifest_md5,
        wf,
        ledger.evidence_keys.len(),
        ledger.curated_refs.len(),
        ledger.knowledge.facts.len()
    )
}

/// The reply of `ctx_handoff merge`: digests and counts only, in a fixed
/// order, so the same merge always answers with the same text (#498).
pub(crate) fn format_merged(outcome: &crate::core::handoff_ledger::MergeOutcome) -> String {
    let ledger = &outcome.ledger;
    let dropped = &outcome.dropped;
    let lines = [
        "ctx_handoff merge".to_string(),
        format!(" path: {}", outcome.path.display()),
        format!(" blake3: {}", ledger.content_md5),
        format!(" inputs: {}", ledger.merged_from.join(", ")),
        format!(
            " findings: {} (beyond bound: {})",
            ledger.session.findings.len(),
            dropped.findings
        ),
        format!(
            " knowledge_facts: {} (beyond bound: {})",
            ledger.knowledge.facts.len(),
            dropped.facts
        ),
        format!(
            " curated_refs: {} (beyond bound: {}, without source: {})",
            ledger.curated_refs.len(),
            dropped.curated_refs,
            dropped.unbound_curated
        ),
        format!(
            " decisions: {}, next_steps: {}",
            ledger.session.decisions.len(),
            ledger.session.next_steps.len()
        ),
        format!(" distinct_tasks: {}", outcome.distinct_tasks),
        // Counts only: which keys conflict is ledger text, served by `show`.
        format!(
            " fact_conflicts: {} keys, {} values kept",
            outcome.conflicts.len(),
            outcome
                .conflicts
                .iter()
                .map(|conflict| conflict.values)
                .sum::<usize>()
        ),
    ];
    lines.join("\n")
}

pub fn format_list(items: &[std::path::PathBuf]) -> String {
    if items.is_empty() {
        return "No handoff ledgers found.".to_string();
    }
    let mut lines = vec![format!("Handoff Ledgers ({}):", items.len())];
    for (i, p) in items.iter().take(20).enumerate() {
        lines.push(format!("  {}. {}", i + 1, p.display()));
    }
    lines.join("\n")
}

pub fn format_show(path: &Path, ledger: &HandoffLedgerV1) -> String {
    let mut out = serde_json::to_string_pretty(ledger).unwrap_or_else(|_| "{}".to_string());
    out.push('\n');
    format!("ctx_handoff show\n path: {}\n{}", path.display(), out)
}

pub fn format_clear(removed: u32) -> String {
    format!("ctx_handoff clear\n removed: {removed}")
}

pub fn format_exported(
    path: Option<&Path>,
    schema_version: u32,
    bytes: usize,
    privacy: &str,
) -> String {
    let mut out = format!(
        "ctx_handoff export\n schema_version: {schema_version}\n privacy: {privacy}\n bytes: {bytes}",
    );
    if let Some(p) = path {
        out.push_str(&format!("\n path: {}", p.display()));
    }
    out
}

pub fn format_imported(
    path: &Path,
    schema_version: u32,
    imported_knowledge: u32,
    contradictions: u32,
    skipped_withheld: usize,
    warning: Option<&str>,
    signature_line: &str,
) -> String {
    // `skipped_withheld`: texts of the bundle that store admission did not
    // let in. A count only — which text, and why, is not reported.
    let mut out = format!(
        "ctx_handoff import\n path: {path}\n schema_version: {schema_version}\n imported_knowledge: {imported_knowledge}\n contradictions: {contradictions}\n skipped_withheld: {skipped_withheld}",
        path = path.display(),
    );
    if !signature_line.is_empty() {
        out.push('\n');
        out.push_str(signature_line);
    }
    if let Some(w) = warning {
        out.push_str(&format!("\n {w}"));
    }
    out
}
