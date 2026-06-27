//! Memory Lifecycle Management — consolidation, decay, compaction, archival.
//!
//! Runs automatically on knowledge stores to keep memory healthy:
//! - Confidence decay over time
//! - Semantic consolidation of similar facts
//! - Compaction when limits are exceeded
//! - Archival of old/unused facts

use chrono::{DateTime, Duration, Utc};
use std::path::PathBuf;

use super::knowledge::{KnowledgeFact, sort_fact_for_output};
use super::memory_archive::{ArchiveConfig, MemoryStore};

const DEFAULT_DECAY_RATE: f32 = 0.01;
const DEFAULT_MAX_FACTS: usize = 1000;
const LOW_CONFIDENCE_THRESHOLD: f32 = 0.3;
const STALE_DAYS: i64 = 30;
/// Default proactive headroom on a capacity reclaim: settle a full store at 75%
/// so it keeps real working room instead of churning at its cap.
pub const DEFAULT_RECLAIM_HEADROOM_PCT: f32 = 0.25;

/// Spacing/testing effect: how strongly each prior retrieval lengthens memory
/// stability. 0.5 ⇒ ~10 retrievals make a fact roughly 6× more durable.
const SPACING_GAIN: f32 = 0.5;
/// Floor on derived stability (days) so even a heavily down-voted fact decays
/// smoothly rather than collapsing in a single pass.
const MIN_STABILITY_DAYS: f32 = 1.0;
/// Confidence never decays below this — archival happens elsewhere, decay never
/// hard-deletes.
const CONFIDENCE_FLOOR: f32 = 0.05;
/// Default characteristic memory stability (days) for the Ebbinghaus curve.
pub const DEFAULT_BASE_STABILITY_DAYS: f32 = 90.0;

/// Which forgetting curve drives confidence decay (#1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ForgettingModel {
    /// Exponential retention `R = exp(-Δt / S)` with spacing-boosted stability
    /// `S` (Ebbinghaus forgetting curve + SM-2 spacing). Deterministic, the
    /// default: durable memories fade gracefully, rehearsed ones persist.
    #[default]
    Ebbinghaus,
    /// Legacy linear subtraction, kept for reproducibility / explicit opt-out.
    Linear,
}

impl ForgettingModel {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "linear" => Self::Linear,
            _ => Self::Ebbinghaus,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ebbinghaus => "ebbinghaus",
            Self::Linear => "linear",
        }
    }
}

#[derive(Debug, Clone)]
pub struct LifecycleConfig {
    pub decay_rate_per_day: f32,
    pub max_facts: usize,
    pub low_confidence_threshold: f32,
    pub stale_days: i64,
    pub consolidation_similarity: f32,
    /// Forgetting curve (#1). Defaults to Ebbinghaus.
    pub forgetting_model: ForgettingModel,
    /// Characteristic stability (days) for the Ebbinghaus curve before spacing
    /// and feedback modulation.
    pub base_stability_days: f32,
    /// When true, scale stability by the fact's archetype so structural *evidence*
    /// (architecture/dependency/…) decays slower than *inference* (#802/cognition).
    /// Default false keeps the baseline tuning byte-for-byte.
    pub archetype_aware_decay: bool,
    /// Archive facts untouched for this many days that were **never** retrieved —
    /// dead weight that costs injection tokens regardless of confidence (#962).
    /// `None` disables it (the default, so existing tuning is unchanged); the
    /// production policy can opt in. Reversible: pruned facts go to the archive.
    pub prune_unretrieved_after_days: Option<i64>,
    /// Proactive headroom on a capacity reclaim (#995): when a store reaches its
    /// cap, drop down to `reclaim_target(max_facts, reclaim_headroom_pct)` so it
    /// settles with working room instead of churning at the cap. `0.25` = 75%.
    pub reclaim_headroom_pct: f32,
    /// Master switch for the proactive capacity reclaim (#995). `false` restores
    /// the legacy "trim only the overflow" behavior — the documented escape
    /// hatch. Eviction stays lossless either way (excess is archived).
    pub reclaim_enabled: bool,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            decay_rate_per_day: DEFAULT_DECAY_RATE,
            max_facts: DEFAULT_MAX_FACTS,
            low_confidence_threshold: LOW_CONFIDENCE_THRESHOLD,
            stale_days: STALE_DAYS,
            consolidation_similarity: 0.85,
            forgetting_model: ForgettingModel::default(),
            base_stability_days: DEFAULT_BASE_STABILITY_DAYS,
            archetype_aware_decay: false,
            prune_unretrieved_after_days: None,
            reclaim_headroom_pct: DEFAULT_RECLAIM_HEADROOM_PCT,
            reclaim_enabled: true,
        }
    }
}

impl LifecycleConfig {
    /// Map the persisted [`crate::core::memory_policy::MemoryPolicy`] to the
    /// runtime lifecycle config. The single mapping site, so adding a knob
    /// touches exactly one place (previously duplicated across the lifecycle and
    /// cognition callers).
    pub fn from_policy(policy: &crate::core::memory_policy::MemoryPolicy) -> Self {
        Self {
            max_facts: policy.knowledge.max_facts,
            decay_rate_per_day: policy.lifecycle.decay_rate,
            low_confidence_threshold: policy.lifecycle.low_confidence_threshold,
            stale_days: policy.lifecycle.stale_days,
            consolidation_similarity: policy.lifecycle.similarity_threshold,
            forgetting_model: ForgettingModel::parse(&policy.lifecycle.forgetting_model),
            base_stability_days: policy.lifecycle.base_stability_days,
            archetype_aware_decay: policy.lifecycle.archetype_aware_decay,
            prune_unretrieved_after_days: policy.lifecycle.prune_unretrieved_after_days,
            reclaim_headroom_pct: policy.lifecycle.reclaim_headroom_pct,
            reclaim_enabled: policy.lifecycle.reclaim_enabled,
        }
    }
}

