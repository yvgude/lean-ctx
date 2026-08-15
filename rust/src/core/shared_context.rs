use std::collections::{BTreeMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// A fact shared between independent agent sessions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SharedContextEntry {
    pub id: String,
    pub content: String,
    pub agent: String,
    pub timestamp: u64,
    pub category: String,
    /// Confidence assigned to this fact, from 0.0 (untrusted) to 1.0 (certain).
    #[serde(default = "default_confidence")]
    pub confidence: f32,
    pub access_count: u32,
    pub last_accessed: u64,
    /// Unix timestamp when an outcome last confirmed this fact.
    #[serde(default)]
    pub last_verified: Option<u64>,
    /// Number of successful verification events recorded for this fact.
    #[serde(default)]
    pub verification_count: u32,
    /// Exponential-moving-average correlation between this fact and successful outcomes.
    #[serde(default)]
    pub outcome_correlation: f32,
    /// Optional per-fact verification window before the fact becomes stale.
    #[serde(default)]
    pub stale_after_days: Option<u16>,
    /// Agent or user that first added this fact.
    #[serde(default)]
    pub created_by: String,
    /// Hash of the fact that supersedes this one.
    #[serde(default)]
    pub superseded_by: Option<String>,
}

/// Backward-compatible name for an entry in shared fact memory.
pub type FactEntry = SharedContextEntry;

/// Summary of the contents of a [`SharedContext`] store.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SharedContextStats {
    pub total_entries: usize,
    pub unique_agents: usize,
    pub categories: BTreeMap<String, usize>,
    pub total_accesses: u64,
}

/// Quality and contributor summary across shared-memory sessions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryStats {
    pub total_facts: usize,
    pub verified_pct: f32,
    pub avg_confidence: f32,
    /// Contributors ordered by descending fact count, then contributor name.
    pub top_contributors: Vec<(String, usize)>,
}

/// Durable cross-agent memory backed by an appendable JSONL file.
#[derive(Debug, Clone)]
pub struct SharedContext {
    path: PathBuf,
}

#[allow(dead_code)]
static STORE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

impl Default for SharedContext {
    fn default() -> Self {
        Self::new(default_path())
    }
}

impl SharedContext {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn default_path() -> PathBuf {
        default_path()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn put(
        &self,
        content: impl Into<String>,
        agent: impl Into<String>,
        category: impl Into<String>,
    ) -> Result<String, String> {
        let content = content.into();
        let agent = agent.into();
        let category = normalize_category(&category.into())?;
        let id = blake3::hash(content.as_bytes()).to_hex().to_string();
        let now = unix_timestamp();
        let mut entries = self.load()?;

        if let Some(existing) = entries.iter_mut().find(|entry| entry.id == id) {
            existing.agent.clone_from(&agent);
            existing.category.clone_from(&category);
            existing.timestamp = now;
            existing.last_accessed = now;
            self.save(&entries)?;
            return Ok(id);
        }

        entries.push(SharedContextEntry {
            id: id.clone(),
            content,
            agent: agent.clone(),
            timestamp: now,
            category,
            confidence: default_confidence(),
            access_count: 0,
            last_accessed: now,
            last_verified: None,
            verification_count: 0,
            outcome_correlation: 0.0,
            stale_after_days: None,
            created_by: agent,
            superseded_by: None,
        });
        self.save(&entries)?;
        Ok(id)
    }

    pub fn get_relevant(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SharedContextEntry>, String> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut entries = self.load()?;
        let query_terms = terms(query);
        if query_terms.is_empty() {
            return Ok(Vec::new());
        }
        let document_count = entries.len() as f64;
        let average_length = entries
            .iter()
            .map(|entry| terms(&entry.content).len() as f64)
            .sum::<f64>()
            / document_count.max(1.0);
        let document_frequency = query_terms
            .iter()
            .map(|term| {
                (
                    term,
                    entries
                        .iter()
                        .filter(|entry| terms(&entry.content).contains(term))
                        .count() as f64,
                )
            })
            .collect::<BTreeMap<_, _>>();

        let mut scored = entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                let entry_terms = terms(&entry.content);
                let length = entry_terms.len() as f64;
                let score = query_terms.iter().fold(0.0, |score, term| {
                    let frequency =
                        entry_terms.iter().filter(|value| *value == term).count() as f64;
                    if frequency == 0.0 {
                        return score;
                    }
                    let df = document_frequency.get(term).copied().unwrap_or_default();
                    let idf = ((document_count - df + 0.5) / (df + 0.5) + 1.0).ln();
                    let k1 = 1.2;
                    let b = 0.75;
                    score
                        + idf * frequency * (k1 + 1.0)
                            / (frequency + k1 * (1.0 - b + b * length / average_length.max(1.0)))
                });
                (score > 0.0).then_some((index, score))
            })
            .collect::<Vec<_>>();
        scored.sort_by(|left, right| {
            right
                .1
                .total_cmp(&left.1)
                .then_with(|| left.0.cmp(&right.0))
        });

