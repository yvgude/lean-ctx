//! ContextProofV2 — Claim-based verification schema.
//!
//! Extends ContextProofV1 with structured claims that decompose LLM
//! output verification into individually verifiable assertions. Each
//! claim is routed to the appropriate verifier and tagged with its
//! verification status and evidence.
//!
//! Every claim is checked at runtime by the verifier it names. The schema
//! has no "formally verified" level: nothing in the runtime checks a proof
//! artifact, so no claim may report one (#1914).
//!
//! Design based on:
//!   - Amazon Cedar VGD (arXiv:2407.01688)
//!   - VERGE neurosymbolic verification (arXiv:2601.20055)

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimKind {
    PathValidity,
    ApiInvariant,
    SecretPolicy,
    TestResult,
    TypeCheck,
    ImportPreservation,
    ScopeCompliance,
    PathjailCompliance,
    CompressionInvariant,
    HandoffValidity,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifierKind {
    Deterministic,
    Ast,
    PathPolicy,
    Test,
    TypeChecker,
    StaticAnalysis,
    Heuristic,
    Unverifiable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStatus {
    Passed,
    Failed,
    Skipped,
    Unverified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum QualityLevel {
    #[serde(rename = "0_provenance")]
    Provenance = 0,
    #[serde(rename = "1_deterministic")]
    Deterministic = 1,
    #[serde(rename = "2_tested")]
    Tested = 2,
    #[serde(rename = "3_policy_checked")]
    PolicyChecked = 3,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claim {
    pub id: String,
    pub text: String,
    pub kind: ClaimKind,
    pub verifier: VerifierKind,
    pub status: ClaimStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextProofV2 {
    pub proof_version: String,
    pub run_id: String,
    pub created_at: String,
    pub lean_ctx_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub quality_level: QualityLevel,
    pub claims: Vec<Claim>,
    pub summary: ProofSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofSummary {
    pub total_claims: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub unverified: usize,
}

impl ContextProofV2 {
    pub fn new(run_id: String, session_id: Option<String>) -> Self {
        Self {
            proof_version: "ContextProofV2".to_string(),
            run_id,
            created_at: lean_ctx_time::UtcDateTime::now().to_rfc3339(),
            lean_ctx_version: env!("CARGO_PKG_VERSION").to_string(),
            session_id,
            quality_level: QualityLevel::Provenance,
            claims: Vec::new(),
            summary: ProofSummary::empty(),
        }
    }

    pub fn add_claim(&mut self, claim: Claim) {
        self.claims.push(claim);
        self.recompute();
    }

    pub fn recompute(&mut self) {
        let mut passed = 0;
        let mut failed = 0;
        let mut skipped = 0;
        let mut unverified = 0;

        for c in &self.claims {
            match c.status {
                ClaimStatus::Passed => passed += 1,
                ClaimStatus::Failed => failed += 1,
                ClaimStatus::Skipped => skipped += 1,
                ClaimStatus::Unverified => unverified += 1,
            }
        }

        self.summary = ProofSummary {
            total_claims: self.claims.len(),
            passed,
            failed,
            skipped,
            unverified,
        };

        self.quality_level = if failed > 0 {
            QualityLevel::Provenance
        } else if self.claims.iter().any(|c| {
            c.kind == ClaimKind::ScopeCompliance || c.kind == ClaimKind::PathjailCompliance
        }) {
            QualityLevel::PolicyChecked
        } else if self
            .claims
            .iter()
            .any(|c| c.kind == ClaimKind::TestResult && c.status == ClaimStatus::Passed)
        {
            QualityLevel::Tested
        } else if self
            .claims
            .iter()
            .all(|c| c.status == ClaimStatus::Passed || c.status == ClaimStatus::Skipped)
        {
            QualityLevel::Deterministic
        } else {
            QualityLevel::Provenance
        };
    }
}

impl ProofSummary {
    pub fn empty() -> Self {
        Self {
            total_claims: 0,
            passed: 0,
            failed: 0,
            skipped: 0,
            unverified: 0,
        }
    }
}

pub fn deterministic_claim(id: &str, text: &str, passed: bool) -> Claim {
    Claim {
        id: id.to_string(),
        text: text.to_string(),
        kind: ClaimKind::PathValidity,
        verifier: VerifierKind::Deterministic,
        status: if passed {
            ClaimStatus::Passed
        } else {
            ClaimStatus::Failed
        },
        evidence_ref: None,
    }
}

pub fn policy_claim(id: &str, text: &str, kind: ClaimKind, passed: bool) -> Claim {
    Claim {
        id: id.to_string(),
        text: text.to_string(),
        kind,
        verifier: VerifierKind::PathPolicy,
        status: if passed {
            ClaimStatus::Passed
        } else {
            ClaimStatus::Failed
        },
        evidence_ref: None,
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn new_proof_starts_at_provenance() {
        let proof = ContextProofV2::new("run_1".into(), None);
        assert_eq!(proof.quality_level, QualityLevel::Provenance);
        assert_eq!(proof.summary.total_claims, 0);
    }

    #[test]
    fn adding_deterministic_claims_reaches_level_1() {
        let mut proof = ContextProofV2::new("run_2".into(), None);
        proof.add_claim(deterministic_claim("c1", "paths valid", true));
        proof.add_claim(deterministic_claim("c2", "imports valid", true));
        assert_eq!(proof.quality_level, QualityLevel::Deterministic);
        assert_eq!(proof.summary.passed, 2);
    }

    #[test]
    fn failed_claim_drops_to_provenance() {
        let mut proof = ContextProofV2::new("run_3".into(), None);
        proof.add_claim(deterministic_claim("c1", "paths valid", true));
        proof.add_claim(deterministic_claim("c2", "imports broken", false));
        assert_eq!(proof.quality_level, QualityLevel::Provenance);
        assert_eq!(proof.summary.failed, 1);
    }

    #[test]
    fn policy_claims_reach_level_3() {
        let mut proof = ContextProofV2::new("run_5".into(), None);
        proof.add_claim(policy_claim(
            "c1",
            "pathjail no escape",
            ClaimKind::PathjailCompliance,
            true,
        ));
        proof.add_claim(policy_claim(
            "c2",
            "scope isolation",
            ClaimKind::ScopeCompliance,
            true,
        ));
        assert_eq!(proof.quality_level, QualityLevel::PolicyChecked);
    }

    #[test]
    fn highest_level_is_a_runtime_policy_check() {
        let json = serde_json::to_string(&QualityLevel::PolicyChecked).unwrap();
        assert_eq!(json, "\"3_policy_checked\"");
        assert!(
            serde_json::from_str::<QualityLevel>("\"4_formally_verified\"").is_err(),
            "no claim is checked against a proof artifact, so no level may say so"
        );
        assert!(serde_json::from_str::<ClaimStatus>("\"proved\"").is_err());
    }

    #[test]
    fn serialization_roundtrip() {
        let mut proof = ContextProofV2::new("run_6".into(), Some("sess_1".into()));
        proof.add_claim(policy_claim(
            "api_surface_preserved",
            "API preserved",
            ClaimKind::ApiInvariant,
            true,
        ));
        let json = serde_json::to_string_pretty(&proof).unwrap();
        assert!(json.contains("ContextProofV2"));
        assert!(json.contains("api_surface_preserved"));
        let deserialized: ContextProofV2 = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.claims.len(), 1);
    }

    #[test]
    fn quality_level_ordering() {
        assert!(QualityLevel::Provenance < QualityLevel::Deterministic);
        assert!(QualityLevel::Deterministic < QualityLevel::Tested);
        assert!(QualityLevel::Tested < QualityLevel::PolicyChecked);
    }

    #[test]
    fn claim_status_ordering() {
        assert!(ClaimStatus::Passed < ClaimStatus::Failed);
        assert!(ClaimStatus::Failed < ClaimStatus::Skipped);
        assert!(ClaimStatus::Skipped < ClaimStatus::Unverified);
    }

    #[test]
    fn empty_proof_summary_all_zeros() {
        let s = ProofSummary::empty();
        assert_eq!(s.total_claims, 0);
        assert_eq!(s.passed, 0);
        assert_eq!(s.failed, 0);
        assert_eq!(s.skipped, 0);
        assert_eq!(s.unverified, 0);
    }

    #[test]
    fn skipped_claims_dont_trigger_failure() {
        let mut proof = ContextProofV2::new("run_skip".into(), None);
        proof.add_claim(Claim {
            id: "c1".into(),
            text: "skipped check".into(),
            kind: ClaimKind::TestResult,
            verifier: VerifierKind::Test,
            status: ClaimStatus::Skipped,
            evidence_ref: None,
        });
        assert_eq!(proof.summary.skipped, 1);
        assert_eq!(proof.summary.failed, 0);
        assert_ne!(proof.quality_level, QualityLevel::Provenance);
    }

    #[test]
    fn unverified_claims_tracked() {
        let mut proof = ContextProofV2::new("run_unv".into(), None);
        proof.add_claim(Claim {
            id: "c1".into(),
            text: "unverified".into(),
            kind: ClaimKind::Custom,
            verifier: VerifierKind::Unverifiable,
            status: ClaimStatus::Unverified,
            evidence_ref: None,
        });
        assert_eq!(proof.summary.unverified, 1);
    }
}
