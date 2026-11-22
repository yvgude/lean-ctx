// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx cloud exchange audit [--json] [--limit N]` (contract §40, kind
//! `cloud.exchange.audit`): the audit records of the agent exchange through
//! LeanCTX Cloud on this machine, newest first. Each record names the
//! direction, outcome, message type, vault address, ids, the refusing rule
//! and a digest of the body; never the content (`lean_ctx_cloud::bus::exchange`).

use serde::Serialize;

use lean_ctx_cloud::CloudStore;
use lean_ctx_cloud::bus::exchange::{self, Entry};

use super::{Failure, flag};

/// The local-read kind of this document.
pub(crate) const KIND: &str = "cloud.exchange.audit";
/// Records printed when `--limit` is not given.
const DEFAULT_LIMIT: usize = 50;

#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct AuditDoc {
    /// Records kept on this machine (at most [`exchange::AUDIT_MAX`]).
    pub total: usize,
    /// The newest `limit` records, newest first.
    pub entries: Vec<Entry>,
}

fn limit(args: &[String]) -> Result<usize, Failure> {
    match flag(args, "--limit")? {
        None => Ok(DEFAULT_LIMIT),
        Some(raw) => raw
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=exchange::AUDIT_MAX).contains(n))
            .ok_or_else(|| {
                Failure::invalid(format!("--limit must be 1 to {}", exchange::AUDIT_MAX))
            }),
    }
}

fn verb(args: &[String]) -> Result<(), Failure> {
    match super::positionals(args).first().copied() {
        Some("audit") => Ok(()),
        _ => Err(Failure::invalid(
            "usage: lean-ctx cloud exchange audit [--json] [--limit N]",
        )),
    }
}

/// The document for `entries` (oldest first, as the ledger keeps them).
pub(crate) fn build(entries: Vec<Entry>, limit: usize) -> AuditDoc {
    let total = entries.len();
    AuditDoc {
        total,
        entries: entries.into_iter().rev().take(limit).collect(),
    }
}

fn read(args: &[String]) -> Result<AuditDoc, Failure> {
    verb(args)?;
    let limit = limit(args)?;
    let store = CloudStore::open().map_err(Failure::from)?;
    let entries = exchange::entries(store.data_dir()).map_err(Failure::from)?;
    Ok(build(entries, limit))
}

pub(crate) fn run(args: &[String]) {
    super::emit(KIND, || read(args));
}

/// The document as lines for people.
pub(crate) fn render(doc: &AuditDoc) -> String {
    if doc.entries.is_empty() {
        return "No agent exchange through LeanCTX Cloud recorded on this machine.\n".into();
    }
    let mut out = format!(
        "Agent exchange audit ({} of {} records, newest first)\n",
        doc.entries.len(),
        doc.total
    );
    for e in &doc.entries {
        let arrow = if e.direction == "in" { "<-" } else { "->" };
        out.push_str(&format!(
            "{} {arrow} {} {} {}",
            e.at, e.outcome, e.kind, e.address
        ));
        if let Some(peer) = &e.peer {
            out.push_str(&format!(" peer {peer}"));
        }
        if let Some(id) = &e.msg_id {
            out.push_str(&format!(" id {id}"));
        }
        if let (Some(rule), Some(reason)) = (&e.rule, &e.reason) {
            out.push_str(&format!(" [{rule}: {reason}]"));
        }
        out.push('\n');
    }
    out
}

/// `lean-ctx cloud exchange audit` for people.
pub(crate) fn run_human(args: &[String]) -> Result<(), lean_ctx_cloud::CloudError> {
    let doc = read(args).map_err(|f| lean_ctx_cloud::CloudError::Config(f.message))?;
    print!("{}", render(&doc));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lean_ctx_cloud::bus::v2::MessageType;

    fn entry(outcome: &str) -> Entry {
        let at = lean_ctx_time::UtcDateTime::parse_from_rfc3339("2026-11-19T10:00:00Z").unwrap();
        Entry::now(at, "out", outcome, MessageType::Finding, "#acme/payments")
    }

    #[test]
    fn newest_first_and_limited() {
        let doc = build(vec![entry("queued"), entry("refused")], 1);
        assert_eq!(doc.total, 2);
        assert_eq!(doc.entries.len(), 1);
        assert_eq!(doc.entries[0].outcome, "refused");
        let lines = render(&doc);
        assert!(
            lines.contains("-> refused finding #acme/payments"),
            "{lines}"
        );
        assert!(render(&build(Vec::new(), 5)).starts_with("No agent exchange"));
    }

    #[test]
    fn arguments_are_checked() {
        let args = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert!(verb(&args(&["audit", "--json"])).is_ok());
        assert!(verb(&args(&["list"])).is_err());
        assert_eq!(limit(&args(&["audit"])).unwrap(), DEFAULT_LIMIT);
        assert_eq!(limit(&args(&["audit", "--limit", "3"])).unwrap(), 3);
        assert!(limit(&args(&["audit", "--limit", "0"])).is_err());
    }
}
