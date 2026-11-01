// SPDX-License-Identifier: Apache-2.0

//! Deterministic Markdown/documentation compaction for LLM-agent reads.
//!
//! Keeps heading topology intact, treats fenced code blocks as atomic units, and
//! selects high-signal body units with a small IDF-style scorer. Intentionally
//! std-only and byte-stable (#498): per-line token sets are ordered
//! (`BTreeSet`), so the f64 score summation order — and therefore the selected
//! lines and omission markers — are a pure function of the input bytes.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;

const SECTION_BODY_FRACTION: f64 = 0.35;
const MIN_SECTION_BODY_LINES: usize = 2;
/// Documents with fewer content (non-blank) lines pass through untouched.
const MIN_CONTENT_LINES: usize = 24;

const STOP_WORDS: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "into", "your", "you", "are", "can",
    "will", "not", "all", "use", "using", "used", "lean", "ctx", "context", "agent", "agents",
];

/// One compaction unit: a heading line, a prose line, or an atomic fenced block.
///
/// Fenced blocks are kept or dropped only as a whole, so the output never
/// contains an unbalanced fence or an omission marker inside a code example.
struct Unit {
    /// Raw line range `[start, end)` covered by this unit.
    start: usize,
    end: usize,
    kind: UnitKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum UnitKind {
    Heading,
    Prose,
    Fence,
}

impl Unit {
    /// Non-blank lines covered by this unit (blank interior fence lines are
    /// emitted verbatim but carry no "content" weight in budgets or markers).
    fn content_lines(&self, lines: &[&str]) -> usize {
        lines[self.start..self.end]
            .iter()
            .filter(|l| !l.trim().is_empty())
            .count()
    }
}

/// Compact Markdown while preserving all headings, whole fenced code blocks,
/// and high-signal details. Returns `None` when the document is too small, not
/// markdown-shaped, or compaction would not actually shrink it.
pub(crate) fn compact_markdown(content: &str) -> Option<String> {
    if content.trim().is_empty() || !looks_like_markdown(content) {
        return None;
    }

    let lines: Vec<&str> = content.lines().collect();
    let units = parse_units(&lines);
    let content_total: usize = units.iter().map(|u| u.content_lines(&lines)).sum();
    if content_total < MIN_CONTENT_LINES {
        return None;
    }

    let keep = select_units(&lines, &units, content_total);

    let mut out = String::new();
    let mut omitted = 0usize;
    for (uidx, unit) in units.iter().enumerate() {
        if keep.contains(&uidx) {
            flush_omission(&mut out, &mut omitted);
            for line in &lines[unit.start..unit.end] {
                out.push_str(line.trim_end());
                out.push('\n');
            }
        } else {
            omitted += unit.content_lines(&lines);
        }
    }
    flush_omission(&mut out, &mut omitted);

    let kept_content = out.lines().filter(|l| !l.trim().is_empty()).count();
    if kept_content >= content_total || out.len() >= content.len() {
        None
    } else {
        Some(out)
    }
}

/// True when the document carries at least one ATX heading — the structural
/// signal a plain `.txt` file must show before the lossy compactor may touch it
/// (hyphen lists alone are not enough to call a text file "markdown").
pub(crate) fn has_markdown_headings(content: &str) -> bool {
    content.lines().any(is_heading)
}

/// Splits raw lines into units. Blank lines outside fences belong to no unit:
/// they carry no signal and are dropped silently (not counted as "omitted"),
/// matching the compact typography of the output. Lines inside a fence — blank
/// or not — always travel with their block.
fn parse_units(lines: &[&str]) -> Vec<Unit> {
    let mut units = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        if trimmed.is_empty() {
            i += 1;
            continue;
        }
        if let Some(marker) = fence_marker(trimmed) {
            let mut end = i + 1;
            while end < lines.len() && !is_closing_fence(lines[end], marker) {
                end += 1;
            }
            // Include the closing fence; an unterminated block runs to EOF.
            let end = (end + 1).min(lines.len());
            units.push(Unit {
                start: i,
                end,
                kind: UnitKind::Fence,
            });
            i = end;
            continue;
        }
        let kind = if is_heading(lines[i]) {
            UnitKind::Heading
        } else {
            UnitKind::Prose
        };
        units.push(Unit {
            start: i,
            end: i + 1,
            kind,
        });
        i += 1;
    }
    units
}

