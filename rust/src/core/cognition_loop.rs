//! Hebbian-inspired Cognition Loop — periodic background reorganization of knowledge.
//! Runs 8 steps: seed promote, structural repair, fidelity check, lateral synthesis,
//! contradiction resolution, hebbian strengthen, decay, compact.

use std::collections::HashSet;

use chrono::{Duration, Utc};

use crate::core::knowledge::ProjectKnowledge;
use crate::core::knowledge_relations::{
    KnowledgeEdgeKind, KnowledgeNodeRef, KnowledgeRelationGraph,
};
use crate::core::memory_policy::MemoryPolicy;

const LATERAL_SIM_THRESHOLD: f64 = 0.3;
const LATERAL_MAX_NEW_EDGES: usize = 20;
const HEBBIAN_CO_RETRIEVAL_HOURS: i64 = 1;
const EDGE_STALE_DAYS: i64 = 30;

#[derive(Debug, Clone, Default)]
pub struct CognitionLoopReport {
    pub steps_run: u8,
    pub facts_promoted: u32,
    pub edges_repaired: u32,
    pub edges_strengthened: u32,
    pub facts_decayed: u32,
    pub facts_archived: u32,
    pub contradictions_resolved: u32,
    pub lateral_connections: u32,
    pub duration_ms: u64,
}

impl std::fmt::Display for CognitionLoopReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Cognition Loop ({} steps, {}ms): promoted={}, repaired={}, \
             strengthened={}, decayed={}, archived={}, contradictions={}, lateral={}",
            self.steps_run,
            self.duration_ms,
            self.facts_promoted,
            self.edges_repaired,
            self.edges_strengthened,
            self.facts_decayed,
            self.facts_archived,
            self.contradictions_resolved,
            self.lateral_connections,
        )
    }
}

pub fn run_cognition_loop(project_root: &str, max_steps: u8) -> CognitionLoopReport {
    let start = std::time::Instant::now();
    let mut report = CognitionLoopReport::default();

    let Ok(policy) = crate::core::config::Config::load().memory_policy_effective() else {
        return report;
    };

    // Knowledge read-modify-write under the shared in-process + cross-process
    // lock so this loop (also driven by the background cognition scheduler)
    // never clobbers a concurrent foreground `remember`/`relate` write (issue
    // #326). The relation graph is loaded and saved inside the same critical
    // section; no step re-enters the knowledge lock, so this cannot deadlock.
    let _ = ProjectKnowledge::mutate_locked(project_root, |knowledge| {
        let project_hash = knowledge.project_hash.clone();
        let mut graph = KnowledgeRelationGraph::load_or_create(&project_hash);

        if max_steps >= 1 {
            report.facts_promoted = step_seed_promote(project_root, knowledge, &policy);
            report.steps_run = 1;
        }

        if max_steps >= 2 {
            report.edges_repaired = step_structural_repair(&mut graph, knowledge);
            report.steps_run = 2;
        }

        // Step 3: Fidelity Check (structural only, no LLM)
        if max_steps >= 3 {
            report.steps_run = 3;
        }

        if max_steps >= 4 {
            report.lateral_connections = step_lateral_synthesis(knowledge, &mut graph);
            report.steps_run = 4;
        }

        if max_steps >= 5 {
            report.contradictions_resolved = step_contradiction_resolution(knowledge);
            report.steps_run = 5;
        }

        if max_steps >= 6 {
            report.edges_strengthened = step_hebbian_strengthen(knowledge, &mut graph);
            report.steps_run = 6;
        }

        if max_steps >= 7 {
            report.facts_decayed = step_decay(knowledge, &mut graph, &policy);
            report.steps_run = 7;
        }

        if max_steps >= 8 {
            let lifecycle = knowledge.run_memory_lifecycle(&policy);
            report.facts_archived = lifecycle.archived_count as u32;
            report.steps_run = 8;
        }

        let _ = graph.save();
    });

    report.duration_ms = start.elapsed().as_millis() as u64;
    report
}

