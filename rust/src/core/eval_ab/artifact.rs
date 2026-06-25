//! Signed, reproducible artifact (#238).
//!
//! Wraps an [`AbReport`] into a portable, tamper-evident attestation. Two guarantees:
//!
//! 1. **Reproducibility** — `determinism_digest` is a SHA-256 over the *evidence only* (task ids,
//!    context + answer digests, scores, model fingerprint, stats config, verdict). Timestamps and
//!    the build version are excluded, so the same inputs yield the same digest on any machine.
//! 2. **Integrity + origin** — an Ed25519 signature over the canonical bytes (signature fields
//!    cleared) proves the artifact was produced by a specific key and not altered since. This
//!    mirrors the established `savings_ledger::signed_batch` pattern exactly.

use std::path::{Path, PathBuf};

use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};

use super::report::{AbReport, Verdict};
use super::sha256_hex;

const SCHEMA_VERSION: u32 = 1;
const KIND: &str = "lean-ctx.eval-ab-artifact";

/// A signed A/B quality attestation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignedAbReportV1 {
    pub schema_version: u32,
    pub kind: String,
    pub created_at: String,
    pub lean_ctx_version: String,
    pub agent_id: String,
    /// Machine-independent digest over the run evidence (see module docs).
    pub determinism_digest: String,
    /// Copied out of the report so a verifier sees the conclusion without parsing stats.
    pub verdict: Verdict,
    /// The full report (records + stats + provenance).
    pub report: AbReport,
    /// Ed25519 public key (hex). `None` until signed.
    pub signer_public_key: Option<String>,
    /// Ed25519 signature over the canonical bytes (hex). `None` until signed.
    pub signature: Option<String>,
}

/// Outcome of verifying a [`SignedAbReportV1`].
#[derive(Debug, Clone, PartialEq)]
pub struct AbVerifyResult {
    /// Signature present + valid over the canonical payload.
    pub signature_valid: bool,
    /// The recomputed digest matches the embedded one (run is internally consistent).
    pub digest_matches: bool,
    pub signer_public_key: Option<String>,
    pub error: Option<String>,
}

impl AbVerifyResult {
    /// Both checks passed.
    #[must_use]
    pub fn ok(&self) -> bool {
        self.signature_valid && self.digest_matches
    }
}

impl SignedAbReportV1 {
    /// Builds an unsigned artifact from a finished report.
    #[must_use]
    pub fn from_report(report: AbReport, agent_id: &str) -> Self {
        let determinism_digest = determinism_digest(&report);
        Self {
            schema_version: SCHEMA_VERSION,
            kind: KIND.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            lean_ctx_version: env!("CARGO_PKG_VERSION").to_string(),
            agent_id: agent_id.to_string(),
            determinism_digest,
            verdict: report.verdict,
            report,
            signer_public_key: None,
            signature: None,
        }
    }

    /// Deterministic bytes that get signed/verified: the whole struct with the signature fields
    /// cleared (identical on sign + verify regardless of JSON float formatting).
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, String> {
        let mut clone = self.clone();
        clone.signature = None;
        clone.signer_public_key = None;
        serde_json::to_vec(&clone).map_err(|e| format!("serialize for signing: {e}"))
    }

    /// Signs with the persistent machine identity (`agent_identity` keystore).
    pub fn sign(&mut self, agent_id: &str) -> Result<(), String> {
        let key = crate::core::agent_identity::get_or_create_keypair(agent_id)?;
        self.sign_with_key(&key)
    }

    /// Signs with an explicit key (used by `sign` and hermetic tests).
    pub fn sign_with_key(&mut self, key: &SigningKey) -> Result<(), String> {
        self.signature = None;
        self.signer_public_key = None;
        let canonical = self.canonical_bytes()?;
        let sig = key.sign(&canonical);
        self.signer_public_key = Some(crate::core::agent_identity::hex_encode(
            &key.verifying_key().to_bytes(),
        ));
        self.signature = Some(crate::core::agent_identity::hex_encode(&sig.to_bytes()));
        Ok(())
    }

