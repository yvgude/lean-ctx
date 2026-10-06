// SPDX-License-Identifier: Apache-2.0
//! BuiltinObservationHook — keeps structured observations per session.
//!
//! Wraps the proxy observation path. Each `observe` call enriches the
//! observation with delivered tokens and compression ratio and appends it to a
//! bounded per-session ring buffer. File-access heatmap recording belongs to
//! the unified execution lifecycle, so the hook has no side effects.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use crate::core::ocla::traits::{ObservationHook, OclaService};
use crate::core::ocla::types::{Observation, OclaCapability, OclaCapabilityKind, OclaResult};
use crate::core::savings_ledger::event::SavingsEvent;

const MAX_OBSERVATIONS: usize = 512;
const ORIGINAL_TOKENS: &str = "original_tokens";
const SAVED_TOKENS: &str = "saved_tokens";
const DELIVERED_TOKENS: &str = "delivered_tokens";
const COMPRESSION_RATIO_MILLI: &str = "compression_ratio_milli";

pub struct BuiltinObservationHook {
    state: Mutex<ObservationState>,
}

#[derive(Default)]
struct ObservationState {
    ring: HashMap<String, VecDeque<Observation>>,
}

impl BuiltinObservationHook {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(ObservationState::default()),
        }
    }

    pub fn recent(&self, session_id: &str, limit: usize) -> Vec<Observation> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(ring) = state.ring.get(session_id) else {
            return Vec::new();
        };
        let start = ring.len().saturating_sub(limit);
        ring.iter().skip(start).cloned().collect()
    }

    /// Compression band from the fraction of tokens saved — the same bands the
    /// savings ledger records, so `quality_signal` has one meaning per event.
    pub fn quality_signal_for_compression(ratio: f64) -> String {
        crate::core::savings_ledger::compression_band(ratio).into()
    }

    /// Attach compression quality context to a savings event.
    pub fn apply_quality_signal(event: &mut SavingsEvent, ratio: f64) {
        event.quality_signal = Some(Self::quality_signal_for_compression(ratio));
    }

    fn enrich(observation: &mut Observation) -> (u64, u64) {
        let original = observation
            .attributes
            .get(ORIGINAL_TOKENS)
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        let saved = observation
            .attributes
            .get(SAVED_TOKENS)
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0)
            .min(original);
        let delivered = original.saturating_sub(saved);

        let ratio = saved
            .saturating_mul(1000)
            .checked_div(original)
            .unwrap_or(0);

        observation
            .attributes
            .insert(DELIVERED_TOKENS.into(), delivered.to_string());
        observation
            .attributes
            .insert(COMPRESSION_RATIO_MILLI.into(), ratio.to_string());
        (original, saved)
    }
}

impl Default for BuiltinObservationHook {
    fn default() -> Self {
        Self::new()
    }
}

impl OclaService for BuiltinObservationHook {
    fn manifest(&self) -> crate::core::ocla::OclaResult<lean_ctx_protocol::CapabilityManifestV1> {
        crate::core::ocla::capability_fabric::builtin_manifest(&self.capability())
    }

    fn capability(&self) -> OclaCapability {
        OclaCapability::available(OclaCapabilityKind::ObservationHook)
    }
}

