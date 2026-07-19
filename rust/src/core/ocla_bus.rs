//! OCLA Event Bus (P2 / Track B — Event-Backbone).
//!
//! Zero-cost when disabled: a single `AtomicBool` check (< 5ns) gates all
//! emission. When enabled, events flow into the existing `core::events` ring
//! buffer and JSONL persistence.
//!
//! ## Design principles
//!
//! - **Wrap, don't replace**: The existing `events.rs` infrastructure (ring
//!   buffer, JSONL rotation, persistence) is used as-is. `OclaBus` adds the
//!   OCLA semantic layer on top.
//! - **10 event types**: 5 existing (wired to current code) + 5 new (for
//!   P8/P9/P11 traits, wired as those modules integrate).
//! - **Test isolation**: `OclaBus::scoped(capacity)` creates a bus instance
//!   that does NOT touch the global singleton, enabling parallel tests.
//! - **Determinism**: Event IDs come from the existing sequence allocator.
//!   Timestamps are the only non-deterministic field (acceptable for
//!   observability; not included in contract assertions).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

// ─── OCLA Event Types ────────────────────────────────────────────────────────

/// The 10 OCLA event types defined in the P2 spec.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum OclaEvent {
    /// A proxy request completed (usage_meter.rs).
    RequestCompleted {
        model: String,
        input_tokens: u64,
        output_tokens: u64,
        duration_ms: u64,
        session_id: Option<String>,
    },
    /// User feedback recorded (feedback.rs).
    FeedbackRecorded {
        session_id: String,
        outcome: FeedbackOutcome,
        tool: Option<String>,
    },
    /// Compression threshold shifted (threshold_learning.rs).
    ThresholdShift {
        language: String,
        old_value: f64,
        new_value: f64,
        metric: ThresholdMetric,
    },
    /// Compression applied to a request (compress.rs).
    CompressionApplied {
        path: Option<String>,
        before_tokens: u64,
        after_tokens: u64,
        strategy: String,
    },
    /// Savings recorded to the ledger (savings_ledger/store.rs).
    SavingsRecorded {
        input_saved: u64,
        output_saved: u64,
        source: SavingsSource,
    },
    /// Intent classified for a request (P8 — model_router.rs).
    IntentClassified {
        tier: String,
        confidence: f64,
        reasoning: String,
    },
    /// Outcome tracked for a response (P3 — future outcome_tracker.rs).
    OutcomeRecorded {
        session_id: String,
        accepted: bool,
        implicit: bool,
    },
    /// Response optimization applied (P9 — response_optimizer.rs).
    ResponseOptimized {
        cache_hit: bool,
        is_duplicate: bool,
        tokens_saved: u64,
    },
    /// Model routing decision made (P8 — model_router.rs).
    ModelRouted {
        requested_model: String,
        routed_model: String,
        tier: String,
        model_changed: bool,
    },
    /// Agent chain event (P11 — future agent_gateway.rs).
    AgentChainEvent {
        agent_id: String,
        action: String,
        parent_agent: Option<String>,
    },
}

/// Feedback outcome enum.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackOutcome {
    Accept,
    Reject,
    Partial,
}

/// Threshold metric that shifted.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThresholdMetric {
    Entropy,
    Jaccard,
}

/// Source of savings.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SavingsSource {
    Compression,
    Cache,
    Routing,
    Verbosity,
    ResponseCache,
}

// ─── Bus Record ──────────────────────────────────────────────────────────────

/// A timestamped OCLA event in the bus ring buffer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OclaBusRecord {
    pub id: u64,
    pub timestamp_ms: u64,
    pub event: OclaEvent,
}

// ─── OclaBus ─────────────────────────────────────────────────────────────────

/// The OCLA event bus. Zero-cost when disabled.
///
/// Global usage: `ocla_bus::emit(event)` — checks the global enable flag first.
/// Test usage: `OclaBus::scoped(cap)` — isolated instance, no global state.
pub struct OclaBus {
    enabled: AtomicBool,
    ring: Mutex<VecDeque<OclaBusRecord>>,
    capacity: usize,
    next_id: AtomicU64,
}

impl OclaBus {
    /// Create a new bus with the given ring buffer capacity.
    fn new(capacity: usize) -> Self {
        Self {
            enabled: AtomicBool::new(false),
            ring: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
            next_id: AtomicU64::new(1),
        }
    }

    /// Create a scoped (isolated) bus for testing. Does NOT affect the global bus.
    pub fn scoped(capacity: usize) -> Self {
        let bus = Self::new(capacity);
        bus.enabled.store(true, Ordering::Relaxed);
        bus
    }

