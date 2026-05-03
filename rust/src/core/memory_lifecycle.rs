//! Memory Lifecycle Management — consolidation, decay, compaction, archival.
//!
//! Runs automatically on knowledge stores to keep memory healthy:
//! - Confidence decay over time
//! - Semantic consolidation of similar facts
//! - Compaction when limits are exceeded
//! - Archival of old/unused facts

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::knowledge::KnowledgeFact;

const DEFAULT_DECAY_RATE: f32 = 0.01;
const DEFAULT_MAX_FACTS: usize = 1000;
const LOW_CONFIDENCE_THRESHOLD: f32 = 0.3;
const STALE_DAYS: i64 = 30;

#[derive(Debug, Clone)]
pub struct LifecycleConfig {
    pub decay_rate_per_day: f32,
    pub max_facts: usize,
    pub low_confidence_threshold: f32,
    pub stale_days: i64,
    pub consolidation_similarity: f32,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            decay_rate_per_day: DEFAULT_DECAY_RATE,
            max_facts: DEFAULT_MAX_FACTS,
            low_confidence_threshold: LOW_CONFIDENCE_THRESHOLD,
            stale_days: STALE_DAYS,
            consolidation_similarity: 0.85,
        }
    }
}

#[derive(Debug, Default)]
pub struct LifecycleReport {
    pub decayed_count: usize,
    pub consolidated_count: usize,
    pub archived_count: usize,
    pub compacted_count: usize,
    pub remaining_facts: usize,
}

pub fn apply_confidence_decay(facts: &mut [KnowledgeFact], config: &LifecycleConfig) -> usize {
    let now = Utc::now();
    let mut count = 0;

    for fact in facts.iter_mut() {
        if !fact.is_current() {
            continue;
        }

        if let Some(valid_until) = fact.valid_until {
            if valid_until < now && fact.confidence > 0.1 {
                fact.confidence = 0.1;
                count += 1;
                continue;
            }
        }

        let days_since_confirmed = now.signed_duration_since(fact.last_confirmed).num_days() as f32;
        let days_since_retrieved = fact
            .last_retrieved
            .map_or(3650.0, |t| now.signed_duration_since(t).num_days() as f32);
        let retrieval_count = fact.retrieval_count as f32;

        if days_since_confirmed > 0.0 {
            // FadeMem-inspired: protect frequently/recently retrieved facts.
            // Deterministic, local-only signals; never hard-delete (archive-only elsewhere).
            let freq_protect = 1.0 / (1.0 + retrieval_count.ln_1p()); // 1.0 .. ~0.2
            let recency_protect = (1.0 - (days_since_retrieved / 30.0).min(1.0)).max(0.0); // 1.0 if today, 0.0 after 30d
            let protect = (freq_protect * (1.0 - 0.5 * recency_protect)).max(0.05);
            let decay = config.decay_rate_per_day * days_since_confirmed * protect;
            let new_confidence = (fact.confidence - decay).max(0.05);
            if (new_confidence - fact.confidence).abs() > 0.001 {
                fact.confidence = new_confidence;
                count += 1;
            }
        }
    }

    count
}

pub fn consolidate_similar(facts: &mut Vec<KnowledgeFact>, similarity_threshold: f32) -> usize {
    let mut to_remove: Vec<usize> = Vec::new();
    let len = facts.len();

    for i in 0..len {
        if to_remove.contains(&i) || !facts[i].is_current() {
            continue;
        }

        for j in (i + 1)..len {
            if to_remove.contains(&j) || !facts[j].is_current() {
                continue;
            }

            if facts[i].category != facts[j].category {
                continue;
            }

            let sim = word_similarity(&facts[i].value, &facts[j].value);
            if sim >= similarity_threshold {
                if facts[i].confidence >= facts[j].confidence {
                    facts[i].confirmation_count += facts[j].confirmation_count;
                    if facts[j].last_confirmed > facts[i].last_confirmed {
                        facts[i].last_confirmed = facts[j].last_confirmed;
                    }
                    to_remove.push(j);
                } else {
                    facts[j].confirmation_count += facts[i].confirmation_count;
                    if facts[i].last_confirmed > facts[j].last_confirmed {
                        facts[j].last_confirmed = facts[i].last_confirmed;
                    }
                    to_remove.push(i);
                    break;
                }
            }
        }
    }

    to_remove.sort_unstable();
    to_remove.dedup();
    let count = to_remove.len();

    for idx in to_remove.into_iter().rev() {
        facts.remove(idx);
    }

    count
}

