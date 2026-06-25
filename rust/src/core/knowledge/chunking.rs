//! Theta-gamma chunking for wakeup facts (#543, EFF-6).
//!
//! Working memory holds ~4±1 items as *chunks* nested in theta cycles, not as
//! a flat list (Lisman/Idiart). The LLM equivalent: semantically related
//! facts grouped under one shared header prime each other and amortize the
//! shared context (category, path prefixes) — fewer tokens, better recall.
//!
//! Deterministic greedy agglomerative clustering — no RNG, no model:
//! similarity = token-Jaccard over key+value, same-category bonus, and
//! file-path co-reference bonus. Cluster topics are derived lexically
//! (dominant category or shared path token), never via an LLM call.

use super::types::KnowledgeFact;
use crate::core::memory_consolidation::token_jaccard;

/// Theta capacity: clusters never grow beyond this many facts (7 = 4±1 max).
const MAX_CLUSTER_SIZE: usize = 6;
/// Minimum blended similarity to join an existing cluster. Calibrated so
/// same-category alone qualifies (categories are lean-ctx "rooms" = topics),
/// while cross-category joins need a strong lexical/path signal.
const JOIN_THRESHOLD: f64 = 0.3;

#[derive(Debug)]
pub struct FactCluster<'a> {
    pub topic: String,
    pub facts: Vec<&'a KnowledgeFact>,
}

fn fact_text(f: &KnowledgeFact) -> String {
    format!("{} {}", f.key, f.value)
}

/// Path-like tokens (contain '/' or a file extension) — co-reference of the
/// same file/module is a strong grouping signal in practice.
fn path_tokens(s: &str) -> Vec<String> {
    s.split_whitespace()
        .filter(|t| t.contains('/') || t.contains(".rs") || t.contains(".ts") || t.contains(".py"))
        .map(|t| t.trim_matches(|c: char| !c.is_alphanumeric() && c != '/' && c != '.'))
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn path_overlap(a: &str, b: &str) -> f64 {
    let pa: std::collections::HashSet<String> = path_tokens(a).into_iter().collect();
    let pb: std::collections::HashSet<String> = path_tokens(b).into_iter().collect();
    if pa.is_empty() || pb.is_empty() {
        return 0.0;
    }
    let inter = pa.intersection(&pb).count() as f64;
    let union = pa.union(&pb).count() as f64;
    inter / union
}

/// Blended pairwise similarity between two facts.
fn fact_similarity(a: &KnowledgeFact, b: &KnowledgeFact) -> f64 {
    let ta = fact_text(a);
    let tb = fact_text(b);
    let lexical = token_jaccard(&ta, &tb);
    let same_cat = if a.category == b.category { 1.0 } else { 0.0 };
    let paths = path_overlap(&ta, &tb);
    0.5 * lexical + 0.3 * same_cat + 0.2 * paths
}

/// Average similarity of `f` to the members of a cluster.
fn cluster_affinity(f: &KnowledgeFact, cluster: &FactCluster<'_>) -> f64 {
    if cluster.facts.is_empty() {
        return 0.0;
    }
    cluster
        .facts
        .iter()
        .map(|m| fact_similarity(f, m))
        .sum::<f64>()
        / cluster.facts.len() as f64
}

/// Lexical topic for a cluster: the dominant category if it covers the
/// majority of members, otherwise the most frequent path token, otherwise
/// the dominant category anyway (deterministic tie-break by name).
fn derive_topic(facts: &[&KnowledgeFact]) -> String {
    use std::collections::HashMap;

    let mut cat_counts: HashMap<&str, usize> = HashMap::new();
    for f in facts {
        *cat_counts.entry(f.category.as_str()).or_insert(0) += 1;
    }
    let (dominant_cat, cat_n) = cat_counts
        .iter()
        .max_by_key(|(name, n)| (**n, std::cmp::Reverse(*name)))
        .map_or(("facts", 0), |(name, n)| (*name, *n));

    if cat_n * 2 > facts.len() {
        return dominant_cat.to_string();
    }

    let mut path_counts: HashMap<String, usize> = HashMap::new();
    for f in facts {
        for t in path_tokens(&fact_text(f)) {
            *path_counts.entry(t).or_insert(0) += 1;
        }
    }
    path_counts
        .into_iter()
        .filter(|(_, n)| *n >= 2)
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
        .map_or_else(|| dominant_cat.to_string(), |(t, _)| t)
}

/// Greedy agglomerative chunking. Input order is the salience order — the
/// first member of each cluster is its most salient fact, and clusters are
/// returned in the order of their founding (= salience) fact.
#[must_use]
pub fn cluster_facts<'a>(facts: &[&'a KnowledgeFact]) -> Vec<FactCluster<'a>> {
    let mut clusters: Vec<FactCluster<'a>> = Vec::new();

    for f in facts {
        let best = clusters
            .iter_mut()
            .filter(|c| c.facts.len() < MAX_CLUSTER_SIZE)
            .map(|c| {
                let affinity = cluster_affinity(f, c);
                (affinity, c)
            })
            .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        match best {
            Some((affinity, cluster)) if affinity >= JOIN_THRESHOLD => cluster.facts.push(f),
            _ => clusters.push(FactCluster {
                topic: String::new(),
                facts: vec![f],
            }),
        }
    }

    for c in &mut clusters {
        c.topic = derive_topic(&c.facts);
    }
    clusters
}

