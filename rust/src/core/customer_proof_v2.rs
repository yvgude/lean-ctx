//! V2 customer-proof assembly from already materialized local observations.
//!
//! This is deliberately not a second verifier and does not reinterpret the
//! frozen V1 ZIP contract. It builds canonical V2 JSON plus a bounded local
//! sidecar directory; `leanctx-verify v2` remains the independent authority
//! for proof eligibility.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const SCHEMA_VERSION: &str = "leanctx.customer-proof-evidence-bundle/v2";
const MAX_ITEMS: usize = 128;
const MAX_ITEM_BYTES: usize = 8 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;

/// The V2 inventory kinds. Inputs are materialized bytes, never references
/// back into an ambient data directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CustomerProofArtifactKind {
    ArmReceipt,
    ReceiptPredecessor,
    QualityMeasurement,
    ReplayInput,
    ReplayResult,
    RunMetadata,
    ClaimBasis,
    FrozenAuditBundleV1,
    TaskEnvelope,
    ExecutionPlan,
    EngineInvocation,
    EngineObservation,
    AcceptedOutcome,
    Measurement,
    Assumption,
    Formula,
    PriceTable,
    Invoice,
    AcceptanceEvidence,
}

impl CustomerProofArtifactKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::ArmReceipt => "arm_receipt",
            Self::ReceiptPredecessor => "receipt_predecessor",
            Self::QualityMeasurement => "quality_measurement",
            Self::ReplayInput => "replay_input",
            Self::ReplayResult => "replay_result",
            Self::RunMetadata => "run_metadata",
            Self::ClaimBasis => "claim_basis",
            Self::FrozenAuditBundleV1 => "frozen_audit_bundle_v1",
            Self::TaskEnvelope => "task_envelope",
            Self::ExecutionPlan => "execution_plan",
            Self::EngineInvocation => "engine_invocation",
            Self::EngineObservation => "engine_observation",
            Self::AcceptedOutcome => "accepted_outcome",
            Self::Measurement => "measurement",
            Self::Assumption => "assumption",
            Self::Formula => "formula",
            Self::PriceTable => "price_table",
            Self::Invoice => "invoice",
            Self::AcceptanceEvidence => "acceptance_evidence",
        }
    }
}

/// The redaction declaration attached to one artifact. It is evidence
/// provenance; assembly never silently claims redaction happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CustomerProofRedactionClass {
    None,
    Pseudonymized,
    MetadataOnly,
    ContentRemoved,
    SecretRemoved,
    Aggregated,
}

impl CustomerProofRedactionClass {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Pseudonymized => "pseudonymized",
            Self::MetadataOnly => "metadata_only",
            Self::ContentRemoved => "content_removed",
            Self::SecretRemoved => "secret_removed",
            Self::Aggregated => "aggregated",
        }
    }
}

/// One present sidecar artifact. The assembler derives its content reference
/// and inventory digest from these exact bytes.
#[derive(Debug, Clone)]
pub(crate) struct CustomerProofArtifact {
    pub(crate) kind: CustomerProofArtifactKind,
    pub(crate) path: String,
    pub(crate) redaction_class: CustomerProofRedactionClass,
    pub(crate) bytes: Vec<u8>,
}

impl CustomerProofArtifact {
    #[must_use]
    pub(crate) fn digest(&self) -> String {
        sha256_digest(&self.bytes)
    }

    #[must_use]
    pub(crate) fn reference(&self) -> String {
        format!("id:{}", self.digest())
    }
}

/// Fields supplied by the caller before inventory and signing are assembled.
///
/// This internal adapter intentionally keeps the evolving body as JSON until
/// Profile/Kit contracts become public in W5. The assembler owns the stable
/// canonical identity, inventory, and signing fields now and rejects callers
/// attempting to inject them.
#[derive(Debug, Clone)]
pub(crate) struct CustomerProofDraftV2 {
    pub(crate) created_at: String,
    pub(crate) status: String,
    pub(crate) subject: Value,
    pub(crate) matched_arms: Value,
    pub(crate) quality: Value,
    pub(crate) replay: Value,
    pub(crate) limitations: Value,
    pub(crate) redaction: Value,
    pub(crate) claims: Value,
}

/// Only a customer-configured or out-of-band key can be used to assemble a
/// customer-proof candidate. A local identity alone is not sufficient.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CustomerProofTrustBasis {
    CustomerConfigured,
    OutOfBand,
}

impl CustomerProofTrustBasis {
    fn as_str(self) -> &'static str {
        match self {
            Self::CustomerConfigured => "customer_configured",
            Self::OutOfBand => "out_of_band",
        }
    }
}

