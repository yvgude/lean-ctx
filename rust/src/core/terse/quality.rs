//! Quality gate for terse compression.
//!
//! Ensures compression does not destroy critical information:
//! - File paths must be preserved
//! - Code identifiers (>= 6 chars) must be preserved
//! - Critical facts (error codes, failing-test counts, failure status, problem
//!   locations) must survive in the *final* output — terse output has no recovery
//!   path, so a dropped critical fact would be lost ([`critical_facts_lost`],
//!   checked by the engine after dictionaries and the auto-dictionary)
//! - Minimum savings threshold (default 10%) must be met

use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct QualityReport {
    pub passed: bool,
    pub savings_pct: f32,
    pub paths_preserved: bool,
    pub identifiers_preserved: bool,
    pub paths_total: usize,
    pub paths_found: usize,
    pub identifiers_total: usize,
    pub identifiers_found: usize,
    /// Critical retention probes missing from the compressed text.
    pub critical_lost: usize,
}

pub struct QualityConfig {
    pub min_savings_pct: f32,
    pub min_path_preservation: f32,
    pub min_identifier_preservation: f32,
    pub min_identifier_len: usize,
}

impl Default for QualityConfig {
    fn default() -> Self {
        Self {
            min_savings_pct: 3.0,
            min_path_preservation: 1.0,
            min_identifier_preservation: 0.90,
            min_identifier_len: 6,
        }
    }
}

/// Checks whether a compression result meets quality thresholds.
pub fn check(
    original: &str,
    compressed: &str,
    tokens_before: u32,
    tokens_after: u32,
    config: &QualityConfig,
) -> QualityReport {
    let savings_pct = if tokens_before > 0 && tokens_before >= tokens_after {
        ((tokens_before - tokens_after) as f32 / tokens_before as f32) * 100.0
    } else {
        0.0
    };

    let orig_paths = extract_paths(original);
    let comp_paths = extract_paths(compressed);
    let paths_found = orig_paths
        .iter()
        .filter(|p| comp_paths.contains(*p))
        .count();
    let paths_preserved = orig_paths.is_empty()
        || (paths_found as f32 / orig_paths.len() as f32) >= config.min_path_preservation;

    let orig_idents = extract_identifiers(original, config.min_identifier_len);
    let comp_words: HashSet<String> = compressed
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| w.len() >= config.min_identifier_len)
        .map(str::to_lowercase)
        .collect();
    let idents_found = orig_idents
        .iter()
        .filter(|id| comp_words.contains(&id.to_lowercase()))
        .count();
    let identifiers_preserved = orig_idents.is_empty()
        || (idents_found as f32 / orig_idents.len() as f32) >= config.min_identifier_preservation;

    let passed = paths_preserved && identifiers_preserved;

    QualityReport {
        passed,
        savings_pct,
        paths_preserved,
        identifiers_preserved,
        paths_total: orig_paths.len(),
        paths_found,
        identifiers_total: orig_idents.len(),
        identifiers_found: idents_found,
        critical_lost: 0,
    }
}

/// Critical facts of `original` missing from the final `delivered` text (after every
/// rewrite). Terse output has no recovery path, so a missing critical fact is lost.
/// Returns at least 1 when the facts could not all be checked (fail closed).
pub fn critical_facts_lost(original: &str, delivered: &str) -> usize {
    if original == delivered {
        return 0;
    }
    let report = crate::core::context_quality::retention::assess(
        original,
        delivered,
        crate::core::context_quality::RecoveryPath::None,
    );
    if report.passes() {
        0
    } else {
        report.critical.lost.max(1)
    }
}

fn extract_paths(text: &str) -> HashSet<String> {
    let mut paths = HashSet::new();
    for word in text.split_whitespace() {
        let cleaned = word.trim_matches(|c: char| c == '\'' || c == '"' || c == ',' || c == ';');
        if looks_like_path(cleaned) {
            paths.insert(cleaned.to_string());
        }
    }
    paths
}

fn looks_like_path(s: &str) -> bool {
    if s.len() < 3 {
        return false;
    }
    let has_separator = s.contains('/') || s.contains('\\');
    let has_extension = s.rfind('.').is_some_and(|dot| {
        let ext = &s[dot + 1..];
        !ext.is_empty() && ext.len() <= 6 && ext.chars().all(|c| c.is_ascii_alphanumeric())
    });
    has_separator || (has_extension && s.chars().filter(|c| *c == '.').count() <= 2)
}