/// Returns the fence character when `trimmed` opens a code fence (``` or ~~~).
fn fence_marker(trimmed: &str) -> Option<char> {
    ['`', '~']
        .into_iter()
        .find(|&marker| trimmed.chars().take_while(|c| *c == marker).count() >= 3)
}

/// A closing fence is a run of >=3 fence characters with nothing but whitespace
/// after it (an info string like ```rust only ever opens a block).
fn is_closing_fence(line: &str, marker: char) -> bool {
    let trimmed = line.trim_start();
    trimmed.chars().take_while(|c| *c == marker).count() >= 3
        && trimmed.chars().all(|c| c == marker || c.is_whitespace())
}

fn select_units(lines: &[&str], units: &[Unit], content_total: usize) -> HashSet<usize> {
    let docs = token_sets(lines);
    let df = document_frequency(&docs);
    let mut keep = HashSet::new();
    let mut section_body: Vec<usize> = Vec::new();

    for (uidx, unit) in units.iter().enumerate() {
        if unit.kind == UnitKind::Heading {
            select_section_body(
                &mut keep,
                &section_body,
                lines,
                units,
                &docs,
                &df,
                content_total,
            );
            section_body.clear();
            keep.insert(uidx);
        } else {
            section_body.push(uidx);
        }
    }
    select_section_body(
        &mut keep,
        &section_body,
        lines,
        units,
        &docs,
        &df,
        content_total,
    );
    keep
}

fn select_section_body(
    keep: &mut HashSet<usize>,
    body: &[usize],
    lines: &[&str],
    units: &[Unit],
    docs: &[BTreeSet<String>],
    df: &HashMap<String, usize>,
    content_total: usize,
) {
    if body.is_empty() {
        return;
    }

    let body_lines: usize = body.iter().map(|u| units[*u].content_lines(lines)).sum();
    let target = section_body_budget(body_lines);

    let mut scored: Vec<(usize, f64)> = body
        .iter()
        .map(|uidx| {
            (
                *uidx,
                score_unit(&units[*uidx], lines, docs, content_total, df),
            )
        })
        .collect();
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    // The first body unit anchors the section (usually its intro sentence).
    if let Some(first) = body.first() {
        keep.insert(*first);
    }
    // Greedy by content lines so a large fenced block spends its real size of
    // the budget instead of counting as one line.
    let mut taken = 0usize;
    for (uidx, _) in scored {
        if taken >= target {
            break;
        }
        keep.insert(uidx);
        taken += units[uidx].content_lines(lines);
    }
}

fn section_body_budget(line_count: usize) -> usize {
    let fractional = ((line_count as f64) * SECTION_BODY_FRACTION).ceil() as usize;
    fractional.max(MIN_SECTION_BODY_LINES).min(line_count)
}

fn flush_omission(out: &mut String, omitted: &mut usize) {
    if *omitted == 0 {
        return;
    }
    let _ = writeln!(out, "... [lean-ctx: omitted {omitted} lines]");
    *omitted = 0;
}

fn looks_like_markdown(content: &str) -> bool {
    content.lines().any(is_heading)
        || content.lines().any(|l| l.trim_start().starts_with("- "))
        || content.lines().any(|l| l.trim_start().starts_with("* "))
        || content.lines().any(|l| l.trim_start().starts_with("| "))
}

/// ATX heading per CommonMark: 1–6 `#` followed by a space (or end of line).
/// The space requirement keeps shebangs (`#!/usr/bin/env`) and `#pragma`-style
/// lines from masquerading as document structure.
fn is_heading(line: &str) -> bool {
    let trimmed = line.trim_start();
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&hashes) && (trimmed.len() == hashes || trimmed[hashes..].starts_with(' '))
}

/// Per-line token sets. `BTreeSet` (not `HashSet`) is load-bearing: the score
/// is an f64 sum over these tokens, and f64 addition is not associative, so the
/// iteration order must be fixed for the output to be byte-stable (#498).
fn token_sets(lines: &[&str]) -> Vec<BTreeSet<String>> {
    lines
        .iter()
        .map(|line| tokens(line).into_iter().collect())
        .collect()
}

fn document_frequency(docs: &[BTreeSet<String>]) -> HashMap<String, usize> {
    let mut df = HashMap::new();
    for doc in docs {
        for token in doc {
            *df.entry(token.clone()).or_insert(0) += 1;
        }
    }
    df
}

