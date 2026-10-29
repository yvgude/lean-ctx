//! Durable facts agents share through `ctx_memory`, partitioned by project.
//!
//! Each project has its own `shared_context/<project hash>.jsonl` in the
//! LeanCTX data directory, and every entry records the project it was stored
//! for; that stored root, not the file name, decides whether an entry is
//! served. Releases before 3.11.5 kept one installation-wide
//! `~/.local/share/lean-ctx/shared_context.jsonl` without a project. Those
//! entries carry no provable scope: the file is left in place, unread, and is
//! never served to any project.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
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
    /// The project this fact was stored for; the only authority for serving it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_root: Option<String>,
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
    /// Set for a project partition: only entries stored for this root load.
    project_root: Option<String>,
}

impl SharedContext {
    /// An unpartitioned store at `path` (tests and explicit tooling).
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            project_root: None,
        }
    }

    /// The installation-wide store of releases before 3.11.5. Served to no
    /// project; read for the explicit `lean-ctx migrate legacy-context`
    /// adoption only.
    pub fn legacy() -> Option<Self> {
        // Those releases wrote under `dirs::home_dir()`. Tests redirect the
        // home through `HOME`, which `dirs` ignores on Windows (known folder):
        // without this they would share the runner's real profile.
        #[cfg(not(test))]
        let home = dirs::home_dir();
        #[cfg(test)]
        let home = crate::core::home::resolve_home_dir();
        home.map(|home| Self::new(home.join(".local/share/lean-ctx/shared_context.jsonl")))
    }

    /// Facts left in a partition that is no longer the current one of the
    /// root they recorded: that project's identity (remote, package name)
    /// changed since. Served to no project until adopted.
    pub(crate) fn orphaned_entries() -> Vec<(PathBuf, SharedContextEntry)> {
        let Ok(dir) = crate::core::data_dir::lean_ctx_data_dir().map(|d| d.join("shared_context"))
        else {
            return Vec::new();
        };
        let Ok(files) = fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut paths: Vec<PathBuf> = files
            .flatten()
            .map(|file| file.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
            .collect();
        paths.sort();
        let mut orphaned = Vec::new();
        for path in paths {
            let stem = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default();
            for entry in Self::new(path.clone()).load().unwrap_or_default() {
                if entry
                    .project_root
                    .as_deref()
                    .is_some_and(|root| crate::core::project_hash::hash_project_root(root) != stem)
                {
                    orphaned.push((path.clone(), entry));
                }
            }
        }
        orphaned
    }

    /// The orphaned facts that recorded `project_root` (canonically).
    pub(crate) fn drifted_entries(project_root: &str) -> Vec<(PathBuf, SharedContextEntry)> {
        let canonical =
            |root: &str| crate::core::pathutil::safe_canonicalize_bounded(Path::new(root), 2000);
        let wanted = canonical(project_root);
        Self::orphaned_entries()
            .into_iter()
            .filter(|(_, entry)| {
                entry
                    .project_root
                    .as_deref()
                    .is_some_and(|root| canonical(root) == wanted)
            })
            .collect()
    }

    /// The shared facts of `project_root`, in its own partition of the data
    /// directory.
    pub fn for_project(project_root: &str) -> Result<Self, String> {
        if project_root.trim().is_empty() {
            return Err("shared memory needs a project".into());
        }
        let path = crate::core::data_dir::lean_ctx_data_dir()?
            .join("shared_context")
            .join(format!(
                "{}.jsonl",
                crate::core::project_hash::hash_project_root(project_root)
            ));
        Ok(Self {
            path,
            project_root: Some(project_root.to_string()),
        })
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
        let _writer = self.write_lock()?;
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
            project_root: self.project_root.clone(),
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
        let _writer = self.write_lock()?;
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
        let _writer = self.write_lock()?;
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
        let _writer = self.write_lock()?;
        let mut entries = self.load()?;
        let entry = find_fact_mut(&mut entries, hash)?;
        entry.last_verified = Some(unix_timestamp());
        entry.verification_count = entry.verification_count.saturating_add(1);
        self.save(&entries)
    }

    /// Lower the confidence of a fact identified as stale without removing it.
    pub fn mark_stale(&self, hash: &str) -> Result<(), String> {
        let _writer = self.write_lock()?;
        let mut entries = self.load()?;
        find_fact_mut(&mut entries, hash)?.confidence = 0.3;
        self.save(&entries)
    }

    /// Mark `old_hash` as replaced by the existing fact identified by `new_fact`.
    pub fn supersede(&self, old_hash: &str, new_fact: &str) -> Result<(), String> {
        let _writer = self.write_lock()?;
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
        let _writer = self.write_lock()?;
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
        let _writer = self.write_lock()?;
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
        let _writer = self.write_lock()?;
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
        let _writer = self.write_lock()?;
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

    /// Every read-modify-write of this store holds an exclusive lock on a
    /// sibling `.lock` file from its load to its save, so writers in other
    /// threads or processes (two agents of one project) cannot replace the
    /// file with a copy that lacks their entries. Released on drop.
    fn write_lock(&self) -> Result<fs::File, String> {
        let lock_path = self.path.with_extension("lock");
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("create {}: {error}", parent.display()))?;
        }
        // Windows `LockFileEx` needs a handle with GENERIC_READ or
        // GENERIC_WRITE; an append-only handle lacks FILE_WRITE_DATA, so every
        // lock attempt failed with "Access is denied" (os error 5).
        let file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|error| format!("open {}: {error}", lock_path.display()))?;
        crate::core::file_lock::acquire_exclusive_timeout(&file, WRITE_LOCK_WAIT)
            .map_err(|error| format!("shared memory is busy: {error}"))?;
        Ok(file)
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
        if let Some(root) = &self.project_root {
            // The file name is a hash; the stored root is the authority.
            entries.retain(|entry| {
                entry
                    .project_root
                    .as_deref()
                    .is_some_and(|stored| same_root(stored, root))
            });
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
        let mut content = String::new();
        for entry in entries {
            content.push_str(
                &serde_json::to_string(entry)
                    .map_err(|error| format!("serialize entry: {error}"))?,
            );
            content.push('\n');
        }
        crate::config_io::write_atomic(&self.path, &content)
            .map_err(|error| format!("replace {}: {error}", self.path.display()))
    }
}

