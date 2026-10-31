//! Line-window and mode resolution helpers for ctx_read.
//!
//! Extracted from ctx_read.rs to satisfy the 1500-LOC gate (#660).

use serde_json::{Map, Value};

/// Honour a `lines` argument as the window it names. It is not in the schema,
/// yet pilot P8 saw it on 58 of 246 `ctx_read` calls (`"231-300"`,
/// `"864:925"`, `":225-410"`, `[30, 50]`, `105` with `limit`). Ignored, each
/// of those was a read without a range and answered with something other than
/// the lines asked for. A `mode` the caller also passed wins; a string or pair
/// becomes `mode="lines:…"` (malformed payloads fail loudly through the normal
/// `lines:` validation), a bare number becomes `start_line`.
pub(super) fn apply_lines_alias(args: &Map<String, Value>) -> Option<Map<String, Value>> {
    if args.contains_key("mode") {
        return None;
    }
    let mut rewritten = args.clone();
    match rewritten.remove("lines")? {
        Value::String(text) => {
            let payload = text.trim();
            let payload = payload.strip_prefix("lines:").unwrap_or(payload);
            let payload = payload.trim_start_matches(':');
            rewritten.insert("mode".into(), Value::String(format!("lines:{payload}")));
        }
        Value::Array(pair) => match pair.as_slice() {
            [start, end] => {
                let (start, end) = (start.as_i64()?, end.as_i64()?);
                rewritten.insert("mode".into(), Value::String(format!("lines:{start}-{end}")));
            }
            _ => return None,
        },
        Value::Number(start) => {
            if !(rewritten.contains_key("start_line") || rewritten.contains_key("offset")) {
                rewritten.insert("start_line".into(), Value::from(start.as_i64()?));
            }
        }
        _ => return None,
    }
    Some(rewritten)
}

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
    if !crate::tools::ctx_read::instruction_file_forces_full(path, mode) {
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
                | crate::tools::ctx_read::ReadMode::LinesTail(_)
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

/// Rewrite the documented bare tail spelling `-N` to `lines:-N` (#1813).
/// Thin wrapper over the shared rule in [`crate::tools::ctx_read::mode`] so the
/// MCP handler and `lean-ctx read` cannot drift apart on spelling.
pub(super) fn canonicalize_tail_mode(mode_arg: Option<String>) -> Option<String> {
    let mode = mode_arg?;
    Some(crate::tools::ctx_read::canonicalize_tail_mode(&mode).unwrap_or(mode))
}

/// The explicit `mode` a `ctx_read` call asked for, after both spelling rules.
///
/// #1965: the order matters. The tail spelling is canonicalized *before* the
/// raw alias looks at the mode, so `mode="-3", raw=true` keeps its window like
/// `lines:-3` does (#1490). The other way round, raw saw a bare `-3`, did not
/// recognise it as a window, and replaced it with the whole file — silently.
pub(in crate::tools::registered) fn resolve_explicit_mode(
    arg_raw: bool,
    mode_arg: Option<String>,
) -> Option<String> {
    resolve_raw_alias(arg_raw, canonicalize_tail_mode(mode_arg))
}

#[cfg(test)]
mod tests {
    use super::*;

    // #1813: the schema documents `-N=tail`; only `lines:-N` was implemented,
    // so a bare `-3` was answered with the head of the file, silently.
    #[test]
    fn bare_tail_mode_canonicalizes_to_lines_form() {
        for (input, expected) in [
            ("-3", "lines:-3"),
            ("-1", "lines:-1"),
            ("-250", "lines:-250"),
        ] {
            assert_eq!(
                canonicalize_tail_mode(Some(input.to_string())),
                Some(expected.to_string()),
                "{input} is the documented tail spelling"
            );
        }
    }

    // GH #2000: the schema documents `5,10-20=multi` without a prefix; the bare
    // form fell through to an unknown mode and returned the whole file.
    #[test]
    fn bare_multi_select_canonicalizes_to_lines_form() {
        for (input, expected) in [
            ("3,7", "lines:3,7"),
            ("3,7-9", "lines:3,7-9"),
            ("105,125,784-835", "lines:105,125,784-835"),
            ("5, 10-20", "lines:5, 10-20"),
            ("1-3,-5", "lines:1-3,-5"),
        ] {
            assert_eq!(
                canonicalize_tail_mode(Some(input.to_string())),
                Some(expected.to_string()),
                "{input} is the documented multi-select spelling"
            );
        }
        // The canonical result must parse as the multi-select it names.
        let mode: crate::tools::ctx_read::ReadMode = "lines:3,7-9".parse().unwrap();
        assert_eq!(mode.to_string(), "lines:3,7-9");
    }

    #[test]
    fn comma_shapes_that_are_not_line_selections_are_left_alone() {
        for mode in ["lines:3,7", "3,x", ",", "3,,7", "full,signatures", "3-,7"] {
            assert_eq!(
                canonicalize_tail_mode(Some(mode.to_string())),
                Some(mode.to_string()),
                "{mode} must not be reinterpreted as a line selection"
            );
        }
    }

    #[test]
    fn canonical_and_other_modes_pass_through_untouched() {
        for mode in [
            "lines:-3", // already canonical
            "full",
            "signatures",
            "lines:5-10",
            "anchored:1-2",
        ] {
            assert_eq!(
                canonicalize_tail_mode(Some(mode.to_string())),
                Some(mode.to_string())
            );
        }
        assert_eq!(canonicalize_tail_mode(None), None);
    }

    /// Only a pure `-<digits>` payload is a tail. Anything else must keep
    /// reaching the unknown-mode warning rather than being silently
    /// reinterpreted as something the caller did not ask for — which is the
    /// exact failure this fix exists to remove.
    #[test]
    fn dash_shapes_that_are_not_tails_are_left_alone() {
        for mode in ["-", "-x", "-3-5", "--3", "-3px", "- 3"] {
            assert_eq!(
                canonicalize_tail_mode(Some(mode.to_string())),
                Some(mode.to_string()),
                "{mode} must not be reinterpreted as a tail"
            );
        }
    }

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

    // #1965: `-N` with raw=true returned the whole file instead of the tail.
    #[test]
    fn gh1965_raw_keeps_bare_tail_window() {
        for (mode, expected) in [
            ("-3", "lines:-3"),
            ("lines:-3", "lines:-3"),
            ("lines:195-201", "lines:195-201"),
            ("full", "raw"),
        ] {
            assert_eq!(
                resolve_explicit_mode(true, Some(mode.into())),
                Some(expected.into()),
                "raw=true with mode={mode}"
            );
        }
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