#[derive(Debug, Default)]
pub struct LifecycleReport {
    pub decayed_count: usize,
    pub consolidated_count: usize,
    pub archived_count: usize,
    pub compacted_count: usize,
    /// Of `archived_count`, how many facts were evicted purely for capacity
    /// (the proactive reclaim) versus quality (low-confidence/stale/unretrieved).
    /// Lets callers report per-store capacity reclaim distinctly (#995).
    pub capacity_archived: usize,
    pub remaining_facts: usize,
}

pub fn apply_confidence_decay(facts: &mut [KnowledgeFact], config: &LifecycleConfig) -> usize {
    let now = Utc::now();
    let mut count = 0;

    for fact in facts.iter_mut() {
        if !fact.is_current() {
            continue;
        }

        if let Some(valid_until) = fact.valid_until
            && valid_until < now
            && fact.confidence > 0.1
        {
            fact.confidence = 0.1;
            count += 1;
            continue;
        }

        let days_since_confirmed = now.signed_duration_since(fact.last_confirmed).num_days() as f32;
        if days_since_confirmed <= 0.0 {
            continue;
        }
        let days_since_retrieved = fact
            .last_retrieved
            .map_or(3650.0, |t| now.signed_duration_since(t).num_days() as f32);
        let retrieval_count = fact.retrieval_count as f32;
        let net_feedback = i64::from(fact.feedback_up) - i64::from(fact.feedback_down);

        // Archetype-aware stability (opt-in): structural evidence is more durable
        // than inference. Off by default → identical to the prior baseline.
        let base_stability = if config.archetype_aware_decay {
            config.base_stability_days * fact.archetype.stability_multiplier()
        } else {
            config.base_stability_days
        };

        let new_confidence = match config.forgetting_model {
            ForgettingModel::Ebbinghaus => ebbinghaus_confidence(
                fact.confidence,
                days_since_confirmed,
                days_since_retrieved,
                retrieval_count,
                net_feedback,
                base_stability,
            ),
            ForgettingModel::Linear => linear_confidence(
                fact.confidence,
                days_since_confirmed,
                days_since_retrieved,
                retrieval_count,
                net_feedback,
                config.decay_rate_per_day,
            ),
        };
        if (new_confidence - fact.confidence).abs() > 0.001 {
            fact.confidence = new_confidence;
            count += 1;
        }
    }

    if count > 0 && config.forgetting_model == ForgettingModel::Ebbinghaus {
        crate::core::introspect::tick("power_law_decay");
    }
    count
}

/// Ebbinghaus retention `R = exp(-Δt / S)` (#1). Stability `S` grows with the
/// spacing effect (each prior retrieval) and net feedback; `Δt` is time since
/// the memory was last reinforced (confirmed *or* retrieved). Multiplicative so
/// confidence approaches the floor smoothly and never overshoots. Deterministic.
fn ebbinghaus_confidence(
    confidence: f32,
    days_since_confirmed: f32,
    days_since_retrieved: f32,
    retrieval_count: f32,
    net_feedback: i64,
    base_stability_days: f32,
) -> f32 {
    let elapsed = days_since_confirmed.min(days_since_retrieved).max(0.0);
    let spacing = 1.0 + SPACING_GAIN * retrieval_count;
    let feedback_mult = match net_feedback.cmp(&0) {
        std::cmp::Ordering::Greater => 1.0 + (net_feedback as f32).ln_1p(),
        std::cmp::Ordering::Less => 1.0 / (1.0 + (net_feedback.unsigned_abs() as f32).ln_1p()),
        std::cmp::Ordering::Equal => 1.0,
    };
    let stability = (base_stability_days * spacing * feedback_mult).max(MIN_STABILITY_DAYS);
    let retention = (-(f64::from(elapsed)) / f64::from(stability)).exp() as f32;
    (confidence * retention).max(CONFIDENCE_FLOOR)
}

/// Legacy linear subtraction, preserved verbatim for `forgetting_model = linear`.
/// FadeMem-inspired: protect frequently/recently retrieved facts; feedback
/// steers retention. Deterministic, local-only.
fn linear_confidence(
    confidence: f32,
    days_since_confirmed: f32,
    days_since_retrieved: f32,
    retrieval_count: f32,
    net_feedback: i64,
    decay_rate_per_day: f32,
) -> f32 {
    let freq_protect = 1.0 / (1.0 + retrieval_count.ln_1p());
    let recency_protect = (1.0 - (days_since_retrieved / 30.0).min(1.0)).max(0.0);
    let protect = (freq_protect * (1.0 - 0.5 * recency_protect)).max(0.05);
    let feedback_factor = match net_feedback.cmp(&0) {
        std::cmp::Ordering::Greater => 1.0 / (1.0 + (net_feedback as f32).ln_1p()),
        std::cmp::Ordering::Less => (1.0 + (net_feedback.unsigned_abs() as f32).ln_1p()).min(4.0),
        std::cmp::Ordering::Equal => 1.0,
    };
    let decay = decay_rate_per_day * days_since_confirmed * protect * feedback_factor;
    (confidence - decay).max(CONFIDENCE_FLOOR)
}

