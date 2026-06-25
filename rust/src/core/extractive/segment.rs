//! Deterministic segmentation of prose into ranking units for extractive
//! compression ([`super`]).
//!
//! A unit is a sentence, except structurally important lines (headings, links,
//! list/quote markers, citations — see [`is_protected_line`]) which are kept
//! whole and never sentence-split or dropped. Blank lines delimit paragraphs so
//! the re-emitted text keeps its shape. The function is a pure, allocation-only
//! transform — no model, no I/O — so the #498 determinism contract holds before
//! any embedding is involved.

use crate::core::web::distill::{is_protected_line, split_sentences};

/// One ranking unit of the source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Segment {
    /// Global order index (0-based): stable tiebreak + original-order re-emit.
    pub idx: usize,
    /// Paragraph index, incremented at each blank-line run. Drives re-emit shape.
    pub para: usize,
    /// Trimmed text used for embedding, scoring and the char budget.
    pub text: String,
    /// Structural line (heading/link/list/quote/citation): always kept and
    /// emitted verbatim on its own line.
    pub protected: bool,
}

/// Split `text` into ordered [`Segment`]s. Pure and deterministic.
pub(super) fn segment(text: &str) -> Vec<Segment> {
    let mut segs = Vec::new();
    let mut idx = 0usize;
    let mut para = 0usize;
    let mut seen_content = false;
    let mut pending_break = false;

    for line in text.lines() {
        if line.trim().is_empty() {
            if seen_content {
                pending_break = true;
            }
            continue;
        }
        if pending_break {
            para += 1;
            pending_break = false;
        }
        seen_content = true;

        if is_protected_line(line) {
            segs.push(Segment {
                idx,
                para,
                text: line.trim().to_string(),
                protected: true,
            });
            idx += 1;
        } else {
            for sentence in split_sentences(line) {
                segs.push(Segment {
                    idx,
                    para,
                    text: sentence,
                    protected: false,
                });
                idx += 1;
            }
        }
    }
    segs
}

/// Reassemble selected segments (which MUST be sorted by `idx`) back into prose.
/// Sentences inside one paragraph flow with a space; paragraph changes insert a
/// blank line; protected lines always sit on their own line. Deterministic.
pub(super) fn reassemble(selected: &[&Segment]) -> String {
    let mut out = String::new();
    let mut prev: Option<&Segment> = None;
    for seg in selected {
        if let Some(p) = prev {
            let sep = if seg.para != p.para {
                "\n\n"
            } else if seg.protected || p.protected {
                "\n"
            } else {
                " "
            };
            out.push_str(sep);
        }
        out.push_str(&seg.text);
        prev = Some(seg);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_sentences_and_assigns_indices() {
        let segs = segment("First sentence. Second sentence. Third one.");
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0].text, "First sentence.");
        assert_eq!(segs[2].text, "Third one.");
        assert_eq!(
            segs.iter().map(|s| s.idx).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert!(segs.iter().all(|s| !s.protected));
    }

    #[test]
    fn marks_protected_lines_whole() {
        let text = "# Heading\nA normal sentence here. Another one.\nhttps://example.com/page\n- [ref] a list item";
        let segs = segment(text);
        let protected: Vec<&str> = segs
            .iter()
            .filter(|s| s.protected)
            .map(|s| s.text.as_str())
            .collect();
        assert!(protected.contains(&"# Heading"));
        assert!(protected.contains(&"https://example.com/page"));
        assert!(protected.contains(&"- [ref] a list item"));
        // The URL is NOT sentence-split on its dots.
        assert!(segs.iter().any(|s| s.text == "https://example.com/page"));
    }

    #[test]
    fn blank_lines_advance_paragraph() {
        let segs = segment("Para one sentence.\n\nPara two sentence.\n\n\nPara three.");
        assert_eq!(segs[0].para, 0);
        assert_eq!(segs[1].para, 1);
        assert_eq!(segs[2].para, 2);
    }

    #[test]
    fn reassemble_preserves_order_and_shape() {
        let segs = segment("Alpha one. Alpha two.\n\nBeta one.");
        let refs: Vec<&Segment> = segs.iter().collect();
        let out = reassemble(&refs);
        assert_eq!(out, "Alpha one. Alpha two.\n\nBeta one.");
    }

    #[test]
    fn reassemble_skips_dropped_middle_segment() {
        let segs = segment("Keep A. Drop B. Keep C.");
        // Drop the middle segment (idx 1).
        let kept: Vec<&Segment> = segs.iter().filter(|s| s.idx != 1).collect();
        assert_eq!(reassemble(&kept), "Keep A. Keep C.");
    }
}
