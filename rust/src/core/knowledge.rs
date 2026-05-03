use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::core::memory_policy::MemoryPolicy;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectKnowledge {
    pub project_root: String,
    pub project_hash: String,
    pub facts: Vec<KnowledgeFact>,
    pub patterns: Vec<ProjectPattern>,
    pub history: Vec<ConsolidatedInsight>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeFact {
    pub category: String,
    pub key: String,
    pub value: String,
    pub source_session: String,
    pub confidence: f32,
    pub created_at: DateTime<Utc>,
    pub last_confirmed: DateTime<Utc>,
    #[serde(default)]
    pub retrieval_count: u32,
    #[serde(default)]
    pub last_retrieved: Option<DateTime<Utc>>,
    #[serde(default)]
    pub valid_from: Option<DateTime<Utc>>,
    #[serde(default)]
    pub valid_until: Option<DateTime<Utc>>,
    #[serde(default)]
    pub supersedes: Option<String>,
    #[serde(default)]
    pub confirmation_count: u32,
    #[serde(default)]
    pub feedback_up: u32,
    #[serde(default)]
    pub feedback_down: u32,
    #[serde(default)]
    pub last_feedback: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contradiction {
    pub existing_key: String,
    pub existing_value: String,
    pub new_value: String,
    pub category: String,
    pub severity: ContradictionSeverity,
    pub resolution: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ContradictionSeverity {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectPattern {
    pub pattern_type: String,
    pub description: String,
    pub examples: Vec<String>,
    pub source_session: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidatedInsight {
    pub summary: String,
    pub from_sessions: Vec<String>,
    pub timestamp: DateTime<Utc>,
}

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
        };
        crate::core::memory_lifecycle::run_lifecycle(&mut self.facts, &cfg)
    }

    pub fn new(project_root: &str) -> Self {
        Self {
            project_root: project_root.to_string(),
            project_hash: hash_project_root(project_root),
            facts: Vec::new(),
            patterns: Vec::new(),
            history: Vec::new(),
            updated_at: Utc::now(),
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
            if existing.value == value {
                existing.last_confirmed = Utc::now();
                existing.source_session = session_id.to_string();
                existing.confidence = f32::midpoint(existing.confidence, confidence);
                existing.confirmation_count += 1;
            } else if existing.confidence >= 0.9 && existing.confirmation_count >= 2 {
                existing.valid_until = Some(Utc::now());
                let superseded_id = format!("{}/{}", existing.category, existing.key);
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
                    supersedes: Some(superseded_id),
                    confirmation_count: 1,
                    feedback_up: 0,
                    feedback_down: 0,
                    last_feedback: None,
                });
            } else {
                existing.value = value.to_string();
                existing.confidence = confidence;
                existing.last_confirmed = Utc::now();
                existing.source_session = session_id.to_string();
                existing.valid_from = existing.valid_from.or(Some(existing.created_at));
                existing.confirmation_count = 1;
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
            });
        }

        // No hard-prune: archive-only lifecycle will compact if needed.
        if self.facts.len() > policy.knowledge.max_facts.saturating_mul(2) {
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

    pub fn recall(&self, query: &str) -> Vec<&KnowledgeFact> {
        let q = query.to_lowercase();
        let terms: Vec<&str> = q.split_whitespace().collect();

        let mut results: Vec<(&KnowledgeFact, f32)> = self
            .facts
            .iter()
            .filter(|f| f.is_current())
            .filter_map(|f| {
                let searchable = format!(
                    "{} {} {} {}",
                    f.category.to_lowercase(),
                    f.key.to_lowercase(),
                    f.value.to_lowercase(),
                    f.source_session
                );
                let match_count = terms.iter().filter(|t| searchable.contains(**t)).count();
                if match_count > 0 {
                    let relevance = (match_count as f32 / terms.len() as f32) * f.quality_score();
                    Some((f, relevance))
                } else {
                    None
                }
            })
            .collect();

        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.into_iter().map(|(f, _)| f).collect()
    }

    pub fn recall_by_category(&self, category: &str) -> Vec<&KnowledgeFact> {
        self.facts
            .iter()
            .filter(|f| f.category == category && f.is_current())
            .collect()
    }

    pub fn recall_at_time(&self, query: &str, at: DateTime<Utc>) -> Vec<&KnowledgeFact> {
        let q = query.to_lowercase();
        let terms: Vec<&str> = q.split_whitespace().collect();

        let mut results: Vec<(&KnowledgeFact, f32)> = self
            .facts
            .iter()
            .filter(|f| f.was_valid_at(at))
            .filter_map(|f| {
                let searchable = format!(
                    "{} {} {}",
                    f.category.to_lowercase(),
                    f.key.to_lowercase(),
                    f.value.to_lowercase(),
                );
                let match_count = terms.iter().filter(|t| searchable.contains(**t)).count();
                if match_count > 0 {
                    Some((f, match_count as f32 / terms.len() as f32))
                } else {
                    None
                }
            })
            .collect();

        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.into_iter().map(|(f, _)| f).collect()
    }

    pub fn timeline(&self, category: &str) -> Vec<&KnowledgeFact> {
        let mut facts: Vec<&KnowledgeFact> = self
            .facts
            .iter()
            .filter(|f| f.category == category)
            .collect();
        facts.sort_by_key(|x| x.created_at);
        facts
    }

    pub fn list_rooms(&self) -> Vec<(String, usize)> {
        let mut categories: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for f in &self.facts {
            if f.is_current() {
                *categories.entry(f.category.clone()).or_insert(0) += 1;
            }
        }
        categories.into_iter().collect()
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

    pub fn consolidate(&mut self, summary: &str, session_ids: Vec<String>, policy: &MemoryPolicy) {
        self.history.push(ConsolidatedInsight {
            summary: summary.to_string(),
            from_sessions: session_ids,
            timestamp: Utc::now(),
        });

        if self.history.len() > policy.knowledge.max_history {
            self.history
                .drain(0..self.history.len() - policy.knowledge.max_history);
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

    pub fn format_summary(&self) -> String {
        let mut out = String::new();
        let current_facts: Vec<&KnowledgeFact> =
            self.facts.iter().filter(|f| f.is_current()).collect();

        if !current_facts.is_empty() {
            out.push_str("PROJECT KNOWLEDGE:\n");
            let mut rooms: Vec<(String, usize)> = self.list_rooms();
            rooms.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

            let total_rooms = rooms.len();
            rooms.truncate(crate::core::budgets::KNOWLEDGE_SUMMARY_ROOMS_LIMIT);

            for (cat, _count) in rooms {
                out.push_str(&format!("  [{cat}]\n"));

                let mut facts_in_cat: Vec<&KnowledgeFact> = current_facts
                    .iter()
                    .copied()
                    .filter(|f| f.category == cat)
                    .collect();
                facts_in_cat.sort_by(|a, b| sort_fact_for_output(a, b));

                let total_in_cat = facts_in_cat.len();
                facts_in_cat.truncate(crate::core::budgets::KNOWLEDGE_SUMMARY_FACTS_PER_ROOM_LIMIT);

                for f in facts_in_cat {
                    let key = crate::core::sanitize::neutralize_metadata(&f.key);
                    let val = crate::core::sanitize::neutralize_metadata(&f.value);
                    out.push_str(&format!(
                        "    {}: {} (confidence: {:.0}%)\n",
                        key,
                        val,
                        f.confidence * 100.0
                    ));
                }
                if total_in_cat > crate::core::budgets::KNOWLEDGE_SUMMARY_FACTS_PER_ROOM_LIMIT {
                    out.push_str(&format!(
                        "    … +{} more\n",
                        total_in_cat - crate::core::budgets::KNOWLEDGE_SUMMARY_FACTS_PER_ROOM_LIMIT
                    ));
                }
            }

            if total_rooms > crate::core::budgets::KNOWLEDGE_SUMMARY_ROOMS_LIMIT {
                out.push_str(&format!(
                    "  … +{} more rooms\n",
                    total_rooms - crate::core::budgets::KNOWLEDGE_SUMMARY_ROOMS_LIMIT
                ));
            }
        }

        if !self.patterns.is_empty() {
            out.push_str("PROJECT PATTERNS:\n");
            let mut patterns = self.patterns.clone();
            patterns.sort_by(|a, b| {
                b.created_at
                    .cmp(&a.created_at)
                    .then_with(|| a.pattern_type.cmp(&b.pattern_type))
                    .then_with(|| a.description.cmp(&b.description))
            });
            let total = patterns.len();
            patterns.truncate(crate::core::budgets::KNOWLEDGE_PATTERNS_LIMIT);
            for p in &patterns {
                let ty = crate::core::sanitize::neutralize_metadata(&p.pattern_type);
                let desc = crate::core::sanitize::neutralize_metadata(&p.description);
                out.push_str(&format!("  [{ty}] {desc}\n"));
            }
            if total > crate::core::budgets::KNOWLEDGE_PATTERNS_LIMIT {
                out.push_str(&format!(
                    "  … +{} more\n",
                    total - crate::core::budgets::KNOWLEDGE_PATTERNS_LIMIT
                ));
            }
        }

        if out.is_empty() {
            out
        } else {
            crate::core::sanitize::fence_content("project_knowledge", out.trim_end())
        }
    }

    pub fn format_aaak(&self) -> String {
        let current_facts: Vec<&KnowledgeFact> =
            self.facts.iter().filter(|f| f.is_current()).collect();

        if current_facts.is_empty() && self.patterns.is_empty() {
            return String::new();
        }

        let mut out = String::new();

        let mut rooms: Vec<(String, usize)> = self.list_rooms();
        rooms.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        rooms.truncate(crate::core::budgets::KNOWLEDGE_AAAK_ROOMS_LIMIT);

        for (cat, _count) in rooms {
            let mut facts_in_cat: Vec<&KnowledgeFact> = current_facts
                .iter()
                .copied()
                .filter(|f| f.category == cat)
                .collect();
            facts_in_cat.sort_by(|a, b| sort_fact_for_output(a, b));
            facts_in_cat.truncate(crate::core::budgets::KNOWLEDGE_AAAK_FACTS_PER_ROOM_LIMIT);

            let items: Vec<String> = facts_in_cat
                .iter()
                .map(|f| {
                    let stars = confidence_stars(f.confidence);
                    let key = crate::core::sanitize::neutralize_metadata(&f.key);
                    let val = crate::core::sanitize::neutralize_metadata(&f.value);
                    format!("{key}={val}{stars}")
                })
                .collect();
            out.push_str(&format!(
                "{}:{}\n",
                crate::core::sanitize::neutralize_metadata(&cat.to_uppercase()),
                items.join("|")
            ));
        }

        if !self.patterns.is_empty() {
            let mut patterns = self.patterns.clone();
            patterns.sort_by(|a, b| {
                b.created_at
                    .cmp(&a.created_at)
                    .then_with(|| a.pattern_type.cmp(&b.pattern_type))
                    .then_with(|| a.description.cmp(&b.description))
            });
            patterns.truncate(crate::core::budgets::KNOWLEDGE_PATTERNS_LIMIT);
            let pat_items: Vec<String> = patterns
                .iter()
                .map(|p| {
                    let ty = crate::core::sanitize::neutralize_metadata(&p.pattern_type);
                    let desc = crate::core::sanitize::neutralize_metadata(&p.description);
                    format!("{ty}.{desc}")
                })
                .collect();
            out.push_str(&format!("PAT:{}\n", pat_items.join("|")));
        }

        if out.is_empty() {
            out
        } else {
            crate::core::sanitize::fence_content("project_memory_aaak", out.trim_end())
        }
    }

    pub fn format_wakeup(&self) -> String {
        let current_facts: Vec<&KnowledgeFact> = self
            .facts
            .iter()
            .filter(|f| f.is_current() && f.confidence >= 0.7)
            .collect();

        if current_facts.is_empty() {
            return String::new();
        }

        let mut top_facts: Vec<&KnowledgeFact> = current_facts;
        top_facts.sort_by(|a, b| sort_fact_for_output(a, b));
        top_facts.truncate(10);

        let items: Vec<String> = top_facts
            .iter()
            .map(|f| {
                let cat = crate::core::sanitize::neutralize_metadata(&f.category);
                let key = crate::core::sanitize::neutralize_metadata(&f.key);
                let val = crate::core::sanitize::neutralize_metadata(&f.value);
                format!("{cat}/{key}={val}")
            })
            .collect();

        crate::core::sanitize::fence_content(
            "project_facts_wakeup",
            &format!("FACTS:{}", items.join("|")),
        )
    }

    pub fn save(&self) -> Result<(), String> {
        let dir = knowledge_dir(&self.project_hash)?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

        let path = dir.join("knowledge.json");
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| e.to_string())
    }

    pub fn load(project_root: &str) -> Option<Self> {
        let hash = hash_project_root(project_root);
        let dir = knowledge_dir(&hash).ok()?;
        let path = dir.join("knowledge.json");

        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(k) = serde_json::from_str::<Self>(&content) {
                return Some(k);
            }
        }

        let old_hash = crate::core::project_hash::hash_path_only(project_root);
        if old_hash != hash {
            crate::core::project_hash::migrate_if_needed(&old_hash, &hash, project_root);
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(mut k) = serde_json::from_str::<Self>(&content) {
                    k.project_hash = hash;
                    let _ = k.save();
                    return Some(k);
                }
            }
        }

        None
    }

    pub fn load_or_create(project_root: &str) -> Self {
        Self::load(project_root).unwrap_or_else(|| Self::new(project_root))
    }

    /// Migrates legacy knowledge that was accidentally stored under an empty project_root ("")
    /// into the given `target_root`. Keeps a timestamped backup of the legacy file.
    pub fn migrate_legacy_empty_root(
        target_root: &str,
        policy: &MemoryPolicy,
    ) -> Result<bool, String> {
        if target_root.trim().is_empty() {
            return Ok(false);
        }

        let Some(legacy) = Self::load("") else {
            return Ok(false);
        };

        if !legacy.project_root.trim().is_empty() {
            return Ok(false);
        }
        if legacy.facts.is_empty() && legacy.patterns.is_empty() && legacy.history.is_empty() {
            return Ok(false);
        }

        let mut target = Self::load_or_create(target_root);

        fn fact_key(f: &KnowledgeFact) -> String {
            format!(
                "{}|{}|{}|{}|{}",
                f.category, f.key, f.value, f.source_session, f.created_at
            )
        }
        fn pattern_key(p: &ProjectPattern) -> String {
            format!(
                "{}|{}|{}|{}",
                p.pattern_type, p.description, p.source_session, p.created_at
            )
        }
        fn history_key(h: &ConsolidatedInsight) -> String {
            format!(
                "{}|{}|{}",
                h.summary,
                h.from_sessions.join(","),
                h.timestamp
            )
        }

        let mut seen_facts: std::collections::HashSet<String> =
            target.facts.iter().map(fact_key).collect();
        for f in legacy.facts {
            if seen_facts.insert(fact_key(&f)) {
                target.facts.push(f);
            }
        }

        let mut seen_patterns: std::collections::HashSet<String> =
            target.patterns.iter().map(pattern_key).collect();
        for p in legacy.patterns {
            if seen_patterns.insert(pattern_key(&p)) {
                target.patterns.push(p);
            }
        }

        let mut seen_history: std::collections::HashSet<String> =
            target.history.iter().map(history_key).collect();
        for h in legacy.history {
            if seen_history.insert(history_key(&h)) {
                target.history.push(h);
            }
        }

        // Enforce caps to avoid unbounded growth from migration.
        target.facts.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.confidence.total_cmp(&a.confidence))
        });
        if target.facts.len() > policy.knowledge.max_facts {
            target.facts.truncate(policy.knowledge.max_facts);
        }
        target
            .patterns
            .sort_by_key(|x| std::cmp::Reverse(x.created_at));
        if target.patterns.len() > policy.knowledge.max_patterns {
            target.patterns.truncate(policy.knowledge.max_patterns);
        }
        target
            .history
            .sort_by_key(|x| std::cmp::Reverse(x.timestamp));
        if target.history.len() > policy.knowledge.max_history {
            target.history.truncate(policy.knowledge.max_history);
        }

        target.updated_at = Utc::now();
        target.save()?;

        let legacy_hash = crate::core::project_hash::hash_path_only("");
        let legacy_dir = knowledge_dir(&legacy_hash)?;
        let legacy_path = legacy_dir.join("knowledge.json");
        if legacy_path.exists() {
            let ts = Utc::now().format("%Y%m%d-%H%M%S");
            let backup = legacy_dir.join(format!("knowledge.legacy-empty-root.{ts}.json"));
            std::fs::rename(&legacy_path, &backup).map_err(|e| e.to_string())?;
        }

        Ok(true)
    }

    pub fn recall_for_output(&mut self, query: &str, limit: usize) -> (Vec<KnowledgeFact>, usize) {
        let q = query.to_lowercase();
        let terms: Vec<&str> = q.split_whitespace().filter(|t| !t.is_empty()).collect();
        if terms.is_empty() {
            return (Vec::new(), 0);
        }

        struct Scored {
            idx: usize,
            relevance: f32,
        }

        let mut scored: Vec<Scored> = self
            .facts
            .iter()
            .enumerate()
            .filter(|(_, f)| f.is_current())
            .filter_map(|(idx, f)| {
                let searchable = format!(
                    "{} {} {} {}",
                    f.category.to_lowercase(),
                    f.key.to_lowercase(),
                    f.value.to_lowercase(),
                    f.source_session
                );
                let match_count = terms.iter().filter(|t| searchable.contains(**t)).count();
                if match_count > 0 {
                    let relevance = (match_count as f32 / terms.len() as f32) * f.confidence;
                    Some(Scored { idx, relevance })
                } else {
                    None
                }
            })
            .collect();

        scored.sort_by(|a, b| {
            b.relevance
                .partial_cmp(&a.relevance)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| sort_fact_for_output(&self.facts[a.idx], &self.facts[b.idx]))
        });

        let total = scored.len();
        scored.truncate(limit);

        let now = Utc::now();
        let mut out: Vec<KnowledgeFact> = Vec::new();
        for s in scored {
            if let Some(f) = self.facts.get_mut(s.idx) {
                f.retrieval_count = f.retrieval_count.saturating_add(1);
                f.last_retrieved = Some(now);
                out.push(f.clone());
            }
        }

        (out, total)
    }

    pub fn recall_by_category_for_output(
        &mut self,
        category: &str,
        limit: usize,
    ) -> (Vec<KnowledgeFact>, usize) {
        let mut idxs: Vec<usize> = self
            .facts
            .iter()
            .enumerate()
            .filter(|(_, f)| f.is_current() && f.category == category)
            .map(|(i, _)| i)
            .collect();

        idxs.sort_by(|a, b| sort_fact_for_output(&self.facts[*a], &self.facts[*b]));

        let total = idxs.len();
        idxs.truncate(limit);

        let now = Utc::now();
        let mut out = Vec::new();
        for idx in idxs {
            if let Some(f) = self.facts.get_mut(idx) {
                f.retrieval_count = f.retrieval_count.saturating_add(1);
                f.last_retrieved = Some(now);
                out.push(f.clone());
            }
        }

        (out, total)
    }
}

