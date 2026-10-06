// SPDX-License-Identifier: Apache-2.0
//! BuiltinIntentClassifier — classifies request intent from candidates.
//!
//! Wraps `core/intent_engine.rs` behind the OCLA trait. Selects the
//! highest-confidence intent from candidates.

use crate::core::intent_engine;
use crate::core::ocla::traits::{IntentClassifier, OclaService};
use crate::core::ocla::types::{
    IntentDecision, IntentRequest, OclaCapability, OclaCapabilityKind, OclaResult,
};

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
    fn manifest(&self) -> crate::core::ocla::OclaResult<lean_ctx_protocol::CapabilityManifestV1> {
        crate::core::ocla::capability_fabric::builtin_manifest(&self.capability())
    }

    fn capability(&self) -> OclaCapability {
        OclaCapability::available(OclaCapabilityKind::IntentClassifier)
    }
}

impl IntentClassifier for BuiltinIntentClassifier {
    fn classify_intent(&self, request: IntentRequest) -> OclaResult<IntentDecision> {
        let (intent, confidence) = request
            .candidate_intents
            .iter()
            .filter_map(|candidate| engine_score(candidate).map(|score| (candidate, score)))
            .max_by(|(_, left), (_, right)| left.total_cmp(right))
            .map_or_else(
                || fallback_decision(&request.candidate_intents),
                |(candidate, score)| (candidate.clone(), confidence_milli(score)),
            );

        Ok(IntentDecision {
            intent,
            confidence_milli: confidence,
            rationale_ref: None,
        })
    }
}

fn engine_score(candidate: &str) -> Option<f64> {
    std::panic::catch_unwind(|| intent_engine::classify(candidate).confidence)
        .ok()
        .filter(|confidence| confidence.is_finite())
}

fn confidence_milli(confidence: f64) -> u16 {
    (confidence.clamp(0.0, 1.0) * 1000.0).round() as u16
}

fn fallback_decision(candidates: &[String]) -> (String, u16) {
    (
        candidates
            .first()
            .cloned()
            .unwrap_or_else(|| "unknown".to_string()),
        0,
    )
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
                trace_id: "tr-unit".into(),
                task_id: None,
                parent_task_id: None,
            },
            candidate_intents: intents.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    #[test]
    fn single_candidate_high_confidence() {
        let classifier = BuiltinIntentClassifier::new();
        let decision = classifier
            .classify_intent(req(&["fix the bug in parser.rs"]))
            .unwrap();
        assert_eq!(decision.intent, "fix the bug in parser.rs");
        assert_eq!(decision.confidence_milli, 950);
    }

    #[test]
    fn engine_selects_highest_confidence_candidate() {
        let classifier = BuiltinIntentClassifier::new();
        let decision = classifier
            .classify_intent(req(&["review the parser", "fix the bug in parser.rs"]))
            .unwrap();
        assert_eq!(decision.intent, "fix the bug in parser.rs");
        assert_eq!(decision.confidence_milli, 950);
    }

    #[test]
    fn empty_candidates_unknown() {
        let classifier = BuiltinIntentClassifier::new();
        let decision = classifier.classify_intent(req(&[])).unwrap();
        assert_eq!(decision.intent, "unknown");
        assert_eq!(decision.confidence_milli, 0);
    }

    #[test]
    fn registry_uses_engine_backed_classifier() {
        use crate::core::ocla::OclaRegistry;

        let decision = OclaRegistry::with_builtins()
            .intent_classifier
            .classify_intent(req(&["explain the cache", "debug the parser crash"]))
            .unwrap();

        assert_eq!(decision.intent, "debug the parser crash");
        assert!(
            decision.confidence_milli > 0,
            "engine should produce non-zero confidence"
        );
    }
}
