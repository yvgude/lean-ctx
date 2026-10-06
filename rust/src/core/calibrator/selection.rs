//! Explicit, evidence-qualified manual profile selection.
//!
//! The record is local and portable. It makes no learned ranking or automatic
//! promotion decision: an operator chooses when to write and apply it.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::core::benchmark_spec::profile_bridge;
use crate::core::benchmark_spec::types::{BenchmarkResult, BenchmarkSpecV1, BenchmarkSummary};
use crate::core::canonical::canonical_serialize;
use crate::core::config;
use crate::core::profiles;

const SCHEMA_VERSION: u32 = 1;
const MAX_EVIDENCE_BUNDLE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_EVIDENCE_BUNDLE_ENTRIES: usize = 1_024;
const MAX_EVIDENCE_BUNDLE_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SelectionEvidenceArmV1 {
    pub candidate_id: String,
    pub profile_name: String,
    pub profile_hash: String,
    pub spec_id: String,
    pub spec_version: String,
    pub spec_digest: String,
    pub result_digest: String,
    pub receipt_refs: Vec<String>,
}

/// Immutable selection constraints captured with the candidate evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SelectionPolicyV1 {
    pub quality_floor: f64,
    pub requires_evaluated_quality: bool,
    pub requires_complete_receipts: bool,
}

/// Deterministic, human-readable basis for the explicitly selected candidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SelectionRationaleV1 {
    pub kind: String,
    pub selected_cost_per_task: f64,
    pub selected_mean_quality: f64,
    pub selected_mean_latency_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManualSelectionRecordV1 {
    pub schema_version: u32,
    pub selection_id: String,
    pub previous_profile: String,
    pub selected_profile: String,
    pub selected_candidate_id: String,
    pub evidence_bundle_sha256: String,
    pub policy: SelectionPolicyV1,
    pub rationale: SelectionRationaleV1,
    pub evidence_arms: Vec<SelectionEvidenceArmV1>,
}

impl ManualSelectionRecordV1 {
    pub(crate) fn create(
        previous_profile: String,
        selected_profile: String,
        selected_candidate_id: String,
        evidence_bundle_sha256: String,
        policy: SelectionPolicyV1,
        rationale: SelectionRationaleV1,
        mut evidence_arms: Vec<SelectionEvidenceArmV1>,
    ) -> Result<Self, String> {
        evidence_arms.sort_by(|left, right| left.candidate_id.cmp(&right.candidate_id));
        for arm in &mut evidence_arms {
            arm.receipt_refs.sort();
            arm.receipt_refs.dedup();
        }
        let mut record = Self {
            schema_version: SCHEMA_VERSION,
            selection_id: String::new(),
            previous_profile,
            selected_profile,
            selected_candidate_id,
            evidence_bundle_sha256,
            policy,
            rationale,
            evidence_arms,
        };
        record.validate()?;
        record.selection_id = selection_id(&record);
        record.validate()?;
        Ok(record)
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(format!(
                "unsupported manual selection schema {}",
                self.schema_version
            ));
        }
        for (label, value) in [
            ("previous_profile", &self.previous_profile),
            ("selected_profile", &self.selected_profile),
            ("selected_candidate_id", &self.selected_candidate_id),
            ("evidence_bundle_sha256", &self.evidence_bundle_sha256),
        ] {
            if value.trim().is_empty() {
                return Err(format!("manual selection {label} is required"));
            }
        }
        if !is_hex_digest(&self.evidence_bundle_sha256) {
            return Err("manual selection evidence_bundle_sha256 must be a SHA-256 digest".into());
        }
        if !self.policy.quality_floor.is_finite()
            || !(0.0..=1.0).contains(&self.policy.quality_floor)
            || !self.policy.requires_evaluated_quality
            || !self.policy.requires_complete_receipts
        {
            return Err(
                "manual selection policy must require evaluated receipt-linked quality".into(),
            );
        }
        if !matches!(
            self.rationale.kind.as_str(),
            "lowest-cost-above-floor" | "only-candidate"
        ) || !self.rationale.selected_cost_per_task.is_finite()
            || self.rationale.selected_cost_per_task < 0.0
            || !self.rationale.selected_mean_quality.is_finite()
            || !(0.0..=1.0).contains(&self.rationale.selected_mean_quality)
            || !self.rationale.selected_mean_latency_ms.is_finite()
            || self.rationale.selected_mean_latency_ms < 0.0
        {
            return Err("manual selection rationale is invalid".into());
        }
        if self.evidence_arms.len() < 2 {
            return Err("manual selection requires at least two evidence arms".into());
        }

