use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::config::LoopDetectionConfig;

const SEARCH_TOOLS: &[&str] = &["ctx_search", "ctx_semantic_search"];

const SEARCH_SHELL_PREFIXES: &[&str] = &["grep ", "rg ", "find ", "fd ", "ag ", "ack "];

const CORRECTION_WINDOW: Duration = Duration::from_mins(2);
const MODE_BOUNCE_WINDOW: Duration = Duration::from_secs(30);
const SHELL_RERUN_WINDOW: Duration = Duration::from_mins(1);
const COLD_START_CALLS: u32 = 3;

/// Classification of why an agent re-requested data it already had.
#[derive(Debug, Clone, PartialEq)]
pub enum CorrectionKind {
    FreshReRead,
    ShellReRun,
    ModeBounce,
}

/// Tracks repeated tool calls within a time window to detect and throttle agent loops.
#[derive(Debug, Clone)]
pub struct LoopDetector {
    call_history: HashMap<String, Vec<Instant>>,
    duplicate_counts: HashMap<String, u32>,
    tool_total_counts: HashMap<String, u32>,
    tool_total_limits: HashMap<String, u32>,
    search_group_history: Vec<Instant>,
    recent_search_patterns: Vec<String>,
    normal_threshold: u32,
    reduced_threshold: u32,
    blocked_threshold: u32,
    window: Duration,
    search_group_limit: u32,
    // Correction-loop tracking (Fix A)
    correction_signals: Vec<(Instant, CorrectionKind)>,
    recent_reads: HashMap<String, (Instant, String)>,
    recent_commands: HashMap<String, Instant>,
    total_calls: u32,
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

impl Default for ThrottleResult {
    fn default() -> Self {
        Self {
            level: ThrottleLevel::Normal,
            call_count: 0,
            message: None,
        }
    }
}

impl Default for LoopDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl LoopDetector {
    /// Creates a loop detector with default thresholds.
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(&LoopDetectionConfig::default())
    }

    /// Creates a loop detector with custom thresholds from config.
    /// Set `blocked_threshold` to 0 to disable blocking entirely (`LeanCTX` philosophy).
    #[must_use]
    pub fn with_config(cfg: &LoopDetectionConfig) -> Self {
        Self {
            call_history: HashMap::new(),
            duplicate_counts: HashMap::new(),
            tool_total_counts: HashMap::new(),
            tool_total_limits: cfg.tool_total_limits.clone(),
            search_group_history: Vec::new(),
            recent_search_patterns: Vec::new(),
            normal_threshold: cfg.normal_threshold.max(1),
            reduced_threshold: cfg.reduced_threshold.max(2),
            blocked_threshold: cfg.blocked_threshold,
            window: Duration::from_secs(cfg.window_secs),
            search_group_limit: if cfg.blocked_threshold == 0 {
                u32::MAX
            } else {
                cfg.search_group_limit.max(3)
            },
            correction_signals: Vec::new(),
            recent_reads: HashMap::new(),
            recent_commands: HashMap::new(),
            total_calls: 0,
        }
    }

