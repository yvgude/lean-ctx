//! Transcript analysis for compression-behavior feedback loops.
//!
//! Summarizes re-reads, correction signals, and dominant compression levels
//! over a sliding window of tool interactions.

use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};

/// A single entry in an agent's interaction transcript.
#[derive(Debug, Clone)]
pub struct TranscriptEntry {
    /// Tool name (`ctx_read`, `ctx_search`, etc.).
    pub tool: String,
    /// Target path or query.
    pub target: String,
    /// Compression level used.
    pub compression_level: String,
    /// Token count of the response.
    pub response_tokens: usize,
    /// Timestamp.
    pub timestamp: DateTime<Utc>,
}

/// Analysis of a transcript window.
#[derive(Debug, Clone)]
pub struct TranscriptAnalysis {
    /// Number of re-reads (same file read twice within window).
    pub re_read_count: usize,
    /// Number of correction signals (see `signals.rs`).
    pub correction_count: usize,
    /// Average response token count.
    pub avg_response_tokens: f64,
    /// Most common compression level used.
    pub dominant_level: String,
    /// Window size (number of entries analyzed).
    pub window_size: usize,
}

/// Analyze the last `window` transcript entries for behavioral patterns.
pub fn analyze_transcript(entries: &[TranscriptEntry], window: usize) -> TranscriptAnalysis {
    let window = if window == 0 { entries.len() } else { window };
    let window_entries = if entries.len() > window {
        &entries[entries.len() - window..]
    } else {
        entries
    };

    let mut seen_targets = HashSet::new();
    let mut last_level_by_target: HashMap<&str, &str> = HashMap::new();
    let mut level_counts: HashMap<&str, usize> = HashMap::new();
    let mut re_read_count = 0;
    let mut correction_count = 0;
    let mut total_tokens = 0usize;
    let mut dominant_level = "";
    let mut dominant_count = 0usize;

    for entry in window_entries {
        let target = entry.target.as_str();
        let level = entry.compression_level.as_str();
        if !seen_targets.insert(target) {
            re_read_count += 1;
            if last_level_by_target
                .get(target)
                .is_some_and(|last| *last != level)
            {
                correction_count += 1;
            }
        }
        last_level_by_target.insert(target, level);

        total_tokens = total_tokens.saturating_add(entry.response_tokens);
        let count = level_counts.entry(level).or_insert(0);
        *count += 1;
        if *count > dominant_count {
            dominant_count = *count;
            dominant_level = level;
        }
    }

    let window_size = window_entries.len();
    TranscriptAnalysis {
        re_read_count,
        correction_count,
        avg_response_tokens: if window_size == 0 {
            0.0
        } else {
            total_tokens as f64 / window_size as f64
        },
        dominant_level: dominant_level.to_owned(),
        window_size,
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use chrono::TimeZone;

    fn entry(target: &str, level: &str, tokens: usize, seconds: i64) -> TranscriptEntry {
        TranscriptEntry {
            tool: "ctx_read".to_owned(),
            target: target.to_owned(),
            compression_level: level.to_owned(),
            response_tokens: tokens,
            timestamp: Utc.timestamp_opt(seconds, 0).single().expect("valid time"),
        }
    }

    #[test]
    fn empty_transcript_analysis() {
        let analysis = analyze_transcript(&[], 20);
        assert_eq!(analysis.window_size, 0);
        assert_eq!(analysis.re_read_count, 0);
        assert_eq!(analysis.avg_response_tokens, 0.0);
        assert!(
            analysis.dominant_level.is_empty(),
            "{:?}",
            analysis.dominant_level
        );
    }

    #[test]
    fn re_reads_detected() {
        let entries = [entry("a.rs", "lite", 10, 0), entry("a.rs", "max", 20, 1)];
        let analysis = analyze_transcript(&entries, 10);
        assert_eq!(analysis.re_read_count, 1);
        assert_eq!(analysis.correction_count, 1);
        assert_eq!(analysis.avg_response_tokens, 15.0);
    }

    #[test]
    fn dominant_level_is_most_common() {
        let entries = [
            entry("a.rs", "standard", 10, 0),
            entry("b.rs", "lite", 10, 1),
            entry("c.rs", "standard", 10, 2),
        ];
        assert_eq!(analyze_transcript(&entries, 10).dominant_level, "standard");
    }
}
