use chrono::Utc;

use super::types::{JudgedPair, KnowledgeFact};

pub(super) fn confidence_stars(confidence: f32) -> &'static str {
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

pub(super) fn string_similarity(a: &str, b: &str) -> f32 {
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

pub(crate) fn sort_fact_for_output(a: &KnowledgeFact, b: &KnowledgeFact) -> std::cmp::Ordering {
    // Pure salience ordering for display/grouping. The observation tier (#802) lives
    // in the *selection* layer (`recall_for_output`, `semantic_recall`,
    // `recall_by_category_for_output`) which has query context to keep a summary above
    // incidental matches yet below an exact key hit; the display then preserves that
    // order. Keeping this comparator tier-free avoids side effects on
    // wakeup/summary/aaak, where facts are already grouped by category.
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

/// Salience-based ranking for fact output ordering.
///
/// Unlike `quality_score()` (which is a stable, intrinsic measure of fact
/// reliability based on confidence, confirmations, and feedback), salience
/// combines category priority, quality, recency, and retrieval frequency
/// into a single sort key for _display_ ordering. Salience is volatile and
/// changes on every access; quality_score is deterministic and stable.
fn salience_score(f: &KnowledgeFact) -> u32 {
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
        let days = Utc::now().signed_duration_since(t).num_days();
        if days <= 7 {
            10u32
        } else if days <= 30 {
            5u32
        } else {
            0u32
        }
    });

    let archetype_bonus = f.archetype.salience_bonus();

    let fidelity_bonus = f
        .fidelity
        .as_ref()
        .map_or(0u32, |fi| (fi.structural * 10.0) as u32);

    base + quality_bonus + recency_bonus + archetype_bonus + fidelity_bonus
}

pub(super) fn hash_project_root(root: &str) -> String {
    crate::core::project_hash::hash_project_root(root)
}

pub(super) fn tokenize_lower(s: &str) -> impl Iterator<Item = String> + '_ {
    s.to_lowercase()
        .split(|c: char| c.is_whitespace() || c == '-' || c == '_' || c == '/' || c == '.')
        .filter(|t| !t.is_empty())
        .map(String::from)
        .collect::<Vec<_>>()
        .into_iter()
}

pub(super) fn build_token_index(
    facts: &[KnowledgeFact],
    include_session: bool,
) -> std::collections::HashMap<String, Vec<usize>> {
    let mut index: std::collections::HashMap<String, Vec<usize>> = std::collections::HashMap::new();
    for (i, f) in facts.iter().enumerate() {
        for token in tokenize_lower(&f.category) {
            index.entry(token).or_default().push(i);
        }
        for token in tokenize_lower(&f.key) {
            index.entry(token).or_default().push(i);
        }
        for token in tokenize_lower(&f.value) {
            index.entry(token).or_default().push(i);
        }
        if include_session {
            for token in tokenize_lower(&f.source_session) {
                index.entry(token).or_default().push(i);
            }
        }
    }
    for indices in index.values_mut() {
        indices.sort_unstable();
        indices.dedup();
    }
    index
}

#[derive(Debug, Clone)]
pub struct SimilarFact {
    pub category: String,
    pub key: String,
    pub value_preview: String,
    pub similarity: f32,
}

pub fn find_cross_key_similar(
    new_category: &str,
    new_key: &str,
    new_value: &str,
    all_facts: &[KnowledgeFact],
    judged_pairs: &[JudgedPair],
    limit: usize,
) -> Vec<SimilarFact> {
    let composite_key = format!("{new_category}/{new_key}");
    let mut results: Vec<SimilarFact> = Vec::new();

    for f in all_facts {
        if !f.is_current() {
            continue;
        }
        let other_key = format!("{}/{}", f.category, f.key);
        if other_key == composite_key {
            continue;
        }

        let already_judged = judged_pairs.iter().any(|jp| {
            (jp.key_a == composite_key && jp.key_b == other_key)
                || (jp.key_a == other_key && jp.key_b == composite_key)
        });
        if already_judged {
            continue;
        }

        let sim = string_similarity(new_value, &f.value);
        if sim > 0.35 {
            let preview = if f.value.len() > 60 {
                format!("{}...", &f.value[..57])
            } else {
                f.value.clone()
            };
            results.push(SimilarFact {
                category: f.category.clone(),
                key: f.key.clone(),
                value_preview: preview,
                similarity: sim,
            });
        }
    }

    results.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(limit);
    results
}

pub(super) fn fact_version_id_v1(f: &KnowledgeFact) -> String {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    hasher.update(f.category.as_bytes());
    hasher.update(b"\n");
    hasher.update(f.key.as_bytes());
    hasher.update(b"\n");
    hasher.update(f.value.as_bytes());
    hasher.update(b"\n");
    hasher.update(f.source_session.as_bytes());
    hasher.update(b"\n");
    hasher.update(f.created_at.to_rfc3339().as_bytes());
    crate::core::agent_identity::hex_encode(&hasher.finalize())
}