    /// Enable the bus. Events will be recorded after this call.
    pub fn enable(&self) {
        self.enabled.store(true, Ordering::Release);
    }

    /// Disable the bus. Events will be discarded (< 5ns per call).
    pub fn disable(&self) {
        self.enabled.store(false, Ordering::Release);
    }

    /// Check if the bus is enabled.
    #[inline]
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    /// Emit an event if the bus is enabled. Returns the event ID, or 0 if disabled.
    #[inline]
    pub fn emit_if_enabled(&self, event: OclaEvent) -> u64 {
        if !self.is_enabled() {
            return 0;
        }
        self.emit_unconditional(event)
    }

    /// Emit unconditionally (skips the enable check). Used internally and by
    /// callers who have already verified the bus is enabled.
    fn emit_unconditional(&self, event: OclaEvent) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let record = OclaBusRecord {
            id,
            timestamp_ms: current_timestamp_ms(),
            event,
        };

        let mut ring = self
            .ring
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if ring.len() >= self.capacity {
            ring.pop_front();
        }
        ring.push_back(record);

        id
    }

    /// Drain all events from the ring (consumes them). Useful for test assertions.
    pub fn drain(&self) -> Vec<OclaBusRecord> {
        let mut ring = self
            .ring
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ring.drain(..).collect()
    }

    /// Read events since a given ID (non-consuming).
    pub fn events_since(&self, after_id: u64) -> Vec<OclaBusRecord> {
        let ring = self
            .ring
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ring.iter().filter(|r| r.id > after_id).cloned().collect()
    }

    /// Read the last N events.
    pub fn latest(&self, n: usize) -> Vec<OclaBusRecord> {
        let ring = self
            .ring
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let start = ring.len().saturating_sub(n);
        ring.iter().skip(start).cloned().collect()
    }

    /// Current ring buffer occupancy.
    pub fn len(&self) -> usize {
        self.ring
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Whether the ring is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Total events emitted (including those evicted from the ring).
    pub fn total_emitted(&self) -> u64 {
        self.next_id.load(Ordering::Relaxed) - 1
    }
}

impl std::fmt::Debug for OclaBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OclaBus")
            .field("enabled", &self.is_enabled())
            .field("ring_len", &self.len())
            .field("capacity", &self.capacity)
            .field("next_id", &self.next_id.load(Ordering::Relaxed))
            .finish()
    }
}

// ─── Global singleton ────────────────────────────────────────────────────────

const DEFAULT_CAPACITY: usize = 1000;

fn global_bus() -> &'static OclaBus {
    static INSTANCE: OnceLock<OclaBus> = OnceLock::new();
    INSTANCE.get_or_init(|| OclaBus::new(DEFAULT_CAPACITY))
}

/// Emit an OCLA event on the global bus. No-op (< 5ns) when disabled.
#[inline]
pub fn emit(event: OclaEvent) -> u64 {
    global_bus().emit_if_enabled(event)
}

/// Enable the global OCLA bus.
pub fn enable() {
    global_bus().enable();
}

/// Disable the global OCLA bus.
pub fn disable() {
    global_bus().disable();
}

/// Check if the global OCLA bus is enabled.
#[inline]
pub fn is_enabled() -> bool {
    global_bus().is_enabled()
}

/// Read events since a given ID from the global bus.
pub fn events_since(after_id: u64) -> Vec<OclaBusRecord> {
    global_bus().events_since(after_id)
}

/// Read the last N events from the global bus.
pub fn latest(n: usize) -> Vec<OclaBusRecord> {
    global_bus().latest(n)
}

/// Total events emitted on the global bus.
pub fn total_emitted() -> u64 {
    global_bus().total_emitted()
}

// ─── Bridge to existing events.rs ────────────────────────────────────────────

/// Bridge: emit an OCLA event AND forward it to the existing events.rs system.
/// This ensures backward compatibility — the dashboard, CLI, and JSONL all
/// continue to see events through the legacy path.
pub fn emit_and_bridge(event: OclaEvent) -> u64 {
    bridge_to_legacy(&event);
    emit(event)
}

