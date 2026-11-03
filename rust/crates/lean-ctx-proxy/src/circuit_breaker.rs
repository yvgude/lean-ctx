// SPDX-License-Identifier: Apache-2.0
//! Public deterministic circuit-breaker reference.
//!
//! The implementation is shared from `lean_ctx_protocol` so clients can use
//! the same state machine. It is intentionally frozen reference behavior:
//! production quality intelligence and adaptive routing remain in
//! `lean-ctx-enterprise` (Class D).

pub use lean_ctx_protocol::circuit_breaker::{
    CircuitBreaker, CircuitBreakerConfig, CircuitBreakerDecision, CircuitBreakerRoute,
    CircuitBreakerState,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn proxy_exposes_circuit_breaker_state_machine() {
        let mut breaker = CircuitBreaker::new(CircuitBreakerConfig {
            failure_threshold: 1,
            recovery_timeout: Duration::from_secs(1),
            half_open_requests: 1,
        });
        assert_eq!(
            breaker.state("coding", "provider"),
            CircuitBreakerState::Closed
        );
        breaker.record_quality_regression("coding", "provider");
        assert_eq!(
            breaker.state("coding", "provider"),
            CircuitBreakerState::Open
        );
    }
}
