//! QUITO-X–style trade-off: compress by dropping low token-entropy lines while targeting an output/input token ratio.
//!
//! Query-conditioned variant (#542, EFF-5): the IB objective is
//! `min I(T;X) − β·I(T;Y)` — the relevance variable Y (the task/query) must
//! condition the compression. `compress_ib_with_query` fuses normalized
//! entropy with an IDF-weighted query-term overlap (the lexical core of
//! BM25), so two different queries keep different lines from the same file
//! (QUITO-X EMNLP'25: query-conditioned beats query-agnostic by 20-25%
//! accuracy at equal rate). Without a query the behavior is byte-identical
//! to the entropy-only path.

use super::entropy::normalized_token_entropy;
use super::tokens::count_tokens;

fn flush_omitted(out: &mut Vec<String>, run: &mut usize) {
    if *run > 0 {
        out.push(format!("// ... {} low-info lines omitted", *run));
        *run = 0;
    }
}

fn render_ib(lines: &[&str], scores: &[f64], threshold: f64) -> String {
    debug_assert_eq!(lines.len(), scores.len());
    let mut out = Vec::new();
    let mut omit_run = 0usize;
    for (&line, &score) in lines.iter().zip(scores.iter()) {
        if score >= threshold {
            flush_omitted(&mut out, &mut omit_run);
            out.push(line.to_string());
        } else {
            omit_run += 1;
        }
    }
    flush_omitted(&mut out, &mut omit_run);
    out.join("\n")
}

/// Compress `text` toward `target_ratio` (output tokens / input tokens) by dropping lines whose
/// normalized BPE token entropy falls below a dynamically chosen threshold.
#[must_use]
pub fn compress_ib(text: &str, target_ratio: f64) -> String {
    compress_ib_with_query(text, target_ratio, None)
}

/// Tokenize for relevance scoring: lowercase alphanumeric runs, length >= 2.
fn relevance_terms(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 2)
        .map(str::to_string)
        .collect()
}

/// Per-line I(T;Y) proxy: IDF-weighted overlap between query terms and line
/// terms, normalized to [0,1] across the document. Deterministic, no model.
fn query_relevance_scores(lines: &[&str], query: &str) -> Option<Vec<f64>> {
    let q_terms: std::collections::HashSet<String> = relevance_terms(query).into_iter().collect();
    if q_terms.is_empty() {
        return None;
    }

    let mut df: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let line_terms: Vec<Vec<String>> = lines.iter().map(|l| relevance_terms(l)).collect();
    for terms in &line_terms {
        let unique: std::collections::HashSet<&str> = terms.iter().map(String::as_str).collect();
        for t in unique {
            if q_terms.contains(t) {
                *df.entry(t).or_insert(0) += 1;
            }
        }
    }
    if df.is_empty() {
        return None;
    }

    let n = lines.len() as f64;
    let raw: Vec<f64> = line_terms
        .iter()
        .map(|terms| {
            let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
            terms
                .iter()
                .filter(|t| q_terms.contains(t.as_str()) && seen.insert(t.as_str()))
                .map(|t| {
                    let d = *df.get(t.as_str()).unwrap_or(&1) as f64;
                    ((n + 1.0) / d).ln()
                })
                .sum::<f64>()
        })
        .collect();

    let max = raw.iter().copied().fold(0.0_f64, f64::max);
    if max <= 0.0 {
        return None;
    }
    Some(raw.into_iter().map(|s| s / max).collect())
}