        let mut candidate_ids = std::collections::BTreeSet::new();
        let mut selected = false;
        for arm in &self.evidence_arms {
            for (label, value) in [
                ("candidate_id", &arm.candidate_id),
                ("profile_name", &arm.profile_name),
                ("profile_hash", &arm.profile_hash),
                ("spec_id", &arm.spec_id),
                ("spec_version", &arm.spec_version),
                ("spec_digest", &arm.spec_digest),
                ("result_digest", &arm.result_digest),
            ] {
                if value.trim().is_empty() {
                    return Err(format!("manual selection evidence {label} is required"));
                }
            }
            if !candidate_ids.insert(&arm.candidate_id) {
                return Err(format!(
                    "manual selection evidence candidate '{}' is duplicated",
                    arm.candidate_id
                ));
            }
            if !is_blake3_digest(&arm.spec_digest) || !is_blake3_digest(&arm.result_digest) {
                return Err("manual selection evidence digests must be BLAKE3 digests".into());
            }
            if arm.receipt_refs.is_empty()
                || !arm.receipt_refs.windows(2).all(|pair| pair[0] < pair[1])
                || arm.receipt_refs.iter().any(|reference| {
                    crate::core::agent_connector::receipt::canonical_receipt_digest(reference)
                        .is_err()
                })
            {
                return Err(
                    "manual selection evidence requires canonical receipt references".into(),
                );
            }
            if arm.candidate_id == self.selected_candidate_id {
                selected = arm.profile_name == self.selected_profile;
            }
        }
        if !selected {
            return Err("selected profile must match an evidence-qualified candidate".into());
        }
        if !self.selection_id.is_empty() && self.selection_id != selection_id(self) {
            return Err("manual selection record digest does not match its contents".into());
        }
        Ok(())
    }
}

pub(crate) fn write_record(path: &Path, record: &ManualSelectionRecordV1) -> Result<(), String> {
    record.validate()?;
    let bytes = canonical_serialize(record);
    crate::core::atomic_fs::write_bytes_with_fallback(path, &bytes, None)
}

pub(crate) fn read_record(path: &Path) -> Result<ManualSelectionRecordV1, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("read selection record: {error}"))?;
    let record: ManualSelectionRecordV1 = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse selection record: {error}"))?;
    record.validate()?;
    Ok(record)
}

/// Apply an explicitly written selection to the global profile config.
///
/// The bundle is required at apply time instead of trusting a digest copied
/// into a local record. This keeps a later apply fail-closed when evidence is
/// unavailable, changed, unsigned, or no longer matches the recorded arms.
pub(crate) fn apply_record(
    record: &ManualSelectionRecordV1,
    evidence_bundle: &Path,
) -> Result<(), String> {
    record.validate()?;
    verify_evidence_bundle(record, evidence_bundle)?;
    reject_environment_override()?;
    let current_profile =
        config::setter::current_value("profile").unwrap_or_else(|| "coder".to_string());
    if current_profile != record.previous_profile {
        return Err(
            "refusing apply: configured profile no longer matches this selection record's previous profile"
                .into(),
        );
    }
    let selected_profile = profiles::load_profile(&record.selected_profile).ok_or_else(|| {
        format!(
            "selected profile '{}' no longer exists",
            record.selected_profile
        )
    })?;
    let selected_evidence = record
        .evidence_arms
        .iter()
        .find(|arm| arm.candidate_id == record.selected_candidate_id)
        .expect("record validation requires an evidence-qualified selected candidate");
    if profile_bridge::profile_hash(&selected_profile) != selected_evidence.profile_hash {
        return Err(format!(
            "selected profile '{}' no longer matches the evidence-qualified profile hash",
            record.selected_profile
        ));
    }
    config::setter::set_by_key("profile", &record.selected_profile)
        .map(|_| ())
        .map_err(|error| format!("apply selected profile: {error}"))
}

