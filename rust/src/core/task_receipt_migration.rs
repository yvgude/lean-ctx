// SPDX-License-Identifier: Apache-2.0

//! Fail-closed V3 task/receipt compatibility migration.
//!
//! Legacy execution receipts are preserved and digest-bound, never promoted
//! into canonical signed `ReceiptDocumentV1` evidence.

use std::io::Write;
use std::path::{Path, PathBuf};

use lean_ctx_protocol::{AcceptedOutcomeV1, ExecutionReceiptV1, TaskEnvelopeV1};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024;
const RECORD_NAME: &str = "leanctx-v4-task-receipt-migration.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LegacyArtifactDigestV1 {
    pub name: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskReceiptMigrationRecordV1 {
    pub schema_version: u32,
    pub task_id: String,
    pub receipt_id: String,
    pub plan_id: String,
    pub source_artifacts: Vec<LegacyArtifactDigestV1>,
    pub task_envelope: String,
    pub receipt_authority: String,
    pub canonical_successor: String,
    pub required_action: String,
    pub sources_retained_for_rollback: bool,
}

pub(crate) fn rollback_legacy_task_receipt(source_dir: &Path) -> Result<(), String> {
    reject_symlink(source_dir, "source directory")?;
    let _lock = lock_task_receipt_migration(source_dir)?;
    let record_path = source_dir.join(RECORD_NAME);
    reject_symlink(&record_path, "migration record")?;
    let bytes =
        std::fs::read(&record_path).map_err(|error| format!("read migration record: {error}"))?;
    let record: TaskReceiptMigrationRecordV1 = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse migration record: {error}"))?;
    if record.schema_version != 1
        || record.task_envelope != "canonical_v1_preserved"
        || record.receipt_authority != "legacy_non_authoritative"
        || record.canonical_successor != "ReceiptDocumentV1"
        || record.required_action != "reexecute_with_canonical_producer"
        || !record.sources_retained_for_rollback
    {
        return Err("migration record is not rollback-compatible".to_string());
    }
    let expected = [
        "baseline.json",
        "context_plan.json",
        "execution_receipt.json",
        "outcome.json",
        "task_envelope.json",
    ];
    if record.source_artifacts.len() != expected.len()
        || record
            .source_artifacts
            .iter()
            .zip(expected)
            .any(|(artifact, expected_name)| artifact.name != expected_name)
    {
        return Err("migration record source set is incomplete or unordered".to_string());
    }
    for artifact in &record.source_artifacts {
        let source = source_dir.join(&artifact.name);
        reject_symlink(&source, &artifact.name)?;
        let source_bytes =
            std::fs::read(&source).map_err(|error| format!("read {}: {error}", artifact.name))?;
        let digest = crate::core::agent_identity::hex_encode(&Sha256::digest(&source_bytes));
        if source_bytes.len() as u64 != artifact.bytes || digest != artifact.sha256 {
            return Err(format!("{} changed since migration", artifact.name));
        }
    }
    std::fs::remove_file(&record_path)
        .map_err(|error| format!("remove migration record: {error}"))?;
    crate::core::atomic_fs::sync_directory(source_dir)
        .map_err(|error| format!("sync rollback directory: {error}"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskReceiptMigrationResult {
    pub record_path: PathBuf,
    pub already_migrated: bool,
    pub record: TaskReceiptMigrationRecordV1,
}

pub(crate) fn migrate_legacy_task_receipt(
    source_dir: &Path,
) -> Result<TaskReceiptMigrationResult, String> {
    let _lock = lock_task_receipt_migration(source_dir)?;
    let record = assess_legacy_task_receipt(source_dir)?;
    let record_bytes =
        serde_json::to_vec_pretty(&record).map_err(|error| format!("encode record: {error}"))?;
    let record_path = source_dir.join(RECORD_NAME);
    let already_migrated = if record_path.exists() {
        reject_symlink(&record_path, "migration record")?;
        let existing = std::fs::read(&record_path)
            .map_err(|error| format!("read migration record: {error}"))?;
        if existing != record_bytes {
            return Err(format!(
                "refusing to overwrite non-identical migration record: {}",
                record_path.display()
            ));
        }
        true
    } else {
        create_without_overwrite(&record_path, &record_bytes)?;
        false
    };

    Ok(TaskReceiptMigrationResult {
        record_path,
        already_migrated,
        record,
    })
}

pub(crate) fn assess_legacy_task_receipt(
    source_dir: &Path,
) -> Result<TaskReceiptMigrationRecordV1, String> {
    reject_symlink(source_dir, "source directory")?;
    if !source_dir.is_dir() {
        return Err(format!(
            "task/receipt migration source is not a directory: {}",
            source_dir.display()
        ));
    }

    let task_bytes = read_source(source_dir, "task_envelope.json")?;
    let receipt_bytes = read_source(source_dir, "execution_receipt.json")?;
    let task: TaskEnvelopeV1 = serde_json::from_slice(&task_bytes)
        .map_err(|error| format!("parse task_envelope.json: {error}"))?;
    task.validate()
        .map_err(|error| format!("validate task_envelope.json: {error}"))?;
    let receipt: ExecutionReceiptV1 = serde_json::from_slice(&receipt_bytes)
        .map_err(|error| format!("parse execution_receipt.json: {error}"))?;
    receipt
        .validate()
        .map_err(|error| format!("validate execution_receipt.json: {error}"))?;
    if task.task_id != receipt.task_id {
        return Err("task_id mismatch between task envelope and execution receipt".to_string());
    }

    let mut source_artifacts = vec![
        artifact_digest("execution_receipt.json", &receipt_bytes),
        artifact_digest("task_envelope.json", &task_bytes),
    ];
    for name in ["baseline.json", "context_plan.json", "outcome.json"] {
        let bytes = read_source(source_dir, name)?;
        validate_source_join(name, &bytes, task.task_id.as_str(), &receipt)?;
        source_artifacts.push(artifact_digest(name, &bytes));
    }
    source_artifacts.sort_by(|left, right| left.name.cmp(&right.name));

    Ok(TaskReceiptMigrationRecordV1 {
        schema_version: 1,
        task_id: task.task_id.as_str().to_string(),
        receipt_id: receipt.receipt_id.as_str().to_string(),
        plan_id: receipt.plan_id.as_str().to_string(),
        source_artifacts,
        task_envelope: "canonical_v1_preserved".to_string(),
        receipt_authority: "legacy_non_authoritative".to_string(),
        canonical_successor: "ReceiptDocumentV1".to_string(),
        required_action: "reexecute_with_canonical_producer".to_string(),
        sources_retained_for_rollback: true,
    })
}

fn read_source(source_dir: &Path, name: &str) -> Result<Vec<u8>, String> {
    let path = source_dir.join(name);
    reject_symlink(&path, name)?;
    let metadata = std::fs::metadata(&path).map_err(|error| format!("stat {name}: {error}"))?;
    if !metadata.is_file() {
        return Err(format!("{name} is not a regular file"));
    }
    if metadata.len() > MAX_ARTIFACT_BYTES {
        return Err(format!("{name} exceeds {MAX_ARTIFACT_BYTES} bytes"));
    }
    std::fs::read(&path).map_err(|error| format!("read {name}: {error}"))
}

fn reject_symlink(path: &Path, label: &str) -> Result<(), String> {
    if path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        Err(format!("refusing symlink {label}: {}", path.display()))
    } else {
        Ok(())
    }
}

fn lock_task_receipt_migration(
    source_dir: &Path,
) -> Result<crate::core::migration_lock::MigrationLock, String> {
    reject_symlink(source_dir, "source directory")?;
    let lock_path = source_dir.join(".leanctx-v4-task-receipt-migration.lock");
    reject_symlink(&lock_path, "migration lock")?;
    crate::core::migration_lock::acquire(&lock_path)
}

fn artifact_digest(name: &str, bytes: &[u8]) -> LegacyArtifactDigestV1 {
    LegacyArtifactDigestV1 {
        name: name.to_string(),
        sha256: crate::core::agent_identity::hex_encode(&Sha256::digest(bytes)),
        bytes: bytes.len() as u64,
    }
}

fn validate_source_join(
    name: &str,
    bytes: &[u8],
    task_id: &str,
    receipt: &ExecutionReceiptV1,
) -> Result<(), String> {
    if name == "outcome.json" {
        let outcome: AcceptedOutcomeV1 = serde_json::from_slice(bytes)
            .map_err(|error| format!("parse outcome.json: {error}"))?;
        outcome
            .validate()
            .map_err(|error| format!("validate outcome.json: {error}"))?;
        if outcome.task_id.as_str() != task_id {
            return Err("outcome.json task_id does not match task envelope".to_string());
        }
        if receipt.outcome_ref.as_deref() != Some(outcome.outcome_id.as_str()) {
            return Err("outcome.json outcome_id does not match receipt outcome_ref".to_string());
        }
        return Ok(());
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|error| format!("parse {name}: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| format!("{name} must contain a JSON object"))?;
    if object
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        != Some(1)
    {
        return Err(format!("{name} schema_version must be 1"));
    }
    if object.get("task_id").and_then(serde_json::Value::as_str) != Some(task_id) {
        return Err(format!("{name} task_id does not match task envelope"));
    }
    Ok(())
}

fn create_without_overwrite(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| format!("invalid record filename: {}", path.display()))?;
    let staged = path.with_file_name(format!(".{name}.staged"));
    reject_symlink(&staged, "staged migration record")?;
    if staged.exists() {
        std::fs::remove_file(&staged)
            .map_err(|error| format!("remove stale staged migration record: {error}"))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged)
        .map_err(|error| format!("create {}: {error}", staged.display()))?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        let _ = std::fs::remove_file(&staged);
        return Err(format!("write {}: {error}", staged.display()));
    }
    std::fs::hard_link(&staged, path)
        .map_err(|error| format!("publish {} without overwrite: {error}", path.display()))?;
    std::fs::remove_file(&staged)
        .map_err(|error| format!("remove staged migration record: {error}"))?;
    crate::core::atomic_fs::sync_directory(path.parent().unwrap_or(Path::new(".")))
        .map_err(|error| format!("sync task/receipt migration directory: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TASK: &str = r#"{"schema_version":1,"task_id":"task-1","trace_id":"trace-1","project_id":"project-1","session_id":"session-1","agent_id":"agent-1","complexity":"low","created_at":"2026-08-01T09:00:00Z"}"#;
    const RECEIPT: &str = r#"{"schema_version":1,"receipt_id":"receipt-1","task_id":"task-1","plan_id":"plan-1","context_balance":{"original_tokens":100,"materialized_tokens":80,"delivered_tokens":60,"provider_billed_tokens":60},"fresh_input_tokens":40,"cached_input_tokens":20,"output_tokens":10,"reasoning_tokens":5,"requested_model":"legacy","selected_model":"legacy","provider":"legacy","model_calls":1,"retries":0,"latency_ms":25,"actual_cost_micros":100,"baseline_cost_micros":150,"avoided_cost_micros":50,"etpao_milli":1000,"outcome_ref":"outcome-1","decision_refs":[],"evidence_refs":[],"signature":"legacy-only"}"#;

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("task_envelope.json"), TASK).unwrap();
        std::fs::write(dir.path().join("execution_receipt.json"), RECEIPT).unwrap();
        std::fs::write(
            dir.path().join("baseline.json"),
            r#"{"schema_version":1,"task_id":"task-1"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("context_plan.json"),
            r#"{"schema_version":1,"task_id":"task-1"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("outcome.json"),
            r#"{"schema_version":1,"outcome_id":"outcome-1","task_id":"task-1","accepted":"accepted","quality_score_milli":900,"signals":{"build":"passed","tests":"passed","lint":"passed","typecheck":"not_run","completion":"passed","pr":"not_run","correction":"not_run","rollback":"not_run","retry":"not_run"},"evidence_refs":[{"schema_version":1,"kind":"QualityMeasurement","uri":"urn:evidence:outcome-1","digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","signature_status":"NotSigned"}],"observed_at":"2026-08-01T09:01:00Z"}"#,
        )
        .unwrap();
        dir
    }

    #[test]
    fn migration_preserves_sources_and_never_promotes_authority() {
        let dir = fixture();
        let first = migrate_legacy_task_receipt(dir.path()).unwrap();
        assert!(!first.already_migrated);
        assert_eq!(first.record.task_envelope, "canonical_v1_preserved");
        assert_eq!(first.record.receipt_authority, "legacy_non_authoritative");
        assert_eq!(
            first.record.required_action,
            "reexecute_with_canonical_producer"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("task_envelope.json")).unwrap(),
            TASK
        );
        let second = migrate_legacy_task_receipt(dir.path()).unwrap();
        assert!(second.already_migrated);
        assert_eq!(first.record, second.record);
    }

    #[test]
    fn rollback_removes_only_record_after_source_verification() {
        let dir = fixture();
        let result = migrate_legacy_task_receipt(dir.path()).unwrap();
        rollback_legacy_task_receipt(dir.path()).unwrap();
        assert!(!result.record_path.exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("task_envelope.json")).unwrap(),
            TASK
        );
    }

    #[test]
    fn rollback_refuses_changed_source_without_mutation() {
        let dir = fixture();
        let result = migrate_legacy_task_receipt(dir.path()).unwrap();
        std::fs::write(dir.path().join("baseline.json"), b"changed").unwrap();
        assert!(rollback_legacy_task_receipt(dir.path()).is_err());
        assert!(result.record_path.exists());
    }

    #[test]
    fn migration_recovers_stale_partial_staging_record() {
        let dir = fixture();
        let staged = dir.path().join(format!(".{RECORD_NAME}.staged"));
        std::fs::write(&staged, b"partial").unwrap();

        let migrated = migrate_legacy_task_receipt(dir.path()).unwrap();

        assert!(migrated.record_path.exists());
        assert!(!staged.exists());
    }

    #[test]
    fn all_representative_v3_task_spines_migrate_with_source_digests() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../_archive/benchmarks/efficiency/task-spine-v1/tasks");
        let mut sources: Vec<_> = std::fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.is_dir())
            .collect();
        sources.sort();
        assert_eq!(sources.len(), 10);

        for source in sources {
            let dir = tempfile::tempdir().unwrap();
            for name in [
                "task_envelope.json",
                "baseline.json",
                "context_plan.json",
                "execution_receipt.json",
                "outcome.json",
            ] {
                std::fs::copy(source.join(name), dir.path().join(name)).unwrap();
            }
            let result = migrate_legacy_task_receipt(dir.path()).unwrap();
            assert_eq!(result.record.source_artifacts.len(), 5);
            assert!(result.record.source_artifacts.iter().all(|artifact| {
                artifact.sha256.len() == 64
                    && artifact
                        .sha256
                        .chars()
                        .all(|character| character.is_ascii_hexdigit())
            }));
        }
    }

    #[test]
    fn mismatched_task_or_outcome_fails_without_record() {
        let dir = fixture();
        std::fs::write(
            dir.path().join("outcome.json"),
            r#"{"schema_version":1,"outcome_id":"outcome-1","task_id":"other","accepted":"accepted","quality_score_milli":900,"signals":{"build":"passed","tests":"passed","lint":"passed","typecheck":"not_run","completion":"passed","pr":"not_run","correction":"not_run","rollback":"not_run","retry":"not_run"},"evidence_refs":[],"observed_at":"2026-08-01T09:01:00Z"}"#,
        )
        .unwrap();
        assert!(migrate_legacy_task_receipt(dir.path()).is_err());
        assert!(!dir.path().join(RECORD_NAME).exists());
    }

    #[test]
    fn incomplete_lineage_fails_without_record() {
        let dir = fixture();
        std::fs::remove_file(dir.path().join("context_plan.json")).unwrap();
        assert!(migrate_legacy_task_receipt(dir.path()).is_err());
        assert!(!dir.path().join(RECORD_NAME).exists());
    }

    #[test]
    fn malformed_typed_outcome_fails_without_record() {
        let dir = fixture();
        std::fs::write(
            dir.path().join("outcome.json"),
            r#"{"schema_version":1,"task_id":"task-1","outcome_id":"outcome-1"}"#,
        )
        .unwrap();
        assert!(migrate_legacy_task_receipt(dir.path()).is_err());
        assert!(!dir.path().join(RECORD_NAME).exists());
    }

    #[test]
    fn oversized_artifact_fails_without_record() {
        let dir = fixture();
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(dir.path().join("baseline.json"))
            .unwrap();
        file.set_len(MAX_ARTIFACT_BYTES + 1).unwrap();
        assert!(migrate_legacy_task_receipt(dir.path()).is_err());
        assert!(!dir.path().join(RECORD_NAME).exists());
    }

    #[test]
    fn conflicting_record_is_not_overwritten() {
        let dir = fixture();
        let path = dir.path().join(RECORD_NAME);
        std::fs::write(&path, b"owned").unwrap();
        assert!(migrate_legacy_task_receipt(dir.path()).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"owned");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_source_is_rejected() {
        use std::os::unix::fs::symlink;
        let dir = fixture();
        let target = dir.path().join("real-task.json");
        std::fs::rename(dir.path().join("task_envelope.json"), &target).unwrap();
        symlink(&target, dir.path().join("task_envelope.json")).unwrap();
        assert!(migrate_legacy_task_receipt(dir.path()).is_err());
        assert!(!dir.path().join(RECORD_NAME).exists());
    }
}
