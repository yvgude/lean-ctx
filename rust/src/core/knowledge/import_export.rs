use chrono::Utc;
use serde::{Deserialize, Serialize};

use super::types::{KnowledgeArchetype, KnowledgeFact, ProjectKnowledge};
use crate::core::memory_boundary::FactPrivacy;
use crate::core::memory_policy::MemoryPolicy;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMerge {
    Replace,
    Append,
    SkipExisting,
}

impl ImportMerge {
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "replace" => Some(Self::Replace),
            "append" => Some(Self::Append),
            "skip-existing" | "skip_existing" | "skip" => Some(Self::SkipExisting),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ImportResult {
    pub added: u32,
    pub skipped: u32,
    pub replaced: u32,
}

/// Community-compatible simple fact format for import/export interop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimpleFactEntry {
    pub category: String,
    pub key: String,
    pub value: String,
    #[serde(default)]
    pub confidence: Option<f32>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub timestamp: Option<String>,
}

/// Parse import data: tries native `ProjectKnowledge` first, then simple `[{...}]` array.
pub fn parse_import_data(data: &str) -> Result<Vec<KnowledgeFact>, String> {
    if let Ok(pk) = serde_json::from_str::<ProjectKnowledge>(data) {
        return Ok(pk.facts);
    }

    if let Ok(entries) = serde_json::from_str::<Vec<SimpleFactEntry>>(data) {
        let now = Utc::now();
        let facts = entries
            .into_iter()
            .map(|e| KnowledgeFact {
                archetype: KnowledgeArchetype::infer_from_category(&e.category),
                sensitivity: crate::core::sensitivity::classify_content(&e.value),
                category: e.category,
                key: e.key,
                value: e.value,
                source_session: e.source.unwrap_or_else(|| "import".to_string()),
                confidence: e.confidence.unwrap_or(0.8),
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
                imported_from: None,
                fidelity: None,
                revision_count: 0,
            })
            .collect();
        return Ok(facts);
    }

    let mut facts = Vec::new();
    for line in data.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<SimpleFactEntry>(line) {
            let now = Utc::now();
            facts.push(KnowledgeFact {
                archetype: KnowledgeArchetype::infer_from_category(&entry.category),
                sensitivity: crate::core::sensitivity::classify_content(&entry.value),
                category: entry.category,
                key: entry.key,
                value: entry.value,
                source_session: entry.source.unwrap_or_else(|| "import".to_string()),
                confidence: entry.confidence.unwrap_or(0.8),
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
                imported_from: None,
                fidelity: None,
                revision_count: 0,
            });
        } else {
            return Err(format!(
                "Invalid JSONL line: {}",
                &line[..line.len().min(80)]
            ));
        }
    }

    if facts.is_empty() {
        return Err("No facts found. Expected: native JSON, simple JSON array, or JSONL.".into());
    }
    Ok(facts)
}

fn imported_fact(source: &KnowledgeFact, session_id: &str) -> KnowledgeFact {
    let now = Utc::now();
    KnowledgeFact {
        category: source.category.clone(),
        key: source.key.clone(),
        value: source.value.clone(),
        source_session: session_id.to_string(),
        confidence: source.confidence,
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
        privacy: source.privacy,
        sensitivity: source.sensitivity,
        imported_from: source.imported_from.clone(),
        archetype: source.archetype.clone(),
        fidelity: None,
        revision_count: 0,
    }
}

impl ProjectKnowledge {
    /// Import facts from an external source with a configurable merge strategy.
    /// Returns (added, skipped, replaced) counts.
    pub fn import_facts(
        &mut self,
        incoming: Vec<KnowledgeFact>,
        merge: ImportMerge,
        session_id: &str,
        policy: &MemoryPolicy,
    ) -> ImportResult {
        let mut added = 0u32;
        let mut skipped = 0u32;
        let mut replaced = 0u32;

        for fact in incoming {
            let existing = self
                .facts
                .iter()
                .position(|f| f.category == fact.category && f.key == fact.key && f.is_current());

            match (&merge, existing) {
                (ImportMerge::SkipExisting, Some(_)) => {
                    skipped += 1;
                }
                (ImportMerge::Replace, Some(idx)) => {
                    self.facts[idx].valid_until = Some(Utc::now());
                    self.facts.push(imported_fact(&fact, session_id));
                    replaced += 1;
                }
                (ImportMerge::Append, Some(_)) | (_, None) => {
                    self.facts.push(imported_fact(&fact, session_id));
                    added += 1;
                }
            }
        }

        if added > 0 || replaced > 0 {
            self.updated_at = Utc::now();
            // Mirror remember()'s hard cap: a bulk import must settle the store at
            // <= max_facts, not 2x. run_lifecycle archives the excess by importance
            // (nothing is lost). The previous `* 2` guard diverged from the
            // remember() path and let an import inflate a store to twice the cap —
            // observed live as a doctor CRIT (facts 232/200).
            if self.facts.len() > policy.knowledge.max_facts {
                let _ = self.run_memory_lifecycle(policy);
            }
        }

        ImportResult {
            added,
            skipped,
            replaced,
        }
    }

    /// Export current facts as a simple JSON array (community-compatible schema).
    #[must_use]
    pub fn export_simple(&self) -> Vec<SimpleFactEntry> {
        self.facts
            .iter()
            .filter(|f| f.is_current())
            .map(|f| SimpleFactEntry {
                category: f.category.clone(),
                key: f.key.clone(),
                value: f.value.clone(),
                confidence: Some(f.confidence),
                source: Some(f.source_session.clone()),
                timestamp: Some(f.created_at.to_rfc3339()),
            })
            .collect()
    }
}
