use chrono::{DateTime, Utc};
use serde::Serialize;

use super::SessionState;

const ATTACH_SESSION_JOURNAL_SCHEMA_VERSION: u32 = 1;
const MAX_ATTACH_SESSION_JOURNAL_BYTES: usize = 64 * 1024;
const MAX_ATTACH_SESSION_JOURNAL_ITEMS: usize = 64;
const MAX_ATTACH_SESSION_JOURNAL_STRING_BYTES: usize = 4 * 1024;

/// Minimal, versioned projection of OSS coding-agent continuity.
///
/// This contract intentionally excludes Product intent, learned state,
/// persistence mechanics, configuration, evidence payloads, and derived
/// counters. Those remain in the legacy session until separately migrated.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct AttachSessionJournalV1 {
    pub(crate) schema_version: u32,
    pub(crate) id: String,
    pub(crate) version: u32,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
    pub(crate) project_root: Option<String>,
    pub(crate) shell_cwd: Option<String>,
    pub(crate) task: Option<AttachSessionTaskV1>,
    pub(crate) findings: Vec<AttachSessionFindingV1>,
    pub(crate) decisions: Vec<AttachSessionDecisionV1>,
    pub(crate) files_touched: Vec<AttachSessionFileV1>,
    pub(crate) next_steps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct AttachSessionTaskV1 {
    pub(crate) description: String,
    pub(crate) progress_pct: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct AttachSessionFindingV1 {
    pub(crate) file: Option<String>,
    pub(crate) line: Option<u32>,
    pub(crate) summary: String,
    pub(crate) timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct AttachSessionDecisionV1 {
    pub(crate) summary: String,
    pub(crate) rationale: Option<String>,
    pub(crate) timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct AttachSessionFileV1 {
    pub(crate) path: String,
    pub(crate) file_ref: Option<String>,
    pub(crate) modified: bool,
    pub(crate) last_mode: String,
    pub(crate) summary: Option<String>,
}

impl SessionState {
    pub(crate) fn attach_session_journal_v1(&self) -> AttachSessionJournalV1 {
        let mut journal = AttachSessionJournalV1 {
            schema_version: ATTACH_SESSION_JOURNAL_SCHEMA_VERSION,
            id: bounded_journal_text(&self.id),
            version: self.version,
            started_at: self.started_at,
            updated_at: self.updated_at,
            project_root: bounded_optional_journal_text(self.project_root.as_deref()),
            shell_cwd: bounded_optional_journal_text(self.shell_cwd.as_deref()),
            task: self.task.as_ref().map(|task| AttachSessionTaskV1 {
                description: bounded_journal_text(&task.description),
                progress_pct: task.progress_pct,
            }),
            findings: self
                .findings
                .iter()
                .take(MAX_ATTACH_SESSION_JOURNAL_ITEMS)
                .map(|finding| AttachSessionFindingV1 {
                    file: bounded_optional_journal_text(finding.file.as_deref()),
                    line: finding.line,
                    summary: bounded_journal_text(&finding.summary),
                    timestamp: finding.timestamp,
                })
                .collect(),
            decisions: self
                .decisions
                .iter()
                .take(MAX_ATTACH_SESSION_JOURNAL_ITEMS)
                .map(|decision| AttachSessionDecisionV1 {
                    summary: bounded_journal_text(&decision.summary),
                    rationale: bounded_optional_journal_text(decision.rationale.as_deref()),
                    timestamp: decision.timestamp,
                })
                .collect(),
            files_touched: self
                .files_touched
                .iter()
                .take(MAX_ATTACH_SESSION_JOURNAL_ITEMS)
                .map(|file| AttachSessionFileV1 {
                    path: bounded_journal_text(&file.path),
                    file_ref: bounded_optional_journal_text(file.file_ref.as_deref()),
                    modified: file.modified,
                    last_mode: bounded_journal_text(&file.last_mode),
                    summary: bounded_optional_journal_text(file.summary.as_deref()),
                })
                .collect(),
            next_steps: self
                .next_steps
                .iter()
                .take(MAX_ATTACH_SESSION_JOURNAL_ITEMS)
                .map(|step| bounded_journal_text(step))
                .collect(),
        };
        bound_serialized_journal(&mut journal);
        journal
    }
}

fn bounded_optional_journal_text(value: Option<&str>) -> Option<String> {
    value.map(bounded_journal_text)
}

fn bounded_journal_text(value: &str) -> String {
    let mut bounded =
        String::with_capacity(value.len().min(MAX_ATTACH_SESSION_JOURNAL_STRING_BYTES));
    for character in value.chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if bounded.len() + character.len_utf8() > MAX_ATTACH_SESSION_JOURNAL_STRING_BYTES {
            break;
        }
        bounded.push(character);
    }
    bounded
}

fn bound_serialized_journal(journal: &mut AttachSessionJournalV1) {
    while serde_json::to_vec(journal)
        .map(|serialized| serialized.len() > MAX_ATTACH_SESSION_JOURNAL_BYTES)
        .unwrap_or(true)
    {
        if journal.next_steps.pop().is_some()
            || journal.findings.pop().is_some()
            || journal.decisions.pop().is_some()
            || journal.files_touched.pop().is_some()
        {
            continue;
        }
        if journal.task.take().is_some()
            || journal.shell_cwd.take().is_some()
            || journal.project_root.take().is_some()
        {
            continue;
        }
        break;
    }
    debug_assert!(
        serde_json::to_vec(journal)
            .map(|serialized| serialized.len() <= MAX_ATTACH_SESSION_JOURNAL_BYTES)
            .unwrap_or(false)
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::session::{
        EvidenceKind, EvidenceRecord, FileTouched, Finding, ProgressEntry, TaskInfo, TestSnapshot,
    };

    fn fixed_time() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-25T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn fixture() -> SessionState {
        let timestamp = fixed_time();
        let mut session = SessionState::new();
        session.id = "attach-1".into();
        session.version = 7;
        session.started_at = timestamp;
        session.updated_at = timestamp;
        session.project_root = Some("/workspace/project".into());
        session.shell_cwd = Some("/workspace/project/src".into());
        session.task = Some(TaskInfo {
            description: "implement seam".into(),
            intent: Some("product-strategy-must-not-leak".into()),
            progress_pct: Some(40),
        });
        session.findings.push(Finding {
            file: Some("src/lib.rs".into()),
            line: Some(8),
            summary: "fact".into(),
            timestamp,
        });
        session.add_decision("keep compatibility", Some("migration remains reversible"));
        session.files_touched.push(FileTouched {
            path: "src/lib.rs".into(),
            file_ref: Some("F1".into()),
            read_count: 99,
            modified: true,
            last_mode: "full".into(),
            tokens: 1234,
            stale: true,
            context_item_id: Some("derived-id".into()),
            summary: Some("library".into()),
        });
        session.next_steps.push("review".into());

        // Explicitly populate deferred fields to prove that V1 does not leak
        // them merely because the legacy state happens to contain them.
        session.test_results = Some(TestSnapshot {
            command: "cargo test".into(),
            passed: 1,
            failed: 0,
            total: 1,
            timestamp,
        });
        session.progress.push(ProgressEntry {
            action: "tested".into(),
            detail: Some("pass".into()),
            timestamp,
        });
        session.evidence.push(EvidenceRecord {
            kind: EvidenceKind::ToolCall,
            key: "gate".into(),
            value: Some("pass".into()),
            tool: Some("cargo".into()),
            input_md5: None,
            output_md5: None,
            agent_id: Some("worker".into()),
            client_name: Some("codex".into()),
            task_id: Some("r4c".into()),
            timestamp,
        });
        session.terse_mode = true;
        session.compression_level = "standard".into();
        session.extra_roots.push("/workspace/shared".into());
        session
    }

    #[test]
    fn projection_contains_only_minimum_attach_continuity() {
        let value = serde_json::to_value(fixture().attach_session_journal_v1()).unwrap();

        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["task"]["description"], "implement seam");
        assert_eq!(value["findings"][0]["summary"], "fact");
        assert_eq!(value["decisions"][0]["summary"], "keep compatibility");
        assert_eq!(value["files_touched"][0]["path"], "src/lib.rs");
        assert_eq!(value["next_steps"][0], "review");
        assert!(value["task"].get("intent").is_none());
        assert!(value["files_touched"][0].get("read_count").is_none());
        assert!(value["files_touched"][0].get("tokens").is_none());
        assert!(value["files_touched"][0].get("stale").is_none());
        assert!(value["files_touched"][0].get("context_item_id").is_none());
        for deferred in [
            "test_results",
            "progress",
            "evidence",
            "terse_mode",
            "compression_level",
            "extra_roots",
            "stats",
            "playbook",
        ] {
            assert!(value.get(deferred).is_none(), "unexpected {deferred}");
        }
    }

    #[test]
    fn projection_is_repeatable_and_owned() {
        let mut session = fixture();
        let journal = session.attach_session_journal_v1();
        let first = serde_json::to_vec(&journal).unwrap();
        session.findings.clear();
        let second = serde_json::to_vec(&journal).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn projection_bounds_legacy_cardinality_strings_and_serialized_bytes() {
        let mut session = fixture();
        let oversized = format!("{}{}", "\\\"".repeat(4_096), "界".repeat(4_096));
        session.id = oversized.clone();
        session.project_root = Some(oversized.clone());
        session.shell_cwd = Some(oversized.clone());
        session.next_steps = vec![oversized.clone(); MAX_ATTACH_SESSION_JOURNAL_ITEMS * 2];
        session.findings = vec![
            Finding {
                file: Some(oversized.clone()),
                line: Some(1),
                summary: oversized.clone(),
                timestamp: fixed_time(),
            };
            MAX_ATTACH_SESSION_JOURNAL_ITEMS * 2
        ];

        let journal = session.attach_session_journal_v1();
        let serialized = serde_json::to_vec(&journal).unwrap();

        assert!(serialized.len() <= MAX_ATTACH_SESSION_JOURNAL_BYTES);
        assert!(journal.id.len() <= MAX_ATTACH_SESSION_JOURNAL_STRING_BYTES);
        assert!(journal.findings.len() <= MAX_ATTACH_SESSION_JOURNAL_ITEMS);
        assert!(journal.next_steps.len() <= MAX_ATTACH_SESSION_JOURNAL_ITEMS);
        assert!(
            journal
                .findings
                .iter()
                .all(|finding| finding.summary.len() <= MAX_ATTACH_SESSION_JOURNAL_STRING_BYTES)
        );
    }

    #[test]
    fn legacy_session_roundtrip_keeps_deferred_fields() {
        let session = fixture();
        let decoded: SessionState =
            serde_json::from_str(&serde_json::to_string(&session).unwrap()).unwrap();

        assert_eq!(
            decoded.task.and_then(|task| task.intent).as_deref(),
            Some("product-strategy-must-not-leak")
        );
        assert_eq!(decoded.files_touched[0].tokens, 1234);
        assert_eq!(decoded.evidence.len(), 1);
        assert_eq!(decoded.progress.len(), 1);
        assert_eq!(decoded.extra_roots, ["/workspace/shared"]);
    }

    #[test]
    fn legacy_save_load_stays_authoritative_without_journal_sidecar() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let mut session = fixture();
        session.id = "compat".into();
        session.prepare_save().unwrap().write_to_disk().unwrap();

        let expected_journal = session.attach_session_journal_v1();
        let loaded = SessionState::load_by_id("compat").unwrap();
        assert_eq!(loaded.evidence.len(), 1);
        assert_eq!(loaded.files_touched[0].tokens, 1234);
        assert_eq!(loaded.attach_session_journal_v1(), expected_journal);
        let dir = super::super::paths::sessions_dir().unwrap();
        let names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().all(|name| !name.contains("journal")));
    }
}
