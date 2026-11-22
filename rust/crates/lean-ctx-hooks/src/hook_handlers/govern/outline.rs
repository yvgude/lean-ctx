// SPDX-License-Identifier: Apache-2.0

//! **R5** — the first whole-file Read of a large code file comes as its
//! outline: every symbol with its line span, so the model reads only the
//! regions it needs (`offset`/`limit`). Benchmarks showed this is where native
//! Read loses to Code Mode: a whole file enters the context once and is re-read
//! from cache on every later step.
//!
//! The rule is the native counterpart of `ctx_read`'s owner rule
//! ([`lean_ctx::core::auto_mode_resolver::unranged_view`]), with one escape
//! the host's Read needs: it fires once per file and reading context, so a
//! second whole-file Read returns the full content. Ranged reads, small files,
//! prose/data formats, instruction files and files without extractable
//! symbols always pass through.

use super::{Decision, MIN_SAVED_TOKENS, Policy, RuleMode, read_dedup};
use lean_ctx::core::auto_mode_resolver::{OUTLINE_MIN_LINES, outline_mode};
use lean_ctx::core::tokens::count_tokens;

/// Offset/limit sentinel of the per-file outline record: no real read window
/// uses it, so the record never collides with R1.read's window records.
const OUTLINE_RECORD_WINDOW: i64 = -1;

/// `Some` = R5 decided for this Read (applied or shadow receipt).
pub(super) fn decide(v: &serde_json::Value, policy: &Policy, session_id: &str) -> Option<Decision> {
    if policy.r5 == RuleMode::Off {
        return None;
    }
    let tool_input = super::super::payload::resolve_tool_args(v)?;
    if ["offset", "limit"]
        .iter()
        .any(|k| tool_input.get(*k).is_some_and(|x| !x.is_null()))
    {
        return None;
    }
    let (_, path) = super::super::payload::resolve_path_field(
        Some(&tool_input),
        super::super::payload::READ_PATH_FIELDS,
    )?;
    let resp = v.get("tool_response")?;
    let slot = read_dedup::locate_content(resp)?;
    let content = read_dedup::slot_text(resp, &slot)?;
    // The host's whole-file Read stops at a line cap (2000 in Claude Code):
    // `shown` is what the model would get, `total_lines` the file.
    let shown = content.lines().count();
    let total_lines = resp
        .get("file")
        .and_then(|f| f.get("totalLines"))
        .and_then(serde_json::Value::as_u64)
        .map_or(0, |n| n as usize)
        .max(shown);
    let agent_id = v.get("agent_id").and_then(|s| s.as_str()).unwrap_or("");
    let tool_use_id = v.get("tool_use_id").and_then(|s| s.as_str()).unwrap_or("");
    let (view, baseline, actual) = first_view(
        &path,
        content,
        total_lines,
        agent_id,
        tool_use_id,
        session_id,
    )?;
    let output = read_dedup::replace_slot(resp, &slot, &view)
        .map(|updated| read_dedup::post_tool_use_output(&updated))?;
    Some(Decision::from_rule(policy.r5, output, "R5", baseline, actual, session_id).with_view(view))
}

/// The R5 outline of a whole-file read of `path` with `content` (`total_lines`
/// when the reader was cut at a line cap), once per file and reading context:
/// `Some((view, baseline, actual))` only for a large code file with symbols
/// whose outline is at most half of it, and only the first time — a later
/// read is the escape and gets the full content, except a duplicate fire of
/// the same `tool_use_id`, which gets the same answer. Writes the record.
pub(super) fn first_view(
    path: &str,
    content: &str,
    total_lines: usize,
    agent_id: &str,
    tool_use_id: &str,
    session_id: &str,
) -> Option<(String, usize, usize)> {
    if outline_mode(path) != "signatures"
        || lean_ctx::core::instruction_files::is_instruction_file(path)
    {
        return None;
    }
    let shown = content.lines().count();
    let total_lines = total_lines.max(shown);
    if total_lines <= OUTLINE_MIN_LINES {
        return None;
    }
    // One record per file however it is spelled (a symlinked directory, a
    // relative `cat` operand), so a Read and a `cat` share the escape.
    let key = std::fs::canonicalize(path)
        .map_or_else(|_| path.to_string(), |p| p.to_string_lossy().into_owned());
    let store = read_dedup::record_path(
        session_id,
        agent_id,
        &key,
        OUTLINE_RECORD_WINDOW,
        OUTLINE_RECORD_WINDOW,
    )?;
    if let Ok(prev) = std::fs::read_to_string(&store) {
        // Already outlined once in this context: this whole-file read is the
        // explicit escape and gets the full content — unless it is a duplicate
        // fire of the same event, which must get the same answer.
        let same_event = read_dedup::parse_record(&prev)
            .is_some_and(|(_, prev_use)| !tool_use_id.is_empty() && prev_use == tool_use_id);
        if !same_event {
            return None;
        }
    }

    // `outline_mode` matched the extension case-insensitively; the extractor
    // needs it lowercase (`.RS` is Rust too).
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let (sigs, _) = lean_ctx::core::signatures::extract_signatures_counted(content, &ext);
    let located: Vec<String> = sigs
        .iter()
        .filter(|s| s.start_line.is_some())
        .map(lean_ctx::core::signatures::Signature::to_compact_located)
        .collect();
    if located.is_empty() {
        return None;
    }
    let view = render(path, shown, total_lines, &located);
    let baseline = count_tokens(content);
    let actual = count_tokens(&view);
    // Worth it only when the outline is at most half the file.
    if baseline.saturating_sub(actual) < MIN_SAVED_TOKENS || actual * 2 > baseline {
        return None;
    }
    // Written in shadow too: the record only steers R5, and without it every
    // later whole read would count as a saving the live rule never makes.
    read_dedup::write_record(&store, "outline", tool_use_id);
    Some((view, baseline, actual))
}

/// The outline body: a deterministic function of (path, line counts, symbols),
/// no timestamps or counters (#498). When the host's Read was cut at its line
/// cap, the outline covers only the lines it returned and says so, so the
/// model never takes a missing symbol for an absent one.
fn render(path: &str, shown: usize, total_lines: usize, located: &[String]) -> String {
    let mut out = if shown < total_lines {
        format!(
            "{path} [outline 1-{shown} of {total_lines}L · lean-ctx governor]\n\
             This file is large, so this first Read returned the outline (symbols with line \
             spans @Lstart-end) of lines 1-{shown} instead of their content. Lines \
             {}-{total_lines} are not covered. Read the parts you need with offset/limit \
             (or Read again without them for lines 1-{shown} in full), and read a region \
             before editing it: never rewrite this file from the outline.\n",
            shown + 1
        )
    } else {
        format!(
            "{path} [outline {total_lines}L · lean-ctx governor]\n\
             This file is large, so this first Read returned its outline (symbols with line \
             spans @Lstart-end) instead of all {total_lines} lines. Read the parts you need \
             with offset/limit (or Read again without them for the whole file), and read a \
             region before editing it: never rewrite this file from the outline.\n"
        )
    };
    for line in located {
        out.push_str(line);
        out.push('\n');
    }
    out
}
