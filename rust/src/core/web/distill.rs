//! Extractive research-compression modes for prose and transcripts.
//!
//! These are deterministic, heuristic distillations — no LLM in the loop — so
//! they are cheap, reproducible, and safe to run inside a synchronous tool
//! handler. They turn a cleaned article or transcript into the high-signal
//! subset an agent actually needs:
//!
//! * [`facts_scored`] — sentences carrying factual signals (numbers, dates,
//!   entities), each with a confidence score.
//! * [`quotes_scored`] — the most central / query-relevant sentences, as
//!   evidence, each with a confidence score.
//! * [`transcript_summary`] — de-duplicated, filler-stripped spoken text.

use std::collections::{HashMap, HashSet};

const MIN_SENTENCE_CHARS: usize = 24;
const MAX_SENTENCE_CHARS: usize = 400;

const STOPWORDS: &[&str] = &[
    "the", "and", "for", "are", "but", "not", "you", "all", "any", "can", "had", "her", "was",
    "one", "our", "out", "day", "get", "has", "him", "his", "how", "man", "new", "now", "old",
    "see", "two", "way", "who", "did", "its", "let", "put", "say", "she", "too", "use", "that",
    "this", "with", "from", "they", "have", "were", "will", "your", "what", "when", "your", "than",
    "then", "them", "into", "more", "some", "such", "only", "also", "been", "very", "just", "over",
];

const FILLER: &[&str] = &[
    "um",
    "uh",
    "erm",
    "hmm",
    "like",
    "basically",
    "actually",
    "literally",
    "honestly",
    "okay",
    "ok",
    "yeah",
    "right",
    "so",
    "well",
    "anyway",
    "anyways",
];

/// Extract sentences carrying factual signals, ranked and de-duplicated. Each
/// sentence carries a confidence (`[0.0, 1.0]`) so callers can build attributable
/// [`crate::core::evidence::Claim`]s. Facts use an *absolute* mapping (more
/// factual signals → higher confidence) rather than min-max, so the score is
/// meaningful even when the top sentences tie.
#[must_use]
pub fn facts_scored(text: &str, query: Option<&str>, max_items: usize) -> Vec<(String, f32)> {
    select_top_scored(facts_ranked(text, query), max_items)
        .into_iter()
        .map(|(text, raw)| (text, factual_confidence(raw)))
        .collect()
}

/// Map a raw factual score (≈ number of factual signals) to absolute confidence.
fn factual_confidence(raw: f32) -> f32 {
    (0.55 + 0.09 * raw).clamp(0.5, 0.97)
}

fn facts_ranked(text: &str, query: Option<&str>) -> Vec<(f64, usize, String)> {
    let qterms = query_terms(query);
    let mut scored = Vec::new();
    for (idx, sentence) in split_sentences(text).into_iter().enumerate() {
        let len = sentence.chars().count();
        if !(MIN_SENTENCE_CHARS..=MAX_SENTENCE_CHARS).contains(&len) {
            continue;
        }
        let base = factual_score(&sentence);
        if base <= 0.0 {
            continue;
        }
        let score = base + query_boost(&sentence, &qterms);
        scored.push((score, idx, sentence));
    }
    scored
}

/// Extract the most central (or query-relevant) sentences as quotable evidence.
/// Each sentence carries a source-relative confidence (`[0.0, 1.0]`).
#[must_use]
pub fn quotes_scored(text: &str, query: Option<&str>, max_items: usize) -> Vec<(String, f32)> {
    normalize_conf(select_top_scored(quotes_ranked(text, query), max_items))
}

fn quotes_ranked(text: &str, query: Option<&str>) -> Vec<(f64, usize, String)> {
    let sentences = split_sentences(text);
    let freq = term_frequencies(&sentences);
    let qterms = query_terms(query);

    let mut scored = Vec::new();
    for (idx, sentence) in sentences.into_iter().enumerate() {
        let len = sentence.chars().count();
        if !(MIN_SENTENCE_CHARS..=MAX_SENTENCE_CHARS).contains(&len) {
            continue;
        }
        let centrality = centrality_score(&sentence, &freq);
        let score = centrality + query_boost(&sentence, &qterms) * 3.0;
        if score <= 0.0 {
            continue;
        }
        scored.push((score, idx, sentence));
    }
    scored
}