        let now = unix_timestamp();
        let selected = scored
            .into_iter()
            .take(limit)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        for index in &selected {
            let entry = &mut entries[*index];
            entry.access_count = entry.access_count.saturating_add(1);
            entry.last_accessed = now;
        }
        let results = selected
            .into_iter()
            .map(|index| entries[index].clone())
            .collect();
        self.save(&entries)?;
        Ok(results)
    }

    pub fn get_recent(&self, limit: usize) -> Result<Vec<SharedContextEntry>, String> {
        let mut entries = self.load()?;
        entries.sort_by(|left, right| {
            right
                .timestamp
                .cmp(&left.timestamp)
                .then_with(|| right.id.cmp(&left.id))
        });
        Ok(entries.into_iter().take(limit).collect())
    }

    pub fn get_by_category(&self, category: &str) -> Result<Vec<SharedContextEntry>, String> {
        let category = normalize_category(category)?;
        let mut entries = self
            .load()?
            .into_iter()
            .filter(|entry| entry.category == category)
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| {
            right
                .timestamp
                .cmp(&left.timestamp)
                .then_with(|| right.id.cmp(&left.id))
        });
        Ok(entries)
    }

    pub fn prune(&self, max_age_seconds: u64, max_entries: usize) -> Result<usize, String> {
        let now = unix_timestamp();
        let mut entries = self.load()?;
        let original_len = entries.len();
        entries.retain(|entry| now.saturating_sub(entry.last_accessed) <= max_age_seconds);
        entries.sort_by(|left, right| {
            right
                .last_accessed
                .cmp(&left.last_accessed)
                .then_with(|| right.access_count.cmp(&left.access_count))
                .then_with(|| right.timestamp.cmp(&left.timestamp))
        });
        entries.truncate(max_entries);
        let removed = original_len.saturating_sub(entries.len());
        self.save(&entries)?;
        Ok(removed)
    }

    /// Record an explicit confirmation of a fact.
    pub fn verify_fact(&self, hash: &str) -> Result<(), String> {
        let mut entries = self.load()?;
        let entry = find_fact_mut(&mut entries, hash)?;
        entry.last_verified = Some(unix_timestamp());
        entry.verification_count = entry.verification_count.saturating_add(1);
        self.save(&entries)
    }

    /// Lower the confidence of a fact identified as stale without removing it.
    pub fn mark_stale(&self, hash: &str) -> Result<(), String> {
        let mut entries = self.load()?;
        find_fact_mut(&mut entries, hash)?.confidence = 0.3;
        self.save(&entries)
    }

    /// Mark `old_hash` as replaced by the existing fact identified by `new_fact`.
    pub fn supersede(&self, old_hash: &str, new_fact: &str) -> Result<(), String> {
        let mut entries = self.load()?;
        if old_hash == new_fact {
            return Err("a fact cannot supersede itself".to_string());
        }
        if !entries.iter().any(|entry| entry.id == new_fact) {
            return Err(format!("fact '{new_fact}' does not exist"));
        }
        find_fact_mut(&mut entries, old_hash)?.superseded_by = Some(new_fact.to_string());
        self.save(&entries)
    }

    /// Remove facts whose latest verification (or creation) predates their expiry window.
    pub fn prune_stale(&self, max_age_days: u16) -> Result<usize, String> {
        let now = unix_timestamp();
        let mut entries = self.load()?;
        let original_len = entries.len();
        entries.retain(|entry| {
            let age_days = entry.stale_after_days.unwrap_or(max_age_days);
            let reference_time = entry.last_verified.unwrap_or(entry.timestamp);
            now.saturating_sub(reference_time) <= u64::from(age_days) * SECONDS_PER_DAY
        });
        let removed = original_len.saturating_sub(entries.len());
        self.save(&entries)?;
        Ok(removed)
    }

    /// Remove facts consistently associated with unsuccessful outcomes.
    pub fn prune_negative(&self) -> Result<usize, String> {
        let mut entries = self.load()?;
        let original_len = entries.len();
        entries.retain(|entry| entry.outcome_correlation >= NEGATIVE_CORRELATION_THRESHOLD);
        let removed = original_len.saturating_sub(entries.len());
        self.save(&entries)?;
        Ok(removed)
    }

    /// Return query-matching facts ordered by their provenance quality score.
    pub fn get_best(&self, query: &str, limit: usize) -> Result<Vec<SharedContextEntry>, String> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut entries = self.load()?;
        let query_terms = terms(query);
        if query_terms.is_empty() {
            return Ok(Vec::new());
        }

        let mut matches = entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                let entry_terms = terms(&entry.content);
                let relevance = query_terms
                    .iter()
                    .filter(|term| entry_terms.contains(term))
                    .count();
                (relevance > 0).then_some((index, relevance))
            })
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| {
            let left_quality = entries[left.0].confidence * entries[left.0].outcome_correlation;
            let right_quality = entries[right.0].confidence * entries[right.0].outcome_correlation;
            right_quality
                .total_cmp(&left_quality)
                .then_with(|| right.1.cmp(&left.1))
                .then_with(|| entries[left.0].id.cmp(&entries[right.0].id))
        });

        let now = unix_timestamp();
        let selected = matches
            .into_iter()
            .take(limit)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        for index in &selected {
            let entry = &mut entries[*index];
            entry.access_count = entry.access_count.saturating_add(1);
            entry.last_accessed = now;
        }
        let results = selected
            .into_iter()
            .map(|index| entries[index].clone())
            .collect();
        self.save(&entries)?;
        Ok(results)
    }

    /// Fold an outcome into a fact's correlation score with an exponential moving average.
    pub fn record_fact_outcome(&self, hash: &str, success: bool) -> Result<(), String> {
        let mut entries = self.load()?;
        let entry = find_fact_mut(&mut entries, hash)?;
        let outcome = if success { 1.0 } else { -1.0 };
        entry.outcome_correlation = (entry.outcome_correlation * (1.0 - OUTCOME_EMA_ALPHA)
            + outcome * OUTCOME_EMA_ALPHA)
            .clamp(-1.0, 1.0);
        self.save(&entries)
    }

    /// Summarize provenance quality and the agents or users that contributed facts.
    pub fn cross_session_stats(&self) -> Result<MemoryStats, String> {
        let entries = self.load()?;
        let total_facts = entries.len();
        let verified = entries
            .iter()
            .filter(|entry| entry.verification_count > 0)
            .count();
        let avg_confidence = (total_facts > 0).then(|| {
            entries.iter().map(|entry| entry.confidence).sum::<f32>() / total_facts as f32
        });
        let mut contributors = BTreeMap::new();
        for entry in &entries {
            let contributor = if entry.created_by.is_empty() {
                &entry.agent
            } else {
                &entry.created_by
            };
            *contributors.entry(contributor.clone()).or_insert(0) += 1;
        }
        let mut top_contributors = contributors.into_iter().collect::<Vec<_>>();
        top_contributors
            .sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));

        Ok(MemoryStats {
            total_facts,
            verified_pct: percentage(verified, total_facts),
            avg_confidence: avg_confidence.unwrap_or(0.0),
            top_contributors,
        })
    }

    pub fn stats(&self) -> Result<SharedContextStats, String> {
        let entries = self.load()?;
        let unique_agents = entries
            .iter()
            .map(|entry| entry.agent.as_str())
            .collect::<HashSet<_>>()
            .len();
        let mut categories = BTreeMap::new();
        for entry in &entries {
            *categories.entry(entry.category.clone()).or_insert(0) += 1;
        }
        Ok(SharedContextStats {
            total_entries: entries.len(),
            unique_agents,
            categories,
            total_accesses: entries
                .iter()
                .map(|entry| u64::from(entry.access_count))
                .sum(),
        })
    }

    fn load(&self) -> Result<Vec<SharedContextEntry>, String> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = fs::File::open(&self.path)
            .map_err(|error| format!("open {}: {error}", self.path.display()))?;
        let mut entries = BufReader::new(file)
            .lines()
            .filter(|line| line.as_ref().is_ok_and(|value| !value.trim().is_empty()))
            .map(|line| {
                let line =
                    line.map_err(|error| format!("read {}: {error}", self.path.display()))?;
                serde_json::from_str(&line)
                    .map_err(|error| format!("parse {}: {error}", self.path.display()))
            })
            .collect::<Result<Vec<SharedContextEntry>, String>>()?;
        for entry in &mut entries {
            apply_provenance_defaults(entry);
        }
        Ok(entries)
    }

    fn save(&self, entries: &[SharedContextEntry]) -> Result<(), String> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| format!("{} has no parent", self.path.display()))?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
        let temporary = self.path.with_extension("jsonl.tmp");
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&temporary)
            .map_err(|error| format!("open {}: {error}", temporary.display()))?;
        for entry in entries {
            serde_json::to_writer(&mut file, entry)
                .map_err(|error| format!("serialize entry: {error}"))?;
            file.write_all(b"\n")
                .map_err(|error| format!("write {}: {error}", temporary.display()))?;
        }
        file.flush()
            .map_err(|error| format!("flush {}: {error}", temporary.display()))?;
        fs::rename(&temporary, &self.path)
            .map_err(|error| format!("replace {}: {error}", self.path.display()))
    }
}

