//! Claim Extraction — decomposes context pipeline outputs into verifiable claims.
//!
//! Inspired by VERGE (arXiv:2601.20055): instead of verifying entire outputs
//! monolithically, we decompose them into atomic, individually verifiable claims
//! and route each to the appropriate verifier.

use std::path::Path;

use super::context_proof_v2::{
    Claim, ClaimKind, ClaimStatus, ContextProofV2, VerifierKind, deterministic_claim,
    lean_proved_claim, policy_claim,
};

pub struct ClaimExtractor {
    proof: ContextProofV2,
}

impl ClaimExtractor {
    pub fn new(run_id: &str, session_id: Option<&str>) -> Self {
        Self {
            proof: ContextProofV2::new(
                run_id.to_string(),
                session_id.map(std::string::ToString::to_string),
            ),
        }
    }

    pub fn verify_pathjail(&mut self, path: &str, jail_root: &Path) {
        let resolved = crate::core::pathjail::jail_path(&std::path::PathBuf::from(path), jail_root);
        let passed = resolved.is_ok();
        self.proof.add_claim(policy_claim(
            &format!("pathjail:{path}"),
            &format!("Path '{path}' is within jail root"),
            ClaimKind::PathjailCompliance,
            passed,
        ));
    }

    pub fn verify_no_secrets_in_output(&mut self, output: &str) {
        let secret_matches = crate::core::secret_detection::detect_secrets(output);

        let passed = secret_matches.is_empty();
        let text = if passed {
            "No secret patterns found in output".to_string()
        } else {
            let names: Vec<&str> = secret_matches.iter().map(|m| m.pattern_name).collect();
            let mut unique_names: Vec<&str> = names.clone();
            unique_names.sort_unstable();
            unique_names.dedup();
            format!(
                "Secret patterns detected ({}): {}",
                secret_matches.len(),
                unique_names.join(", ")
            )
        };

        self.proof.add_claim(policy_claim(
            "no_secrets",
            &text,
            ClaimKind::SecretPolicy,
            passed,
        ));
    }

    pub fn verify_budget_compliance(&mut self) {
        let snapshot = crate::core::budget_tracker::BudgetTracker::global().check();
        let level = snapshot.worst_level();
        let passed = *level != crate::core::budget_tracker::BudgetLevel::Exhausted;

        self.proof.add_claim(policy_claim(
            "budget_compliance",
            &format!("Budget level: {level}"),
            ClaimKind::BudgetCompliance,
            passed,
        ));
    }

    pub fn verify_signatures_preserved(&mut self, original: &[String], compressed: &[String]) {
        let mut missing = Vec::new();
        for sig in original {
            if !compressed.iter().any(|c| c.contains(sig)) {
                missing.push(sig.clone());
            }
        }

        let passed = missing.is_empty();
        let text = if passed {
            format!("All {} signatures preserved", original.len())
        } else {
            format!(
                "{} of {} signatures missing: {:?}",
                missing.len(),
                original.len(),
                &missing[..missing.len().min(3)]
            )
        };

        self.proof.add_claim(Claim {
            id: "signatures_preserved".to_string(),
            text,
            kind: ClaimKind::CompressionInvariant,
            verifier: VerifierKind::Ast,
            status: if passed {
                ClaimStatus::Passed
            } else {
                ClaimStatus::Failed
            },
            evidence_ref: None,
            lean_theorem: if passed {
                Some("pinned_items_always_preserved".to_string())
            } else {
                None
            },
            lean_axioms: None,
        });
    }

    pub fn verify_imports_preserved(&mut self, original_imports: &[String], compressed: &str) {
        let mut missing = Vec::new();
        for imp in original_imports {
            if !compressed.contains(imp) {
                missing.push(imp.clone());
            }
        }

        let passed = missing.is_empty();
        let text = if passed {
            format!("All {} imports preserved", original_imports.len())
        } else {
            format!(
                "{} of {} imports missing",
                missing.len(),
                original_imports.len()
            )
        };

        self.proof
            .add_claim(deterministic_claim("imports_preserved", &text, passed));
    }

    pub fn add_lean_proof(&mut self, id: &str, text: &str, kind: ClaimKind, theorem: &str) {
        self.proof
            .add_claim(lean_proved_claim(id, text, kind, theorem));
    }

    pub fn add_custom_claim(&mut self, claim: Claim) {
        self.proof.add_claim(claim);
    }