/// Summarize prose to a `max_chars` budget, query-aware.
///
/// For inputs that already fit the budget this is exactly [`transcript_summary`]
/// (filler-strip + adjacent-dedup, no truncation) — no behaviour change. For
/// OVERSIZED inputs, where [`transcript_summary`] would FIFO-truncate to the
/// prefix, it instead uses extractive ranking ([`crate::core::extractive`]) to
/// keep the most query-relevant (or, without a query, the most central)
/// sentences. Falls back to [`transcript_summary`] when the embedding engine is
/// unavailable, so no build/OS regresses.
pub fn summarize_prose(text: &str, max_chars: usize, query: Option<&str>) -> String {
    if text.len() <= max_chars {
        return transcript_summary(text, max_chars);
    }
    let mode = if query.is_some() {
        crate::core::extractive::RankMode::Query
    } else {
        crate::core::extractive::RankMode::Centrality
    };
    if let Some(ranked) = crate::core::extractive::rank_and_squeeze(text, max_chars, mode, query) {
        return ranked;
    }
    transcript_summary(text, max_chars)
}

/// Condense a transcript: strip filler, drop near-duplicate runs, cap length.
#[must_use]
pub fn transcript_summary(text: &str, max_chars: usize) -> String {
    let mut kept: Vec<String> = Vec::new();
    let mut total = 0usize;

    for sentence in split_sentences(text) {
        let cleaned = strip_filler(&sentence);
        let cleaned = cleaned.trim();
        if cleaned.chars().count() < 8 {
            continue;
        }
        if let Some(last) = kept.last()
            && jaccard(last, cleaned) > 0.8
        {
            continue;
        }
        if total + cleaned.len() > max_chars && !kept.is_empty() {
            break;
        }
        total += cleaned.len();
        kept.push(cleaned.to_string());
    }
    kept.join(" ")
}

/// Line-structure-preserving prose squeeze for the proxy tool-result funnel.
///
/// Unlike [`transcript_summary`] (which collapses everything into one paragraph),
/// this keeps paragraph/heading shape and only:
/// * collapses runs of blank lines to a single blank,
/// * drops a line that is a near-duplicate of a recently kept line
///   (boilerplate / nav repeats common in scraped pages),
/// * caps total length to `max_chars` with a truncation marker.
///
/// Filler-word stripping is intentionally *not* applied here: words like
/// "so" / "like" / "right" carry meaning in written prose and are only noise in
/// spoken transcripts.
pub fn squeeze_prose(text: &str, max_chars: usize) -> String {
    const RECENT: usize = 12;
    let mut out: Vec<String> = Vec::new();
    let mut recent: Vec<String> = Vec::new();
    let mut total = 0usize;
    let mut blank_run = 0u32;

    for raw in text.lines() {
        let line = raw.trim_end();
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run == 1 && !out.is_empty() {
                out.push(String::new());
            }
            continue;
        }
        blank_run = 0;

        let normalized = line.trim();
        if !is_protected_line(line) && recent.iter().any(|p| jaccard(p, normalized) > 0.9) {
            continue;
        }

        if total + line.len() > max_chars && !out.is_empty() {
            out.push("…[truncated]".to_string());
            break;
        }
        total += line.len();
        out.push(line.to_string());

        recent.push(normalized.to_string());
        if recent.len() > RECENT {
            recent.remove(0);
        }
    }

    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out.join("\n")
}

/// Lines that must survive dedup: citations, links, headings and quote/list
/// markers carry attribution or structure even when textually similar.
pub(crate) fn is_protected_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("Source:")
        || t.starts_with("Site:")
        || t.starts_with("http://")
        || t.starts_with("https://")
        || t.starts_with("- [")
        || t.starts_with("> ")
        || t.starts_with('#')
        || t.starts_with("---")
}

// ── Sentence splitting ─────────────────────────────────────────────────────

/// Split text into trimmed, non-empty sentences across line boundaries.
#[must_use]
pub fn split_sentences(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut current = String::new();
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            current.push(c);
            if matches!(c, '.' | '!' | '?') {
                let boundary = chars.peek().is_none_or(|n| n.is_whitespace());
                if boundary {
                    push_trimmed(&mut sentences, &current);
                    current.clear();
                }
            }
        }
        push_trimmed(&mut sentences, &current);
    }
    sentences
}