fn same_root(stored: &str, requested: &str) -> bool {
    use crate::core::pathutil::normalize_tool_path;
    normalize_tool_path(stored).trim_end_matches('/')
        == normalize_tool_path(requested).trim_end_matches('/')
}

/// How long a writer waits for another writer of the same store.
const WRITE_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(5);
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

/// The newest shared facts of `project_root` as a context hint, re-admitted
/// under the originless rule; nothing when the project has none or the hint
/// may not be served now.
pub fn session_start_hint(project_root: &str) -> Option<String> {
    crate::core::legacy_adoption::auto_adopt_shared_context(project_root);
    let entries = SharedContext::for_project(project_root)
        .ok()?
        .get_recent(3)
        .ok()?;
    if entries.is_empty() {
        return None;
    }
    let facts = entries
        .iter()
        .map(|entry| format!("- [{} · {}] {}", entry.category, entry.agent, entry.content))
        .collect::<Vec<_>>()
        .join("\n");
    crate::tools::admitted_sources::serve_originless(
        &format!("--- SHARED CONTEXT ---\n{facts}"),
        "shared memory",
    )
    .ok()
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

    /// AP05-G2: one installation-wide file served every project's facts, and
    /// the session hint decorated every tool result with them.
    #[test]
    fn shared_facts_and_hint_stay_in_their_project() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let (a, b) = (a.path().to_str().unwrap(), b.path().to_str().unwrap());
        SharedContext::for_project(a)
            .unwrap()
            .put("project-a-marker lives in net/retry.rs", "codex", "fact")
            .unwrap();
        let foreign = SharedContext::for_project(b).unwrap();
        assert!(foreign.get_recent(10).unwrap().is_empty());
        assert!(
            foreign
                .get_relevant("project-a-marker", 10)
                .unwrap()
                .is_empty()
        );
        assert_eq!(session_start_hint(b), None);
        let own = session_start_hint(a).expect("own project hint");
        assert!(own.contains("project-a-marker"), "{own}");
    }

    /// Several projects through several stores at once, in one data
    /// directory (AP01 §1/§3; ABNAHME I02, C01): a scope that leaks through
    /// process-wide state (cwd, environment, a "current project" global) or a
    /// store without a writer lock shows up as a value in the wrong partition
    /// or as a lost acknowledged write.
    mod partitions {
        use std::collections::BTreeSet;
        use std::path::PathBuf;
        use std::sync::{Arc, Barrier};

        use crate::core::agents::AgentRegistry;
        use crate::core::knowledge::ProjectKnowledge;
        use crate::core::memory_policy::MemoryPolicy;
        use crate::core::session::{SessionState, TaskInfo};
        use crate::core::shared_context::SharedContext;

        /// The same fact keys in every project: a partition mix-up overwrites.
        const KEYS: [&str; 4] = ["db", "queue", "cache", "auth"];

        fn in_project<T>(root: &str, f: impl FnOnce() -> T) -> T {
            crate::core::policy::runtime::REQUEST_PROJECT
                .sync_scope(std::cell::RefCell::new(Some(PathBuf::from(root))), f)
        }

        fn project(dir: &tempfile::TempDir, name: &str) -> String {
            let root = dir.path().join(name);
            std::fs::create_dir_all(&root).unwrap();
            root.canonicalize().unwrap().to_string_lossy().into_owned()
        }

        /// Every write one writer makes, each acknowledged before the writer ends.
        fn write_everywhere(root: &str, key: &str, value: &str) {
            in_project(root, || {
                let policy = MemoryPolicy::default();
                let (knowledge, ()) = ProjectKnowledge::mutate_locked(root, |k| {
                    k.remember("decision", key, value, "partition-test", 0.9, &policy);
                })
                .expect("knowledge write acknowledged");
                let fact = knowledge
                    .facts
                    .iter()
                    .find(|f| f.key == key && f.value == value)
                    .cloned()
                    .expect("acknowledged fact is in the returned store");
                crate::core::memory_lifecycle::archive_facts(&[fact])
                    .expect("archive acknowledged");
                SharedContext::for_project(root)
                    .and_then(|store| store.put(format!("{key} {value}"), key, "decision"))
                    .expect("shared memory write acknowledged");
                let mut session = SessionState::new();
                session.project_root = Some(root.to_owned());
                session.task = Some(TaskInfo {
                    description: value.to_owned(),
                    intent: None,
                    progress_pct: None,
                });
                session.save().expect("session save acknowledged");
                // The registry lock is a polled try-lock (750 ms, not a fair
                // queue, GitLab #1295): with every writer released by one
                // barrier, a write can be refused under load. A refused write
                // was never acknowledged and is retried; the property under
                // test is that no acknowledged write is lost.
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                loop {
                    match AgentRegistry::mutate_locked(|registry| {
                        registry.share_knowledge(
                            root,
                            key,
                            "decision",
                            &[(key.into(), value.into())],
                        )
                    }) {
                        Ok((_, shared)) => {
                            shared.expect("shared knowledge acknowledged");
                            break;
                        }
                        Err(error)
                            if error.contains("timed out")
                                && std::time::Instant::now() < deadline => {}
                        Err(error) => panic!("registry lock: {error}"),
                    }
                }
            });
        }

        /// What each store of `root` holds now, as one set of values per store.
        fn partition_contents(root: &str) -> Vec<(&'static str, BTreeSet<String>)> {
            in_project(root, || {
                let knowledge: BTreeSet<String> = ProjectKnowledge::load(root)
                    .map(|k| {
                        k.facts
                            .iter()
                            .filter(|f| f.is_current())
                            .map(|f| format!("{} {}", f.key, f.value))
                            .collect()
                    })
                    .unwrap_or_default();
                let archive: BTreeSet<String> = crate::core::memory_lifecycle::list_archives(root)
                    .iter()
                    .flat_map(|path| {
                        crate::core::memory_lifecycle::restore_archive(
                            root,
                            &path.to_string_lossy(),
                        )
                        .expect("own archive restores")
                    })
                    .map(|f| format!("{} {}", f.key, f.value))
                    .collect();
                let shared: BTreeSet<String> = SharedContext::for_project(root)
                    .and_then(|store| store.get_recent(usize::MAX))
                    .expect("shared memory")
                    .into_iter()
                    .map(|entry| entry.content)
                    .collect();
                let sessions: BTreeSet<String> =
                    SessionState::load_recent_for_project_root(root, usize::MAX)
                        .into_iter()
                        .filter_map(|session| session.task.map(|task| task.description))
                        .map(|value| {
                            let key = KEYS
                                .iter()
                                .find(|key| value.ends_with(&format!("-{key}")))
                                .expect("task names its key");
                            format!("{key} {value}")
                        })
                        .collect();
                let registry: BTreeSet<String> = AgentRegistry::new()
                    .receive_shared_knowledge(root, "partition-reader")
                    .expect("shared knowledge")
                    .into_iter()
                    .map(|fact| fact.text.replacen('=', " ", 1))
                    .collect();
                vec![
                    ("knowledge", knowledge),
                    ("archive", archive),
                    ("shared memory", shared),
                    ("sessions", sessions),
                    ("shared knowledge", registry),
                ]
            })
        }

        /// I02/C01: writers of two projects start together (one barrier) and write
        /// the same fact keys into one data directory through knowledge, archive,
        /// shared memory, sessions and shared agent knowledge. Every acknowledged
        /// write is kept, each in its own project only.
        #[test]
        fn concurrent_writes_into_two_projects_keep_every_write_in_its_own_partition() {
            let _data = crate::core::data_dir::isolated_data_dir();
            let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
            let dir = tempfile::tempdir().unwrap();
            let projects = [("p1", project(&dir, "p1")), ("p2", project(&dir, "p2"))];
            let start = Arc::new(Barrier::new(projects.len() * KEYS.len()));
            let start = &start;
            let writers: Vec<_> = projects
                .iter()
                .flat_map(|(name, root)| {
                    KEYS.iter().map(move |key| {
                        let (start, root) = (Arc::clone(start), root.clone());
                        let value = format!("canary-{name}-{key}");
                        std::thread::spawn(move || {
                            start.wait();
                            write_everywhere(&root, key, &value);
                        })
                    })
                })
                .collect();
            for writer in writers {
                writer.join().expect("writer acknowledged every write");
            }

            for (name, root) in &projects {
                let expected: BTreeSet<String> = KEYS
                    .iter()
                    .map(|key| format!("{key} canary-{name}-{key}"))
                    .collect();
                for (store, held) in partition_contents(root) {
                    assert_eq!(held, expected, "{store} of {name}");
                }
            }
        }
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
        entries
            .iter_mut()
            .find(|entry| entry.content == "old")
            .unwrap()
            .last_accessed = 0;
        context.save(&entries).unwrap();
        // An hour, not a second: a slow runner must not age the fresh entries
        // out. "old" expires by age, and one fresh entry goes to the cap.
        assert_eq!(context.prune(3600, 1).unwrap(), 2);
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
