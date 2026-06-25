use std::path::Path;

use crate::core::handoff_ledger::HandoffLedgerV1;

#[must_use]
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

#[must_use]
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

#[must_use]
pub fn format_show(path: &Path, ledger: &HandoffLedgerV1) -> String {
    let mut out = serde_json::to_string_pretty(ledger).unwrap_or_else(|_| "{}".to_string());
    out.push('\n');
    format!("ctx_handoff show\n path: {}\n{}", path.display(), out)
}

#[must_use]
pub fn format_clear(removed: u32) -> String {
    format!("ctx_handoff clear\n removed: {removed}")
}

#[must_use]
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

#[must_use]
pub fn format_imported(
    path: &Path,
    schema_version: u32,
    imported_knowledge: u32,
    contradictions: u32,
    warning: Option<&str>,
    signature_line: &str,
) -> String {
    let mut out = format!(
        "ctx_handoff import\n path: {path}\n schema_version: {schema_version}\n imported_knowledge: {imported_knowledge}\n contradictions: {contradictions}",
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
