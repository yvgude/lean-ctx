// SPDX-License-Identifier: Apache-2.0
//! Token budget for the `signatures`/`map` outline of a large file.
//!
//! Pilot P10: the outline of a 4,188-line Python module (≈260 top-level
//! decoders) was 12–13k chars, and it came back on every unranged read, every
//! `map`/`signatures` read and every missed `symbol=`. Above
//! [`OUTLINE_BUDGET_TOKENS`] the outline is compacted in deterministic steps,
//! stopping at the first that fits:
//!
//! 1. runs of ≥3 consecutive same-kind symbols sharing a name prefix become one
//!    line (`fn decode_cart_{created,paid,voided}(payload) @L206-327`): every
//!    name stays visible, the per-symbol spans fold into the run's span;
//! 2. the same with the shorter prefix (up to the first `_`);
//! 3. nested members (methods) collapse into a count on their parent;
//! 4. the remaining lines are cut at the budget, with the count and the span
//!    of what was cut.
//!
//! A one-line note says what was compacted and how to get the detail.

use std::collections::HashMap;

use crate::core::signatures::Signature;
use crate::core::tokens::count_tokens;

/// Outline entries above this many tokens are compacted.
pub(crate) const OUTLINE_BUDGET_TOKENS: usize = 1_500;

/// Shortest run of prefix-sharing symbols that folds into one line.
const MIN_RUN: usize = 3;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Prefix {
    /// Up to and including the last `_` (`decode_cart_`).
    Long,
    /// Up to and including the first `_` (`decode_`).
    Short,
}

/// Render `sigs` as outline lines (without leading indentation). Returns the
/// lines and, when the budget forced a compaction, the note to append.
pub(crate) fn outline_lines(
    sigs: &[&Signature],
    tdd: bool,
    health: &HashMap<String, String>,
) -> (Vec<String>, Option<String>) {
    let render = |sig: &Signature, annotate: bool| {
        let mut line = if tdd {
            sig.to_tdd_located()
        } else {
            sig.to_compact_located()
        };
        if annotate && let Some(note) = health.get(&sig.name) {
            line.push_str("  ");
            line.push_str(note);
        }
        line
    };
    let plain: Vec<String> = sigs.iter().map(|s| render(s, true)).collect();
    if tokens(&plain) <= OUTLINE_BUDGET_TOKENS {
        return (plain, None);
    }

    let owned: Vec<Signature> = sigs.iter().map(|s| (*s).clone()).collect();
    let mut steps = vec!["names grouped by prefix"];
    let mut lines = grouped(&owned, Prefix::Long, &render);
    if tokens(&lines) > OUTLINE_BUDGET_TOKENS {
        lines = grouped(&owned, Prefix::Short, &render);
    }
    if tokens(&lines) > OUTLINE_BUDGET_TOKENS && owned.iter().any(|s| s.indent > 0) {
        steps.push("members collapsed");
        lines = grouped(&collapse_members(&owned), Prefix::Short, &render);
    }
    let mut omitted = None;
    if tokens(&lines) > OUTLINE_BUDGET_TOKENS {
        let (kept, rest) = cut(lines);
        lines = kept;
        omitted = Some(rest);
    }
    if let Some((count, span)) = &omitted {
        lines.push(format!("… {count} more lines{span}"));
    }
    let note = format!(
        "[outline over ~{OUTLINE_BUDGET_TOKENS} tok: {} symbols, {}{} — symbol=\"Name\" reads one definition, mode=\"lines:N-M\" a range]",
        sigs.len(),
        steps.join(", "),
        if omitted.is_some() { ", tail cut" } else { "" },
    );
    (lines, Some(note))
}

fn tokens(lines: &[String]) -> usize {
    count_tokens(&lines.join("\n"))
}

fn prefix(name: &str, kind: Prefix) -> Option<&str> {
    let at = match kind {
        Prefix::Long => name.rfind('_')?,
        Prefix::Short => name.find('_')?,
    };
    (at > 0 && at + 1 < name.len()).then(|| &name[..=at])
}

/// One line per symbol, except runs of [`MIN_RUN`]+ consecutive symbols of
/// the same shape and name prefix, which fold into one line.
fn grouped(
    sigs: &[Signature],
    kind: Prefix,
    render: &dyn Fn(&Signature, bool) -> String,
) -> Vec<String> {
    let shape = |s: &Signature| (s.kind, s.indent, s.is_exported, s.is_async);
    let mut lines = Vec::new();
    let mut i = 0;
    while i < sigs.len() {
        let head = &sigs[i];
        let mut end = i + 1;
        if let Some(p) = prefix(&head.name, kind) {
            while end < sigs.len()
                && shape(&sigs[end]) == shape(head)
                && prefix(&sigs[end].name, kind) == Some(p)
            {
                end += 1;
            }
        }
        if end - i >= MIN_RUN {
            lines.push(render(&fold(&sigs[i..end], kind), false));
        } else {
            for sig in &sigs[i..end] {
                lines.push(render(sig, true));
            }
        }
        i = end;
    }
    lines
}

