// SPDX-License-Identifier: Apache-2.0

//! Splitting a markdown body into sections and joining it back.
//!
//! A body splits at its `## ` headings. A `## ` block larger than the limit
//! splits further at `### `, then at blank lines, then at line ends, and a
//! single overlong line at a character boundary. Headings inside code fences
//! are text. The split is lossless: [`join`] of [`split`] gives the body back
//! whenever it opens with its `# ` title line.

use crate::model::Section;
use crate::{MAX_DOC_BODY_BYTES, ORDER_GAP, SECTION_MAX_BYTES};

/// Splits `body` into its title and sections no larger than `max` bytes.
pub fn split_with(body: &str, max: usize) -> (String, Vec<Section>) {
    let max = max.max(4);
    let (title, rest) = take_title(body);
    let mut chunks = Vec::new();
    for block in blocks(rest, 2) {
        pack_into(&mut chunks, block, max, 3);
    }
    (title, lay_out(chunks))
}

/// [`split_with`] at [`SECTION_MAX_BYTES`].
pub fn split(body: &str) -> (String, Vec<Section>) {
    split_with(body, SECTION_MAX_BYTES)
}

/// The body of `title` and `sections`, sections in `order`.
pub fn join(title: &str, sections: &[Section]) -> String {
    let mut ordered: Vec<&Section> = sections.iter().collect();
    ordered.sort_by_key(|s| s.order);
    let mut out = String::new();
    if !title.is_empty() {
        out.push_str("# ");
        out.push_str(title);
        out.push('\n');
    }
    for s in ordered {
        out.push_str(&s.text);
    }
    out
}

/// Fresh orders for `n` sections: `ORDER_GAP`, `2 * ORDER_GAP`, ….
pub fn renumber(n: usize) -> Vec<u32> {
    (1..=n as u32)
        .map(|i| i.saturating_mul(ORDER_GAP))
        .collect()
}

/// An order strictly between `before` and `after` (either end open), or
/// `None` when no integer fits and the page has to be renumbered.
pub fn order_between(before: Option<u32>, after: Option<u32>) -> Option<u32> {
    let lo = before.unwrap_or(0);
    let Some(hi) = after else {
        return lo.checked_add(ORDER_GAP);
    };
    (hi > lo.saturating_add(1)).then(|| lo + (hi - lo) / 2)
}

fn lay_out(chunks: Vec<String>) -> Vec<Section> {
    let chunks: Vec<String> = chunks.into_iter().filter(|c| !c.is_empty()).collect();
    renumber(chunks.len())
        .into_iter()
        .zip(chunks)
        .map(|(order, text)| Section { order, text })
        .collect()
}

/// Strips a leading `# ` title line; a body without one has an empty title.
fn take_title(body: &str) -> (String, &str) {
    let first_end = body.find('\n').map_or(body.len(), |i| i + 1);
    let first = &body[..first_end];
    match first.strip_prefix("# ") {
        Some(t) => (
            t.trim_end_matches(['\n', '\r']).to_owned(),
            &body[first_end..],
        ),
        None => (String::new(), body),
    }
}

/// Is `line` an ATX heading of exactly `level`?
fn is_heading(line: &str, level: usize) -> bool {
    let b = line.as_bytes();
    b.len() > level && b[..level].iter().all(|c| *c == b'#') && b[level] == b' '
}

/// Tracks fenced code blocks line by line.
#[derive(Default)]
pub(crate) struct Fence {
    open: Option<(u8, usize)>,
}

impl Fence {
    /// Feeds one line; true while the line is inside (or opens/closes) a fence.
    pub(crate) fn feed(&mut self, line: &str) -> bool {
        let t = line.trim_start();
        let marker = t.bytes().next().filter(|c| *c == b'`' || *c == b'~');
        let run = marker.map_or(0, |m| t.bytes().take_while(|c| *c == m).count());
        match (self.open, marker) {
            (Some((m, n)), Some(c)) if c == m && run >= n && t[run..].trim().is_empty() => {
                self.open = None;
                true
            }
            (Some(_), _) => true,
            (None, Some(c)) if run >= 3 => {
                self.open = Some((c, run));
                true
            }
            (None, _) => false,
        }
    }
}