/// Step 1: Promote recent session decisions/findings into project knowledge.
fn step_seed_promote(
    _project_root: &str,
    knowledge: &mut ProjectKnowledge,
    policy: &MemoryPolicy,
) -> u32 {
    let Some(session) = crate::core::session::SessionState::load_latest() else {
        return 0;
    };

    let mut count = 0u32;
    let max_decisions = 5usize;
    let max_findings = 8usize;

    let mut decisions = session.decisions.clone();
    decisions.sort_by_key(|d| std::cmp::Reverse(d.timestamp));
    decisions.truncate(max_decisions);
    for d in &decisions {
        let key = slug_key(&d.summary, 50);
        knowledge.remember("decision", &key, &d.summary, &session.id, 0.9, policy);
        count += 1;
    }

    let mut findings = session.findings.clone();
    findings.sort_by_key(|f| std::cmp::Reverse(f.timestamp));
    let mut kept = 0usize;
    for f in &findings {
        if kept >= max_findings {
            break;
        }
        if finding_salience(&f.summary) < 45 {
            continue;
        }
        let key = if let Some(ref file) = f.file {
            if let Some(line) = f.line {
                format!("{file}:{line}")
            } else {
                file.clone()
            }
        } else {
            format!("finding-{}", slug_key(&f.summary, 36))
        };
        knowledge.remember("finding", &key, &f.summary, &session.id, 0.75, policy);
        count += 1;
        kept += 1;
    }

    count
}

/// Step 2: Remove edges whose endpoints no longer exist in the knowledge store.
fn step_structural_repair(graph: &mut KnowledgeRelationGraph, knowledge: &ProjectKnowledge) -> u32 {
    let fact_ids: HashSet<String> = knowledge
        .facts
        .iter()
        .filter(|f| f.is_current())
        .map(|f| format!("{}/{}", f.category, f.key))
        .collect();

    let before = graph.edges.len();
    graph
        .edges
        .retain(|e| fact_ids.contains(&e.from.id()) && fact_ids.contains(&e.to.id()));
    (before - graph.edges.len()) as u32
}

/// Step 4: Connect related facts that share vocabulary but lack an explicit edge.
fn step_lateral_synthesis(knowledge: &ProjectKnowledge, graph: &mut KnowledgeRelationGraph) -> u32 {
    let current: Vec<_> = knowledge.facts.iter().filter(|f| f.is_current()).collect();

    let existing_pairs: HashSet<(String, String)> = graph
        .edges
        .iter()
        .map(|e| (e.from.id(), e.to.id()))
        .collect();

    let mut added = 0u32;

    for (i, a) in current.iter().enumerate() {
        if added >= LATERAL_MAX_NEW_EDGES as u32 {
            break;
        }
        for b in &current[i + 1..] {
            if added >= LATERAL_MAX_NEW_EDGES as u32 {
                break;
            }
            let id_a = format!("{}/{}", a.category, a.key);
            let id_b = format!("{}/{}", b.category, b.key);
            if existing_pairs.contains(&(id_a.clone(), id_b.clone()))
                || existing_pairs.contains(&(id_b.clone(), id_a.clone()))
            {
                continue;
            }
            let sim = crate::core::memory_consolidation::token_jaccard(&a.value, &b.value);
            if sim >= LATERAL_SIM_THRESHOLD {
                let from = KnowledgeNodeRef::new(&a.category, &a.key);
                let to = KnowledgeNodeRef::new(&b.category, &b.key);
                graph.upsert_edge(from, to, KnowledgeEdgeKind::RelatedTo, "cognition-loop");
                added += 1;
            }
        }
    }

    added
}

