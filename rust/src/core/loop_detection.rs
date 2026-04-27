use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::config::LoopDetectionConfig;

const SEARCH_TOOLS: &[&str] = &["ctx_search", "ctx_semantic_search"];

const SEARCH_SHELL_PREFIXES: &[&str] = &["grep ", "rg ", "find ", "fd ", "ag ", "ack "];

/// Tracks repeated tool calls within a time window to detect and throttle agent loops.
#[derive(Debug, Clone)]
pub struct LoopDetector {
    call_history: HashMap<String, Vec<Instant>>,
    duplicate_counts: HashMap<String, u32>,
    search_group_history: Vec<Instant>,
    recent_search_patterns: Vec<String>,
    normal_threshold: u32,
    reduced_threshold: u32,
    blocked_threshold: u32,
    window: Duration,
    search_group_limit: u32,
}

/// Severity of throttling applied to a repeated call: normal, reduced, or blocked.
#[derive(Debug, Clone, PartialEq)]
pub enum ThrottleLevel {
    Normal,
    Reduced,
    Blocked,
}

/// Outcome of a loop detection check: throttle level, count, and optional warning.
#[derive(Debug, Clone)]
pub struct ThrottleResult {
    pub level: ThrottleLevel,
    pub call_count: u32,
    pub message: Option<String>,
}

impl Default for LoopDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl LoopDetector {
    /// Creates a loop detector with default thresholds.
    pub fn new() -> Self {
        Self::with_config(&LoopDetectionConfig::default())
    }

    /// Creates a loop detector with custom thresholds from config.
    pub fn with_config(cfg: &LoopDetectionConfig) -> Self {
        Self {
            call_history: HashMap::new(),
            duplicate_counts: HashMap::new(),
            search_group_history: Vec::new(),
            recent_search_patterns: Vec::new(),
            normal_threshold: cfg.normal_threshold.max(1),
            reduced_threshold: cfg.reduced_threshold.max(2),
            blocked_threshold: cfg.blocked_threshold.max(3),
            window: Duration::from_secs(cfg.window_secs),
            search_group_limit: cfg.search_group_limit.max(3),
        }
    }

    /// Records a tool call and returns the throttle result based on repetition count.
    pub fn record_call(&mut self, tool: &str, args_fingerprint: &str) -> ThrottleResult {
        let now = Instant::now();
        self.prune_window(now);

        let key = format!("{tool}:{args_fingerprint}");
        let entries = self.call_history.entry(key.clone()).or_default();
        entries.push(now);
        let count = entries.len() as u32;
        *self.duplicate_counts.entry(key).or_default() = count;

        if count > self.blocked_threshold {
            return ThrottleResult {
                level: ThrottleLevel::Blocked,
                call_count: count,
                message: Some(self.block_message(tool, count)),
            };
        }
        if count > self.reduced_threshold {
            return ThrottleResult {
                level: ThrottleLevel::Reduced,
                call_count: count,
                message: Some(format!(
                    "Warning: {tool} called {count}x with same args. \
                     Results reduced. Try a different approach or narrow your scope."
                )),
            };
        }
        if count > self.normal_threshold {
            return ThrottleResult {
                level: ThrottleLevel::Reduced,
                call_count: count,
                message: Some(format!(
                    "Note: {tool} called {count}x with similar args. Consider narrowing scope."
                )),
            };
        }
        ThrottleResult {
            level: ThrottleLevel::Normal,
            call_count: count,
            message: None,
        }
    }

