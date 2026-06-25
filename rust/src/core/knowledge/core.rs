use chrono::Utc;

use super::ranking::{fact_version_id_v1, hash_project_root, string_similarity};
use super::types::{
    Contradiction, ContradictionSeverity, KnowledgeArchetype, KnowledgeFact, ProjectKnowledge,
    ProjectPattern,
};
use crate::core::memory_boundary::FactPrivacy;
use crate::core::memory_policy::MemoryPolicy;

impl ProjectKnowledge {
    pub fn run_memory_lifecycle(
        &mut self,
        policy: &MemoryPolicy,
    ) -> crate::core::memory_lifecycle::LifecycleReport {
        let cfg = crate::core::memory_lifecycle::LifecycleConfig {
            max_facts: policy.knowledge.max_facts,
            decay_rate_per_day: policy.lifecycle.decay_rate,
            low_confidence_threshold: policy.lifecycle.low_confidence_threshold,
            stale_days: policy.lifecycle.stale_days,
            consolidation_similarity: policy.lifecycle.similarity_threshold,
            forgetting_model: crate::core::memory_lifecycle::ForgettingModel::parse(
                &policy.lifecycle.forgetting_model,
            ),
            base_stability_days: policy.lifecycle.base_stability_days,
            archetype_aware_decay: policy.lifecycle.archetype_aware_decay,
        };
        crate::core::memory_lifecycle::run_lifecycle(&mut self.facts, &cfg)
    }

    #[must_use]
    pub fn new(project_root: &str) -> Self {
        Self {
            project_root: project_root.to_string(),
            project_hash: hash_project_root(project_root),
            facts: Vec::new(),
            patterns: Vec::new(),
            history: Vec::new(),
            updated_at: Utc::now(),
            judged_pairs: Vec::new(),
        }
    }

    #[must_use]
    pub fn check_contradiction(
        &self,
        category: &str,
        key: &str,
        new_value: &str,
        policy: &MemoryPolicy,
    ) -> Option<Contradiction> {
        let existing = self
            .facts
            .iter()
            .find(|f| f.category == category && f.key == key && f.is_current())?;

        if existing.value.to_lowercase() == new_value.to_lowercase() {
            return None;
        }

        let similarity = string_similarity(&existing.value, new_value);
        if similarity > 0.8 {
            return None;
        }

        let severity = if existing.confidence >= 0.9 && existing.confirmation_count >= 2 {
            ContradictionSeverity::High
        } else if existing.confidence >= policy.knowledge.contradiction_threshold {
            ContradictionSeverity::Medium
        } else {
            ContradictionSeverity::Low
        };

        let resolution = match severity {
            ContradictionSeverity::High => format!(
                "High-confidence fact [{category}/{key}] changed: '{}' -> '{new_value}' (was confirmed {}x). Previous value archived.",
                existing.value, existing.confirmation_count
            ),
            ContradictionSeverity::Medium => format!(
                "Fact [{category}/{key}] updated: '{}' -> '{new_value}'",
                existing.value
            ),
            ContradictionSeverity::Low => format!(
                "Low-confidence fact [{category}/{key}] replaced: '{}' -> '{new_value}'",
                existing.value
            ),
        };

        Some(Contradiction {
            existing_key: key.to_string(),
            existing_value: existing.value.clone(),
            new_value: new_value.to_string(),
            category: category.to_string(),
            severity,
            resolution,
        })
    }