fn verify_evidence_bundle(record: &ManualSelectionRecordV1, path: &Path) -> Result<(), String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("read selection evidence bundle {}: {error}", path.display()))?;
    let size = u64::try_from(bytes.len()).expect("usize fits in u64");
    if size > MAX_EVIDENCE_BUNDLE_BYTES {
        return Err(format!(
            "selection evidence bundle has {size} bytes; limit is {MAX_EVIDENCE_BUNDLE_BYTES}"
        ));
    }
    let actual_sha256 = sha256_hex(&bytes);
    if !actual_sha256.eq_ignore_ascii_case(&record.evidence_bundle_sha256) {
        return Err("selection evidence bundle SHA-256 does not match the selection record".into());
    }

    let files = read_bundle_files(&bytes)?;
    let manifest: serde_json::Value = serde_json::from_slice(
        files
            .get("manifest.json")
            .ok_or("selection evidence bundle is missing manifest.json")?,
    )
    .map_err(|error| format!("parse selection evidence manifest: {error}"))?;
    verify_bundle_manifest(&manifest, &files)?;

    let mut claimed_paths = BTreeSet::new();
    for arm in &record.evidence_arms {
        let path = files
            .keys()
            .filter(|path| path.starts_with("arms/") && path.ends_with("/benchmark-spec.json"))
            .find_map(|spec_path| {
                if claimed_paths.contains(spec_path) {
                    return None;
                }
                let prefix = spec_path.trim_end_matches("benchmark-spec.json");
                let result_path = format!("{prefix}benchmark-result.json");
                let (Some(spec_bytes), Some(result_bytes)) =
                    (files.get(spec_path), files.get(&result_path))
                else {
                    return None;
                };
                let spec: BenchmarkSpecV1 = serde_json::from_slice(spec_bytes).ok()?;
                let result: BenchmarkResult = serde_json::from_slice(result_bytes).ok()?;
                (matches_selection_arm(arm, &spec, &result, record.policy.quality_floor)
                    && has_receipt_artifacts(&files, arm))
                .then_some(spec_path.clone())
            })
            .ok_or_else(|| {
                format!(
                    "selection evidence bundle has no matching evaluated arm for candidate '{}'",
                    arm.candidate_id
                )
            })?;
        claimed_paths.insert(path);
    }
    Ok(())
}

fn read_bundle_files(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, String> {
    read_bundle_files_with_limits(
        bytes,
        MAX_EVIDENCE_BUNDLE_ENTRIES,
        MAX_EVIDENCE_BUNDLE_BYTES,
        MAX_EVIDENCE_BUNDLE_TOTAL_BYTES,
    )
}

fn read_bundle_files_with_limits(
    bytes: &[u8],
    max_entries: usize,
    max_entry_bytes: u64,
    max_total_bytes: u64,
) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|error| format!("selection evidence bundle is not a ZIP archive: {error}"))?;
    if archive.len() > max_entries {
        return Err(format!(
            "selection evidence bundle has {} entries; limit is {max_entries}",
            archive.len()
        ));
    }

    let mut files = BTreeMap::new();
    let mut total_bytes = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("read selection evidence entry {index}: {error}"))?;
        let path = entry.name().to_owned();
        if !is_safe_archive_path(&path) || entry.is_dir() {
            return Err(format!(
                "selection evidence bundle has unsafe entry '{path}'"
            ));
        }
        let declared_size = entry.size();
        if declared_size > max_entry_bytes {
            return Err(format!(
                "selection evidence entry '{path}' exceeds size limit"
            ));
        }
        total_bytes = total_bytes
            .checked_add(declared_size)
            .filter(|total| *total <= max_total_bytes)
            .ok_or("selection evidence bundle exceeds total decompressed size limit")?;
        let mut contents = Vec::with_capacity(
            usize::try_from(declared_size).expect("evidence bundle size fits in usize"),
        );
        entry
            .by_ref()
            .take(declared_size.saturating_add(1))
            .read_to_end(&mut contents)
            .map_err(|error| format!("read selection evidence entry '{path}': {error}"))?;
        if u64::try_from(contents.len()).expect("usize fits in u64") != declared_size {
            return Err(format!(
                "selection evidence entry '{path}' has an invalid size"
            ));
        }
        if files.insert(path.clone(), contents).is_some() {
            return Err(format!(
                "selection evidence bundle duplicates entry '{path}'"
            ));
        }
    }
    Ok(files)
}

