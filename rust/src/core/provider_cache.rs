//! Provider caching awareness — helps LLM providers cache repeated context.
//!
//! Many LLM providers (Anthropic, `OpenAI`, Google) implement prefix caching:
//! if the beginning of a prompt matches a previous request, the provider
//! can skip re-processing those tokens. This module helps lean-ctx structure
//! output to maximize prefix cache hits.
//!
//! Strategies:
//! 1. **Stable prefix ordering**: Static context (project structure, types)
//!    placed BEFORE dynamic context (current file, recent changes)
//! 2. **Hash-based change detection**: Only re-emit context sections that changed
//! 3. **Cacheable block markers**: Mark stable blocks so the LLM host knows
//!    they can be cached aggressively

use std::collections::HashMap;

use md5::{Digest, Md5};

/// A section of context with caching metadata.
#[derive(Debug, Clone)]
pub struct CacheableSection {
    pub id: String,
    pub content: String,
    pub hash: String,
    pub priority: SectionPriority,
    pub stable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SectionPriority {
    System = 0,
    ProjectStructure = 1,
    TypeDefinitions = 2,
    Dependencies = 3,
    RecentContext = 4,
    CurrentTask = 5,
}

/// Tracks which sections have been sent to the provider.
#[derive(Debug)]
pub struct ProviderCacheState {
    sent_hashes: HashMap<String, String>,
    cache_hits: u64,
    cache_misses: u64,
}

impl ProviderCacheState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            sent_hashes: HashMap::new(),
            cache_hits: 0,
            cache_misses: 0,
        }
    }

    /// Check if a section has changed since last sent.
    #[must_use]
    pub fn needs_update(&self, section: &CacheableSection) -> bool {
        match self.sent_hashes.get(&section.id) {
            Some(prev_hash) => prev_hash != &section.hash,
            None => true,
        }
    }

    /// Mark a section as sent to the provider.
    pub fn mark_sent(&mut self, section: &CacheableSection) {
        self.sent_hashes
            .insert(section.id.clone(), section.hash.clone());
    }

    /// Filter sections to only include those that changed.
    /// Stable sections that haven't changed can be skipped (provider caches them).
    pub fn filter_changed<'a>(
        &mut self,
        sections: &'a [CacheableSection],
    ) -> Vec<&'a CacheableSection> {
        let mut result = Vec::new();
        for section in sections {
            if self.needs_update(section) {
                self.cache_misses += 1;
                result.push(section);
            } else {
                self.cache_hits += 1;
            }
        }
        result
    }

    #[must_use]
    pub fn cache_hit_rate(&self) -> f64 {
        let total = self.cache_hits + self.cache_misses;
        if total == 0 {
            return 0.0;
        }
        self.cache_hits as f64 / total as f64
    }

    pub fn reset(&mut self) {
        self.sent_hashes.clear();
        self.cache_hits = 0;
        self.cache_misses = 0;
    }
}

impl Default for ProviderCacheState {
    fn default() -> Self {
        Self::new()
    }
}

impl CacheableSection {
    #[must_use]
    pub fn new(id: &str, content: String, priority: SectionPriority, stable: bool) -> Self {
        let hash = content_hash(&content);
        Self {
            id: id.to_string(),
            content,
            hash,
            priority,
            stable,
        }
    }
}

/// Order sections for optimal prefix caching.
/// Stable sections first (system, project structure, types),
/// dynamic sections last (recent changes, current task).
#[must_use]
pub fn order_for_caching(mut sections: Vec<CacheableSection>) -> Vec<CacheableSection> {
    sections.sort_by(|a, b| {
        a.stable
            .cmp(&b.stable)
            .reverse()
            .then(a.priority.cmp(&b.priority))
    });
    sections
}

/// Render sections with cache boundary markers.
#[must_use]
pub fn render_with_cache_hints(sections: &[CacheableSection]) -> String {
    let mut output = String::new();
    let mut last_stable = true;

    for section in sections {
        if last_stable && !section.stable {
            output.push_str("\n--- dynamic context ---\n");
        }
        output.push_str(&section.content);
        output.push('\n');
        last_stable = section.stable;
    }

    output
}

fn content_hash(content: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(content.as_bytes());
    crate::core::agent_identity::hex_encode(&hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_hash_deterministic() {
        let s1 = CacheableSection::new("id", "content".into(), SectionPriority::System, true);
        let s2 = CacheableSection::new("id", "content".into(), SectionPriority::System, true);
        assert_eq!(s1.hash, s2.hash);
    }

    #[test]
    fn section_hash_changes_with_content() {
        let s1 = CacheableSection::new("id", "content_v1".into(), SectionPriority::System, true);
        let s2 = CacheableSection::new("id", "content_v2".into(), SectionPriority::System, true);
        assert_ne!(s1.hash, s2.hash);
    }

    #[test]
    fn needs_update_new_section() {
        let state = ProviderCacheState::new();
        let section =
            CacheableSection::new("test", "content".into(), SectionPriority::System, true);
        assert!(state.needs_update(&section));
    }

    #[test]
    fn needs_update_unchanged() {
        let mut state = ProviderCacheState::new();
        let section =
            CacheableSection::new("test", "content".into(), SectionPriority::System, true);
        state.mark_sent(&section);
        assert!(!state.needs_update(&section));
    }

    #[test]
    fn needs_update_changed() {
        let mut state = ProviderCacheState::new();
        let s1 = CacheableSection::new("test", "v1".into(), SectionPriority::System, true);
        state.mark_sent(&s1);
        let s2 = CacheableSection::new("test", "v2".into(), SectionPriority::System, true);
        assert!(state.needs_update(&s2));
    }

    #[test]
    fn filter_changed_tracks_hits() {
        let mut state = ProviderCacheState::new();
        let s1 = CacheableSection::new("a", "stable".into(), SectionPriority::System, true);
        state.mark_sent(&s1);

        let sections = vec![
            s1.clone(),
            CacheableSection::new("b", "new".into(), SectionPriority::CurrentTask, false),
        ];
        let changed = state.filter_changed(&sections);
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].id, "b");
        assert!((state.cache_hit_rate() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn order_stable_first() {
        let sections = vec![
            CacheableSection::new(
                "task",
                "current".into(),
                SectionPriority::CurrentTask,
                false,
            ),
            CacheableSection::new("system", "system".into(), SectionPriority::System, true),
            CacheableSection::new(
                "types",
                "types".into(),
                SectionPriority::TypeDefinitions,
                true,
            ),
        ];
        let ordered = order_for_caching(sections);
        assert!(ordered[0].stable);
        assert!(ordered[1].stable);
        assert!(!ordered[2].stable);
        assert_eq!(ordered[0].id, "system");
        assert_eq!(ordered[1].id, "types");
    }

    #[test]
    fn render_marks_dynamic_boundary() {
        let sections = vec![
            CacheableSection::new("sys", "system prompt".into(), SectionPriority::System, true),
            CacheableSection::new(
                "task",
                "current task".into(),
                SectionPriority::CurrentTask,
                false,
            ),
        ];
        let output = render_with_cache_hints(&sections);
        assert!(output.contains("--- dynamic context ---"));
        assert!(output.contains("system prompt"));
        assert!(output.contains("current task"));
    }
}
