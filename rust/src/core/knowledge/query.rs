use chrono::{DateTime, Duration, Utc};
use lean_ctx_protocol::knowledge::{ClassificationLevel, KnowledgeObjectV1};

use super::ranking::sort_fact_for_output;
use super::types::{KnowledgeFact, ProjectKnowledge};
use crate::core::cognitive_gate::full_science_enabled;
use crate::core::memory_scheduler::{initial_state, retrievability};

const DEFAULT_ELAPSED_DAYS: f64 = 7.0;

/// Filters understood by portable Knowledge Hub stores.
#[derive(Debug, Clone, Default)]
pub struct KnowledgeQuery {
    pub source: Option<String>,
    pub classification: Option<ClassificationLevel>,
    pub valid_at: Option<DateTime<Utc>>,
    pub tags: Vec<String>,
}

impl KnowledgeQuery {
    /// Match objects that are valid and not superseded at `timestamp`.
    pub fn valid_at(timestamp: DateTime<Utc>) -> Self {
        Self {
            valid_at: Some(timestamp),
            ..Self::default()
        }
    }

    /// Return whether an object satisfies every configured filter.
    pub fn matches(&self, object: &KnowledgeObjectV1) -> bool {
        if self.source.as_ref().is_some_and(|source| {
            object
                .source_ref
                .as_ref()
                .is_none_or(|reference| reference.uri != *source)
        }) {
            return false;
        }
        if self.classification.is_some_and(|level| {
            object
                .classification
                .as_ref()
                .is_none_or(|classification| classification.level != level)
        }) {
            return false;
        }
        if let Some(timestamp) = self.valid_at {
            let Some(validity) = &object.validity else {
                return false;
            };
            if validity.superseded_by.is_some() {
                return false;
            }
            let Ok(valid_from) = DateTime::parse_from_rfc3339(&validity.valid_from) else {
                return false;
            };
            if timestamp < valid_from.with_timezone(&Utc) {
                return false;
            }
            if let Some(valid_until) = &validity.valid_until {
                let Ok(valid_until) = DateTime::parse_from_rfc3339(valid_until) else {
                    return false;
                };
                if timestamp > valid_until.with_timezone(&Utc) {
                    return false;
                }
            }
        }
        let object_tags = object
            .extra
            .get("tags")
            .and_then(serde_json::Value::as_array);
        self.tags.iter().all(|tag| {
            object_tags.is_some_and(|tags| tags.iter().any(|value| value.as_str() == Some(tag)))
        })
    }
}

fn fact_elapsed_days(fact: &KnowledgeFact, now: DateTime<Utc>) -> f64 {
    match fact.last_retrieved {
        Some(ts) => ((now - ts).num_seconds() as f64 / 86_400.0).max(0.0),
        None => DEFAULT_ELAPSED_DAYS,
    }
}

fn fsrs_boosted_relevance(fact: &KnowledgeFact, relevance: f32, now: DateTime<Utc>) -> f32 {
    let elapsed_days = fact_elapsed_days(fact, now);
    let elapsed_secs = (elapsed_days * 86_400.0).round() as i64;
    let mut state = initial_state(fact.key.clone(), 3);
    state.last_review = now - Duration::seconds(elapsed_secs);
    let ret = retrievability(&state, now).clamp(0.0, 1.0);
    let multiplier = (1.5_f64 - ret).max(0.1_f64) as f32;
    relevance * multiplier
}