fn verify_bundle_manifest(
    manifest: &serde_json::Value,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<(), String> {
    if manifest["bundle"] != "evidence-bundle"
        || manifest["version"] != 1
        || manifest["run"]["kind"] != "benchmark-comparison-evidence-v1"
    {
        return Err(
            "selection evidence bundle is not a benchmark comparison evidence-bundle v1".into(),
        );
    }
    let listed = manifest["files"]
        .as_array()
        .ok_or("selection evidence manifest files must be an array")?;
    if listed.is_empty() {
        return Err("selection evidence manifest lists no files".into());
    }
    let mut listed_paths = BTreeSet::new();
    for item in listed {
        let path = item["path"]
            .as_str()
            .filter(|path| is_safe_archive_path(path))
            .ok_or("selection evidence manifest contains an unsafe file path")?;
        let expected_hash = item["sha256"]
            .as_str()
            .filter(|hash| is_hex_digest(hash))
            .ok_or("selection evidence manifest contains an invalid SHA-256 hash")?;
        if !listed_paths.insert(path) {
            return Err(format!(
                "selection evidence manifest duplicates file '{path}'"
            ));
        }
        let bytes = files
            .get(path)
            .ok_or_else(|| format!("selection evidence bundle is missing listed file '{path}'"))?;
        if !sha256_hex(bytes).eq_ignore_ascii_case(expected_hash) {
            return Err(format!("selection evidence hash mismatch for '{path}'"));
        }
    }
    if files
        .keys()
        .any(|path| path != "manifest.json" && !listed_paths.contains(path.as_str()))
    {
        return Err("selection evidence bundle contains an unlisted payload file".into());
    }

    let signing = manifest["signing"]
        .as_object()
        .ok_or("selection evidence manifest is missing signing metadata")?;
    if signing.get("algorithm").and_then(serde_json::Value::as_str) != Some("ed25519") {
        return Err("selection evidence manifest does not use Ed25519 signing".into());
    }
    let public_key = signing
        .get("public_key")
        .and_then(serde_json::Value::as_str)
        .ok_or("selection evidence manifest is missing its public key")?;
    let signed_digest = signing
        .get("signed_digest")
        .and_then(serde_json::Value::as_str)
        .filter(|digest| is_hex_digest(digest))
        .ok_or("selection evidence manifest has an invalid signed digest")?;
    let signature = signing
        .get("signature")
        .and_then(serde_json::Value::as_str)
        .ok_or("selection evidence manifest is missing its signature")?;

    let mut unsigned = manifest.clone();
    unsigned["signing"]["signed_digest"] = serde_json::Value::String(String::new());
    unsigned["signing"]["signature"] = serde_json::Value::String(String::new());
    let digest = sha256_hex(&canonical_serialize(&unsigned));
    if !digest.eq_ignore_ascii_case(signed_digest) {
        return Err("selection evidence manifest signed digest does not recompute".into());
    }
    let public_key = crate::core::agent_identity::hex_decode(public_key)
        .map_err(|_| "selection evidence manifest has an invalid public key".to_string())?;
    let signature = crate::core::agent_identity::hex_decode(signature)
        .map_err(|_| "selection evidence manifest has an invalid signature".to_string())?;
    if !crate::core::agent_identity::verify_signature(&public_key, digest.as_bytes(), &signature) {
        return Err("selection evidence manifest signature does not verify".into());
    }
    Ok(())
}

fn matches_selection_arm(
    arm: &SelectionEvidenceArmV1,
    spec: &BenchmarkSpecV1,
    result: &BenchmarkResult,
    policy_quality_floor: f64,
) -> bool {
    if spec.validate_evidence().is_err()
        || spec.id != arm.spec_id
        || spec.version != arm.spec_version
        || format!(
            "blake3:{}",
            blake3::hash(&canonical_serialize(spec)).to_hex()
        ) != arm.spec_digest
        || format!(
            "blake3:{}",
            blake3::hash(&canonical_serialize(result)).to_hex()
        ) != arm.result_digest
        || result.spec_id != spec.id
        || result.spec_version != spec.version
        || result.profile_hash != arm.profile_hash
        || spec.configuration.profile_hash.as_deref() != Some(&arm.profile_hash)
    {
        return false;
    }
    let expected_summary =
        BenchmarkSummary::from_outcomes(&result.outcomes, spec.configuration.quality_floor);
    if canonical_serialize(&result.summary) != canonical_serialize(&expected_summary)
        || !result.summary.quality_evaluated
        || !result.summary.quality_floor_met
        || !result.summary.receipt_evidence_complete
        || result.summary.mean_quality < policy_quality_floor
        || result
            .outcomes
            .iter()
            .any(|outcome| outcome.error.is_some())
    {
        return false;
    }
    let receipt_refs = result
        .outcomes
        .iter()
        .filter_map(|outcome| outcome.execution_receipt_ref.as_deref())
        .collect::<BTreeSet<_>>();
    receipt_refs.len() == arm.receipt_refs.len()
        && receipt_refs
            .iter()
            .zip(&arm.receipt_refs)
            .all(|(actual, expected)| *actual == expected)
}

fn has_receipt_artifacts(files: &BTreeMap<String, Vec<u8>>, arm: &SelectionEvidenceArmV1) -> bool {
    arm.receipt_refs.iter().all(|reference| {
        crate::core::agent_connector::receipt::verify_bundled_receipt(reference, files)
            .unwrap_or(false)
    })
}

fn is_safe_archive_path(path: &str) -> bool {
    let path = Path::new(path);
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    crate::core::agent_identity::hex_encode(&hasher.finalize())
}

/// Restore the profile that was active when this record was created.
pub(crate) fn rollback_record(record: &ManualSelectionRecordV1) -> Result<(), String> {
    record.validate()?;
    reject_environment_override()?;
    if config::setter::current_value("profile").as_deref() != Some(&record.selected_profile) {
        return Err(
            "refusing rollback: configured profile no longer matches this selection record".into(),
        );
    }
    if profiles::load_profile(&record.previous_profile).is_none() {
        return Err(format!(
            "previous profile '{}' no longer exists",
            record.previous_profile
        ));
    }
    config::setter::set_by_key("profile", &record.previous_profile)
        .map(|_| ())
        .map_err(|error| format!("restore previous profile: {error}"))
}

fn selection_id(record: &ManualSelectionRecordV1) -> String {
    let mut unsigned = record.clone();
    unsigned.selection_id.clear();
    format!(
        "selection-{}",
        blake3::hash(&canonical_serialize(&unsigned)).to_hex()
    )
}

fn reject_environment_override() -> Result<(), String> {
    if std::env::var("LEAN_CTX_PROFILE").is_ok_and(|value| !value.trim().is_empty()) {
        return Err("LEAN_CTX_PROFILE overrides config; unset it before apply or rollback".into());
    }
    Ok(())
}

fn is_hex_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_blake3_digest(value: &str) -> bool {
    value.strip_prefix("blake3:").is_some_and(is_hex_digest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent_connector::receipt::record_provider_receipt;
    use crate::core::agent_connector::traits::TaskRequest;
    use crate::core::benchmark_spec::evidence::{
        ArtifactRedaction, EvidenceArm, write_comparison_bundle,
    };
    use crate::core::benchmark_spec::types::{
        BenchmarkConfiguration, BenchmarkEvaluation, BenchmarkKind, BenchmarkOutcome,
        BenchmarkSuite, BenchmarkTask, EvaluationSpecV1, TaskKind,
    };
    use std::io::Write;
    use std::path::PathBuf;

    fn arm(candidate_id: &str, profile_name: &str) -> SelectionEvidenceArmV1 {
        SelectionEvidenceArmV1 {
            candidate_id: candidate_id.into(),
            profile_name: profile_name.into(),
            profile_hash: "profile-hash".into(),
            spec_id: "workload".into(),
            spec_version: "1.0.0".into(),
            spec_digest: format!("blake3:{}", "a".repeat(64)),
            result_digest: format!("blake3:{}", "b".repeat(64)),
            receipt_refs: vec![format!("id:sha256:{}", "d".repeat(64))],
        }
    }

    fn policy() -> SelectionPolicyV1 {
        SelectionPolicyV1 {
            quality_floor: 0.95,
            requires_evaluated_quality: true,
            requires_complete_receipts: true,
        }
    }

    fn rationale() -> SelectionRationaleV1 {
        SelectionRationaleV1 {
            kind: "lowest-cost-above-floor".into(),
            selected_cost_per_task: 0.1,
            selected_mean_quality: 0.99,
            selected_mean_latency_ms: 10.0,
        }
    }

    fn evaluated_spec(id: &str, profile_hash: String) -> BenchmarkSpecV1 {
        BenchmarkSpecV1 {
            id: id.into(),
            version: "1.0.0".into(),
            name: format!("{id} evidence fixture"),
            description: "Deterministic selection evidence fixture".into(),
            suite: BenchmarkSuite {
                kind: BenchmarkKind::TaskScore,
                tasks: vec![BenchmarkTask {
                    id: "task-1".into(),
                    name: "Task".into(),
                    description: "Return the expected answer".into(),
                    kind: TaskKind::Custom,
                    timeout_ms: Some(1_000),
                    evaluation: Some(EvaluationSpecV1::Qa {
                        answers: vec!["answer".into()],
                        minimum_f1: 1.0,
                    }),
                }],
            },
            configuration: BenchmarkConfiguration {
                profile_hash: Some(profile_hash),
                agent: Some("codex".into()),
                model: Some("test-model".into()),
                runtime_version: "test".into(),
                repeats: 1,
                quality_floor: 0.95,
            },
            created_at: "2026-08-22T00:00:00Z".into(),
        }
    }

    fn evaluated_result(spec: &BenchmarkSpecV1, receipt_ref: String) -> BenchmarkResult {
        let outcomes = vec![BenchmarkOutcome {
            task_id: "task-1".into(),
            passed: true,
            cost_usd: 0.000123,
            quality_score: 1.0,
            latency_ms: 42,
            tokens_input: 100,
            tokens_output: 40,
            error: None,
            evaluation: Some(BenchmarkEvaluation {
                evaluator_id: "qa-f1-v1".into(),
                metric: "f1".into(),
                score: 1.0,
                passed: true,
                detail: "fixture".into(),
                output_digest: "digest".into(),
            }),
            execution_receipt_ref: Some(receipt_ref),
            capability_observation: None,
        }];
        BenchmarkResult {
            spec_id: spec.id.clone(),
            spec_version: spec.version.clone(),
            profile_hash: spec.configuration.profile_hash.clone().unwrap(),
            agent: "codex".into(),
            model: "test-model".into(),
            runtime_version: "test".into(),
            summary: BenchmarkSummary::from_outcomes(&outcomes, spec.configuration.quality_floor),
            outcomes,
            completed_at: "0".into(),
        }
    }

    fn evidence_arm(
        candidate_id: &str,
        profile_name: &str,
        spec: &BenchmarkSpecV1,
        result: &BenchmarkResult,
    ) -> SelectionEvidenceArmV1 {
        SelectionEvidenceArmV1 {
            candidate_id: candidate_id.into(),
            profile_name: profile_name.into(),
            profile_hash: result.profile_hash.clone(),
            spec_id: spec.id.clone(),
            spec_version: spec.version.clone(),
            spec_digest: format!(
                "blake3:{}",
                blake3::hash(&canonical_serialize(spec)).to_hex()
            ),
            result_digest: format!(
                "blake3:{}",
                blake3::hash(&canonical_serialize(result)).to_hex()
            ),
            receipt_refs: result
                .outcomes
                .iter()
                .filter_map(|outcome| outcome.execution_receipt_ref.clone())
                .collect(),
        }
    }

    fn verified_record(directory: &Path) -> (ManualSelectionRecordV1, PathBuf) {
        let request = TaskRequest {
            id: "task-1".into(),
            prompt: "answer".into(),
            working_dir: PathBuf::from("."),
            timeout_ms: 1_000,
            model: Some("test-model".into()),
            max_turns: None,
            profile_name: Some("exploration".into()),
            profile_hash: None,
            delivery_profile: None,
        };
        let receipt = record_provider_receipt(
            "codex",
            "openai",
            &request,
            r#"{"model":"test-model","usage":{"input_tokens":100,"output_tokens":40},"total_cost_usd":"0.000123"}"#,
            42,
        )
        .expect("fixture must create a locally verified receipt");
        let coder = profiles::load_profile("coder").expect("built-in coder profile");
        let exploration =
            profiles::load_profile("exploration").expect("built-in exploration profile");
        let baseline_spec = evaluated_spec("baseline", profile_bridge::profile_hash(&coder));
        let selected_spec = evaluated_spec("treatment", profile_bridge::profile_hash(&exploration));
        let baseline_result = evaluated_result(&baseline_spec, receipt.reference.clone());
        let selected_result = evaluated_result(&selected_spec, receipt.reference);
        let bundle_path = directory.join("selection-evidence.zip");
        let bundle = write_comparison_bundle(
            &bundle_path,
            &[
                EvidenceArm {
                    label: "baseline".into(),
                    spec: &baseline_spec,
                    result: &baseline_result,
                },
                EvidenceArm {
                    label: "treatment".into(),
                    spec: &selected_spec,
                    result: &selected_result,
                },
            ],
            ArtifactRedaction::SelfContained,
        )
        .expect("fixture must create a signed evidence bundle");
        let record = ManualSelectionRecordV1::create(
            "coder".into(),
            "exploration".into(),
            "treatment".into(),
            bundle.sha256,
            policy(),
            rationale(),
            vec![
                evidence_arm("baseline", "coder", &baseline_spec, &baseline_result),
                evidence_arm("treatment", "exploration", &selected_spec, &selected_result),
            ],
        )
        .expect("fixture selection record");
        (record, bundle_path)
    }

    #[test]
    fn bundled_receipts_require_complete_untampered_canonical_evidence() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let directory = tempfile::tempdir().unwrap();
        let (record, bundle_path) = verified_record(directory.path());
        let files = read_bundle_files(&std::fs::read(bundle_path).unwrap()).unwrap();
        let arm = &record.evidence_arms[0];
        assert!(has_receipt_artifacts(&files, arm));
        let artifact_paths: Vec<_> = files
            .keys()
            .filter(|path| path.starts_with("execution-receipts/"))
            .cloned()
            .collect();
        assert!(artifact_paths.len() >= 3);
        for path in artifact_paths {
            let mut missing = files.clone();
            missing.remove(&path);
            assert!(!has_receipt_artifacts(&missing, arm), "missing {path}");
            let mut tampered = files.clone();
            tampered.get_mut(&path).unwrap().push(b' ');
            assert!(!has_receipt_artifacts(&tampered, arm), "tampered {path}");
        }
        for reference in ["receipt:legacy", "id:sha256:../escape", "id:sha256:abc"] {
            let mut invalid = arm.clone();
            invalid.receipt_refs = vec![reference.into()];
            assert!(!has_receipt_artifacts(&files, &invalid));
        }
    }

    #[test]
    fn apply_requires_the_exact_signed_evidence_bundle_before_mutating_config() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let directory = tempfile::tempdir().unwrap();
        config::setter::set_by_key("profile", "coder").unwrap();
        let (record, bundle_path) = verified_record(directory.path());

        apply_record(&record, &bundle_path).expect("verified evidence must permit apply");
        assert_eq!(
            config::setter::current_value("profile").as_deref(),
            Some("exploration")
        );
        rollback_record(&record).expect("rollback remains available after verified apply");

        std::fs::write(&bundle_path, b"tampered").unwrap();
        let error = apply_record(&record, &bundle_path).unwrap_err();
        assert!(error.contains("SHA-256"));
        assert_eq!(
            config::setter::current_value("profile").as_deref(),
            Some("coder")
        );
    }

    #[test]
    fn bundle_replay_rejects_a_record_with_unmatched_arm_digests() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let directory = tempfile::tempdir().unwrap();
        let (record, bundle_path) = verified_record(directory.path());
        let mut arms = record.evidence_arms.clone();
        arms[0].result_digest = format!("blake3:{}", "f".repeat(64));
        let forged = ManualSelectionRecordV1::create(
            record.previous_profile.clone(),
            record.selected_profile.clone(),
            record.selected_candidate_id.clone(),
            record.evidence_bundle_sha256.clone(),
            record.policy.clone(),
            record.rationale.clone(),
            arms,
        )
        .unwrap();

        let error = verify_evidence_bundle(&forged, &bundle_path).unwrap_err();
        assert!(error.contains("no matching evaluated arm"));
    }

    #[test]
    fn apply_rejects_stale_config_after_verifying_evidence() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let directory = tempfile::tempdir().unwrap();
        let (record, bundle_path) = verified_record(directory.path());
        config::setter::set_by_key("profile", "exploration").unwrap();

        let error = apply_record(&record, &bundle_path).unwrap_err();

        assert!(error.contains("previous profile"));
        assert_eq!(
            config::setter::current_value("profile").as_deref(),
            Some("exploration")
        );
    }

    #[test]
    fn bundle_reader_rejects_total_decompressed_size_over_its_budget() {
        let mut bytes = Vec::new();
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for path in ["one.json", "two.json"] {
            writer.start_file(path, options).unwrap();
            writer.write_all(b"123456").unwrap();
        }
        writer.finish().unwrap();

        let error = read_bundle_files_with_limits(&bytes, 2, 6, 10).unwrap_err();
        assert!(error.contains("total decompressed size"));
    }

    #[test]
    fn canonical_record_is_stable_across_evidence_order() {
        let first = ManualSelectionRecordV1::create(
            "coder".into(),
            "exploration".into(),
            "profile-exploration".into(),
            "c".repeat(64),
            policy(),
            rationale(),
            vec![
                arm("profile-exploration", "exploration"),
                arm("profile-coder", "coder"),
            ],
        )
        .unwrap();
        let second = ManualSelectionRecordV1::create(
            "coder".into(),
            "exploration".into(),
            "profile-exploration".into(),
            "c".repeat(64),
            policy(),
            rationale(),
            vec![
                arm("profile-coder", "coder"),
                arm("profile-exploration", "exploration"),
            ],
        )
        .unwrap();

        assert_eq!(first.selection_id, second.selection_id);
        assert_eq!(canonical_serialize(&first), canonical_serialize(&second));
    }

    #[test]
    fn record_rejects_unlinked_or_mismatched_selection_evidence() {
        let mut invalid = arm("profile-exploration", "exploration");
        invalid.receipt_refs.clear();
        assert!(
            ManualSelectionRecordV1::create(
                "coder".into(),
                "exploration".into(),
                "profile-exploration".into(),
                "c".repeat(64),
                policy(),
                rationale(),
                vec![invalid, arm("profile-coder", "coder")],
            )
            .is_err()
        );
    }

    #[test]
    fn record_round_trip_is_atomic_and_portable() {
        let record = ManualSelectionRecordV1::create(
            "coder".into(),
            "exploration".into(),
            "profile-exploration".into(),
            "c".repeat(64),
            policy(),
            rationale(),
            vec![
                arm("profile-exploration", "exploration"),
                arm("profile-coder", "coder"),
            ],
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("selection.json");

        write_record(&path, &record).unwrap();
        let restored = read_record(&path).unwrap();

        assert_eq!(restored.selection_id, record.selection_id);
        assert_eq!(canonical_serialize(&restored), canonical_serialize(&record));
    }

    #[test]
    fn unavailable_evidence_leaves_config_unchanged_and_rollback_is_available() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let directory = tempfile::tempdir().unwrap();
        config::setter::set_by_key("profile", "coder").unwrap();
        let record = ManualSelectionRecordV1::create(
            "coder".into(),
            "exploration".into(),
            "profile-exploration".into(),
            "c".repeat(64),
            policy(),
            rationale(),
            vec![
                arm("profile-exploration", "exploration"),
                arm("profile-coder", "coder"),
            ],
        )
        .unwrap();

        let evidence_path = directory.path().join("unavailable-evidence.zip");
        assert!(apply_record(&record, &evidence_path).is_err());
        assert_eq!(
            config::setter::current_value("profile").as_deref(),
            Some("coder")
        );
        config::setter::set_by_key("profile", "exploration").unwrap();
        assert_eq!(
            config::setter::current_value("profile").as_deref(),
            Some("exploration")
        );
        rollback_record(&record).unwrap();
        assert_eq!(
            config::setter::current_value("profile").as_deref(),
            Some("coder")
        );
    }

    #[test]
    fn apply_rejects_unavailable_evidence_before_checking_stale_config() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let directory = tempfile::tempdir().unwrap();
        config::setter::set_by_key("profile", "exploration").unwrap();
        let record = ManualSelectionRecordV1::create(
            "coder".into(),
            "exploration".into(),
            "profile-exploration".into(),
            "c".repeat(64),
            policy(),
            rationale(),
            vec![
                arm("profile-exploration", "exploration"),
                arm("profile-coder", "coder"),
            ],
        )
        .unwrap();

        assert!(
            apply_record(&record, &directory.path().join("unavailable-evidence.zip"))
                .unwrap_err()
                .contains("read selection evidence bundle")
        );
        assert_eq!(
            config::setter::current_value("profile").as_deref(),
            Some("exploration")
        );
    }
}