    /// Record a search-category call and check the cross-tool search group limit.
    /// `search_pattern` is the extracted query/regex the agent is looking for (if available).
    pub fn record_search(
        &mut self,
        tool: &str,
        args_fingerprint: &str,
        search_pattern: Option<&str>,
    ) -> ThrottleResult {
        let now = Instant::now();

        self.search_group_history.push(now);
        let search_count = self.search_group_history.len() as u32;

        let similar_count = if let Some(pat) = search_pattern {
            let sc = self.count_similar_patterns(pat);
            if !pat.is_empty() {
                self.recent_search_patterns.push(pat.to_string());
                if self.recent_search_patterns.len() > 15 {
                    self.recent_search_patterns.remove(0);
                }
            }
            sc
        } else {
            0
        };

        if similar_count >= self.blocked_threshold {
            return ThrottleResult {
                level: ThrottleLevel::Blocked,
                call_count: similar_count,
                message: Some(self.search_block_message(similar_count)),
            };
        }

        if search_count > self.search_group_limit {
            return ThrottleResult {
                level: ThrottleLevel::Blocked,
                call_count: search_count,
                message: Some(self.search_group_block_message(search_count)),
            };
        }

        if similar_count >= self.reduced_threshold {
            return ThrottleResult {
                level: ThrottleLevel::Reduced,
                call_count: similar_count,
                message: Some(format!(
                    "Warning: You've searched for similar patterns {similar_count}x. \
                     Narrow your search with the 'path' parameter or try ctx_tree first."
                )),
            };
        }

        if search_count > self.search_group_limit.saturating_sub(3) {
            let per_fp = self.record_call(tool, args_fingerprint);
            if per_fp.level != ThrottleLevel::Normal {
                return per_fp;
            }
            return ThrottleResult {
                level: ThrottleLevel::Reduced,
                call_count: search_count,
                message: Some(format!(
                    "Note: {search_count} search calls in the last {}s. \
                     Use ctx_tree to orient first, then scope searches with 'path'.",
                    self.window.as_secs()
                )),
            };
        }

        self.record_call(tool, args_fingerprint)
    }

    /// Returns `true` if the tool name is a known search tool (ctx_search, etc.).
    pub fn is_search_tool(tool: &str) -> bool {
        SEARCH_TOOLS.contains(&tool)
    }

    /// Returns `true` if the shell command starts with a search tool (grep, rg, find, etc.).
    pub fn is_search_shell_command(command: &str) -> bool {
        let cmd = command.trim_start();
        SEARCH_SHELL_PREFIXES.iter().any(|p| cmd.starts_with(p))
    }

    /// Computes a deterministic hash fingerprint of JSON tool arguments.
    pub fn fingerprint(args: &serde_json::Value) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let canonical = canonical_json(args);
        let mut hasher = DefaultHasher::new();
        canonical.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    /// Returns duplicate call entries sorted by count (descending), filtered to count > 1.
    pub fn stats(&self) -> Vec<(String, u32)> {
        let mut entries: Vec<(String, u32)> = self
            .duplicate_counts
            .iter()
            .filter(|(_, &count)| count > 1)
            .map(|(k, &v)| (k.clone(), v))
            .collect();
        entries.sort_by_key(|x| std::cmp::Reverse(x.1));
        entries
    }

    /// Clears all tracking state (call history, search patterns, counters).
    pub fn reset(&mut self) {
        self.call_history.clear();
        self.duplicate_counts.clear();
        self.search_group_history.clear();
        self.recent_search_patterns.clear();
    }

    fn prune_window(&mut self, now: Instant) {
        for entries in self.call_history.values_mut() {
            entries.retain(|t| now.duration_since(*t) < self.window);
        }
        self.search_group_history
            .retain(|t| now.duration_since(*t) < self.window);
    }

    fn count_similar_patterns(&self, new_pattern: &str) -> u32 {
        let new_lower = new_pattern.to_lowercase();
        let new_root = extract_alpha_root(&new_lower);

        let mut count = 0u32;
        for existing in &self.recent_search_patterns {
            let existing_lower = existing.to_lowercase();
            if patterns_are_similar(&new_lower, &existing_lower) {
                count += 1;
            } else if new_root.len() >= 4 {
                let existing_root = extract_alpha_root(&existing_lower);
                if existing_root.len() >= 4
                    && (new_root.starts_with(&existing_root)
                        || existing_root.starts_with(&new_root))
                {
                    count += 1;
                }
            }
        }
        count
    }

    fn block_message(&self, tool: &str, count: u32) -> String {
        if Self::is_search_tool(tool) {
            self.search_block_message(count)
        } else {
            format!(
                "LOOP DETECTED: {tool} called {count}x with same/similar args. \
                 Call blocked. Change your approach — the current strategy is not working."
            )
        }
    }

