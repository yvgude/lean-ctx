//! Auto-mined phrase dictionary: abbreviates long identifiers/phrases that
//! repeat within a single compression call.
//!
//! Unlike the static `GENERAL`/`GIT`/`CARGO`/`NPM` dictionaries (known English
//! abbreviations an LLM already understands without a lookup), a repeated
//! project-specific identifier has no universally-known short form. So every
//! substitution is accompanied by a self-describing legend line mapping
//! short -> long. This keeps the full identifier present in the output (the
//! legend), which is what `quality::check`'s identifier-preservation gate
//! looks for, and lets the model resolve the short form unambiguously instead
//! of guessing.

use super::dictionaries::replace_whole_word;
use std::collections::HashMap;

const MIN_PHRASE_LEN: usize = 10;
const MIN_REPEATS: usize = 3;
const MAX_CANDIDATES: usize = 8;
/// Bytes for the `"[dict: ]\n"` wrapper, independent of entry count.
const LEGEND_WRAPPER_OVERHEAD: isize = 9;

/// Finds long tokens repeated at least `MIN_REPEATS` times, replaces them
/// with short codes, and prepends a legend. Returns `None` when no
/// substitution would reduce total size (legend overhead not covered).
pub fn apply(text: &str) -> Option<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for tok in text.split(|c: char| !c.is_alphanumeric() && c != '_') {
        if tok.len() >= MIN_PHRASE_LEN && tok.chars().any(char::is_alphabetic) {
            *counts.entry(tok).or_insert(0) += 1;
        }
    }

    let mut candidates: Vec<(&str, usize)> = counts
        .into_iter()
        .filter(|(_, n)| *n >= MIN_REPEATS)
        .collect();
    if candidates.is_empty() {
        return None;
    }
    candidates.sort_by_key(|(tok, n)| std::cmp::Reverse(tok.len() * n));
    candidates.truncate(MAX_CANDIDATES);

    let mut output = text.to_string();
    let mut legend: Vec<(String, &str)> = Vec::new();
    let mut total_savings: isize = 0;

    for (idx, (tok, n)) in candidates.iter().enumerate() {
        let short = format!("@D{idx}");
        let entry_cost = (short.len() + 1 + tok.len() + 2) as isize;
        let savings = (*n as isize) * (tok.len() as isize - short.len() as isize) - entry_cost;
        if savings <= 0 {
            continue;
        }
        output = replace_whole_word(&output, tok, &short);
        legend.push((short, *tok));
        total_savings += savings;
    }

    if legend.is_empty() || total_savings <= LEGEND_WRAPPER_OVERHEAD {
        return None;
    }

    let legend_line = format!(
        "[dict: {}]\n",
        legend
            .iter()
            .map(|(short, tok)| format!("{short}={tok}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    Some(format!("{legend_line}{output}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_candidates_returns_none() {
        assert!(apply("short words only, nothing repeats here").is_none());
    }

    #[test]
    fn below_repeat_threshold_returns_none() {
        let text = "ConfigurationManagerFactory appears twice ConfigurationManagerFactory here";
        assert!(apply(text).is_none());
    }

    #[test]
    fn repeated_long_identifier_gets_abbreviated_with_legend() {
        let text = "ConfigurationManagerFactory init\nConfigurationManagerFactory ready\nConfigurationManagerFactory done\nConfigurationManagerFactory closed";
        let result = apply(text).expect("should fire: 4 repeats of a 27-char token");
        assert!(
            result.contains("ConfigurationManagerFactory"),
            "full identifier must survive once, in the legend: {result}"
        );
        assert!(result.starts_with("[dict: "), "legend header missing: {result}");
        assert!(
            result.matches("ConfigurationManagerFactory").count() == 1,
            "only the legend should keep the long form, body must use the short code: {result}"
        );
    }

    #[test]
    fn short_output_is_never_larger_than_input() {
        let text = "ConfigurationManagerFactory init\nConfigurationManagerFactory ready\nConfigurationManagerFactory done\nConfigurationManagerFactory closed";
        let result = apply(text).unwrap();
        assert!(
            result.len() < text.len(),
            "mined dictionary must actually shrink output: {} vs {}",
            result.len(),
            text.len()
        );
    }
}
