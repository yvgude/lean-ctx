use chrono::Utc;

use super::ranking::{fact_version_id_v1, hash_project_root, string_similarity};
use super::types::{
    AdmissionResult, Contradiction, ContradictionSeverity, KnowledgeArchetype, KnowledgeFact,
    ProjectKnowledge, ProjectPattern,
};
use crate::core::memory_boundary::FactPrivacy;
use crate::core::memory_policy::MemoryPolicy;

impl ProjectKnowledge {
    pub fn run_memory_lifecycle(
        &mut self,
        policy: &MemoryPolicy,
    ) -> crate::core::memory_lifecycle::LifecycleReport {
        let cfg = crate::core::memory_lifecycle::LifecycleConfig::from_policy(policy);
        crate::core::memory_lifecycle::run_lifecycle(&mut self.facts, &cfg)
    }

    /// Cluster compaction (#971): collapse piles of low-value, mutually-similar
    /// facts into recoverable digests, returning the number of clusters
    /// collapsed. Heavier than [`Self::run_memory_lifecycle`], so it runs only
    /// from the background cognition loop (hourly), never on every write. The
    /// originals are archived and rehydrate on recall — nothing is lost.
    pub fn compact_low_value_clusters(&mut self, policy: &MemoryPolicy) -> u32 {
        if !policy.compaction.enabled {
            return 0;
        }
        let cfg = crate::core::memory_lifecycle::ClusterCompactionConfig {
            min_cluster: policy.compaction.min_cluster,
            similarity: policy.compaction.similarity,
            max_confidence: policy.compaction.max_confidence,
            max_confirmations: policy.compaction.max_confirmations,
        };
        let (collapsed, archived) =
            crate::core::memory_lifecycle::compact_clusters(&mut self.facts, &cfg);
        if !archived.is_empty() {
            let _ = crate::core::memory_lifecycle::archive_facts(&archived);
        }
        collapsed as u32
    }

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
        crate::core::events::emit(crate::core::events::EventKind::KnowledgeUpdate {
            category: category.to_string(),
            key: key.to_string(),
            action: action.to_string(),
        });

        contradiction
    }

    /// Write-time admission gate for the agent-facing `ctx_knowledge remember`
    /// path (#970). Unlike [`Self::remember`] (which every internal restorer also
    /// calls), this enforces the [`crate::core::memory_policy::AdmissionPolicy`]:
    /// near-duplicates are merged instead of inserted, and low-salience noise is
    /// kept out of the capped store — so eviction never has to drop a good fact to
    /// make room for a paraphrase. Internal callers (archive rehydrate, cognition
    /// auto-promotion) keep using [`Self::remember`] and are never gated.
    pub fn remember_admitted(
        &mut self,
        category: &str,
        key: &str,
        value: &str,
        session_id: &str,
        confidence: f32,
        policy: &MemoryPolicy,
    ) -> AdmissionResult {
        let adm = &policy.admission;

        // Admission only governs *new* facts. An existing (category,key) is a
        // confirm/supersede the agent explicitly addressed — defer to remember().
        let has_exact = self
            .facts
            .iter()
            .any(|f| f.category == category && f.key == key && f.is_current());
        if !adm.enabled || has_exact {
            return AdmissionResult::Stored(
                self.remember(category, key, value, session_id, confidence, policy),
            );
        }

        // Salience floor: keep low-signal noise out of a capped store.
        if adm.min_salience > 0 {
            let salience = crate::core::memory_salience::text_salience(value);
            if salience < adm.min_salience {
                return AdmissionResult::RejectedLowSalience {
                    salience,
                    floor: adm.min_salience,
                };
            }
        }

        // Cross-key near-duplicate auto-merge within the same category.
        if adm.auto_merge_similarity > 0.0
            && let Some(merged) = self.merge_near_duplicate(
                category,
                value,
                session_id,
                confidence,
                adm.auto_merge_similarity,
            )
        {
            return merged;
        }

        AdmissionResult::Stored(self.remember(category, key, value, session_id, confidence, policy))
    }

    /// Find the best same-category, different-key, *current* near-duplicate of
    /// `value` at/above `threshold` and merge into it (confirmation bump,
    /// confidence midpoint, keep the longer/more-complete value). Returns the
    /// merge outcome, or `None` when nothing qualifies. Deterministic: ties
    /// resolve to the earliest-inserted fact (stable scan, strict `>`).
    fn merge_near_duplicate(
        &mut self,
        category: &str,
        value: &str,
        session_id: &str,
        confidence: f32,
        threshold: f32,
    ) -> Option<AdmissionResult> {
        let mut best: Option<(usize, f32)> = None;
        for (i, f) in self.facts.iter().enumerate() {
            if !f.is_current() || f.category != category {
                continue;
            }
            let sim = string_similarity(value, &f.value);
            if sim >= threshold && best.is_none_or(|(_, bs)| sim > bs) {
                best = Some((i, sim));
            }
        }
        let (idx, _) = best?;
        let now = Utc::now();
        let f = &mut self.facts[idx];
        f.last_confirmed = now;
        f.source_session = session_id.to_string();
        f.confidence = f32::midpoint(f.confidence, confidence);
        f.confirmation_count += 1;
        f.revision_count += 1;
        if value.len() > f.value.len() {
            f.value = value.to_string();
        }
        Some(AdmissionResult::Merged {
            category: f.category.clone(),
            key: f.key.clone(),
            confirmations: f.confirmation_count,
            value: f.value.clone(),
        })
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

        // Lossless capacity reclaim (#995): keep the newest patterns and archive
        // the rest. The previous `truncate` kept the *oldest* patterns (it dropped
        // the just-pushed one once at the cap) and lost them permanently.
        crate::core::memory_capacity::reclaim_store(
            crate::core::memory_archive::MemoryStore::Patterns,
            Some(&self.project_hash),
            &mut self.patterns,
            policy.knowledge.max_patterns,
            policy.lifecycle.reclaim_headroom_pct,
            policy.lifecycle.reclaim_enabled,
            |a, b| {
                b.created_at
                    .cmp(&a.created_at)
                    .then_with(|| a.pattern_type.cmp(&b.pattern_type))
                    .then_with(|| a.description.cmp(&b.description))
            },
        );
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