fn push_trimmed(acc: &mut Vec<String>, s: &str) {
    let trimmed = s.trim();
    if !trimmed.is_empty() {
        acc.push(trimmed.to_string());
    }
}

// ── Scoring ────────────────────────────────────────────────────────────────

fn factual_score(sentence: &str) -> f64 {
    let lower = sentence.to_lowercase();
    let mut score = 0.0;

    if sentence.chars().any(|c| c.is_ascii_digit()) {
        score += 1.0;
    }
    if sentence.contains('%') || sentence.contains('$') || sentence.contains('€') {
        score += 1.0;
    }
    if has_year(sentence) {
        score += 1.0;
    }
    if has_magnitude_word(&lower) {
        score += 1.0;
    }
    if proper_noun_runs(sentence) >= 1 {
        score += 0.5;
    }
    score
}

fn has_year(sentence: &str) -> bool {
    let bytes = sentence.as_bytes();
    let mut run = 0;
    for &b in bytes {
        if b.is_ascii_digit() {
            run += 1;
            if run == 4 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

fn has_magnitude_word(lower: &str) -> bool {
    const WORDS: &[&str] = &[
        "percent",
        "million",
        "billion",
        "trillion",
        "thousand",
        "kg",
        "km",
        "mph",
        "gb",
        "mb",
        "tb",
        "ghz",
        "kwh",
        "celsius",
        "fahrenheit",
        "dollars",
        "euros",
    ];
    WORDS.iter().any(|w| contains_word(lower, w))
}

fn proper_noun_runs(sentence: &str) -> usize {
    let mut runs = 0;
    let mut consecutive = 0;
    for (i, word) in sentence.split_whitespace().enumerate() {
        let is_cap = word.chars().next().is_some_and(char::is_uppercase);
        // Ignore the very first word (sentence-initial capital is not a signal).
        if is_cap && i > 0 {
            consecutive += 1;
            if consecutive == 2 {
                runs += 1;
            }
        } else {
            consecutive = 0;
        }
    }
    runs
}

fn term_frequencies(sentences: &[String]) -> HashMap<String, usize> {
    let mut freq = HashMap::new();
    for sentence in sentences {
        for word in content_words(sentence) {
            *freq.entry(word).or_insert(0) += 1;
        }
    }
    freq
}

fn centrality_score(sentence: &str, freq: &HashMap<String, usize>) -> f64 {
    let words = content_words(sentence);
    if words.is_empty() {
        return 0.0;
    }
    let sum: usize = words.iter().filter_map(|w| freq.get(w)).sum();
    sum as f64 / (words.len() as f64).sqrt()
}

fn query_terms(query: Option<&str>) -> HashSet<String> {
    query
        .map(|q| {
            q.split(|c: char| !c.is_alphanumeric())
                .filter(|w| w.len() >= 3)
                .map(str::to_lowercase)
                .collect()
        })
        .unwrap_or_default()
}

fn query_boost(sentence: &str, qterms: &HashSet<String>) -> f64 {
    if qterms.is_empty() {
        return 0.0;
    }
    let lower = sentence.to_lowercase();
    qterms.iter().filter(|t| contains_word(&lower, t)).count() as f64
}

fn select_top_scored(
    mut scored: Vec<(f64, usize, String)>,
    max_items: usize,
) -> Vec<(String, f32)> {
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
    });

    let mut seen = HashSet::new();
    let mut chosen: Vec<(usize, String, f64)> = Vec::new();
    for (score, idx, sentence) in scored {
        if seen.insert(norm_key(&sentence)) {
            chosen.push((idx, sentence, score));
            if chosen.len() >= max_items {
                break;
            }
        }
    }
    chosen.sort_by_key(|(idx, _, _)| *idx);
    chosen
        .into_iter()
        .map(|(_, s, sc)| (s, sc as f32))
        .collect()
}

/// Map raw heuristic scores onto a source-relative confidence in `[0.45, 0.95]`
/// (single/uniform item → 0.8). Deterministic min-max within the selected set.
fn normalize_conf(items: Vec<(String, f32)>) -> Vec<(String, f32)> {
    if items.is_empty() {
        return items;
    }
    let max = items.iter().map(|(_, s)| *s).fold(f32::MIN, f32::max);
    let min = items.iter().map(|(_, s)| *s).fold(f32::MAX, f32::min);
    let span = max - min;
    if span < f32::EPSILON {
        return items.into_iter().map(|(t, _)| (t, 0.8)).collect();
    }
    items
        .into_iter()
        .map(|(t, s)| (t, 0.45 + 0.5 * (s - min) / span))
        .collect()
}

// ── Word helpers ───────────────────────────────────────────────────────────

fn content_words(sentence: &str) -> Vec<String> {
    sentence
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3)
        .map(str::to_lowercase)
        .filter(|w| !STOPWORDS.contains(&w.as_str()))
        .collect()
}