pub fn compact(
    facts: &mut Vec<KnowledgeFact>,
    config: &LifecycleConfig,
) -> (usize, Vec<KnowledgeFact>) {
    let mut archived: Vec<KnowledgeFact> = Vec::new();
    let now = Utc::now();
    let stale_threshold = now - Duration::days(config.stale_days);

    let mut to_archive: Vec<usize> = Vec::new();

    for (i, fact) in facts.iter().enumerate() {
        let recently_retrieved = fact
            .last_retrieved
            .is_some_and(|t| now.signed_duration_since(t).num_days() < 14);
        let frequently_retrieved = fact.retrieval_count >= 5;

        if fact.confidence < config.low_confidence_threshold {
            to_archive.push(i);
            continue;
        }

        if fact.last_confirmed < stale_threshold
            && fact.confirmation_count <= 1
            && fact.confidence < 0.5
            && !recently_retrieved
            && !frequently_retrieved
        {
            to_archive.push(i);
        }
    }

    to_archive.sort_unstable();
    to_archive.dedup();
    let count = to_archive.len();

    for idx in to_archive.into_iter().rev() {
        archived.push(facts.remove(idx));
    }

    if facts.len() > config.max_facts {
        facts.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let excess: Vec<KnowledgeFact> = facts.drain(config.max_facts..).collect();
        archived.extend(excess);
    }

    (count, archived)
}

