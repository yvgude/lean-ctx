use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};

static SLO_EVALS: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize)]
pub struct VerificationObservabilityV1 {
    pub schema_version: u32,
    pub created_at: String,
    pub role: String,
    pub profile: String,
    pub budgets: crate::core::budget_tracker::BudgetSnapshot,
    pub slo: crate::core::slo::SloSnapshot,
    pub verification: crate::core::output_verification::VerificationSnapshot,
    pub proof: crate::core::context_proof::ProofStatsSnapshot,
    pub pipeline: crate::core::pipeline::PipelineStats,
    pub counters: CountersSnapshot,
}

#[derive(Debug, Clone, Serialize)]
pub struct CountersSnapshot {
    pub slo_evals: u64,
}

pub fn record_slo_eval() {
    SLO_EVALS.fetch_add(1, Ordering::Relaxed);
}

pub fn snapshot_v1() -> VerificationObservabilityV1 {
    let created_at = chrono::Utc::now().to_rfc3339();
    let role = crate::core::roles::active_role_name();
    let profile = crate::core::profiles::active_profile_name();

    let budgets = crate::core::budget_tracker::BudgetTracker::global().check();
    let slo = crate::core::slo::evaluate_quiet();
    let verification = crate::core::output_verification::stats_snapshot();
    let proof = crate::core::context_proof::proof_stats_snapshot();
    let pipeline = crate::core::pipeline::PipelineStats::load();

    VerificationObservabilityV1 {
        schema_version: crate::core::contracts::VERIFICATION_OBSERVABILITY_V1_SCHEMA_VERSION,
        created_at,
        role,
        profile,
        budgets,
        slo,
        verification,
        proof,
        pipeline,
        counters: CountersSnapshot {
            slo_evals: SLO_EVALS.load(Ordering::Relaxed),
        },
    }
}

#[must_use]
pub fn format_compact(v: &VerificationObservabilityV1) -> String {
    let proof_last = v
        .proof
        .last_written_at
        .clone()
        .unwrap_or_else(|| "-".to_string());
    format!(
        "{}\n{}\n{}\nProof: written={} last={proof_last}",
        v.verification.format_compact(),
        v.slo.format_compact(),
        v.budgets.format_compact(),
        v.proof.written
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_has_schema_version() {
        let s = snapshot_v1();
        assert_eq!(s.schema_version, 1);
        assert!(s.created_at.contains('T'));
        let compact = format_compact(&s);
        assert!(compact.contains("Verification:"));
        assert!(compact.contains("Proof: written="));
    }
}
