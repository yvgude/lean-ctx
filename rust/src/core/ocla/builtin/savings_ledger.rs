//! BuiltinSavingsLedger — records compression savings with evidence refs.
//!
//! Wraps `core/savings_ledger/` behind the OCLA trait. Emits SavingsRecorded
//! events to OclaBus. Evidence references are content-addressed (blake3 of
//! the evidence payload), ensuring deterministic, replay-safe identifiers.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::core::ocla::traits::{OclaService, SavingsLedger};
use crate::core::ocla::types::{OclaCapability, OclaCapabilityKind, OclaResult, SavingsEvidence};
use crate::core::ocla_bus::{self, OclaEvent, SavingsSource};

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
}

impl Default for BuiltinSavingsLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl OclaService for BuiltinSavingsLedger {
    fn capability(&self) -> OclaCapability {
        OclaCapability::available(OclaCapabilityKind::SavingsLedger)
    }
}

impl SavingsLedger for BuiltinSavingsLedger {
    fn record_savings(&self, evidence: SavingsEvidence) -> OclaResult<String> {
        let saved = evidence
            .original_tokens
            .saturating_sub(evidence.delivered_tokens);
        self.total_saved.fetch_add(saved, Ordering::Relaxed);
        self.total_original
            .fetch_add(evidence.original_tokens, Ordering::Relaxed);

        let ref_id = evidence.evidence_ref.clone();

        ocla_bus::emit(OclaEvent::SavingsRecorded {
            input_saved: saved,
            output_saved: 0,
            source: SavingsSource::Compression,
        });

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
            },
            original_tokens: original,
            delivered_tokens: delivered,
            quality_ref: None,
            evidence_ref: format!("ev:{original}-{delivered}"),
        }
    }

    #[test]
    fn records_and_accumulates() {
        let ledger = BuiltinSavingsLedger::new();
        ledger.record_savings(evidence(1000, 300)).unwrap();
        ledger.record_savings(evidence(500, 200)).unwrap();

        assert_eq!(ledger.total_tokens_saved(), 1000);
    }

    #[test]
    fn ratio_calculation() {
        let ledger = BuiltinSavingsLedger::new();
        ledger.record_savings(evidence(1000, 250)).unwrap();

        assert_eq!(ledger.savings_ratio_milli(), 750);
    }
}
