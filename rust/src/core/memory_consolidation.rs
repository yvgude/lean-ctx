//! Case-folded token Jaccard shared by knowledge chunking and playbook dedup.
use std::collections::HashSet;

/// Jaccard similarity over whitespace-split tokens (case-folded).
pub(crate) fn token_jaccard(a: &str, b: &str) -> f64 {
    let sa: HashSet<String> = a.split_whitespace().map(str::to_lowercase).collect();
    let sb: HashSet<String> = b.split_whitespace().map(str::to_lowercase).collect();
    let inter = sa.intersection(&sb).count();
    let uni = sa.union(&sb).count();
    if uni == 0 {
        0.0
    } else {
        inter as f64 / uni as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_jaccard_folds_case_and_scores_empty_as_zero() {
        assert!((token_jaccard("Alpha beta", "alpha BETA") - 1.0).abs() < 1e-9);
        assert!((token_jaccard("alpha beta", "alpha gamma") - 1.0 / 3.0).abs() < 1e-9);
        assert!(token_jaccard("", "").abs() < 1e-9);
    }
}
