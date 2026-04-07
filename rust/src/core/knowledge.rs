use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const MAX_FACTS: usize = 200;
const MAX_PATTERNS: usize = 50;
const MAX_HISTORY: usize = 100;
const CONTRADICTION_THRESHOLD: f32 = 0.5;

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
    pub valid_from: Option<DateTime<Utc>>,
    #[serde(default)]
    pub valid_until: Option<DateTime<Utc>>,
    #[serde(default)]
    pub supersedes: Option<String>,
    #[serde(default)]
    pub confirmation_count: u32,
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
        } else if existing.confidence >= CONTRADICTION_THRESHOLD {
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
    ) -> Option<Contradiction> {
        let contradiction = self.check_contradiction(category, key, value);

        if let Some(existing) = self
            .facts
            .iter_mut()
            .find(|f| f.category == category && f.key == key && f.is_current())
        {
            if existing.value != value {
                if existing.confidence >= 0.9 && existing.confirmation_count >= 2 {
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
                        valid_from: Some(now),
                        valid_until: None,
                        supersedes: Some(superseded_id),
                        confirmation_count: 1,
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
                existing.last_confirmed = Utc::now();
                existing.source_session = session_id.to_string();
                existing.confidence = (existing.confidence + confidence) / 2.0;
                existing.confirmation_count += 1;
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
                valid_from: Some(now),
                valid_until: None,
                supersedes: None,
                confirmation_count: 1,
            });
        }

        if self.facts.len() > MAX_FACTS {
            self.facts
                .sort_by(|a, b| b.last_confirmed.cmp(&a.last_confirmed));
            self.facts.truncate(MAX_FACTS);
        }

        self.updated_at = Utc::now();
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
                    let relevance = (match_count as f32 / terms.len() as f32) * f.confidence;
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
        facts.sort_by(|a, b| a.created_at.cmp(&b.created_at));
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

        if self.patterns.len() > MAX_PATTERNS {
            self.patterns.truncate(MAX_PATTERNS);
        }
        self.updated_at = Utc::now();
    }

    pub fn consolidate(&mut self, summary: &str, session_ids: Vec<String>) {
        self.history.push(ConsolidatedInsight {
            summary: summary.to_string(),
            from_sessions: session_ids,
            timestamp: Utc::now(),
        });

        if self.history.len() > MAX_HISTORY {
            self.history.drain(0..self.history.len() - MAX_HISTORY);
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
            let mut categories: Vec<&str> =
                current_facts.iter().map(|f| f.category.as_str()).collect();
            categories.sort();
            categories.dedup();

            for cat in categories {
                out.push_str(&format!("  [{cat}]\n"));
                for f in current_facts.iter().filter(|f| f.category == cat) {
                    out.push_str(&format!(
                        "    {}: {} (confidence: {:.0}%)\n",
                        f.key,
                        f.value,
                        f.confidence * 100.0
                    ));
                }
            }
        }

        if !self.patterns.is_empty() {
            out.push_str("PROJECT PATTERNS:\n");
            for p in &self.patterns {
                out.push_str(&format!("  [{}] {}\n", p.pattern_type, p.description));
            }
        }

        out
    }

    pub fn format_aaak(&self) -> String {
        let current_facts: Vec<&KnowledgeFact> =
            self.facts.iter().filter(|f| f.is_current()).collect();

        if current_facts.is_empty() && self.patterns.is_empty() {
            return String::new();
        }

        let mut out = String::new();
        let mut categories: Vec<&str> = current_facts.iter().map(|f| f.category.as_str()).collect();
        categories.sort();
        categories.dedup();

        for cat in categories {
            let facts_in_cat: Vec<&&KnowledgeFact> =
                current_facts.iter().filter(|f| f.category == cat).collect();
            let items: Vec<String> = facts_in_cat
                .iter()
                .map(|f| {
                    let stars = confidence_stars(f.confidence);
                    format!("{}={}{}", f.key, f.value, stars)
                })
                .collect();
            out.push_str(&format!("{}:{}\n", cat.to_uppercase(), items.join("|")));
        }

        if !self.patterns.is_empty() {
            let pat_items: Vec<String> = self
                .patterns
                .iter()
                .map(|p| format!("{}.{}", p.pattern_type, p.description))
                .collect();
            out.push_str(&format!("PAT:{}\n", pat_items.join("|")));
        }

        out
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
        top_facts.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.confirmation_count.cmp(&a.confirmation_count))
        });
        top_facts.truncate(10);

        let items: Vec<String> = top_facts
            .iter()
            .map(|f| format!("{}/{}={}", f.category, f.key, f.value))
            .collect();

        format!("FACTS:{}", items.join("|"))
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

        let content = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&content).ok()
    }

    pub fn load_or_create(project_root: &str) -> Self {
        Self::load(project_root).unwrap_or_else(|| Self::new(project_root))
    }
}

impl KnowledgeFact {
    pub fn is_current(&self) -> bool {
        self.valid_until.is_none()
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
    let home = dirs::home_dir().ok_or("Cannot determine home directory")?;
    Ok(home.join(".lean-ctx").join("knowledge").join(project_hash))
}

fn hash_project_root(root: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    root.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remember_and_recall() {
        let mut k = ProjectKnowledge::new("/tmp/test-project");
        k.remember("architecture", "auth", "JWT RS256", "session-1", 0.9);
        k.remember("api", "rate-limit", "100/min", "session-1", 0.8);

        let results = k.recall("auth");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].value, "JWT RS256");

