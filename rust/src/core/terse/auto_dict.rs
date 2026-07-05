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

/// A repeated-token candidate, tracked with its first-occurrence position so
/// selection and numbering never depend on `HashMap` iteration order (which
/// varies per-process) — required for byte-stable output across runs so
/// providers can prompt-cache on it (#498).
struct Candidate<'a> {
    token: &'a str,
    count: usize,
    first_idx: usize,
}

/// Finds long tokens repeated at least `MIN_REPEATS` times, replaces them
/// with short codes, and prepends a legend. Returns `None` when no
/// substitution would reduce total size (legend overhead not covered).
pub fn apply(text: &str) -> Option<String> {
    // Count occurrences while recording first-occurrence order in a plain
    // Vec — the HashMap below is only used for O(1) lookups during counting,
    // never for iteration order.
    let mut first_seen_order: Vec<&str> = Vec::new();
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for tok in text.split(|c: char| !c.is_alphanumeric() && c != '_') {
        if tok.len() >= MIN_PHRASE_LEN && tok.chars().any(char::is_alphabetic) {
            let entry = counts.entry(tok).or_insert(0);
            if *entry == 0 {
                first_seen_order.push(tok);
            }
            *entry += 1;
        }
    }

    let mut candidates: Vec<Candidate> = first_seen_order
        .into_iter()
        .enumerate()
        .filter_map(|(first_idx, token)| {
            let count = counts[token];
            (count >= MIN_REPEATS).then_some(Candidate {
                token,
                count,
                first_idx,
            })
        })
        .collect();
    if candidates.is_empty() {
        return None;
    }

    // Selection: biggest savings first, so the limited MAX_CANDIDATES slots
    // go to the highest-value tokens. Ties are broken by first occurrence
    // then lexicographically — never left to arbitrary hash order.
    candidates.sort_by(|a, b| {
        let score_a = a.token.len() * a.count;
        let score_b = b.token.len() * b.count;
        score_b
            .cmp(&score_a)
            .then_with(|| a.first_idx.cmp(&b.first_idx))
            .then_with(|| a.token.cmp(b.token))
    });
    candidates.truncate(MAX_CANDIDATES);

    // Numbering: @D0/@D1/... assigned by first occurrence (tie-broken
    // lexicographically), independent of the savings-based selection order
    // above, so the same input always yields the same short-code assignment.
    candidates.sort_by(|a, b| {
        a.first_idx
            .cmp(&b.first_idx)
            .then_with(|| a.token.cmp(b.token))
    });

    let mut output = text.to_string();
    let mut legend: Vec<(String, &str)> = Vec::new();
    let mut total_savings: isize = 0;

    for (idx, c) in candidates.iter().enumerate() {
        let short = format!("@D{idx}");
        let entry_cost = (short.len() + 1 + c.token.len() + 2) as isize;
        let savings =
            (c.count as isize) * (c.token.len() as isize - short.len() as isize) - entry_cost;
        if savings <= 0 {
            continue;
        }
        output = replace_whole_word(&output, c.token, &short);
        legend.push((short, c.token));
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
        assert!(
            result.starts_with("[dict: "),
            "legend header missing: {result}"
        );
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

    /// #498: two candidates with an identical savings score (same length,
    /// same repeat count) must still get a byte-stable @D0/@D1 assignment —
    /// the first one to appear in the text, not whichever a HashMap's
    /// randomized iteration order happens to visit first.
    #[test]
    fn tied_candidates_are_numbered_by_first_occurrence_not_hash_order() {
        let text = "AlphaBetaGammaDeltaEpsilon here\nAlphaBetaGammaDeltaEpsilon there\nAlphaBetaGammaDeltaEpsilon again\nZetaEtaThetaIotaKappaOmega here\nZetaEtaThetaIotaKappaOmega there\nZetaEtaThetaIotaKappaOmega again";
        let result = apply(text).expect("both 26-char tokens repeat 3x — should fire");
        assert!(
            result.starts_with(
                "[dict: @D0=AlphaBetaGammaDeltaEpsilon, @D1=ZetaEtaThetaIotaKappaOmega]"
            ),
            "first-seen token must be @D0 even when tied on savings score: {result}"
        );
    }

    /// #498: same input compressed repeatedly must yield byte-identical
    /// output — the determinism/prompt-cache guarantee the maintainer asked
    /// for, same pattern as the existing guards in `ctx_read`/`shell::redact`.
    #[test]
    fn mining_output_is_deterministic_across_repeated_calls() {
        let text = "AlphaBetaGammaDeltaEpsilon here\nAlphaBetaGammaDeltaEpsilon there\nAlphaBetaGammaDeltaEpsilon again\nZetaEtaThetaIotaKappaOmega here\nZetaEtaThetaIotaKappaOmega there\nZetaEtaThetaIotaKappaOmega again";
        let first = apply(text);
        for _ in 0..20 {
            assert_eq!(
                apply(text),
                first,
                "mining must be byte-stable across repeated calls on identical input"
            );
        }
    }
}