pub fn run_lifecycle(facts: &mut Vec<KnowledgeFact>, config: &LifecycleConfig) -> LifecycleReport {
    let decayed = apply_confidence_decay(facts, config);
    let consolidated = consolidate_similar(facts, config.consolidation_similarity);
    let (compacted, archived) = compact(facts, config);

    if !archived.is_empty() {
        let _ = archive_facts(&archived);
    }

    LifecycleReport {
        decayed_count: decayed,
        consolidated_count: consolidated,
        archived_count: archived.len(),
        compacted_count: compacted,
        remaining_facts: facts.len(),
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ArchivedFacts {
    pub archived_at: DateTime<Utc>,
    pub facts: Vec<KnowledgeFact>,
}

fn archive_facts(facts: &[KnowledgeFact]) -> Result<(), String> {
    let dir = crate::core::data_dir::lean_ctx_data_dir()?
        .join("memory")
        .join("archive");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{e}"))?;

    let filename = format!("archive-{}.json", Utc::now().format("%Y%m%d-%H%M%S"));
    let archive = ArchivedFacts {
        archived_at: Utc::now(),
        facts: facts.to_vec(),
    };
    let json = serde_json::to_string_pretty(&archive).map_err(|e| format!("{e}"))?;
    std::fs::write(dir.join(filename), json).map_err(|e| format!("{e}"))
}

pub fn restore_archive(archive_path: &str) -> Result<Vec<KnowledgeFact>, String> {
    let data = std::fs::read_to_string(archive_path).map_err(|e| format!("{e}"))?;
    let archive: ArchivedFacts = serde_json::from_str(&data).map_err(|e| format!("{e}"))?;
    Ok(archive.facts)
}

pub fn list_archives() -> Vec<PathBuf> {
    let dir = match crate::core::data_dir::lean_ctx_data_dir() {
        Ok(d) => d.join("memory").join("archive"),
        Err(_) => return Vec::new(),
    };

    if !dir.exists() {
        return Vec::new();
    }

    let mut archives: Vec<PathBuf> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .map(|e| e.path())
        .collect();

    archives.sort();
    archives
}

fn word_similarity(a: &str, b: &str) -> f32 {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_fact(category: &str, key: &str, value: &str, confidence: f32) -> KnowledgeFact {
        KnowledgeFact {
            category: category.to_string(),
            key: key.to_string(),
            value: value.to_string(),
            source_session: "s1".to_string(),
            confidence,
            created_at: Utc::now(),
            last_confirmed: Utc::now(),
            retrieval_count: 0,
            last_retrieved: None,
            valid_from: Some(Utc::now()),
            valid_until: None,
            supersedes: None,
            confirmation_count: 1,
            feedback_up: 0,
            feedback_down: 0,
            last_feedback: None,
        }
    }

    fn make_old_fact(
        category: &str,
        key: &str,
        value: &str,
        confidence: f32,
        days_old: i64,
    ) -> KnowledgeFact {
        let past = Utc::now() - Duration::days(days_old);
        KnowledgeFact {
            category: category.to_string(),
            key: key.to_string(),
            value: value.to_string(),
            source_session: "s1".to_string(),
            confidence,
            created_at: past,
            last_confirmed: past,
            retrieval_count: 0,
            last_retrieved: None,
            valid_from: Some(past),
            valid_until: None,
            supersedes: None,
            confirmation_count: 1,
            feedback_up: 0,
            feedback_down: 0,
            last_feedback: None,
        }
    }

    #[test]
    fn decay_reduces_confidence() {
        let config = LifecycleConfig::default();
        let mut facts = vec![make_old_fact("arch", "db", "PostgreSQL", 0.9, 10)];

        let count = apply_confidence_decay(&mut facts, &config);
        assert_eq!(count, 1);
        assert!(facts[0].confidence < 0.9);
        assert!(facts[0].confidence > 0.7);
    }

    #[test]
    fn decay_skips_recent_facts() {
        let config = LifecycleConfig::default();
        let mut facts = vec![make_fact("arch", "db", "PostgreSQL", 0.9)];

        let count = apply_confidence_decay(&mut facts, &config);
        assert_eq!(count, 0);
    }

    #[test]
    fn consolidate_similar_facts() {
        let mut facts = vec![
            make_fact("arch", "db", "uses PostgreSQL database", 0.8),
            make_fact("arch", "db2", "uses PostgreSQL database system", 0.6),
            make_fact("ops", "deploy", "docker compose up", 0.9),
        ];

        let count = consolidate_similar(&mut facts, 0.7);
        assert!(count > 0, "Should consolidate similar facts");
        assert!(facts.len() < 3);
    }

    #[test]
    fn consolidate_keeps_different_categories() {
        let mut facts = vec![
            make_fact("arch", "db", "PostgreSQL", 0.8),
            make_fact("ops", "db", "PostgreSQL", 0.8),
        ];

        let count = consolidate_similar(&mut facts, 0.9);
        assert_eq!(count, 0, "Different categories should not consolidate");
    }

    #[test]
    fn compact_removes_low_confidence() {
        let config = LifecycleConfig::default();
        let mut facts = vec![
            make_fact("arch", "db", "PostgreSQL", 0.9),
            make_fact("arch", "cache", "Redis", 0.1),
        ];

        let (count, archived) = compact(&mut facts, &config);
        assert_eq!(count, 1);
        assert_eq!(facts.len(), 1);
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0].key, "cache");
    }

    #[test]
    fn compact_archives_stale_facts() {
        let config = LifecycleConfig::default();
        let mut facts = vec![
            make_fact("arch", "db", "PostgreSQL", 0.9),
            make_old_fact("arch", "old", "ancient thing", 0.4, 60),
        ];

        let (count, archived) = compact(&mut facts, &config);
        assert_eq!(count, 1);
        assert_eq!(archived[0].key, "old");
    }

    #[test]
    fn full_lifecycle_run() {
        let config = LifecycleConfig {
            max_facts: 5,
            ..Default::default()
        };

        let mut facts = vec![
            make_fact("arch", "db", "PostgreSQL", 0.9),
            make_fact("arch", "cache", "Redis", 0.8),
            make_old_fact("arch", "old1", "thing1", 0.2, 50),
            make_old_fact("arch", "old2", "thing2", 0.15, 60),
            make_fact("ops", "deploy", "docker compose", 0.7),
        ];

        let report = run_lifecycle(&mut facts, &config);
        assert!(report.remaining_facts <= config.max_facts);
        assert!(report.decayed_count > 0 || report.compacted_count > 0);
    }

    #[test]
    fn word_similarity_identical() {
        assert!((word_similarity("hello world", "hello world") - 1.0).abs() < 0.01);
    }

    #[test]
    fn word_similarity_partial() {
        let sim = word_similarity("uses PostgreSQL database", "PostgreSQL database system");
        assert!(sim >= 0.5, "Expected >= 0.5 but got {sim}");
        assert!(sim < 1.0);
    }

    #[test]
    fn word_similarity_different() {
        let sim = word_similarity("Redis cache", "Docker compose");
        assert!(sim < 0.1);
    }
}
