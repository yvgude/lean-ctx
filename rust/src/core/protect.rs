//! Explicit, deterministic preservation of user-marked spans across every
//! compressor (read, shell, proxy, prose).
//!
//! Two complementary, fully deterministic mechanisms (#498 — pure functions of
//! their inputs, no model, no clock, no global state):
//!
//! 1. **Universal markers** `<lc_safe>…</lc_safe>`: any compressor wraps its
//!    work in [`compress_preserving`]; the content between the markers passes
//!    through verbatim and the markers themselves are stripped from the output.
//! 2. **`protect` token list** (`ctx_read` convenience): `line_is_protected`
//!    lets the line-based lossy filters (entropy / information-bottleneck)
//!    force-keep every line that contains one of the given tokens.
//!
//! Security: callers MUST run secret redaction *before* protect (`redact →
//! protect → compress`). Protect never re-introduces redacted secrets because
//! it only ever passes through bytes that already survived redaction.

/// Opening marker for a verbatim-preserved span.
pub(crate) const SAFE_OPEN: &str = "<lc_safe>";
/// Closing marker for a verbatim-preserved span.
pub(crate) const SAFE_CLOSE: &str = "</lc_safe>";

/// Cheap pre-check: does the input contain at least one protect marker? Lets
/// hot paths skip the span-splitting machinery entirely when nothing is marked.
#[must_use]
pub fn has_markers(s: &str) -> bool {
    s.contains(SAFE_OPEN)
}

/// Compress only the *unprotected* regions of `input` with `f`; everything
/// between `<lc_safe>` and `</lc_safe>` passes through byte-for-byte and the
/// markers are stripped from the output.
///
/// Deterministic: pure function of `(input, f)`. An unterminated open marker
/// keeps the remainder of the input verbatim (fail-safe: never compress what the
/// user tried to protect). When there are no markers the input is handed to `f`
/// unchanged, so existing callers keep their exact byte output.
#[must_use]
pub fn compress_preserving<F: Fn(&str) -> String>(input: &str, f: F) -> String {
    if !has_markers(input) {
        return f(input);
    }
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find(SAFE_OPEN) {
        out.push_str(&f(&rest[..start]));
        let after = &rest[start + SAFE_OPEN.len()..];
        let Some(end) = after.find(SAFE_CLOSE) else {
            out.push_str(after); // unterminated → keep remainder verbatim
            return out;
        };
        out.push_str(&after[..end]); // verbatim, markers dropped
        rest = &after[end + SAFE_CLOSE.len()..];
    }
    out.push_str(&f(rest));
    out
}

/// True if `line` must survive a lossy line filter because it contains one of
/// the explicit `protect` tokens. Empty tokens are ignored so an empty list (or
/// a list of empty strings) reproduces today's behaviour exactly.
#[must_use]
pub(crate) fn line_is_protected(line: &str, needles: &[String]) -> bool {
    needles
        .iter()
        .any(|n| !n.is_empty() && line.contains(n.as_str()))
}

/// Stable cache-key fragment for a `protect` token list, or `""` when the list
/// is empty (so unprotected reads keep their current cache key).
///
/// The fragment is order- and duplicate-independent: force-keep matching is a
/// set operation, so `["a","b"]` and `["b","a","a"]` must map to the same key
/// (#498). Tokens are canonicalised (non-empty, sorted, deduped) before hashing.
#[must_use]
pub fn protect_fragment(needles: &[String]) -> String {
    let mut canon: Vec<&str> = needles
        .iter()
        .map(String::as_str)
        .filter(|s| !s.is_empty())
        .collect();
    if canon.is_empty() {
        return String::new();
    }
    canon.sort_unstable();
    canon.dedup();
    // NUL separator: cannot occur inside a realistic source token, so distinct
    // token sets cannot collide by joining (e.g. ["ab","c"] vs ["a","bc"]).
    let joined = canon.join("\u{0}");
    format!("p{}", &crate::core::hasher::hash_short(&joined)[..8])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A maximally aggressive test compressor: it deletes *everything*. If a
    /// span survives this, it survives any real compressor.
    fn drop_all(_: &str) -> String {
        String::new()
    }

    #[test]
    fn no_markers_passes_input_to_f() {
        assert_eq!(
            compress_preserving("plain text", str::to_uppercase),
            "PLAIN TEXT"
        );
        assert!(!has_markers("plain text"));
    }

    #[test]
    fn protect_spans_survive_compression() {
        let input =
            "noise before\n<lc_safe>CRITICAL = 42\nkeep me literally</lc_safe>\nnoise after";
        let out = compress_preserving(input, drop_all);
        // Protected content is byte-identical and the markers are gone.
        assert_eq!(out, "CRITICAL = 42\nkeep me literally");
        assert!(!out.contains(SAFE_OPEN));
        assert!(!out.contains(SAFE_CLOSE));
    }

    #[test]
    fn unprotected_regions_are_compressed_protected_are_not() {
        let f = |s: &str| s.to_uppercase();
        let out = compress_preserving("a<lc_safe>b</lc_safe>c", f);
        assert_eq!(out, "AbC");
    }

    #[test]
    fn multiple_spans_all_survive() {
        let input = "x<lc_safe>one</lc_safe>y<lc_safe>two</lc_safe>z";
        let out = compress_preserving(input, drop_all);
        assert_eq!(out, "onetwo");
    }

    #[test]
    fn unterminated_marker_keeps_remainder_verbatim() {
        let input = "drop<lc_safe>tail without close\nstill kept";
        let out = compress_preserving(input, drop_all);
        assert_eq!(out, "tail without close\nstill kept");
    }

    #[test]
    fn compress_preserving_is_deterministic() {
        let input = "a<lc_safe>SAFE</lc_safe>b<lc_safe>X</lc_safe>c";
        let a = compress_preserving(input, str::to_uppercase);
        let b = compress_preserving(input, str::to_uppercase);
        assert_eq!(a, b);
    }

    #[test]
    fn line_is_protected_matches_token_and_ignores_empty() {
        let needles = vec!["TODO".to_string(), String::new()];
        assert!(line_is_protected("  // TODO: fix", &needles));
        assert!(!line_is_protected("nothing here", &needles));
        // An empty needle must never match every line.
        assert!(!line_is_protected("anything", &[String::new()]));
        assert!(!line_is_protected("anything", &[]));
    }

    #[test]
    fn protect_fragment_empty_is_blank() {
        assert_eq!(protect_fragment(&[]), "");
        assert_eq!(protect_fragment(&[String::new()]), "");
    }

    #[test]
    fn protect_fragment_is_order_and_dup_independent() {
        let a = protect_fragment(&["alpha".to_string(), "beta".to_string()]);
        let b = protect_fragment(&["beta".to_string(), "alpha".to_string(), "alpha".to_string()]);
        assert_eq!(a, b);
        assert!(a.starts_with('p'));
        assert_eq!(a.len(), 9); // 'p' + 8 hex chars
    }

    #[test]
    fn protect_fragment_distinguishes_sets() {
        let a = protect_fragment(&["alpha".to_string()]);
        let b = protect_fragment(&["beta".to_string()]);
        assert_ne!(a, b);
    }
}
