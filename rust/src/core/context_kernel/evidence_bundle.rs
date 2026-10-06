//! Deterministic, task-scoped grouping of typed evidence references.

use lean_ctx_protocol::EvidenceRefV1;

/// A finalized set of typed evidence references for one task.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceBundle {
    pub task_id: String,
    pub refs: Vec<EvidenceRefV1>,
    /// BLAKE3 of the domain-separated task and complete ordered references.
    pub bundle_hash: String,
}

impl EvidenceBundle {
    /// Create an empty bundle for `task_id`.
    #[must_use]
    pub fn new(task_id: String) -> Self {
        Self {
            task_id,
            refs: Vec::new(),
            bundle_hash: String::new(),
        }
    }

    /// Append one reference and invalidate a previously finalized hash.
    pub fn add_ref(&mut self, evidence: EvidenceRefV1) {
        self.refs.push(evidence);
        self.bundle_hash.clear();
    }

    /// Compute the content hash for the current reference list.
    pub fn finalize(&mut self) {
        self.bundle_hash = self.calculate_hash();
    }

    /// Return whether the bundle is internally consistent and finalized.
    /// This does not fetch referenced content or authenticate a sender's claimed
    /// signature status; consumers must verify that evidence independently.
    #[must_use]
    pub fn verify(&self) -> bool {
        !self.task_id.is_empty()
            && self.refs.iter().all(|evidence| evidence.validate().is_ok())
            && self.bundle_hash == self.calculate_hash()
    }

    fn calculate_hash(&self) -> String {
        let transcript =
            serde_json::to_string(&("leanctx.evidence.bundle.v1", &self.task_id, &self.refs))
                .expect("typed evidence references serialize without fallible map keys");
        crate::core::hasher::hash_str(&transcript)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lean_ctx_protocol::{EvidenceKind, SignatureStatus};

    fn evidence(ref_id: &str, content_hash: &str) -> EvidenceRefV1 {
        EvidenceRefV1 {
            schema_version: None,
            media_type: None,
            extensions: Default::default(),
            uri: ref_id.to_owned(),
            kind: EvidenceKind::ProviderReceipt,
            digest: content_hash.to_owned(),
            signature_status: SignatureStatus::Unverified,
        }
    }

    #[test]
    fn finalize_and_verify_round_trip() {
        let mut bundle = EvidenceBundle::new("task-1".to_owned());
        bundle.add_ref(evidence("ref-1", "hash-1"));
        bundle.add_ref(evidence("ref-2", "hash-2"));
        assert!(!bundle.verify());
        bundle.finalize();
        assert!(bundle.verify());
    }

    #[test]
    fn changing_a_reference_invalidates_the_bundle() {
        let mut bundle = EvidenceBundle::new("task-1".to_owned());
        bundle.add_ref(evidence("ref-1", "hash-1"));
        bundle.finalize();
        bundle.refs[0].digest = "tampered".to_owned();
        assert!(!bundle.verify());
    }

    #[test]
    fn mismatched_task_id_is_not_verified() {
        let mut bundle = EvidenceBundle::new("task-1".to_owned());
        bundle.add_ref(evidence("ref-1", "hash-1"));
        bundle.finalize();
        bundle.task_id = "task-2".to_owned();
        assert!(!bundle.verify());
    }

    #[test]
    fn reference_metadata_and_field_boundaries_are_bound() {
        let mut original = EvidenceBundle::new("task-1".into());
        original.add_ref(evidence("ab", "c"));
        original.finalize();
        for field in ["uri", "kind", "signature_status"] {
            let mut changed = original.clone();
            match field {
                "uri" => changed.refs[0].uri = "other".into(),
                "kind" => changed.refs[0].kind = EvidenceKind::RuntimeLog,
                _ => changed.refs[0].signature_status = SignatureStatus::Verified,
            }
            assert!(!changed.verify(), "{field}");
        }
        let mut other = EvidenceBundle::new("task-1".into());
        other.add_ref(evidence("a", "bc"));
        other.finalize();
        assert_ne!(original.bundle_hash, other.bundle_hash);
    }
}
