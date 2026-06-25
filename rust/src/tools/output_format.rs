/// Compact aligned text output formatters for ctx_search and ctx_graph tools.
///
/// All functions produce plain text — no JSON, no serde, no external dependencies.

/// Format the header line for a tool output section.
///
/// The header uses Unicode box-drawing `───` borders and selects a template
/// based on the `action` name:
///
/// | action    | output structure                     |
/// |-----------|--------------------------------------|
/// | `"grep"`  | `N enriched / extra`                 |
/// | `"search"`| `N results (extra)`                  |
/// | `"reindex"`| `Reindexed: N files, 0 chunks`     |
/// | *default* | `N results`                          |
///
/// # Examples
///
/// ```
/// assert_eq!(
///     lean_ctx::tools::output_format::format_header("grep", 5, "4.0x dedup"),
///     "─── 5 enriched / 4.0x dedup ───",
/// );
/// ```
pub fn format_header(action: &str, total: usize, extra: &str) -> String {
    let body = match action {
        "grep" => format!("{} enriched / {}", total, extra),
        "search" => format!("{} results ({})", total, extra),
        "reindex" => format!("Reindexed: {} files, 0 chunks", total),
        _ => {
            if extra.is_empty() {
                format!("{} results", total)
            } else {
                format!("{} results ({})", total, extra)
            }
        }
    };
    format!("─── {} ───", body)
}

/// Format a single result row with aligned columns.
///
/// Layout: `  NNN  path:start-end  name  [label]  extra`
///
/// - `rank` is right-aligned to 3 characters.
/// - The range is always shown as `start-end` (even when equal).
/// - If `label` is empty it defaults to `"?"`.
///
/// # Examples
///
/// ```
/// assert_eq!(
///     lean_ctx::tools::output_format::format_row(1, "src/main.rs", 12, 45, "auth", "fn", "3 hits"),
///     "  1  src/main.rs:12-45  auth  [fn]  3 hits",
/// );
/// ```
pub fn format_row(
    rank: usize,
    file: &str,
    start_line: usize,
    end_line: usize,
    name: &str,
    label: &str,
    extra: &str,
) -> String {
    let label = if label.is_empty() { "?" } else { label };
    let location = format!("{}:{}-{}", file, start_line, end_line);
    format!("{:>3}  {}  {}  [{}]  {}", rank, location, name, label, extra)
}

/// Format pagination footer.
///
/// Produces a line like `  page 2/3 offset=20 limit=20`.
///
/// - `offset` is the zero-based start index of the current page.
/// - `limit` is the page size.
/// - `total` is the total number of results.
///
/// An empty result set always shows page 1/1.
///
/// # Examples
///
/// ```
/// assert_eq!(
///     lean_ctx::tools::output_format::format_footer(0, 20, 5),
///     "  page 1/1 offset=0 limit=20",
/// );
/// ```
pub fn format_footer(offset: usize, limit: usize, total: usize) -> String {
    let current_page = if limit == 0 {
        1
    } else {
        (offset / limit) + 1
    };
    let total_pages = if limit == 0 || total == 0 {
        1
    } else {
        (total + limit - 1) / limit
    };
    format!(
        "  page {}/{} offset={} limit={}",
        current_page, total_pages, offset, limit
    )
}

