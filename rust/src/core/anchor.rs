//! Hash-anchored line identifiers — the shared spine of anchored editing
//! (epic #1008 / GL#1009).
//!
//! Each source line is tagged with a short content hash so an agent can edit
//! "by reference" (line number + hash) instead of reproducing the exact old
//! text byte-for-byte (the `str_replace` *exact-recall tax*). The anchor format
//! is `N:hh|content` — `N` = 1-based line number, `hh` = the first
//! [`ANCHOR_HASH_LEN`] hex chars of `blake3(trim_end(line))`.
//!
//! Two design choices make anchors safe and cheap:
//!
//! * **Whitespace-tolerant**: the hash is computed over the line with trailing
//!   whitespace trimmed, so re-indentation or a stray trailing space does not
//!   spuriously invalidate an anchor while still pinning the meaningful content.
//! * **Determinism-native (#498)**: an anchor is a pure function of the line's
//!   bytes, so `ctx_read(mode="anchored")` is byte-stable across identical
//!   re-reads and provider prompt caching still applies.
//!
//! This module is the single source of truth for the hash + rendering so the
//! read side (`tools::ctx_read`) and the edit side
//! (`tools::ctx_patch`) can never compute anchors differently.

use crate::core::hasher;

/// Hex chars of the per-line BLAKE3 digest carried in an anchor.
///
/// 4 hex = 16 bits. Combined with the line number, a coincidental stale-line
/// collision (a *different* line that happens to share both position and hash)
/// is ~1/65536, while the token overhead stays at a few chars per line.
pub const ANCHOR_HASH_LEN: usize = 4;

/// The anchor hash of a single line.
///
/// Trailing whitespace is ignored so the hash pins meaningful content and stays
/// stable across trivial trailing-whitespace churn. Returns lowercase hex.
#[must_use]
pub fn line_hash(line: &str) -> String {
    let full = hasher::hash_hex(line.trim_end().as_bytes());
    full[..ANCHOR_HASH_LEN].to_string()
}

/// Whether `provided` is the anchor hash of `line` (case-insensitive, trimmed).
///
/// The edit side calls this to detect staleness: if the line on disk no longer
/// hashes to the anchor the model was given, the file drifted and the edit must
/// be rejected rather than applied to the wrong content.
#[must_use]
pub fn hash_matches(line: &str, provided: &str) -> bool {
    line_hash(line).eq_ignore_ascii_case(provided.trim())
}

/// Render `content` as anchored lines `N:hh|text`, numbering from `start_line`
/// (1-based). Pure function of `(content, start_line)` for determinism (#498).
///
/// The returned string has no trailing newline, so callers control framing.
/// Note: line splitting follows [`str::lines`] (a trailing newline does not
/// yield an extra empty line), matching how the rest of `ctx_read` counts lines.
#[must_use]
pub fn annotate(content: &str, start_line: usize) -> String {
    let mut out =
        String::with_capacity(content.len() + content.lines().count() * (ANCHOR_HASH_LEN + 6));
    for (i, line) in content.lines().enumerate() {
        let n = start_line + i;
        out.push_str(&n.to_string());
        out.push(':');
        out.push_str(&line_hash(line));
        out.push('|');
        out.push_str(line);
        out.push('\n');
    }
    out.pop(); // drop the trailing '\n'; callers frame as needed
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_hash_is_short_and_lowercase_hex() {
        let h = line_hash("let x = 1;");
        assert_eq!(h.len(), ANCHOR_HASH_LEN);
        assert!(
            h.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    #[test]
    fn line_hash_ignores_trailing_whitespace() {
        assert_eq!(line_hash("foo"), line_hash("foo   "));
        assert_eq!(line_hash("foo\t"), line_hash("foo"));
    }

    #[test]
    fn line_hash_is_sensitive_to_leading_whitespace() {
        // Leading indentation IS meaningful (it can change scope/semantics), so
        // it must affect the hash — only trailing whitespace is normalized.
        assert_ne!(line_hash("foo"), line_hash("  foo"));
    }

    #[test]
    fn hash_matches_is_case_insensitive_and_trimmed() {
        let h = line_hash("bar");
        assert!(hash_matches("bar", &h));
        assert!(hash_matches("bar", &h.to_uppercase()));
        assert!(hash_matches("bar", &format!("  {h} ")));
        assert!(!hash_matches("baz", &h));
    }

    #[test]
    fn annotate_numbers_from_start_line() {
        let out = annotate("a\nb\nc", 1);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("1:"));
        assert!(lines[1].starts_with("2:"));
        assert!(lines[2].starts_with("3:"));
        assert!(lines[0].ends_with("|a"));
    }

    #[test]
    fn annotate_respects_custom_start_line() {
        let out = annotate("x\ny", 10);
        assert!(out.lines().next().unwrap().starts_with("10:"));
        assert!(out.lines().nth(1).unwrap().starts_with("11:"));
    }

    #[test]
    fn annotate_format_is_parseable() {
        // Format contract relied on by ctx_patch's anchor parser: `N:hh|content`.
        let out = annotate("hello world", 1);
        let (prefix, body) = out.split_once('|').unwrap();
        assert_eq!(body, "hello world");
        let (n, h) = prefix.split_once(':').unwrap();
        assert_eq!(n, "1");
        assert_eq!(h, line_hash("hello world"));
    }

    #[test]
    fn annotate_empty_content_is_empty() {
        assert_eq!(annotate("", 1), "");
    }

    #[test]
    fn annotate_is_deterministic() {
        let content = "fn main() {\n    println!(\"hi\");\n}";
        assert_eq!(annotate(content, 1), annotate(content, 1));
    }

    #[test]
    fn annotate_preserves_blank_lines() {
        let out = annotate("a\n\nb", 1);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[1].starts_with("2:"));
        assert!(lines[1].ends_with('|'), "blank line keeps an empty body");
    }
}