    /// Verifies the embedded signature *and* recomputes the determinism digest — offline.
    #[must_use]
    pub fn verify(&self) -> AbVerifyResult {
        let digest_matches = determinism_digest(&self.report) == self.determinism_digest;
        let fail = |msg: &str| AbVerifyResult {
            signature_valid: false,
            digest_matches,
            signer_public_key: self.signer_public_key.clone(),
            error: Some(msg.to_string()),
        };

        let (Some(sig_hex), Some(pk_hex)) = (&self.signature, &self.signer_public_key) else {
            return fail("artifact is not signed");
        };
        let (Ok(sig_bytes), Ok(pk_bytes)) = (
            crate::core::agent_identity::hex_decode(sig_hex),
            crate::core::agent_identity::hex_decode(pk_hex),
        ) else {
            return fail("malformed signature or public key hex");
        };
        let canonical = match self.canonical_bytes() {
            Ok(c) => c,
            Err(e) => return fail(&e),
        };
        if crate::core::agent_identity::verify_signature(&pk_bytes, &canonical, &sig_bytes) {
            AbVerifyResult {
                signature_valid: true,
                digest_matches,
                signer_public_key: Some(pk_hex.clone()),
                error: None,
            }
        } else {
            fail("signature does not match payload (tampered or wrong key)")
        }
    }
}

/// Canonical evidence the determinism digest commits to — timestamps + version excluded.
#[derive(Serialize)]
struct Evidence {
    suite: String,
    budget_tokens: usize,
    model_fingerprint: String,
    bootstrap_iters: usize,
    bootstrap_seed: u64,
    noninferiority_margin: f64,
    verdict: Verdict,
    tasks: Vec<EvidenceRow>,
}

#[derive(Serialize)]
struct EvidenceRow {
    task_id: String,
    domain: String,
    baseline_value: f64,
    lean_ctx_value: f64,
    baseline_passed: bool,
    lean_ctx_passed: bool,
    baseline_context_digest: String,
    lean_ctx_context_digest: String,
    baseline_answer_digest: String,
    lean_ctx_answer_digest: String,
}

/// Computes the machine-independent run digest (records sorted by id for a canonical order).
#[must_use]
pub fn determinism_digest(report: &AbReport) -> String {
    let mut rows: Vec<EvidenceRow> = report
        .records
        .iter()
        .map(|r| EvidenceRow {
            task_id: r.task_id.clone(),
            domain: r.domain.clone(),
            baseline_value: r.baseline_value,
            lean_ctx_value: r.lean_ctx_value,
            baseline_passed: r.baseline_passed,
            lean_ctx_passed: r.lean_ctx_passed,
            baseline_context_digest: r.baseline_context_digest.clone(),
            lean_ctx_context_digest: r.lean_ctx_context_digest.clone(),
            baseline_answer_digest: r.baseline_answer_digest.clone(),
            lean_ctx_answer_digest: r.lean_ctx_answer_digest.clone(),
        })
        .collect();
    rows.sort_by(|a, b| a.task_id.cmp(&b.task_id));

    let evidence = Evidence {
        suite: report.suite.clone(),
        budget_tokens: report.budget_tokens,
        model_fingerprint: report.model.digest(),
        bootstrap_iters: report.stats.bootstrap_iters,
        bootstrap_seed: report.stats.bootstrap_seed,
        noninferiority_margin: report.stats.noninferiority_margin,
        verdict: report.verdict,
        tasks: rows,
    };
    let bytes = serde_json::to_vec(&evidence).unwrap_or_default();
    sha256_hex(&bytes)
}