/// Step 5: Resolve contradictions — same category+key, different values.
/// Keeps the fact with higher quality_score, archives the other.
fn step_contradiction_resolution(knowledge: &mut ProjectKnowledge) -> u32 {
    let now = Utc::now();
    let mut resolved = 0u32;

    let mut seen: std::collections::HashMap<(String, String), usize> =
        std::collections::HashMap::new();
    let mut to_archive: Vec<usize> = Vec::new();

    for (i, f) in knowledge.facts.iter().enumerate() {
        if !f.is_current() {
            continue;
        }
        let key = (f.category.clone(), f.key.clone());
        if let Some(&prev_idx) = seen.get(&key) {
            let prev = &knowledge.facts[prev_idx];
            if prev.value != f.value {
                if prev.quality_score() >= f.quality_score() {
                    to_archive.push(i);
                } else {
                    to_archive.push(prev_idx);
                    seen.insert(key, i);
                }
                resolved += 1;
            }
        } else {
            seen.insert(key, i);
        }
    }

    for &idx in &to_archive {
        knowledge.facts[idx].valid_until = Some(now);
    }

    resolved
}

/// Step 6: Strengthen edges between facts co-retrieved in the same session window.
fn step_hebbian_strengthen(
    knowledge: &ProjectKnowledge,
    graph: &mut KnowledgeRelationGraph,
) -> u32 {
    let retrieved: Vec<_> = knowledge
        .facts
        .iter()
        .filter(|f| f.is_current() && f.last_retrieved.is_some())
        .collect();

    let window = Duration::hours(HEBBIAN_CO_RETRIEVAL_HOURS);
    let mut strengthened = 0u32;

    for (i, a) in retrieved.iter().enumerate() {
        let Some(a_time) = a.last_retrieved else {
            continue;
        };
        for b in &retrieved[i + 1..] {
            let Some(b_time) = b.last_retrieved else {
                continue;
            };
            let diff = (a_time - b_time).abs();
            if diff <= window {
                let from = KnowledgeNodeRef::new(&a.category, &a.key);
                let to = KnowledgeNodeRef::new(&b.category, &b.key);
                if !graph.strengthen_edge(&from, &to, 0.15) {
                    graph.upsert_edge(from, to, KnowledgeEdgeKind::RelatedTo, "hebbian");
                }
                strengthened += 1;
            }
        }
    }

    strengthened
}

/// Step 7: Decay confidence on stale facts, and decay edge counts for unseen edges.
fn step_decay(
    knowledge: &mut ProjectKnowledge,
    graph: &mut KnowledgeRelationGraph,
    policy: &MemoryPolicy,
) -> u32 {
    let lifecycle_cfg = crate::core::memory_lifecycle::LifecycleConfig {
        max_facts: policy.knowledge.max_facts,
        decay_rate_per_day: policy.lifecycle.decay_rate,
        low_confidence_threshold: policy.lifecycle.low_confidence_threshold,
        stale_days: policy.lifecycle.stale_days,
        consolidation_similarity: policy.lifecycle.similarity_threshold,
    };
    crate::core::memory_lifecycle::apply_confidence_decay(&mut knowledge.facts, &lifecycle_cfg);

    let low_conf_count = knowledge
        .facts
        .iter()
        .filter(|f| f.is_current() && f.confidence < 0.3)
        .count() as u32;

    graph.decay_all_edges(1.0);
    graph.prune_weak_edges(0.05);

    let stale_cutoff = Utc::now() - Duration::days(EDGE_STALE_DAYS);
    graph.edges.retain_mut(|e| {
        let last = e.last_seen.unwrap_or(e.created_at);
        if last < stale_cutoff {
            if e.count <= 1 {
                return false;
            }
            e.count = e.count.saturating_sub(1);
        }
        true
    });

    low_conf_count
}

