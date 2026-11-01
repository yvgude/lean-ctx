//! Health snapshots and readiness classification for A2A transports.

use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::a2a::rate_limiter::{RateLimitResult, check_rate_limit};
use crate::core::capsule_transport::LocalSignedCapsuleTransport;
use crate::core::ocla::health::dead_letter_queue;

const HEALTH_PROBE_AGENT_ID: &str = "lean-ctx-transport-health";
const HEALTH_PROBE_TOOL_NAME: &str = "a2a-delivery";

static LOCAL_TRANSPORT: OnceLock<LocalSignedCapsuleTransport> = OnceLock::new();

/// Outcomes of the reserved capsule deliveries of this process.
#[derive(Debug, Clone, Copy, Default)]
struct DeliveryOutcomes {
    last_success: Option<DateTime<Utc>>,
    last_failure: Option<DateTime<Utc>>,
    consecutive_failures: u32,
}

static DELIVERY_OUTCOMES: Mutex<DeliveryOutcomes> = Mutex::new(DeliveryOutcomes {
    last_success: None,
    last_failure: None,
    consecutive_failures: 0,
});

fn delivery_outcomes() -> std::sync::MutexGuard<'static, DeliveryOutcomes> {
    DELIVERY_OUTCOMES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Records the outcome of one reserved capsule delivery: a success resets the
/// consecutive-failure count, a failure (negative or lost acknowledgement)
/// increments it.
pub(crate) fn record_delivery(succeeded: bool) {
    delivery_outcomes().record(succeeded, Utc::now());
}

impl DeliveryOutcomes {
    fn record(&mut self, succeeded: bool, at: DateTime<Utc>) {
        if succeeded {
            self.last_success = Some(at);
            self.consecutive_failures = 0;
        } else {
            self.last_failure = Some(at);
            self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        }
    }
}

/// Point-in-time health measurements for the A2A transport subsystem.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransportHealth {
    pub local_queue_depth: usize,
    pub dlq_depth: usize,
    pub dlq_available: bool,
    pub rate_limiter_available: bool,
    pub last_successful_delivery: Option<DateTime<Utc>>,
    pub last_failed_delivery: Option<DateTime<Utc>>,
    pub consecutive_failures: u32,
}

/// Delivery readiness derived from a [`TransportHealth`] snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TransportReadiness {
    Ready,
    Degraded(String),
    Unavailable(String),
}

/// Returns the process-local transport used by A2A health checks.
pub(crate) fn local_transport() -> &'static LocalSignedCapsuleTransport {
    LOCAL_TRANSPORT.get_or_init(LocalSignedCapsuleTransport::default)
}

/// Collects a bounded, point-in-time snapshot of transport dependencies.
pub fn check_transport_health() -> TransportHealth {
    let (dlq_depth, dlq_available) = dead_letter_queue()
        .stats(None)
        .map_or((0, false), |stats| (stats.total, true));
    let rate_limiter_available = matches!(
        check_rate_limit(HEALTH_PROBE_AGENT_ID, HEALTH_PROBE_TOOL_NAME),
        RateLimitResult::Allowed
    );
    let outcomes = *delivery_outcomes();

    TransportHealth {
        local_queue_depth: local_transport().inbox_depth(HEALTH_PROBE_AGENT_ID),
        dlq_depth,
        dlq_available,
        rate_limiter_available,
        last_successful_delivery: outcomes.last_success,
        last_failed_delivery: outcomes.last_failure,
        consecutive_failures: outcomes.consecutive_failures,
    }
}

/// Classifies whether delivery should proceed for a health snapshot.
pub fn readiness(health: &TransportHealth) -> TransportReadiness {
    if !health.dlq_available {
        return TransportReadiness::Unavailable("dead-letter queue is unavailable".to_string());
    }
    if health.consecutive_failures > 5 {
        return TransportReadiness::Unavailable(format!(
            "transport has {} consecutive delivery failures",
            health.consecutive_failures
        ));
    }

    if health.dlq_depth > 100 {
        return TransportReadiness::Degraded(format!(
            "dead-letter queue contains {} entries",
            health.dlq_depth
        ));
    }

    if !health.rate_limiter_available {
        return TransportReadiness::Degraded("rate limiter is saturated".to_string());
    }

    TransportReadiness::Ready
}

#[cfg(test)]
mod tests {
    use super::{DeliveryOutcomes, TransportHealth, TransportReadiness, readiness};

    #[test]
    fn failures_count_up_until_a_delivery_succeeds() {
        let mut outcomes = DeliveryOutcomes::default();
        let at = chrono::Utc::now();
        for _ in 0..3 {
            outcomes.record(false, at);
        }
        assert_eq!(outcomes.consecutive_failures, 3);
        assert_eq!(outcomes.last_failure, Some(at));
        assert_eq!(outcomes.last_success, None);
        outcomes.record(true, at);
        assert_eq!(outcomes.consecutive_failures, 0);
        assert_eq!(outcomes.last_success, Some(at));
        assert_eq!(outcomes.last_failure, Some(at));
    }

    #[test]
    fn health_snapshot_reports_recorded_deliveries() {
        super::record_delivery(false);
        super::record_delivery(true);
        let health = super::check_transport_health();
        assert!(health.last_failed_delivery.is_some());
        assert!(health.last_successful_delivery.is_some());
    }

    fn nominal_health() -> TransportHealth {
        TransportHealth {
            local_queue_depth: 0,
            dlq_depth: 0,
            dlq_available: true,
            rate_limiter_available: true,
            last_successful_delivery: None,
            last_failed_delivery: None,
            consecutive_failures: 0,
        }
    }

    #[test]
    fn nominal_transport_is_ready() {
        assert_eq!(readiness(&nominal_health()), TransportReadiness::Ready);
    }

    #[test]
    fn deep_dlq_degrades_transport() {
        let health = TransportHealth {
            dlq_depth: 101,
            ..nominal_health()
        };

        assert_eq!(
            readiness(&health),
            TransportReadiness::Degraded("dead-letter queue contains 101 entries".to_string())
        );
    }

    #[test]
    fn repeated_failures_make_transport_unavailable() {
        let health = TransportHealth {
            dlq_depth: 101,
            rate_limiter_available: false,
            consecutive_failures: 6,
            ..nominal_health()
        };

        assert_eq!(
            readiness(&health),
            TransportReadiness::Unavailable(
                "transport has 6 consecutive delivery failures".to_string()
            )
        );
    }

    #[test]
    fn unavailable_dlq_makes_transport_unavailable() {
        let health = TransportHealth {
            dlq_available: false,
            ..nominal_health()
        };
        assert_eq!(
            readiness(&health),
            TransportReadiness::Unavailable("dead-letter queue is unavailable".to_string())
        );
    }

    #[test]
    fn saturated_rate_limiter_degrades_transport() {
        let health = TransportHealth {
            rate_limiter_available: false,
            ..nominal_health()
        };

        assert_eq!(
            readiness(&health),
            TransportReadiness::Degraded("rate limiter is saturated".to_string())
        );
    }
}
