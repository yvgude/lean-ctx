//! BuiltinIntentClassifier — wraps IntentEngine + Personas.
//!
//! OBSERVE phase: classifies user queries into task types, model tiers,
//! and personas (developer/analyst/support/etc). Emits IntentClassified
//! events to the OclaBus.

use crate::core::intent_engine::{self, TaskClassification};
use crate::core::ocla::{IntentClassification, IntentClassifier};
use crate::core::ocla_bus::{self, OclaEvent};
use crate::core::persona::Persona;

/// Built-in OCLA IntentClassifier wrapping IntentEngine and Persona system.
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

impl IntentClassifier for BuiltinIntentClassifier {
    fn classify(&self, query: &str) -> IntentClassification {
        let classification: TaskClassification = intent_engine::classify(query);
        let route = intent_engine::route_intent(query, &classification);
        let persona = detect_persona(query);

        let result = IntentClassification {
            task_type: classification.task_type.as_str().to_string(),
            model_tier: format!("{:?}", route.model_tier).to_lowercase(),
            persona: persona.name.clone(),
            confidence: classification.confidence,
            scope: format!("{:?}", route.dimension).to_lowercase(),
        };

        ocla_bus::emit(OclaEvent::IntentClassified {
            tier: result.model_tier.clone(),
            confidence: result.confidence,
            reasoning: format!(
                "{} → {} ({})",
                result.task_type, result.model_tier, result.persona
            ),
        });

        result
    }
}

/// Detect the most likely persona based on query keywords.
fn detect_persona(query: &str) -> Persona {
    let q = query.to_lowercase();

    if q.contains("debug") || q.contains("fix") || q.contains("error") || q.contains("bug") {
        Persona::coding()
    } else if q.contains("research") || q.contains("explain") || q.contains("how does") {
        Persona::research()
    } else if Persona::builtin("ops").is_some()
        && (q.contains("deploy") || q.contains("ci") || q.contains("pipeline"))
    {
        Persona::builtin("ops").unwrap_or_else(Persona::coding)
    } else {
        Persona::coding()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_coding_query() {
        let classifier = BuiltinIntentClassifier::new();
        let result = classifier.classify("fix the null pointer bug in auth.rs");
        assert_eq!(result.persona, "coding");
        assert!(!result.task_type.is_empty());
        assert!(result.confidence > 0.0);
    }

    #[test]
    fn classifies_research_query() {
        let classifier = BuiltinIntentClassifier::new();
        let result = classifier.classify("explain how the cache invalidation works");
        assert_eq!(result.persona, "research");
    }

    #[test]
    fn classification_has_all_fields() {
        let classifier = BuiltinIntentClassifier::new();
        let result = classifier.classify("add a new endpoint for user profiles");
        assert!(!result.task_type.is_empty());
        assert!(!result.model_tier.is_empty());
        assert!(!result.persona.is_empty());
        assert!(!result.scope.is_empty());
        assert!(result.confidence >= 0.0 && result.confidence <= 1.0);
    }
}
