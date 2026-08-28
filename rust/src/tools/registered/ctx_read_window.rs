//! Line-window and mode resolution helpers for ctx_read.
//!
//! Extracted from ctx_read.rs to satisfy the 1500-LOC gate (#660).

/// Resolve the `start_line`/`offset`/`limit` arguments into `(start, limit)`.
///
/// `offset` is an alias for `start_line` (1-based first line); `start_line`
/// wins if a caller passes both. `limit` (when > 0) bounds the number of lines;
/// a bare `limit` reads from line 1. Returns `None` when no windowing argument
/// is present, so the caller leaves the mode untouched (GitHub #432).
pub(super) fn resolve_line_window(
    start_line: Option<i64>,
    offset: Option<i64>,
    limit: Option<i64>,
) -> Option<(i64, Option<i64>)> {
    let start = start_line.or(offset).map(|v| v.max(1));
    let limit = limit.filter(|&l| l > 0);
    match (start, limit) {
        (Some(s), l) => Some((s, l)),
        (None, Some(_)) => Some((1, limit)),
        (None, None) => None,
    }
}

/// Build the `lines:N-M` mode string for a resolved window. An unbounded window
/// (no `limit`) reads to EOF via the historical `999999` sentinel.
pub(super) fn lines_mode(start: i64, limit: Option<i64>) -> String {
    match limit {
        Some(l) => format!("lines:{start}-{}", start + l - 1),
        None => format!("lines:{start}-999999"),
    }
}

/// Build the `anchored:N-M` mode string for a resolved window (#811) — mirrors
/// `lines_mode`, keeping the `anchored:` prefix so the render path re-attaches
/// hash anchors to the window instead of falling back to plain numbered lines.
pub(super) fn anchored_lines_mode(start: i64, limit: Option<i64>) -> String {
    match limit {
        Some(l) => format!("anchored:{start}-{}", start + l - 1),
        None => format!("anchored:{start}-999999"),
    }
}
pub(super) fn resolve_instruction_file_mode(path: &str, mode: &str) -> (String, Option<String>) {
    // #1584: `diff` is preserved alongside the other lossless views. A delta
    // against the cached baseline never *withholds* content the agent has not
    // already been given, so the "instruction files need complete content"
    // rule does not apply — overriding it to `full` re-sent the whole file and
    // silently discarded the delta the caller asked for.
    if !crate::tools::ctx_read::is_instruction_file(path)
        || matches!(mode, "full" | "raw" | "anchored" | "diff")
        || mode.starts_with("anchored:")
        || mode.starts_with("lines:")
    {
        return (mode.to_string(), None);
    }

    (
        "full".to_string(),
        Some(format!(
            "[mode overridden: {mode} -> full, reason=instruction file requires complete content]"
        )),
    )
}

pub(super) fn scoped_read_ranges(
    mode: &str,
) -> Option<Vec<crate::tools::ctx_read::mode::LineRange>> {
    use crate::tools::ctx_read::{ReadMode, mode::LineRange};

    match mode.parse::<ReadMode>().ok()? {
        ReadMode::Lines(range) | ReadMode::Anchored(Some(range)) => Some(vec![range]),
        ReadMode::LinesMulti(payload) => Some(
            payload
                .split(',')
                .filter_map(|part| {
                    let (start, end) = part.split_once('-').unwrap_or((part, part));
                    Some(LineRange::new(start.parse().ok()?, end.parse().ok()?))
                })
                .collect(),
        ),
        _ => None,
    }
}

pub(super) fn hint_intersects_ranges(
    hint: &crate::core::cross_source_hints::CrossSourceHint,
    ranges: &[crate::tools::ctx_read::mode::LineRange],
    graph: &crate::core::property_graph::CodeGraph,
    relative_path: &str,
) -> bool {
    if hint.relation != "health_hotspot" {
        return false;
    }
    let Some((_, symbol)) = hint.source_uri.rsplit_once('#') else {
        return false;
    };
    let Ok(Some(node)) = graph.get_node_by_symbol(symbol, relative_path) else {
        return false;
    };
    let (Some(start), Some(end)) = (node.line_start, node.line_end) else {
        return false;
    };
    ranges
        .iter()
        .any(|range| start <= range.end as usize && end >= range.start as usize)
}

