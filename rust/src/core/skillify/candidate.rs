//! Skill candidates mined from the project's session diary + knowledge facts.
//!
//! A candidate is a *distilled, potentially-reusable* pattern. We never invent
//! content — every candidate is backed by a real diary entry or knowledge fact.

use std::collections::HashMap;

use crate::core::agents::{AgentDiary, DiaryEntryType};
use crate::core::knowledge::ProjectKnowledge;

/// A distilled pattern mined from a project's memory, before the gate decides
/// whether it is durable enough to become a rule.
#[derive(Debug, Clone)]
pub struct SkillCandidate {
    /// Normalized, prefix-free slug (e.g. `stop-before-build`).
    pub slug: String,
    /// One-line human title (becomes the rule `description`).
    pub title: String,
    /// The actionable body of the rule.
    pub body: String,
    /// Origin category (decision / insight / convention / …).
    pub category: String,
    /// How many sources reinforced this pattern (confirmations + mentions).
    pub recurrence: u32,
    /// Highest source confidence (0.0..=1.0).
    pub confidence: f32,
    /// Provenance: source session ids / agent ids.
    pub sources: Vec<String>,
}

/// Knowledge categories that can become durable guidance. Progress and raw
/// blockers are status, not reusable rules, so they are excluded.
fn knowledge_category_is_skillable(cat: &str) -> bool {
    matches!(
        cat.to_ascii_lowercase().as_str(),
        "decision" | "insight" | "gotcha" | "pattern" | "convention" | "preference"
    )
}

/// Mine candidates from the project's knowledge facts and agent diaries,
/// merged by slug (recurrence summed, strongest body + confidence kept).
#[must_use]
pub fn mine_candidates(project_root: &str) -> Vec<SkillCandidate> {
    let mut by_slug: HashMap<String, SkillCandidate> = HashMap::new();

    // Source 1 — curated knowledge facts (already deduped by the knowledge layer).
    let knowledge = ProjectKnowledge::load_or_create(project_root);
    for fact in &knowledge.facts {
        if !knowledge_category_is_skillable(&fact.category) {
            continue;
        }
        let title = title_from(&fact.key, &fact.value);
        let slug = slugify(&title);
        if slug.is_empty() {
            continue;
        }
        let sources: Vec<String> = if fact.source_session.is_empty() {
            Vec::new()
        } else {
            vec![fact.source_session.clone()]
        };
        merge_candidate(
            &mut by_slug,
            SkillCandidate {
                slug,
                title,
                body: fact.value.trim().to_string(),
                category: fact.category.to_ascii_lowercase(),
                // Confirmations are explicit reinforcement; floor at 1.
                recurrence: fact.confirmation_count.max(1),
                confidence: fact.confidence,
                sources,
            },
        );
    }

    // Source 2 — diary decisions / insights / discoveries across the project's agents.
    for diary in AgentDiary::load_all_for_project(project_root) {
        for entry in &diary.entries {
            let category = match entry.entry_type {
                DiaryEntryType::Decision => "decision",
                DiaryEntryType::Insight => "insight",
                DiaryEntryType::Discovery => "discovery",
                DiaryEntryType::Progress | DiaryEntryType::Blocker => continue,
            };
            let body = entry.content.trim().to_string();
            if body.is_empty() {
                continue;
            }
            let title = title_from("", &body);
            let slug = slugify(&title);
            if slug.is_empty() {
                continue;
            }
            merge_candidate(
                &mut by_slug,
                SkillCandidate {
                    slug,
                    title,
                    body,
                    category: category.to_string(),
                    recurrence: 1,
                    confidence: 0.6,
                    sources: vec![diary.agent_id.clone()],
                },
            );
        }
    }

    let mut out: Vec<SkillCandidate> = by_slug.into_values().collect();
    out.sort_by(|a, b| {
        b.recurrence
            .cmp(&a.recurrence)
            .then(b.confidence.total_cmp(&a.confidence))
            .then(a.slug.cmp(&b.slug))
    });
    out
}

fn merge_candidate(map: &mut HashMap<String, SkillCandidate>, cand: SkillCandidate) {
    if let Some(existing) = map.get_mut(&cand.slug) {
        existing.recurrence = existing.recurrence.saturating_add(cand.recurrence);
        if cand.confidence > existing.confidence {
            existing.confidence = cand.confidence;
        }
        // Prefer the longer, more informative body.
        if cand.body.len() > existing.body.len() {
            existing.body = cand.body;
        }
        for s in cand.sources {
            if !s.is_empty() && !existing.sources.contains(&s) {
                existing.sources.push(s);
            }
        }
    } else {
        map.insert(cand.slug.clone(), cand);
    }
}

/// A concise title: prefer a short fact key, else the first sentence of the body.
fn title_from(key: &str, value: &str) -> String {
    let k = key.trim();
    if !k.is_empty() && k.chars().count() <= 80 {
        return k.to_string();
    }
    first_sentence(value)
}

fn first_sentence(text: &str) -> String {
    let t = text.trim();
    let end = t.find(['.', '\n', '!', '?']).unwrap_or(t.len());
    let candidate = t[..end].trim();
    let s = if candidate.is_empty() { t } else { candidate };
    truncate_chars(s, 80)
}

/// URL/file-safe slug: lowercase alphanumerics, single dashes, trimmed, capped.
#[must_use]
pub fn slugify(text: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for ch in text.trim().to_ascii_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            prev_dash = false;
        } else if !slug.is_empty() && !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }
    let trimmed = slug.trim_matches('-');
    truncate_chars(trimmed, 50).trim_matches('-').to_string()
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_normalizes() {
        assert_eq!(slugify("Stop before Build!"), "stop-before-build");
        assert_eq!(slugify("  multiple   spaces  "), "multiple-spaces");
        assert_eq!(slugify("___weird@@@chars###"), "weird-chars");
        assert_eq!(slugify(""), "");
    }

    #[test]
    fn first_sentence_truncates() {
        assert_eq!(
            first_sentence("Use atomic writes. And more."),
            "Use atomic writes"
        );
        assert!(first_sentence(&"x".repeat(200)).chars().count() <= 80);
    }

    #[test]
    fn merge_sums_recurrence_and_keeps_strongest() {
        let mut map = HashMap::new();
        merge_candidate(
            &mut map,
            SkillCandidate {
                slug: "s".into(),
                title: "t".into(),
                body: "short".into(),
                category: "decision".into(),
                recurrence: 1,
                confidence: 0.5,
                sources: vec!["a".into()],
            },
        );
        merge_candidate(
            &mut map,
            SkillCandidate {
                slug: "s".into(),
                title: "t".into(),
                body: "a much longer body".into(),
                category: "decision".into(),
                recurrence: 2,
                confidence: 0.9,
                sources: vec!["b".into()],
            },
        );
        let c = &map["s"];
        assert_eq!(c.recurrence, 3);
        assert_eq!(c.confidence, 0.9);
        assert_eq!(c.body, "a much longer body");
        assert_eq!(c.sources, vec!["a".to_string(), "b".to_string()]);
    }
}