    pub fn remember(
        &mut self,
        category: &str,
        key: &str,
        value: &str,
        session_id: &str,
        confidence: f32,
        policy: &MemoryPolicy,
    ) -> Option<Contradiction> {
        let contradiction = self.check_contradiction(category, key, value, policy);

        if let Some(existing) = self
            .facts
            .iter_mut()
            .find(|f| f.category == category && f.key == key && f.is_current())
        {
            let now = Utc::now();
            let same_value_ci = existing.value.to_lowercase() == value.to_lowercase();
            let similarity = string_similarity(&existing.value, value);

            if existing.value == value || same_value_ci || similarity > 0.8 {
                existing.last_confirmed = now;
                existing.source_session = session_id.to_string();
                existing.confidence = f32::midpoint(existing.confidence, confidence);
                existing.confirmation_count += 1;
                existing.revision_count += 1;

                if existing.value != value && similarity > 0.8 && value.len() > existing.value.len()
                {
                    existing.value = value.to_string();
                }
            } else {
                let superseded = fact_version_id_v1(existing);
                let next_revision = existing.revision_count + 1;
                existing.valid_until = Some(now);
                existing.valid_from = existing.valid_from.or(Some(existing.created_at));

                self.facts.push(KnowledgeFact {
                    category: category.to_string(),
                    key: key.to_string(),
                    value: value.to_string(),
                    source_session: session_id.to_string(),
                    confidence,
                    created_at: now,
                    last_confirmed: now,
                    retrieval_count: 0,
                    last_retrieved: None,
                    valid_from: Some(now),
                    valid_until: None,
                    supersedes: Some(superseded),
                    confirmation_count: 1,
                    feedback_up: 0,
                    feedback_down: 0,
                    last_feedback: None,
                    privacy: FactPrivacy::default(),
                    sensitivity: crate::core::sensitivity::classify_content(value),
                    imported_from: None,
                    archetype: KnowledgeArchetype::infer_from_category(category),
                    fidelity: None,
                    revision_count: next_revision,
                });
            }
        } else {
            let now = Utc::now();
            self.facts.push(KnowledgeFact {
                category: category.to_string(),
                key: key.to_string(),
                value: value.to_string(),
                source_session: session_id.to_string(),
                confidence,
                created_at: now,
                last_confirmed: now,
                retrieval_count: 0,
                last_retrieved: None,
                valid_from: Some(now),
                valid_until: None,
                supersedes: None,
                confirmation_count: 1,
                feedback_up: 0,
                feedback_down: 0,
                last_feedback: None,
                privacy: FactPrivacy::default(),
                sensitivity: crate::core::sensitivity::classify_content(value),
                imported_from: None,
                archetype: KnowledgeArchetype::infer_from_category(category),
                fidelity: None,
                revision_count: 1,
            });
        }

        // Run the lifecycle as soon as we exceed the configured budget.
        // `run_lifecycle` sorts by importance and drains the excess back down to
        // `max_facts` (archiving it), so this is self-limiting: the store settles
        // at <= max_facts. The previous `* 2` guard let a project's facts grow to
        // twice the cap before any eviction fired, which is why stores were
        // observed sitting at 103% (206/200) with no reclamation.
        if self.facts.len() > policy.knowledge.max_facts {
            let _ = self.run_memory_lifecycle(policy);
        }

        self.updated_at = Utc::now();

        let action = if contradiction.is_some() {
            "contradict"
        } else {
            "remember"
        };
        let _ = crate::core::events::emit(crate::core::events::EventKind::KnowledgeUpdate {
            category: category.to_string(),
            key: key.to_string(),
            action: action.to_string(),
        });

        contradiction
    }

    pub fn add_pattern(
        &mut self,
        pattern_type: &str,
        description: &str,
        examples: Vec<String>,
        session_id: &str,
        policy: &MemoryPolicy,
    ) {
        if let Some(existing) = self
            .patterns
            .iter_mut()
            .find(|p| p.pattern_type == pattern_type && p.description == description)
        {
            for ex in &examples {
                if !existing.examples.contains(ex) {
                    existing.examples.push(ex.clone());
                }
            }
            return;
        }

        self.patterns.push(ProjectPattern {
            pattern_type: pattern_type.to_string(),
            description: description.to_string(),
            examples,
            source_session: session_id.to_string(),
            created_at: Utc::now(),
        });

        if self.patterns.len() > policy.knowledge.max_patterns {
            self.patterns.truncate(policy.knowledge.max_patterns);
        }
        self.updated_at = Utc::now();
    }

    pub fn remove_fact(&mut self, category: &str, key: &str) -> bool {
        let before = self.facts.len();
        self.facts
            .retain(|f| !(f.category == category && f.key == key));
        let removed = self.facts.len() < before;
        if removed {
            self.updated_at = Utc::now();
        }
        removed
    }
}
