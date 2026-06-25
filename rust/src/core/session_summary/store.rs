//! Bounded, per-project persistence for session summaries (#292).
//!
//! Mirrors the episodic-memory store pattern: one JSON file per project under
//! `{data_dir}/memory/summaries/{project_hash}.json`, newest summaries kept.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::record::SummaryRecord;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryStore {
    pub project_hash: String,
    /// Tool-call count at the last recorded summary (cadence watermark).
    #[serde(default)]
    pub last_recorded_calls: u64,
    #[serde(default)]
    pub summaries: Vec<SummaryRecord>,
}

impl SummaryStore {
    fn new(project_hash: &str) -> Self {
        Self {
            project_hash: project_hash.to_string(),
            last_recorded_calls: 0,
            summaries: Vec::new(),
        }
    }

    fn store_path(project_hash: &str) -> Option<PathBuf> {
        let dir = crate::core::data_dir::lean_ctx_data_dir()
            .ok()?
            .join("memory")
            .join("summaries");
        Some(dir.join(format!("{project_hash}.json")))
    }

    #[must_use]
    pub fn load_or_create(project_root: &str) -> Self {
        let hash = crate::core::project_hash::hash_project_root(project_root);
        let Some(path) = Self::store_path(&hash) else {
            return Self::new(&hash);
        };
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|c| serde_json::from_str::<SummaryStore>(&c).ok())
            .unwrap_or_else(|| Self::new(&hash))
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::store_path(&self.project_hash)
            .ok_or_else(|| "cannot resolve data dir".to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| e.to_string())
    }

    /// Next sequence number (1-based), monotonic across the store's lifetime.
    #[must_use]
    pub fn next_seq(&self) -> u32 {
        self.summaries
            .iter()
            .filter_map(|s| s.id.rsplit('-').next())
            .filter_map(|n| n.parse::<u32>().ok())
            .max()
            .unwrap_or(0)
            + 1
    }

    /// Append a record and prune to `max_kept` (newest kept).
    pub fn push(&mut self, record: SummaryRecord, max_kept: usize) {
        self.summaries.push(record);
        let cap = max_kept.max(1);
        if self.summaries.len() > cap {
            let excess = self.summaries.len() - cap;
            self.summaries.drain(0..excess);
        }
    }

    /// Lexical token-overlap search → `(index, score)`, best first.
    #[must_use]
    pub fn search_lexical(&self, query: &str, top_k: usize) -> Vec<(usize, f64)> {
        let terms = tokenize(query);
        if terms.is_empty() {
            return Vec::new();
        }
        let mut scored: Vec<(usize, f64)> = self
            .summaries
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                let hay = tokenize(&s.searchable_text());
                if hay.is_empty() {
                    return None;
                }
                let matches = terms.iter().filter(|t| hay.contains(*t)).count();
                if matches == 0 {
                    None
                } else {
                    Some((i, matches as f64 / terms.len() as f64))
                }
            })
            .collect();
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
        scored.truncate(top_k);
        scored
    }
}

fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 2)
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn rec(id: &str, title: &str, body: &str) -> SummaryRecord {
        SummaryRecord {
            id: id.to_string(),
            session_id: "sess".to_string(),
            created_at: Utc::now(),
            title: title.to_string(),
            body: body.to_string(),
            files: vec![],
            decisions: vec![],
            next_steps: vec![],
            tool_calls: 0,
        }
    }

    #[test]
    fn push_prunes_to_cap_keeping_newest() {
        let mut s = SummaryStore::new("h");
        for i in 0..5 {
            s.push(rec(&format!("s-{i}"), "t", "b"), 3);
        }
        assert_eq!(s.summaries.len(), 3);
        assert_eq!(s.summaries.first().unwrap().id, "s-2");
        assert_eq!(s.summaries.last().unwrap().id, "s-4");
    }

    #[test]
    fn next_seq_is_monotonic() {
        let mut s = SummaryStore::new("h");
        assert_eq!(s.next_seq(), 1);
        s.push(rec("abc-0007", "t", "b"), 100);
        assert_eq!(s.next_seq(), 8);
    }

    #[test]
    fn lexical_search_ranks_by_overlap() {
        let mut s = SummaryStore::new("h");
        s.push(
            rec("a-1", "graph traversal edges", "co-access learning"),
            100,
        );
        s.push(rec("a-2", "billing webhook", "stripe meter events"), 100);
        let hits = s.search_lexical("graph edges", 5);
        assert_eq!(hits.first().unwrap().0, 0, "graph summary ranks first");
        assert!(s.search_lexical("nonexistentterm", 5).is_empty());
    }
}