pub fn consolidate_similar(facts: &mut Vec<KnowledgeFact>, similarity_threshold: f32) -> usize {
    let mut to_remove: std::collections::HashSet<usize> = std::collections::HashSet::new();

    let mut category_groups: std::collections::HashMap<String, Vec<usize>> =
        std::collections::HashMap::new();
    for (i, f) in facts.iter().enumerate() {
        if f.is_current() {
            category_groups
                .entry(f.category.clone())
                .or_default()
                .push(i);
        }
    }

    for indices in category_groups.values() {
        for (pos_a, &i) in indices.iter().enumerate() {
            if to_remove.contains(&i) {
                continue;
            }
            for &j in &indices[pos_a + 1..] {
                if to_remove.contains(&j) {
                    continue;
                }
                let sim = word_similarity(&facts[i].value, &facts[j].value);
                if sim >= similarity_threshold {
                    if facts[i].confidence >= facts[j].confidence {
                        facts[i].confirmation_count += facts[j].confirmation_count;
                        if facts[j].last_confirmed > facts[i].last_confirmed {
                            facts[i].last_confirmed = facts[j].last_confirmed;
                        }
                        to_remove.insert(j);
                    } else {
                        facts[j].confirmation_count += facts[i].confirmation_count;
                        if facts[i].last_confirmed > facts[j].last_confirmed {
                            facts[j].last_confirmed = facts[i].last_confirmed;
                        }
                        to_remove.insert(i);
                        break;
                    }
                }
            }
        }
    }

    let count = to_remove.len();
    let mut sorted: Vec<usize> = to_remove.into_iter().collect();
    sorted.sort_unstable();
    for idx in sorted.into_iter().rev() {
        facts.remove(idx);
    }

    count
}

pub fn compact(
    facts: &mut Vec<KnowledgeFact>,
    config: &LifecycleConfig,
) -> (usize, Vec<KnowledgeFact>) {
    let mut archived: Vec<KnowledgeFact> = Vec::new();
    let now = Utc::now();
    let stale_threshold = now - Duration::days(config.stale_days);

    let mut to_archive: Vec<usize> = Vec::new();

    for (i, fact) in facts.iter().enumerate() {
        let recently_retrieved = fact
            .last_retrieved
            .is_some_and(|t| now.signed_duration_since(t).num_days() < 14);
        let frequently_retrieved = fact.retrieval_count >= 5;

        if fact.confidence < config.low_confidence_threshold {
            to_archive.push(i);
            continue;
        }

        // Real pruning (#962): a single-confirmation fact untouched for the
        // configured horizon that was *never* retrieved is dead weight even at
        // high confidence — archive it. Gated on `confirmation_count <= 1` so
        // repeatedly-confirmed (structurally important) facts are always kept.
        if let Some(days) = config.prune_unretrieved_after_days {
            let cutoff = now - Duration::days(days);
            if fact.last_confirmed < cutoff
                && fact.retrieval_count == 0
                && fact.last_retrieved.is_none()
                && fact.confirmation_count <= 1
            {
                to_archive.push(i);
                continue;
            }
        }

        if fact.last_confirmed < stale_threshold
            && fact.confirmation_count <= 1
            && fact.confidence < 0.5
            && !recently_retrieved
            && !frequently_retrieved
        {
            to_archive.push(i);
        }
    }

    to_archive.sort_unstable();
    to_archive.dedup();

    for idx in to_archive.into_iter().rev() {
        archived.push(facts.remove(idx));
    }

    // Quality-only archival here. Capacity reclaim moved to `run_lifecycle` so it
    // flows through the single capacity manager ([`crate::core::memory_capacity`])
    // like every other store, keeping `compact` a pure quality pass.
    (archived.len(), archived)
}

/// Guardrails for cluster compaction (#971). See
/// [`crate::core::memory_policy::CompactionPolicy`] for field meanings.
#[derive(Debug, Clone)]
pub struct ClusterCompactionConfig {
    pub min_cluster: usize,
    pub similarity: f32,
    pub max_confidence: f32,
    pub max_confirmations: u32,
}

/// Maximum digest value length (chars). Bounded so a digest never re-bloats the
/// store it was meant to shrink.
const COMPACTION_VALUE_MAX: usize = 400;