/// Words that frame a task rather than name its subject: function words and
/// the action verbs nearly every task starts with.
const TASK_FILLER_WORDS: &[&str] = &[
    "a",
    "an",
    "and",
    "are",
    "as",
    "at",
    "be",
    "by",
    "for",
    "from",
    "how",
    "i",
    "in",
    "into",
    "is",
    "it",
    "its",
    "me",
    "my",
    "of",
    "on",
    "or",
    "our",
    "please",
    "so",
    "that",
    "the",
    "then",
    "this",
    "to",
    "up",
    "we",
    "what",
    "when",
    "where",
    "which",
    "why",
    "with",
    // Generic task verbs. Words that are just as often the subject ("build",
    // "test", "run", "support") stay content words.
    "add",
    "analyse",
    "analyze",
    "change",
    "check",
    "create",
    "debug",
    "explain",
    "explore",
    "find",
    "fix",
    "implement",
    "improve",
    "inspect",
    "investigate",
    "look",
    "make",
    "refactor",
    "remove",
    "rename",
    "review",
    "see",
    "show",
    "understand",
    "update",
    "verify",
    "write",
];

/// The task's content words, lowercased and split like the knowledge index
/// splits facts, with surrounding punctuation trimmed ("validation." →
/// "validation") and filler words dropped. Order-preserving and deduplicated.
fn task_content_terms(task: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for token in super::ranking::tokenize_lower(task) {
        let term = token.trim_matches(|c: char| !c.is_alphanumeric());
        if term.is_empty() || TASK_FILLER_WORDS.contains(&term) {
            continue;
        }
        if !terms.iter().any(|t| t == term) {
            terms.push(term.to_string());
        }
    }
    terms
}