fn word_set(s: &str) -> HashSet<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn jaccard(a: &str, b: &str) -> f64 {
    let sa = word_set(a);
    let sb = word_set(b);
    if sa.is_empty() && sb.is_empty() {
        return 1.0;
    }
    let inter = sa.intersection(&sb).count() as f64;
    let union = sa.union(&sb).count() as f64;
    if union == 0.0 { 0.0 } else { inter / union }
}

fn strip_filler(sentence: &str) -> String {
    sentence
        .split_whitespace()
        .filter(|tok| {
            let core: String = tok
                .chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
                .to_lowercase();
            !core.is_empty() && !FILLER.contains(&core.as_str())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn contains_word(haystack: &str, word: &str) -> bool {
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(word) {
        let idx = start + pos;
        let before = idx
            .checked_sub(1)
            .is_none_or(|i| !haystack.as_bytes()[i].is_ascii_alphanumeric());
        let after_idx = idx + word.len();
        let after = haystack
            .as_bytes()
            .get(after_idx)
            .is_none_or(|b| !b.is_ascii_alphanumeric());
        if before && after {
            return true;
        }
        start = idx + word.len();
    }
    false
}

fn norm_key(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drop confidence scores so ranking assertions read like prod callers.
    fn names(scored: Vec<(String, f32)>) -> Vec<String> {
        scored.into_iter().map(|(s, _)| s).collect()
    }

    #[test]
    fn splits_sentences_across_lines() {
        let text = "First sentence here. Second one follows!\nThird line stands alone?";
        let s = split_sentences(text);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0], "First sentence here.");
        assert_eq!(s[2], "Third line stands alone?");
    }

    #[test]
    fn facts_keep_numeric_and_drop_fluff() {
        let text = "Revenue grew to 12 million dollars in 2023. \
                    I really enjoyed the lovely afternoon weather today.";
        let f = names(facts_scored(text, None, 5));
        assert_eq!(f.len(), 1);
        assert!(f[0].contains("12 million"));
    }

    #[test]
    fn facts_respect_query_boost_and_limit() {
        let text = "The rocket reached 400 km altitude. \
                    The budget was 5 billion euros overall. \
                    Apollo Eleven landed in 1969 successfully.";
        let f = names(facts_scored(text, Some("budget"), 1));
        assert_eq!(f.len(), 1);
        assert!(f[0].contains("budget"));
    }

    #[test]
    fn quotes_prefer_query_relevant_sentences() {
        let text = "Climate policy shapes future energy markets across regions. \
                    The cat sat quietly on the warm windowsill all day. \
                    Energy markets respond to climate policy and carbon pricing.";
        let q = names(quotes_scored(text, Some("climate energy"), 2));
        assert_eq!(q.len(), 2);
        assert!(
            q.iter().all(
                |s| s.to_lowercase().contains("energy") || s.to_lowercase().contains("climate")
            )
        );
    }

    #[test]
    fn transcript_summary_strips_filler_and_dupes() {
        let text = "Um so basically the model is really fast. \
                    Um so basically the model is really fast. \
                    Actually it scales to millions of requests.";
        let summary = transcript_summary(text, 500);
        assert!(!summary.to_lowercase().contains("basically"));
        // Near-duplicate second line is dropped.
        assert_eq!(summary.matches("the model is really fast").count(), 1);
        assert!(summary.contains("scales to millions"));
    }

    #[test]
    fn transcript_summary_respects_budget() {
        let text = "Alpha statement number one here. Beta statement number two here. \
                    Gamma statement number three here.";
        let summary = transcript_summary(text, 30);
        assert!(summary.len() <= 60, "got {} chars", summary.len());
        assert!(summary.contains("Alpha"));
    }

    #[test]
    fn summarize_prose_below_budget_matches_transcript_summary() {
        // When the text already fits, summarize_prose is exactly the
        // filler-stripping transcript_summary — no extractive path, no change.
        let text = "Um so basically the cache is fast. Actually it also persists.";
        assert_eq!(
            summarize_prose(text, 10_000, Some("cache")),
            transcript_summary(text, 10_000)
        );
    }

    #[test]
    fn summarize_prose_is_deterministic_and_bounded_when_oversized() {
        // Oversized input: in `cargo test` the engine is never loaded, so this
        // exercises the graceful fallback to transcript_summary. Determinism and
        // the budget must hold on every build.
        let text = "Sentence about alpha topic here. ".repeat(40);
        let a = summarize_prose(&text, 120, Some("alpha"));
        let b = summarize_prose(&text, 120, Some("alpha"));
        assert_eq!(a, b);
        assert!(!a.is_empty() && a.len() < text.len());
    }

    #[test]
    fn squeeze_prose_dedupes_and_collapses_blanks() {
        let text = "Rust is a systems programming language focused on safety.\n\n\n\
                    Rust is a systems programming language focused on safety.\n\
                    It guarantees memory safety without a garbage collector.";
        let out = squeeze_prose(text, 10_000);
        // Near-duplicate line dropped.
        assert_eq!(out.matches("focused on safety").count(), 1);
        // Blank run collapsed to at most a single blank line.
        assert!(!out.contains("\n\n\n"));
        assert!(out.contains("memory safety"));
    }

    #[test]
    fn squeeze_prose_keeps_protected_lines() {
        let text = "- [Home](https://x.com)\n- [Home](https://x.com)\n\
                    > A quote that repeats.\n> A quote that repeats.";
        let out = squeeze_prose(text, 10_000);
        // Protected (link/quote) lines are never deduped away.
        assert_eq!(out.matches("[Home]").count(), 2);
        assert_eq!(out.matches("A quote that repeats").count(), 2);
    }

    #[test]
    fn squeeze_prose_caps_length() {
        let big = "This is a unique sentence number ";
        let text = (0..500)
            .map(|i| format!("{big}{i}."))
            .collect::<Vec<_>>()
            .join("\n");
        let out = squeeze_prose(&text, 400);
        assert!(out.contains("…[truncated]"));
        assert!(out.len() <= 600, "got {} chars", out.len());
    }

    #[test]
    fn contains_word_matches_whole_words_only() {
        assert!(contains_word("the budget is large", "budget"));
        assert!(!contains_word("budgetary spending", "budget"));
    }

    #[test]
    fn facts_scored_assigns_bounded_confidence() {
        let text = "Revenue grew to 12 million dollars in 2023. \
                    Apollo Eleven landed on the Moon in 1969 successfully. \
                    The annual budget was 5 billion euros overall.";
        let scored = facts_scored(text, None, 3);
        assert!(!scored.is_empty(), "expected scored facts");
        for (_, conf) in &scored {
            assert!(
                (0.0..=1.0).contains(conf),
                "confidence out of range: {conf}"
            );
        }
    }

    #[test]
    fn facts_confidence_scales_with_signals() {
        // Rich fact (digits + magnitude + year) should outrank a thin one.
        let rich =
            factual_confidence(factual_score("Revenue grew to 12 million dollars in 2023.") as f32);
        let thin = factual_confidence(factual_score("There were 3 cats.") as f32);
        assert!(rich > thin, "rich={rich} thin={thin}");
        assert!((0.5..=0.97).contains(&rich));
    }

    #[test]
    fn quotes_single_item_gets_default_confidence() {
        let scored = normalize_conf(vec![("only one".to_string(), 4.2)]);
        assert_eq!(scored.len(), 1);
        assert!((scored[0].1 - 0.8).abs() < 1e-6);
    }
}
