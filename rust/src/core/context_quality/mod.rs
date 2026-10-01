//! Context Quality System — shared vocabulary for quality evidence.
//!
//! LeanCTX produces several kinds of quality evidence (deterministic fidelity checks,
//! retention probes, recovery verification, security checks, recorded and live task
//! evaluations). They answer different questions, so every report states which kind of
//! evidence it is ([`EvidenceTier`]) and a weaker tier can never be presented as a
//! stronger one.

pub mod receipt;
pub mod retention;
pub mod tier;

pub use receipt::ContextQualityReceiptV1;
pub use retention::{RecoveryPath, RetentionReport};
pub use tier::EvidenceTier;