fn default_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local/share/lean-ctx/shared_context.jsonl")
}

const SECONDS_PER_DAY: u64 = 24 * 60 * 60;
const NEGATIVE_CORRELATION_THRESHOLD: f32 = -0.3;
const OUTCOME_EMA_ALPHA: f32 = 0.2;

fn default_confidence() -> f32 {
    0.8
}

fn find_fact_mut<'a>(
    entries: &'a mut [SharedContextEntry],
    hash: &str,
) -> Result<&'a mut SharedContextEntry, String> {
    entries
        .iter_mut()
        .find(|entry| entry.id == hash)
        .ok_or_else(|| format!("fact '{hash}' does not exist"))
}

fn apply_provenance_defaults(entry: &mut SharedContextEntry) {
    entry.confidence = entry.confidence.clamp(0.0, 1.0);
    entry.outcome_correlation = entry.outcome_correlation.clamp(-1.0, 1.0);
    if entry.created_by.is_empty() {
        entry.created_by.clone_from(&entry.agent);
    }
}

fn percentage(numerator: usize, denominator: usize) -> f32 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f32 / denominator as f32 * 100.0
    }
}

fn normalize_category(category: &str) -> Result<String, String> {
    match category {
        "fact" | "decision" | "blocker" | "pattern" => Ok(category.to_string()),
        other => Err(format!(
            "invalid category '{other}' (expected fact|decision|blocker|pattern)"
        )),
    }
}

