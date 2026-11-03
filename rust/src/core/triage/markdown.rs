//! Markdown-aware keep rules for post-read triage.
//!
//! Level 2 is built for Rust: it keeps `pub`/`fn`/`{}` and lines that start with
//! `#` (attributes). On prose that means ATX headings survive and every body
//! paragraph is deleted — `ctx_read(full)` on a vision doc becomes a TOC.
//!
//! Markdown documents instead keep the section lead, lists, tables, and fences.

use std::collections::HashSet;

pub fn looks_like_markdown(lines: &[&str]) -> bool {
    let sample = lines.iter().take(120);
    let mut headings = 0u32;
    let mut codeish = 0u32;
    for line in sample {
        let start = line.trim_start();
        if is_atx_heading(start) {
            headings += 1;
        }
        if is_code_signal(start) {
            codeish += 1;
        }
    }
    headings >= 3 && headings > codeish
}

pub fn keep_indices(lines: &[&str], keywords: &[String]) -> HashSet<usize> {
    let mut keep = HashSet::new();
    let mut in_fence = false;
    let mut lead = Lead::Waiting;

    for (i, line) in lines.iter().enumerate() {
        let start = line.trim_start();
        let empty = start.is_empty();

        if start.starts_with("```") {
            in_fence = !in_fence;
            keep.insert(i);
            lead = Lead::Done;
            continue;
        }
        if in_fence {
            keep.insert(i);
            continue;
        }
        if is_atx_heading(start) {
            keep.insert(i);
            lead = Lead::Waiting;
            continue;
        }
        if is_markdown_block(start) {
            keep.insert(i);
            continue;
        }
        if keyword_hit(start, keywords) {
            keep.insert(i);
            continue;
        }
        match lead {
            Lead::Waiting if empty => {}
            Lead::Waiting => {
                keep.insert(i);
                lead = Lead::Capturing;
            }
            Lead::Capturing if empty => {
                lead = Lead::Done;
            }
            Lead::Capturing => {
                keep.insert(i);
            }
            Lead::Done => {}
        }
    }
    keep
}

pub fn is_heading_only_collapse(lines: &[&str], keep: &HashSet<usize>) -> bool {
    !keep.is_empty()
        && keep.iter().all(|&i| {
            lines
                .get(i)
                .is_none_or(|line| is_atx_heading(line.trim_start()))
        })
}

#[derive(Clone, Copy)]
enum Lead {
    Waiting,
    Capturing,
    Done,
}

fn is_atx_heading(line: &str) -> bool {
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    (1..=6).contains(&hashes) && line.as_bytes().get(hashes) == Some(&b' ')
}

fn is_code_signal(line: &str) -> bool {
    line.starts_with("pub ")
        || line.starts_with("fn ")
        || line.starts_with("struct ")
        || line.starts_with("impl ")
        || line.starts_with("use ")
        || line.starts_with("mod ")
}

fn is_markdown_block(line: &str) -> bool {
    line.starts_with("> ")
        || line.starts_with('|')
        || line.starts_with("- ")
        || line.starts_with("* ")
        || line.starts_with("+ ")
        || is_ordered_list(line)
}

fn is_ordered_list(line: &str) -> bool {
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && line.get(digits..digits.saturating_add(2)) == Some(". ")
}

fn keyword_hit(line: &str, keywords: &[String]) -> bool {
    if keywords.is_empty() {
        return false;
    }
    let lower = line.to_lowercase();
    keywords.iter().any(|keyword| lower.contains(keyword))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_vision_markdown_not_rust() {
        let md = [
            "# LeanCTX: The Context SDK",
            "",
            "## The loop",
            "Connect then measure.",
            "## Receipt",
            "Evidence, not a log.",
        ];
        assert!(looks_like_markdown(&md));

        let rust = [
            "use crate::core::triage::profile::TaskProfileLocal;",
            "pub fn apply_triage_filter() {}",
            "impl Foo {",
            "    fn bar() {}",
            "}",
            "mod tests {}",
        ];
        assert!(!looks_like_markdown(&rust));
    }

    #[test]
    fn keeps_heading_and_first_paragraph_not_later_filler() {
        let lines = [
            "# Product",
            "",
            "Context shapes performance.",
            "This second sentence stays in the lead paragraph.",
            "",
            "Later filler about racing aesthetics must drop.",
            "## Loop",
            "Connect, measure, tune, prove.",
            "",
            "Ignore this extra rationale.",
        ];
        let keep = keep_indices(&lines, &[]);
        let kept: Vec<_> = keep.iter().copied().collect();
        let text: Vec<_> = kept.iter().map(|&i| lines[i]).collect();
        assert!(
            text.iter()
                .any(|l| l.contains("Context shapes performance"))
        );
        assert!(text.iter().any(|l| l.contains("second sentence")));
        assert!(text.iter().any(|l| l.contains("Connect, measure")));
        assert!(!text.iter().any(|l| l.contains("Later filler")));
        assert!(!text.iter().any(|l| l.contains("extra rationale")));
    }

    #[test]
    fn keeps_lists_and_tables() {
        let lines = [
            "# Boundary",
            "Manual tuning is open.",
            "",
            "- Local runtime",
            "- Local receipt",
            "",
            "| Capability | OSS |",
            "| Runtime | yes |",
        ];
        let keep = keep_indices(&lines, &[]);
        assert!(keep.contains(&3));
        assert!(keep.contains(&4));
        assert!(keep.contains(&6));
        assert!(keep.contains(&7));
    }

    #[test]
    fn heading_only_collapse_is_detected() {
        let lines = ["# A", "## B", "Body that would be dropped.", "## C"];
        let mut keep = HashSet::new();
        keep.insert(0);
        keep.insert(1);
        keep.insert(3);
        assert!(is_heading_only_collapse(&lines, &keep));
    }
}
