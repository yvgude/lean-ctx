//! Token counting — budget context the same way the engine does.

/// Estimate the token count of `text` using the engine's default tokenizer
/// family. Empty input is `0`.
#[must_use]
pub fn count(text: &str) -> usize {
    lean_ctx::core::tokens::count_tokens(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_zero_nonempty_is_positive() {
        assert_eq!(count(""), 0);
        assert!(count("the quick brown fox") > 0);
    }
}