impl ProjectKnowledge {
    fn matching_indices(&self, term: &str, include_session: bool) -> Vec<usize> {
        let Some(indices) = self.index.token_positions.get(term) else {
            return if include_session {
                self.index
                    .session_token_positions
                    .get(term)
                    .cloned()
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
        };

        if !include_session {
            return indices.clone();
        }

        let Some(session_indices) = self.index.session_token_positions.get(term) else {
            return indices.clone();
        };
        let mut merged = indices.clone();
        merged.extend(
            session_indices
                .iter()
                .copied()
                .filter(|idx| indices.binary_search(idx).is_err()),
        );
        merged
    }

    pub fn recall(&self, query: &str) -> Vec<&KnowledgeFact> {
        let q = query.to_lowercase();
        let terms: Vec<&str> = q.split_whitespace().collect();
        if terms.is_empty() {
            return Vec::new();
        }

        let mut match_counts: std::collections::HashMap<usize, usize> =
            std::collections::HashMap::new();
        for term in &terms {
            for idx in self.matching_indices(term, true) {
                if self.facts[idx].is_current() {
                    *match_counts.entry(idx).or_insert(0) += 1;
                }
            }
        }

        let mut results: Vec<(&KnowledgeFact, f32)> = match_counts
            .into_iter()
            .map(|(idx, count)| {
                let f = &self.facts[idx];
                let relevance = (count as f32 / terms.len() as f32) * f.quality_score();
                (f, relevance)
            })
            .collect();

        if full_science_enabled() {
            let now = Utc::now();
            results = results
                .into_iter()
                .map(|(f, relevance)| (f, fsrs_boosted_relevance(f, relevance, now)))
                .collect();
        }

        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.into_iter().map(|(f, _)| f).collect()
    }

    /// Recall for a free-text task description (overview, sub-agent briefing).
    ///
    /// Unlike [`Self::recall`], which serves an explicit knowledge query, a
    /// task sentence is mostly framing: "Inspect alpha parser header
    /// validation." shares only "inspect" with an unrelated fact about
    /// deployments, and that single word used to be enough to present the
    /// fact as relevant (#1832). Only the task's content words are matched,
    /// tokenized the way the index is, so a task made of nothing but generic
    /// words recalls nothing.
    pub fn recall_for_task(&self, task: &str) -> Vec<&KnowledgeFact> {
        let terms = task_content_terms(task);
        if terms.is_empty() {
            return Vec::new();
        }
        self.recall(&terms.join(" "))
    }

    pub fn recall_by_category(&self, category: &str) -> Vec<&KnowledgeFact> {
        self.index
            .category_positions
            .get(category)
            .into_iter()
            .flatten()
            .filter_map(|&idx| self.facts.get(idx))
            .filter(|f| f.is_current())
            .collect()
    }

    pub fn recall_at_time(&self, query: &str, at: DateTime<Utc>) -> Vec<&KnowledgeFact> {
        let q = query.to_lowercase();
        let terms: Vec<&str> = q.split_whitespace().collect();
        if terms.is_empty() {
            return Vec::new();
        }

        let mut match_counts: std::collections::HashMap<usize, usize> =
            std::collections::HashMap::new();
        for term in &terms {
            for idx in self.matching_indices(term, false) {
                if self.facts[idx].was_valid_at(at) {
                    *match_counts.entry(idx).or_insert(0) += 1;
                }
            }
        }

        let mut results: Vec<(&KnowledgeFact, f32)> = match_counts
            .into_iter()
            .map(|(idx, count)| {
                let f = &self.facts[idx];
                (f, count as f32 / terms.len() as f32)
            })
            .collect();

        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.into_iter().map(|(f, _)| f).collect()
    }

    pub fn timeline(&self, category: &str) -> Vec<&KnowledgeFact> {
        let mut facts: Vec<&KnowledgeFact> = self
            .index
            .category_positions
            .get(category)
            .into_iter()
            .flatten()
            .filter_map(|&idx| self.facts.get(idx))
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

    pub fn recall_for_output(&mut self, query: &str, limit: usize) -> (Vec<KnowledgeFact>, usize) {
        let q = query.to_lowercase();
        let terms: Vec<&str> = q.split_whitespace().filter(|t| !t.is_empty()).collect();
        if terms.is_empty() {
            return (Vec::new(), 0);
        }

        let mut match_counts: std::collections::HashMap<usize, usize> =
            std::collections::HashMap::new();
        for term in &terms {
            for idx in self.matching_indices(term, true) {
                if self.facts[idx].is_current() {
                    *match_counts.entry(idx).or_insert(0) += 1;
                }
            }
        }

        struct Scored {
            idx: usize,
            relevance: f32,
        }

        let mut scored: Vec<Scored> = match_counts
            .into_iter()
            .map(|(idx, count)| {
                let f = &self.facts[idx];
                let mut relevance = (count as f32 / terms.len() as f32) * f.confidence;
                // Exact-match boost: an exact hit on the fact key (or category)
                // should rank above incidental lexical matches (#2363). The +1.0
                // dominates the [0,1] coverage*confidence base.
                let key_lower = f.key.to_lowercase();
                if key_lower == q {
                    relevance += 1.0;
                } else if f.category.to_lowercase() == q {
                    relevance += 0.5;
                }
                // Observation tier (#802): a relevant synthesized entity-summary is
                // orientation — lift it above incidental matches, but keep it below an
                // exact key hit (+1.0) so a stale summary never buries a precise raw
                // fact. Balanced, not absolute.
                if f.is_synthesized_observation() {
                    relevance += 0.4;
                }
                Scored { idx, relevance }
            })
            .collect();

        let now = Utc::now();
        if full_science_enabled() {
            for s in &mut scored {
                s.relevance = fsrs_boosted_relevance(&self.facts[s.idx], s.relevance, now);
            }
        }

        scored.sort_by(|a, b| {
            b.relevance
                .partial_cmp(&a.relevance)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| sort_fact_for_output(&self.facts[a.idx], &self.facts[b.idx]))
        });

        let total = scored.len();
        scored.truncate(limit);

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
            .index
            .category_positions
            .get(category)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&idx| self.facts[idx].is_current())
            .collect();

        // Within a category, synthesized observation summaries lead (#802) — a
        // balanced tier ahead of the usual salience sort, never an absolute override.
        idxs.sort_by(|a, b| {
            let (fa, fb) = (&self.facts[*a], &self.facts[*b]);
            fb.is_synthesized_observation()
                .cmp(&fa.is_synthesized_observation())
                .then_with(|| sort_fact_for_output(fa, fb))
        });

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
