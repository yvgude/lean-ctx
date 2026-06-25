//! Adaptive chunk sizing from a rough “prefrontal” budget controller (tight → signatures, generous → full bodies).
use crate::core::tokens::count_tokens;

/// One slice of source text chosen for inclusion.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkResult {
    pub content: String,
    pub start_line: usize,
    pub end_line: usize,
    pub priority: f64,
}

const TIGHT_PER_ITEM: usize = 50;
const GENEROUS_PER_ITEM: usize = 200;

fn is_fn_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("fn ")
        || t.starts_with("pub fn ")
        || t.starts_with("async fn ")
        || t.starts_with("pub async fn ")
        || t.starts_with("unsafe fn ")
        || t.starts_with("pub unsafe fn ")
        || t.starts_with("pub(crate) fn ")
}

fn chunk_ranges(lines: &[&str]) -> Vec<(usize, usize)> {
    let n = lines.len();
    if n == 0 {
        return Vec::new();
    }
    let starts: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, l)| is_fn_line(l).then_some(i))
        .collect();
    if starts.is_empty() {
        return vec![(0, n - 1)];
    }
    let mut ranges = Vec::new();
    if starts[0] > 0 {
        ranges.push((0, starts[0] - 1));
    }
    for (k, &s) in starts.iter().enumerate() {
        let end = if k + 1 < starts.len() {
            starts[k + 1] - 1
        } else {
            n - 1
        };
        ranges.push((s, end));
    }
    ranges
}

fn import_hits(lines: &[&str]) -> usize {
    lines
        .iter()
        .filter(|l| {
            let t = l.trim_start();
            t.starts_with("use ") || t.starts_with("import ")
        })
        .count()
}

fn brace_complexity(text: &str) -> f64 {
    let mut depth = 0i32;
    let mut maxd = 0i32;
    for c in text.chars() {
        match c {
            '{' | '(' | '[' => {
                depth += 1;
                maxd = maxd.max(depth);
            }
            '}' | ')' | ']' => {
                depth -= 1;
            }
            _ => {}
        }
    }
    let kw = ["for ", "while ", "match ", "loop ", "if ", "else"];
    let mut kc = 0.0_f64;
    for k in kw {
        kc += text.matches(k).count() as f64;
    }
    f64::from(maxd) * 0.18 + kc * 0.06
}

fn build_chunk_body(lines: &[&str], start: usize, end: usize) -> String {
    lines[start..=end].join("\n")
}

/// Extract a compact signature-oriented prefix (first lines until `{` or trailing `;`).
fn signature_body(lines: &[&str], start: usize, end: usize) -> String {
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate().take(end + 1).skip(start) {
        out.push_str(line);
        if i < end {
            out.push('\n');
        }
        if line.contains('{') || line.trim_end().ends_with(';') {
            break;
        }
        if out.lines().count() >= 6 {
            break;
        }
    }
    out.trim_end().to_string()
}

fn chunk_priority(lines: &[&str], start: usize, end: usize, total_lines: usize) -> f64 {
    let slice = &lines[start..=end];
    let body = slice.join("\n");
    let cx = brace_complexity(&body);
    let im = import_hits(slice) as f64 * 0.12;
    let denom = total_lines.max(1) as f64;
    let recency = (usize::midpoint(start, end) + 1) as f64 / denom * 0.55;
    (cx + im + recency).min(12.0)
}

fn proportional_body(lines: &[&str], start: usize, end: usize, target_tokens: usize) -> String {
    let full = build_chunk_body(lines, start, end);
    let ftoks = count_tokens(&full).max(1);
    let frac = (target_tokens as f64 / ftoks as f64).clamp(0.12, 1.0);
    let nlines = end - start + 1;
    let take = ((nlines as f64 * frac).ceil() as usize).clamp(1, nlines);
    lines[start..start + take].join("\n")
}

