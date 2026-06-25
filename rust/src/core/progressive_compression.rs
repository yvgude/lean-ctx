//! Progressive compression: newest segments stay verbose; older tiers lose detail under exponential budget shrink.

use super::tokens::count_tokens;

fn truncate_to_token_budget(s: &str, max_tokens: usize) -> String {
    if max_tokens == 0 {
        return String::new();
    }
    if count_tokens(s) <= max_tokens {
        return s.to_string();
    }
    let mut lo = 0usize;
    let mut hi = s.len();
    while lo + 1 < hi {
        let mid = usize::midpoint(lo, hi);
        let pref = s.get(..mid).unwrap_or("");
        if count_tokens(pref) <= max_tokens {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let pref = s.get(..lo).unwrap_or("");
    format!("{pref} …")
}

fn map_like(s: &str, max_tokens: usize) -> String {
    let keywords = [
        "fn ", "pub ", "struct ", "enum ", "trait ", "impl ", "mod ", "use ", "def ", "class ",
    ];
    let mut picked: Vec<&str> = Vec::new();
    for (i, line) in s.lines().enumerate() {
        if i == 0 || keywords.iter().any(|k| line.contains(k)) {
            picked.push(line);
        }
        if picked.len() >= 48 {
            break;
        }
    }
    if picked.is_empty() {
        picked.push(s.lines().next().unwrap_or(""));
    }
    let draft = picked.join("\n");
    truncate_to_token_budget(&draft, max_tokens.max(4))
}

fn one_line_summary(segment_idx: usize, s: &str, max_tokens: usize) -> String {
    let preview = s
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(120)
        .collect::<String>();
    let draft = format!(
        "// seg[{segment_idx}] {} lines, {} chars | {preview}",
        s.lines().count(),
        s.len(),
    );
    truncate_to_token_budget(&draft, max_tokens.max(8))
}

fn tier_for_index(i: usize, n: usize) -> usize {
    if n <= 1 {
        return 2;
    }
    let r = i as f64 / (n.saturating_sub(1)) as f64;
    if r < 1.0 / 3.0 {
        0
    } else if r < 2.0 / 3.0 {
        1
    } else {
        2
    }
}

fn allocate_budget_chunks(budget_tokens: usize, w: &[f64]) -> Vec<usize> {
    let n = w.len();
    if n == 0 || budget_tokens == 0 {
        return vec![0; n];
    }
    let sum_w: f64 = w.iter().sum::<f64>().max(f64::EPSILON);
    let mut base = vec![0usize; n];
    let mut frac = vec![0.0_f64; n];
    for i in 0..n {
        let exact = budget_tokens as f64 * w[i] / sum_w;
        base[i] = exact.floor() as usize;
        frac[i] = exact - base[i] as f64;
    }
    let given: usize = base.iter().sum();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        frac[b]
            .partial_cmp(&frac[a])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut extra = budget_tokens.saturating_sub(given);
    for &i in &order {
        if extra == 0 {
            break;
        }
        base[i] += 1;
        extra -= 1;
    }
    base
}

fn exp_weights(n: usize) -> Vec<f64> {
    if n == 0 {
        return Vec::new();
    }
    let lambda = 1.35_f64;
    (0..n).map(|i| (lambda * i as f64).exp()).collect()
}

/// `segments[0]` is oldest, `segments[last]` newest. Budget follows exponential weights toward recent slices.
#[must_use]
pub fn compress_progressive(segments: &[String], budget_tokens: usize) -> Vec<String> {
    let n = segments.len();
    if n == 0 {
        return Vec::new();
    }
    if budget_tokens == 0 {
        return segments.iter().map(|_| String::new()).collect();
    }

    let w = exp_weights(n);
    let allocs = allocate_budget_chunks(budget_tokens, &w);

    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let alloc = allocs[i];

        let tier = tier_for_index(i, n);
        let seg = &segments[i];

        let compressed = if alloc == 0 {
            String::new()
        } else {
            match tier {
                2 => truncate_to_token_budget(seg, alloc),
                1 => map_like(seg, alloc),
                _ => one_line_summary(i, seg, alloc),
            }
        };

        let capped = if alloc == 0 {
            String::new()
        } else {
            truncate_to_token_budget(&compressed, alloc)
        };
        out.push(capped);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_segments() {
        assert!(compress_progressive(&[], 100).is_empty());
    }

    #[test]
    fn newest_more_verbose_than_oldest() {
        let mut segs = Vec::new();
        for i in 0..9 {
            let body = format!(
                "pub fn func_{i}(x: u32, y: &str) -> Option<()> {{ let z = x.wrapping_add({i}); Some(()) }}\n",
            );
            segs.push(body.repeat(4));
        }
        let budget = 5000usize;
        let out = compress_progressive(&segs, budget);
        assert_eq!(out.len(), segs.len());
        assert!(count_tokens(&out[0]) < count_tokens(&out[8]));
        assert!(
            out[0].starts_with("// seg[") || count_tokens(&out[0]) < 16,
            "oldest tier should be highly compressed"
        );
        assert!(out[8].contains("pub fn"));
    }

    #[test]
    fn respects_global_budget_order_of_magnitude() {
        let segs: Vec<String> = (0..4).map(|i| format!("line {i}\nabc\n")).collect();
        let out = compress_progressive(&segs, 80);
        let total: usize = out.iter().map(|s| count_tokens(s)).sum();
        assert!(total <= 80);
    }

    #[test]
    fn single_segment_full_path() {
        let one = vec!["hello world token budget".into()];
        let out = compress_progressive(&one, 50);
        assert_eq!(out.len(), 1);
        assert!(!out[0].is_empty());
    }
}