/// Collapse clusters of low-value, mutually-similar, same-category facts into one
/// recoverable digest each. Returns `(clusters_collapsed, archived_originals)`;
/// the caller archives the originals so the operation is lossless. Deterministic:
/// candidates are scanned in the store's existing order and similarity ties
/// resolve to the earliest-founded cluster.
pub fn compact_clusters(
    facts: &mut Vec<KnowledgeFact>,
    cfg: &ClusterCompactionConfig,
) -> (usize, Vec<KnowledgeFact>) {
    if cfg.min_cluster < 2 {
        return (0, Vec::new());
    }
    let now = Utc::now();

    // Eligible = current, faded, barely-confirmed, cold, and not itself a digest
    // or a synthesized summary (summaries are never compacted).
    let eligible = |f: &KnowledgeFact| -> bool {
        if !f.is_current() {
            return false;
        }
        if f.source_session == crate::core::knowledge::COMPACTION_DIGEST_SOURCE
            || f.source_session == crate::core::knowledge::COGNITION_SYNTHESIS_SOURCE
        {
            return false;
        }
        let recently_retrieved = f
            .last_retrieved
            .is_some_and(|t| now.signed_duration_since(t).num_days() < 14);
        let frequently_retrieved = f.retrieval_count >= 5;
        f.confidence < cfg.max_confidence
            && f.confirmation_count <= cfg.max_confirmations
            && !recently_retrieved
            && !frequently_retrieved
    };

    // Group eligible indices by category, preserving first-seen order.
    let mut by_category: Vec<(String, Vec<usize>)> = Vec::new();
    for (i, f) in facts.iter().enumerate() {
        if !eligible(f) {
            continue;
        }
        match by_category.iter_mut().find(|(c, _)| *c == f.category) {
            Some((_, v)) => v.push(i),
            None => by_category.push((f.category.clone(), vec![i])),
        }
    }

    // Greedy agglomerate within each category by average word similarity, then
    // keep only clusters that reach the minimum size.
    let mut clusters: Vec<Vec<usize>> = Vec::new();
    for (_, indices) in &by_category {
        let mut cat_clusters: Vec<Vec<usize>> = Vec::new();
        for &i in indices {
            let mut best: Option<(usize, f32)> = None;
            for (ci, cl) in cat_clusters.iter().enumerate() {
                let avg = cl
                    .iter()
                    .map(|&j| word_similarity(&facts[i].value, &facts[j].value))
                    .sum::<f32>()
                    / cl.len() as f32;
                if avg >= cfg.similarity && best.is_none_or(|(_, b)| avg > b) {
                    best = Some((ci, avg));
                }
            }
            if let Some((ci, _)) = best {
                cat_clusters[ci].push(i);
            } else {
                cat_clusters.push(vec![i]);
            }
        }
        clusters.extend(
            cat_clusters
                .into_iter()
                .filter(|c| c.len() >= cfg.min_cluster),
        );
    }

    if clusters.is_empty() {
        return (0, Vec::new());
    }

    // Build a digest per cluster, then remove the originals (high→low so indices
    // stay valid) and append the digests.
    let mut remove: Vec<usize> = Vec::new();
    let mut digests: Vec<KnowledgeFact> = Vec::with_capacity(clusters.len());
    for cluster in &clusters {
        let members: Vec<&KnowledgeFact> = cluster.iter().map(|&i| &facts[i]).collect();
        digests.push(build_digest(&members, now));
        remove.extend(cluster.iter().copied());
    }

    remove.sort_unstable();
    remove.dedup();
    let mut archived: Vec<KnowledgeFact> = Vec::with_capacity(remove.len());
    for idx in remove.into_iter().rev() {
        archived.push(facts.remove(idx));
    }
    facts.extend(digests);

    (clusters.len(), archived)
}

/// Synthesize one digest fact from a cluster's members. Byte-stable for a given
/// set of members: members are sorted, the value is built deterministically, and
/// the key is content-addressed (md5 of category + sorted member keys) so a
/// re-run over the same inputs is idempotent.
fn build_digest(members: &[&KnowledgeFact], now: DateTime<Utc>) -> KnowledgeFact {
    use md5::{Digest, Md5};

    let mut sorted: Vec<&KnowledgeFact> = members.to_vec();
    sorted.sort_by(|a, b| a.key.cmp(&b.key).then_with(|| a.value.cmp(&b.value)));

    let category = sorted[0].category.clone();
    let max_conf = sorted.iter().map(|f| f.confidence).fold(0.0_f32, f32::max);
    let confirmations: u32 = sorted.iter().map(|f| f.confirmation_count).sum();

    let body: Vec<String> = sorted
        .iter()
        .map(|f| format!("{}: {}", f.key, f.value))
        .collect();
    let value_full = format!(
        "Compacted {} low-signal {category} facts — {}",
        sorted.len(),
        body.join("; ")
    );
    let value = truncate_chars(&value_full, COMPACTION_VALUE_MAX);

    let mut hasher = Md5::new();
    hasher.update(category.as_bytes());
    for f in &sorted {
        hasher.update(b"\n");
        hasher.update(f.key.as_bytes());
    }
    let hash = crate::core::agent_identity::hex_encode(&hasher.finalize());
    let key = format!("digest-{}", &hash[..8]);

    let sensitivity = crate::core::sensitivity::classify_content(&value);
    KnowledgeFact {
        category,
        key,
        value,
        source_session: crate::core::knowledge::COMPACTION_DIGEST_SOURCE.to_string(),
        confidence: max_conf,
        created_at: now,
        last_confirmed: now,
        retrieval_count: 0,
        last_retrieved: None,
        valid_from: Some(now),
        valid_until: None,
        supersedes: None,
        confirmation_count: confirmations.max(1),
        feedback_up: 0,
        feedback_down: 0,
        last_feedback: None,
        privacy: crate::core::memory_boundary::FactPrivacy::default(),
        sensitivity,
        imported_from: None,
        archetype: crate::core::knowledge::KnowledgeArchetype::Observation,
        fidelity: None,
        revision_count: 0,
    }
}

/// Truncate to at most `max` characters on a char boundary, appending an ellipsis
/// when content was dropped.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