/// Split `content` into prioritized chunks sized to `budget_tokens` spread across `total_items` sibling slices.
#[must_use]
pub fn adaptive_chunk(content: &str, budget_tokens: usize, total_items: usize) -> Vec<ChunkResult> {
    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len().max(1);
    let ranges = chunk_ranges(&lines);
    let per_item = budget_tokens / total_items.max(1);

    let mut raw: Vec<(usize, usize, f64)> = ranges
        .into_iter()
        .map(|(s, e)| {
            let p = chunk_priority(&lines, s, e, total_lines);
            (s, e, p)
        })
        .collect();

    if raw.is_empty() {
        return Vec::new();
    }

    raw.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

    let mut results = Vec::new();

    if per_item < TIGHT_PER_ITEM {
        let mut used = 0usize;
        for (s, e, pri) in raw {
            let body = signature_body(&lines, s, e);
            if body.is_empty() {
                continue;
            }
            let t = count_tokens(&body);
            if used + t > budget_tokens {
                continue;
            }
            used += t;
            results.push(ChunkResult {
                content: body,
                start_line: s + 1,
                end_line: e + 1,
                priority: pri,
            });
        }
        results.sort_by(|a, b| {
            a.start_line.cmp(&b.start_line).then_with(|| {
                b.priority
                    .partial_cmp(&a.priority)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        });
        return results;
    }

    if per_item > GENEROUS_PER_ITEM {
        for (s, e, pri) in raw {
            let body = build_chunk_body(&lines, s, e);
            results.push(ChunkResult {
                content: body,
                start_line: s + 1,
                end_line: e + 1,
                priority: pri,
            });
        }
        results.sort_by_key(|c| c.start_line);
        return results;
    }

    // Middle: proportional inclusion per chunk, processed in priority order but output sorted by line.
    let mut tmp = Vec::new();
    for (s, e, pri) in raw {
        let body = proportional_body(&lines, s, e, per_item);
        tmp.push(ChunkResult {
            content: body,
            start_line: s + 1,
            end_line: e + 1,
            priority: pri,
        });
    }
    tmp.sort_by_key(|c| c.start_line);
    tmp
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"use std::io;

fn foo() {
    if true {
        println!("a");
    }
}

fn bar(x: i32) -> i32 {
    let mut s = 0;
    for i in 0..x {
        s += i;
    }
    s
}
"#;

    #[test]
    fn tight_mode_prefers_signatures_and_respects_budget() {
        let chunks = adaptive_chunk(SAMPLE, 80, 4);
        assert!(!chunks.is_empty());
        for c in &chunks {
            assert!(!c.content.contains("println!"));
        }
        let tok_total: usize = chunks.iter().map(|c| count_tokens(&c.content)).sum();
        assert!(tok_total <= 80);
    }

    #[test]
    fn generous_mode_keeps_full_bodies() {
        let chunks = adaptive_chunk(SAMPLE, 50_000, 1);
        assert!(chunks.iter().any(|c| c.content.contains("println!")));
        assert!(chunks.iter().any(|c| c.content.contains("for i")));
    }

    #[test]
    fn middle_mode_partial_body() {
        let mut big = SAMPLE.to_string();
        big.push_str("\nfn baz() {\n");
        for i in 0..120 {
            big.push_str(&format!("    let _z{i} = {i};\n"));
        }
        big.push_str("}\n");
        // per-item budget ~75 tokens → proportional clipping on large fn body.
        let chunks = adaptive_chunk(&big, 750, 10);
        let baz = chunks
            .iter()
            .find(|c| c.content.contains("baz"))
            .expect("baz chunk");
        let baz_full_lines = big.lines().filter(|l| l.contains("_z")).count();
        let baz_kept_lines = baz.content.lines().filter(|l| l.contains("_z")).count();
        assert!(
            baz_kept_lines < baz_full_lines,
            "expected proportional truncation inside baz, kept={baz_kept_lines} full={baz_full_lines}"
        );
    }

    #[test]
    fn non_fn_file_single_chunk() {
        let t = "hello world\nline two\n";
        let chunks = adaptive_chunk(t, 500, 1);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].start_line, 1);
    }
}
