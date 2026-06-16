//! Non-code compression tuning (`nc-compress-v1`, EPIC 12.14).
//!
//! The built-in `identity`/`whitespace` compressors are conservative — right for
//! code, but they leave easy savings on prose, scraped web text, and Markdown.
//! This module adds two **lossless-of-meaning** compressors tuned for non-code
//! corpora, registered through the same
//! [`extension_registry`](crate::core::extension_registry) path so they are
//! discoverable and conformance-checked:
//!
//! * `prose` — collapse blank-line runs, strip trailing whitespace, collapse
//!   intra-line whitespace, and drop adjacent duplicate lines (common in logs
//!   and scraped text).
//! * `markdown` — everything `prose` does, plus strip HTML comments, drop image
//!   /badge syntax, and rewrite `[text](url)` links to their visible text
//!   (removing URL token noise an LLM rarely needs).
//!
//! Both honor a hard byte budget and are deterministic — the invariants the
//! conformance suite (`conformance-v1`) enforces on every registered compressor.

// Compressor::name returns &str; the literal names here would otherwise trip
// the lint. The flexibility (runtime-owned names) is intentional registry-wide.
#![allow(clippy::unnecessary_literal_bound)]

use std::sync::Arc;

use super::extension_registry::{Compressor, ExtensionRegistry, truncate_to_budget};

/// `prose`: whitespace + adjacent-duplicate-line compaction for prose corpora.
struct ProseCompressor;
impl Compressor for ProseCompressor {
    fn name(&self) -> &str {
        "prose"
    }
    fn compress(&self, input: &str, budget: Option<usize>) -> String {
        truncate_to_budget(normalize_prose(input), budget)
    }
}

/// `markdown`: prose compaction plus Markdown-noise removal (comments, images,
/// link URLs).
struct MarkdownCompressor;
impl Compressor for MarkdownCompressor {
    fn name(&self) -> &str {
        "markdown"
    }
    fn compress(&self, input: &str, budget: Option<usize>) -> String {
        let stripped = strip_html_comments(input);
        let delinked = rewrite_md_links(&stripped);
        truncate_to_budget(normalize_prose(&delinked), budget)
    }
}

/// Register the non-code compressors into `reg`. Called from
/// [`ExtensionRegistry::with_builtins`].
pub fn register_into(reg: &mut ExtensionRegistry) {
    reg.register_compressor(Arc::new(ProseCompressor));
    reg.register_compressor(Arc::new(MarkdownCompressor));
}

/// Collapse blank-line runs (max one), trim + collapse intra-line whitespace,
/// and drop adjacent duplicate lines. Leading/trailing blank lines are trimmed.
fn normalize_prose(input: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut blank_run = 0u32;
    for line in input.lines() {
        let collapsed = collapse_spaces(line.trim());
        if collapsed.is_empty() {
            blank_run += 1;
            if blank_run <= 1 {
                out.push(String::new());
            }
        } else {
            blank_run = 0;
            // Drop a line identical to the one immediately before it.
            if out.last().map(String::as_str) == Some(collapsed.as_str()) {
                continue;
            }
            out.push(collapsed);
        }
    }
    out.join("\n").trim().to_string()
}

/// Collapse runs of spaces/tabs to a single space; trim trailing space.
fn collapse_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        if ch == ' ' || ch == '\t' {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out.trim_end().to_string()
}

/// Remove `<!-- … -->` comments (unterminated comment drops the remainder).
fn strip_html_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        if let Some(end) = rest[start..].find("-->") {
            rest = &rest[start + end + 3..];
        } else {
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out
}

/// Drop `![alt](url)` images and rewrite `[text](url)` → `text`.
fn rewrite_md_links(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        let rest = &input[i..];
        if let Some(stripped) = rest.strip_prefix("![")
            && let Some((_, consumed)) = parse_md_link(stripped)
        {
            // Skip the whole image: the leading '!' plus the '[..](..)'.
            i += 1 + consumed;
            continue;
        }
        if rest.starts_with('[')
            && let Some((text, consumed)) = parse_md_link(rest)
        {
            out.push_str(&text);
            i += consumed;
            continue;
        }
        let ch = rest.chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Parse a `[text](url)` starting at the leading `[`. Returns the link text and
/// the number of bytes consumed (through the closing `)`).
fn parse_md_link(s: &str) -> Option<(String, usize)> {
    let close_br = s.find(']')?;
    if s.as_bytes().get(close_br + 1) != Some(&b'(') {
        return None;
    }
    let after = &s[close_br + 2..];
    let close_par = after.find(')')?;
    let text = s[1..close_br].to_string();
    let consumed = close_br + 2 + close_par + 1;
    Some((text, consumed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prose() -> ProseCompressor {
        ProseCompressor
    }
    fn markdown() -> MarkdownCompressor {
        MarkdownCompressor
    }

    #[test]
    fn prose_collapses_blanks_and_trailing_ws() {
        let out = prose().compress("a  \n\n\n\nb\t\n", None);
        assert_eq!(out, "a\n\nb");
    }

    #[test]
    fn prose_drops_adjacent_duplicate_lines() {
        let out = prose().compress("same\nsame\nother\nother\nsame", None);
        assert_eq!(out, "same\nother\nsame");
    }

    #[test]
    fn prose_actually_saves_bytes() {
        let input = "line one    \n\n\n\nline one    \nline two\n\n\n";
        let out = prose().compress(input, None);
        assert!(out.len() < input.len());
    }

    #[test]
    fn markdown_strips_comments_images_and_link_urls() {
        let input =
            "<!-- hidden -->Visit ![badge](http://img) the [docs](https://example.com/x) now.";
        let out = markdown().compress(input, None);
        assert!(!out.contains("hidden"));
        assert!(!out.contains("http://img"));
        assert!(!out.contains("https://example.com"));
        assert!(out.contains("docs"));
        assert!(out.contains("Visit"));
    }

    #[test]
    fn budget_is_a_hard_byte_ceiling_utf8_safe() {
        let out = markdown().compress("äöü漢字 text", Some(3));
        assert!(out.len() <= 3);
    }

    #[test]
    fn deterministic() {
        let input = "a\n\nb  \n[x](http://y)";
        assert_eq!(
            markdown().compress(input, None),
            markdown().compress(input, None)
        );
    }

    #[test]
    fn empty_input_stays_empty() {
        assert_eq!(prose().compress("", None), "");
        assert_eq!(markdown().compress("", None), "");
    }
}
