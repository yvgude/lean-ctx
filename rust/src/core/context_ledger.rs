use std::collections::HashMap;

use serde::{Deserialize, Serialize};

const DEFAULT_CONTEXT_WINDOW: usize = 128_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextLedger {
    pub window_size: usize,
    pub entries: Vec<LedgerEntry>,
    pub total_tokens_sent: usize,
    pub total_tokens_saved: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub path: String,
    pub mode: String,
    pub original_tokens: usize,
    pub sent_tokens: usize,
    pub timestamp: i64,
}

#[derive(Debug, Clone)]
pub struct ContextPressure {
    pub utilization: f64,
    pub remaining_tokens: usize,
    pub entries_count: usize,
    pub recommendation: PressureAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressureAction {
    NoAction,
    SuggestCompression,
    ForceCompression,
    EvictLeastRelevant,
}

impl ContextLedger {
    pub fn new() -> Self {
        Self {
            window_size: DEFAULT_CONTEXT_WINDOW,
            entries: Vec::new(),
            total_tokens_sent: 0,
            total_tokens_saved: 0,
        }
    }

    pub fn with_window_size(size: usize) -> Self {
        Self {
            window_size: size,
            entries: Vec::new(),
            total_tokens_sent: 0,
            total_tokens_saved: 0,
        }
    }

    pub fn record(&mut self, path: &str, mode: &str, original_tokens: usize, sent_tokens: usize) {
        if let Some(existing) = self.entries.iter_mut().find(|e| e.path == path) {
            self.total_tokens_sent -= existing.sent_tokens;
            self.total_tokens_saved -= existing
                .original_tokens
                .saturating_sub(existing.sent_tokens);
            existing.mode = mode.to_string();
            existing.original_tokens = original_tokens;
            existing.sent_tokens = sent_tokens;
            existing.timestamp = chrono::Utc::now().timestamp();
        } else {
            self.entries.push(LedgerEntry {
                path: path.to_string(),
                mode: mode.to_string(),
                original_tokens,
                sent_tokens,
                timestamp: chrono::Utc::now().timestamp(),
            });
        }
        self.total_tokens_sent += sent_tokens;
        self.total_tokens_saved += original_tokens.saturating_sub(sent_tokens);
    }

    pub fn pressure(&self) -> ContextPressure {
        let utilization = self.total_tokens_sent as f64 / self.window_size as f64;
        let remaining = self.window_size.saturating_sub(self.total_tokens_sent);

        let recommendation = if utilization > 0.9 {
            PressureAction::EvictLeastRelevant
        } else if utilization > 0.75 {
            PressureAction::ForceCompression
        } else if utilization > 0.5 {
            PressureAction::SuggestCompression
        } else {
            PressureAction::NoAction
        };

        ContextPressure {
            utilization,
            remaining_tokens: remaining,
            entries_count: self.entries.len(),
            recommendation,
        }
    }

    pub fn compression_ratio(&self) -> f64 {
        let total_original: usize = self.entries.iter().map(|e| e.original_tokens).sum();
        if total_original == 0 {
            return 1.0;
        }
        self.total_tokens_sent as f64 / total_original as f64
    }

    pub fn files_by_token_cost(&self) -> Vec<(String, usize)> {
        let mut costs: Vec<(String, usize)> = self
            .entries
            .iter()
            .map(|e| (e.path.clone(), e.sent_tokens))
            .collect();
        costs.sort_by(|a, b| b.1.cmp(&a.1));
        costs
    }

    pub fn mode_distribution(&self) -> HashMap<String, usize> {
        let mut dist: HashMap<String, usize> = HashMap::new();
        for entry in &self.entries {
            *dist.entry(entry.mode.clone()).or_insert(0) += 1;
        }
        dist
    }

    pub fn eviction_candidates(&self, keep_count: usize) -> Vec<String> {
        if self.entries.len() <= keep_count {
            return Vec::new();
        }
        let mut sorted = self.entries.clone();
        sorted.sort_by_key(|e| e.timestamp);
        sorted
            .iter()
            .take(self.entries.len() - keep_count)
            .map(|e| e.path.clone())
            .collect()
    }