    #[allow(clippy::unused_self)]
    fn search_block_message(&self, count: u32) -> String {
        format!(
            "LOOP DETECTED: You've searched {count}x with similar patterns. STOP searching and change strategy. \
             1) Use ctx_tree to understand the project structure first. \
             2) Narrow your search with the 'path' parameter to a specific directory. \
             3) Use ctx_read with mode='map' to understand a file before searching more."
        )
    }

    fn search_group_block_message(&self, count: u32) -> String {
        format!(
            "LOOP DETECTED: {count} search calls in {}s — too many. STOP and rethink. \
             1) Use ctx_tree to map the project structure. \
             2) Pick ONE specific directory and search there with the 'path' parameter. \
             3) Read files with ctx_read mode='map' instead of searching blindly.",
            self.window.as_secs()
        )
    }
}

fn extract_alpha_root(pattern: &str) -> String {
    pattern
        .chars()
        .take_while(|c| c.is_alphanumeric())
        .collect()
}

fn patterns_are_similar(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    if a.contains(b) || b.contains(a) {
        return true;
    }
    let a_alpha: String = a.chars().filter(|c| c.is_alphanumeric()).collect();
    let b_alpha: String = b.chars().filter(|c| c.is_alphanumeric()).collect();
    if a_alpha.len() >= 3
        && b_alpha.len() >= 3
        && (a_alpha.contains(&b_alpha) || b_alpha.contains(&a_alpha))
    {
        return true;
    }
    false
}

