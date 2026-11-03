//! Shared edit-distance "did you mean" helper (#712).
//!
//! One implementation for every typo-suggestion surface: CLI commands
//! (`cli/dispatch/suggest.rs`), config keys (`cli/config_cmd.rs`) and MCP
//! tool names (`server/dispatch`). Wagner-Fischer over Unicode scalar values
//! with a single rolling row — candidate sets are tiny (dozens of names), so
//! O(a·b) time per pair is irrelevant; what matters is that all callers agree
//! on distances.

/// Classic Wagner-Fischer edit distance with O(min) memory.
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// The closest candidate within a length-scaled edit budget (one edit for
/// short names, roughly a third of the length for longer ones), or `None`
/// when nothing is near enough to suggest with confidence. Ties resolve to
/// the first candidate in iteration order.
pub fn closest<'a, I>(input: &str, candidates: I) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    let budget = (input.chars().count() / 3).max(1);
    candidates
        .into_iter()
        .map(|cand| (cand, levenshtein(input, cand)))
        .filter(|&(_, dist)| dist <= budget)
        .min_by_key(|&(_, dist)| dist)
        .map(|(cand, _)| cand)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_distances() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("abc", "abc"), 0);
        assert_eq!(levenshtein("abc", "abd"), 1);
        assert_eq!(levenshtein("udpate", "update"), 2);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
    }

    #[test]
    fn unicode_scalars_not_bytes() {
        assert_eq!(levenshtein("héllo", "hello"), 1);
    }

    #[test]
    fn closest_respects_length_scaled_budget() {
        let tools = ["ctx_read", "ctx_search", "ctx_shell", "ctx_tree"];
        assert_eq!(closest("ctx_raed", tools), Some("ctx_read"));
        assert_eq!(closest("ctx_serach", tools), Some("ctx_search"));
        // Distance beyond the budget → no confident suggestion.
        assert_eq!(closest("completely_else", tools), None);
        assert_eq!(closest("", tools), None);
    }
}