const MAX_IDENTIFIERS: usize = 200;

/// A token counts as a must-preserve identifier if it looks like an ordinary
/// code identifier (has a letter) OR looks like an encoded blob (hex/base64,
/// e.g. a `git rev-parse HEAD` hash) — the latter has no alphabetic
/// character for the ordinary check to key on, but dropping it is exactly
/// the failure mode the noise-detector in `scoring.rs` must not cause: a
/// payload-shaped blob must still trip this gate if compression removes it.
fn extract_identifiers(text: &str, min_len: usize) -> HashSet<String> {
    let mut idents = HashSet::new();
    for word in text.split(|c: char| !c.is_alphanumeric() && c != '_') {
        let looks_like_identifier = word.len() >= min_len
            && (word.chars().any(char::is_alphabetic) || super::scoring::is_blob_token(word));
        if looks_like_identifier {
            idents.insert(word.to_string());
            if idents.len() >= MAX_IDENTIFIERS {
                break;
            }
        }
    }
    idents
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_detection() {
        assert!(looks_like_path("src/main.rs"));
        assert!(looks_like_path("config.toml"));
        assert!(!looks_like_path("hello"));
        assert!(!looks_like_path("a"));
    }

    #[test]
    fn extract_paths_from_text() {
        let paths = extract_paths("error in src/lib.rs at line 42");
        assert!(paths.contains("src/lib.rs"));
    }

    #[test]
    fn extract_identifiers_min_len() {
        let idents = extract_identifiers("fn configure_premium_features(home: Path)", 6);
        assert!(idents.contains("configure_premium_features"));
        assert!(!idents.contains("home"));
    }

    #[test]
    fn quality_passes_with_good_compression() {
        let original =
            "src/main.rs: error[E0308]: mismatched types\nlong description here that is verbose";
        let compressed = "src/main.rs: err[E0308]: mismatched types";
        let report = check(original, compressed, 100, 60, &QualityConfig::default());
        assert!(report.paths_preserved);
    }

    #[test]
    fn quality_passes_when_identifiers_preserved() {
        let report = check("hello", "hello", 100, 98, &QualityConfig::default());
        assert!(
            report.passed,
            "should pass when paths and identifiers are preserved"
        );
        assert!(
            report.savings_pct < 3.0,
            "savings should still be tracked as low"
        );
    }

    #[test]
    fn quality_fails_missing_path() {
        let original = "error in src/config.rs";
        let compressed = "error occurred";
        let report = check(original, compressed, 100, 50, &QualityConfig::default());
        assert!(!report.paths_preserved);
    }

    /// The identifier ratio tolerates 10 % loss, so a long output can lose its one
    /// failing-test count and still clear it. The critical-fact check must not.
    #[test]
    fn critical_fact_check_catches_what_the_identifier_ratio_tolerates() {
        let cases = (0..40)
            .map(|i| format!("payments_integration::refund_case_{i:02} ... ok"))
            .collect::<Vec<_>>()
            .join("\n");
        let original = format!("{cases}\nsummary: 40 passed\n2 failed\n");
        let compressed = format!("{cases}\nsummary: 40 passed\n");
        let report = check(&original, &compressed, 100, 70, &QualityConfig::default());
        assert!(report.identifiers_preserved, "{report:?}");
        assert!(critical_facts_lost(&original, &compressed) > 0);
    }

    /// A pure-digit, hex-shaped blob (e.g. an all-numeric slice of a
    /// `git rev-parse HEAD` hash) has no alphabetic character, so the
    /// ordinary identifier check alone would miss it — `is_blob_token` closes
    /// that gap so dropping it still fails the gate.
    #[test]
    fn quality_fails_when_payload_blob_is_dropped() {
        let blob = "1".repeat(9) + &"2".repeat(9) + &"3".repeat(9);
        let original = format!("commit hash: {blob}");
        let compressed = "commit hash:";
        let report = check(&original, compressed, 100, 10, &QualityConfig::default());
        assert!(
            !report.identifiers_preserved,
            "dropping a pure-digit hash-shaped blob must fail the identifier gate"
        );
    }
}