/// Format context source lines with match markers.
///
/// Each line is prefixed with `│ `. Lines whose 1-based index appears in
/// `match_lines` also get a trailing ` ←` marker.
///
/// Trailing empty lines from the source are ignored (matching the behaviour of
/// `str::lines()`).
///
/// # Examples
///
/// ```
/// assert_eq!(
///     lean_ctx::tools::output_format::format_context("line1\nline2\nline3\n", &[2]),
///     "│ line1\n│ line2 ←\n│ line3",
/// );
/// ```
pub fn format_context(source: &str, match_lines: &[usize]) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let lineno = i + 1; // 1-based
        if match_lines.contains(&lineno) {
            out.push_str(&format!("│ {} ←", line));
        } else {
            out.push_str(&format!("│ {}", line));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── format_header ────────────────────────────────────────────────────

    #[test]
    fn header_grep() {
        assert_eq!(
            format_header("grep", 5, "4.0x dedup"),
            "─── 5 enriched / 4.0x dedup ───"
        );
    }

    #[test]
    fn header_search() {
        assert_eq!(
            format_header("search", 3, "bm25"),
            "─── 3 results (bm25) ───"
        );
    }

    #[test]
    fn header_reindex() {
        assert_eq!(
            format_header("reindex", 0, "incremental"),
            "─── Reindexed: 0 files, 0 chunks ───"
        );
    }

    #[test]
    fn header_default_no_extra() {
        assert_eq!(format_header("graph", 7, ""), "─── 7 results ───");
    }

    #[test]
    fn header_default_with_extra() {
        assert_eq!(
            format_header("graph", 7, "2.1x dedup"),
            "─── 7 results (2.1x dedup) ───"
        );
    }

    #[test]
    fn header_zero_total() {
        assert_eq!(format_header("search", 0, "bm25"), "─── 0 results (bm25) ───");
    }

    // ── format_row ───────────────────────────────────────────────────────

    #[test]
    fn row_normal() {
        assert_eq!(
            format_row(1, "src/main.rs", 12, 45, "auth", "fn", "3 hits"),
            "  1  src/main.rs:12-45  auth  [fn]  3 hits"
        );
    }

    #[test]
    fn row_single_line() {
        assert_eq!(
            format_row(2, "src/lib.rs", 5, 5, "?", "?", "1 hit"),
            "  2  src/lib.rs:5-5  ?  [?]  1 hit"
        );
    }

    #[test]
    fn row_empty_label_defaults_to_question() {
        assert_eq!(
            format_row(3, "mod.rs", 1, 10, "foo", "", "2 hits"),
            "  3  mod.rs:1-10  foo  [?]  2 hits"
        );
    }

    #[test]
    fn row_rank_alignment() {
        let r1 = format_row(1, "a.rs", 1, 1, "x", "fn", "");
        let r100 = format_row(100, "a.rs", 1, 1, "x", "fn", "");
        assert!(r1.starts_with("  1"));
        assert!(r100.starts_with("100"));
    }

    #[test]
    fn row_high_rank() {
        assert_eq!(
            format_row(999, "z.rs", 1, 2, "sym", "t", ""),
            "999  z.rs:1-2  sym  [t]  "
        );
    }

    // ── format_footer ────────────────────────────────────────────────────

    #[test]
    fn footer_first_page_all_fit() {
        assert_eq!(
            format_footer(0, 20, 5),
            "  page 1/1 offset=0 limit=20"
        );
    }

    #[test]
    fn footer_second_page() {
        assert_eq!(
            format_footer(20, 20, 45),
            "  page 2/3 offset=20 limit=20"
        );
    }

    #[test]
    fn footer_last_page_exact() {
        assert_eq!(
            format_footer(40, 20, 60),
            "  page 3/3 offset=40 limit=20"
        );
    }

    #[test]
    fn footer_zero_total() {
        assert_eq!(format_footer(0, 20, 0), "  page 1/1 offset=0 limit=20");
    }

    #[test]
    fn footer_limit_zero() {
        // Edge: division by zero guard — always page 1/1
        assert_eq!(format_footer(0, 0, 5), "  page 1/1 offset=0 limit=0");
    }

    // ── format_context ───────────────────────────────────────────────────

    #[test]
    fn context_single_match() {
        assert_eq!(
            format_context("line1\nline2\nline3\n", &[2]),
            "│ line1\n│ line2 ←\n│ line3"
        );
    }

    #[test]
    fn context_no_match() {
        assert_eq!(
            format_context("hello\nworld", &[]),
            "│ hello\n│ world"
        );
    }

    #[test]
    fn context_all_match() {
        assert_eq!(
            format_context("a\nb\nc", &[1, 2, 3]),
            "│ a ←\n│ b ←\n│ c ←"
        );
    }

    #[test]
    fn context_empty_source() {
        assert_eq!(format_context("", &[1]), "");
    }

    #[test]
    fn context_single_line_no_match() {
        assert_eq!(format_context("only", &[]), "│ only");
    }

    #[test]
    fn context_single_line_match() {
        assert_eq!(format_context("only", &[1]), "│ only ←");
    }
}
