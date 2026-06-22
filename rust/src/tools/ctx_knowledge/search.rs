//! Semantic knowledge search + fact formatting/salience helpers.
//! Split out of `ctx_knowledge/mod.rs`; `use super::*` re-imports parent items.

#[allow(clippy::wildcard_imports)]
use super::*;
pub(crate) fn handle_search(query: Option<&str>) -> String {
    let Some(q) = query else {
        return "Error: query is required for search".to_string();
    };

    let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() else {
        return "Cannot determine data directory.".to_string();
    };

    let sessions_dir = data_dir.join("sessions");

    if !sessions_dir.exists() {
        return "No sessions found.".to_string();
    }

    let knowledge_dir = data_dir.join("knowledge");

    let allow_cross_project = {
        let role = crate::core::roles::active_role();
        role.io.allow_cross_project_search
    };

    let current_project_hash = std::env::current_dir()
        .ok()
        .map(|p| crate::core::project_hash::hash_project_root(&p.to_string_lossy()));

    let q_lower = q.to_lowercase();
    let terms: Vec<&str> = q_lower.split_whitespace().collect();
    let mut results = Vec::new();

    if knowledge_dir.exists()
        && let Ok(entries) = std::fs::read_dir(&knowledge_dir)
    {
        for entry in entries.flatten() {
            let dir_name = entry.file_name().to_string_lossy().to_string();

            if !allow_cross_project
                && let Some(ref current_hash) = current_project_hash
                && &dir_name != current_hash
            {
                continue;
            }

            if let Some(ref current_hash) = current_project_hash
                && dir_name != *current_hash
            {
                let policy = crate::core::config::Config::load().boundary_policy;
                let allowed = crate::core::memory_boundary::check_boundary(
                    current_hash,
                    &dir_name,
                    &policy,
                    &crate::core::memory_boundary::CrossProjectEventType::Search,
                );
                crate::core::memory_boundary::record_audit_event(
                    &crate::core::memory_boundary::CrossProjectAuditEvent {
                        timestamp: Utc::now().to_rfc3339(),
                        event_type: crate::core::memory_boundary::CrossProjectEventType::Search,
                        source_project_hash: current_hash.clone(),
                        target_project_hash: dir_name.clone(),
                        tool: "ctx_knowledge".to_string(),
                        action: "search".to_string(),
                        facts_accessed: 0,
                        allowed,
                        policy_reason: if allowed {
                            "boundary_policy_allowed".to_string()
                        } else {
                            "boundary_policy_denied".to_string()
                        },
                    },
                );
                if !allowed {
                    continue;
                }
            }

            let knowledge_file = entry.path().join("knowledge.json");
            if let Ok(content) = std::fs::read_to_string(&knowledge_file)
                && let Ok(knowledge) = serde_json::from_str::<ProjectKnowledge>(&content)
            {
                let is_foreign = current_project_hash
                    .as_ref()
                    .is_some_and(|h| h != &knowledge.project_hash);

                for fact in &knowledge.facts {
                    if is_foreign
                        && fact.privacy == crate::core::memory_boundary::FactPrivacy::ProjectOnly
                    {
                        continue;
                    }

                    let searchable = format!(
                        "{} {} {}",
                        fact.category.to_lowercase(),
                        fact.key.to_lowercase(),
                        fact.value.to_lowercase()
                    );
                    let match_count = terms.iter().filter(|t| searchable.contains(**t)).count();
                    if match_count > 0 {
                        results.push((
                            knowledge.project_root.clone(),
                            fact.category.clone(),
                            fact.key.clone(),
                            fact.value.clone(),
                            fact.confidence,
                            match_count as f32 / terms.len() as f32,
                        ));
                    }
                }
            }
        }
    }

    if let Ok(entries) = std::fs::read_dir(&sessions_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if path.file_name().and_then(|n| n.to_str()) == Some("latest.json") {
                continue;
            }
            if let Ok(json) = std::fs::read_to_string(&path)
                && let Ok(session) = serde_json::from_str::<SessionState>(&json)
            {
                for finding in &session.findings {
                    let searchable = finding.summary.to_lowercase();
                    let match_count = terms.iter().filter(|t| searchable.contains(**t)).count();
                    if match_count > 0 {
                        let project = session
                            .project_root
                            .clone()
                            .unwrap_or_else(|| "unknown".to_string());
                        results.push((
                            project,
                            "session-finding".to_string(),
                            session.id.clone(),
                            finding.summary.clone(),
                            0.6,
                            match_count as f32 / terms.len() as f32,
                        ));
                    }
                }
                for decision in &session.decisions {
                    let searchable = decision.summary.to_lowercase();
                    let match_count = terms.iter().filter(|t| searchable.contains(**t)).count();
                    if match_count > 0 {
                        let project = session
                            .project_root
                            .clone()
                            .unwrap_or_else(|| "unknown".to_string());
                        results.push((
                            project,
                            "session-decision".to_string(),
                            session.id.clone(),
                            decision.summary.clone(),
                            0.7,
                            match_count as f32 / terms.len() as f32,
                        ));
                    }
                }
            }
        }
    }

    if results.is_empty() {
        return format!("No results found for '{q}' across all sessions and projects.");
    }

    results.sort_by(|a, b| {
        b.5.partial_cmp(&a.5)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.4.partial_cmp(&a.4).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| a.0.cmp(&b.0))
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
            .then_with(|| a.3.cmp(&b.3))
    });
    results.truncate(crate::core::budgets::KNOWLEDGE_CROSS_PROJECT_SEARCH_LIMIT);

    let mut out = format!("Cross-session search '{q}' ({} results):\n", results.len());
    for (project, cat, key, value, conf, _relevance) in &results {
        let project_short = short_path(project);
        out.push_str(&format!(
            "  [{cat}/{key}] {value} (project: {project_short}, conf: {:.0}%)\n",
            conf * 100.0
        ));
    }
    out
}