/// The one signature standing for a run: `prefix{suffix,…}`, the shared
/// params/return type (or `…` where they differ), the run's whole span.
fn fold(run: &[Signature], kind: Prefix) -> Signature {
    let head = &run[0];
    let p = prefix(&head.name, kind).unwrap_or_default();
    let suffixes: Vec<&str> = run.iter().map(|s| &s.name[p.len()..]).collect();
    let shared = |field: fn(&Signature) -> &String| {
        let first = field(head);
        if run.iter().all(|s| field(s) == first) {
            first.clone()
        } else {
            "…".to_string()
        }
    };
    let mut folded = head.clone();
    folded.name = format!("{p}{{{}}}", suffixes.join(","));
    folded.params = shared(|s| &s.params);
    folded.return_type = shared(|s| &s.return_type);
    folded.start_line = run.iter().filter_map(|s| s.start_line).min();
    folded.end_line = run.iter().filter_map(|s| s.end_line.or(s.start_line)).max();
    folded
}

/// Drop nested members; their parent's name carries the count.
fn collapse_members(sigs: &[Signature]) -> Vec<Signature> {
    let mut out: Vec<Signature> = Vec::new();
    let mut nested = 0usize;
    let flush = |out: &mut Vec<Signature>, nested: &mut usize| {
        if *nested > 0
            && let Some(parent) = out.last_mut()
        {
            parent.name = format!("{} (+{nested} members)", parent.name);
        }
        *nested = 0;
    };
    for sig in sigs {
        if sig.indent > 0 && !out.is_empty() {
            nested += 1;
            continue;
        }
        flush(&mut out, &mut nested);
        out.push(sig.clone());
    }
    flush(&mut out, &mut nested);
    out
}

/// Keep lines while they fit the budget; report how many were cut and the
/// first line number they start at (from the first cut line's `@L`).
fn cut(lines: Vec<String>) -> (Vec<String>, (usize, String)) {
    let mut kept = Vec::new();
    let mut used = 0usize;
    let mut iter = lines.into_iter();
    let mut first_cut = None;
    for line in iter.by_ref() {
        let cost = count_tokens(&line) + 1;
        if used + cost > OUTLINE_BUDGET_TOKENS {
            first_cut = Some(line);
            break;
        }
        used += cost;
        kept.push(line);
    }
    let rest = iter.count() + usize::from(first_cut.is_some());
    let span = first_cut
        .as_deref()
        .and_then(|l| l.rfind("@L").map(|at| &l[at..]))
        .and_then(|s| s.split_whitespace().next())
        .map(|s| format!(" from {}", s.split('-').next().unwrap_or(s)))
        .unwrap_or_default();
    (kept, (rest, span))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(name: &str, indent: usize, line: usize) -> Signature {
        Signature {
            kind: "fn",
            name: name.to_string(),
            params: "payload".to_string(),
            return_type: String::new(),
            is_async: false,
            is_exported: true,
            indent,
            start_line: Some(line),
            end_line: Some(line + 9),
        }
    }

    fn decoders(entities: usize, verbs: usize) -> Vec<Signature> {
        let mut sigs = Vec::new();
        for e in 0..entities {
            for v in 0..verbs {
                sigs.push(sig(
                    &format!("decode_entity{e}_verb{v}"),
                    0,
                    10 * (e * verbs + v) + 1,
                ));
            }
        }
        sigs
    }

    #[test]
    fn small_outline_is_untouched() {
        let sigs = decoders(2, 5);
        let refs: Vec<&Signature> = sigs.iter().collect();
        let (lines, note) = outline_lines(&refs, false, &HashMap::new());
        assert!(note.is_none());
        assert_eq!(lines.len(), 10);
        assert_eq!(lines[0], "fn pub decode_entity0_verb0(payload) @L1-10");
    }

    /// P10 shape: hundreds of prefix-sharing decoders. Every name survives,
    /// the result fits the budget, and the same input gives the same bytes.
    #[test]
    fn large_outline_groups_by_prefix_within_budget() {
        let sigs = decoders(26, 10);
        let refs: Vec<&Signature> = sigs.iter().collect();
        let (lines, note) = outline_lines(&refs, false, &HashMap::new());
        assert!(
            tokens(&lines) <= OUTLINE_BUDGET_TOKENS,
            "{}",
            lines.join("\n")
        );
        assert_eq!(lines.len(), 26);
        assert_eq!(
            lines[1],
            "fn pub decode_entity1_{verb0,verb1,verb2,verb3,verb4,verb5,verb6,verb7,verb8,verb9}(payload) @L101-200"
        );
        assert!(note.unwrap().contains("260 symbols"));
        assert_eq!(outline_lines(&refs, false, &HashMap::new()).0, lines);
    }

    /// Without shared prefixes, members collapse and the tail is cut with a
    /// pointer to where the cut starts.
    #[test]
    fn unrelated_names_collapse_members_then_cut() {
        let mut sigs = Vec::new();
        for c in 0..400 {
            sigs.push(sig(&format!("Class{c}"), 0, c * 100 + 1));
            sigs.push(sig(&format!("method{c}"), 4, c * 100 + 2));
        }
        let refs: Vec<&Signature> = sigs.iter().collect();
        let (lines, note) = outline_lines(&refs, false, &HashMap::new());
        let note = note.unwrap();
        assert!(
            note.contains("members collapsed") && note.contains("tail cut"),
            "{note}"
        );
        assert!(lines[0].contains("Class0 (+1 members)"), "{}", lines[0]);
        let last = lines.last().unwrap();
        assert!(
            last.starts_with("… ") && last.contains(" more lines from @L"),
            "{last}"
        );
        assert!(tokens(&lines) <= OUTLINE_BUDGET_TOKENS + 20);
    }
}
