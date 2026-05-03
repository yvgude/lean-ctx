use std::path::Path;

use crate::core::handoff_ledger::HandoffLedgerV1;

pub fn format_created(path: &Path, ledger: &HandoffLedgerV1) -> String {
    let wf = ledger.workflow.as_ref().map_or_else(
        || "none".to_string(),
        |w| format!("{}@{}", w.spec.name, w.current),
    );
    format!(
        "ctx_handoff create\n path: {}\n md5: {}\n manifest_md5: {}\n workflow: {}\n evidence_keys: {}\n curated_refs: {}\n knowledge_facts: {}",
        path.display(),
        ledger.content_md5,
        ledger.manifest_md5,
        wf,
        ledger.evidence_keys.len(),
        ledger.curated_refs.len(),
        ledger.knowledge.facts.len()
    )
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