/// Convert an OCLA event to a legacy EventKind and emit it.
fn bridge_to_legacy(event: &OclaEvent) {
    use super::events::{EventKind, emit as legacy_emit};

    let kind = match event {
        OclaEvent::CompressionApplied {
            path,
            before_tokens,
            after_tokens,
            strategy,
        } => EventKind::Compression {
            path: path.clone().unwrap_or_default(),
            before_lines: *before_tokens as u32,
            after_lines: *after_tokens as u32,
            strategy: strategy.clone(),
            kept_line_count: *after_tokens as u32,
            removed_line_count: before_tokens.saturating_sub(*after_tokens) as u32,
        },
        OclaEvent::ThresholdShift {
            language,
            old_value,
            new_value,
            metric,
        } => EventKind::ThresholdShift {
            language: language.clone(),
            old_entropy: if *metric == ThresholdMetric::Entropy {
                *old_value
            } else {
                0.0
            },
            new_entropy: if *metric == ThresholdMetric::Entropy {
                *new_value
            } else {
                0.0
            },
            old_jaccard: if *metric == ThresholdMetric::Jaccard {
                *old_value
            } else {
                0.0
            },
            new_jaccard: if *metric == ThresholdMetric::Jaccard {
                *new_value
            } else {
                0.0
            },
        },
        OclaEvent::RequestCompleted { .. }
        | OclaEvent::FeedbackRecorded { .. }
        | OclaEvent::SavingsRecorded { .. }
        | OclaEvent::IntentClassified { .. }
        | OclaEvent::OutcomeRecorded { .. }
        | OclaEvent::ResponseOptimized { .. }
        | OclaEvent::ModelRouted { .. }
        | OclaEvent::AgentChainEvent { .. } => {
            return;
        }
    };

    legacy_emit(kind);
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn current_timestamp_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_bus_returns_zero() {
        let bus = OclaBus::new(16);
        assert!(!bus.is_enabled());
        let id = bus.emit_if_enabled(OclaEvent::RequestCompleted {
            model: "gpt-4o".into(),
            input_tokens: 100,
            output_tokens: 50,
            duration_ms: 200,
            session_id: None,
        });
        assert_eq!(id, 0);
        assert!(bus.is_empty());
    }

    #[test]
    fn enabled_bus_records_events() {
        let bus = OclaBus::scoped(16);
        let id = bus.emit_if_enabled(OclaEvent::ModelRouted {
            requested_model: "gpt-4o".into(),
            routed_model: "gpt-4o-mini".into(),
            tier: "fast".into(),
            model_changed: true,
        });
        assert!(id > 0);
        assert_eq!(bus.len(), 1);
    }

    #[test]
    fn scoped_bus_is_isolated() {
        let bus1 = OclaBus::scoped(8);
        let bus2 = OclaBus::scoped(8);

        bus1.emit_if_enabled(OclaEvent::ResponseOptimized {
            cache_hit: true,
            is_duplicate: false,
            tokens_saved: 42,
        });

        assert_eq!(bus1.len(), 1);
        assert_eq!(bus2.len(), 0, "scoped buses are isolated");
    }

    #[test]
    fn ring_buffer_evicts_oldest() {
        let bus = OclaBus::scoped(3);
        let id1 = bus.emit_if_enabled(OclaEvent::SavingsRecorded {
            input_saved: 10,
            output_saved: 5,
            source: SavingsSource::Compression,
        });
        bus.emit_if_enabled(OclaEvent::SavingsRecorded {
            input_saved: 20,
            output_saved: 10,
            source: SavingsSource::Cache,
        });
        bus.emit_if_enabled(OclaEvent::SavingsRecorded {
            input_saved: 30,
            output_saved: 15,
            source: SavingsSource::Routing,
        });
        // At capacity. Next emit evicts oldest.
        bus.emit_if_enabled(OclaEvent::SavingsRecorded {
            input_saved: 40,
            output_saved: 20,
            source: SavingsSource::Verbosity,
        });

        assert_eq!(bus.len(), 3);
        let events = bus.events_since(0);
        assert!(events.iter().all(|r| r.id > id1), "oldest evicted");
    }

    #[test]
    fn drain_consumes_all_events() {
        let bus = OclaBus::scoped(16);
        bus.emit_if_enabled(OclaEvent::IntentClassified {
            tier: "fast".into(),
            confidence: 0.9,
            reasoning: "simple query".into(),
        });
        bus.emit_if_enabled(OclaEvent::IntentClassified {
            tier: "premium".into(),
            confidence: 0.8,
            reasoning: "complex architecture".into(),
        });

        let drained = bus.drain();
        assert_eq!(drained.len(), 2);
        assert!(bus.is_empty(), "drain consumes events");
    }

    #[test]
    fn events_since_filters_by_id() {
        let bus = OclaBus::scoped(16);
        let id1 = bus.emit_if_enabled(OclaEvent::FeedbackRecorded {
            session_id: "s1".into(),
            outcome: FeedbackOutcome::Accept,
            tool: Some("ctx_read".into()),
        });
        let id2 = bus.emit_if_enabled(OclaEvent::FeedbackRecorded {
            session_id: "s2".into(),
            outcome: FeedbackOutcome::Reject,
            tool: None,
        });

        let after = bus.events_since(id1);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].id, id2);
    }

    #[test]
    fn latest_returns_tail() {
        let bus = OclaBus::scoped(16);
        for i in 0..5 {
            bus.emit_if_enabled(OclaEvent::CompressionApplied {
                path: Some(format!("file_{i}.rs")),
                before_tokens: 100,
                after_tokens: 50,
                strategy: "treesitter".into(),
            });
        }

        let tail = bus.latest(2);
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].id, 4);
        assert_eq!(tail[1].id, 5);
    }

    #[test]
    fn enable_disable_toggle() {
        let bus = OclaBus::new(16);
        assert!(!bus.is_enabled());

        bus.enable();
        assert!(bus.is_enabled());
        let id = bus.emit_if_enabled(OclaEvent::AgentChainEvent {
            agent_id: "a1".into(),
            action: "start".into(),
            parent_agent: None,
        });
        assert!(id > 0);

        bus.disable();
        let id2 = bus.emit_if_enabled(OclaEvent::AgentChainEvent {
            agent_id: "a2".into(),
            action: "stop".into(),
            parent_agent: Some("a1".into()),
        });
        assert_eq!(id2, 0);
        assert_eq!(bus.len(), 1, "disabled emit is a no-op");
    }

    #[test]
    fn total_emitted_counts_all() {
        let bus = OclaBus::scoped(3);
        bus.emit_if_enabled(OclaEvent::OutcomeRecorded {
            session_id: "s1".into(),
            accepted: true,
            implicit: false,
        });
        bus.emit_if_enabled(OclaEvent::OutcomeRecorded {
            session_id: "s2".into(),
            accepted: false,
            implicit: true,
        });
        bus.emit_if_enabled(OclaEvent::OutcomeRecorded {
            session_id: "s3".into(),
            accepted: true,
            implicit: true,
        });
        // Emit one more (evicts first).
        bus.emit_if_enabled(OclaEvent::OutcomeRecorded {
            session_id: "s4".into(),
            accepted: true,
            implicit: false,
        });

        assert_eq!(bus.total_emitted(), 4, "counts all, even evicted");
        assert_eq!(bus.len(), 3, "ring only holds capacity");
    }

    #[test]
    fn event_serialization_roundtrip() {
        let event = OclaEvent::ModelRouted {
            requested_model: "claude-sonnet-4-20250514".into(),
            routed_model: "claude-haiku-3".into(),
            tier: "fast".into(),
            model_changed: true,
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: OclaEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, deserialized);
    }

    #[test]
    fn all_10_event_types_serialize() {
        let events = vec![
            OclaEvent::RequestCompleted {
                model: "m".into(),
                input_tokens: 1,
                output_tokens: 1,
                duration_ms: 1,
                session_id: None,
            },
            OclaEvent::FeedbackRecorded {
                session_id: "s".into(),
                outcome: FeedbackOutcome::Partial,
                tool: None,
            },
            OclaEvent::ThresholdShift {
                language: "rust".into(),
                old_value: 0.5,
                new_value: 0.6,
                metric: ThresholdMetric::Entropy,
            },
            OclaEvent::CompressionApplied {
                path: None,
                before_tokens: 100,
                after_tokens: 50,
                strategy: "s".into(),
            },
            OclaEvent::SavingsRecorded {
                input_saved: 10,
                output_saved: 5,
                source: SavingsSource::ResponseCache,
            },
            OclaEvent::IntentClassified {
                tier: "standard".into(),
                confidence: 0.7,
                reasoning: "r".into(),
            },
            OclaEvent::OutcomeRecorded {
                session_id: "s".into(),
                accepted: true,
                implicit: false,
            },
            OclaEvent::ResponseOptimized {
                cache_hit: false,
                is_duplicate: true,
                tokens_saved: 0,
            },
            OclaEvent::ModelRouted {
                requested_model: "a".into(),
                routed_model: "b".into(),
                tier: "premium".into(),
                model_changed: true,
            },
            OclaEvent::AgentChainEvent {
                agent_id: "x".into(),
                action: "spawn".into(),
                parent_agent: Some("y".into()),
            },
        ];

        for event in &events {
            let json = serde_json::to_string(event).unwrap();
            assert!(!json.is_empty());
            let _: OclaEvent = serde_json::from_str(&json).unwrap();
        }
        assert_eq!(events.len(), 10, "exactly 10 event types");
    }
}