/// Explicit signing input. Trust is never embedded as a public key in V2; an
/// independent verifier requires a matching external trust store.
#[derive(Clone, Copy)]
pub(crate) struct CustomerProofSigner<'a> {
    pub(crate) signing_key: &'a SigningKey,
    pub(crate) trust_basis: CustomerProofTrustBasis,
}

/// Pure assembly result, suitable for a caller that wants to hand the bytes
/// straight to a separate verifier before writing anything.
#[derive(Debug, Clone)]
pub(crate) struct AssembledCustomerProofV2 {
    pub(crate) bundle_id: String,
    pub(crate) bundle_digest: String,
    pub(crate) canonical_json: Vec<u8>,
    artifacts: Vec<CustomerProofArtifact>,
}

impl AssembledCustomerProofV2 {
    /// Writes one atomic local proof directory containing `customer-proof.json`
    /// and only the inventory files whose bytes were signed into the document.
    pub(crate) fn write_to(&self, output: &Path) -> Result<(), String> {
        if output.exists() {
            return Err(format!(
                "refusing to overwrite existing proof directory {}",
                output.display()
            ));
        }
        let parent = output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or_else(|| format!("proof output {} has no parent directory", output.display()))?;
        fs::create_dir_all(parent).map_err(|error| format!("create proof parent: {error}"))?;
        let leaf = output
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| "proof output must have a UTF-8 directory name".to_string())?;
        let staging = parent.join(format!(".{leaf}.{}.staging", uuid::Uuid::new_v4().simple()));
        fs::create_dir(&staging)
            .map_err(|error| format!("create proof staging directory: {error}"))?;
        let result = self.write_staging(&staging);
        if let Err(error) = result {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
        fs::rename(&staging, output).map_err(|error| {
            let _ = fs::remove_dir_all(&staging);
            format!("atomically publish proof directory: {error}")
        })
    }

    fn write_staging(&self, staging: &Path) -> Result<(), String> {
        fs::write(staging.join("customer-proof.json"), &self.canonical_json)
            .map_err(|error| format!("write canonical proof JSON: {error}"))?;
        for artifact in &self.artifacts {
            let destination = staging.join(&artifact.path);
            let parent = destination
                .parent()
                .ok_or_else(|| "artifact destination has no parent".to_string())?;
            fs::create_dir_all(parent)
                .map_err(|error| format!("create artifact directory: {error}"))?;
            fs::write(destination, &artifact.bytes)
                .map_err(|error| format!("write proof artifact: {error}"))?;
        }
        Ok(())
    }
}

/// Build a V2 document from explicit evidence bytes. This is deterministic for
/// the same draft, artifact bytes, and signing key.
pub(crate) fn assemble_customer_proof_v2(
    draft: &CustomerProofDraftV2,
    artifacts: Vec<CustomerProofArtifact>,
    signer: CustomerProofSigner<'_>,
) -> Result<AssembledCustomerProofV2, String> {
    validate_draft(draft)?;
    let inventory = build_inventory(&artifacts)?;
    validate_declared_artifact_refs(draft, &inventory.references)?;

    let key_id = format!(
        "id:{}",
        sha256_digest(signer.signing_key.verifying_key().as_bytes())
    );
    let mut document = json!({
        "schema_version": SCHEMA_VERSION,
        "bundle_kind": "customer-proof",
        "created_at": draft.created_at,
        "status": draft.status,
        "subject": draft.subject,
        "matched_arms": draft.matched_arms,
        "inventory": inventory.value,
        "quality": draft.quality,
        "replay": draft.replay,
        "limitations": draft.limitations,
        "redaction": draft.redaction,
        "claims": draft.claims,
        "signing": {
            "algorithm": "Ed25519",
            "trusted_signer_ref": format!("signer:{key_id}"),
            "key_id": key_id,
            "trust_basis": signer.trust_basis.as_str(),
            "signed_digest": "",
            "signature": ""
        }
    });
    let unsigned = unsigned_document(&document)?;
    let bundle_digest = sha256_digest(&crate::core::canonical::canonical_json_bytes(&unsigned));
    let root = document
        .as_object_mut()
        .ok_or_else(|| "assembled proof must be an object".to_string())?;
    root.insert(
        "bundle_id".to_string(),
        Value::String(format!("id:{bundle_digest}")),
    );
    root.insert(
        "bundle_digest".to_string(),
        Value::String(bundle_digest.clone()),
    );
    let signing = root
        .get_mut("signing")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| "assembled proof signing must be an object".to_string())?;
    signing.insert(
        "signed_digest".to_string(),
        Value::String(bundle_digest.clone()),
    );
    signing.insert(
        "signature".to_string(),
        Value::String(
            STANDARD.encode(
                signer
                    .signing_key
                    .sign(&crate::core::canonical::canonical_json_bytes(&unsigned))
                    .to_bytes(),
            ),
        ),
    );
    Ok(AssembledCustomerProofV2 {
        bundle_id: format!("id:{bundle_digest}"),
        bundle_digest,
        canonical_json: crate::core::canonical::canonical_json_bytes(&document),
        artifacts,
    })
}