/// Apply a resolved line window to `mode`/`fresh`. Explicit `lines:N-M` and
/// `anchored:N-M` modes are preserved when `limit` is the only alias (#1254).
/// A `start_line` or `offset` still overrides any mode to prevent full-file
/// materialization, while `start_line=1` without a limit remains a no-op (#253).
pub(super) fn apply_line_window(
    mode: &mut String,
    fresh: &mut bool,
    explicit_mode: bool,
    start_line: Option<i64>,
    offset: Option<i64>,
    limit: Option<i64>,
) {
    let preserve_explicit_window = explicit_mode
        && start_line.is_none()
        && offset.is_none()
        && limit.is_some_and(|value| value > 0)
        && matches!(
            mode.parse::<crate::tools::ctx_read::ReadMode>(),
            Ok(crate::tools::ctx_read::ReadMode::Lines(_)
                | crate::tools::ctx_read::ReadMode::Anchored(Some(_)))
        );
    if preserve_explicit_window {
        return;
    }

    let Some((start, limit)) = resolve_line_window(start_line, offset, limit) else {
        return;
    };
    if start <= 1 && limit.is_none() {
        return;
    }
    *fresh = true;
    // #811: anchored gets its own windowed variant (preserves hashes for
    // ctx_patch); every other mode switches to lines:N-M to prevent
    // full-file materialization on large files.
    if mode == "anchored" {
        *mode = anchored_lines_mode(start, limit);
    } else {
        *mode = lines_mode(start, limit);
    }
}

/// #513: resolve the `raw=true` convenience flag into the effective explicit
/// `mode` argument. Agents reach for `raw:true` to get exact bytes; it aliases
/// to `mode="raw"` (verbatim, unframed) and wins over any caller-supplied
/// `mode`. When `raw` is unset, the caller's `mode` (if any) passes through
/// unchanged. The caller separately forces `fresh=true` for raw so a re-read
/// never collapses to an `[unchanged]`/auto-delta stub.
///
/// #1490: when the caller specified both `raw=true` and a `lines:N-M` /
/// `anchored:N-M` range, preserve the range — the agent explicitly chose a
/// window and `raw` means "lossless" not "ignore my selector". The triage
/// bypass already fires for both `raw=true` and pinned modes, so the content
/// is returned verbatim within the requested window.
pub(super) fn resolve_raw_alias(arg_raw: bool, mode_arg: Option<String>) -> Option<String> {
    if arg_raw {
        if let Some(ref m) = mode_arg {
            if m.starts_with("lines:") || m.starts_with("anchored:") {
                return mode_arg;
            }
        }
        Some("raw".to_string())
    } else {
        mode_arg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // #1490: raw=true must NOT override an explicit lines: or anchored: mode.
    #[test]
    fn gh1490_raw_preserves_lines_mode() {
        assert_eq!(
            resolve_raw_alias(true, Some("lines:90-100".into())),
            Some("lines:90-100".into()),
        );
        assert_eq!(
            resolve_raw_alias(true, Some("anchored:5-10".into())),
            Some("anchored:5-10".into()),
        );
    }

    #[test]
    fn gh1490_raw_still_works_without_range() {
        assert_eq!(resolve_raw_alias(true, None), Some("raw".into()));
        assert_eq!(
            resolve_raw_alias(true, Some("full".into())),
            Some("raw".into()),
        );
        assert_eq!(
            resolve_raw_alias(true, Some("map".into())),
            Some("raw".into()),
        );
    }

    #[test]
    fn raw_false_passes_through_mode() {
        assert_eq!(resolve_raw_alias(false, None), None);
        assert_eq!(
            resolve_raw_alias(false, Some("lines:5-10".into())),
            Some("lines:5-10".into()),
        );
    }

    /// #1584: `mode=diff` was advertised in the schema and then rejected at
    /// runtime. One of the two paths that silently rewrote it was the
    /// instruction-file rule, which forced every "lossy-looking" mode to
    /// `full`. A delta withholds nothing the caller has not already been
    /// given, so it belongs with the lossless views.
    #[test]
    fn instruction_file_rule_preserves_diff_mode() {
        let (mode, note) = resolve_instruction_file_mode("AGENTS.md", "diff");
        assert_eq!(mode, "diff", "diff must survive the instruction-file rule");
        assert!(
            note.is_none(),
            "no override note when nothing was overridden"
        );

        // The rule itself is intact for genuinely lossy views.
        let (mode, note) = resolve_instruction_file_mode("AGENTS.md", "signatures");
        assert_eq!(mode, "full");
        assert!(note.is_some());
    }
}
