// SPDX-License-Identifier: Apache-2.0

//! Persist one completed read, never an MCP process's stale ledger snapshot.

use std::path::Path;

use super::{ContextLedger, LedgerEntry};
use crate::core::context_field::ContextState;

impl ContextLedger {
    pub(crate) fn read_entry(&self, path: &str) -> Option<LedgerEntry> {
        let path = crate::core::pathutil::normalize_tool_path(path);
        self.entries
            .iter()
            .find(|entry| entry.path == path)
            .cloned()
    }

    /// Complete persistence before acknowledging a read. No async state lock
    /// may be held while this operation waits for the existing OS ledger lock.
    pub(crate) async fn persist_read(entry: Option<LedgerEntry>) -> Result<(), String> {
        let entry = entry.ok_or("completed read has no ledger entry")?;
        tokio::task::spawn_blocking(move || {
            let path = super::helpers::ledger_path("default")?;
            persist_entry_at(&path, entry)
        })
        .await
        .map_err(|error| format!("ledger persistence worker failed: {error}"))?
    }
}

fn persist_entry_at(path: &Path, mut entry: LedgerEntry) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        crate::config_io::ensure_dir(parent)?;
    }
    // fs2 uses the same advisory flock as the existing Unix ledger writer,
    // and also provides a real lock on Windows. Lock failure prevents writes.
    let _lock = crate::core::agents::FileLock::acquire(&path.with_extension("json.lock"))?;
    let mut current: ContextLedger = match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            if !meta.is_file() || meta.len() > 8 * 1024 * 1024 {
                return Err("ledger must be a regular file of at most 8 MiB".into());
            }
            let bytes = std::fs::read(path).map_err(|error| format!("read ledger: {error}"))?;
            serde_json::from_slice(&bytes).map_err(|error| format!("decode ledger: {error}"))?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ContextLedger::new(),
        Err(error) => return Err(format!("inspect ledger: {error}")),
    };
    if let Some(previous) = current
        .entries
        .iter_mut()
        .find(|old| old.path == entry.path)
    {
        // Merge observations only. Another writer may have changed source
        // provenance, controls or identity since this MCP process loaded state.
        previous.mode = entry.mode;
        previous.original_tokens = entry.original_tokens;
        previous.sent_tokens = entry.sent_tokens;
        previous.timestamp = entry.timestamp;
        previous.access_count = previous.access_count.saturating_add(1);
        previous.active_view = entry.active_view;
        previous.phi = entry.phi;
        if previous.id.is_none() {
            previous.id = entry.id;
        }
        if previous.state.is_none() || previous.state == Some(ContextState::Candidate) {
            previous.state = Some(ContextState::Included);
        }
    } else {
        // A reset followed by a new read restores only that new read, not the
        // caller's other stale entries or counters.
        entry.access_count = 1;
        current.entries.push(entry);
    }
    current.total_tokens_sent = current.entries.iter().map(|item| item.sent_tokens).sum();
    current.total_tokens_saved = current
        .entries
        .iter()
        .map(|item| item.original_tokens.saturating_sub(item.sent_tokens))
        .sum();
    let json =
        serde_json::to_string(&current).map_err(|error| format!("encode ledger: {error}"))?;
    crate::config_io::write_atomic(path, &json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_readers_preserve_each_other_and_reset() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("context_ledger.json");
        let mut first = ContextLedger::new();
        first.record("/first.txt", "raw", 10, 8);
        let mut second = ContextLedger::new();
        second.record("/second.txt", "raw", 20, 12);
        let a = first.read_entry("/first.txt").unwrap();
        let b = second.read_entry("/second.txt").unwrap();
        persist_entry_at(&path, a.clone()).unwrap();
        persist_entry_at(&path, b.clone()).unwrap();
        let mut saved: ContextLedger =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved.entries.len(), 2);
        assert_eq!(
            (saved.total_tokens_sent, saved.total_tokens_saved),
            (20, 10)
        );
        saved.entries[0].state = Some(ContextState::Stale);
        saved.entries[0].source_hash = Some("concurrent-source-digest".into());
        saved.entries[0].kind = Some(crate::core::context_field::ContextKind::Shell);
        std::fs::write(&path, serde_json::to_vec(&saved).unwrap()).unwrap();
        persist_entry_at(&path, a).unwrap();
        let saved: ContextLedger = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved.entries[0].state, Some(ContextState::Stale));
        assert_eq!(
            saved.entries[0].source_hash.as_deref(),
            Some("concurrent-source-digest")
        );
        assert_eq!(
            saved.entries[0].kind,
            Some(crate::core::context_field::ContextKind::Shell)
        );
        assert_eq!(saved.entries[0].access_count, 2);
        std::fs::write(&path, serde_json::to_vec(&ContextLedger::new()).unwrap()).unwrap();
        persist_entry_at(&path, b).unwrap();
        let saved: ContextLedger = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved.entries.len(), 1);
        assert_eq!(saved.entries[0].path, "/second.txt");
        assert_eq!((saved.total_tokens_sent, saved.total_tokens_saved), (12, 8));
    }

    #[test]
    fn corrupt_or_locked_ledger_is_not_replaced() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("context_ledger.json");
        let mut ledger = ContextLedger::new();
        ledger.record("/read.txt", "raw", 10, 8);
        let entry = ledger.read_entry("/read.txt").unwrap();
        std::fs::write(&path, b"corrupt").unwrap();
        assert!(persist_entry_at(&path, entry.clone()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"corrupt");
        let empty = serde_json::to_vec(&ContextLedger::new()).unwrap();
        std::fs::write(&path, &empty).unwrap();
        let _lock =
            crate::core::agents::FileLock::acquire(&path.with_extension("json.lock")).unwrap();
        assert!(persist_entry_at(&path, entry).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), empty);
    }
}