/// Default artifact location: `<data_dir>/eval/ab-report-v1_<utc-stamp>.json`.
pub fn default_artifact_path() -> Result<PathBuf, String> {
    let dir = crate::core::data_dir::lean_ctx_data_dir()?.join("eval");
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir eval: {e}"))?;
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    Ok(dir.join(format!("ab-report-v1_{stamp}.json")))
}

/// Pretty-prints the artifact to `out` (creating parent dirs). Returns the written path.
pub fn write_artifact(artifact: &SignedAbReportV1, out: &Path) -> Result<PathBuf, String> {
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }
    let json = serde_json::to_string_pretty(artifact).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(out, json).map_err(|e| format!("write {}: {e}", out.display()))?;
    Ok(out.to_path_buf())
}

/// Loads + parses an artifact, rejecting unrelated JSON by `kind`.
pub fn load_artifact(path: &Path) -> Result<SignedAbReportV1, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let artifact: SignedAbReportV1 =
        serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
    if artifact.kind != KIND {
        return Err(format!(
            "not a {KIND} artifact (kind = {:?})",
            artifact.kind
        ));
    }
    Ok(artifact)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::eval_ab::model::{ModelFingerprint, ModelParams};
    use crate::core::eval_ab::report::{AbReport, PairRecord, ReportConfig};

    fn report() -> AbReport {
        let records = vec![PairRecord {
            task_id: "t1".into(),
            domain: "qa".into(),
            baseline_value: 0.2,
            lean_ctx_value: 0.9,
            baseline_passed: false,
            lean_ctx_passed: true,
            baseline_tokens: 100,
            lean_ctx_tokens: 90,
            baseline_context_digest: "ca".into(),
            lean_ctx_context_digest: "cb".into(),
            baseline_answer_digest: "aa".into(),
            lean_ctx_answer_digest: "ab".into(),
        }];
        let fp = ModelFingerprint {
            provider: "recorded".into(),
            endpoint: "rec".into(),
            params: ModelParams::default(),
        };
        AbReport::build("suite", 4000, fp, records, ReportConfig::default())
    }

    fn key() -> SigningKey {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).unwrap();
        SigningKey::from_bytes(&seed)
    }

    #[test]
    fn digest_ignores_timestamp_and_version() {
        let a = SignedAbReportV1::from_report(report(), "local");
        let mut r2 = report();
        r2.created_at = "2000-01-01T00:00:00Z".into();
        r2.lean_ctx_version = "0.0.0".into();
        let b = SignedAbReportV1::from_report(r2, "local");
        assert_eq!(a.determinism_digest, b.determinism_digest);
    }

    #[test]
    fn sign_then_verify_roundtrips() {
        let mut a = SignedAbReportV1::from_report(report(), "local");
        a.sign_with_key(&key()).unwrap();
        let res = a.verify();
        assert!(res.ok(), "{res:?}");
    }

    #[test]
    fn tampering_with_scores_breaks_digest_and_signature() {
        let mut a = SignedAbReportV1::from_report(report(), "local");
        a.sign_with_key(&key()).unwrap();
        a.report.records[0].lean_ctx_value = 0.0;
        let res = a.verify();
        assert!(!res.digest_matches, "edited score must break the digest");
        assert!(
            !res.signature_valid,
            "edited payload must break the signature"
        );
    }

    #[test]
    fn write_load_roundtrip_preserves_signature() {
        let mut a = SignedAbReportV1::from_report(report(), "local");
        a.sign_with_key(&key()).unwrap();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path = std::env::temp_dir().join(format!("lc-ab-{}-{nanos}.json", std::process::id()));
        write_artifact(&a, &path).unwrap();
        let loaded = load_artifact(&path).unwrap();
        assert_eq!(loaded, a);
        assert!(loaded.verify().ok());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_rejects_foreign_json() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path =
            std::env::temp_dir().join(format!("lc-foreign-ab-{}-{nanos}.json", std::process::id()));
        std::fs::write(&path, r#"{"kind":"nope"}"#).unwrap();
        assert!(load_artifact(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }
}