fn terms(value: &str) -> Vec<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(str::to_lowercase)
        .collect()
}

pub fn session_start_hint() -> Option<String> {
    let entries = SharedContext::default().get_recent(3).ok()?;
    (!entries.is_empty()).then(|| {
        let facts = entries
            .iter()
            .map(|entry| format!("- [{} · {}] {}", entry.category, entry.agent, entry.content))
            .collect::<Vec<_>>()
            .join("\n");
        format!("--- SHARED CONTEXT ---\n{facts}")
    })
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(name: &str) -> SharedContext {
        let path = std::env::temp_dir().join(format!(
            "lean-ctx-shared-context-{name}-{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        SharedContext::new(path)
    }

    #[test]
    fn put_and_get_round_trip() {
        let context = store("round-trip");
        let id = context
            .put("Use cargo test --lib before commit", "codex", "fact")
            .unwrap();
        let entries = context.get_relevant("cargo commit", 10).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, id);
        assert_eq!(entries[0].agent, "codex");
        assert_eq!(entries[0].access_count, 1);
    }

    #[test]
    fn duplicate_content_updates_in_place() {
        let context = store("dedup");
        let first = context
            .put("The parser needs UTF-8 input", "claude", "fact")
            .unwrap();
        let second = context
            .put("The parser needs UTF-8 input", "codex", "decision")
            .unwrap();
        assert_eq!(first, second);
        let stats = context.stats().unwrap();
        assert_eq!(stats.total_entries, 1);
        assert_eq!(stats.unique_agents, 1);
        assert_eq!(context.get_recent(1).unwrap()[0].category, "decision");
    }

    #[test]
    fn bm25_prefers_more_relevant_entries() {
        let context = store("bm25");
        context
            .put("SQLite migration preserves agent memory", "claude", "fact")
            .unwrap();
        context
            .put(
                "Agent memory migration migration migration is complete",
                "cursor",
                "fact",
            )
            .unwrap();
        context
            .put("Rendering uses SVG sprites", "codex", "pattern")
            .unwrap();
        let entries = context.get_relevant("agent memory migration", 2).unwrap();
        assert_eq!(entries[0].agent, "cursor");
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn prune_removes_expired_and_excess_entries() {
        let context = store("prune");
        context.put("old", "claude", "fact").unwrap();
        context.put("new one", "codex", "fact").unwrap();
        context.put("new two", "cursor", "fact").unwrap();
        let mut entries = context.load().unwrap();
        entries[0].last_accessed = 0;
        context.save(&entries).unwrap();
        assert_eq!(context.prune(1, 1).unwrap(), 2);
        assert_eq!(context.stats().unwrap().total_entries, 1);
    }

    #[test]
    fn verify_fact_records_timestamp_and_count() {
        let context = store("verify");
        let id = context
            .put("Cargo checks are required", "codex", "fact")
            .unwrap();

        context.verify_fact(&id).unwrap();
        context.verify_fact(&id).unwrap();

        let entry = context.get_recent(1).unwrap().pop().unwrap();
        assert_eq!(entry.verification_count, 2);
        assert!(entry.last_verified.is_some());
    }

    #[test]
    fn prune_stale_removes_old_unverified_facts() {
        let context = store("stale");
        context.put("outdated command", "codex", "fact").unwrap();
        context.put("current command", "codex", "fact").unwrap();
        let mut entries = context.load().unwrap();
        entries[0].timestamp = unix_timestamp().saturating_sub(2 * SECONDS_PER_DAY);
        entries[1].last_verified = Some(unix_timestamp());
        context.save(&entries).unwrap();

        assert_eq!(context.prune_stale(1).unwrap(), 1);
        assert_eq!(
            context.get_recent(10).unwrap()[0].content,
            "current command"
        );
    }

    #[test]
    fn get_best_prioritizes_high_quality_facts() {
        let context = store("best");
        context
            .put("cargo test validates changes", "claude", "fact")
            .unwrap();
        context
            .put("cargo test captures regressions", "codex", "fact")
            .unwrap();
        let mut entries = context.load().unwrap();
        entries[0].confidence = 0.4;
        entries[0].outcome_correlation = 0.5;
        entries[1].confidence = 0.9;
        entries[1].outcome_correlation = 0.9;
        context.save(&entries).unwrap();

        let best = context.get_best("cargo test", 2).unwrap();
        assert_eq!(best[0].agent, "codex");
    }

    #[test]
    fn recording_outcomes_updates_correlation_with_ema() {
        let context = store("outcomes");
        let id = context.put("test outcome", "codex", "fact").unwrap();

        context.record_fact_outcome(&id, true).unwrap();
        let positive = context
            .get_recent(1)
            .unwrap()
            .pop()
            .unwrap()
            .outcome_correlation;
        context.record_fact_outcome(&id, false).unwrap();
        let mixed = context
            .get_recent(1)
            .unwrap()
            .pop()
            .unwrap()
            .outcome_correlation;

        assert!((positive - 0.2).abs() < f32::EPSILON);
        assert!(mixed < positive);
    }

    #[test]
    fn legacy_jsonl_entries_receive_provenance_defaults() {
        let context = store("legacy");
        fs::write(
            context.path(),
            r#"{"id":"legacy","content":"legacy fact","agent":"cursor","timestamp":7,"category":"fact","confidence":1.0,"access_count":0,"last_accessed":7}"#,
        )
        .unwrap();

        let entry = context.get_recent(1).unwrap().pop().unwrap();
        assert_eq!(entry.confidence, 1.0);
        assert_eq!(entry.created_by, "cursor");
        assert_eq!(entry.last_verified, None);
        assert_eq!(entry.verification_count, 0);
        assert_eq!(entry.outcome_correlation, 0.0);
        assert_eq!(entry.stale_after_days, None);
        assert_eq!(entry.superseded_by, None);
    }

    #[test]
    fn superseding_and_negative_pruning_track_quality() {
        let context = store("supersede");
        let old = context.put("old guidance", "codex", "fact").unwrap();
        let new = context.put("new guidance", "codex", "fact").unwrap();
        context.supersede(&old, &new).unwrap();
        context.record_fact_outcome(&old, false).unwrap();
        context.record_fact_outcome(&old, false).unwrap();

        let entries = context.get_recent(2).unwrap();
        let old_entry = entries
            .iter()
            .find(|e| e.content == "old guidance")
            .unwrap();
        assert_eq!(old_entry.superseded_by, Some(new));
        assert_eq!(context.prune_negative().unwrap(), 1);
    }
}