struct Inventory {
    value: Value,
    references: BTreeSet<String>,
}

fn build_inventory(artifacts: &[CustomerProofArtifact]) -> Result<Inventory, String> {
    if artifacts.is_empty() || artifacts.len() > MAX_ITEMS {
        return Err("customer proof requires 1..=128 present artifacts".to_string());
    }
    let mut artifacts = artifacts.to_vec();
    artifacts.sort_by(|left, right| left.path.cmp(&right.path));
    let mut paths = BTreeSet::new();
    let mut references = BTreeSet::new();
    let mut total_bytes = 0_usize;
    let items: Result<Vec<Value>, String> = artifacts
        .iter()
        .map(|artifact| {
            if !is_safe_relative_path(&artifact.path) {
                return Err(format!("unsafe proof artifact path {}", artifact.path));
            }
            if artifact.bytes.len() > MAX_ITEM_BYTES {
                return Err(format!("proof artifact {} exceeds 8 MiB", artifact.path));
            }
            if !paths.insert(artifact.path.clone()) {
                return Err(format!("duplicate proof artifact path {}", artifact.path));
            }
            total_bytes = total_bytes
                .checked_add(artifact.bytes.len())
                .ok_or_else(|| "proof artifact total overflows".to_string())?;
            if total_bytes > MAX_TOTAL_BYTES {
                return Err("proof artifacts exceed 64 MiB".to_string());
            }
            let digest = artifact.digest();
            let reference = artifact.reference();
            if !references.insert(reference.clone()) {
                return Err(
                    "proof artifacts must not have duplicate content references".to_string()
                );
            }
            Ok(json!({
                "ref": reference,
                "kind": artifact.kind.as_str(),
                "path": artifact.path,
                "digest": digest,
                "size_bytes": artifact.bytes.len(),
                "availability": "present",
                "redaction_class": artifact.redaction_class.as_str(),
            }))
        })
        .collect();
    Ok(Inventory {
        value: json!({
            "max_items": MAX_ITEMS,
            "item_count": artifacts.len(),
            "total_bytes": total_bytes,
            "items": items?,
        }),
        references,
    })
}

fn validate_draft(draft: &CustomerProofDraftV2) -> Result<(), String> {
    if !is_timestamp(&draft.created_at) {
        return Err("customer proof created_at must be a UTC second timestamp".to_string());
    }
    if !matches!(draft.status.as_str(), "complete" | "partial" | "invalid") {
        return Err("customer proof status is invalid".to_string());
    }
    for (name, value) in [
        ("subject", &draft.subject),
        ("matched_arms", &draft.matched_arms),
        ("quality", &draft.quality),
        ("replay", &draft.replay),
        ("limitations", &draft.limitations),
        ("redaction", &draft.redaction),
    ] {
        if !value.is_object() {
            return Err(format!("customer proof {name} must be an object"));
        }
    }
    if !draft.claims.is_array() {
        return Err("customer proof claims must be an array".to_string());
    }
    Ok(())
}

fn validate_declared_artifact_refs(
    draft: &CustomerProofDraftV2,
    references: &BTreeSet<String>,
) -> Result<(), String> {
    let mut declared = Vec::new();
    for arm in ["control", "treatment"] {
        let refs = draft
            .matched_arms
            .pointer(&format!("/{arm}/evidence_refs"))
            .ok_or_else(|| format!("matched_arms.{arm}.evidence_refs is required"))?;
        declared.extend(string_refs(
            refs,
            &format!("matched_arms.{arm}.evidence_refs"),
        )?);
    }
    declared.extend(string_refs(
        draft
            .quality
            .get("evidence_refs")
            .ok_or_else(|| "quality.evidence_refs is required".to_string())?,
        "quality.evidence_refs",
    )?);
    for field in ["input_refs", "result_refs"] {
        declared.extend(string_refs(
            draft
                .replay
                .get(field)
                .ok_or_else(|| format!("replay.{field} is required"))?,
            &format!("replay.{field}"),
        )?);
    }
    let claims = draft
        .claims
        .as_array()
        .ok_or_else(|| "claims must be an array".to_string())?;
    for (index, claim) in claims.iter().enumerate() {
        declared.extend(string_refs(
            claim
                .get("basis_refs")
                .ok_or_else(|| format!("claims[{index}].basis_refs is required"))?,
            &format!("claims[{index}].basis_refs"),
        )?);
    }
    if let Some(missing) = declared
        .iter()
        .find(|reference| !references.contains(*reference))
    {
        return Err(format!(
            "proof body references artifact not supplied to assembler: {missing}"
        ));
    }
    Ok(())
}

