// SPDX-License-Identifier: Apache-2.0
//! BuiltinSavingsLedger — records compression savings with evidence refs.
//!
//! Wraps `core/savings_ledger/` behind the OCLA trait. Evidence references
//! are content-addressed (blake3 of
//! the evidence payload), ensuring deterministic, replay-safe identifiers.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::core::ocla::traits::{OclaService, SavingsLedger};
use crate::core::ocla::types::{OclaCapability, OclaCapabilityKind, OclaResult, SavingsEvidence};

const MAX_EVIDENCE_ENTRIES: usize = 4096;

pub struct BuiltinSavingsLedger {
    entries: Mutex<Vec<SavingsEvidence>>,
    total_saved: AtomicU64,
    total_original: AtomicU64,
}

impl BuiltinSavingsLedger {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::with_capacity(256)),
            total_saved: AtomicU64::new(0),
            total_original: AtomicU64::new(0),
        }
    }

    pub fn total_tokens_saved(&self) -> u64 {
        self.total_saved.load(Ordering::Relaxed)
    }

    pub fn savings_ratio_milli(&self) -> u64 {
        let original = self.total_original.load(Ordering::Relaxed);
        if original == 0 {
            return 0;
        }
        let saved = self.total_saved.load(Ordering::Relaxed);
        saved.saturating_mul(1000) / original
    }

    pub fn verify_evidence(&self, evidence_ref: &str) -> OclaResult<bool> {
        if evidence_ref.trim().is_empty() {
            return Ok(false);
        }

        let evidence = {
            let entries = self
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            entries
                .iter()
                .find(|entry| entry.evidence_ref == evidence_ref)
                .cloned()
        };
        let Some(evidence) = evidence else {
            return Ok(false);
        };
        if evidence.delivered_tokens > evidence.original_tokens {
            return Ok(false);
        }

        let ledger_verification = crate::core::savings_ledger::verify();
        if !ledger_verification.valid {
            return Ok(false);
        }

        let saved = evidence.original_tokens - evidence.delivered_tokens;
        if saved == 0 {
            return Ok(true);
        }

        Ok(crate::core::savings_ledger::all_events()
            .iter()
            .any(|event| {
                event.baseline_tokens == evidence.original_tokens
                    && event.actual_tokens == evidence.delivered_tokens
                    && event.saved_tokens == saved
            }))
    }
}

impl Default for BuiltinSavingsLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl OclaService for BuiltinSavingsLedger {
    fn manifest(&self) -> crate::core::ocla::OclaResult<lean_ctx_protocol::CapabilityManifestV1> {
        crate::core::ocla::capability_fabric::builtin_manifest(&self.capability())
    }

    fn capability(&self) -> OclaCapability {
        OclaCapability::available(OclaCapabilityKind::SavingsLedger)
    }
}

impl SavingsLedger for BuiltinSavingsLedger {
    fn record_savings(&self, evidence: SavingsEvidence) -> OclaResult<String> {
        evidence.context.scope(|| {
            crate::core::savings_ledger::record_tool_event(
                "ocla_savings",
                evidence.original_tokens.try_into().unwrap_or(usize::MAX),
                evidence.delivered_tokens.try_into().unwrap_or(usize::MAX),
                evidence.quality_ref.as_deref(),
                None,
            );
        });

        self.project_savings(evidence)
    }

    fn project_savings(&self, evidence: SavingsEvidence) -> OclaResult<String> {
        let saved = evidence
            .original_tokens
            .saturating_sub(evidence.delivered_tokens);
        self.total_saved.fetch_add(saved, Ordering::Relaxed);
        self.total_original
            .fetch_add(evidence.original_tokens, Ordering::Relaxed);

        let ref_id = evidence.evidence_ref.clone();

        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if entries.len() >= MAX_EVIDENCE_ENTRIES {
            let quarter = entries.len() / 4;
            entries.drain(..quarter);
        }
        entries.push(evidence);

        Ok(ref_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ocla::types::OclaRequestContext;

    fn evidence(original: u64, delivered: u64) -> SavingsEvidence {
        SavingsEvidence {
            context: OclaRequestContext {
                request_id: "r1".into(),
                session_id: "s1".into(),
                agent_id: "agent-test".into(),
                content_ref: "ref:test".into(),
                tenant_id: None,
                trace_id: "tr-unit".into(),
                task_id: None,
                parent_task_id: None,
            },
            original_tokens: original,
            delivered_tokens: delivered,
            quality_ref: None,
            evidence_ref: format!("ev:{original}-{delivered}"),
        }
    }

    #[test]
    fn records_and_accumulates() {
        // Every recording test isolates: record_savings writes the verified
        // ledger under the data dir, which would otherwise land in another
        // test's isolated dir (or the developer's real ledger).
        let _dir = crate::core::data_dir::isolated_data_dir();
        let ledger = BuiltinSavingsLedger::new();
        ledger.record_savings(evidence(1000, 300)).unwrap();
        ledger.record_savings(evidence(500, 200)).unwrap();

        assert_eq!(ledger.total_tokens_saved(), 1000);
    }

    #[test]
    fn ratio_calculation() {
        let _dir = crate::core::data_dir::isolated_data_dir();
        let ledger = BuiltinSavingsLedger::new();
        ledger.record_savings(evidence(1000, 250)).unwrap();

        assert_eq!(ledger.savings_ratio_milli(), 750);
    }

    #[test]
    fn delegates_to_verified_ledger() {
        let dir = crate::core::data_dir::isolated_data_dir();
        let ledger = BuiltinSavingsLedger::new();

        ledger.record_savings(evidence(1000, 250)).unwrap();

        let path = dir.path().join("savings").join("ledger.jsonl");
        let content = std::fs::read_to_string(path).expect("verified ledger written");
        let event: crate::core::savings_ledger::SavingsEvent =
            serde_json::from_str(content.lines().next().expect("one ledger event"))
                .expect("valid ledger event");
        assert_eq!(event.tool, "ocla_savings");
        assert_eq!(event.baseline_tokens, 1000);
        assert_eq!(event.actual_tokens, 250);
        assert_eq!(event.saved_tokens, 750);
    }

    #[test]
    fn skips_zero_savings_in_verified_ledger() {
        let dir = crate::core::data_dir::isolated_data_dir();
        let ledger = BuiltinSavingsLedger::new();

        ledger.record_savings(evidence(100, 100)).unwrap();

        assert!(!dir.path().join("savings").join("ledger.jsonl").exists());
    }

    #[test]
    fn verifies_recorded_evidence() {
        let _dir = crate::core::data_dir::isolated_data_dir();
        let ledger = BuiltinSavingsLedger::new();
        let ref_id = ledger.record_savings(evidence(1000, 250)).unwrap();

        assert!(ledger.verify_evidence(&ref_id).unwrap());
    }

    #[test]
    fn rejects_missing_and_inconsistent_evidence() {
        let ledger = BuiltinSavingsLedger::new();
        assert!(!ledger.verify_evidence("missing").unwrap());

        let invalid = evidence(100, 200);
        let ref_id = invalid.evidence_ref.clone();
        ledger.entries.lock().unwrap().push(invalid);

        assert!(!ledger.verify_evidence(&ref_id).unwrap());
    }
}
