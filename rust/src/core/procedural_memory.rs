//! Procedural Memory — recurring workflow detection and template storage.
//!
//! Detects repeated tool-call sequences in Episodic Memory and stores them
//! as reusable Procedures with activation/termination conditions.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use super::episodic_memory::{Episode, Outcome};

use crate::core::memory_policy::ProceduralPolicy;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProceduralStore {
    pub project_hash: String,
    pub procedures: Vec<Procedure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Procedure {
    pub id: String,
    pub name: String,
    pub description: String,
    pub steps: Vec<ProcedureStep>,
    pub activation_keywords: Vec<String>,
    pub confidence: f32,
    pub times_used: u32,
    pub times_succeeded: u32,
    pub last_used: DateTime<Utc>,
    pub project_specific: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ProcedureStep {
    pub tool: String,
    pub description: String,
    pub optional: bool,
}

impl Procedure {
    pub fn success_rate(&self) -> f32 {
        if self.times_used == 0 {
            return 0.0;
        }
        self.times_succeeded as f32 / self.times_used as f32
    }

    pub fn matches_context(&self, task: &str) -> bool {
        let task_lower = task.to_lowercase();
        self.activation_keywords
            .iter()
            .any(|kw| task_lower.contains(&kw.to_lowercase()))
    }
}

impl ProceduralStore {
    pub fn new(project_hash: &str) -> Self {
        Self {
            project_hash: project_hash.to_string(),
            procedures: Vec::new(),
        }
    }

    pub fn suggest(&self, task: &str) -> Vec<&Procedure> {
        let mut matches: Vec<(&Procedure, f32)> = self
            .procedures
            .iter()
            .filter(|p| p.matches_context(task) && p.confidence >= 0.3)
            .map(|p| {
                let score = p.confidence * 0.5 + p.success_rate() * 0.3 + usage_recency(p) * 0.2;
                (p, score)
            })
            .collect();

        matches.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        matches.into_iter().map(|(p, _)| p).collect()
    }

    pub fn record_usage(&mut self, procedure_id: &str, success: bool) {
        if let Some(proc) = self.procedures.iter_mut().find(|p| p.id == procedure_id) {
            proc.times_used += 1;
            if success {
                proc.times_succeeded += 1;
            }
            proc.last_used = Utc::now();
            proc.confidence =
                (proc.confidence * 0.8 + if success { 0.2 } else { -0.1 }).clamp(0.0, 1.0);
        }
    }

    pub fn add_procedure(&mut self, procedure: Procedure, policy: &ProceduralPolicy) {
        if let Some(existing) = self
            .procedures
            .iter_mut()
            .find(|p| p.name == procedure.name)
        {
            existing.confidence = existing.confidence.midpoint(procedure.confidence);
            existing.steps = procedure.steps;
            existing.activation_keywords = procedure.activation_keywords;
        } else {
            self.procedures.push(procedure);
        }

        if self.procedures.len() > policy.max_procedures {
            self.procedures.sort_by(|a, b| {
                b.confidence
                    .partial_cmp(&a.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            self.procedures.truncate(policy.max_procedures);
        }
    }

    pub fn detect_patterns(&mut self, episodes: &[Episode], policy: &ProceduralPolicy) {
        let sequences = extract_tool_sequences(episodes);
        let patterns = find_repeated_sequences(&sequences, policy);

        for (steps, count, keywords) in patterns {
            if count < policy.min_repetitions || steps.len() < policy.min_sequence_len {
                continue;
            }

            let name = generate_procedure_name(&steps);
            let already_exists = self.procedures.iter().any(|p| p.name == name);
            if already_exists {
                continue;
            }

            let success_count = episodes
                .iter()
                .filter(|ep| matches!(ep.outcome, Outcome::Success { .. }))
                .count();
            let confidence = success_count as f32 / episodes.len().max(1) as f32;

            self.add_procedure(
                Procedure {
                    id: format!("proc-{}", md5_short(&name)),
                    name,
                    description: format!("Detected workflow ({count} repetitions)"),
                    steps,
                    activation_keywords: keywords,
                    confidence,
                    times_used: count as u32,
                    times_succeeded: success_count as u32,
                    last_used: Utc::now(),
                    project_specific: true,
                    created_at: Utc::now(),
                },
                policy,
            );
        }
    }

    fn store_path(project_hash: &str) -> Option<PathBuf> {
        let dir = crate::core::data_dir::lean_ctx_data_dir()
            .ok()?
            .join("memory")
            .join("procedures");
        Some(dir.join(format!("{project_hash}.json")))
    }

    pub fn load(project_hash: &str) -> Option<Self> {
        let path = Self::store_path(project_hash)?;
        let data = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&data).ok()
    }

    pub fn load_or_create(project_hash: &str) -> Self {
        Self::load(project_hash).unwrap_or_else(|| Self::new(project_hash))
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::store_path(&self.project_hash)
            .ok_or_else(|| "Cannot determine data directory".to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{e}"))?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| format!("{e}"))?;
        std::fs::write(path, json).map_err(|e| format!("{e}"))
    }
}

fn extract_tool_sequences(episodes: &[Episode]) -> Vec<Vec<String>> {
    episodes
        .iter()
        .map(|ep| ep.actions.iter().map(|a| a.tool.clone()).collect())
        .collect()
}

fn find_repeated_sequences(
    sequences: &[Vec<String>],
    policy: &ProceduralPolicy,
) -> Vec<(Vec<ProcedureStep>, usize, Vec<String>)> {
    let mut ngram_counts: HashMap<Vec<String>, usize> = HashMap::new();

    for seq in sequences {
        if seq.len() < policy.min_sequence_len {
            continue;
        }
        let max_win = seq.len().min(policy.max_window_size);
        for window_size in policy.min_sequence_len..=max_win {
            for window in seq.windows(window_size) {
                let key: Vec<String> = window.to_vec();
                *ngram_counts.entry(key).or_insert(0) += 1;
            }
        }
    }

    let mut results: Vec<(Vec<ProcedureStep>, usize, Vec<String>)> = Vec::new();

    let mut sorted: Vec<_> = ngram_counts.into_iter().collect();
    sorted.sort_by(|a, b| {
        let score_a = a.1 * a.0.len();
        let score_b = b.1 * b.0.len();
        score_b.cmp(&score_a)
    });

    let mut seen_prefixes: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (tools, count) in sorted {
        if count < policy.min_repetitions {
            continue;
        }

        let prefix = tools.join("->");
        let is_substring = seen_prefixes.iter().any(|s| s.contains(&prefix));
        if is_substring {
            continue;
        }

        seen_prefixes.insert(prefix);

        let steps: Vec<ProcedureStep> = tools
            .iter()
            .map(|t| ProcedureStep {
                tool: t.clone(),
                description: String::new(),
                optional: false,
            })
            .collect();

        let keywords: Vec<String> = tools
            .iter()
            .filter(|t| !t.starts_with("ctx_"))
            .cloned()
            .collect();

        results.push((steps, count, keywords));
    }

    results
}

fn generate_procedure_name(steps: &[ProcedureStep]) -> String {
    let tools: Vec<&str> = steps.iter().map(|s| s.tool.as_str()).collect();
    let short: Vec<&str> = tools
        .iter()
        .map(|t| t.strip_prefix("ctx_").unwrap_or(t))
        .collect();
    format!("workflow-{}", short.join("-"))
}

fn md5_short(input: &str) -> String {
    use md5::{Digest, Md5};
    let result = Md5::digest(input.as_bytes());
    format!("{result:x}")[..8].to_string()
}

fn usage_recency(proc: &Procedure) -> f32 {
    let days_old = Utc::now().signed_duration_since(proc.last_used).num_days() as f32;
    (1.0 - days_old / 30.0).max(0.0)
}

pub fn format_suggestion(proc: &Procedure) -> String {
    let mut output = format!(
        "Suggested workflow: {} (confidence: {:.0}%, used {}x, success rate: {:.0}%)\n",
        proc.name,
        proc.confidence * 100.0,
        proc.times_used,
        proc.success_rate() * 100.0
    );
    for (i, step) in proc.steps.iter().enumerate() {
        let opt = if step.optional { " (optional)" } else { "" };
        output.push_str(&format!("  {}. {}{opt}\n", i + 1, step.tool));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::episodic_memory::{Action, Episode, Outcome};

    fn make_episode_with_tools(tools: &[&str]) -> Episode {
        Episode {
            id: "ep-1".to_string(),
            session_id: "s-1".to_string(),
            timestamp: Utc::now(),
            task_description: "test task".to_string(),
            actions: tools
                .iter()
                .map(|t| Action {
                    tool: t.to_string(),
                    description: String::new(),
                    timestamp: Utc::now(),
                    duration_ms: 100,
                    success: true,
                })
                .collect(),
            outcome: Outcome::Success { tests_passed: true },
            affected_files: vec![],
            summary: String::new(),
            duration_secs: 60,
            tokens_used: 1000,
        }
    }

    #[test]
    fn detect_patterns_from_episodes() {
        let policy = ProceduralPolicy::default();
        let episodes: Vec<Episode> = (0..5)
            .map(|_| make_episode_with_tools(&["ctx_read", "ctx_shell", "ctx_read"]))
            .collect();

        let mut store = ProceduralStore::new("test");
        store.detect_patterns(&episodes, &policy);

        assert!(
            !store.procedures.is_empty(),
            "Should detect at least one pattern"
        );
    }

    #[test]
    fn suggest_matching_procedure() {
        let policy = ProceduralPolicy::default();
        let mut store = ProceduralStore::new("test");
        store.add_procedure(
            Procedure {
                id: "proc-1".to_string(),
                name: "deploy-workflow".to_string(),
                description: "Deploy".to_string(),
                steps: vec![ProcedureStep {
                    tool: "ctx_shell".to_string(),
                    description: "cargo build".to_string(),
                    optional: false,
                }],
                activation_keywords: vec!["deploy".to_string(), "release".to_string()],
                confidence: 0.8,
                times_used: 5,
                times_succeeded: 4,
                last_used: Utc::now(),
                project_specific: true,
                created_at: Utc::now(),
            },
            &policy,
        );

        let suggestions = store.suggest("deploy the new version");
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].name, "deploy-workflow");

        let none = store.suggest("refactor the database layer");
        assert!(none.is_empty());
    }

    #[test]
    fn record_usage_updates_confidence() {
        let policy = ProceduralPolicy::default();
        let mut store = ProceduralStore::new("test");
        store.add_procedure(
            Procedure {
                id: "proc-1".to_string(),
                name: "test-workflow".to_string(),
                description: "Test".to_string(),
                steps: vec![],
                activation_keywords: vec![],
                confidence: 0.5,
                times_used: 0,
                times_succeeded: 0,
                last_used: Utc::now(),
                project_specific: false,
                created_at: Utc::now(),
            },
            &policy,
        );

        store.record_usage("proc-1", true);
        let proc = &store.procedures[0];
        assert_eq!(proc.times_used, 1);
        assert_eq!(proc.times_succeeded, 1);
        assert!(proc.confidence > 0.5);
    }

    #[test]
    fn success_rate_calculation() {
        let proc = Procedure {
            id: "p".to_string(),
            name: "n".to_string(),
            description: String::new(),
            steps: vec![],
            activation_keywords: vec![],
            confidence: 0.5,
            times_used: 10,
            times_succeeded: 7,
            last_used: Utc::now(),
            project_specific: false,
            created_at: Utc::now(),
        };
        assert!((proc.success_rate() - 0.7).abs() < 0.01);
    }

    #[test]
    fn max_procedures_enforced() {
        let policy = ProceduralPolicy::default();
        let mut store = ProceduralStore::new("test");
        for i in 0..110 {
            store.add_procedure(
                Procedure {
                    id: format!("p-{i}"),
                    name: format!("workflow-{i}"),
                    description: String::new(),
                    steps: vec![],
                    activation_keywords: vec![],
                    confidence: i as f32 / 110.0,
                    times_used: 0,
                    times_succeeded: 0,
                    last_used: Utc::now(),
                    project_specific: false,
                    created_at: Utc::now(),
                },
                &policy,
            );
        }
        assert!(store.procedures.len() <= policy.max_procedures);
    }

    #[test]
    fn format_suggestion_output() {
        let proc = Procedure {
            id: "p".to_string(),
            name: "deploy-workflow".to_string(),
            description: String::new(),
            steps: vec![
                ProcedureStep {
                    tool: "ctx_shell".to_string(),
                    description: "test".to_string(),
                    optional: false,
                },
                ProcedureStep {
                    tool: "ctx_shell".to_string(),
                    description: "build".to_string(),
                    optional: true,
                },
            ],
            activation_keywords: vec![],
            confidence: 0.85,
            times_used: 10,
            times_succeeded: 8,
            last_used: Utc::now(),
            project_specific: false,
            created_at: Utc::now(),
        };
        let output = format_suggestion(&proc);
        assert!(output.contains("deploy-workflow"));
        assert!(output.contains("85%"));
        assert!(output.contains("(optional)"));
    }
}
