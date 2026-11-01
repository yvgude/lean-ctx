// SPDX-License-Identifier: Apache-2.0
//! Change journal of a project's knowledge.
//!
//! Every save of the store appends one row per fact it added, changed or
//! removed, numbered by a log sequence number (LSN) that only grows. A
//! reader asks "what changed since LSN n" instead of reloading and diffing
//! the whole store, and a session can be shown the facts it touched.
//!
//! Rows carry the fact's identity (category, key with secrets redacted,
//! creation time), the names of the fields that changed and the writing
//! session, never a value: the store holds the values, and a row is shown
//! with its key only while that fact is visible in the reader's admitted
//! view (`ctx_knowledge changes`). Local only; nothing is sent anywhere.
//!
//! The journal sits beside `knowledge.json` and is written under the same
//! project lock as the store, so its order is the order of the saves. It
//! keeps the newest rows within `MAX_JOURNAL_BYTES`; LSNs continue across
//! a trim.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::types::KnowledgeFact;

pub(crate) const JOURNAL_FILE: &str = "journal.jsonl";
/// Size at which the oldest half of the rows is dropped.
const MAX_JOURNAL_BYTES: u64 = 2 * 1024 * 1024;
/// Bytes read from the end of the journal to find the newest LSN.
const TAIL_BYTES: u64 = 4096;
/// Fields whose change is a change of the fact. Read counters, feedback
/// tallies and confirmation stamps change on every use and are not journaled.
const TRACKED_FIELDS: [&str; 8] = [
    "value",
    "status",
    "validity",
    "anchors",
    "review",
    "valid_until",
    "supersedes",
    "privacy",
];

/// One journaled change of one fact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JournalRow {
    pub lsn: u64,
    pub at: String,
    /// `added`, `changed` or `removed`.
    pub op: String,
    pub category: String,
    pub key: String,
    /// The fact's creation time: with category and key, its identity (a
    /// superseded fact and its successor share category and key).
    pub created: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
    pub session: String,
}

/// A change before it is numbered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Change {
    pub op: &'static str,
    pub category: String,
    pub key: String,
    pub created: String,
    pub fields: Vec<String>,
    pub session: String,
}

fn identity(fact: &KnowledgeFact) -> (String, String, String) {
    (
        fact.category.clone(),
        fact.key.clone(),
        fact.created_at.to_rfc3339(),
    )
}

fn tracked(fact: &KnowledgeFact) -> Vec<serde_json::Value> {
    let value = serde_json::to_value(fact).unwrap_or_default();
    TRACKED_FIELDS
        .iter()
        .map(|field| value.get(field).cloned().unwrap_or_default())
        .collect()
}

/// The changes from `before` to `after`, in identity order.
pub(crate) fn diff(before: &[KnowledgeFact], after: &[KnowledgeFact]) -> Vec<Change> {
    use std::collections::BTreeMap;
    let index = |facts: &[KnowledgeFact]| -> BTreeMap<(String, String, String), usize> {
        facts
            .iter()
            .enumerate()
            .map(|(i, fact)| (identity(fact), i))
            .collect()
    };
    let (old, new) = (index(before), index(after));
    let change = |op, fact: &KnowledgeFact, fields: Vec<String>| Change {
        op,
        category: fact.category.clone(),
        key: crate::core::redaction::redact_text(&fact.key),
        created: fact.created_at.to_rfc3339(),
        fields,
        session: fact.source_session.clone(),
    };
    let mut changes = Vec::new();
    for (id, &i) in &new {
        let fact = &after[i];
        match old.get(id) {
            None => changes.push(change("added", fact, Vec::new())),
            Some(&j) => {
                let fields: Vec<String> = tracked(&before[j])
                    .iter()
                    .zip(tracked(fact))
                    .zip(TRACKED_FIELDS)
                    .filter(|((was, now), _)| *was != now)
                    .map(|(_, name)| name.to_owned())
                    .collect();
                if !fields.is_empty() {
                    changes.push(change("changed", fact, fields));
                }
            }
        }
    }
    for (id, &j) in &old {
        if !new.contains_key(id) {
            changes.push(change("removed", &before[j], Vec::new()));
        }
    }
    changes
        .sort_by(|a, b| (&a.category, &a.key, &a.created).cmp(&(&b.category, &b.key, &b.created)));
    changes
}

/// Append `changes` under the next LSNs. The caller holds the project's
/// knowledge lock. Best effort: a journal that cannot be written never fails
/// the save it describes.
pub(crate) fn append(dir: &Path, changes: &[Change], at: &str) {
    if changes.is_empty() {
        return;
    }
    let path = dir.join(JOURNAL_FILE);
    let first = head_lsn(dir) + 1;
    let mut text = String::new();
    for (lsn, change) in (first..).zip(changes) {
        let row = JournalRow {
            lsn,
            at: at.to_owned(),
            op: change.op.to_owned(),
            category: change.category.clone(),
            key: change.key.clone(),
            created: change.created.clone(),
            fields: change.fields.clone(),
            session: change.session.clone(),
        };
        if let Ok(line) = serde_json::to_string(&row) {
            text.push_str(&line);
            text.push('\n');
        }
    }
    let appended = crate::core::policy::diagnostics::open_append(&path)
        .and_then(|mut file| file.write_all(text.as_bytes()));
    if appended.is_err() {
        tracing::debug!("[knowledge] journal append failed");
        return;
    }
    trim(&path);
}