#[cfg(feature = "embeddings")]
pub(crate) struct SemanticHit {
    pub(crate) category: String,
    pub(crate) key: String,
    pub(crate) value: String,
    pub(crate) score: f32,
    pub(crate) semantic_score: f32,
    pub(crate) confidence_score: f32,
}

#[cfg(feature = "embeddings")]
pub(crate) fn apply_retrieval_signals_from_hits(
    knowledge: &mut ProjectKnowledge,
    hits: &[SemanticHit],
) {
    let now = Utc::now();
    for s in hits {
        for f in &mut knowledge.facts {
            if !f.is_current() {
                continue;
            }
            if f.category == s.category && f.key == s.key {
                f.retrieval_count = f.retrieval_count.saturating_add(1);
                f.last_retrieved = Some(now);
                break;
            }
        }
    }
}

#[cfg(feature = "embeddings")]
pub(crate) fn format_semantic_facts(query: &str, hits: &[SemanticHit]) -> String {
    if hits.is_empty() {
        return format!("No facts matching '{query}'.");
    }
    let mut out = format!("Semantic recall '{query}' (showing {}):\n", hits.len());
    for s in hits {
        out.push_str(&format!(
            "  [{}/{}]: {} (score: {:.0}%, sem: {:.0}%, conf: {:.0}%)\n",
            s.category,
            s.key,
            s.value,
            s.score * 100.0,
            s.semantic_score * 100.0,
            s.confidence_score * 100.0
        ));
    }
    out
}

pub(crate) fn format_facts_with_annotations(
    facts: &[crate::core::knowledge::KnowledgeFact],
    total: usize,
    category: Option<&str>,
    judged_pairs: &[crate::core::knowledge::JudgedPair],
) -> String {
    // Preserve the caller's order: recall_for_output / recall_by_category_for_output
    // already rank facts (balanced observation tier #802, exact-match precedence,
    // relevance). Re-sorting here by salience would discard that ranking and bury a
    // synthesized summary under a high-salience raw fact. The formatter renders; it
    // does not re-rank.
    let facts: Vec<&crate::core::knowledge::KnowledgeFact> = facts.iter().collect();

    let mut out = String::new();
    if let Some(cat) = category {
        out.push_str(&format!(
            "Facts [{cat}] (showing {}/{}):\n",
            facts.len(),
            total
        ));
    } else {
        out.push_str(&format!(
            "Matching facts (showing {}/{}):\n",
            facts.len(),
            total
        ));
    }
    for f in facts {
        let temporal = if f.is_current() { "" } else { " [archived]" };
        let rev = if f.revision_count > 1 {
            format!(" rev {}", f.revision_count)
        } else {
            String::new()
        };
        out.push_str(&format!(
            "  [{}/{}]: {} (quality: {:.0}%, confidence: {:.0}%, confirmed: {} x{}){rev}{temporal}\n",
            f.category,
            f.key,
            f.value,
            f.quality_score() * 100.0,
            f.confidence * 100.0,
            f.last_confirmed.format("%Y-%m-%d"),
            f.confirmation_count
        ));

        if !judged_pairs.is_empty() {
            let composite = format!("{}/{}", f.category, f.key);
            for jp in judged_pairs {
                if jp.key_a == composite {
                    out.push_str(&format!("    ↳ {} {}\n", jp.verdict, jp.key_b));
                } else if jp.key_b == composite && jp.verdict == "supersedes" {
                    out.push_str(&format!("    ↳ superseded by {}\n", jp.key_a));
                }
            }
        }
    }
    out
}

