//! Process-wide usage event sink (enterprise#17).
//!
//! `usage_meter::record` is the single choke-point every measured turn flows
//! through (streaming and non-streaming alike). This module lets a run-mode
//! (the self-hosted gateway) subscribe to that stream without the proxy knowing
//! anything about Postgres: the gateway installs an `mpsc` sender at startup,
//! and `push` forwards each finalized [`RealUsage`].
//!
//! Fail-open by construction (enterprise#12): `push` never blocks and never
//! errors the request path — when no sink is installed it is a no-op, and when
//! the writer falls behind the event is dropped (counted, visible in logs)
//! rather than back-pressuring live LLM traffic.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use super::usage::RealUsage;

static SINK: OnceLock<tokio::sync::mpsc::Sender<RealUsage>> = OnceLock::new();
static DROPPED: AtomicU64 = AtomicU64::new(0);

/// Installs the process-wide sink. First caller wins (one gateway run-mode per
/// process); later calls return `false` and change nothing.
pub fn install(sender: tokio::sync::mpsc::Sender<RealUsage>) -> bool {
    SINK.set(sender).is_ok()
}

/// True once a sink is installed (the gateway run-mode is active).
#[must_use]
pub fn installed() -> bool {
    SINK.get().is_some()
}

/// Forwards one finalized usage record to the sink, if any. Never blocks: on a
/// full or closed channel the event is dropped and counted.
pub fn push(usage: &RealUsage) {
    let Some(tx) = SINK.get() else { return };
    if tx.try_send(usage.clone()).is_err() {
        let n = DROPPED.fetch_add(1, Ordering::Relaxed) + 1;
        // Log sparsely (powers of two) so a stalled writer can't flood stderr.
        if n.is_power_of_two() {
            tracing::warn!("usage sink backlogged: {n} event(s) dropped so far");
        }
    }
}

/// Events dropped because the sink was full/closed (observability, #34).
#[must_use]
pub fn dropped_count() -> u64 {
    DROPPED.load(Ordering::Relaxed)
}

/// Events currently queued (sent but not yet consumed by the writer). Used by
/// the gateway's graceful shutdown to drain metering before exit (#51).
#[must_use]
pub fn pending_count() -> usize {
    SINK.get().map_or(0, |tx| tx.max_capacity() - tx.capacity())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_without_sink_is_noop() {
        // No sink installed in this test binary at this point: must not panic.
        push(&RealUsage {
            model: "m".into(),
            input_tokens: 1,
            ..Default::default()
        });
    }
}