#[async_trait::async_trait]
impl ObservationHook for BuiltinObservationHook {
    async fn observe(&self, mut observation: Observation) -> OclaResult<()> {
        Self::enrich(&mut observation);
        let session_id = observation.context.session_id.clone();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let ring = state
            .ring
            .entry(session_id)
            .or_insert_with(|| VecDeque::with_capacity(MAX_OBSERVATIONS));

        if ring.len() >= MAX_OBSERVATIONS {
            ring.pop_front();
        }
        ring.push_back(observation);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ocla::types::OclaRequestContext;
    use std::collections::BTreeMap;

    fn ctx(session: &str) -> OclaRequestContext {
        OclaRequestContext {
            request_id: "r1".into(),
            session_id: session.to_string(),
            agent_id: "agent-test".into(),
            content_ref: "ref:test".into(),
            tenant_id: None,
            trace_id: "tr-unit".into(),
            task_id: None,
            parent_task_id: None,
        }
    }

    #[tokio::test]
    async fn observe_stores_and_bounds() {
        let hook = BuiltinObservationHook::new();
        for i in 0..600 {
            let obs = Observation {
                context: ctx("s1"),
                name: format!("obs-{i}"),
                attributes: BTreeMap::new(),
            };
            hook.observe(obs).await.unwrap();
        }
        let state = hook.state.lock().unwrap();
        assert_eq!(state.ring.get("s1").unwrap().len(), MAX_OBSERVATIONS);
    }

    #[tokio::test]
    async fn observe_enriches_tokens_without_projecting_side_effects() {
        let hook = BuiltinObservationHook::new();
        let mut context = ctx("s1");
        context.content_ref = "file:src/observed.rs".into();
        let observation = Observation {
            context,
            name: "tool_call:ctx_read".into(),
            attributes: BTreeMap::from([
                (ORIGINAL_TOKENS.into(), "100".into()),
                (SAVED_TOKENS.into(), "40".into()),
            ]),
        };

        hook.observe(observation).await.unwrap();

        let state = hook.state.lock().unwrap();
        let stored = state.ring.get("s1").unwrap().back().unwrap();
        assert_eq!(stored.attributes[DELIVERED_TOKENS], "60");
        assert_eq!(stored.attributes[COMPRESSION_RATIO_MILLI], "400");
        assert_eq!(stored.context.content_ref, "file:src/observed.rs");
    }

    #[tokio::test]
    async fn observe_valid_input_is_returned_by_recent() {
        let hook = BuiltinObservationHook::new();
        let observation = Observation {
            context: ctx("session-valid"),
            name: "compression".into(),
            attributes: BTreeMap::from([
                (ORIGINAL_TOKENS.into(), "80".into()),
                (SAVED_TOKENS.into(), "20".into()),
            ]),
        };

        hook.observe(observation).await.unwrap();

        let recent = hook.recent("session-valid", 1);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].name, "compression");
        assert_eq!(recent[0].attributes[DELIVERED_TOKENS], "60");
        assert_eq!(recent[0].attributes[COMPRESSION_RATIO_MILLI], "250");
    }

    #[tokio::test]
    async fn observe_zero_tokens_reports_zero_ratio() {
        let hook = BuiltinObservationHook::new();
        let observation = Observation {
            context: ctx("session-empty"),
            name: "empty".into(),
            attributes: BTreeMap::from([
                (ORIGINAL_TOKENS.into(), "0".into()),
                (SAVED_TOKENS.into(), "0".into()),
            ]),
        };

        hook.observe(observation).await.unwrap();

        let stored = hook.recent("session-empty", 1);
        assert_eq!(stored[0].attributes[DELIVERED_TOKENS], "0");
        assert_eq!(stored[0].attributes[COMPRESSION_RATIO_MILLI], "0");
    }

    #[tokio::test]
    async fn observe_clamps_invalid_savings() {
        let hook = BuiltinObservationHook::new();
        let observation = Observation {
            context: ctx("s1"),
            name: "tool_call:ctx_read".into(),
            attributes: BTreeMap::from([
                (ORIGINAL_TOKENS.into(), "10".into()),
                (SAVED_TOKENS.into(), "99".into()),
            ]),
        };

        hook.observe(observation).await.unwrap();

        let state = hook.state.lock().unwrap();
        let stored = state.ring.get("s1").unwrap().back().unwrap();
        assert_eq!(stored.attributes[DELIVERED_TOKENS], "0");
        assert_eq!(stored.attributes[COMPRESSION_RATIO_MILLI], "1000");
    }

    #[test]
    fn apply_quality_signal_sets_event_quality() {
        let mut event = SavingsEvent {
            ts: "2026-06-01T00:00:00+00:00".into(),
            tool: "ctx_read".into(),
            mechanism: "compression".into(),
            model_id: "model".into(),
            tokenizer: "tokenizer".into(),
            baseline_tokens: 100,
            actual_tokens: 50,
            saved_tokens: 50,
            bounce_adjustment: 0,
            unit_price_per_m_usd: 1.0,
            saved_usd: 0.00005,
            repo_hash: "repo".into(),
            agent_id: "agent".into(),
            prev_hash: String::new(),
            entry_hash: String::new(),
            version: String::new(),
            intent_tag: None,
            outcome: None,
            model_original: None,
            model_routed: None,
            routing_savings: None,
            response_original_tokens: None,
            response_delivered_tokens: None,
            agent_chain_id: None,
            chain_depth: None,
            measurement_method: None,
            evidence_class: None,
            confidence: None,
            request_id: None,
            session_id: None,
            trace_id: None,
            solution_decision: None,
            loc_added: None,
            loc_removed: None,
            path: None,
            lines_added: None,
            lines_removed: None,
            net: None,
            quality_signal: None,
            attribution_group: None,
            attribution_id: None,
            baseline_ref: None,
            price_version: None,
            customer_approval: None,
            settlement_status: None,
            is_first_inject: None,
            cache_read_per_m_usd: None,
            cache_write_per_m_usd: None,
        };

        BuiltinObservationHook::apply_quality_signal(&mut event, 0.5);

        assert_eq!(event.quality_signal.as_deref(), Some("good"));
    }
}