fn score_unit(
    unit: &Unit,
    lines: &[&str],
    docs: &[BTreeSet<String>],
    content_total: usize,
    df: &HashMap<String, usize>,
) -> f64 {
    match unit.kind {
        UnitKind::Fence => {
            // A block is one unit of meaning: score the union of its tokens
            // once, with the same code bonus a backticked prose line gets.
            let mut tokens = BTreeSet::new();
            for doc in &docs[unit.start..unit.end] {
                tokens.extend(doc.iter().cloned());
            }
            idf_sum(&tokens, content_total, df) + 10.0 + position_bonus(unit.start)
        }
        _ => score_line(
            unit.start,
            lines[unit.start],
            &docs[unit.start],
            content_total,
            df,
        ),
    }
}

fn score_line(
    idx: usize,
    line: &str,
    tokens: &BTreeSet<String>,
    line_count: usize,
    df: &HashMap<String, usize>,
) -> f64 {
    let mut score = idf_sum(tokens, line_count, df);

    let trimmed = line.trim_start();
    if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("| ") {
        score += 2.0;
    }
    if line.contains('`')
        || line.contains("MUST")
        || line.contains("SHOULD")
        || line.contains("BLOCKING")
        || line.contains("WARNING")
        || line.contains("ctx_")
        || line.contains("lean-ctx")
    {
        score += 10.0;
    }
    score + position_bonus(idx)
}

/// IDF-style sum; iterating a `BTreeSet` keeps the f64 summation order fixed.
fn idf_sum(tokens: &BTreeSet<String>, line_count: usize, df: &HashMap<String, usize>) -> f64 {
    let mut score = 0.0;
    for token in tokens {
        let freq = *df.get(token).unwrap_or(&1) as f64;
        score += ((line_count as f64 + 1.0) / (freq + 1.0)).ln();
    }
    score
}

fn position_bonus(idx: usize) -> f64 {
    1.0 / (idx + 1) as f64
}

fn tokens(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in line.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '/' | '.' | ':') {
            cur.push(ch);
        } else if !cur.is_empty() {
            push_token(&mut out, &cur);
            cur.clear();
        }
    }
    if !cur.is_empty() {
        push_token(&mut out, &cur);
    }
    out
}

fn push_token(out: &mut Vec<String>, token: &str) {
    let t = token
        .trim_matches(|c: char| matches!(c, '.' | ',' | ':' | ';' | '(' | ')' | '[' | ']'))
        .to_ascii_lowercase();
    if t.len() < 3 || STOP_WORDS.contains(&t.as_str()) {
        return;
    }
    if t.len() >= 8 || t.chars().any(|c| matches!(c, '_' | '-' | '/' | '.' | ':')) {
        out.push(t);
    }
}

#[cfg(test)]
mod tests {
    use super::{compact_markdown, has_markdown_headings};

    #[test]
    fn keeps_all_headings_and_shrinks() {
        let input = "# Title\n\nIntro paragraph with ordinary words.\n\n## Setup\n\n";
        let body = "- `ctx_read` keeps important details for agents.\n";
        let repeated = "This sentence is useful once but repeated many times for filler.\n";
        let mut doc = input.to_string();
        for _ in 0..20 {
            doc.push_str(repeated);
        }
        doc.push_str(body);
        doc.push_str("## Safety\n\nMUST preserve warnings and exact commands.\n");
        for _ in 0..20 {
            doc.push_str(repeated);
        }

        let compacted = compact_markdown(&doc).expect("markdown should compact");
        assert!(compacted.len() < doc.len());
        assert!(compacted.contains("# Title"));
        assert!(compacted.contains("## Setup"));
        assert!(compacted.contains("## Safety"));
        assert!(compacted.contains("ctx_read"));
        assert!(compacted.contains("MUST preserve"));
        assert!(compacted.contains("[lean-ctx: omitted"));
    }

