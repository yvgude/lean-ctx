use chrono::{DateTime, Utc};
use serde::Serialize;

use super::SessionState;

const ATTACH_SESSION_JOURNAL_SCHEMA_VERSION: u32 = 1;

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
        AttachSessionJournalV1 {
            schema_version: ATTACH_SESSION_JOURNAL_SCHEMA_VERSION,
            id: self.id.clone(),
            version: self.version,
            started_at: self.started_at,
            updated_at: self.updated_at,
            project_root: self.project_root.clone(),
            shell_cwd: self.shell_cwd.clone(),
            task: self.task.as_ref().map(|task| AttachSessionTaskV1 {
                description: task.description.clone(),
                progress_pct: task.progress_pct,
            }),
            findings: self
                .findings
                .iter()
                .map(|finding| AttachSessionFindingV1 {
                    file: finding.file.clone(),
                    line: finding.line,
                    summary: finding.summary.clone(),
                    timestamp: finding.timestamp,
                })
                .collect(),
            decisions: self
                .decisions
                .iter()
                .map(|decision| AttachSessionDecisionV1 {
                    summary: decision.summary.clone(),
                    rationale: decision.rationale.clone(),
                    timestamp: decision.timestamp,
                })
                .collect(),
            files_touched: self
                .files_touched
                .iter()
                .map(|file| AttachSessionFileV1 {
                    path: file.path.clone(),
                    file_ref: file.file_ref.clone(),
                    modified: file.modified,
                    last_mode: file.last_mode.clone(),
                    summary: file.summary.clone(),
                })
                .collect(),
            next_steps: self.next_steps.clone(),
        }
    }
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

        let loaded = SessionState::load_by_id("compat").unwrap();
        assert_eq!(loaded.evidence.len(), 1);
        assert_eq!(loaded.files_touched[0].tokens, 1234);
        let dir = super::super::paths::sessions_dir().unwrap();
        let names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().all(|name| !name.contains("journal")));
    }
}
