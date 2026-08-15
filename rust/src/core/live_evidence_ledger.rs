//! Bounded, append-only evidence emitted for completed live tool calls.

use chrono::Utc;
use fs2::FileExt;
use serde::Serialize;
use std::{
    fs::OpenOptions,
    io::{Read, Seek, SeekFrom, Write},
};

const MAX_ENTRIES: usize = 10_000;
const EVIDENCE_STAGES: [&str; 4] = ["ingress", "triage", "router", "value_gate"];

#[derive(Debug, Clone, Serialize)]
pub struct EvidenceLedgerEntry {
    pub task_id: String,
    pub session_id: String,
    pub timestamp: String,
    pub stages: [&'static str; 4],
    pub triage_class: String,
    pub savings_pct: f64,
    pub cpao: Option<u64>,
    pub outcome: String,
}

impl EvidenceLedgerEntry {
    #[must_use]
    pub fn completed(
        task_id: &str,
        session_id: &str,
        triage_class: &str,
        saved_tokens: u64,
        output_tokens: u64,
        cpao: Option<u64>,
        outcome_accepted: bool,
    ) -> Self {
        let raw_tokens = saved_tokens.saturating_add(output_tokens);
        let savings_pct = if raw_tokens == 0 {
            0.0
        } else {
            (saved_tokens as f64 / raw_tokens as f64) * 100.0
        };

        Self {
            task_id: task_id.to_owned(),
            session_id: session_id.to_owned(),
            timestamp: Utc::now().to_rfc3339(),
            stages: EVIDENCE_STAGES,
            triage_class: triage_class.to_owned(),
            savings_pct,
            cpao,
            outcome: if outcome_accepted {
                "accepted".to_owned()
            } else {
                "rejected".to_owned()
            },
        }
    }
}

pub fn append_completion(entry: &EvidenceLedgerEntry) -> Result<(), String> {
    let path = crate::core::data_dir::lean_ctx_data_dir()?.join("evidence_ledger.jsonl");
    let line = serde_json::to_string(entry).map_err(|error| error.to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|error| format!("open {}: {error}", path.display()))?;

    file.lock_exclusive()
        .map_err(|error| format!("lock {}: {error}", path.display()))?;
    let result = append_and_rotate(&mut file, &line);
    let _ = fs2::FileExt::unlock(&file);
    result.map_err(|error| format!("write {}: {error}", path.display()))
}

fn append_and_rotate(file: &mut std::fs::File, line: &str) -> std::io::Result<()> {
    file.seek(SeekFrom::Start(0))?;
    let mut existing = String::new();
    file.read_to_string(&mut existing)?;
    let entries = existing
        .lines()
        .filter(|entry| !entry.trim().is_empty())
        .collect::<Vec<_>>();

    if entries.len() < MAX_ENTRIES {
        file.seek(SeekFrom::End(0))?;
        writeln!(file, "{line}")?;
    } else {
        let retained = &entries[entries.len().saturating_sub(MAX_ENTRIES - 1)..];
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        for entry in retained {
            writeln!(file, "{entry}")?;
        }
        writeln!(file, "{line}")?;
    }
    file.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_entry_contains_the_complete_stage_chain() {
        let entry =
            EvidenceLedgerEntry::completed("task-1", "session-1", "coding", 75, 25, Some(12), true);

        assert_eq!(entry.stages, EVIDENCE_STAGES);
        assert_eq!(entry.savings_pct, 75.0);
        assert_eq!(entry.cpao, Some(12));
        assert_eq!(entry.outcome, "accepted");
    }

    #[test]
    fn evidence_ledger_rotation_keeps_the_newest_ten_thousand_entries() {
        let path =
            std::env::temp_dir().join(format!("lean-ctx-evidence-ledger-{}", std::process::id()));
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .expect("create temporary ledger");
        for index in 0..MAX_ENTRIES {
            writeln!(file, "{{\"index\":{index}}}").expect("seed ledger");
        }

        append_and_rotate(&mut file, "{\"index\":10000}").expect("rotate ledger");
        file.seek(SeekFrom::Start(0)).expect("rewind ledger");
        let mut contents = String::new();
        file.read_to_string(&mut contents).expect("read ledger");
        let entries = contents.lines().collect::<Vec<_>>();

        assert_eq!(entries.len(), MAX_ENTRIES);
        assert_eq!(entries.first(), Some(&"{\"index\":1}"));
        assert_eq!(entries.last(), Some(&"{\"index\":10000}"));
        std::fs::remove_file(path).expect("remove temporary ledger");
    }
}