    #[test]
    fn keeps_body_lines_from_each_section() {
        let mut doc = String::from("# Root\n\nRoot intro.\n");
        for section in 0..8 {
            doc.push_str(&format!("\n## Section {section}\n\n"));
            doc.push_str(&format!("Section {section} overview line.\n"));
            for item in 0..12 {
                doc.push_str(&format!(
                    "- repeated filler item {item} for section {section}.\n"
                ));
            }
            doc.push_str(&format!(
                "BLOCKING section {section} exact requirement with `ctx_read`.\n"
            ));
        }

        let compacted = compact_markdown(&doc).expect("markdown should compact");
        assert!(compacted.len() < doc.len());
        for section in 0..8 {
            assert!(compacted.contains(&format!("## Section {section}")));
            assert!(compacted.contains(&format!("Section {section} overview line.")));
            assert!(compacted.contains(&format!("BLOCKING section {section}")));
        }
        assert!(compacted.contains("[lean-ctx: omitted"));
    }

    #[test]
    fn short_docs_pass_through() {
        assert!(compact_markdown("# Title\n\nSmall.\n").is_none());
    }

    #[test]
    fn output_is_deterministic_across_calls() {
        // #498 regression guard: the score is an f64 sum over per-line token
        // sets. With unordered sets, near-tied lines could flip across calls
        // (each std HashSet instance iterates in its own random order), moving
        // omission markers and changing bytes. Mixed token frequencies below
        // engineer many near-ties on purpose.
        let mut doc = String::from("# Determinism\n\nIntro line for the document.\n");
        for section in 0..4 {
            doc.push_str(&format!("\n## Section {section}\n\n"));
            for i in 0..30 {
                doc.push_str(&format!(
                    "candidate_{i} shared_token_{} another_token_{} overlapping detail item.\n",
                    i % 3,
                    i % 7,
                ));
            }
        }

        let first = compact_markdown(&doc).expect("doc should compact");
        for _ in 0..16 {
            let next = compact_markdown(&doc).expect("doc should compact");
            assert_eq!(first, next, "compaction must be byte-stable across calls");
        }
    }

    #[test]
    fn fenced_blocks_stay_atomic() {
        let filler = "Ordinary explanatory filler sentence repeated for volume.\n";
        let mut doc = String::from("# Guide\n\nIntro line.\n\n## Usage\n\n");
        for _ in 0..30 {
            doc.push_str(filler);
        }
        doc.push_str("Run the exact `ctx_read` command below:\n\n");
        doc.push_str(
            "```bash\nlean-ctx read src/lib.rs\n\nlean-ctx search \"ctx_read\" src/\n```\n",
        );
        for _ in 0..30 {
            doc.push_str(filler);
        }

        let compacted = compact_markdown(&doc).expect("doc should compact");

        // The fence must never be split: fences stay balanced and no omission
        // marker may appear inside a block.
        let mut in_fence = false;
        for line in compacted.lines() {
            if line.trim_start().starts_with("```") {
                in_fence = !in_fence;
            } else if in_fence {
                assert!(
                    !line.starts_with("... [lean-ctx:"),
                    "omission marker inside a fenced block:\n{compacted}"
                );
            }
        }
        assert!(!in_fence, "unbalanced code fences:\n{compacted}");

        // This block is high-signal, so it must survive whole — including its
        // blank interior line (kept fences are verbatim).
        assert!(compacted.contains(
            "```bash\nlean-ctx read src/lib.rs\n\nlean-ctx search \"ctx_read\" src/\n```"
        ));
        assert!(compacted.contains("[lean-ctx: omitted"));
    }

    #[test]
    fn heading_inside_fence_is_not_structure() {
        // A `# comment` inside a code block is code, not a heading: it must not
        // be force-kept or start a new section.
        let mut doc = String::from("# Real Heading\n\nIntro.\n\n");
        doc.push_str("```sh\n# just a shell comment\necho done\n```\n");
        for i in 0..30 {
            doc.push_str(&format!("Body filler sentence number {i} for volume.\n"));
        }

        let compacted = compact_markdown(&doc).expect("doc should compact");
        let fences = compacted.matches("```").count();
        assert_eq!(fences % 2, 0, "fences must stay balanced:\n{compacted}");
    }

    #[test]
    fn has_markdown_headings_requires_atx_space() {
        assert!(has_markdown_headings("# Title\nbody\n"));
        assert!(has_markdown_headings("###\nempty heading is valid\n"));
        assert!(!has_markdown_headings("#!/usr/bin/env bash\necho hi\n"));
        assert!(!has_markdown_headings("#pragma once\nplain text\n"));
        assert!(!has_markdown_headings("- a list\n- alone\n"));
    }
}
