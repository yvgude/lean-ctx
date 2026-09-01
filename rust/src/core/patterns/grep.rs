use std::collections::HashMap;

use crate::core::tokens::count_tokens;

fn normalize_shell_tokens(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One rendered line: a match, or a context line around one.
#[derive(Clone, Copy)]
struct Entry<'a> {
    line_num: usize,
    content: &'a str,
    is_match: bool,
}

pub fn compress(output: &str) -> Option<String> {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() < 3 {
        return None;
    }

    let mut by_file: HashMap<&str, Vec<Entry<'_>>> = HashMap::new();
    let mut total_matches = 0usize;

    // Pass 1 — matches only (`path:line:content`).
    for line in &lines {
        if let Some((file, line_num, content)) = parse_match_line(line) {
            total_matches += 1;
            by_file.entry(file).or_default().push(Entry {
                line_num,
                content,
                is_match: true,
            });
        }
    }

    if total_matches == 0 {
        return None;
    }

    // Pass 2 — context (`path-line-content`), resolved against the paths pass 1
    // already proved exist (GH #1648).
    //
    // Doing it this way, rather than scanning for a `-digits-` delimiter, is
    // what makes it unambiguous: both a path and the context text may contain
    // hyphens and digits, but grep only emits context for a file it also
    // matched in, so the path is always one we have already seen.
    let known: Vec<&str> = by_file.keys().copied().collect();
    for line in &lines {
        if parse_match_line(line).is_some() {
            continue;
        }
        if let Some((file, line_num, content)) = parse_context_line(line, &known) {
            by_file.entry(file).or_default().push(Entry {
                line_num,
                content,
                is_match: false,
            });
        }
        // Anything else — grep's `--` group separators, stray text — is
        // dropped. It used to become a synthetic file with empty content,
        // inflating both the file and the match count.
    }

    let max_matches_per_file = if total_matches > 200 { 5 } else { 10 };

    let mut result = format!("{total_matches} matches in {}F:\n", by_file.len());
    let mut sorted_files: Vec<_> = by_file.iter_mut().collect();
    sorted_files.sort_by(|a, b| {
        let am = a.1.iter().filter(|e| e.is_match).count();
        let bm = b.1.iter().filter(|e| e.is_match).count();
        bm.cmp(&am).then_with(|| a.0.cmp(b.0))
    });

    for (file, entries) in &mut sorted_files {
        entries.sort_by_key(|e| (e.line_num, !e.is_match));
        let match_count = entries.iter().filter(|e| e.is_match).count();
        let short = shorten_path(file);
        result.push_str(&format!("\n{short} ({match_count}):"));

        // The cap counts *matches*, and any context around a shown match comes
        // with it — capping raw lines would silently drop matches once context
        // was requested.
        let mut shown_matches = 0usize;
        for entry in entries.iter() {
            if entry.is_match {
                if shown_matches == max_matches_per_file {
                    break;
                }
                shown_matches += 1;
            }
            let trimmed = entry.content.trim();
            let short_content = if trimmed.len() > 120 {
                let truncated: String = trimmed.chars().take(119).collect();
                format!("{truncated}…")
            } else {
                trimmed.to_string()
            };
            // grep's own convention: `:` marks a match, `-` marks context.
            let sep = if entry.is_match { ':' } else { '-' };
            if entry.line_num > 0 {
                result.push_str(&format!("\n  {}{sep} {short_content}", entry.line_num));
            } else {
                result.push_str(&format!("\n  {short_content}"));
            }
        }
        if match_count > shown_matches {
            result.push_str(&format!("\n  ... +{} more", match_count - shown_matches));
        }
    }

    let out_n = normalize_shell_tokens(output);
    let res_n = normalize_shell_tokens(&result);
    let ct_r = count_tokens(&res_n);
    let ct_o = count_tokens(&out_n);
    if ct_r >= ct_o && !(ct_r == ct_o && res_n.len() < out_n.len()) {
        return None;
    }

    Some(result)
}