/// The newest LSN, 0 for an empty or missing journal.
pub fn head_lsn(dir: &Path) -> u64 {
    let Ok(mut file) = std::fs::File::open(dir.join(JOURNAL_FILE)) else {
        return 0;
    };
    let len = file.metadata().map_or(0, |m| m.len());
    let start = len.saturating_sub(TAIL_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return 0;
    }
    let mut tail = String::new();
    if file.read_to_string(&mut tail).is_err() {
        return 0;
    }
    tail.lines()
        .rev()
        .find_map(|line| serde_json::from_str::<JournalRow>(line).ok())
        .map_or(0, |row| row.lsn)
}

/// The rows after `since`, oldest first, at most `limit`.
pub fn rows_since(dir: &Path, since: u64, limit: usize) -> Vec<JournalRow> {
    let Ok(text) = std::fs::read_to_string(dir.join(JOURNAL_FILE)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str::<JournalRow>(line).ok())
        .filter(|row| row.lsn > since)
        .take(limit)
        .collect()
}

/// Keep the journal within [`MAX_JOURNAL_BYTES`] by dropping its older half.
fn trim(path: &Path) {
    if std::fs::metadata(path).map_or(0, |m| m.len()) <= MAX_JOURNAL_BYTES {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let lines: Vec<&str> = text.lines().collect();
    let kept = lines[lines.len() / 2..].join("\n") + "\n";
    let _ = crate::core::atomic_fs::try_atomic_write(path, kept.as_bytes(), None);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(category: &str, key: &str, value: &str, created: &str) -> KnowledgeFact {
        serde_json::from_value(serde_json::json!({
            "category": category, "key": key, "value": value,
            "source_session": "s1", "confidence": 0.9,
            "created_at": created, "last_confirmed": created,
        }))
        .unwrap()
    }

    const T0: &str = "2026-10-31T10:00:00Z";
    const T1: &str = "2026-10-31T11:00:00Z";

    #[test]
    fn a_diff_names_added_changed_and_removed_facts_by_identity() {
        let before = vec![
            fact("decision", "db", "postgres", T0),
            fact("decision", "queue", "redis", T0),
            fact("convention", "naming", "snake", T0),
        ];
        let mut changed = fact("decision", "db", "sqlite", T0);
        changed.retrieval_count = 9; // a read counter is not a change
        let mut read_only = fact("convention", "naming", "snake", T0);
        read_only.retrieval_count = 3;
        let after = vec![changed, read_only, fact("decision", "db", "duckdb", T1)];
        let changes = diff(&before, &after);
        let summary: Vec<(&str, &str, &[String])> = changes
            .iter()
            .map(|c| (c.op, c.key.as_str(), c.fields.as_slice()))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("changed", "db", &["value".to_owned()][..]),
                ("added", "db", &[][..]),
                ("removed", "queue", &[][..]),
            ]
        );
    }

    #[test]
    fn rows_are_numbered_on_and_read_back_after_an_lsn() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(head_lsn(dir.path()), 0);
        let changes = diff(
            &[],
            &[
                fact("decision", "db", "pg", T0),
                fact("decision", "q", "r", T0),
            ],
        );
        append(dir.path(), &changes, T0);
        append(
            dir.path(),
            &diff(&[fact("decision", "db", "pg", T0)], &[]),
            T1,
        );
        assert_eq!(head_lsn(dir.path()), 3);
        let rows = rows_since(dir.path(), 1, 10);
        assert_eq!(rows.iter().map(|r| r.lsn).collect::<Vec<_>>(), vec![2, 3]);
        assert_eq!(rows[1].op, "removed");
        assert_eq!(rows_since(dir.path(), 0, 1).len(), 1);
        // No row carries a value.
        let raw = std::fs::read_to_string(dir.path().join(JOURNAL_FILE)).unwrap();
        assert!(!raw.contains("\"pg\""));
    }

    #[test]
    fn a_secret_in_a_key_is_redacted_in_the_journal() {
        let key = concat!("deploy-", "gh", "p_", "Zq8LmW3vXc9TbN2rKp7YdF4sHj6Ga1Ue");
        let changes = diff(&[], &[fact("decision", key, "v", T0)]);
        assert!(
            !changes[0].key.contains("Zq8LmW3vXc9TbN2rKp7Y"),
            "{}",
            changes[0].key
        );
    }

    #[test]
    fn a_trimmed_journal_keeps_its_newest_rows_and_numbering() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(JOURNAL_FILE);
        let row = |lsn: u64| {
            serde_json::to_string(&JournalRow {
                lsn,
                at: T0.into(),
                op: "added".into(),
                category: "c".into(),
                key: "k".repeat(200),
                created: T0.into(),
                fields: Vec::new(),
                session: "s".into(),
            })
            .unwrap()
        };
        let lines: Vec<String> = (1..=10_000).map(row).collect();
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        trim(&path);
        assert!(std::fs::metadata(&path).unwrap().len() <= MAX_JOURNAL_BYTES);
        assert_eq!(head_lsn(dir.path()), 10_000);
        assert_eq!(rows_since(dir.path(), 0, 1)[0].lsn, 5_001);
    }
}