/// Render clusters in the compact wakeup notation: one line per chunk,
/// `[topic] key=val|key=val`. Facts whose category equals the topic drop the
/// category prefix entirely (the header amortizes it) — that is where the
/// token savings over the flat `cat/key=val|cat/key=val` list come from.
#[must_use]
pub fn render_chunked(clusters: &[FactCluster<'_>]) -> String {
    let mut out = String::from("FACTS:\n");
    for c in clusters {
        let items: Vec<String> = c
            .facts
            .iter()
            .map(|f| {
                let key = crate::core::sanitize::neutralize_metadata(&f.key);
                let val = crate::core::sanitize::neutralize_metadata(&f.value);
                if f.category == c.topic {
                    format!("{key}={val}")
                } else {
                    let cat = crate::core::sanitize::neutralize_metadata(&f.category);
                    format!("{cat}/{key}={val}")
                }
            })
            .collect();
        let topic = crate::core::sanitize::neutralize_metadata(&c.topic);
        out.push_str(&format!("[{topic}] {}\n", items.join("|")));
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn fact(cat: &str, key: &str, val: &str) -> KnowledgeFact {
        // Build via serde so all `#[serde(default)]` fields fill themselves.
        serde_json::from_value(serde_json::json!({
            "category": cat,
            "key": key,
            "value": val,
            "source_session": "test",
            "confidence": 0.9,
            "created_at": Utc::now(),
            "last_confirmed": Utc::now(),
        }))
        .expect("valid fact json")
    }

    fn three_topic_facts() -> Vec<KnowledgeFact> {
        vec![
            fact(
                "billing-stripe",
                "webhook",
                "parses cancel_at from stripe payloads",
            ),
            fact(
                "billing-stripe",
                "emails",
                "welcome email sent on subscription start",
            ),
            fact(
                "billing-stripe",
                "purge",
                "account purge runs in one transaction",
            ),
            fact(
                "billing-stripe",
                "portal",
                "cancellation reasons enabled in portal",
            ),
            fact(
                "billing-stripe",
                "entitlements",
                "internal key header required",
            ),
            fact(
                "billing-stripe",
                "metadata",
                "update format is metadata bracket key",
            ),
            fact(
                "dashboard-ui",
                "heatmap",
                "renders bounce counters per file",
            ),
            fact("dashboard-ui", "charts", "cumulative actions over 90 days"),
            fact("dashboard-ui", "auth", "token required in sessionStorage"),
            fact("dashboard-ui", "port", "dashboard serves on 7421 with flag"),
            fact(
                "dashboard-ui",
                "pressure",
                "context pressure table lists files",
            ),
            fact("dashboard-ui", "roi", "savings ledger feeds roi panel"),
            fact(
                "infrastructure",
                "deploy",
                "rsync to pounce-server then script",
            ),
            fact(
                "infrastructure",
                "docker",
                "billing uses separate database container",
            ),
            fact(
                "infrastructure",
                "launchagent",
                "keepalive respawns the proxy",
            ),
            fact("infrastructure", "postgres", "cloud user is leanctx_cloud"),
            fact(
                "infrastructure",
                "traefik",
                "routes by host rule to services",
            ),
            fact("infrastructure", "smtp", "zeptomail sends lifecycle emails"),
        ]
    }

    #[test]
    fn three_topics_form_three_clusters() {
        let facts = three_topic_facts();
        let refs: Vec<&KnowledgeFact> = facts.iter().collect();
        let clusters = cluster_facts(&refs);
        assert_eq!(clusters.len(), 3, "one cluster per topic: {clusters:#?}");
        for c in &clusters {
            assert!(c.facts.len() <= MAX_CLUSTER_SIZE);
            let cats: std::collections::HashSet<&str> =
                c.facts.iter().map(|f| f.category.as_str()).collect();
            assert_eq!(cats.len(), 1, "category-pure clusters for this fixture");
        }
    }

    #[test]
    fn cluster_never_exceeds_theta_capacity() {
        let facts: Vec<KnowledgeFact> = (0..20)
            .map(|i| {
                fact(
                    "arch",
                    &format!("component_{i}"),
                    "shares the same words entirely",
                )
            })
            .collect();
        let refs: Vec<&KnowledgeFact> = facts.iter().collect();
        let clusters = cluster_facts(&refs);
        for c in &clusters {
            assert!(c.facts.len() <= MAX_CLUSTER_SIZE, "got {}", c.facts.len());
        }
        assert!(clusters.len() >= 20 / MAX_CLUSTER_SIZE);
    }

    #[test]
    fn chunked_rendering_saves_tokens_vs_flat() {
        use crate::core::tokens::count_tokens;
        let facts = three_topic_facts();
        let refs: Vec<&KnowledgeFact> = facts.iter().collect();

        let flat: Vec<String> = refs
            .iter()
            .map(|f| format!("{}/{}={}", f.category, f.key, f.value))
            .collect();
        let flat_block = format!("FACTS:{}", flat.join("|"));

        let clusters = cluster_facts(&refs);
        let chunked_block = render_chunked(&clusters);

        let flat_tok = count_tokens(&flat_block);
        let chunk_tok = count_tokens(&chunked_block);
        assert!(
            (chunk_tok as f64) <= (flat_tok as f64) * 0.9,
            "chunked must save >=10% tokens: flat={flat_tok} chunked={chunk_tok}"
        );
    }

    #[test]
    fn singletons_stay_readable() {
        let f1 = fact("misc", "lone", "completely unrelated standalone fact");
        let refs = vec![&f1];
        let clusters = cluster_facts(&refs);
        assert_eq!(clusters.len(), 1);
        let rendered = render_chunked(&clusters);
        assert!(rendered.contains("[misc] lone="));
    }

    #[test]
    fn deterministic_across_runs() {
        let facts = three_topic_facts();
        let refs: Vec<&KnowledgeFact> = facts.iter().collect();
        let a = render_chunked(&cluster_facts(&refs));
        let b = render_chunked(&cluster_facts(&refs));
        assert_eq!(a, b);
    }
}