/// Parse a match line: `path:line:content`.
///
/// The delimiter is the first colon *followed by digits and another colon* —
/// not simply the first colon on the line (GH #1648). The old rule split
/// `…/mod_1.py-2-    try:` at the colon inside the Python source, inventing a
/// file named after the whole fragment.
fn parse_match_line(line: &str) -> Option<(&str, usize, &str)> {
    // Skip a Windows drive letter (e.g. "C:" at position 1).
    let start = if line.len() >= 2
        && line.as_bytes()[0].is_ascii_alphabetic()
        && line.as_bytes()[1] == b':'
    {
        2
    } else {
        0
    };

    let (file, line_num, content) = split_numbered(line, ':', start)?;
    if file.contains('/') || file.contains('\\') || file.contains('.') {
        Some((file, line_num, content))
    } else {
        None
    }
}

/// Split `prefix<D><digits><D>rest` at the first delimiter that is actually
/// followed by a line number.
fn split_numbered(line: &str, delim: char, start: usize) -> Option<(&str, usize, &str)> {
    let bytes = line.as_bytes();
    let mut from = start;
    while let Some(rel) = line[from..].find(delim) {
        let at = from + rel;
        let after = at + delim.len_utf8();
        let digits = bytes[after..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        if digits > 0 && bytes.get(after + digits).copied() == Some(delim as u8) {
            let num = line[after..after + digits].parse().ok()?;
            return Some((&line[..at], num, &line[after + digits + 1..]));
        }
        from = at + delim.len_utf8();
    }
    None
}

/// Parse a context line: `path-line-content`, where `path` is one grep already
/// reported a match in.
///
/// Anchoring on a known path rather than scanning for `-digits-` is what keeps
/// this unambiguous: `src/v-1-x/mod.py-10-ctx` has three candidate delimiters
/// and only one correct answer.
fn parse_context_line<'a>(line: &'a str, known: &[&'a str]) -> Option<(&'a str, usize, &'a str)> {
    for file in known {
        let Some(rest) = line.strip_prefix(*file).and_then(|r| r.strip_prefix('-')) else {
            continue;
        };
        let digits = rest
            .as_bytes()
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        if digits > 0 && rest.as_bytes().get(digits).copied() == Some(b'-') {
            let num = rest[..digits].parse().ok()?;
            return Some((file, num, &rest[digits + 1..]));
        }
    }
    None
}

fn shorten_path(path: &str) -> &str {
    path.strip_prefix("./").unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_grep_output_is_not_claimed_without_matches() {
        assert!(compress("hello\nworld").is_none());
    }

    #[test]
    fn small_grep_output_still_compresses() {
        let output = (0..20)
            .map(|i| format!("src/main.rs:{i}: let x = {i};"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = compress(&output);
        assert!(result.is_some());
        let compressed = result.unwrap();
        assert!(
            compressed.contains("20 matches in 1F:"),
            "should group by file: {compressed}"
        );
        assert!(
            count_tokens(&compressed) < count_tokens(&output),
            "should compress: {} vs {}",
            count_tokens(&compressed),
            count_tokens(&output)
        );
    }

    #[test]
    fn large_output_reduces_per_file_lines() {
        let mut lines = Vec::new();
        for i in 0..250 {
            lines.push(format!("src/a.rs:{i}: line content {i}"));
        }
        let output = lines.join("\n");
        let result = compress(&output).unwrap();
        assert!(
            result.contains("... +245 more"),
            "should show +more for large output: {result}"
        );
    }

    #[test]
    fn non_grep_output_returns_none() {
        let output = "no file:line pattern here\njust regular text\nmore text\nand more";
        assert!(compress(output).is_none());
    }

    #[test]
    fn tiny_grep_output_returns_none_if_inflation() {
        let output = "a.rs:1:x\nb.rs:2:y\nc.rs:3:z\n";
        let result = compress(output);
        if let Some(ref compressed) = result {
            assert!(
                count_tokens(compressed) < count_tokens(output),
                "must never inflate: compressed={} vs original={}",
                count_tokens(compressed),
                count_tokens(output)
            );
        }
    }

    #[test]
    fn multi_file_many_matches_compresses_well() {
        let mut lines = Vec::new();
        for i in 0..50 {
            lines.push(format!(
                "src/models/user.rs:{}: pub fn method_{i}() {{}}",
                i + 1
            ));
        }
        for i in 0..30 {
            lines.push(format!(
                "src/controllers/auth.rs:{}: let val = method_{i}();",
                i + 1
            ));
        }
        let output = lines.join("\n");
        let result = compress(&output).expect("80 matches should compress");
        assert!(
            count_tokens(&result) < count_tokens(&output),
            "must compress: {} vs {}",
            count_tokens(&result),
            count_tokens(&output)
        );
        assert!(result.contains("80 matches in 2F:"));
        assert!(result.contains("src/models/user.rs (50):"));
        assert!(result.contains("src/controllers/auth.rs (30):"));
    }

    #[test]
    fn many_single_match_files_falls_back_to_none() {
        let lines: Vec<String> = (1..=30)
            .map(|i| format!("src/file{i}.rs:42: fn search_result()"))
            .collect();
        let output = lines.join("\n");
        let result = compress(&output);
        if let Some(ref c) = result {
            assert!(
                count_tokens(c) < count_tokens(&output),
                "if claimed, must be shorter in tokens: {} vs {}",
                count_tokens(c),
                count_tokens(&output)
            );
        }
    }

    #[test]
    fn never_returns_inflated_output() {
        for count in [3, 5, 10, 15, 25, 50] {
            let lines: Vec<String> = (0..count).map(|i| format!("f{i}.rs:{i}:x")).collect();
            let output = lines.join("\n");
            if let Some(ref c) = compress(&output) {
                assert!(
                    count_tokens(c) < count_tokens(&output),
                    "count={count}: inflated {} vs {}",
                    count_tokens(c),
                    count_tokens(&output)
                );
            }
        }
    }

    // --- GH #1648: context lines (`path-line-text`) are not matches ---

    /// The reporter's repro, reduced: 40 files, one match each, `-C 2`.
    /// GNU grep separates a match with `:` and context with `-`; the parser
    /// took the FIRST colon anywhere on the line, so `…/mod_1.py-2-    try:`
    /// became a *file* named `…/mod_1.py-2-    try` with empty content —
    /// counted as both a file and a match.
    fn repro_with_context(files: usize) -> String {
        let mut out = Vec::new();
        for i in 1..=files {
            let f = format!("/tmp/leanctx-grep-repro/mod_{i}.py");
            out.push(format!("{f}-1-def handler_{i}():"));
            out.push(format!("{f}-2-    try:"));
            out.push(format!("{f}:3:        target_pattern_here()"));
            out.push(format!("{f}-4-    except Exception:"));
            out.push(format!("{f}-5-        pass"));
            out.push("--".to_string());
        }
        out.join("\n")
    }

    #[test]
    fn context_lines_do_not_become_files_or_matches() {
        let result = compress(&repro_with_context(40)).expect("should compress");
        assert!(
            result.starts_with("40 matches in 40F:"),
            "40 real matches in 40 files, not one per context line: {}",
            result.lines().next().unwrap_or("")
        );
        assert!(
            !result.contains("-2-"),
            "a context line must not appear as a file header: {result}"
        );
        assert!(
            !result.contains("try (1)"),
            "text must not be truncated at a colon into a path: {result}"
        );
    }

    /// The content the reporter lost. A context line whose text contains a
    /// colon (Python, YAML, Go tags, prose) is exactly the case that broke.
    #[test]
    fn context_text_survives_intact() {
        let result = compress(&repro_with_context(40)).expect("should compress");
        assert!(
            result.contains("try:"),
            "the context line's own text, colon included, must survive: {result}"
        );
    }

    /// `--` group separators are grep's, not data.
    #[test]
    fn group_separators_are_ignored() {
        let out = (1..=20)
            .map(|i| format!("a/b.rs-{}-ctx\na/b.rs:{i}:hit\n--", i * 10))
            .collect::<Vec<_>>()
            .join("\n");
        let result = compress(&out).expect("should compress");
        assert!(result.starts_with("20 matches in 1F:"), "{result}");
    }

    /// A path containing `-<digits>-` must not be mistaken for a context
    /// delimiter. Context is resolved against paths already seen in match
    /// lines, so the ambiguity never arises.
    #[test]
    fn a_hyphenated_path_is_not_split_as_context() {
        let out = (1..=20)
            .map(|i| format!("src/v-1-x/mod.py-{}-ctx\nsrc/v-1-x/mod.py:{i}:hit", i * 10))
            .collect::<Vec<_>>()
            .join("\n");
        let result = compress(&out).expect("should compress");
        assert!(
            result.starts_with("20 matches in 1F:"),
            "one file, not one per hyphen: {result}"
        );
    }
}
