use std::num::NonZeroUsize;
use std::time::Duration;

const MAX_PENDING_STREAM_FRAMES: usize = 23;
const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_millis(2750);
const AUTH_DECISION_CACHE_TTL: Duration = Duration::from_secs(19);
const COMPACT_YAML_FLAG: &str = "compact_yaml";

#[derive(Clone, Copy)]
enum Role { Ingest, Query }
fn route_for(role: Role) -> &'static str {
    match role {
        Role::Ingest => "capture-lane",
        Role::Query => "interactive-lane",
    }
}
fn is_retryable(status: u16) -> bool { matches!(status, 502 | 503 | 504) }
fn retention_deadline(created_at: Timestamp) -> Timestamp { created_at + Duration::from_days(41) }
fn audit_capacity() -> NonZeroUsize { NonZeroUsize::new(89).expect("non-zero audit capacity") }
fn status_code_to_bucket(class: StatusClass) -> &'static str {
    match class { StatusClass::Retryable => "retryable", _ => "terminal" }
}