fn string_refs(value: &Value, label: &str) -> Result<Vec<String>, String> {
    let values = value
        .as_array()
        .ok_or_else(|| format!("{label} must be an array"))?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{label} must contain only strings"))
        })
        .collect()
}

fn unsigned_document(document: &Value) -> Result<Value, String> {
    let mut unsigned = document.clone();
    let root = unsigned
        .as_object_mut()
        .ok_or_else(|| "proof document must be an object".to_string())?;
    root.remove("bundle_id");
    root.remove("bundle_digest");
    let signing = root
        .get_mut("signing")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| "proof document signing must be an object".to_string())?;
    signing.remove("signed_digest");
    signing.remove("signature");
    Ok(unsigned)
}

fn sha256_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut hex, "{byte:02x}").expect("writing to a String cannot fail");
    }
    format!("sha256:{hex}")
}

fn is_safe_relative_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 256
        && path
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/'))
        && !Path::new(path).is_absolute()
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn is_timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 20
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b':'
        && bytes[16] == b':'
        && bytes[19] == b'Z'
        && bytes
            .iter()
            .enumerate()
            .filter(|(index, _)| !matches!(index, 4 | 7 | 10 | 13 | 16 | 19))
            .all(|(_, byte)| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(
        path: &str,
        kind: CustomerProofArtifactKind,
        bytes: &[u8],
    ) -> CustomerProofArtifact {
        CustomerProofArtifact {
            kind,
            path: path.to_string(),
            redaction_class: CustomerProofRedactionClass::MetadataOnly,
            bytes: bytes.to_vec(),
        }
    }

    fn draft(artifacts: &[CustomerProofArtifact]) -> CustomerProofDraftV2 {
        let control = artifacts[0].reference();
        let treatment = artifacts[1].reference();
        let quality = artifacts[2].reference();
        let replay_input = artifacts[3].reference();
        let replay_result = artifacts[4].reference();
        CustomerProofDraftV2 {
            created_at: "2026-08-22T09:00:00Z".to_string(),
            status: "complete".to_string(),
            subject: json!({
                "customer_ref": "customer:acme-labs",
                "project_ref": "project:lean-ctx",
                "workload_ref": "id:sha256:42d8afd8fea1184e1cb2a42075e602611597f2486636929f0a128fc907f489d5"
            }),
            matched_arms: json!({
                "match_id": "id:sha256:edcaf288f759371516be564ba8e346981a0ed7068581bb11d1da3fd38e585864",
                "match_basis": ["provider", "model", "source_commit", "workload_digest"],
                "shared_identity": {"provider":"openai","model":"gpt-5.6-luna","source_commit":"git:6d4fefa595000000000000000000000000000000","workload_digest":"sha256:42d8afd8fea1184e1cb2a42075e602611597f2486636929f0a128fc907f489d5"},
                "control": {"role":"control","arm_id":"id:sha256:660fb46674b8a16166026551e50b032a1ccf58a4d260c4bb4e42182350660830","identity":{"provider":"openai","model":"gpt-5.6-luna","source_commit":"git:6d4fefa595000000000000000000000000000000","workload_digest":"sha256:42d8afd8fea1184e1cb2a42075e602611597f2486636929f0a128fc907f489d5"},"status":"complete","measurements":{"input_tokens":12000,"cached_input_tokens":0,"output_tokens":900,"latency_ms":820,"cost":{"currency":"USD","amount_micros":1000000,"status":"observed"},"status":"observed"},"evidence_refs":[control]},
                "treatment": {"role":"treatment","arm_id":"id:sha256:b90873fbdffe04dad346c267e9dbdcf3c7a6f121e917620bc80a2531b78e4691","identity":{"provider":"openai","model":"gpt-5.6-luna","source_commit":"git:6d4fefa595000000000000000000000000000000","workload_digest":"sha256:42d8afd8fea1184e1cb2a42075e602611597f2486636929f0a128fc907f489d5"},"status":"complete","measurements":{"input_tokens":8000,"cached_input_tokens":4000,"output_tokens":900,"latency_ms":790,"cost":{"currency":"USD","amount_micros":700000,"status":"observed"},"status":"observed"},"evidence_refs":[treatment]}
            }),
            quality: json!({"status":"preserved","metric":"score_milli","control_score_milli":920,"treatment_score_milli":920,"confidence":"high","method":"automated","evidence_refs":[quality]}),
            replay: json!({"status":"partial","mode":"offline","determinism":"same_inputs_expected","input_refs":[replay_input],"result_refs":[replay_result],"notes":"fixture"}),
            limitations: json!({"known_limitations":[],"unproven":["omission_before_capture"]}),
            redaction: json!({"class":"metadata_only","policy":"content_minimization","reversible":false,"notes":"fixture"}),
            claims: json!([
                {"claim_id":"id:sha256:178e80b928614a26b58c35af69d77e01f917bbb85263ee1251106b050f0b8bfc","claim_type":"cost_reduction","statement":"Treatment cost was lower.","claim_validity":"supported","scope":"matched_run","basis_refs":[artifacts[0].reference(), artifacts[1].reference()]},
                {"claim_id":"id:sha256:528e6c9b4f275560ef2456e5ca68601b124e215c3494055187467cc30a670371","claim_type":"quality_preserved","statement":"Treatment quality was preserved.","claim_validity":"supported","scope":"matched_run","basis_refs":[artifacts[2].reference()]}
            ]),
        }
    }

    fn artifacts() -> Vec<CustomerProofArtifact> {
        vec![
            artifact(
                "arms/control.json",
                CustomerProofArtifactKind::ArmReceipt,
                b"control",
            ),
            artifact(
                "arms/treatment.json",
                CustomerProofArtifactKind::ArmReceipt,
                b"treatment",
            ),
            artifact(
                "quality/comparison.json",
                CustomerProofArtifactKind::QualityMeasurement,
                b"quality",
            ),
            artifact(
                "replay/input.json",
                CustomerProofArtifactKind::ReplayInput,
                b"input",
            ),
            artifact(
                "replay/result.json",
                CustomerProofArtifactKind::ReplayResult,
                b"result",
            ),
        ]
    }

    #[test]
    fn assembly_is_deterministic_and_signs_canonical_body() {
        let artifacts = artifacts();
        let draft = draft(&artifacts);
        let key = SigningKey::from_bytes(&[17; 32]);
        let first = assemble_customer_proof_v2(
            &draft,
            artifacts.clone(),
            CustomerProofSigner {
                signing_key: &key,
                trust_basis: CustomerProofTrustBasis::OutOfBand,
            },
        )
        .unwrap();
        let second = assemble_customer_proof_v2(
            &draft,
            artifacts,
            CustomerProofSigner {
                signing_key: &key,
                trust_basis: CustomerProofTrustBasis::OutOfBand,
            },
        )
        .unwrap();
        assert_eq!(first.canonical_json, second.canonical_json);
        assert_eq!(first.bundle_id, format!("id:{}", first.bundle_digest));
    }

    #[test]
    fn write_to_publishes_a_complete_new_proof_directory() {
        let artifacts = artifacts();
        let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
        let assembled = assemble_customer_proof_v2(
            &draft(&artifacts),
            artifacts.clone(),
            CustomerProofSigner {
                signing_key: &signing_key,
                trust_basis: CustomerProofTrustBasis::CustomerConfigured,
            },
        )
        .unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let output = temporary.path().join("proof");

        assembled.write_to(&output).unwrap();

        assert_eq!(
            fs::read(output.join("customer-proof.json")).unwrap(),
            assembled.canonical_json
        );
        assert_eq!(
            fs::read(output.join(&artifacts[0].path)).unwrap(),
            artifacts[0].bytes
        );
        assert!(assembled.write_to(&output).is_err());
    }

    #[test]
    fn assembly_rejects_dangling_or_unsafe_artifact_inputs() {
        let mut artifacts = artifacts();
        artifacts[0].path = "../escape.json".to_string();
        let draft = draft(&artifacts);
        let key = SigningKey::from_bytes(&[17; 32]);
        let error = assemble_customer_proof_v2(
            &draft,
            artifacts,
            CustomerProofSigner {
                signing_key: &key,
                trust_basis: CustomerProofTrustBasis::CustomerConfigured,
            },
        )
        .unwrap_err();
        assert!(error.contains("unsafe proof artifact path"));
    }
}
