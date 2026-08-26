//! Frozen compatibility DTOs for the retired behavioral-outcome inference path.
//!
//! The Engine does not infer task acceptance from retries, response length, or
//! delivery success. Explicit host/evaluator observations own outcome semantics.
//! These serialized labels remain only because ProxyKernelResult exposed them.
//! Remove them in the next public API major after consumers migrate to explicit
//! evaluator observations.

use super::types::ReceiptOutcome;

/// Legacy compatibility label; the Engine emits only [Self::Ambiguous].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OutcomeSignal {
    /// Legacy label retained for serialized compatibility.
    FirstPass,
    /// Legacy label retained for serialized compatibility.
    Retry,
    /// Legacy label retained for serialized compatibility.
    Ignored,
    /// No explicit evaluator outcome is available.
    Ambiguous,
}

/// Frozen compatibility representation for an unevaluated proxy outcome.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InferredOutcome {
    /// Explicit evaluator result, or [ReceiptOutcome::Unknown].
    pub outcome: ReceiptOutcome,
    /// Evaluator confidence; zero when no evaluator result exists.
    pub confidence: f64,
    /// Compatibility signal; [OutcomeSignal::Ambiguous] when unevaluated.
    pub signal: OutcomeSignal,
}
