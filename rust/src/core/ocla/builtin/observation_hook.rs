//! BuiltinObservationHook — emits structured observations to OclaBus.
//!
//! Wraps the proxy observation path. Each `observe` call appends to a
//! bounded per-session ring buffer and emits a CompressionApplied event
//! (the closest existing event type for observation signals).

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use crate::core::ocla::traits::{ObservationHook, OclaService};
use crate::core::ocla::types::{Observation, OclaCapability, OclaCapabilityKind, OclaResult};
use crate::core::ocla_bus::{self, OclaEvent};

const MAX_OBSERVATIONS: usize = 512;

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
}

impl Default for BuiltinObservationHook {
    fn default() -> Self {
        Self::new()
    }
}

impl OclaService for BuiltinObservationHook {
    fn capability(&self) -> OclaCapability {
        OclaCapability::available(OclaCapabilityKind::ObservationHook)
    }
}

impl ObservationHook for BuiltinObservationHook {
    fn observe(&self, observation: Observation) -> OclaResult<()> {
        let session_id = observation.context.session_id.clone();
        let name = observation.name.clone();

        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let ring = state
            .ring
            .entry(session_id.clone())
            .or_insert_with(|| VecDeque::with_capacity(MAX_OBSERVATIONS));

        if ring.len() >= MAX_OBSERVATIONS {
            ring.pop_front();
        }
        ring.push_back(observation);

        ocla_bus::emit(OclaEvent::CompressionApplied {
            path: Some(name),
            before_tokens: 0,
            after_tokens: 0,
            strategy: format!("observation:{session_id}"),
        });

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
        }
    }

    #[test]
    fn observe_stores_and_bounds() {
        let hook = BuiltinObservationHook::new();
        for i in 0..600 {
            let obs = Observation {
                context: ctx("s1"),
                name: format!("obs-{i}"),
                attributes: BTreeMap::new(),
            };
            hook.observe(obs).unwrap();
        }
        let state = hook.state.lock().unwrap();
        assert_eq!(state.ring.get("s1").unwrap().len(), MAX_OBSERVATIONS);
    }
}