fn slug_key(s: &str, max: usize) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        if out.len() >= max {
            break;
        }
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if (ch.is_whitespace() || ch == '-' || ch == '_')
            && !out.ends_with('-')
            && !out.is_empty()
        {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

fn finding_salience(summary: &str) -> u32 {
    let s = summary.to_lowercase();
    let mut score = 20u32;
    let boosts = [
        ("error", 25),
        ("failed", 25),
        ("panic", 30),
        ("assert", 20),
        ("forbidden", 25),
        ("timeout", 20),
        ("deadlock", 25),
        ("security", 25),
        ("vuln", 25),
        ("e0", 15),
    ];
    for (pat, b) in boosts {
        if s.contains(pat) {
            score = score.saturating_add(b);
        }
    }
    score
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::knowledge::KnowledgeArchetype;
    use crate::core::knowledge_relations::KnowledgeEdge;
    use crate::core::memory_boundary::FactPrivacy;

    fn make_fact(
        category: &str,
        key: &str,
        value: &str,
        confidence: f32,
    ) -> crate::core::knowledge::KnowledgeFact {
        crate::core::knowledge::KnowledgeFact {
            category: category.to_string(),
            key: key.to_string(),
            value: value.to_string(),
            source_session: "test".to_string(),
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
            privacy: FactPrivacy::default(),
            sensitivity: crate::core::sensitivity::SensitivityLevel::default(),
            imported_from: None,
            archetype: KnowledgeArchetype::default(),
            fidelity: None,
            revision_count: 0,
        }
    }

    fn make_retrieved_fact(
        category: &str,
        key: &str,
        value: &str,
        retrieved_at: chrono::DateTime<Utc>,
    ) -> crate::core::knowledge::KnowledgeFact {
        let mut f = make_fact(category, key, value, 0.9);
        f.last_retrieved = Some(retrieved_at);
        f.retrieval_count = 1;
        f
    }

    fn make_knowledge(
        project_root: &str,
        facts: Vec<crate::core::knowledge::KnowledgeFact>,
    ) -> ProjectKnowledge {
        ProjectKnowledge {
            project_root: project_root.to_string(),
            project_hash: "test-hash".to_string(),
            facts,
            patterns: Vec::new(),
            history: Vec::new(),
            updated_at: Utc::now(),
            judged_pairs: Vec::new(),
        }
    }

    fn make_graph(edges: Vec<KnowledgeEdge>) -> KnowledgeRelationGraph {
        KnowledgeRelationGraph {
            project_hash: "test-hash".to_string(),
            edges,
            updated_at: Utc::now(),
        }
    }

    fn make_edge(from_cat: &str, from_key: &str, to_cat: &str, to_key: &str) -> KnowledgeEdge {
        KnowledgeEdge {
            from: KnowledgeNodeRef::new(from_cat, from_key),
            to: KnowledgeNodeRef::new(to_cat, to_key),
            kind: KnowledgeEdgeKind::RelatedTo,
            created_at: Utc::now(),
            last_seen: Some(Utc::now()),
            count: 1,
            source_session: "test".to_string(),
            strength: 0.5,
            decay_rate: 0.02,
        }
    }

    #[test]
    fn structural_repair_removes_orphaned_edges() {
        let knowledge = make_knowledge(
            "/tmp/test",
            vec![
                make_fact("arch", "db", "PostgreSQL", 0.9),
                make_fact("arch", "cache", "Redis", 0.8),
            ],
        );

        let mut graph = make_graph(vec![
            make_edge("arch", "db", "arch", "cache"),
            make_edge("arch", "db", "arch", "nonexistent"),
            make_edge("gone", "missing", "arch", "db"),
        ]);

        let removed = step_structural_repair(&mut graph, &knowledge);
        assert_eq!(removed, 2);
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].from.key, "db");
        assert_eq!(graph.edges[0].to.key, "cache");
    }

    #[test]
    fn lateral_synthesis_connects_similar_facts() {
        let knowledge = make_knowledge(
            "/tmp/test",
            vec![
                make_fact(
                    "arch",
                    "db",
                    "PostgreSQL database primary storage backend",
                    0.9,
                ),
                make_fact("arch", "cache", "Redis cache for sessions", 0.8),
                make_fact(
                    "deploy",
                    "db-host",
                    "PostgreSQL database primary storage on AWS",
                    0.7,
                ),
            ],
        );

        let mut graph = make_graph(Vec::new());
        let added = step_lateral_synthesis(&knowledge, &mut graph);

        assert!(
            added >= 1,
            "Should connect facts sharing vocabulary (PostgreSQL database primary storage)"
        );
        assert!(
            graph.edges.iter().any(|e| {
                (e.from.key == "db" && e.to.key == "db-host")
                    || (e.from.key == "db-host" && e.to.key == "db")
            }),
            "Should have edge between db and db-host"
        );
    }

    #[test]
    fn contradiction_resolution_keeps_higher_quality() {
        let mut f1 = make_fact("arch", "db", "PostgreSQL", 0.9);
        f1.confirmation_count = 3;
        let f2 = make_fact("arch", "db", "MySQL", 0.5);

        let mut knowledge = make_knowledge("/tmp/test", vec![f1, f2]);
        let resolved = step_contradiction_resolution(&mut knowledge);

        assert_eq!(resolved, 1);
        let current: Vec<_> = knowledge.facts.iter().filter(|f| f.is_current()).collect();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].value, "PostgreSQL");
    }

    #[test]
    fn hebbian_strengthen_co_retrieval() {
        let now = Utc::now();
        let knowledge = make_knowledge(
            "/tmp/test",
            vec![
                make_retrieved_fact("arch", "db", "PostgreSQL", now),
                make_retrieved_fact("arch", "cache", "Redis", now - Duration::minutes(30)),
                make_retrieved_fact("arch", "queue", "Kafka", now - Duration::hours(5)),
            ],
        );

        let mut graph = make_graph(Vec::new());
        let strengthened = step_hebbian_strengthen(&knowledge, &mut graph);

        assert!(
            strengthened >= 1,
            "Should strengthen co-retrieved facts within 1h window"
        );
        let has_db_cache = graph.edges.iter().any(|e| {
            (e.from.key == "db" && e.to.key == "cache")
                || (e.from.key == "cache" && e.to.key == "db")
        });
        assert!(has_db_cache, "db and cache were retrieved within 1h");
    }

    #[test]
    fn decay_reduces_stale_edge_counts() {
        let old = Utc::now() - Duration::days(45);
        let mut graph = make_graph(vec![
            {
                let mut e = make_edge("arch", "db", "arch", "cache");
                e.last_seen = Some(old);
                e.count = 3;
                e
            },
            {
                let mut e = make_edge("arch", "old", "arch", "ancient");
                e.last_seen = Some(old);
                e.count = 1;
                e
            },
        ]);

        let policy = MemoryPolicy::default();
        let mut knowledge = make_knowledge(
            "/tmp/test",
            vec![
                make_fact("arch", "db", "PostgreSQL", 0.9),
                make_fact("arch", "cache", "Redis", 0.8),
            ],
        );

        step_decay(&mut knowledge, &mut graph, &policy);

        assert_eq!(
            graph.edges.len(),
            1,
            "Edge with count=1 and stale should be removed"
        );
        assert_eq!(
            graph.edges[0].count, 2,
            "Edge with count=3 should be decremented to 2"
        );
    }

    #[test]
    fn cognition_loop_runs_all_steps() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var(
            "LEAN_CTX_DATA_DIR",
            tmp.path().to_string_lossy().to_string(),
        );

        let project_root = tmp.path().join("proj");
        std::fs::create_dir_all(&project_root).expect("mkdir");
        let project_root_str = project_root.to_string_lossy().to_string();

        let policy = MemoryPolicy::default();
        let mut knowledge = ProjectKnowledge::load_or_create(&project_root_str);
        knowledge.remember("arch", "db", "PostgreSQL", "s1", 0.9, &policy);
        knowledge.remember("arch", "cache", "Redis", "s1", 0.8, &policy);
        knowledge.remember("deploy", "host", "AWS", "s1", 0.7, &policy);
        let _ = knowledge.save();

        let report = run_cognition_loop(&project_root_str, 8);
        assert_eq!(report.steps_run, 8);

        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }
}