impl KnowledgeFact {
    pub fn is_current(&self) -> bool {
        self.valid_until.is_none()
    }

    pub fn quality_score(&self) -> f32 {
        let confidence = self.confidence.clamp(0.0, 1.0);
        let confirmations_norm = (self.confirmation_count.min(5) as f32) / 5.0;
        let balance = self.feedback_up as i32 - self.feedback_down as i32;
        let feedback_effect = (balance as f32 / 4.0).tanh() * 0.1;

        // IMPORTANT: quality_score must be stable across repeated recall calls.
        // Retrieval signals (retrieval_count/last_retrieved) are persisted, but should not change
        // the displayed "quality" score, otherwise recall output becomes non-deterministic.
        (0.8 * confidence + 0.2 * confirmations_norm + feedback_effect).clamp(0.0, 1.0)
    }

    pub fn was_valid_at(&self, at: DateTime<Utc>) -> bool {
        let after_start = self.valid_from.is_none_or(|from| at >= from);
        let before_end = self.valid_until.is_none_or(|until| at <= until);
        after_start && before_end
    }
}

fn confidence_stars(confidence: f32) -> &'static str {
    if confidence >= 0.95 {
        "★★★★★"
    } else if confidence >= 0.85 {
        "★★★★"
    } else if confidence >= 0.7 {
        "★★★"
    } else if confidence >= 0.5 {
        "★★"
    } else {
        "★"
    }
}