/// Query-conditioned IB compression (#542). With a query, the keep-score is
/// `0.5·entropy + 0.5·relevance`; without one (or when the query shares no
/// terms with the document) this is exactly the entropy-only `compress_ib`.
#[must_use]
pub fn compress_ib_with_query(text: &str, target_ratio: f64, query: Option<&str>) -> String {
    if text.is_empty() {
        return String::new();
    }
    let input_tokens = count_tokens(text);
    if input_tokens == 0 {
        return text.to_string();
    }
    let ratio_target = target_ratio.clamp(0.02, 1.0);

    let lines_vec: Vec<&str> = text.lines().collect();
    let lines: &[&str] = &lines_vec;
    let entropy_scores: Vec<f64> = lines
        .iter()
        .map(|ln| normalized_token_entropy(ln))
        .collect();

    let scores: Vec<f64> = match query.and_then(|q| query_relevance_scores(lines, q)) {
        Some(relevance) => entropy_scores
            .iter()
            .zip(relevance.iter())
            .map(|(e, r)| 0.5 * e + 0.5 * r)
            .collect(),
        None => entropy_scores,
    };

    // Higher threshold ⇒ fewer kept lines ⇒ lower output ratio (monotone decreasing in threshold).
    let mut lo = 0.0_f64;
    let mut hi = 1.0_f64;
    let mut best = render_ib(lines, &scores, 0.0);
    let mut best_diff = f64::INFINITY;

    let mut consider = |thr: f64| {
        let cand = render_ib(lines, &scores, thr);
        let r = count_tokens(&cand) as f64 / input_tokens as f64;
        let diff = (r - ratio_target).abs();
        if diff < best_diff {
            best_diff = diff;
            best = cand;
        }
    };

    for _ in 0..26 {
        let mid = f64::midpoint(lo, hi);
        let cand = render_ib(lines, &scores, mid);
        let r = count_tokens(&cand) as f64 / input_tokens as f64;
        consider(mid);
        if r > ratio_target {
            lo = mid;
        } else {
            hi = mid;
        }
    }

    for thr in [0.0_f64, 1.0_f64, lo, hi, f64::midpoint(lo, hi)] {
        consider(thr);
    }

    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_ratio_one_keeps_content() {
        assert_eq!(compress_ib("", 0.5), "");
        let s = "fn main() {\n    println!(\"hi\");\n}\n";
        let full = compress_ib(s, 1.0);
        assert!(full.contains("fn main"));
    }

    #[test]
    fn strong_compression_drops_redundant_lines() {
        let mut boring = String::new();
        for _ in 0..30 {
            boring.push_str("aaa bbb aaa bbb\n");
        }
        boring.push_str("unique_identifier_xyz_quartz\n");
        let out = compress_ib(&boring, 0.15);
        assert!(out.contains("low-info lines omitted"));
        assert!(out.contains("unique_identifier_xyz_quartz"));
        assert!(count_tokens(&out) < count_tokens(&boring));
    }

    #[test]
    fn placeholder_counts_skipped_lines() {
        let lines: Vec<String> = (0..5).map(|_| "x x x x".into()).collect();
        let mut text = lines.join("\n");
        text.push('\n');
        text.push_str("serde Deserialize TraitBounds\n");
        let out = compress_ib(&text, 0.25);
        assert!(out.contains("low-info lines omitted"));
        assert!(out.contains("serde"));
    }

    fn two_topic_fixture() -> String {
        let mut s = String::new();
        for _ in 0..10 {
            s.push_str(
                "fn parse_webhook_event(payload: Json) -> StripeEvent { decode(payload) }\n",
            );
        }
        for _ in 0..10 {
            s.push_str("fn render_dashboard_chart(data: Series) -> Svg { plot(data) }\n");
        }
        s
    }

    #[test]
    fn different_queries_keep_different_lines() {
        let text = two_topic_fixture();
        let a = compress_ib_with_query(&text, 0.3, Some("stripe webhook event parsing"));
        let b = compress_ib_with_query(&text, 0.3, Some("dashboard chart rendering svg"));
        assert_ne!(a, b, "query must condition the kept lines");
        assert!(a.contains("webhook"), "query-a keeps its topic: {a}");
        assert!(b.contains("dashboard"), "query-b keeps its topic: {b}");
    }

    #[test]
    fn no_query_is_byte_identical_to_entropy_only() {
        let text = two_topic_fixture();
        assert_eq!(
            compress_ib_with_query(&text, 0.3, None),
            compress_ib(&text, 0.3)
        );
        // A query sharing no terms with the document degrades gracefully to
        // the entropy-only result as well.
        assert_eq!(
            compress_ib_with_query(&text, 0.3, Some("zzz qqq vvv")),
            compress_ib(&text, 0.3)
        );
    }

    #[test]
    fn compression_ratio_invariant_holds_with_query() {
        let text = two_topic_fixture();
        let out = compress_ib_with_query(&text, 0.3, Some("stripe webhook"));
        let ratio = count_tokens(&out) as f64 / count_tokens(&text) as f64;
        // The 2-topic fixture only has two score levels, so the closest
        // reachable ratio to 0.3 is "keep one topic" (~0.55). The invariant
        // is: never blow past the coarsest achievable step.
        assert!(ratio <= 0.6, "ratio stays near target, got {ratio}");
        assert!(out.contains("webhook") && !out.contains("dashboard"));
    }
}