    pub fn remove(&mut self, path: &str) {
        if let Some(idx) = self.entries.iter().position(|e| e.path == path) {
            let entry = &self.entries[idx];
            self.total_tokens_sent -= entry.sent_tokens;
            self.total_tokens_saved -= entry.original_tokens.saturating_sub(entry.sent_tokens);
            self.entries.remove(idx);
        }
    }

    pub fn format_summary(&self) -> String {
        let pressure = self.pressure();
        format!(
            "CTX: {}/{} tokens ({:.0}%), {} files, ratio {:.2}, action: {:?}",
            self.total_tokens_sent,
            self.window_size,
            pressure.utilization * 100.0,
            self.entries.len(),
            self.compression_ratio(),
            pressure.recommendation,
        )
    }
}

impl Default for ContextLedger {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_ledger_is_empty() {
        let ledger = ContextLedger::new();
        assert_eq!(ledger.total_tokens_sent, 0);
        assert_eq!(ledger.entries.len(), 0);
        assert_eq!(ledger.pressure().recommendation, PressureAction::NoAction);
    }

    #[test]
    fn record_tracks_tokens() {
        let mut ledger = ContextLedger::with_window_size(10000);
        ledger.record("src/main.rs", "full", 500, 500);
        ledger.record("src/lib.rs", "signatures", 1000, 200);
        assert_eq!(ledger.total_tokens_sent, 700);
        assert_eq!(ledger.total_tokens_saved, 800);
        assert_eq!(ledger.entries.len(), 2);
    }

    #[test]
    fn record_updates_existing_entry() {
        let mut ledger = ContextLedger::with_window_size(10000);
        ledger.record("src/main.rs", "full", 500, 500);
        ledger.record("src/main.rs", "signatures", 500, 100);
        assert_eq!(ledger.entries.len(), 1);
        assert_eq!(ledger.total_tokens_sent, 100);
        assert_eq!(ledger.total_tokens_saved, 400);
    }

    #[test]
    fn pressure_escalates() {
        let mut ledger = ContextLedger::with_window_size(1000);
        ledger.record("a.rs", "full", 600, 600);
        assert_eq!(
            ledger.pressure().recommendation,
            PressureAction::SuggestCompression
        );
        ledger.record("b.rs", "full", 200, 200);
        assert_eq!(
            ledger.pressure().recommendation,
            PressureAction::ForceCompression
        );
        ledger.record("c.rs", "full", 150, 150);
        assert_eq!(
            ledger.pressure().recommendation,
            PressureAction::EvictLeastRelevant
        );
    }

    #[test]
    fn compression_ratio_accurate() {
        let mut ledger = ContextLedger::with_window_size(10000);
        ledger.record("a.rs", "full", 1000, 1000);
        ledger.record("b.rs", "signatures", 1000, 200);
        let ratio = ledger.compression_ratio();
        assert!((ratio - 0.6).abs() < 0.01);
    }

    #[test]
    fn eviction_returns_oldest() {
        let mut ledger = ContextLedger::with_window_size(10000);
        ledger.record("old.rs", "full", 100, 100);
        std::thread::sleep(std::time::Duration::from_millis(10));
        ledger.record("new.rs", "full", 100, 100);
        let candidates = ledger.eviction_candidates(1);
        assert_eq!(candidates, vec!["old.rs"]);
    }

    #[test]
    fn remove_updates_totals() {
        let mut ledger = ContextLedger::with_window_size(10000);
        ledger.record("a.rs", "full", 500, 500);
        ledger.record("b.rs", "full", 300, 300);
        ledger.remove("a.rs");
        assert_eq!(ledger.total_tokens_sent, 300);
        assert_eq!(ledger.entries.len(), 1);
    }

    #[test]
    fn mode_distribution_counts() {
        let mut ledger = ContextLedger::new();
        ledger.record("a.rs", "full", 100, 100);
        ledger.record("b.rs", "signatures", 100, 50);
        ledger.record("c.rs", "full", 100, 100);
        let dist = ledger.mode_distribution();
        assert_eq!(dist.get("full"), Some(&2));
        assert_eq!(dist.get("signatures"), Some(&1));
    }

    #[test]
    fn format_summary_includes_key_info() {
        let mut ledger = ContextLedger::with_window_size(10000);
        ledger.record("a.rs", "full", 500, 500);
        let summary = ledger.format_summary();
        assert!(summary.contains("500/10000"));
        assert!(summary.contains("1 files"));
    }
}