        let results = k.recall("api rate");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "rate-limit");
    }

    #[test]
    fn upsert_existing_fact() {
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.7);
        k.remember("arch", "db", "PostgreSQL 16 with pgvector", "s2", 0.95);

        let current: Vec<_> = k.facts.iter().filter(|f| f.is_current()).collect();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].value, "PostgreSQL 16 with pgvector");
    }

    #[test]
    fn contradiction_detection() {
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.95);
        k.facts[0].confirmation_count = 3;

        let contradiction = k.check_contradiction("arch", "db", "MySQL");
        assert!(contradiction.is_some());
        let c = contradiction.unwrap();
        assert_eq!(c.severity, ContradictionSeverity::High);
    }

    #[test]
    fn temporal_validity() {
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.95);
        k.facts[0].confirmation_count = 3;

        k.remember("arch", "db", "MySQL", "s2", 0.9);

        let current: Vec<_> = k.facts.iter().filter(|f| f.is_current()).collect();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].value, "MySQL");

        let all_db: Vec<_> = k.facts.iter().filter(|f| f.key == "db").collect();
        assert_eq!(all_db.len(), 2);
    }

    #[test]
    fn confirmation_count() {
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.9);
        assert_eq!(k.facts[0].confirmation_count, 1);

        k.remember("arch", "db", "PostgreSQL", "s2", 0.9);
        assert_eq!(k.facts[0].confirmation_count, 2);
    }

    #[test]
    fn remove_fact() {
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.9);
        assert!(k.remove_fact("arch", "db"));
        assert!(k.facts.is_empty());
        assert!(!k.remove_fact("arch", "db"));
    }

    #[test]
    fn list_rooms() {
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("architecture", "auth", "JWT", "s1", 0.9);
        k.remember("architecture", "db", "PG", "s1", 0.9);
        k.remember("deploy", "host", "AWS", "s1", 0.8);

        let rooms = k.list_rooms();
        assert_eq!(rooms.len(), 2);
    }

    #[test]
    fn aaak_format() {
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("architecture", "auth", "JWT RS256", "s1", 0.95);
        k.remember("architecture", "db", "PostgreSQL", "s1", 0.7);

        let aaak = k.format_aaak();
        assert!(aaak.contains("ARCHITECTURE:"));
        assert!(aaak.contains("auth=JWT RS256"));
    }

    #[test]
    fn consolidate_history() {
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.consolidate(
            "Migrated from REST to GraphQL",
            vec!["s1".into(), "s2".into()],
        );
        assert_eq!(k.history.len(), 1);
        assert_eq!(k.history[0].from_sessions.len(), 2);
    }

    #[test]
    fn format_summary_output() {
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("architecture", "auth", "JWT RS256", "s1", 0.9);
        k.add_pattern(
            "naming",
            "snake_case for functions",
            vec!["get_user()".into()],
            "s1",
        );
        let summary = k.format_summary();
        assert!(summary.contains("PROJECT KNOWLEDGE:"));
        assert!(summary.contains("auth: JWT RS256"));
        assert!(summary.contains("PROJECT PATTERNS:"));
    }

    #[test]
    fn temporal_recall_at_time() {
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.95);
        k.facts[0].confirmation_count = 3;

        let before_change = Utc::now();
        std::thread::sleep(std::time::Duration::from_millis(10));

        k.remember("arch", "db", "MySQL", "s2", 0.9);

        let results = k.recall_at_time("db", before_change);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].value, "PostgreSQL");

        let results_now = k.recall_at_time("db", Utc::now());
        assert_eq!(results_now.len(), 1);
        assert_eq!(results_now[0].value, "MySQL");
    }

    #[test]
    fn timeline_shows_history() {
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.95);
        k.facts[0].confirmation_count = 3;
        k.remember("arch", "db", "MySQL", "s2", 0.9);

        let timeline = k.timeline("arch");
        assert_eq!(timeline.len(), 2);
        assert!(!timeline[0].is_current());
        assert!(timeline[1].is_current());
    }

    #[test]
    fn wakeup_format() {
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "auth", "JWT", "s1", 0.95);
        k.remember("arch", "db", "PG", "s1", 0.8);

        let wakeup = k.format_wakeup();
        assert!(wakeup.contains("FACTS:"));
        assert!(wakeup.contains("arch/auth=JWT"));
        assert!(wakeup.contains("arch/db=PG"));
    }

    #[test]
    fn low_confidence_contradiction() {
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.4);

        let c = k.check_contradiction("arch", "db", "MySQL");
        assert!(c.is_some());
        assert_eq!(c.unwrap().severity, ContradictionSeverity::Low);
    }

    #[test]
    fn no_contradiction_for_same_value() {
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.95);

        let c = k.check_contradiction("arch", "db", "PostgreSQL");
        assert!(c.is_none());
    }

    #[test]
    fn no_contradiction_for_similar_values() {
        let mut k = ProjectKnowledge::new("/tmp/test");
        k.remember(
            "arch",
            "db",
            "PostgreSQL 16 production database server",
            "s1",
            0.95,
        );

        let c = k.check_contradiction(
            "arch",
            "db",
            "PostgreSQL 16 production database server config",
        );
        assert!(c.is_none());
    }
}
