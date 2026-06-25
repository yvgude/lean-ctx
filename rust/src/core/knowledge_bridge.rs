//! Cross-Agent Knowledge Bridge — controlled sharing of high-confidence facts between agents.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::core::knowledge::{KnowledgeArchetype, KnowledgeFact};
use crate::core::memory_boundary::FactPrivacy;

const PUBLISHABLE_ARCHETYPES: &[KnowledgeArchetype] = &[
    KnowledgeArchetype::Architecture,
    KnowledgeArchetype::Convention,
    KnowledgeArchetype::Decision,
    KnowledgeArchetype::Dependency,
    KnowledgeArchetype::Gotcha,
];

const MIN_PUBLISH_CONFIDENCE: f32 = 0.8;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeEntry {
    pub fact_key: String,
    pub fact_category: String,
    pub fact_value: String,
    pub source_agent: String,
    pub published_at: DateTime<Utc>,
    pub archetype: KnowledgeArchetype,
    pub confidence: f32,
    pub provenance: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeBridge {
    pub project_hash: String,
    pub shared_facts: Vec<BridgeEntry>,
    pub updated_at: DateTime<Utc>,
}

impl KnowledgeBridge {
    #[must_use]
    pub fn new(project_hash: &str) -> Self {
        Self {
            project_hash: project_hash.to_string(),
            shared_facts: Vec::new(),
            updated_at: Utc::now(),
        }
    }

    pub fn path(project_hash: &str) -> Result<PathBuf, String> {
        Ok(crate::core::data_dir::lean_ctx_data_dir()?
            .join("knowledge")
            .join(project_hash)
            .join("bridge.json"))
    }

    #[must_use]
    pub fn load(project_hash: &str) -> Option<Self> {
        let path = Self::path(project_hash).ok()?;
        let content = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str::<Self>(&content).ok()
    }

    #[must_use]
    pub fn load_or_create(project_hash: &str) -> Self {
        Self::load(project_hash).unwrap_or_else(|| Self::new(project_hash))
    }

    pub fn save(&mut self) -> Result<(), String> {
        self.updated_at = Utc::now();
        let path = Self::path(&self.project_hash)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        crate::config_io::write_atomic(&path, &json)
    }

    /// Publish eligible facts from an agent's knowledge store.
    /// Only publishes facts with sufficient confidence, a publishable archetype,
    /// and that haven't already been published by this agent.
    pub fn publish(&mut self, agent_id: &str, facts: &[KnowledgeFact]) -> u32 {
        let mut count = 0u32;
        for fact in facts {
            if !fact.is_current() {
                continue;
            }
            if fact.confidence < MIN_PUBLISH_CONFIDENCE {
                continue;
            }
            if !PUBLISHABLE_ARCHETYPES.contains(&fact.archetype) {
                continue;
            }
            let already_published = self.shared_facts.iter().any(|e| {
                e.fact_key == fact.key
                    && e.fact_category == fact.category
                    && e.source_agent == agent_id
            });
            if already_published {
                continue;
            }
            self.shared_facts.push(BridgeEntry {
                fact_key: fact.key.clone(),
                fact_category: fact.category.clone(),
                fact_value: fact.value.clone(),
                source_agent: agent_id.to_string(),
                published_at: Utc::now(),
                archetype: fact.archetype.clone(),
                confidence: fact.confidence,
                provenance: fact.source_session.clone(),
            });
            count += 1;
        }
        count
    }

    /// Pull facts from the bridge that were published by other agents.
    #[must_use]
    pub fn pull(&self, requesting_agent: &str) -> Vec<BridgeEntry> {
        self.shared_facts
            .iter()
            .filter(|e| e.source_agent != requesting_agent)
            .cloned()
            .collect()
    }

    /// Convert a [`BridgeEntry`] into a [`KnowledgeFact`] for import.
    /// Applies a 10% trust penalty to imported confidence.
    #[must_use]
    pub fn entry_to_fact(entry: &BridgeEntry) -> KnowledgeFact {
        let now = Utc::now();
        KnowledgeFact {
            category: entry.fact_category.clone(),
            key: entry.fact_key.clone(),
            value: entry.fact_value.clone(),
            source_session: entry.provenance.clone(),
            confidence: entry.confidence * 0.9,
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
            sensitivity: crate::core::sensitivity::classify_content(&entry.fact_value),
            imported_from: Some(format!("bridge:{}", entry.source_agent)),
            archetype: entry.archetype.clone(),
            fidelity: None,
            revision_count: 0,
        }
    }

    /// Remove entries older than `max_age_days` or below `min_confidence`.
    pub fn cleanup(&mut self, max_age_days: i64, min_confidence: f32) -> usize {
        let cutoff = Utc::now() - chrono::Duration::days(max_age_days);
        let before = self.shared_facts.len();
        self.shared_facts
            .retain(|e| e.published_at >= cutoff && e.confidence >= min_confidence);
        before - self.shared_facts.len()
    }

    #[must_use]
    pub fn entries_for_agent(&self, agent_id: &str) -> Vec<&BridgeEntry> {
        self.shared_facts
            .iter()
            .filter(|e| e.source_agent == agent_id)
            .collect()
    }

    #[must_use]
    pub fn summary(&self) -> String {
        if self.shared_facts.is_empty() {
            return format!(
                "Knowledge Bridge [{}]: empty",
                short_hash(&self.project_hash)
            );
        }

        let mut agents: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
        for entry in &self.shared_facts {
            *agents.entry(&entry.source_agent).or_default() += 1;
        }

        let mut out = format!(
            "Knowledge Bridge [{}]: {} shared facts from {} agent(s)\n",
            short_hash(&self.project_hash),
            self.shared_facts.len(),
            agents.len(),
        );
        let mut sorted_agents: Vec<_> = agents.into_iter().collect();
        sorted_agents.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        for (agent, count) in &sorted_agents {
            out.push_str(&format!("  {agent}: {count} fact(s)\n"));
        }
        out.push_str(&format!(
            "Last updated: {}",
            self.updated_at.format("%Y-%m-%d %H:%M UTC")
        ));
        out
    }
}

fn short_hash(hash: &str) -> &str {
    if hash.len() > 8 { &hash[..8] } else { hash }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::knowledge::KnowledgeFact;
    use crate::core::memory_boundary::FactPrivacy;

    fn make_fact(
        cat: &str,
        key: &str,
        val: &str,
        confidence: f32,
        archetype: KnowledgeArchetype,
    ) -> KnowledgeFact {
        KnowledgeFact {
            category: cat.into(),
            key: key.into(),
            value: val.into(),
            source_session: "test-session".into(),
            confidence,
            created_at: Utc::now(),
            last_confirmed: Utc::now(),
            retrieval_count: 0,
            last_retrieved: None,
            valid_from: None,
            valid_until: None,
            supersedes: None,
            confirmation_count: 1,
            feedback_up: 0,
            feedback_down: 0,
            last_feedback: None,
            privacy: FactPrivacy::default(),
            sensitivity: crate::core::sensitivity::SensitivityLevel::default(),
            imported_from: None,
            archetype,
            fidelity: None,
            revision_count: 0,
        }
    }

    #[test]
    fn publish_only_eligible_facts() {
        let mut bridge = KnowledgeBridge::new("test-hash");
        let facts = vec![
            make_fact(
                "arch",
                "db",
                "PostgreSQL",
                0.9,
                KnowledgeArchetype::Architecture,
            ),
            make_fact("random", "x", "low-conf", 0.3, KnowledgeArchetype::Fact),
            make_fact(
                "gotcha",
                "trap",
                "watch out",
                0.85,
                KnowledgeArchetype::Gotcha,
            ),
            make_fact(
                "pref",
                "editor",
                "vim",
                0.95,
                KnowledgeArchetype::Preference,
            ),
        ];
        let count = bridge.publish("agent-1", &facts);
        assert_eq!(count, 2);
        assert_eq!(bridge.shared_facts.len(), 2);
    }

    #[test]
    fn pull_excludes_own_facts() {
        let mut bridge = KnowledgeBridge::new("test-hash");
        let facts = vec![make_fact(
            "arch",
            "db",
            "PostgreSQL",
            0.9,
            KnowledgeArchetype::Architecture,
        )];
        bridge.publish("agent-1", &facts);

        let pulled = bridge.pull("agent-1");
        assert!(pulled.is_empty(), "Should not pull own facts");

        let pulled = bridge.pull("agent-2");
        assert_eq!(pulled.len(), 1);
    }

    #[test]
    fn entry_to_fact_preserves_provenance() {
        let entry = BridgeEntry {
            fact_key: "db".into(),
            fact_category: "arch".into(),
            fact_value: "PostgreSQL".into(),
            source_agent: "agent-1".into(),
            published_at: Utc::now(),
            archetype: KnowledgeArchetype::Architecture,
            confidence: 0.9,
            provenance: "session-abc".into(),
        };
        let fact = KnowledgeBridge::entry_to_fact(&entry);
        assert_eq!(fact.imported_from, Some("bridge:agent-1".into()));
        assert!(fact.confidence < 0.9);
        assert_eq!(fact.archetype, KnowledgeArchetype::Architecture);
    }

    #[test]
    fn no_duplicate_publish() {
        let mut bridge = KnowledgeBridge::new("test-hash");
        let facts = vec![make_fact(
            "arch",
            "db",
            "PostgreSQL",
            0.9,
            KnowledgeArchetype::Architecture,
        )];
        bridge.publish("agent-1", &facts);
        let second = bridge.publish("agent-1", &facts);
        assert_eq!(second, 0, "Should not re-publish same fact");
        assert_eq!(bridge.shared_facts.len(), 1);
    }

    #[test]
    fn cleanup_removes_old_entries() {
        let mut bridge = KnowledgeBridge::new("test-hash");
        bridge.shared_facts.push(BridgeEntry {
            fact_key: "old".into(),
            fact_category: "arch".into(),
            fact_value: "ancient".into(),
            source_agent: "agent-1".into(),
            published_at: Utc::now() - chrono::Duration::days(60),
            archetype: KnowledgeArchetype::Architecture,
            confidence: 0.9,
            provenance: "old-session".into(),
        });
        bridge.shared_facts.push(BridgeEntry {
            fact_key: "fresh".into(),
            fact_category: "arch".into(),
            fact_value: "new".into(),
            source_agent: "agent-1".into(),
            published_at: Utc::now(),
            archetype: KnowledgeArchetype::Architecture,
            confidence: 0.9,
            provenance: "new-session".into(),
        });
        let removed = bridge.cleanup(30, 0.5);
        assert_eq!(removed, 1);
        assert_eq!(bridge.shared_facts.len(), 1);
        assert_eq!(bridge.shared_facts[0].fact_key, "fresh");
    }

    #[test]
    fn entries_for_agent_filters_correctly() {
        let mut bridge = KnowledgeBridge::new("test-hash");
        let facts_a = vec![make_fact(
            "arch",
            "db",
            "PostgreSQL",
            0.9,
            KnowledgeArchetype::Architecture,
        )];
        let facts_b = vec![make_fact(
            "gotcha",
            "trap",
            "watch out",
            0.85,
            KnowledgeArchetype::Gotcha,
        )];
        bridge.publish("agent-a", &facts_a);
        bridge.publish("agent-b", &facts_b);

        assert_eq!(bridge.entries_for_agent("agent-a").len(), 1);
        assert_eq!(bridge.entries_for_agent("agent-b").len(), 1);
        assert_eq!(bridge.entries_for_agent("agent-c").len(), 0);
    }

    #[test]
    fn summary_format() {
        let mut bridge = KnowledgeBridge::new("test-hash");
        assert!(bridge.summary().contains("empty"));

        let facts = vec![make_fact(
            "arch",
            "db",
            "PostgreSQL",
            0.9,
            KnowledgeArchetype::Architecture,
        )];
        bridge.publish("agent-1", &facts);
        let summary = bridge.summary();
        assert!(summary.contains("1 shared facts"));
        assert!(summary.contains("agent-1"));
    }

    #[test]
    fn cleanup_removes_low_confidence() {
        let mut bridge = KnowledgeBridge::new("test-hash");
        bridge.shared_facts.push(BridgeEntry {
            fact_key: "weak".into(),
            fact_category: "arch".into(),
            fact_value: "uncertain".into(),
            source_agent: "agent-1".into(),
            published_at: Utc::now(),
            archetype: KnowledgeArchetype::Architecture,
            confidence: 0.3,
            provenance: "session".into(),
        });
        bridge.shared_facts.push(BridgeEntry {
            fact_key: "strong".into(),
            fact_category: "arch".into(),
            fact_value: "certain".into(),
            source_agent: "agent-1".into(),
            published_at: Utc::now(),
            archetype: KnowledgeArchetype::Architecture,
            confidence: 0.9,
            provenance: "session".into(),
        });
        let removed = bridge.cleanup(365, 0.5);
        assert_eq!(removed, 1);
        assert_eq!(bridge.shared_facts[0].fact_key, "strong");
    }

    #[test]
    fn trust_penalty_reduces_confidence() {
        let entry = BridgeEntry {
            fact_key: "k".into(),
            fact_category: "c".into(),
            fact_value: "v".into(),
            source_agent: "src".into(),
            published_at: Utc::now(),
            archetype: KnowledgeArchetype::Decision,
            confidence: 1.0,
            provenance: "s".into(),
        };
        let fact = KnowledgeBridge::entry_to_fact(&entry);
        assert!((fact.confidence - 0.9).abs() < f32::EPSILON);
    }

    #[test]
    fn archived_facts_not_published() {
        let mut bridge = KnowledgeBridge::new("test-hash");
        let mut fact = make_fact(
            "arch",
            "old-db",
            "MySQL",
            0.95,
            KnowledgeArchetype::Architecture,
        );
        fact.valid_until = Some(Utc::now() - chrono::Duration::days(1));
        let count = bridge.publish("agent-1", &[fact]);
        assert_eq!(count, 0);
    }
}
