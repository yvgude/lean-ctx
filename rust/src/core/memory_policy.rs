use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct MemoryPolicy {
    pub knowledge: KnowledgePolicy,
    pub episodic: EpisodicPolicy,
    pub procedural: ProceduralPolicy,
    pub lifecycle: LifecyclePolicy,
    pub embeddings: EmbeddingsPolicy,
    pub gotcha: GotchaPolicy,
    pub admission: AdmissionPolicy,
    pub compaction: CompactionPolicy,
}

impl MemoryPolicy {
    pub fn apply_env_overrides(&mut self) {
        self.knowledge.apply_env_overrides();
        self.episodic.apply_env_overrides();
        self.procedural.apply_env_overrides();
        self.lifecycle.apply_env_overrides();
        self.embeddings.apply_env_overrides();
        self.gotcha.apply_env_overrides();
        self.admission.apply_env_overrides();
        self.compaction.apply_env_overrides();
    }

    pub fn apply_overrides(&mut self, o: &MemoryPolicyOverrides) {
        self.knowledge.apply_overrides(&o.knowledge);
        self.lifecycle.apply_overrides(&o.lifecycle);
    }

    pub fn validate(&self) -> Result<(), String> {
        self.knowledge.validate()?;
        self.episodic.validate()?;
        self.procedural.validate()?;
        self.lifecycle.validate()?;
        self.embeddings.validate()?;
        self.gotcha.validate()?;
        self.admission.validate()?;
        self.compaction.validate()?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct MemoryPolicyOverrides {
    pub knowledge: KnowledgePolicyOverrides,
    pub lifecycle: LifecyclePolicyOverrides,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct KnowledgePolicyOverrides {
    pub max_facts: Option<usize>,
    pub max_patterns: Option<usize>,
    pub max_history: Option<usize>,
    pub contradiction_threshold: Option<f32>,
    pub recall_facts_limit: Option<usize>,
    pub rooms_limit: Option<usize>,
    pub timeline_limit: Option<usize>,
    pub relations_limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LifecyclePolicyOverrides {
    pub decay_rate: Option<f32>,
    pub low_confidence_threshold: Option<f32>,
    pub stale_days: Option<i64>,
    pub similarity_threshold: Option<f32>,
    pub forgetting_model: Option<String>,
    pub base_stability_days: Option<f32>,
    pub archetype_aware_decay: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct KnowledgePolicy {
    pub max_facts: usize,
    pub max_patterns: usize,
    pub max_history: usize,
    pub contradiction_threshold: f32,
    /// Maximum number of facts returned by recall operations.
    pub recall_facts_limit: usize,
    /// Maximum number of rooms returned by `ctx_knowledge action=rooms`.
    pub rooms_limit: usize,
    /// Maximum number of timeline entries returned by `ctx_knowledge action=timeline`.
    pub timeline_limit: usize,
    /// Maximum number of relations/edges returned by relations queries/diagrams.
    pub relations_limit: usize,
}

impl Default for KnowledgePolicy {
    fn default() -> Self {
        Self {
            max_facts: 200,
            max_patterns: 50,
            max_history: 100,
            contradiction_threshold: 0.5,
            recall_facts_limit: crate::core::budgets::KNOWLEDGE_RECALL_FACTS_LIMIT,
            rooms_limit: crate::core::budgets::KNOWLEDGE_ROOMS_LIMIT,
            timeline_limit: crate::core::budgets::KNOWLEDGE_TIMELINE_LIMIT,
            relations_limit: 40,
        }
    }
}

impl KnowledgePolicy {
    fn apply_env_overrides(&mut self) {
        if let Ok(v) = std::env::var("LEAN_CTX_KNOWLEDGE_MAX_FACTS")
            && let Ok(n) = v.parse()
        {
            self.max_facts = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_KNOWLEDGE_MAX_PATTERNS")
            && let Ok(n) = v.parse()
        {
            self.max_patterns = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_KNOWLEDGE_MAX_HISTORY")
            && let Ok(n) = v.parse()
        {
            self.max_history = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_KNOWLEDGE_CONTRADICTION_THRESHOLD")
            && let Ok(n) = v.parse()
        {
            self.contradiction_threshold = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_KNOWLEDGE_RECALL_FACTS_LIMIT")
            && let Ok(n) = v.parse()
        {
            self.recall_facts_limit = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_KNOWLEDGE_ROOMS_LIMIT")
            && let Ok(n) = v.parse()
        {
            self.rooms_limit = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_KNOWLEDGE_TIMELINE_LIMIT")
            && let Ok(n) = v.parse()
        {
            self.timeline_limit = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_KNOWLEDGE_RELATIONS_LIMIT")
            && let Ok(n) = v.parse()
        {
            self.relations_limit = n;
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.max_facts == 0 {
            return Err("memory.knowledge.max_facts must be > 0".to_string());
        }
        if self.max_patterns == 0 {
            return Err("memory.knowledge.max_patterns must be > 0".to_string());
        }
        if self.max_history == 0 {
            return Err("memory.knowledge.max_history must be > 0".to_string());
        }
        if !(0.0..=1.0).contains(&self.contradiction_threshold) {
            return Err(
                "memory.knowledge.contradiction_threshold must be in [0.0, 1.0]".to_string(),
            );
        }
        if self.recall_facts_limit == 0 {
            return Err("memory.knowledge.recall_facts_limit must be > 0".to_string());
        }
        if self.rooms_limit == 0 {
            return Err("memory.knowledge.rooms_limit must be > 0".to_string());
        }
        if self.timeline_limit == 0 {
            return Err("memory.knowledge.timeline_limit must be > 0".to_string());
        }
        if self.relations_limit == 0 {
            return Err("memory.knowledge.relations_limit must be > 0".to_string());
        }
        Ok(())
    }

    fn apply_overrides(&mut self, o: &KnowledgePolicyOverrides) {
        if let Some(v) = o.max_facts {
            self.max_facts = v;
        }
        if let Some(v) = o.max_patterns {
            self.max_patterns = v;
        }
        if let Some(v) = o.max_history {
            self.max_history = v;
        }
        if let Some(v) = o.contradiction_threshold {
            self.contradiction_threshold = v;
        }
        if let Some(v) = o.recall_facts_limit {
            self.recall_facts_limit = v;
        }
        if let Some(v) = o.rooms_limit {
            self.rooms_limit = v;
        }
        if let Some(v) = o.timeline_limit {
            self.timeline_limit = v;
        }
        if let Some(v) = o.relations_limit {
            self.relations_limit = v;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EpisodicPolicy {
    pub max_episodes: usize,
    pub max_actions_per_episode: usize,
    pub summary_max_chars: usize,
}

impl Default for EpisodicPolicy {
    fn default() -> Self {
        Self {
            max_episodes: 500,
            max_actions_per_episode: 50,
            summary_max_chars: 200,
        }
    }
}

impl EpisodicPolicy {
    fn apply_env_overrides(&mut self) {
        if let Ok(v) = std::env::var("LEAN_CTX_EPISODIC_MAX_EPISODES")
            && let Ok(n) = v.parse()
        {
            self.max_episodes = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_EPISODIC_MAX_ACTIONS_PER_EPISODE")
            && let Ok(n) = v.parse()
        {
            self.max_actions_per_episode = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_EPISODIC_SUMMARY_MAX_CHARS")
            && let Ok(n) = v.parse()
        {
            self.summary_max_chars = n;
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.max_episodes == 0 {
            return Err("memory.episodic.max_episodes must be > 0".to_string());
        }
        if self.max_actions_per_episode == 0 {
            return Err("memory.episodic.max_actions_per_episode must be > 0".to_string());
        }
        if self.summary_max_chars < 40 {
            return Err("memory.episodic.summary_max_chars must be >= 40".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProceduralPolicy {
    pub min_repetitions: usize,
    pub min_sequence_len: usize,
    pub max_procedures: usize,
    pub max_window_size: usize,
}

impl Default for ProceduralPolicy {
    fn default() -> Self {
        Self {
            min_repetitions: 3,
            min_sequence_len: 2,
            max_procedures: 100,
            max_window_size: 10,
        }
    }
}

impl ProceduralPolicy {
    fn apply_env_overrides(&mut self) {
        if let Ok(v) = std::env::var("LEAN_CTX_PROCEDURAL_MIN_REPETITIONS")
            && let Ok(n) = v.parse()
        {
            self.min_repetitions = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_PROCEDURAL_MIN_SEQUENCE_LEN")
            && let Ok(n) = v.parse()
        {
            self.min_sequence_len = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_PROCEDURAL_MAX_PROCEDURES")
            && let Ok(n) = v.parse()
        {
            self.max_procedures = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_PROCEDURAL_MAX_WINDOW_SIZE")
            && let Ok(n) = v.parse()
        {
            self.max_window_size = n;
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.min_repetitions == 0 {
            return Err("memory.procedural.min_repetitions must be > 0".to_string());
        }
        if self.min_sequence_len < 2 {
            return Err("memory.procedural.min_sequence_len must be >= 2".to_string());
        }
        if self.max_procedures == 0 {
            return Err("memory.procedural.max_procedures must be > 0".to_string());
        }
        if self.max_window_size < self.min_sequence_len {
            return Err(
                "memory.procedural.max_window_size must be >= min_sequence_len".to_string(),
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LifecyclePolicy {
    pub decay_rate: f32,
    pub low_confidence_threshold: f32,
    pub stale_days: i64,
    pub similarity_threshold: f32,
    /// Forgetting curve (#1): `ebbinghaus` (default) or `linear` (legacy).
    pub forgetting_model: String,
    /// Characteristic memory stability in days for the Ebbinghaus curve.
    pub base_stability_days: f32,
    /// Scale Ebbinghaus stability by fact archetype so structural evidence decays
    /// slower than inference. Default false keeps the baseline tuning unchanged.
    pub archetype_aware_decay: bool,
    /// Archive single-confirmation facts untouched for this many days that were
    /// never retrieved — dead weight regardless of confidence (#962). Defaults to
    /// a conservative 90 days (#972): genuinely cold facts are archived (and
    /// rehydrate on recall), so a store self-curates instead of only churning at
    /// its cap. Set `None`/`off` to disable.
    pub prune_unretrieved_after_days: Option<i64>,
    /// Proactive headroom on a capacity reclaim (#995): when a store reaches its
    /// cap, settle it at `1 - reclaim_headroom_pct` (e.g. `0.25` → 75%) instead
    /// of churning right at the cap. Lossless — the reclaimed tail is archived
    /// and restorable.
    pub reclaim_headroom_pct: f32,
    /// Master switch for the proactive reclaim (#995). `false` is the documented
    /// escape hatch: trim only the overflow, no headroom. Eviction stays lossless
    /// either way.
    pub reclaim_enabled: bool,
}

impl Default for LifecyclePolicy {
    fn default() -> Self {
        Self {
            decay_rate: 0.01,
            low_confidence_threshold: 0.3,
            stale_days: 30,
            similarity_threshold: 0.85,
            forgetting_model: "ebbinghaus".to_string(),
            base_stability_days: crate::core::memory_lifecycle::DEFAULT_BASE_STABILITY_DAYS,
            archetype_aware_decay: false,
            prune_unretrieved_after_days: Some(90),
            reclaim_headroom_pct: crate::core::memory_lifecycle::DEFAULT_RECLAIM_HEADROOM_PCT,
            reclaim_enabled: true,
        }
    }
}

impl LifecyclePolicy {
    fn apply_env_overrides(&mut self) {
        if let Ok(v) = std::env::var("LEAN_CTX_LIFECYCLE_DECAY_RATE")
            && let Ok(n) = v.parse()
        {
            self.decay_rate = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_LIFECYCLE_LOW_CONFIDENCE_THRESHOLD")
            && let Ok(n) = v.parse()
        {
            self.low_confidence_threshold = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_LIFECYCLE_STALE_DAYS")
            && let Ok(n) = v.parse()
        {
            self.stale_days = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_LIFECYCLE_SIMILARITY_THRESHOLD")
            && let Ok(n) = v.parse()
        {
            self.similarity_threshold = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_LIFECYCLE_FORGETTING") {
            self.forgetting_model = v;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_LIFECYCLE_BASE_STABILITY_DAYS")
            && let Ok(n) = v.parse()
        {
            self.base_stability_days = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_LIFECYCLE_ARCHETYPE_AWARE") {
            self.archetype_aware_decay = v == "1" || v.eq_ignore_ascii_case("true");
        }
        if let Ok(v) = std::env::var("LEAN_CTX_LIFECYCLE_PRUNE_UNRETRIEVED_DAYS") {
            self.prune_unretrieved_after_days = match v.trim().to_lowercase().as_str() {
                "" | "off" | "none" | "0" => None,
                s => s.parse::<i64>().ok().filter(|&n| n > 0),
            };
        }
        if let Ok(v) = std::env::var("LEAN_CTX_LIFECYCLE_RECLAIM_HEADROOM_PCT")
            && let Ok(n) = v.parse()
        {
            self.reclaim_headroom_pct = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_LIFECYCLE_RECLAIM_ENABLED") {
            self.reclaim_enabled = !(v == "0" || v.eq_ignore_ascii_case("false"));
        }
    }

    fn validate(&self) -> Result<(), String> {
        if !(0.0..=1.0).contains(&self.decay_rate) {
            return Err("memory.lifecycle.decay_rate must be in [0.0, 1.0]".to_string());
        }
        if !(0.0..=1.0).contains(&self.low_confidence_threshold) {
            return Err(
                "memory.lifecycle.low_confidence_threshold must be in [0.0, 1.0]".to_string(),
            );
        }
        if self.stale_days < 0 {
            return Err("memory.lifecycle.stale_days must be >= 0".to_string());
        }
        if !(0.0..=1.0).contains(&self.similarity_threshold) {
            return Err("memory.lifecycle.similarity_threshold must be in [0.0, 1.0]".to_string());
        }
        if self.base_stability_days <= 0.0 {
            return Err("memory.lifecycle.base_stability_days must be > 0".to_string());
        }
        if !(0.0..=0.95).contains(&self.reclaim_headroom_pct) {
            return Err("memory.lifecycle.reclaim_headroom_pct must be in [0.0, 0.95]".to_string());
        }
        Ok(())
    }

    fn apply_overrides(&mut self, o: &LifecyclePolicyOverrides) {
        if let Some(v) = o.decay_rate {
            self.decay_rate = v;
        }
        if let Some(v) = o.low_confidence_threshold {
            self.low_confidence_threshold = v;
        }
        if let Some(v) = o.stale_days {
            self.stale_days = v;
        }
        if let Some(v) = o.similarity_threshold {
            self.similarity_threshold = v;
        }
        if let Some(ref v) = o.forgetting_model {
            self.forgetting_model.clone_from(v);
        }
        if let Some(v) = o.base_stability_days {
            self.base_stability_days = v;
        }
        if let Some(v) = o.archetype_aware_decay {
            self.archetype_aware_decay = v;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EmbeddingsPolicy {
    pub max_facts: usize,
}

impl Default for EmbeddingsPolicy {
    fn default() -> Self {
        Self { max_facts: 2000 }
    }
}

impl EmbeddingsPolicy {
    fn apply_env_overrides(&mut self) {
        if let Ok(v) = std::env::var("LEAN_CTX_KNOWLEDGE_EMBEDDINGS_MAX_FACTS")
            && let Ok(n) = v.parse()
        {
            self.max_facts = n;
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.max_facts == 0 {
            return Err("memory.embeddings.max_facts must be > 0".to_string());
        }
        Ok(())
    }
}

use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GotchaPolicy {
    pub max_gotchas_per_project: usize,
    pub retrieval_budget_per_room: usize,
    pub default_decay_rate: f32,
    pub category_decay_overrides: HashMap<String, f32>,
    pub auto_expire_days: Option<i64>,
}

impl Default for GotchaPolicy {
    fn default() -> Self {
        Self {
            max_gotchas_per_project: 100,
            retrieval_budget_per_room: 10,
            default_decay_rate: 0.03,
            category_decay_overrides: HashMap::new(),
            auto_expire_days: None,
        }
    }
}

impl GotchaPolicy {
    fn apply_env_overrides(&mut self) {
        if let Ok(v) = std::env::var("LEAN_CTX_GOTCHA_MAX_PER_PROJECT")
            && let Ok(n) = v.parse()
        {
            self.max_gotchas_per_project = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_GOTCHA_RETRIEVAL_BUDGET")
            && let Ok(n) = v.parse()
        {
            self.retrieval_budget_per_room = n;
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.max_gotchas_per_project == 0 {
            return Err("memory.gotcha.max_gotchas_per_project must be > 0".to_string());
        }
        if self.retrieval_budget_per_room == 0 {
            return Err("memory.gotcha.retrieval_budget_per_room must be > 0".to_string());
        }
        if !(0.0..=1.0).contains(&self.default_decay_rate) {
            return Err("memory.gotcha.default_decay_rate must be 0.0-1.0".to_string());
        }
        Ok(())
    }

    pub fn effective_decay_rate(&self, category: &str) -> f32 {
        self.category_decay_overrides
            .get(category)
            .copied()
            .unwrap_or(self.default_decay_rate)
    }
}

/// Write-time admission control for the knowledge store (#970). The cap +
/// importance-eviction is a backstop *after* the fact is written; admission is
/// the boundary *before* it, so a capped store fills with signal, not noise.
/// Applied only to direct `ctx_knowledge remember` (the agent-facing path);
/// internal restorers (archive rehydrate, cognition auto-promotion) bypass it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AdmissionPolicy {
    /// Master switch for write-time admission. When off, every `remember`
    /// inserts as before (legacy behavior).
    pub enabled: bool,
    /// A new fact whose value is at least this similar (word-Jaccard, 0.0–1.0) to
    /// an existing *current* fact in the **same category** under a different key
    /// is merged into it (a confirmation bump) instead of inserted as a new row.
    /// High by default so only genuine near-duplicates collapse; `0.0` disables
    /// auto-merge.
    pub auto_merge_similarity: f32,
    /// Facts whose content salience ([`crate::core::memory_salience::text_salience`])
    /// is below this floor are not admitted as normal facts. `0` (default)
    /// disables the floor — the lossless choice; raise it to curate a noisy store.
    pub min_salience: u32,
}

impl Default for AdmissionPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_merge_similarity: 0.9,
            min_salience: 0,
        }
    }
}

impl AdmissionPolicy {
    fn apply_env_overrides(&mut self) {
        if let Ok(v) = std::env::var("LEAN_CTX_ADMISSION_ENABLED") {
            self.enabled = !(v == "0" || v.eq_ignore_ascii_case("false"));
        }
        if let Ok(v) = std::env::var("LEAN_CTX_ADMISSION_MERGE_SIMILARITY")
            && let Ok(n) = v.parse()
        {
            self.auto_merge_similarity = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_ADMISSION_MIN_SALIENCE")
            && let Ok(n) = v.parse()
        {
            self.min_salience = n;
        }
    }

    fn validate(&self) -> Result<(), String> {
        if !(0.0..=1.0).contains(&self.auto_merge_similarity) {
            return Err("memory.admission.auto_merge_similarity must be in [0.0, 1.0]".to_string());
        }
        Ok(())
    }
}

/// Cluster compaction (#971): the background cognition loop collapses piles of
/// low-value, mutually-similar facts into a single recoverable digest, so a busy
/// store's live fact count actually *drops* instead of churning at its cap. It is
/// strictly guarded — only faded (`< max_confidence`), barely-confirmed
/// (`<= max_confirmations`), never-frequently/recently-retrieved facts in a
/// cluster of at least `min_cluster` qualify — and lossless, since the originals
/// are archived and rehydrate on recall.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactionPolicy {
    /// Master switch for cluster compaction in the cognition loop.
    pub enabled: bool,
    /// Minimum number of facts in a same-category cluster before it is collapsed
    /// into a digest. Must be `>= 2`.
    pub min_cluster: usize,
    /// Average word-Jaccard similarity (0.0–1.0) a fact needs to join a cluster.
    pub similarity: f32,
    /// Importance ceiling: only facts *below* this confidence are eligible, so a
    /// high-confidence fact is never compacted.
    pub max_confidence: f32,
    /// Only facts confirmed at most this many times are eligible — a
    /// repeatedly-confirmed fact is structurally important and always kept.
    pub max_confirmations: u32,
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            min_cluster: 4,
            similarity: 0.5,
            max_confidence: 0.5,
            max_confirmations: 1,
        }
    }
}

impl CompactionPolicy {
    fn apply_env_overrides(&mut self) {
        if let Ok(v) = std::env::var("LEAN_CTX_COMPACTION_ENABLED") {
            self.enabled = !(v == "0" || v.eq_ignore_ascii_case("false"));
        }
        if let Ok(v) = std::env::var("LEAN_CTX_COMPACTION_MIN_CLUSTER")
            && let Ok(n) = v.parse()
        {
            self.min_cluster = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_COMPACTION_SIMILARITY")
            && let Ok(n) = v.parse()
        {
            self.similarity = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_COMPACTION_MAX_CONFIDENCE")
            && let Ok(n) = v.parse()
        {
            self.max_confidence = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_COMPACTION_MAX_CONFIRMATIONS")
            && let Ok(n) = v.parse()
        {
            self.max_confirmations = n;
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.min_cluster < 2 {
            return Err("memory.compaction.min_cluster must be >= 2".to_string());
        }
        if !(0.0..=1.0).contains(&self.similarity) {
            return Err("memory.compaction.similarity must be in [0.0, 1.0]".to_string());
        }
        if !(0.0..=1.0).contains(&self.max_confidence) {
            return Err("memory.compaction.max_confidence must be in [0.0, 1.0]".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn restore_env(key: &str, prev: Option<String>) {
        match prev {
            Some(v) => crate::test_env::set_var(key, v),
            None => crate::test_env::remove_var(key),
        }
    }

    #[test]
    fn default_policy_is_valid() {
        let p = MemoryPolicy::default();
        p.validate().expect("default policy must be valid");
    }

    #[test]
    fn memory_discipline_defaults_are_premium() {
        // #970/#971/#972: self-curation is on by default, but lossless.
        let p = MemoryPolicy::default();
        assert!(p.admission.enabled, "admission on by default");
        assert_eq!(p.admission.min_salience, 0, "salience floor off (lossless)");
        assert!(p.compaction.enabled, "cluster compaction on by default");
        assert!(p.compaction.min_cluster >= 2);
        assert_eq!(
            p.lifecycle.prune_unretrieved_after_days,
            Some(90),
            "conservative recoverable prune default"
        );
    }

    #[test]
    fn admission_and_compaction_env_overrides_apply() {
        let _lock = crate::core::data_dir::test_env_lock();

        let prev = [
            (
                "LEAN_CTX_ADMISSION_ENABLED",
                std::env::var("LEAN_CTX_ADMISSION_ENABLED").ok(),
            ),
            (
                "LEAN_CTX_ADMISSION_MIN_SALIENCE",
                std::env::var("LEAN_CTX_ADMISSION_MIN_SALIENCE").ok(),
            ),
            (
                "LEAN_CTX_COMPACTION_MIN_CLUSTER",
                std::env::var("LEAN_CTX_COMPACTION_MIN_CLUSTER").ok(),
            ),
        ];
        crate::test_env::set_var("LEAN_CTX_ADMISSION_ENABLED", "0");
        crate::test_env::set_var("LEAN_CTX_ADMISSION_MIN_SALIENCE", "42");
        crate::test_env::set_var("LEAN_CTX_COMPACTION_MIN_CLUSTER", "7");

        let mut p = MemoryPolicy::default();
        p.apply_env_overrides();

        assert!(!p.admission.enabled);
        assert_eq!(p.admission.min_salience, 42);
        assert_eq!(p.compaction.min_cluster, 7);

        for (key, val) in prev {
            restore_env(key, val);
        }
    }

    #[test]
    fn validate_rejects_invalid_compaction() {
        let mut p = MemoryPolicy::default();
        p.compaction.min_cluster = 1;
        assert!(p.validate().is_err());

        let mut p = MemoryPolicy::default();
        p.admission.auto_merge_similarity = 1.5;
        assert!(p.validate().is_err());
    }

    #[test]
    fn env_overrides_apply() {
        let _lock = crate::core::data_dir::test_env_lock();

        let prev_facts = std::env::var("LEAN_CTX_KNOWLEDGE_MAX_FACTS").ok();
        let prev_stale = std::env::var("LEAN_CTX_LIFECYCLE_STALE_DAYS").ok();
        let prev_rep = std::env::var("LEAN_CTX_PROCEDURAL_MIN_REPETITIONS").ok();

        crate::test_env::set_var("LEAN_CTX_KNOWLEDGE_MAX_FACTS", "123");
        crate::test_env::set_var("LEAN_CTX_LIFECYCLE_STALE_DAYS", "7");
        crate::test_env::set_var("LEAN_CTX_PROCEDURAL_MIN_REPETITIONS", "4");

        let mut p = MemoryPolicy::default();
        p.apply_env_overrides();

        assert_eq!(p.knowledge.max_facts, 123);
        assert_eq!(p.lifecycle.stale_days, 7);
        assert_eq!(p.procedural.min_repetitions, 4);

        restore_env("LEAN_CTX_KNOWLEDGE_MAX_FACTS", prev_facts);
        restore_env("LEAN_CTX_LIFECYCLE_STALE_DAYS", prev_stale);
        restore_env("LEAN_CTX_PROCEDURAL_MIN_REPETITIONS", prev_rep);
    }

    #[test]
    fn validate_rejects_invalid_values() {
        let mut p = MemoryPolicy::default();
        p.knowledge.max_facts = 0;
        assert!(p.validate().is_err());

        let mut p = MemoryPolicy::default();
        p.lifecycle.decay_rate = 2.0;
        assert!(p.validate().is_err());

        let mut p = MemoryPolicy::default();
        p.procedural.min_sequence_len = 1;
        assert!(p.validate().is_err());
    }
}