    #[must_use]
    pub fn finalize(self) -> ContextProofV2 {
        self.proof
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_detection_catches_aws_keys() {
        let mut ext = ClaimExtractor::new("test_1", None);
        ext.verify_no_secrets_in_output("const key = 'AKIAIOSFODNN7EXAMPLE'");
        let proof = ext.finalize();
        assert_eq!(proof.summary.failed, 1);
    }

    #[test]
    fn clean_output_passes_secret_check() {
        let mut ext = ClaimExtractor::new("test_2", None);
        ext.verify_no_secrets_in_output("fn main() { println!(\"hello\"); }");
        let proof = ext.finalize();
        assert_eq!(proof.summary.passed, 1);
        assert_eq!(proof.summary.failed, 0);
    }

    #[test]
    fn signatures_preserved_check() {
        let mut ext = ClaimExtractor::new("test_3", None);
        let original = vec!["fn main".to_string(), "pub fn process".to_string()];
        let compressed = vec![
            "fn main() { ... }".to_string(),
            "pub fn process(data: &[u8]) -> Result<(), Error>".to_string(),
        ];
        ext.verify_signatures_preserved(&original, &compressed);
        let proof = ext.finalize();
        assert_eq!(proof.summary.passed, 1);
    }

    #[test]
    fn missing_signatures_detected() {
        let mut ext = ClaimExtractor::new("test_4", None);
        let original = vec!["fn main()".to_string(), "pub fn gone()".to_string()];
        let compressed = vec!["fn main() { ... }".to_string()];
        ext.verify_signatures_preserved(&original, &compressed);
        let proof = ext.finalize();
        assert_eq!(proof.summary.failed, 1);
    }

    #[test]
    fn lean_proof_integration() {
        let mut ext = ClaimExtractor::new("test_5", None);
        ext.add_lean_proof(
            "pathjail_no_escape",
            "PathJail prevents access outside root",
            ClaimKind::PathjailCompliance,
            "LeanCtxProofs.Policy.PathJail.jail_no_escape",
        );
        let proof = ext.finalize();
        assert_eq!(proof.summary.proved, 1);
        assert!(proof.claims[0].lean_theorem.is_some());
    }

    #[test]
    fn combined_extraction_computes_quality() {
        let mut ext = ClaimExtractor::new("test_6", None);
        ext.verify_no_secrets_in_output("clean code");
        ext.add_lean_proof(
            "excluded_never_rendered",
            "Excluded items never in output",
            ClaimKind::CompressionInvariant,
            "LeanCtxProofs.Policy.ContextGovernance.excluded_items_never_rendered",
        );
        let proof = ext.finalize();
        assert_eq!(
            proof.quality_level,
            super::super::context_proof_v2::QualityLevel::FormallyVerified
        );
    }

    #[test]
    fn detects_github_pat() {
        let mut ext = ClaimExtractor::new("test_gh", None);
        ext.verify_no_secrets_in_output("token = ghp_1234567890abcdef");
        let proof = ext.finalize();
        assert_eq!(proof.summary.failed, 1);
    }

    #[test]
    fn detects_gitlab_pat() {
        let mut ext = ClaimExtractor::new("test_gl", None);
        ext.verify_no_secrets_in_output("glpat-xxxxxxxxxxxxxxxxxxxx");
        let proof = ext.finalize();
        assert_eq!(proof.summary.failed, 1);
    }

    #[test]
    fn detects_pem_private_key() {
        let mut ext = ClaimExtractor::new("test_pem", None);
        ext.verify_no_secrets_in_output("-----BEGIN RSA PRIVATE KEY-----");
        let proof = ext.finalize();
        assert_eq!(proof.summary.failed, 1);
    }

    #[test]
    fn empty_signatures_passes() {
        let mut ext = ClaimExtractor::new("test_empty", None);
        ext.verify_signatures_preserved(&[], &[]);
        let proof = ext.finalize();
        assert_eq!(proof.summary.passed, 1);
        assert_eq!(proof.summary.failed, 0);
    }

    #[test]
    fn empty_imports_passes() {
        let mut ext = ClaimExtractor::new("test_imp", None);
        ext.verify_imports_preserved(&[], "some content");
        let proof = ext.finalize();
        assert_eq!(proof.summary.passed, 1);
    }

    #[test]
    fn finalize_returns_correct_run_id() {
        let ext = ClaimExtractor::new("my_run_42", Some("sess_7"));
        let proof = ext.finalize();
        assert_eq!(proof.run_id, "my_run_42");
        assert_eq!(proof.session_id, Some("sess_7".to_string()));
    }

    #[test]
    fn multiple_secret_patterns_all_detected() {
        let mut ext = ClaimExtractor::new("test_multi", None);
        ext.verify_no_secrets_in_output(
            "AKIAIOSFODNN7EXAMPLE1 and sk-abcdefghijklmnopqrstuvwx and password=longvalue1234567890extra",
        );
        let proof = ext.finalize();
        assert_eq!(proof.summary.failed, 1);
        assert!(proof.claims[0].text.contains("aws_key"));
    }
}
