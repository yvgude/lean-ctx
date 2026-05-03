use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct MemoryPolicy {
    pub knowledge: KnowledgePolicy,
    pub episodic: EpisodicPolicy,
    pub procedural: ProceduralPolicy,
    pub lifecycle: LifecyclePolicy,
    pub embeddings: EmbeddingsPolicy,
}

impl MemoryPolicy {
    pub fn apply_env_overrides(&mut self) {
        self.knowledge.apply_env_overrides();
        self.episodic.apply_env_overrides();
        self.procedural.apply_env_overrides();
        self.lifecycle.apply_env_overrides();
        self.embeddings.apply_env_overrides();
    }

    pub fn validate(&self) -> Result<(), String> {
        self.knowledge.validate()?;
        self.episodic.validate()?;
        self.procedural.validate()?;
        self.lifecycle.validate()?;
        self.embeddings.validate()?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct KnowledgePolicy {
    pub max_facts: usize,
    pub max_patterns: usize,
    pub max_history: usize,
    pub contradiction_threshold: f32,
}

impl Default for KnowledgePolicy {
    fn default() -> Self {
        Self {
            max_facts: 200,
            max_patterns: 50,
            max_history: 100,
            contradiction_threshold: 0.5,
        }
    }
}

impl KnowledgePolicy {
    fn apply_env_overrides(&mut self) {
        if let Ok(v) = std::env::var("LEAN_CTX_KNOWLEDGE_MAX_FACTS") {
            if let Ok(n) = v.parse() {
                self.max_facts = n;
            }
        }
        if let Ok(v) = std::env::var("LEAN_CTX_KNOWLEDGE_MAX_PATTERNS") {
            if let Ok(n) = v.parse() {
                self.max_patterns = n;
            }
        }
        if let Ok(v) = std::env::var("LEAN_CTX_KNOWLEDGE_MAX_HISTORY") {
            if let Ok(n) = v.parse() {
                self.max_history = n;
            }
        }
        if let Ok(v) = std::env::var("LEAN_CTX_KNOWLEDGE_CONTRADICTION_THRESHOLD") {
            if let Ok(n) = v.parse() {
                self.contradiction_threshold = n;
            }
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
        Ok(())
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
        if let Ok(v) = std::env::var("LEAN_CTX_EPISODIC_MAX_EPISODES") {
            if let Ok(n) = v.parse() {
                self.max_episodes = n;
            }
        }
        if let Ok(v) = std::env::var("LEAN_CTX_EPISODIC_MAX_ACTIONS_PER_EPISODE") {
            if let Ok(n) = v.parse() {
                self.max_actions_per_episode = n;
            }
        }
        if let Ok(v) = std::env::var("LEAN_CTX_EPISODIC_SUMMARY_MAX_CHARS") {
            if let Ok(n) = v.parse() {
                self.summary_max_chars = n;
            }
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
        if let Ok(v) = std::env::var("LEAN_CTX_PROCEDURAL_MIN_REPETITIONS") {
            if let Ok(n) = v.parse() {
                self.min_repetitions = n;
            }
        }
        if let Ok(v) = std::env::var("LEAN_CTX_PROCEDURAL_MIN_SEQUENCE_LEN") {
            if let Ok(n) = v.parse() {
                self.min_sequence_len = n;
            }
        }
        if let Ok(v) = std::env::var("LEAN_CTX_PROCEDURAL_MAX_PROCEDURES") {
            if let Ok(n) = v.parse() {
                self.max_procedures = n;
            }
        }
        if let Ok(v) = std::env::var("LEAN_CTX_PROCEDURAL_MAX_WINDOW_SIZE") {
            if let Ok(n) = v.parse() {
                self.max_window_size = n;
            }
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
}

impl Default for LifecyclePolicy {
    fn default() -> Self {
        Self {
            decay_rate: 0.01,
            low_confidence_threshold: 0.3,
            stale_days: 30,
            similarity_threshold: 0.85,
        }
    }
}

impl LifecyclePolicy {
    fn apply_env_overrides(&mut self) {
        if let Ok(v) = std::env::var("LEAN_CTX_LIFECYCLE_DECAY_RATE") {
            if let Ok(n) = v.parse() {
                self.decay_rate = n;
            }
        }
        if let Ok(v) = std::env::var("LEAN_CTX_LIFECYCLE_LOW_CONFIDENCE_THRESHOLD") {
            if let Ok(n) = v.parse() {
                self.low_confidence_threshold = n;
            }
        }
        if let Ok(v) = std::env::var("LEAN_CTX_LIFECYCLE_STALE_DAYS") {
            if let Ok(n) = v.parse() {
                self.stale_days = n;
            }
        }
        if let Ok(v) = std::env::var("LEAN_CTX_LIFECYCLE_SIMILARITY_THRESHOLD") {
            if let Ok(n) = v.parse() {
                self.similarity_threshold = n;
            }
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
        Ok(())
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
        if let Ok(v) = std::env::var("LEAN_CTX_KNOWLEDGE_EMBEDDINGS_MAX_FACTS") {
            if let Ok(n) = v.parse() {
                self.max_facts = n;
            }
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.max_facts == 0 {
            return Err("memory.embeddings.max_facts must be > 0".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn restore_env(key: &str, prev: Option<String>) {
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn default_policy_is_valid() {
        let p = MemoryPolicy::default();
        p.validate().expect("default policy must be valid");
    }

    #[test]
    fn env_overrides_apply() {
        let _lock = crate::core::data_dir::test_env_lock();

        let prev_facts = std::env::var("LEAN_CTX_KNOWLEDGE_MAX_FACTS").ok();
        let prev_stale = std::env::var("LEAN_CTX_LIFECYCLE_STALE_DAYS").ok();
        let prev_rep = std::env::var("LEAN_CTX_PROCEDURAL_MIN_REPETITIONS").ok();

        std::env::set_var("LEAN_CTX_KNOWLEDGE_MAX_FACTS", "123");
        std::env::set_var("LEAN_CTX_LIFECYCLE_STALE_DAYS", "7");
        std::env::set_var("LEAN_CTX_PROCEDURAL_MIN_REPETITIONS", "4");

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