    /// Records a tool call and returns the throttle result based on repetition count.
    pub fn record_call(&mut self, tool: &str, args_fingerprint: &str) -> ThrottleResult {
        let now = Instant::now();
        self.prune_window(now);

        // Per-tool total count (regardless of args)
        let total = self.tool_total_counts.entry(tool.to_string()).or_insert(0);
        *total += 1;
        let total_count = *total;

        if let Some(&limit) = self.tool_total_limits.get(tool)
            && total_count > limit
        {
            let msg = if crate::core::protocol::meta_visible() {
                Some(format!(
                    "Warning: {tool} called {total_count}x total (limit: {limit}). \
                         Consider ctx_compress or narrowing scope."
                ))
            } else {
                None
            };
            return ThrottleResult {
                level: ThrottleLevel::Reduced,
                call_count: total_count,
                message: msg,
            };
        }

        let key = format!("{tool}:{args_fingerprint}");
        let entries = self.call_history.entry(key.clone()).or_default();
        entries.push(now);
        let count = entries.len() as u32;
        *self.duplicate_counts.entry(key).or_default() = count;

        if self.blocked_threshold > 0 && count > self.blocked_threshold {
            return ThrottleResult {
                level: ThrottleLevel::Blocked,
                call_count: count,
                message: Some(self.block_message(tool, count)),
            };
        }
        if count > self.reduced_threshold {
            if !crate::core::protocol::meta_visible() {
                return ThrottleResult {
                    level: ThrottleLevel::Reduced,
                    call_count: count,
                    message: None,
                };
            }
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
            if !crate::core::protocol::meta_visible() {
                return ThrottleResult {
                    level: ThrottleLevel::Reduced,
                    call_count: count,
                    message: None,
                };
            }
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

    /// Undo the pre-dispatch count for a call that resulted in an error.
    /// Prevents failed retries from triggering throttling prematurely.
    pub fn record_error_outcome(&mut self, tool: &str, args_fingerprint: &str) {
        let key = format!("{tool}:{args_fingerprint}");
        if let Some(entries) = self.call_history.get_mut(&key) {
            entries.pop();
            let count = entries.len() as u32;
            self.duplicate_counts.insert(key, count);
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

        // blocked_threshold == 0 means blocking is disabled (LeanCTX default)
        if self.blocked_threshold > 0 && similar_count >= self.blocked_threshold {
            return ThrottleResult {
                level: ThrottleLevel::Blocked,
                call_count: similar_count,
                message: Some(self.search_block_message(similar_count)),
            };
        }

        // search_group_limit == u32::MAX when blocking is disabled
        if self.blocked_threshold > 0 && search_count > self.search_group_limit {
            return ThrottleResult {
                level: ThrottleLevel::Blocked,
                call_count: search_count,
                message: Some(self.search_group_block_message(search_count)),
            };
        }

        if similar_count >= self.reduced_threshold {
            if !crate::core::protocol::meta_visible() {
                return ThrottleResult {
                    level: ThrottleLevel::Reduced,
                    call_count: similar_count,
                    message: None,
                };
            }
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
            if !crate::core::protocol::meta_visible() {
                return ThrottleResult {
                    level: ThrottleLevel::Reduced,
                    call_count: search_count,
                    message: None,
                };
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

    /// Returns `true` if the tool name is a known search tool (`ctx_search`, etc.).
    #[must_use]
    pub fn is_search_tool(tool: &str) -> bool {
        SEARCH_TOOLS.contains(&tool)
    }

    /// Returns `true` if the shell command starts with a search tool (grep, rg, find, etc.).
    #[must_use]
    pub fn is_search_shell_command(command: &str) -> bool {
        let cmd = command.trim_start();
        SEARCH_SHELL_PREFIXES.iter().any(|p| cmd.starts_with(p))
    }

    /// Computes a deterministic hash fingerprint of JSON tool arguments.
    #[must_use]
    pub fn fingerprint(args: &serde_json::Value) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let canonical = canonical_json(args);
        let mut hasher = DefaultHasher::new();
        canonical.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    /// Returns duplicate call entries sorted by count (descending), filtered to count > 1.
    #[must_use]
    pub fn stats(&self) -> Vec<(String, u32)> {
        let mut entries: Vec<(String, u32)> = self
            .duplicate_counts
            .iter()
            .filter(|&(_, &count)| count > 1)
            .map(|(k, &v)| (k.clone(), v))
            .collect();
        entries.sort_by_key(|x| std::cmp::Reverse(x.1));
        entries
    }

    /// Records a `ctx_read` call and detects correction signals:
    /// - `fresh=true` re-read of a previously cached file
    /// - Mode bounce: map/signatures followed by full within 30s
    pub fn record_read_for_correction(&mut self, path: &str, mode: &str, fresh: bool) {
        self.total_calls += 1;
        let now = Instant::now();

        if self.total_calls <= COLD_START_CALLS {
            self.recent_reads
                .insert(path.to_string(), (now, mode.to_string()));
            return;
        }

        if fresh
            && let Some((prev_time, _)) = self.recent_reads.get(path)
            && now.duration_since(*prev_time) < CORRECTION_WINDOW
        {
            self.correction_signals
                .push((now, CorrectionKind::FreshReRead));
        }

        if mode == "full"
            && let Some((prev_time, prev_mode)) = self.recent_reads.get(path)
        {
            let is_bounce = (prev_mode == "map" || prev_mode == "signatures")
                && now.duration_since(*prev_time) < MODE_BOUNCE_WINDOW;
            if is_bounce {
                self.correction_signals
                    .push((now, CorrectionKind::ModeBounce));
            }
        }

        self.recent_reads
            .insert(path.to_string(), (now, mode.to_string()));
    }

    /// Records a `ctx_shell` command and detects re-runs of the same command within 60s.
    pub fn record_shell_for_correction(&mut self, command: &str) {
        self.total_calls += 1;
        let now = Instant::now();

        if self.total_calls <= COLD_START_CALLS {
            self.recent_commands.insert(command.to_string(), now);
            return;
        }

        let key = normalize_shell_command(command);
        if let Some(prev_time) = self.recent_commands.get(&key)
            && now.duration_since(*prev_time) < SHELL_RERUN_WINDOW
        {
            self.correction_signals
                .push((now, CorrectionKind::ShellReRun));
        }
        self.recent_commands.insert(key, now);
    }

    /// Returns the number of correction signals in the sliding window.
    #[must_use]
    pub fn correction_count(&self) -> u32 {
        let now = Instant::now();
        self.correction_signals
            .iter()
            .filter(|(t, _)| now.duration_since(*t) < CORRECTION_WINDOW)
            .count() as u32
    }

    /// Returns the correction rate: signals per minute within the window.
    #[must_use]
    pub fn correction_rate(&self) -> f64 {
        let count = self.correction_count();
        if count == 0 {
            return 0.0;
        }
        let window_mins = CORRECTION_WINDOW.as_secs_f64() / 60.0;
        f64::from(count) / window_mins
    }

    /// Prunes expired correction signals and stale read/command entries.
    pub fn prune_corrections(&mut self) {
        let now = Instant::now();
        self.correction_signals
            .retain(|(t, _)| now.duration_since(*t) < CORRECTION_WINDOW);
        self.recent_reads
            .retain(|_, (t, _)| now.duration_since(*t) < CORRECTION_WINDOW);
        self.recent_commands
            .retain(|_, t| now.duration_since(*t) < CORRECTION_WINDOW);
    }

    /// Clears all tracking state (call history, search patterns, counters).
    pub fn reset(&mut self) {
        self.call_history.clear();
        self.duplicate_counts.clear();
        self.search_group_history.clear();
        self.recent_search_patterns.clear();
        self.correction_signals.clear();
        self.recent_reads.clear();
        self.recent_commands.clear();
        self.total_calls = 0;
    }

    fn prune_window(&mut self, now: Instant) {
        for entries in self.call_history.values_mut() {
            entries.retain(|t| now.duration_since(*t) < self.window);
        }
        // Drop keys whose window emptied, plus their orphaned duplicate counts, so the
        // per-fingerprint maps don't grow unbounded over a long session. Behavior-neutral:
        // empty Vecs contribute 0 to record_call's count, and duplicate_counts is only
        // read by stats() (already filtered to count > 1). tool_total_counts is left
        // intact — it is a cumulative per-tool-name guard (bounded by the tool set).
        self.call_history.retain(|_, v| !v.is_empty());
        let live = &self.call_history;
        self.duplicate_counts.retain(|k, _| live.contains_key(k));
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

fn normalize_shell_command(cmd: &str) -> String {
    cmd.split_whitespace()
        .take(5)
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
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
            tool_total_limits: std::collections::HashMap::new(),
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
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var("LEAN_CTX_META", "1");
        let cfg = LoopDetectionConfig::default();
        let mut detector = LoopDetector::with_config(&cfg);
        for _ in 0..cfg.normal_threshold {
            detector.record_call("ctx_read", "same_fp");
        }
        let result = detector.record_call("ctx_read", "same_fp");
        assert_eq!(result.level, ThrottleLevel::Reduced);
        assert!(result.message.is_some());
        crate::test_env::remove_var("LEAN_CTX_META");
    }

    #[test]
    fn excessive_calls_get_blocked_when_enabled() {
        // Blocking must be explicitly enabled (blocked_threshold > 0)
        let cfg = LoopDetectionConfig {
            blocked_threshold: 6,
            ..Default::default()
        };
        let mut detector = LoopDetector::with_config(&cfg);
        for _ in 0..cfg.blocked_threshold {
            detector.record_call("ctx_shell", "same_fp");
        }
        let result = detector.record_call("ctx_shell", "same_fp");
        assert_eq!(result.level, ThrottleLevel::Blocked);
        assert!(result.message.unwrap().contains("LOOP DETECTED"));
    }

    #[test]
    fn blocking_disabled_by_default() {
        // Default config has blocked_threshold = 0, so blocking never happens
        let cfg = LoopDetectionConfig::default();
        assert_eq!(cfg.blocked_threshold, 0);
        let mut detector = LoopDetector::with_config(&cfg);
        // Even 100 calls should not block when blocking is disabled
        for _ in 0..100 {
            detector.record_call("ctx_shell", "same_fp");
        }
        let result = detector.record_call("ctx_shell", "same_fp");
        // Should be Reduced (warning) but never Blocked
        assert_ne!(result.level, ThrottleLevel::Blocked);
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
    fn search_group_tracking_when_blocking_enabled() {
        // Blocking must be explicitly enabled for search group limits to block
        let cfg = LoopDetectionConfig {
            search_group_limit: 5,
            blocked_threshold: 6, // Enable blocking
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
    fn similar_search_patterns_trigger_block_when_enabled() {
        // Blocking must be explicitly enabled
        let cfg = LoopDetectionConfig {
            blocked_threshold: 6,
            ..Default::default()
        };
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
    fn correction_fresh_reread_detected() {
        let mut detector = LoopDetector::new();
        // First read (cold start period, skipped)
        detector.record_read_for_correction("src/main.rs", "full", false);
        detector.record_read_for_correction("src/lib.rs", "full", false);
        detector.record_read_for_correction("src/util.rs", "full", false);
        // 4th call: past cold start
        detector.record_read_for_correction("src/main.rs", "full", false);
        assert_eq!(detector.correction_count(), 0);
        // fresh=true re-read of previously read file = correction signal
        detector.record_read_for_correction("src/main.rs", "full", true);
        assert_eq!(detector.correction_count(), 1);
    }

    #[test]
    fn correction_mode_bounce_detected() {
        let mut detector = LoopDetector::new();
        // Cold start
        for i in 0..COLD_START_CALLS {
            detector.record_read_for_correction(&format!("f{i}.rs"), "full", false);
        }
        // Read with map mode
        detector.record_read_for_correction("src/cache.rs", "map", false);
        assert_eq!(detector.correction_count(), 0);
        // Immediately bounce to full mode = correction
        detector.record_read_for_correction("src/cache.rs", "full", false);
        assert_eq!(detector.correction_count(), 1);
    }

    #[test]
    fn correction_shell_rerun_detected() {
        let mut detector = LoopDetector::new();
        // Cold start
        for i in 0..COLD_START_CALLS {
            detector.record_shell_for_correction(&format!("echo {i}"));
        }
        // First run
        detector.record_shell_for_correction("cargo test --lib");
        assert_eq!(detector.correction_count(), 0);
        // Same command again within 60s = correction
        detector.record_shell_for_correction("cargo test --lib");
        assert_eq!(detector.correction_count(), 1);
    }

    #[test]
    fn correction_rate_calculation() {
        let mut detector = LoopDetector::new();
        for i in 0..COLD_START_CALLS {
            detector.record_shell_for_correction(&format!("init{i}"));
        }
        detector.record_shell_for_correction("cargo check");
        detector.record_shell_for_correction("cargo check");
        detector.record_shell_for_correction("cargo check");
        // 2 corrections (first run doesn't count)
        assert_eq!(detector.correction_count(), 2);
        assert!(detector.correction_rate() > 0.0);
    }

    #[test]
    fn correction_cold_start_ignored() {
        let mut detector = LoopDetector::new();
        // During cold start, same-command re-runs are not counted
        detector.record_shell_for_correction("cargo check");
        detector.record_shell_for_correction("cargo check");
        detector.record_shell_for_correction("cargo check");
        assert_eq!(detector.correction_count(), 0);
    }

    #[test]
    fn search_block_message_has_guidance_when_blocking_enabled() {
        // Blocking must be explicitly enabled to get block messages
        let cfg = LoopDetectionConfig {
            blocked_threshold: 6,
            search_group_limit: 8,
            ..Default::default()
        };
        let mut detector = LoopDetector::with_config(&cfg);
        for i in 0..10 {
            detector.record_search("ctx_search", &format!("fp_{i}"), Some("compress"));
        }
        let r = detector.record_search("ctx_search", "fp_new", Some("compress"));
        assert_eq!(r.level, ThrottleLevel::Blocked);
        let msg = r.message.unwrap();
        assert!(msg.contains("ctx_tree"));
        assert!(msg.contains("path"));
        assert!(msg.contains("ctx_read"));
    }

    #[test]
    fn error_outcome_undoes_pre_dispatch_count() {
        let cfg = test_config(2, 4, 0);
        let mut detector = LoopDetector::with_config(&cfg);

        detector.record_call("ctx_read", "fp1");
        detector.record_call("ctx_read", "fp1");
        detector.record_error_outcome("ctx_read", "fp1");

        let r = detector.record_call("ctx_read", "fp1");
        assert_eq!(r.call_count, 2, "error should have undone one count");
        assert_eq!(r.level, ThrottleLevel::Normal);
    }

    #[test]
    fn repeated_errors_dont_trigger_reduced() {
        let cfg = test_config(2, 4, 0);
        let mut detector = LoopDetector::with_config(&cfg);

        for _ in 0..5 {
            detector.record_call("ctx_read", "fp1");
            detector.record_error_outcome("ctx_read", "fp1");
        }

        let r = detector.record_call("ctx_read", "fp1");
        assert_eq!(
            r.level,
            ThrottleLevel::Normal,
            "5 failed retries should not throttle"
        );
    }

    #[test]
    fn mixed_success_and_error_correct_count() {
        let cfg = test_config(2, 4, 0);
        let mut detector = LoopDetector::with_config(&cfg);

        detector.record_call("ctx_read", "fp1");
        detector.record_error_outcome("ctx_read", "fp1");
        detector.record_call("ctx_read", "fp1");
        detector.record_error_outcome("ctx_read", "fp1");
        detector.record_call("ctx_read", "fp1");
        // 3 pre-dispatch, 2 error undos -> effective count = 1
        assert_eq!(detector.record_call("ctx_read", "fp1").call_count, 2);
    }

    #[test]
    fn error_outcome_on_nonexistent_key_is_noop() {
        let mut detector = LoopDetector::new();
        detector.record_error_outcome("ctx_read", "never_called");
        let r = detector.record_call("ctx_read", "never_called");
        assert_eq!(r.call_count, 1);
    }

    #[test]
    fn error_outcome_doesnt_go_negative() {
        let mut detector = LoopDetector::new();
        detector.record_call("ctx_read", "fp1");
        detector.record_error_outcome("ctx_read", "fp1");
        detector.record_error_outcome("ctx_read", "fp1");
        let r = detector.record_call("ctx_read", "fp1");
        assert_eq!(r.call_count, 1, "count should never go below 0");
    }

    #[test]
    fn error_in_tool_a_doesnt_affect_tool_b() {
        let cfg = test_config(2, 4, 0);
        let mut detector = LoopDetector::with_config(&cfg);

        for _ in 0..5 {
            detector.record_call("ctx_read", "fp1");
            detector.record_error_outcome("ctx_read", "fp1");
        }

        let r = detector.record_call("ctx_shell", "fp_shell");
        assert_eq!(r.call_count, 1);
        assert_eq!(r.level, ThrottleLevel::Normal);
    }

    #[test]
    fn different_fingerprints_independent_after_errors() {
        let cfg = test_config(2, 4, 0);
        let mut detector = LoopDetector::with_config(&cfg);

        detector.record_call("ctx_read", "fp_a");
        detector.record_error_outcome("ctx_read", "fp_a");

        detector.record_call("ctx_read", "fp_b");
        let r = detector.record_call("ctx_read", "fp_b");
        assert_eq!(r.call_count, 2);

        let r_a = detector.record_call("ctx_read", "fp_a");
        assert_eq!(r_a.call_count, 1, "fp_a count should be reset to 0 then +1");
    }

    #[test]
    fn correction_degrade_recovery_after_prune() {
        let mut detector = LoopDetector::new();
        for i in 0..4u32 {
            detector.record_read_for_correction(&format!("warmup{i}.rs"), "full", false);
        }
        detector.record_read_for_correction("target.rs", "full", false);
        detector.record_read_for_correction("target.rs", "full", true);
        assert!(detector.correction_count() > 0);
        detector.prune_corrections();
        // After prune, count is still > 0 because signal is within window
        // but the mechanism works: once window expires, count drops
        assert!(detector.correction_count() >= 1);
    }

    #[test]
    fn success_after_errors_resets_to_normal() {
        let cfg = test_config(2, 4, 0);
        let mut detector = LoopDetector::with_config(&cfg);

        for _ in 0..3 {
            detector.record_call("ctx_read", "fp1");
            detector.record_error_outcome("ctx_read", "fp1");
        }

        let r = detector.record_call("ctx_read", "fp1");
        assert_eq!(r.level, ThrottleLevel::Normal);
        assert_eq!(r.call_count, 1);
    }

    #[test]
    fn throttle_result_default_is_normal() {
        let r = ThrottleResult::default();
        assert_eq!(r.level, ThrottleLevel::Normal);
        assert_eq!(r.call_count, 0);
        assert!(r.message.is_none());
    }
}