pub fn run_lifecycle(facts: &mut Vec<KnowledgeFact>, config: &LifecycleConfig) -> LifecycleReport {
    let decayed = apply_confidence_decay(facts, config);
    let consolidated = consolidate_similar(facts, config.consolidation_similarity);
    let (compacted, archived) = compact(facts, config);

    if !archived.is_empty() {
        let _ = archive_facts(&archived);
    }

    // Capacity reclaim (#995): facts settle at headroom via the single capacity
    // manager, archiving the evicted tail losslessly under the legacy facts root
    // — the same path quality archival uses, so recall rehydration is uniform.
    let capacity_archived = crate::core::memory_capacity::reclaim_store(
        MemoryStore::Facts,
        None,
        facts,
        config.max_facts,
        config.reclaim_headroom_pct,
        config.reclaim_enabled,
        |a, b| {
            b.is_current()
                .cmp(&a.is_current())
                .then_with(|| sort_fact_for_output(a, b))
        },
    )
    .len();

    LifecycleReport {
        decayed_count: decayed,
        consolidated_count: consolidated,
        archived_count: archived.len() + capacity_archived,
        compacted_count: compacted + capacity_archived,
        capacity_archived,
        remaining_facts: facts.len(),
    }
}

/// Archive evicted facts (lossless). Facts keep the legacy global archive root
/// for backward compatibility; the generic multi-store archive lives in
/// [`crate::core::memory_archive`].
pub fn archive_facts(facts: &[KnowledgeFact]) -> Result<(), String> {
    crate::core::memory_archive::archive_items(
        MemoryStore::Facts,
        None,
        facts,
        &ArchiveConfig::from_env(),
    )
    .map(|_| ())
}

/// Restore the facts from a single archive file (legacy `facts` key supported).
pub fn restore_archive(archive_path: &str) -> Result<Vec<KnowledgeFact>, String> {
    crate::core::memory_archive::restore_items(std::path::Path::new(archive_path))
}

/// All facts archive files, sorted ascending (chronological).
pub fn list_archives() -> Vec<PathBuf> {
    crate::core::memory_archive::list_archives(MemoryStore::Facts, None)
}

/// The newest reachable facts archives for the recall-miss rehydrate path —
/// bounded by [`ArchiveConfig::rehydrate_reach`] so every retained archive is
/// reachable (closes the pre-#995 retained-vs-reachable gap).
pub fn reachable_archives(cfg: &ArchiveConfig) -> Vec<PathBuf> {
    crate::core::memory_archive::reachable_archives(MemoryStore::Facts, None, cfg)
}