fn string_similarity(a: &str, b: &str) -> f32 {
    let a_lower = a.to_lowercase();
    let b_lower = b.to_lowercase();
    let a_words: std::collections::HashSet<&str> = a_lower.split_whitespace().collect();
    let b_words: std::collections::HashSet<&str> = b_lower.split_whitespace().collect();

    if a_words.is_empty() && b_words.is_empty() {
        return 1.0;
    }

    let intersection = a_words.intersection(&b_words).count();
    let union = a_words.union(&b_words).count();

    if union == 0 {
        return 0.0;
    }

    intersection as f32 / union as f32
}

fn knowledge_dir(project_hash: &str) -> Result<PathBuf, String> {
    Ok(crate::core::data_dir::lean_ctx_data_dir()?
        .join("knowledge")
        .join(project_hash))
}

fn sort_fact_for_output(a: &KnowledgeFact, b: &KnowledgeFact) -> std::cmp::Ordering {
    salience_score(b)
        .cmp(&salience_score(a))
        .then_with(|| {
            b.quality_score()
                .partial_cmp(&a.quality_score())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .then_with(|| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .then_with(|| b.confirmation_count.cmp(&a.confirmation_count))
        .then_with(|| b.retrieval_count.cmp(&a.retrieval_count))
        .then_with(|| b.last_retrieved.cmp(&a.last_retrieved))
        .then_with(|| b.last_confirmed.cmp(&a.last_confirmed))
        .then_with(|| a.category.cmp(&b.category))
        .then_with(|| a.key.cmp(&b.key))
        .then_with(|| a.value.cmp(&b.value))
}

fn salience_score(f: &KnowledgeFact) -> u32 {
    let cat = f.category.to_lowercase();
    let base: u32 = match cat.as_str() {
        "decision" => 70,
        "gotcha" => 75,
        "architecture" | "arch" => 60,
        "security" => 65,
        "testing" | "tests" | "deployment" | "deploy" => 55,
        "conventions" | "convention" => 45,
        "finding" => 40,
        _ => 30,
    };

    let quality_bonus = (f.quality_score() * 60.0) as u32;

    let recency_bonus = f.last_retrieved.map_or(0u32, |t| {
        let days = Utc::now().signed_duration_since(t).num_days();
        if days <= 7 {
            10u32
        } else if days <= 30 {
            5u32
        } else {
            0u32
        }
    });

    base + quality_bonus + recency_bonus
}

fn hash_project_root(root: &str) -> String {
    crate::core::project_hash::hash_project_root(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_policy() -> MemoryPolicy {
        MemoryPolicy::default()
    }

    #[test]
    fn remember_and_recall() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test-project");
        k.remember(
            "architecture",
            "auth",
            "JWT RS256",
            "session-1",
            0.9,
            &policy,
        );
        k.remember("api", "rate-limit", "100/min", "session-1", 0.8, &policy);

        let results = k.recall("auth");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].value, "JWT RS256");

        let results = k.recall("api rate");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "rate-limit");
    }

    #[test]
    fn upsert_existing_fact() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.7, &policy);
        k.remember(
            "arch",
            "db",
            "PostgreSQL 16 with pgvector",
            "s2",
            0.95,
            &policy,
        );

        let current: Vec<_> = k.facts.iter().filter(|f| f.is_current()).collect();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].value, "PostgreSQL 16 with pgvector");
    }

    #[test]
    fn contradiction_detection() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.95, &policy);
        k.facts[0].confirmation_count = 3;

        let contradiction = k.check_contradiction("arch", "db", "MySQL", &policy);
        assert!(contradiction.is_some());
        let c = contradiction.unwrap();
        assert_eq!(c.severity, ContradictionSeverity::High);
    }

    #[test]
    fn temporal_validity() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.95, &policy);
        k.facts[0].confirmation_count = 3;

        k.remember("arch", "db", "MySQL", "s2", 0.9, &policy);

        let current: Vec<_> = k.facts.iter().filter(|f| f.is_current()).collect();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].value, "MySQL");

        let all_db: Vec<_> = k.facts.iter().filter(|f| f.key == "db").collect();
        assert_eq!(all_db.len(), 2);
    }

    #[test]
    fn confirmation_count() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.9, &policy);
        assert_eq!(k.facts[0].confirmation_count, 1);

        k.remember("arch", "db", "PostgreSQL", "s2", 0.9, &policy);
        assert_eq!(k.facts[0].confirmation_count, 2);
    }

    #[test]
    fn remove_fact() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.9, &policy);
        assert!(k.remove_fact("arch", "db"));
        assert!(k.facts.is_empty());
        assert!(!k.remove_fact("arch", "db"));
    }

    #[test]
    fn list_rooms() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("architecture", "auth", "JWT", "s1", 0.9, &policy);
        k.remember("architecture", "db", "PG", "s1", 0.9, &policy);
        k.remember("deploy", "host", "AWS", "s1", 0.8, &policy);

        let rooms = k.list_rooms();
        assert_eq!(rooms.len(), 2);
    }

    #[test]
    fn aaak_format() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("architecture", "auth", "JWT RS256", "s1", 0.95, &policy);
        k.remember("architecture", "db", "PostgreSQL", "s1", 0.7, &policy);

        let aaak = k.format_aaak();
        assert!(aaak.contains("ARCHITECTURE:"));
        assert!(aaak.contains("auth=JWT RS256"));
    }

    #[test]
    fn consolidate_history() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.consolidate(
            "Migrated from REST to GraphQL",
            vec!["s1".into(), "s2".into()],
            &policy,
        );
        assert_eq!(k.history.len(), 1);
        assert_eq!(k.history[0].from_sessions.len(), 2);
    }

    #[test]
    fn format_summary_output() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("architecture", "auth", "JWT RS256", "s1", 0.9, &policy);
        k.add_pattern(
            "naming",
            "snake_case for functions",
            vec!["get_user()".into()],
            "s1",
            &policy,
        );
        let summary = k.format_summary();
        assert!(summary.contains("PROJECT KNOWLEDGE:"));
        assert!(summary.contains("auth: JWT RS256"));
        assert!(summary.contains("PROJECT PATTERNS:"));
    }

    #[test]
    fn temporal_recall_at_time() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.95, &policy);
        k.facts[0].confirmation_count = 3;

        let before_change = Utc::now();
        std::thread::sleep(std::time::Duration::from_millis(10));

        k.remember("arch", "db", "MySQL", "s2", 0.9, &policy);

        let results = k.recall_at_time("db", before_change);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].value, "PostgreSQL");

        let results_now = k.recall_at_time("db", Utc::now());
        assert_eq!(results_now.len(), 1);
        assert_eq!(results_now[0].value, "MySQL");
    }

    #[test]
    fn timeline_shows_history() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.95, &policy);
        k.facts[0].confirmation_count = 3;
        k.remember("arch", "db", "MySQL", "s2", 0.9, &policy);

        let timeline = k.timeline("arch");
        assert_eq!(timeline.len(), 2);
        assert!(!timeline[0].is_current());
        assert!(timeline[1].is_current());
    }

    #[test]
    fn wakeup_format() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "auth", "JWT", "s1", 0.95, &policy);
        k.remember("arch", "db", "PG", "s1", 0.8, &policy);

        let wakeup = k.format_wakeup();
        assert!(wakeup.contains("FACTS:"));
        assert!(wakeup.contains("arch/auth=JWT"));
        assert!(wakeup.contains("arch/db=PG"));
    }

    #[test]
    fn salience_prioritizes_decisions_over_findings_at_similar_confidence() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("finding", "f1", "some thing", "s1", 0.9, &policy);
        k.remember("decision", "d1", "important", "s1", 0.85, &policy);

        let wakeup = k.format_wakeup();
        let items = wakeup
            .strip_prefix("FACTS:")
            .unwrap_or(&wakeup)
            .split('|')
            .collect::<Vec<_>>();
        assert!(
            items
                .first()
                .is_some_and(|s| s.contains("decision/d1=important")),
            "expected decision first in wakeup: {wakeup}"
        );
    }

    #[test]
    fn low_confidence_contradiction() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.4, &policy);

        let c = k.check_contradiction("arch", "db", "MySQL", &policy);
        assert!(c.is_some());
        assert_eq!(c.unwrap().severity, ContradictionSeverity::Low);
    }

    #[test]
    fn no_contradiction_for_same_value() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.95, &policy);

        let c = k.check_contradiction("arch", "db", "PostgreSQL", &policy);
        assert!(c.is_none());
    }

    #[test]
    fn no_contradiction_for_similar_values() {
        let policy = default_policy();
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember(
            "arch",
            "db",
            "PostgreSQL 16 production database server",
            "s1",
            0.95,
            &policy,
        );

        let c = k.check_contradiction(
            "arch",
            "db",
            "PostgreSQL 16 production database server config",
            &policy,
        );
        assert!(c.is_none());
    }
}