/// Splits `text` before every heading of `level` outside fences.
fn blocks(text: &str, level: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut fence = Fence::default();
    let mut start = 0;
    let mut pos = 0;
    for line in text.split_inclusive('\n') {
        let fenced = fence.feed(line);
        if !fenced && pos > start && is_heading(line, level) {
            out.push(&text[start..pos]);
            start = pos;
        }
        pos += line.len();
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// Splits `text` after every blank line outside fences.
fn paragraphs(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut fence = Fence::default();
    let mut start = 0;
    let mut pos = 0;
    for line in text.split_inclusive('\n') {
        let fenced = fence.feed(line);
        pos += line.len();
        if !fenced && line.trim().is_empty() {
            out.push(&text[start..pos]);
            start = pos;
        }
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// Appends `block` to `chunks`, split finer until every piece fits `max`.
/// `level` is the next heading level to split at (7 = paragraphs, 8 = lines).
fn pack_into(chunks: &mut Vec<String>, block: &str, max: usize, level: usize) {
    if fits(block.len(), escaped_len(block), max) {
        chunks.push(block.to_owned());
        return;
    }
    let (pieces, next): (Vec<&str>, usize) = match level {
        3..=6 => (blocks(block, level), level + 1),
        7 => (paragraphs(block), 8),
        8 => (block.split_inclusive('\n').collect(), 9),
        _ => {
            hard_cut(chunks, block, max);
            return;
        }
    };
    if pieces.len() <= 1 {
        pack_into(chunks, block, max, next);
        return;
    }
    // Greedily pack consecutive pieces; a piece too large alone splits finer.
    let mut acc = String::new();
    let mut acc_escaped = 0;
    for piece in pieces {
        let escaped = escaped_len(piece);
        if !fits(piece.len(), escaped, max) {
            if !acc.is_empty() {
                chunks.push(std::mem::take(&mut acc));
                acc_escaped = 0;
            }
            pack_into(chunks, piece, max, next);
        } else if !fits(acc.len() + piece.len(), acc_escaped + escaped, max) {
            chunks.push(std::mem::replace(&mut acc, piece.to_owned()));
            acc_escaped = escaped;
        } else {
            acc.push_str(piece);
            acc_escaped += escaped;
        }
    }
    if !acc.is_empty() {
        chunks.push(acc);
    }
}

/// Room the rest of a section body (`order`, the braces) takes in its JSON.
const BODY_ENVELOPE: usize = 64;

/// Does a text of `len` bytes, `escaped` bytes as a JSON string without its
/// quotes, fit a section of `max` bytes and a doc body of
/// [`MAX_DOC_BODY_BYTES`]?
fn fits(len: usize, escaped: usize, max: usize) -> bool {
    len <= max && escaped + 2 + BODY_ENVELOPE <= MAX_DOC_BODY_BYTES
}

/// The length of `text` as a JSON string, without the quotes. Additive over
/// concatenation, so packed pieces can be summed.
fn escaped_len(text: &str) -> usize {
    text.chars()
        .map(|c| match c {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{08}' | '\u{0c}' => 2,
            c if (c as u32) < 0x20 => 6,
            c => c.len_utf8(),
        })
        .sum()
}

/// Cuts an overlong line at character boundaries.
fn hard_cut(chunks: &mut Vec<String>, text: &str, max: usize) {
    let mut rest = text;
    while !fits(rest.len(), escaped_len(rest), max) {
        let mut cut = max.min(rest.len());
        while !rest.is_char_boundary(cut) || !fits(cut, escaped_len(&rest[..cut]), max) {
            cut -= 1;
        }
        chunks.push(rest[..cut].to_owned());
        rest = &rest[cut..];
    }
    if !rest.is_empty() {
        chunks.push(rest.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(s: &[Section]) -> Vec<&str> {
        s.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn splits_at_level_two_headings_and_round_trips() {
        let body = "# Title\nintro\n\n## One\na\n### Sub\nb\n## Two\nc\n";
        let (title, sections) = split(body);
        assert_eq!(title, "Title");
        assert_eq!(
            texts(&sections),
            ["intro\n\n", "## One\na\n### Sub\nb\n", "## Two\nc\n"]
        );
        assert_eq!(sections[1].heading(), Some("One"));
        assert_eq!(sections[0].heading(), None);
        assert_eq!(join(&title, &sections), body);
    }

    #[test]
    fn headings_inside_fences_are_text() {
        let body = "# T\n## A\n```md\n## not a heading\n```\n## B\nx\n";
        let (_, sections) = split(body);
        assert_eq!(sections.len(), 2);
        assert!(sections[0].text.contains("## not a heading"));
    }

    #[test]
    fn oversized_blocks_split_finer_and_stay_lossless() {
        let para = "word ".repeat(30) + "\n\n";
        let body = format!("# T\n## Big\n{}### Sub\n{}", para.repeat(4), para.repeat(4));
        let (title, sections) = split_with(&body, 400);
        assert!(sections.len() > 2);
        assert!(sections.iter().all(|s| s.text.len() <= 400));
        assert_eq!(join(&title, &sections), body);
        let orders: Vec<u32> = sections.iter().map(|s| s.order).collect();
        assert_eq!(orders, renumber(sections.len()));
    }

    #[test]
    fn a_single_overlong_line_is_cut_at_char_boundaries() {
        let body = format!("# T\n{}\n", "ä".repeat(300));
        let (title, sections) = split_with(&body, 101);
        assert!(sections.iter().all(|s| s.text.len() <= 101));
        assert_eq!(join(&title, &sections), body);
    }

    #[test]
    fn heavy_escaping_splits_under_the_json_body_limit() {
        let body = format!("# T\n{}\n", "\"\\".repeat(9_000));
        let (title, sections) = split(&body);
        assert!(sections.len() >= 2, "18 000 bytes escape to 36 000");
        for s in &sections {
            let json = serde_json::to_string(&s.text).unwrap_or_default();
            assert!(json.len() + BODY_ENVELOPE <= MAX_DOC_BODY_BYTES);
            assert_eq!(json.len(), escaped_len(&s.text) + 2);
        }
        assert_eq!(join(&title, &sections), body);
    }

    #[test]
    fn a_body_without_title_keeps_everything() {
        let (title, sections) = split("plain\n## A\nx\n");
        assert_eq!(title, "");
        assert_eq!(join(&title, &sections), "plain\n## A\nx\n");
    }

    #[test]
    fn order_between_finds_gaps_or_asks_for_renumbering() {
        assert_eq!(order_between(None, None), Some(ORDER_GAP));
        assert_eq!(order_between(Some(1024), Some(2048)), Some(1536));
        assert_eq!(order_between(Some(5), Some(6)), None);
        assert_eq!(order_between(Some(2048), None), Some(3072));
        assert_eq!(order_between(None, Some(1024)), Some(512));
    }
}