fn canonical_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let entries: Vec<String> = keys
                .iter()
                .map(|k| format!("{}:{}", k, canonical_json(&map[*k])))
                .collect();
            format!("{{{}}}", entries.join(","))
        }
        serde_json::Value::Array(arr) => {
            let entries: Vec<String> = arr.iter().map(canonical_json).collect();
            format!("[{}]", entries.join(","))
        }
        _ => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(normal: u32, reduced: u32, blocked: u32) -> LoopDetectionConfig {
        LoopDetectionConfig {
            normal_threshold: normal,
            reduced_threshold: reduced,
            blocked_threshold: blocked,
            window_secs: 300,
            search_group_limit: 10,
        }
    }

    #[test]
    fn normal_calls_pass_through() {
        let mut detector = LoopDetector::new();
        let r1 = detector.record_call("ctx_read", "abc123");
        assert_eq!(r1.level, ThrottleLevel::Normal);
        assert_eq!(r1.call_count, 1);
        assert!(r1.message.is_none());
    }

    #[test]
    fn repeated_calls_trigger_reduced() {
        let cfg = LoopDetectionConfig::default();
        let mut detector = LoopDetector::with_config(&cfg);
        for _ in 0..cfg.normal_threshold {
            detector.record_call("ctx_read", "same_fp");
        }
        let result = detector.record_call("ctx_read", "same_fp");
        assert_eq!(result.level, ThrottleLevel::Reduced);
        assert!(result.message.is_some());
    }

    #[test]
    fn excessive_calls_get_blocked() {
        let cfg = LoopDetectionConfig::default();
        let mut detector = LoopDetector::with_config(&cfg);
        for _ in 0..cfg.blocked_threshold {
            detector.record_call("ctx_shell", "same_fp");
        }
        let result = detector.record_call("ctx_shell", "same_fp");
        assert_eq!(result.level, ThrottleLevel::Blocked);
        assert!(result.message.unwrap().contains("LOOP DETECTED"));
    }

    #[test]
    fn different_args_tracked_separately() {
        let mut detector = LoopDetector::new();
        for _ in 0..10 {
            detector.record_call("ctx_read", "fp_a");
        }
        let result = detector.record_call("ctx_read", "fp_b");
        assert_eq!(result.level, ThrottleLevel::Normal);
        assert_eq!(result.call_count, 1);
    }

    #[test]
    fn fingerprint_deterministic() {
        let args = serde_json::json!({"path": "test.rs", "mode": "full"});
        let fp1 = LoopDetector::fingerprint(&args);
        let fp2 = LoopDetector::fingerprint(&args);
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn fingerprint_order_independent() {
        let a = serde_json::json!({"mode": "full", "path": "test.rs"});
        let b = serde_json::json!({"path": "test.rs", "mode": "full"});
        assert_eq!(LoopDetector::fingerprint(&a), LoopDetector::fingerprint(&b));
    }

    #[test]
    fn stats_shows_duplicates() {
        let mut detector = LoopDetector::new();
        for _ in 0..5 {
            detector.record_call("ctx_read", "fp_a");
        }
        detector.record_call("ctx_shell", "fp_b");
        let stats = detector.stats();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].1, 5);
    }

    #[test]
    fn reset_clears_state() {
        let mut detector = LoopDetector::new();
        for _ in 0..5 {
            detector.record_call("ctx_read", "fp_a");
        }
        detector.reset();
        let result = detector.record_call("ctx_read", "fp_a");
        assert_eq!(result.call_count, 1);
    }

    #[test]
    fn custom_thresholds_from_config() {
        let cfg = test_config(1, 2, 3);
        let mut detector = LoopDetector::with_config(&cfg);
        detector.record_call("ctx_read", "fp");
        let r = detector.record_call("ctx_read", "fp");
        assert_eq!(r.level, ThrottleLevel::Reduced);
        detector.record_call("ctx_read", "fp");
        let r = detector.record_call("ctx_read", "fp");
        assert_eq!(r.level, ThrottleLevel::Blocked);
    }

    #[test]
    fn similar_patterns_detected() {
        assert!(patterns_are_similar("compress", "compress"));
        assert!(patterns_are_similar("compress", "compression"));
        assert!(patterns_are_similar("compress.*data", "compress"));
        assert!(!patterns_are_similar("foo", "bar"));
        assert!(!patterns_are_similar("ab", "cd"));
    }

    #[test]
    fn search_group_tracking() {
        let cfg = LoopDetectionConfig {
            search_group_limit: 5,
            ..Default::default()
        };
        let mut detector = LoopDetector::with_config(&cfg);
        for i in 0..5 {
            let fp = format!("fp_{i}");
            let r = detector.record_search("ctx_search", &fp, Some(&format!("pattern_{i}")));
            assert_ne!(r.level, ThrottleLevel::Blocked, "call {i} should not block");
        }
        let r = detector.record_search("ctx_search", "fp_5", Some("pattern_5"));
        assert_eq!(r.level, ThrottleLevel::Blocked);
        assert!(r.message.unwrap().contains("search calls"));
    }

    #[test]
    fn similar_search_patterns_trigger_block() {
        let cfg = LoopDetectionConfig::default();
        let mut detector = LoopDetector::with_config(&cfg);
        let variants = [
            "compress",
            "compression",
            "compress.*data",
            "compress_output",
            "compressor",
            "compress_result",
            "compress_file",
        ];
        for (i, pat) in variants
            .iter()
            .enumerate()
            .take(cfg.blocked_threshold as usize)
        {
            detector.record_search("ctx_search", &format!("fp_{i}"), Some(pat));
        }
        let r = detector.record_search("ctx_search", "fp_new", Some("compress_all"));
        assert_eq!(r.level, ThrottleLevel::Blocked);
    }

    #[test]
    fn is_search_tool_detection() {
        assert!(LoopDetector::is_search_tool("ctx_search"));
        assert!(LoopDetector::is_search_tool("ctx_semantic_search"));
        assert!(!LoopDetector::is_search_tool("ctx_read"));
        assert!(!LoopDetector::is_search_tool("ctx_shell"));
    }

    #[test]
    fn is_search_shell_command_detection() {
        assert!(LoopDetector::is_search_shell_command("grep -r foo ."));
        assert!(LoopDetector::is_search_shell_command("rg pattern src/"));
        assert!(LoopDetector::is_search_shell_command("find . -name '*.rs'"));
        assert!(!LoopDetector::is_search_shell_command("cargo build"));
        assert!(!LoopDetector::is_search_shell_command("git status"));
    }

    #[test]
    fn search_block_message_has_guidance() {
        let mut detector = LoopDetector::new();
        for i in 0..10 {
            detector.record_search("ctx_search", &format!("fp_{i}"), Some("compress"));
        }
        let r = detector.record_search("ctx_search", "fp_new", Some("compress"));
        let msg = r.message.unwrap();
        assert!(msg.contains("ctx_tree"));
        assert!(msg.contains("path"));
        assert!(msg.contains("ctx_read"));
    }
}