fn word_similarity(a: &str, b: &str) -> f32 {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::knowledge::KnowledgeArchetype;

    /// Capacity reclaim archives the evicted tail to disk, so any test that drives
    /// [`run_lifecycle`] over capacity must sandbox the data dir.
    fn with_temp_data_dir<T>(f: impl FnOnce() -> T) -> T {
        let _lock = crate::core::data_dir::test_env_lock();
        let dir = std::env::temp_dir().join(format!(
            "lctx-lifecycle-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        let _ = std::fs::create_dir_all(&dir);
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", dir.to_str().unwrap());
        let out = f();
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    fn make_fact(category: &str, key: &str, value: &str, confidence: f32) -> KnowledgeFact {
        KnowledgeFact {
            category: category.to_string(),
            key: key.to_string(),
            value: value.to_string(),
            source_session: "s1".to_string(),
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
            privacy: crate::core::memory_boundary::FactPrivacy::default(),
            sensitivity: crate::core::sensitivity::SensitivityLevel::default(),
            imported_from: None,
            archetype: KnowledgeArchetype::default(),
            fidelity: None,
            revision_count: 0,
        }
    }

    fn make_old_fact(
        category: &str,
        key: &str,
        value: &str,
        confidence: f32,
        days_old: i64,
    ) -> KnowledgeFact {
        let past = Utc::now() - Duration::days(days_old);
        KnowledgeFact {
            category: category.to_string(),
            key: key.to_string(),
            value: value.to_string(),
            source_session: "s1".to_string(),
            confidence,
            created_at: past,
            last_confirmed: past,
            retrieval_count: 0,
            last_retrieved: None,
            valid_from: Some(past),
            valid_until: None,
            supersedes: None,
            confirmation_count: 1,
            feedback_up: 0,
            feedback_down: 0,
            last_feedback: None,
            privacy: crate::core::memory_boundary::FactPrivacy::default(),
            sensitivity: crate::core::sensitivity::SensitivityLevel::default(),
            imported_from: None,
            archetype: KnowledgeArchetype::default(),
            fidelity: None,
            revision_count: 0,
        }
    }

    #[test]
    fn decay_reduces_confidence() {
        let config = LifecycleConfig::default();
        let mut facts = vec![make_old_fact("arch", "db", "PostgreSQL", 0.9, 10)];

        let count = apply_confidence_decay(&mut facts, &config);
        assert_eq!(count, 1);
        assert!(facts[0].confidence < 0.9);
        assert!(facts[0].confidence > 0.7);
    }

    #[test]
    fn archetype_aware_decay_protects_evidence() {
        // Opt-in: structural evidence (Architecture) decays slower than inference
        // (Preference). Off (default), archetype is ignored and both decay alike.
        let mut evidence = make_old_fact("arch", "db", "PostgreSQL", 0.9, 30);
        evidence.archetype = KnowledgeArchetype::Architecture;
        let mut inference = make_old_fact("pref", "style", "tabs", 0.9, 30);
        inference.archetype = KnowledgeArchetype::Preference;

        let off = LifecycleConfig::default();
        let mut a = vec![evidence.clone(), inference.clone()];
        apply_confidence_decay(&mut a, &off);
        assert!(
            (a[0].confidence - a[1].confidence).abs() < 1e-6,
            "flag off → archetype ignored, equal decay"
        );

        let on = LifecycleConfig {
            archetype_aware_decay: true,
            ..Default::default()
        };
        let mut b = vec![evidence, inference];
        apply_confidence_decay(&mut b, &on);
        assert!(
            b[0].confidence > b[1].confidence,
            "evidence {} should outlast inference {}",
            b[0].confidence,
            b[1].confidence
        );
    }

    #[test]
    fn decay_skips_recent_facts() {
        let config = LifecycleConfig::default();
        let mut facts = vec![make_fact("arch", "db", "PostgreSQL", 0.9)];

        let count = apply_confidence_decay(&mut facts, &config);
        assert_eq!(count, 0);
    }

    #[test]
    fn feedback_steers_decay_keep_vs_forget() {
        let config = LifecycleConfig::default();
        let mut praised = make_old_fact("arch", "loved", "keep me", 0.9, 10);
        praised.feedback_up = 5;
        let mut panned = make_old_fact("arch", "hated", "forget me", 0.9, 10);
        panned.feedback_down = 5;
        let neutral = make_old_fact("arch", "meh", "neutral", 0.9, 10);

        let mut facts = vec![praised, panned, neutral];
        apply_confidence_decay(&mut facts, &config);

        let (praised_c, panned_c, neutral_c) = (
            facts[0].confidence,
            facts[1].confidence,
            facts[2].confidence,
        );

        // Reward bridge: up-voted retains more than neutral, neutral more than down-voted.
        assert!(
            praised_c > neutral_c,
            "praised {praised_c} should outlast neutral {neutral_c}"
        );
        assert!(
            neutral_c > panned_c,
            "neutral {neutral_c} should outlast panned {panned_c}"
        );
        // Even a heavily down-voted fact only fades toward the floor — never hard-deleted.
        assert!(panned_c >= 0.05);
    }

    #[test]
    fn spacing_effect_protects_frequently_retrieved() {
        // #1: under the Ebbinghaus curve, a fact retrieved many times must decay
        // slower than an identical never-retrieved fact of the same age.
        let config = LifecycleConfig::default();
        let rarely = make_old_fact("arch", "rare", "x", 0.9, 20);
        let mut often = make_old_fact("arch", "often", "y", 0.9, 20);
        often.retrieval_count = 20;
        let mut facts = vec![rarely, often];
        apply_confidence_decay(&mut facts, &config);
        assert!(
            facts[1].confidence > facts[0].confidence,
            "spacing effect: rehearsed {} should outlast un-rehearsed {}",
            facts[1].confidence,
            facts[0].confidence
        );
    }

    #[test]
    fn ebbinghaus_decay_is_deterministic() {
        // Determinism contract (#498): same input → same output, no RNG.
        let config = LifecycleConfig::default();
        let mut a = vec![make_old_fact("arch", "k", "v", 0.8, 15)];
        let mut b = a.clone();
        apply_confidence_decay(&mut a, &config);
        apply_confidence_decay(&mut b, &config);
        assert_eq!(a[0].confidence, b[0].confidence);
    }

    #[test]
    fn linear_model_still_available() {
        // Opt-out path keeps the legacy subtractive behavior.
        let config = LifecycleConfig {
            forgetting_model: ForgettingModel::Linear,
            ..Default::default()
        };
        let mut facts = vec![make_old_fact("arch", "db", "PostgreSQL", 0.9, 10)];
        let count = apply_confidence_decay(&mut facts, &config);
        assert_eq!(count, 1);
        assert!(facts[0].confidence < 0.9 && facts[0].confidence > 0.7);
    }

    #[test]
    fn forgetting_model_parses() {
        assert_eq!(ForgettingModel::parse("linear"), ForgettingModel::Linear);
        assert_eq!(
            ForgettingModel::parse("ebbinghaus"),
            ForgettingModel::Ebbinghaus
        );
        assert_eq!(
            ForgettingModel::parse("garbage"),
            ForgettingModel::Ebbinghaus
        );
    }

    #[test]
    fn consolidate_similar_facts() {
        let mut facts = vec![
            make_fact("arch", "db", "uses PostgreSQL database", 0.8),
            make_fact("arch", "db2", "uses PostgreSQL database system", 0.6),
            make_fact("ops", "deploy", "docker compose up", 0.9),
        ];

        let count = consolidate_similar(&mut facts, 0.7);
        assert!(count > 0, "Should consolidate similar facts");
        assert!(facts.len() < 3);
    }

    #[test]
    fn consolidate_keeps_different_categories() {
        let mut facts = vec![
            make_fact("arch", "db", "PostgreSQL", 0.8),
            make_fact("ops", "db", "PostgreSQL", 0.8),
        ];

        let count = consolidate_similar(&mut facts, 0.9);
        assert_eq!(count, 0, "Different categories should not consolidate");
    }

    #[test]
    fn compact_removes_low_confidence() {
        let config = LifecycleConfig::default();
        let mut facts = vec![
            make_fact("arch", "db", "PostgreSQL", 0.9),
            make_fact("arch", "cache", "Redis", 0.1),
        ];

        let (count, archived) = compact(&mut facts, &config);
        assert_eq!(count, 1);
        assert_eq!(facts.len(), 1);
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0].key, "cache");
    }

    #[test]
    fn compact_is_quality_only_and_ignores_capacity() {
        // Post-#995: capacity reclaim moved out of `compact` into the single
        // capacity manager (driven by `run_lifecycle`). A store full of healthy,
        // current, high-confidence facts is a *capacity* concern, so `compact`
        // (quality only) must leave it untouched.
        let config = LifecycleConfig {
            max_facts: 8,
            ..Default::default()
        };
        let mut facts: Vec<KnowledgeFact> = (0..8)
            .map(|i| make_fact("finding", &format!("k{i}"), &format!("value {i}"), 0.8))
            .collect();

        let (count, archived) = compact(&mut facts, &config);

        assert_eq!(count, 0, "quality compact must not evict for capacity");
        assert!(archived.is_empty());
        assert_eq!(facts.len(), 8);
    }

    #[test]
    fn run_lifecycle_reclaims_capacity_to_headroom() {
        with_temp_data_dir(|| {
            let config = LifecycleConfig {
                max_facts: 8,
                ..Default::default()
            };
            let mut facts: Vec<KnowledgeFact> = (0..8)
                .map(|i| make_fact("finding", &format!("k{i}"), &format!("value {i}"), 0.8))
                .collect();

            let report = run_lifecycle(&mut facts, &config);

            // Hysteresis: at cap (8) → settle to headroom target (6), archive 2.
            assert_eq!(report.capacity_archived, 2);
            assert_eq!(facts.len(), 6);
            assert_eq!(report.remaining_facts, 6);
        });
    }

    #[test]
    fn run_lifecycle_evicts_expired_before_current() {
        with_temp_data_dir(|| {
            let config = LifecycleConfig {
                max_facts: 4,
                ..Default::default()
            };
            let decision = make_fact("decision", "keep-decision", "important decision", 0.7);
            let finding = make_fact("finding", "keep-finding", "fresh finding", 0.9);
            let mut old = make_fact("decision", "drop-archived", "old decision", 0.95);
            // Definitively expired (not just "now") so retention ordering is stable.
            old.valid_until = Some(Utc::now() - Duration::seconds(1));
            let low = make_fact("misc", "drop-low", "low salience", 0.6);
            let mut facts = vec![old, low, finding, decision];

            let report = run_lifecycle(&mut facts, &config);
            let keys: Vec<&str> = facts.iter().map(|f| f.key.as_str()).collect();

            // 4 at cap → settle to 3; the expired fact sorts last (not current) and
            // is the single eviction, so every current fact survives.
            assert_eq!(report.capacity_archived, 1);
            assert!(keys.contains(&"keep-decision"));
            assert!(keys.contains(&"keep-finding"));
            assert!(!keys.contains(&"drop-archived"));
        });
    }

    #[test]
    fn prune_unretrieved_archives_old_never_retrieved_facts() {
        // Opt-in (#962): a 60-day-old, high-confidence, never-retrieved,
        // single-confirmation fact is dead weight and must be archived even
        // though its confidence is well above the low-confidence floor.
        let config = LifecycleConfig {
            prune_unretrieved_after_days: Some(30),
            ..Default::default()
        };
        let mut facts = vec![make_old_fact("arch", "x", "still confident", 0.9, 60)];
        let (count, archived) = compact(&mut facts, &config);
        assert_eq!(count, 1);
        assert_eq!(archived.len(), 1);
        assert!(facts.is_empty());
    }

    #[test]
    fn prune_unretrieved_is_off_by_default() {
        // Default config (None) must not touch a high-confidence stale fact —
        // existing tuning stays byte-for-byte.
        let config = LifecycleConfig::default();
        let mut facts = vec![make_old_fact("arch", "x", "still confident", 0.9, 60)];
        let (count, _) = compact(&mut facts, &config);
        assert_eq!(count, 0);
        assert_eq!(facts.len(), 1);
    }

    #[test]
    fn prune_unretrieved_keeps_retrieved_and_confirmed_facts() {
        let config = LifecycleConfig {
            prune_unretrieved_after_days: Some(30),
            ..Default::default()
        };
        let mut retrieved = make_old_fact("arch", "used", "v", 0.9, 60);
        retrieved.retrieval_count = 3;
        let mut confirmed = make_old_fact("arch", "confirmed", "v", 0.9, 60);
        confirmed.confirmation_count = 4;
        let mut facts = vec![retrieved, confirmed];
        let (count, _) = compact(&mut facts, &config);
        assert_eq!(count, 0, "retrieved or repeatedly-confirmed facts are kept");
        assert_eq!(facts.len(), 2);
    }

    #[test]
    fn compact_archives_stale_facts() {
        let config = LifecycleConfig::default();
        let mut facts = vec![
            make_fact("arch", "db", "PostgreSQL", 0.9),
            make_old_fact("arch", "old", "ancient thing", 0.4, 60),
        ];

        let (count, archived) = compact(&mut facts, &config);
        assert_eq!(count, 1);
        assert_eq!(archived[0].key, "old");
    }

    #[test]
    fn full_lifecycle_run() {
        let config = LifecycleConfig {
            max_facts: 5,
            ..Default::default()
        };

        let mut facts = vec![
            make_fact("arch", "db", "PostgreSQL", 0.9),
            make_fact("arch", "cache", "Redis", 0.8),
            make_old_fact("arch", "old1", "thing1", 0.2, 50),
            make_old_fact("arch", "old2", "thing2", 0.15, 60),
            make_fact("ops", "deploy", "docker compose", 0.7),
        ];

        let report = run_lifecycle(&mut facts, &config);
        assert!(report.remaining_facts <= config.max_facts);
        assert!(report.decayed_count > 0 || report.compacted_count > 0);
    }

    #[test]
    fn word_similarity_identical() {
        assert!((word_similarity("hello world", "hello world") - 1.0).abs() < 0.01);
    }

    #[test]
    fn word_similarity_partial() {
        let sim = word_similarity("uses PostgreSQL database", "PostgreSQL database system");
        assert!(sim >= 0.5, "Expected >= 0.5 but got {sim}");
        assert!(sim < 1.0);
    }

    #[test]
    fn word_similarity_different() {
        let sim = word_similarity("Redis cache", "Docker compose");
        assert!(sim < 0.1);
    }

    // === Cluster compaction (#971) ===

    fn cc_config() -> ClusterCompactionConfig {
        ClusterCompactionConfig {
            min_cluster: 4,
            similarity: 0.5,
            max_confidence: 0.5,
            max_confirmations: 1,
        }
    }

    fn faded_cluster(n: usize) -> Vec<KnowledgeFact> {
        (0..n)
            .map(|i| {
                make_old_fact(
                    "logs",
                    &format!("entry{i}"),
                    &format!("request handler returned a transient retry case {i}"),
                    0.2,
                    40,
                )
            })
            .collect()
    }

    #[test]
    fn compact_clusters_collapses_low_value_cluster_into_digest() {
        let mut facts = faded_cluster(5);
        let (collapsed, archived) = compact_clusters(&mut facts, &cc_config());

        assert_eq!(collapsed, 1);
        assert_eq!(archived.len(), 5, "all originals archived (recoverable)");
        assert_eq!(facts.len(), 1, "five facts became one digest");

        let digest = &facts[0];
        assert_eq!(
            digest.source_session,
            crate::core::knowledge::COMPACTION_DIGEST_SOURCE
        );
        assert!(digest.key.starts_with("digest-"));
        assert!(digest.value.contains("Compacted 5 low-signal logs facts"));
    }

    #[test]
    fn compact_clusters_leaves_high_value_facts() {
        let cfg = cc_config();

        // High confidence → above the importance ceiling.
        let mut high_conf: Vec<KnowledgeFact> = (0..5)
            .map(|i| {
                make_old_fact(
                    "logs",
                    &format!("k{i}"),
                    "request handler returned a transient retry",
                    0.9,
                    40,
                )
            })
            .collect();
        let (c1, _) = compact_clusters(&mut high_conf, &cfg);
        assert_eq!(c1, 0);
        assert_eq!(high_conf.len(), 5);

        // Frequently retrieved → valuable even when faded.
        let mut retrieved: Vec<KnowledgeFact> = (0..5)
            .map(|i| {
                let mut f = make_old_fact(
                    "logs",
                    &format!("k{i}"),
                    "request handler returned a transient retry",
                    0.2,
                    40,
                );
                f.retrieval_count = 9;
                f
            })
            .collect();
        let (c2, _) = compact_clusters(&mut retrieved, &cfg);
        assert_eq!(c2, 0);
        assert_eq!(retrieved.len(), 5);
    }

    #[test]
    fn compact_clusters_respects_min_cluster() {
        let mut facts = faded_cluster(3); // below min_cluster (4)
        let (collapsed, archived) = compact_clusters(&mut facts, &cc_config());
        assert_eq!(collapsed, 0);
        assert!(archived.is_empty());
        assert_eq!(facts.len(), 3);
    }

    #[test]
    fn compact_clusters_is_deterministic() {
        let cfg = cc_config();
        let mut a = faded_cluster(5);
        let mut b = faded_cluster(5);
        compact_clusters(&mut a, &cfg);
        compact_clusters(&mut b, &cfg);
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
        assert_eq!(a[0].key, b[0].key, "content-addressed digest key is stable");
        assert_eq!(a[0].value, b[0].value, "digest value is byte-stable");
    }

    #[test]
    fn compact_clusters_skips_digests_and_summaries() {
        let mut facts = faded_cluster(5);
        for f in &mut facts {
            f.source_session = crate::core::knowledge::COMPACTION_DIGEST_SOURCE.to_string();
        }
        let (collapsed, _) = compact_clusters(&mut facts, &cc_config());
        assert_eq!(collapsed, 0, "existing digests are never re-compacted");
        assert_eq!(facts.len(), 5);
    }

    #[test]
    fn truncate_chars_is_char_boundary_safe() {
        let s = "äöü".repeat(300); // 900 multibyte chars, well over the cap
        let t = truncate_chars(&s, 400);
        assert!(t.chars().count() <= 400);
        assert!(t.ends_with('…'));
        // No panic on a non-ASCII boundary is the real assertion here.
    }
}