pub(crate) fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 2 {
        return path.to_string();
    }
    parts[parts.len() - 2..].join("/")
}

pub(crate) fn short_hash(hash: &str) -> &str {
    if hash.len() > 8 { &hash[..8] } else { hash }
}

pub(crate) fn sort_fact_for_output(
    a: &crate::core::knowledge::KnowledgeFact,
    b: &crate::core::knowledge::KnowledgeFact,
) -> std::cmp::Ordering {
    // Pure salience ordering. The observation tier (#802) is applied at the selection
    // layer (recall_*), not here; this comparator is the as-of recall tiebreak and the
    // display only preserves the order the recall functions already produced.
    salience_score(b)
        .cmp(&salience_score(a))
        .then_with(|| {
            b.quality_score()
                .partial_cmp(&a.quality_score())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .then_with(|| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .then_with(|| b.confirmation_count.cmp(&a.confirmation_count))
        .then_with(|| b.retrieval_count.cmp(&a.retrieval_count))
        .then_with(|| b.last_retrieved.cmp(&a.last_retrieved))
        .then_with(|| b.last_confirmed.cmp(&a.last_confirmed))
        .then_with(|| a.category.cmp(&b.category))
        .then_with(|| a.key.cmp(&b.key))
        .then_with(|| a.value.cmp(&b.value))
}

pub(crate) fn salience_score(f: &crate::core::knowledge::KnowledgeFact) -> u32 {
    let cat = f.category.to_lowercase();
    let base: u32 = match cat.as_str() {
        "decision" => 70,
        "gotcha" => 75,
        "architecture" | "arch" => 60,
        "security" => 65,
        "testing" | "tests" | "deployment" | "deploy" => 55,
        "conventions" | "convention" => 45,
        "finding" => 40,
        _ => 30,
    };

    let quality_bonus = (f.quality_score() * 60.0) as u32;
    let recency_bonus = f.last_retrieved.map_or(0u32, |t| {
        let days = chrono::Utc::now().signed_duration_since(t).num_days();
        if days <= 7 {
            10u32
        } else if days <= 30 {
            5u32
        } else {
            0u32
        }
    });

    base + quality_bonus + recency_bonus
}

#[cfg(test)]
mod tests {
    use crate::core::knowledge::{COGNITION_SYNTHESIS_SOURCE, ProjectKnowledge};
    use crate::core::memory_policy::MemoryPolicy;

    // #802: the formatter renders in the caller's order and must never re-rank.
    // `recall_for_output` ranks a synthesized observation ahead via the balanced tier;
    // the display must preserve that, not re-sort by salience (which would bury the
    // summary under a high-salience gotcha). Feeding both orders proves no re-sort.
    #[test]
    fn format_preserves_caller_order_not_salience() {
        let policy = MemoryPolicy::default();
        let mut k = ProjectKnowledge::new("/tmp/test-fmt-preserve");
        k.remember(
            "gotcha",
            "src/a.rs:1",
            "race condition",
            "s1",
            0.95,
            &policy,
        );
        k.remember(
            "observation",
            "src/a.rs",
            "src/a.rs — gotcha: race condition",
            COGNITION_SYNTHESIS_SOURCE,
            0.60,
            &policy,
        );
        let obs = k
            .facts
            .iter()
            .find(|f| f.is_synthesized_observation())
            .cloned()
            .expect("observation present");
        let gotcha = k
            .facts
            .iter()
            .find(|f| !f.is_synthesized_observation())
            .cloned()
            .expect("gotcha present");

        let out =
            super::format_facts_with_annotations(&[obs.clone(), gotcha.clone()], 2, None, &[]);
        assert!(
            out.find("[observation/").unwrap() < out.find("[gotcha/").unwrap(),
            "observation ranked first by recall must stay first in the rendered output"
        );

        let out_rev = super::format_facts_with_annotations(&[gotcha, obs], 2, None, &[]);
        assert!(
            out_rev.find("[gotcha/").unwrap() < out_rev.find("[observation/").unwrap(),
            "formatter must preserve caller order, never impose its own salience sort"
        );
    }
}
