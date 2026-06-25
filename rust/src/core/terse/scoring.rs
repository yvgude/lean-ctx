//! Surprisal-based line scoring for deterministic compression.
//!
//! Each line receives an information density score based on:
//! - Character-level entropy (Shannon)
//! - Structural markers (paths, numbers, identifiers)
//! - Repetition detection (overlap with previous lines)

use std::collections::HashSet;

/// Score for a single line — higher means more informative.
#[derive(Debug, Clone)]
pub struct LineScore {
    pub line_idx: usize,
    pub entropy: f32,
    pub has_structural_marker: bool,
    pub repetition_ratio: f32,
    pub combined: f32,
}

const MAX_TRIGRAM_SET_SIZE: usize = 10_000;

/// Scores all lines in the input text for information density.
#[must_use]
pub fn score_lines(text: &str) -> Vec<LineScore> {
    let lines: Vec<&str> = text.lines().collect();
    let mut seen_trigrams: HashSet<String> = HashSet::new();
    let mut trigram_saturated = false;
    let mut scores = Vec::with_capacity(lines.len());

    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        let entropy = char_entropy(trimmed);
        let has_marker = has_structural_marker(trimmed);
        let rep_ratio = if trigram_saturated {
            0.0
        } else {
            repetition_ratio(trimmed, &seen_trigrams)
        };

        if !trigram_saturated {
            register_trigrams(trimmed, &mut seen_trigrams);
            if seen_trigrams.len() >= MAX_TRIGRAM_SET_SIZE {
                trigram_saturated = true;
            }
        }

        let combined = compute_combined(entropy, has_marker, rep_ratio);

        scores.push(LineScore {
            line_idx: idx,
            entropy,
            has_structural_marker: has_marker,
            repetition_ratio: rep_ratio,
            combined,
        });
    }

    scores
}

fn char_entropy(line: &str) -> f32 {
    if line.is_empty() {
        return 0.0;
    }
    let mut freq = [0u32; 128];
    let mut total = 0u32;
    for b in line.bytes() {
        if (b as usize) < 128 {
            freq[b as usize] += 1;
            total += 1;
        }
    }
    if total == 0 {
        return 0.0;
    }
    let mut ent = 0.0f32;
    for &count in &freq {
        if count > 0 {
            let p = count as f32 / total as f32;
            ent -= p * p.log2();
        }
    }
    ent
}

fn has_structural_marker(line: &str) -> bool {
    if line.contains('/') && (line.contains('.') || line.contains("src")) {
        return true;
    }
    if line.chars().any(|c| c.is_ascii_digit()) {
        return true;
    }
    if line.contains("error") || line.contains("Error") || line.contains("ERROR") {
        return true;
    }
    if line.contains("warning") || line.contains("Warning") || line.contains("WARN") {
        return true;
    }
    let long_idents = line
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| w.len() >= 6)
        .count();
    long_idents >= 2
}

fn repetition_ratio(line: &str, seen: &HashSet<String>) -> f32 {
    let chars: Vec<char> = line.chars().collect();
    if chars.len() < 9 {
        return 0.0;
    }
    let total = chars.len().saturating_sub(2);
    if total == 0 {
        return 0.0;
    }
    let mut repeated = 0;
    for i in 0..total {
        let end = (i + 3).min(chars.len());
        let trigram: String = chars[i..end].iter().collect();
        if seen.contains(&trigram) {
            repeated += 1;
        }
    }
    repeated as f32 / total as f32
}

fn register_trigrams(line: &str, seen: &mut HashSet<String>) {
    let chars: Vec<char> = line.chars().collect();
    if chars.len() < 3 {
        return;
    }
    for i in 0..chars.len().saturating_sub(2) {
        let end = (i + 3).min(chars.len());
        let trigram: String = chars[i..end].iter().collect();
        seen.insert(trigram);
    }
}

fn compute_combined(entropy: f32, has_marker: bool, rep_ratio: f32) -> f32 {
    let marker_bonus = if has_marker { 0.3 } else { 0.0 };
    let rep_penalty = rep_ratio * 0.5;
    (entropy + marker_bonus - rep_penalty).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_line_zero_entropy() {
        assert_eq!(char_entropy(""), 0.0);
    }

    #[test]
    fn uniform_string_low_entropy() {
        let e = char_entropy("aaaaaaaaaa");
        assert!(e < 0.01, "uniform string should have ~0 entropy, got {e}");
    }

    #[test]
    fn mixed_string_higher_entropy() {
        let low = char_entropy("aaaaaaaaaa");
        let high = char_entropy("abcdefghij");
        assert!(high > low, "mixed > uniform entropy");
    }

    #[test]
    fn structural_marker_path() {
        assert!(has_structural_marker("src/core/config.rs"));
    }

    #[test]
    fn structural_marker_error() {
        assert!(has_structural_marker("error[E0308]: mismatched types"));
    }

    #[test]
    fn structural_marker_missing() {
        assert!(!has_structural_marker("this is a simple line"));
    }

    #[test]
    fn score_lines_returns_all_lines() {
        let text = "line one\nline two\nline three";
        let scores = score_lines(text);
        assert_eq!(scores.len(), 3);
    }

    #[test]
    fn repetitive_lines_get_lower_score() {
        let text = "exactly the same line repeated here\nexactly the same line repeated here\nunique content with different words";
        let scores = score_lines(text);
        assert!(
            scores[2].combined >= scores[1].combined,
            "unique line should score >= repeated: {} vs {}",
            scores[2].combined,
            scores[1].combined
        );
    }
}
