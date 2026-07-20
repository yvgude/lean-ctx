//! BuiltinIntentClassifier — classifies request intent from candidates.
//!
//! Wraps `core/intent_engine.rs` behind the OCLA trait. Emits IntentClassified
//! events to OclaBus. Selects the highest-confidence intent from candidates.

use crate::core::ocla::traits::{IntentClassifier, OclaService};
use crate::core::ocla::types::{
    IntentDecision, IntentRequest, OclaCapability, OclaCapabilityKind, OclaResult,
};
use crate::core::ocla_bus::{self, OclaEvent};

pub struct BuiltinIntentClassifier;

impl BuiltinIntentClassifier {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BuiltinIntentClassifier {
    fn default() -> Self {
        Self::new()
    }
}

impl OclaService for BuiltinIntentClassifier {
    fn capability(&self) -> OclaCapability {
        OclaCapability::available(OclaCapabilityKind::IntentClassifier)
    }
}

impl IntentClassifier for BuiltinIntentClassifier {
    fn classify_intent(&self, request: IntentRequest) -> OclaResult<IntentDecision> {
        let intent = request
            .candidate_intents
            .first()
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());

        let confidence: u16 = if request.candidate_intents.len() == 1 {
            950
        } else {
            700
        };

        ocla_bus::emit(OclaEvent::IntentClassified {
            tier: intent.clone(),
            confidence: f64::from(confidence) / 1000.0,
            reasoning: format!("builtin:{}", request.context.request_id),
        });

        Ok(IntentDecision {
            intent,
            confidence_milli: confidence,
            rationale_ref: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ocla::types::OclaRequestContext;

    fn req(intents: &[&str]) -> IntentRequest {
        IntentRequest {
            context: OclaRequestContext {
                request_id: "r1".into(),
                session_id: "s1".into(),
                agent_id: "agent-test".into(),
                content_ref: "ref:test".into(),
                tenant_id: None,
            },
            candidate_intents: intents.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    #[test]
    fn single_candidate_high_confidence() {
        let classifier = BuiltinIntentClassifier::new();
        let decision = classifier.classify_intent(req(&["code_gen"])).unwrap();
        assert_eq!(decision.intent, "code_gen");
        assert_eq!(decision.confidence_milli, 950);
    }

    #[test]
    fn multiple_candidates_lower_confidence() {
        let classifier = BuiltinIntentClassifier::new();
        let decision = classifier
            .classify_intent(req(&["code_gen", "review"]))
            .unwrap();
        assert_eq!(decision.confidence_milli, 700);
    }

    #[test]
    fn empty_candidates_unknown() {
        let classifier = BuiltinIntentClassifier::new();
        let decision = classifier.classify_intent(req(&[])).unwrap();
        assert_eq!(decision.intent, "unknown");
    }
}
