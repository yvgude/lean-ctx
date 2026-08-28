//! Output rendering: full-output framing, header building, per-mode
//! processing, task-relevant filtering and line-range extraction.
//! Split out of `ctx_read/mod.rs`.

use super::{
    CrpMode, SymbolMap, append_compressed_hint, compressor, count_tokens, deps,
    detect_project_root, entropy, protocol, resolve_auto_mode, signatures, symbol_map,
};
use crate::core::aggressiveness::AggressivenessProfile;

fn monotonic_check(original: usize, compressed: usize) -> bool {
    compressed < original
}

/// Below this many raw tokens a silent full-content fallback is not worth a
/// banner: the file is small enough that the framing itself was the expensive
/// part (which is exactly what the #361 cap exists to strip), and a banner
/// would push the read back above the raw file it just protected. Above it, the
/// caller is being handed a whole file they did not order and must be told.
pub(crate) const NO_COMPRESSION_BANNER_MIN_TOKENS: usize = 400;

/// One-line notice that a compression path gave up and returned the untouched
/// file. Without it the caller pays full-file tokens believing a summary was
/// delivered — the failure is invisible in the output and surfaces only on the
/// bill. `None` for files below [`NO_COMPRESSION_BANNER_MIN_TOKENS`], where the
/// fallback is the cap working as designed rather than a degradation.
pub(crate) fn no_compression_banner(requested_mode: &str, raw_tokens: usize) -> Option<String> {
    (raw_tokens >= NO_COMPRESSION_BANNER_MIN_TOKENS).then(|| {
        format!(
            "[lean-ctx] no compression applied (mode={requested_mode}): \
             output was not smaller than the file — returning full content ({raw_tokens} tok)"
        )
    })
}

fn raw_fallback(
    path: &str,
    content: &str,
    original_tokens: usize,
    compressed: usize,
) -> (String, usize) {
    tracing::debug!(
        "monotonic guard: {path} compressed {compressed} >= original {original_tokens}, using raw"
    );
    let mode = crate::core::savings_footer::current_mode().unwrap_or_else(|| "compressed".into());
    match no_compression_banner(&mode, original_tokens) {
        Some(banner) => {
            let out = format!("{banner}\n{content}");
            let sent = count_tokens(&out);
            (out, sent)
        }
        None => (content.to_string(), original_tokens),
    }
}

/// Per-read tuning threaded into the per-mode renderers. `Default` reproduces
/// the behaviour from before the aggressiveness knob existed (no override), so
/// every existing caller and test keeps its exact byte output (#498).
#[derive(Clone, Copy, Default)]
pub(crate) struct ReadTuning<'a> {
    /// Resolved 0.0–1.0 compression intensity, or `None` to use each mode's
    /// built-in default. Already resolved via `aggressiveness::effective` at the
    /// read boundary, so the renderer treats it as authoritative.
    pub aggressiveness: Option<f64>,
    /// Explicit `protect` tokens (#709): every line containing one of these
    /// survives the line-based lossy filters (entropy / information-bottleneck)
    /// verbatim. Empty slice reproduces the pre-protect byte output (#498).
    /// Borrowed from the read boundary for the duration of the render call.
    pub protect: &'a [String],
}

impl<'a> ReadTuning<'a> {
    /// Resolves the effective tuning from an explicit per-call aggressiveness
    /// (falling back to the `LEAN_CTX_AGGRESSIVENESS` env var / config field) and
    /// the explicit `protect` token list.
    pub(crate) fn resolve(explicit_aggressiveness: Option<f64>, protect: &'a [String]) -> Self {
        Self {
            aggressiveness: crate::core::aggressiveness::effective(explicit_aggressiveness),
            protect,
        }
    }

    /// For an `auto` read, the `density:` mode an aggressiveness setting maps to
    /// (so one knob drives whole-file intensity via the proven density path).
    pub(crate) fn auto_density_mode(self) -> Option<String> {
        self.aggressiveness.map(|a| {
            format!(
                "density:{:.2}",
                AggressivenessProfile::from_level(a).density_target
            )
        })
    }
}

/// Render a trailing ` [a, b]` techniques tag, or `""` when no compression
/// technique fired. Avoids the empty ` []` metadata field a bare `join` would
/// leave on an incompressible file (#509 output-waste audit, same class as the
/// `ctx_semantic_search` `(rrf: X, )` fix in #511).
fn techniques_tag(techniques: &[String]) -> String {
    if techniques.is_empty() {
        String::new()
    } else {
        format!(" [{}]", techniques.join(", "))
    }
}

/// Code-health annotations (function name → note like `cc=18`) for the
/// over-threshold functions in `content`, honoring `[code_health].annotate_reads`.
/// Empty when disabled, unsupported, or nothing qualifies. Deterministic
/// (#498-safe): a pure function of content + the active threshold.
fn health_annotations(content: &str, ext: &str) -> std::collections::HashMap<String, String> {
    let cfg = crate::core::config::Config::load();
    if !cfg.code_health.annotate_reads {
        return std::collections::HashMap::new();
    }
    let annotations = crate::core::code_health::annotate::annotations_for_file(
        content,
        ext,
        cfg.code_health.cognitive_threshold,
    );
    crate::core::code_health::annotate::by_name(&annotations)
}

pub(crate) fn format_full_output(
    file_ref: &str,
    short: &str,
    ext: &str,
    content: &str,
    original_tokens: usize,
    line_count: usize,
    _task: Option<&str>,
) -> (String, usize) {
    let _mode_guard = crate::core::savings_footer::ModeGuard::new("full");
    let tokens = original_tokens;
    let metadata = build_header(file_ref, short, ext, content, line_count, true);

    let output = format!("{metadata}\n{content}");
    let sent = count_tokens(&output);
    (protocol::append_savings(&output, tokens, sent), sent)
}

/// Headerless, trailing-whitespace-stripped output for the Read redirect path.
///
/// The hook redirect writes this into a temp file the host reads *as* the real
/// file's content. No framing header (fixes offset/limit correctness, #1021
/// follow-up) and trailing whitespace is stripped per line for modest
/// compression without breaking line structure or edit round-trips.
pub(crate) fn format_full_compact_output(content: &str) -> (String, usize) {
    let _mode_guard = crate::core::savings_footer::ModeGuard::new("full-compact");
    let output: String = content
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    let sent = count_tokens(&output);
    (output, sent)
}

/// Render `content` with per-line `N:hh|` hash anchors for `ctx_patch`
/// (mode=anchored, epic #1008). The body is verbatim source prefixed with a
/// stable, self-describing one-line legend so a vanilla agent can read the
/// format without prior knowledge (GL#580 self-describing-output philosophy).
///
/// #498 determinism: the output is a pure function of `(file_ref, short,
/// content, line_count)` — no savings footer, timestamps or counters — so
/// identical re-reads stay byte-stable and provider prompt caching applies. No
/// `append_savings`: anchors are lossless *additions*, so a savings line would
/// always be negative and misleading.
pub(crate) fn format_anchored_output(
    file_ref: &str,
    short: &str,
    content: &str,
    line_count: usize,
) -> (String, usize) {
    format_anchored_output_window(file_ref, short, content, line_count, None)
}

/// Windowed variant of [`format_anchored_output`] (#811): when `window` is
/// `Some((start, end))`, `content` is sliced to that 1-based inclusive span
/// *before* anchoring, so a bounded anchored read never has to hash/render
/// lines outside the requested window — only the slice is annotated, numbered
/// from its true position in the file so anchors still line up with
/// `ctx_patch`. `None` reproduces the whole-file behaviour above.
pub(crate) fn format_anchored_output_window(
    file_ref: &str,
    short: &str,
    body: &str,
    line_count: usize,
    window: Option<(usize, usize)>,
) -> (String, usize) {
    let _mode_guard = crate::core::savings_footer::ModeGuard::new("anchored");
    let (start_line, range_suffix) = match window {
        Some((start, end)) => (start, format!(" lines:{start}-{end}")),
        None => (1, String::new()),
    };
    let header = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
        format!(
            "{file_ref}={short} {line_count}L{range_suffix} [anchored: N:hh|line → edit via ctx_patch]"
        )
    } else {
        format!("{short} {line_count}L{range_suffix} [anchored: N:hh|line → edit via ctx_patch]")
    };
    let annotated = crate::core::anchor::annotate(body, start_line);
    let output = format!("{header}\n{annotated}");
    let sent = count_tokens(&output);
    (output, sent)
}

pub(crate) fn build_header(
    file_ref: &str,
    short: &str,
    ext: &str,
    content: &str,
    line_count: usize,
    include_deps: bool,
) -> String {
    let mut header = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
        format!("{file_ref}={short} {line_count}L")
    } else {
        format!("{short} {line_count}L")
    };

    if include_deps {
        let dep_info = deps::extract_deps(content, ext);
        if !dep_info.imports.is_empty() {
            let imports_str: Vec<&str> = dep_info
                .imports
                .iter()
                .take(8)
                .map(std::string::String::as_str)
                .collect();
            header.push_str(&format!("\n deps {}", imports_str.join(",")));
        }
        if !dep_info.exports.is_empty() {
            let exports_str: Vec<&str> = dep_info
                .exports
                .iter()
                .take(8)
                .map(std::string::String::as_str)
                .collect();
            header.push_str(&format!("\n exports {}", exports_str.join(",")));
        }
    }

    header
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn process_mode(
    content: &str,
    mode: &str,
    file_ref: &str,
    short: &str,
    ext: &str,
    original_tokens: usize,
    crp_mode: CrpMode,
    file_path: &str,
    task: Option<&str>,
) -> (String, usize) {
    process_mode_tuned(
        content,
        mode,
        file_ref,
        short,
        ext,
        original_tokens,
        crp_mode,
        file_path,
        task,
        ReadTuning::default(),
    )
}

/// Renders `content` for `mode`, honouring the aggressiveness knob carried in
/// `tuning`. `process_mode` is the unchanged-behaviour wrapper (`ReadTuning::
/// default()`); the real read path threads a resolved `tuning` through here.
#[allow(clippy::too_many_arguments)]
pub(crate) fn process_mode_tuned(
    content: &str,
    mode: &str,
    file_ref: &str,
    short: &str,
    ext: &str,
    original_tokens: usize,
    crp_mode: CrpMode,
    file_path: &str,
    task: Option<&str>,
    tuning: ReadTuning<'_>,
) -> (String, usize) {
    let _mode_guard = crate::core::savings_footer::ModeGuard::new(mode);
    let line_count = content.lines().count();
    let ctx = RenderCtx {
        file_ref,
        short,
        ext,
        file_path,
        original_tokens,
        crp_mode,
        line_count,
        task,
    };

    match mode {
        "raw" => {
            let sent = count_tokens(content);
            (content.to_string(), sent)
        }
        "auto" => {
            // The aggressiveness knob routes `auto` through the density path so a
            // single number drives whole-file intensity; otherwise the learned
            // auto-resolver picks the mode.
            let chosen = tuning.auto_density_mode().unwrap_or_else(|| {
                let lc = content.lines().count();
                resolve_auto_mode(None, file_path, original_tokens, Some(lc), task)
            });
            process_mode_tuned(
                content,
                &chosen,
                file_ref,
                short,
                ext,
                original_tokens,
                crp_mode,
                file_path,
                task,
                tuning,
            )
        }
        "full" => format_full_output(
            file_ref,
            short,
            ext,
            content,
            original_tokens,
            line_count,
            task,
        ),
        "full-compact" => format_full_compact_output(content),
        "anchored" => format_anchored_output(file_ref, short, content, line_count),
        mode if mode.starts_with("anchored:") => {
            let range_str = &mode[9..];
            let (start, end) = parse_anchored_range(range_str, line_count);
            let (body, start, end) = slice_window(content, start, end);
            format_anchored_output_window(file_ref, short, &body, line_count, Some((start, end)))
        }
        "signatures" => render_signatures(content, ctx),
        "map" => render_map(content, ctx),
        "aggressive" => render_aggressive(content, ctx),
        "entropy" => render_entropy(content, ctx, &tuning),
        "cognitive" => render_cognitive(content, ctx),
        "mdl" => render_mdl(content, ctx),
        "task" => render_task_mode(content, ctx, &tuning),
        "reference" => {
            let tok = count_tokens(content);
            let output = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
                format!("{file_ref}={short}: {line_count} lines, {tok} tok ({ext})")
            } else {
                format!("{short}: {line_count} lines, {tok} tok ({ext})")
            };
            let sent = count_tokens(&output);
            let savings = protocol::format_savings(original_tokens, sent);
            (format!("{output}\n{savings}"), sent)
        }
        mode if mode.starts_with("lines:") => {
            let range_str = &mode[6..];
            let extracted = extract_line_range(content, range_str);
            let header = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
                format!("{file_ref}={short} {line_count}L lines:{range_str}")
            } else {
                format!("{short} {line_count}L lines:{range_str}")
            };
            let multi_hint = if range_str.contains(',') {
                LINES_COMMA_HINT
            } else {
                ""
            };
            let sent = count_tokens(&extracted);
            let savings = protocol::format_savings(original_tokens, sent);
            (
                format!("{header}\n{extracted}{multi_hint}\n{savings}"),
                sent,
            )
        }
        mode if mode.starts_with("density:") => {
            // SDE target-density mode: compress to a token budget instead of
            // maximum compression. `density:0.4` ≈ 40% of original tokens. A bare
            // `density:` falls back to the aggressiveness target (else 0.5).
            let aggr_target = tuning
                .aggressiveness
                .map(|a| AggressivenessProfile::from_level(a).density_target);
            let target: f64 = mode[8..].parse().ok().or(aggr_target).unwrap_or(0.5);
            let result = entropy::entropy_compress_to_density(content, target);
            // #798: Quality gate — reject density output that breaks AST/symbols.
            // #940: use density-specific guard that only checks AST/identifiers,
            // not line count — density mode *intentionally* reduces lines.
            let (guarded_output, _q) =
                crate::core::quality::guard_density(content, &result.output, ext, target);
            let guarded_tokens = count_tokens(&guarded_output);
            let actual = if result.original_tokens > 0 {
                guarded_tokens as f64 / result.original_tokens as f64
            } else {
                0.0
            };
            let techs = techniques_tag(&result.techniques);
            let target_clamped = target.clamp(0.05, 1.0);
            let header = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
                format!(
                    "{file_ref}={short} {line_count}L density target={target_clamped:.2} actual={actual:.2}{techs}"
                )
            } else {
                format!(
                    "{short} {line_count}L density target={target_clamped:.2} actual={actual:.2}{techs}"
                )
            };
            let output = format!("{header}\n{guarded_output}");
            let sent = count_tokens(&output);
            if !monotonic_check(original_tokens, sent) {
                return raw_fallback(file_path, content, original_tokens, sent);
            }
            let savings = protocol::format_savings(original_tokens, sent);
            (
                append_compressed_hint(&format!("{output}\n{savings}"), file_path),
                sent,
            )
        }
        // `diff` is a delta view rendered against the session cache by
        // `core_logic::handle_diff`, not a whole-file render — there is no
        // content-only way to produce it. Reaching the generic renderer means a
        // caller bypassed the cache-aware path; answer in one bounded line
        // instead of falling through to `unknown` and dumping the whole file
        // under a warning the caller pays full tokens for (#1584).
        "diff" => {
            let msg = format!(
                "{short}: mode=diff renders against the session cache and cannot be produced here. \
                 Read the file once with mode=full, then request mode=diff on the re-read."
            );
            let sent = count_tokens(&msg);
            (msg, sent)
        }
        unknown => {
            let header = build_header(file_ref, short, ext, content, line_count, true);
            let out = format!(
                "[WARNING: unknown mode '{unknown}', falling back to full — valid modes: {}]\n{header}\n{content}",
                crate::core::mcp_manifest::READ_MODES.join(", ")
            );
            let sent = count_tokens(&out);
            (out, sent)
        }
    }
}

/// When a task is active, find the symbol whose name best matches a task
/// keyword and return its body as numbered source lines (capped).
///
/// `map`/`signatures` stay compact but include the one symbol body the agent is
/// most likely about to read, avoiding a follow-up full read. Uses the
/// tree-sitter chunk extractor (which carries spans + body across languages); a
/// no-op when tree-sitter is unavailable.
#[cfg(not(feature = "tree-sitter"))]
pub(crate) fn task_relevant_body(
    _content: &str,
    _file_path: &str,
    _ext: &str,
    _task: Option<&str>,
) -> Option<String> {
    None
}

#[cfg(feature = "tree-sitter")]
pub(crate) fn task_relevant_body(
    content: &str,
    file_path: &str,
    ext: &str,
    task: Option<&str>,
) -> Option<String> {
    const MAX_BODY_LINES: usize = 80;

    let task = task.map(str::trim).filter(|t| !t.is_empty())?;
    let (_files, keywords) = crate::core::task_relevance::parse_task_hints(task);
    if keywords.is_empty() {
        return None;
    }
    let kw_lower: Vec<String> = keywords.iter().map(|k| k.to_lowercase()).collect();

    let chunks = crate::core::chunks_ts::extract_chunks_ts(file_path, content, ext)?;

    // Score: exact name match (2) beats substring overlap (1).
    let mut best_idx: Option<usize> = None;
    let mut best_score = 0u8;
    for (i, ch) in chunks.iter().enumerate() {
        if ch.symbol_name.is_empty() {
            continue;
        }
        let name_l = ch.symbol_name.to_lowercase();
        let substr = kw_lower
            .iter()
            .any(|k| k.len() >= 3 && (name_l.contains(k.as_str()) || k.contains(name_l.as_str())));
        let score = if kw_lower.contains(&name_l) {
            2
        } else {
            u8::from(substr)
        };
        if score > best_score {
            best_score = score;
            best_idx = Some(i);
        }
    }

    let ch = &chunks[best_idx?];
    let body_lines: Vec<&str> = ch.content.lines().collect();
    let total = body_lines.len();
    let shown = total.min(MAX_BODY_LINES);
    let body: String = body_lines[..shown]
        .iter()
        .enumerate()
        .map(|(i, l)| format!("{:>4}|{l}", ch.start_line + i))
        .collect::<Vec<_>>()
        .join("\n");
    let truncated = if shown < total {
        format!(
            "\n  … +{} lines — ctx_read(mode=\"lines:{}-{}\")",
            total - shown,
            ch.start_line + shown,
            ch.end_line
        )
    } else {
        String::new()
    };
    Some(format!(
        "  ▸ body {} L{}-{}:\n{body}{truncated}",
        ch.symbol_name, ch.start_line, ch.end_line
    ))
}

/// One-line explainer appended whenever a `lines:` payload uses a comma —
/// comma means multi-select, and a caller who meant a `N-M` span must be able
/// to see that from the output (limitations #7). Shared with the CLI arm.
pub(crate) const LINES_COMMA_HINT: &str = "\n[lines: comma = multi-select (e.g. 5,10-20 picks line 5 and lines 10-20); use N-M for one span]";

/// Marker for a map/signatures view that extracted nothing — a language with
/// no grammar/regex coverage must be distinguishable from a file with no API
/// (limitations audit, #4). Shared with the CLI arms.
pub(crate) fn no_structure_marker(ext: &str) -> String {
    format!(
        "\n  [no extractable structure for .{ext} — header only; use mode=\"lines:N-M\" or \"full\"]"
    )
}

pub(crate) fn extract_line_range(content: &str, range_str: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    let mut selected = Vec::new();

    for part in range_str.split(',') {
        let part = part.trim();
        if let Some((start_s, end_s)) = part.split_once('-') {
            let start = start_s.trim().parse::<usize>().unwrap_or(1).max(1);
            let end = end_s.trim().parse::<usize>().unwrap_or(total).min(total);
            for i in start..=end {
                if i >= 1 && i <= total {
                    selected.push(format!("{i:>4}| {}", lines[i - 1]));
                }
            }
        } else if let Ok(n) = part.parse::<usize>()
            && n >= 1
            && n <= total
        {
            selected.push(format!("{n:>4}| {}", lines[n - 1]));
        }
    }

    if selected.is_empty() {
        "No lines matched the range.".to_string()
    } else {
        selected.join("\n")
    }
}

/// Parses a single `"start-end"` (or bare `"start"`, meaning "to EOF") window
/// payload for `anchored:N-M` (#811). Single-span only — unlike `lines:`,
/// an anchored window feeds `ctx_patch`, which edits one contiguous region at
/// a time, so comma multi-select isn't needed here.
fn parse_anchored_range(range_str: &str, total: usize) -> (usize, usize) {
    if let Some((s, e)) = range_str.split_once('-') {
        let start = s.trim().parse::<usize>().unwrap_or(1).max(1);
        let end = e.trim().parse::<usize>().unwrap_or(total).min(total);
        (start, end)
    } else {
        let start = range_str.trim().parse::<usize>().unwrap_or(1).max(1);
        (start, total)
    }
}

/// Slices `content` to 1-based inclusive `[start, end]`, clamped to the
/// file's real bounds. Returns the joined body and the clamped `(start, end)`
/// actually served — shared by the in-memory `anchored:N-M` render path.
fn slice_window(content: &str, start: usize, end: usize) -> (String, usize, usize) {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    let start = start.max(1).min(total.max(1));
    let end = end.min(total);
    let body = if total > 0 && end >= start {
        lines[start - 1..end].join("\n")
    } else {
        String::new()
    };
    (body, start, end)
}

#[cfg(test)]
mod render_tests {
    use super::techniques_tag;

    #[test]
    fn techniques_tag_omits_empty_brackets() {
        // An incompressible file leaves no techniques — the header must not
        // carry an empty ` []` field (#509 output-waste audit).
        assert_eq!(techniques_tag(&[]), "");
    }

    #[test]
    fn techniques_tag_wraps_nonempty_with_leading_space() {
        assert_eq!(
            techniques_tag(&["⊘ 3 low-entropy lines".to_string(), "⊘ 2 dups".to_string()]),
            " [⊘ 3 low-entropy lines, ⊘ 2 dups]"
        );
    }

    #[test]
    fn techniques_tag_single() {
        assert_eq!(
            techniques_tag(&["density target=0.40".to_string()]),
            " [density target=0.40]"
        );
    }

    /// #1584: `diff` reaching the whole-file dispatcher used to be reported as
    /// an *unknown* mode and answered with the entire file under a warning —
    /// the caller paid full tokens for a delta they never got. It now answers
    /// in one bounded line that names the way to get a real delta.
    #[test]
    fn diff_mode_never_falls_through_to_the_unknown_mode_dump() {
        let content = "fn a() {}\nfn b() {}\nfn c() {}\n";
        let (out, _) = super::process_mode_tuned(
            content,
            "diff",
            "",
            "sample.rs",
            "rs",
            32,
            crate::tools::CrpMode::Off,
            "sample.rs",
            None,
            super::ReadTuning {
                aggressiveness: None,
                protect: &[],
            },
        );
        assert!(
            !out.contains("unknown mode"),
            "diff is a documented mode, not an unknown one: {out}"
        );
        assert!(
            !out.contains("fn b()"),
            "the fallback must not dump the file the caller did not ask for: {out}"
        );
        assert!(
            out.contains("mode=full"),
            "it must name the way forward: {out}"
        );
    }

    /// #1584: when a mode really is unknown, the warning names what is valid
    /// instead of leaving the caller to guess — the schema list and the runtime
    /// now come from one constant, which is what let `diff` drift apart.
    #[test]
    fn unknown_mode_warning_names_the_valid_modes() {
        let (out, _) = super::process_mode_tuned(
            "fn a() {}\n",
            "definitely-not-a-mode",
            "",
            "sample.rs",
            "rs",
            8,
            crate::tools::CrpMode::Off,
            "sample.rs",
            None,
            super::ReadTuning {
                aggressiveness: None,
                protect: &[],
            },
        );
        assert!(
            out.contains("unknown mode 'definitely-not-a-mode'"),
            "{out}"
        );
        for mode in crate::core::mcp_manifest::READ_MODES {
            assert!(out.contains(mode), "warning must list `{mode}`: {out}");
        }
    }

    /// #1587: a compression request that degrades to the whole file says so.
    /// Below the threshold it stays silent, so the #361 cap still guarantees a
    /// read never costs more than the raw file.
    #[test]
    fn no_compression_banner_only_above_threshold() {
        assert!(super::no_compression_banner("signatures", 10).is_none());
        let banner =
            super::no_compression_banner("signatures", super::NO_COMPRESSION_BANNER_MIN_TOKENS)
                .expect("a whole file handed back instead of a summary must be announced");
        assert!(banner.contains("no compression applied"), "{banner}");
        assert!(banner.contains("mode=signatures"), "{banner}");
    }
}

/// Shared, `Copy` bundle of the per-call rendering context threaded to the
/// per-mode `render_*` helpers extracted from `process_mode_tuned` (keeps each
/// mode arm testable and the dispatcher a thin `match`).
#[derive(Clone, Copy)]
struct RenderCtx<'a> {
    file_ref: &'a str,
    short: &'a str,
    ext: &'a str,
    file_path: &'a str,
    original_tokens: usize,
    crp_mode: CrpMode,
    line_count: usize,
    task: Option<&'a str>,
}

fn render_signatures(content: &str, ctx: RenderCtx<'_>) -> (String, usize) {
    let RenderCtx {
        file_ref,
        short,
        ext,
        file_path,
        original_tokens,
        crp_mode,
        line_count,
        task,
    } = ctx;
    let sigs = signatures::extract_signatures(content, ext);
    let dep_info = deps::extract_deps(content, ext);

    let mut output = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
        format!("{file_ref}={short} {line_count}L")
    } else {
        format!("{short} {line_count}L")
    };
    if !dep_info.imports.is_empty() {
        let imports_str: Vec<&str> = dep_info
            .imports
            .iter()
            .take(8)
            .map(std::string::String::as_str)
            .collect();
        output.push_str(&format!("\n deps {}", imports_str.join(",")));
    }
    // Self-describing outputs (GL #580): symbol notation always ships
    // its own one-line legend so vanilla agents can read it.
    if crp_mode.is_tdd() {
        let refs: Vec<&signatures::Signature> = sigs.iter().collect();
        let legend = signatures::tdd_legend(&refs);
        if !legend.is_empty() {
            output.push('\n');
            output.push_str(&legend);
        }
    }
    let health = health_annotations(content, ext);
    for sig in &sigs {
        output.push('\n');
        if crp_mode.is_tdd() {
            output.push_str(&sig.to_tdd_located());
        } else {
            output.push_str(&sig.to_compact_located());
        }
        if let Some(note) = health.get(&sig.name) {
            output.push_str("  ");
            output.push_str(note);
        }
    }
    // Same honesty rule as map: an empty signature view for a language
    // without an extractor must be labeled (limitations audit, #4).
    if sigs.is_empty() && dep_info.imports.is_empty() {
        output.push_str(&no_structure_marker(ext));
    }
    if let Some(body) = task_relevant_body(content, file_path, ext, task) {
        output.push('\n');
        output.push_str(&body);
    }
    // JIT disclosure (GL#447): signatures carry L-spans, so point at the
    // targeted range expansion before the full-read escalation.
    if crate::core::profiles::active_profile()
        .output_hints
        .compressed_hint()
        && !sigs.is_empty()
    {
        output.push_str(&format!(
                    "\n  ↳ expand a symbol: ctx_read(\"{file_path}\", mode=\"lines:N-M\") using the spans above"
                ));
        // Located symbols are addressable as stable handles (#607).
        output.push_str(&format!("\n  {}", crate::core::handle::USAGE_HINT));
    }
    let sent = count_tokens(&output);
    (
        append_compressed_hint(
            &protocol::append_savings(&output, original_tokens, sent),
            file_path,
        ),
        sent,
    )
}

fn render_map(content: &str, ctx: RenderCtx<'_>) -> (String, usize) {
    let RenderCtx {
        file_ref,
        short,
        ext,
        file_path,
        original_tokens,
        crp_mode,
        line_count,
        task,
    } = ctx;
    if ext == "php"
        && let Some(php_map) = crate::core::patterns::php::compress_php_map(content, short)
    {
        let output = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
            format!("{file_ref}={short} {line_count}L\n{php_map}")
        } else {
            format!("{short} {line_count}L\n{php_map}")
        };
        let sent = count_tokens(&output);
        let output = protocol::append_savings(&output, original_tokens, sent);
        return (append_compressed_hint(&output, file_path), sent);
    }

    let structured = match ext {
        "md" | "mdx" | "rst" => crate::core::structured_read::extract_markdown_outline(content),
        "json" => crate::core::structured_read::extract_json_structure(content),
        "yaml" | "yml" => crate::core::structured_read::extract_yaml_structure(content),
        "toml" => crate::core::structured_read::extract_toml_structure(content),
        _ if file_path.to_lowercase().ends_with(".lock")
            || file_path.to_lowercase().ends_with("go.sum") =>
        {
            crate::core::structured_read::extract_lock_summary(content, file_path)
        }
        _ => String::new(),
    };

    if !structured.is_empty() {
        let mut output = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
            format!("{file_ref}={short} {line_count}L\n{structured}")
        } else {
            format!("{short} {line_count}L\n{structured}")
        };
        let sent = count_tokens(&output);
        output = protocol::append_savings(&output, original_tokens, sent);
        return (append_compressed_hint(&output, file_path), sent);
    }

    let sigs = signatures::extract_signatures(content, ext);
    let dep_info = deps::extract_deps(content, ext);

    let mut output = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
        format!("{file_ref}={short} {line_count}L")
    } else {
        format!("{short} {line_count}L")
    };

    if !dep_info.imports.is_empty() {
        output.push_str("\n  deps: ");
        output.push_str(&dep_info.imports.join(", "));
    }

    let key_sigs: Vec<&signatures::Signature> = sigs
        .iter()
        .filter(|s| s.is_exported || s.indent == 0)
        .collect();

    // Drop exports the API section already lists with full signatures
    // (pure redundant tokens in map mode, #361).
    let extra_exports = signatures::exports_not_in_signatures(&dep_info.exports, &key_sigs);
    if !extra_exports.is_empty() {
        output.push_str("\n  exports: ");
        output.push_str(&extra_exports.join(", "));
    }

    if !key_sigs.is_empty() {
        output.push_str("\n  API:");
        // Self-describing outputs (GL #580): legend precedes symbols.
        if crp_mode.is_tdd() {
            let legend = signatures::tdd_legend(&key_sigs);
            if !legend.is_empty() {
                output.push_str(&format!(" {legend}"));
            }
        }
        let health = health_annotations(content, ext);
        for sig in &key_sigs {
            output.push_str("\n    ");
            if crp_mode.is_tdd() {
                output.push_str(&sig.to_tdd_located());
            } else {
                output.push_str(&sig.to_compact_located());
            }
            if let Some(note) = health.get(&sig.name) {
                output.push_str("  ");
                output.push_str(note);
            }
        }
    }

    // Nothing extractable (no grammar/regex coverage for this language):
    // an information-free map must say so, or the caller reads the bare
    // header as "this file has no API" (limitations audit, #4 residual).
    if key_sigs.is_empty() && dep_info.imports.is_empty() && extra_exports.is_empty() {
        output.push_str(&no_structure_marker(ext));
    }

    if let Some(body) = task_relevant_body(content, file_path, ext, task) {
        output.push('\n');
        output.push_str(&body);
    }
    // Located symbols are addressable as stable handles (#607).
    if crate::core::profiles::active_profile()
        .output_hints
        .compressed_hint()
        && !key_sigs.is_empty()
    {
        output.push_str(&format!("\n  {}", crate::core::handle::USAGE_HINT));
    }

    let sent = count_tokens(&output);
    (
        append_compressed_hint(
            &protocol::append_savings(&output, original_tokens, sent),
            file_path,
        ),
        sent,
    )
}

/// Full science pipeline: IB intent → relevance scores → Wasserstein OT allocation.
/// Returns chunk indices sorted by file position, with per-chunk token budgets
/// optimally distributed by the Sinkhorn solver.
fn wasserstein_select(
    chunks: &[crate::core::cognitive::SemanticChunk],
    total_budget: usize,
) -> Vec<usize> {
    use crate::core::cognitive::budget_select;
    use crate::core::ib::{classify_intent, compute_relevance};
    use crate::core::session::SessionState;

    let session = SessionState::load_latest().unwrap_or_default();
    let intent = classify_intent(&session);

    let chunk_texts: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
    let task_query = session
        .task
        .as_ref()
        .map(|t| t.description.as_str())
        .unwrap_or("");
    let scores = compute_relevance(&chunk_texts, &intent, Some(task_query));

    if scores.is_empty() || scores.iter().all(|s| s.score == 0.0) {
        return budget_select(chunks, None);
    }

    let names: Vec<String> = (0..chunks.len()).map(|i| format!("chunk_{i}")).collect();
    let files: Vec<(&str, usize, f64)> = names
        .iter()
        .zip(chunks.iter().zip(scores.iter()))
        .map(|(name, (chunk, score))| (name.as_str(), chunk.token_count, score.score))
        .collect();

    let allocations = crate::core::wasserstein::allocate_budget(&files, total_budget / 2);

    let mut ranked: Vec<(usize, usize)> = allocations
        .iter()
        .enumerate()
        .filter(|(_, alloc)| alloc.tokens > 0)
        .map(|(i, alloc)| (i, alloc.tokens))
        .collect();

    const MAX_CHUNKS: usize = 9;
    ranked.sort_by_key(|b| std::cmp::Reverse(b.1));
    ranked.truncate(MAX_CHUNKS);
    ranked.sort_by_key(|(i, _)| chunks[*i].line_range);
    ranked.into_iter().map(|(i, _)| i).collect()
}

fn render_cognitive(content: &str, ctx: RenderCtx<'_>) -> (String, usize) {
    use crate::core::cognitive::{budget_select, detect_chunks, render_budget_output};
    use crate::core::cognitive_gate::basic_science_enabled;

    let RenderCtx {
        file_ref,
        short,
        ext,
        file_path,
        original_tokens,
        line_count,
        ..
    } = ctx;

    if !basic_science_enabled() {
        return render_map(content, ctx);
    }

    let chunks = detect_chunks(content, ext);
    if chunks.is_empty() {
        return render_map(content, ctx);
    }

    let selected = {
        use crate::core::cognitive_gate::full_science_enabled;
        if full_science_enabled() {
            wasserstein_select(&chunks, original_tokens)
        } else {
            budget_select(&chunks, None)
        }
    };
    let body = render_budget_output(&chunks, &selected, file_path);

    let output = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
        format!("{file_ref}={short} {line_count}L cognitive\n{body}")
    } else {
        format!("{short} {line_count}L cognitive\n{body}")
    };

    let sent = count_tokens(&output);
    if !monotonic_check(original_tokens, sent) {
        return raw_fallback(file_path, content, original_tokens, sent);
    }
    (
        append_compressed_hint(
            &protocol::append_savings(&output, original_tokens, sent),
            file_path,
        ),
        sent,
    )
}

fn render_mdl(content: &str, ctx: RenderCtx<'_>) -> (String, usize) {
    use crate::core::cognitive_gate::basic_science_enabled;
    use crate::core::mdl_mode::generate_structural_description;

    let RenderCtx {
        file_ref,
        short,
        ext,
        file_path,
        original_tokens,
        line_count,
        ..
    } = ctx;

    if !basic_science_enabled() {
        return render_map(content, ctx);
    }

    let desc = generate_structural_description(content, file_path, ext);
    let body = desc.render();

    let output = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
        format!("{file_ref}={short} {line_count}L mdl\n{body}")
    } else {
        format!("{short} {line_count}L mdl\n{body}")
    };

    let sent = count_tokens(&output);
    if !monotonic_check(original_tokens, sent) {
        return raw_fallback(file_path, content, original_tokens, sent);
    }
    (
        append_compressed_hint(
            &protocol::append_savings(&output, original_tokens, sent),
            file_path,
        ),
        sent,
    )
}

fn render_aggressive(content: &str, ctx: RenderCtx<'_>) -> (String, usize) {
    let RenderCtx {
        file_ref,
        short,
        ext,
        file_path,
        original_tokens,
        line_count,
        ..
    } = ctx;
    // Structured JSON (#936): a redundant array-of-objects compacts far
    // better — and losslessly — through the shared `json_crush` core than
    // generic text pruning, which mangles structure. Fires only when it
    // at least halves the file and shrinks the token count; the exact
    // bytes stay recoverable via a `full`/`raw` re-read.
    if ext == "json"
        && let Some(crushed) = crate::core::json_crush::crush_text_if_beneficial(content)
    {
        let header = build_header(file_ref, short, ext, content, line_count, true);
        let body = format!("{header}\n{crushed}");
        let sent = count_tokens(&body);
        if monotonic_check(original_tokens, sent) {
            let savings = protocol::format_savings(original_tokens, sent);
            return (
                append_compressed_hint(&format!("{body}\n{savings}"), file_path),
                sent,
            );
        }
        return raw_fallback(file_path, content, original_tokens, sent);
    }

    // Tabular data (CSV/TSV, #982): a redundant table hoists its constant
    // columns once through the columnar crusher (lossless); the exact
    // bytes stay recoverable via a `full`/`raw` re-read.
    if let Some(delim) = compressor::tabular_delimiter(Some(ext))
        && let Some(crushed) = crate::core::tabular_crush::crush_text_if_beneficial(content, delim)
    {
        let header = build_header(file_ref, short, ext, content, line_count, true);
        let body = format!("{header}\n{crushed}");
        let sent = count_tokens(&body);
        if monotonic_check(original_tokens, sent) {
            let savings = protocol::format_savings(original_tokens, sent);
            return (
                append_compressed_hint(&format!("{body}\n{savings}"), file_path),
                sent,
            );
        }
        return raw_fallback(file_path, content, original_tokens, sent);
    }

    // YAML (#985): a verbose document compacts losslessly to compact JSON
    // through the shared crusher (formatting dropped, redundant
    // `items`/`list` arrays factored); the exact bytes stay recoverable
    // via a `full`/`raw` re-read.
    if compressor::is_yaml_ext(Some(ext))
        && let Some(crushed) = crate::core::yaml_crush::crush_text_if_beneficial(content)
    {
        let header = build_header(file_ref, short, ext, content, line_count, true);
        let body = format!("{header}\n{crushed}");
        let sent = count_tokens(&body);
        if monotonic_check(original_tokens, sent) {
            let savings = protocol::format_savings(original_tokens, sent);
            return (
                append_compressed_hint(&format!("{body}\n{savings}"), file_path),
                sent,
            );
        }
        return raw_fallback(file_path, content, original_tokens, sent);
    }

    #[cfg(feature = "tree-sitter")]
    let ast_pruned = crate::core::signatures_ts::ast_prune(content, ext);
    #[cfg(not(feature = "tree-sitter"))]
    let ast_pruned: Option<String> = None;

    let base = ast_pruned.as_deref().unwrap_or(content);

    let session_intent =
        crate::core::session::SessionState::load_latest().and_then(|s| s.active_structured_intent);
    let raw = if let Some(ref intent) = session_intent {
        compressor::task_aware_compress(base, Some(ext), intent)
    } else {
        compressor::aggressive_compress(base, Some(ext))
    };
    let compressed = compressor::safeguard_ratio(content, &raw);
    let header = build_header(file_ref, short, ext, content, line_count, true);

    let mut sym = SymbolMap::new();
    let idents = symbol_map::extract_identifiers(&compressed, &[ext]);
    for ident in &idents {
        sym.register(ident);
    }

    if symbol_map::substitution_enabled() && sym.len() >= 3 {
        let sym_table = sym.format_table();
        let sym_applied = sym.apply(&compressed);
        let orig_tok = count_tokens(&compressed);
        let comp_tok = count_tokens(&sym_applied) + count_tokens(&sym_table);
        let net = orig_tok.saturating_sub(comp_tok);
        if orig_tok > 0 && net * 100 / orig_tok >= 5 {
            if !monotonic_check(original_tokens, comp_tok) {
                return raw_fallback(file_path, content, original_tokens, comp_tok);
            }
            let savings = protocol::format_savings(original_tokens, comp_tok);
            return (
                append_compressed_hint(
                    &format!("{header}\n{sym_applied}{sym_table}\n{savings}"),
                    file_path,
                ),
                comp_tok,
            );
        }
        if !monotonic_check(original_tokens, orig_tok) {
            return raw_fallback(file_path, content, original_tokens, orig_tok);
        }
        let savings = protocol::format_savings(original_tokens, orig_tok);
        return (
            append_compressed_hint(&format!("{header}\n{compressed}\n{savings}"), file_path),
            orig_tok,
        );
    }

    let sent = count_tokens(&compressed);
    if !monotonic_check(original_tokens, sent) {
        return raw_fallback(file_path, content, original_tokens, sent);
    }
    let savings = protocol::format_savings(original_tokens, sent);
    (
        append_compressed_hint(&format!("{header}\n{compressed}\n{savings}"), file_path),
        sent,
    )
}

fn render_entropy(content: &str, ctx: RenderCtx<'_>, tuning: &ReadTuning<'_>) -> (String, usize) {
    let RenderCtx {
        file_ref,
        short,
        ext,
        file_path,
        original_tokens,
        line_count,
        task,
        ..
    } = ctx;
    // Query-conditioned IB (#542) — relevance source chain: explicit
    // task param > active session intent > last semantic-search query.
    let task_kws: Vec<String> = task
        .filter(|t| !t.trim().is_empty())
        .map(|t| crate::core::task_relevance::parse_task_hints(t).1)
        .filter(|kws| !kws.is_empty())
        .or_else(|| {
            let session = crate::core::session::SessionState::load_latest()?;
            if let Some(intent) = session.active_structured_intent
                && !intent.keywords.is_empty()
            {
                return Some(intent.keywords);
            }
            let q = session.last_semantic_query?;
            let kws = crate::core::task_relevance::parse_task_hints(&q).1;
            (!kws.is_empty()).then_some(kws)
        })
        .unwrap_or_default();
    let result = match (task_kws.is_empty(), tuning.aggressiveness) {
        // Aggressiveness overrides the learned BPE-entropy threshold for
        // the plain (no task keywords) path; task-conditioned entropy
        // keeps its own relevance-aware thresholds.
        (true, Some(a)) => entropy::entropy_compress_with_threshold(
            content,
            file_path,
            AggressivenessProfile::from_level(a).bpe_entropy,
            tuning.protect,
        ),
        (true, None) => entropy::entropy_compress_adaptive(content, file_path, tuning.protect),
        (false, _) => entropy::entropy_compress_task_conditioned(
            content,
            file_path,
            &task_kws,
            tuning.protect,
        ),
    };
    let avg_h = entropy::analyze_entropy(content).avg_entropy;
    let header = build_header(file_ref, short, ext, content, line_count, false);
    let output = format!(
        "{header} H̄={avg_h:.1}{}\n{}",
        techniques_tag(&result.techniques),
        result.output
    );
    let sent = count_tokens(&output);
    if !monotonic_check(original_tokens, sent) {
        return raw_fallback(file_path, content, original_tokens, sent);
    }
    let savings = protocol::format_savings(original_tokens, sent);
    let compression_ratio = if original_tokens > 0 {
        1.0 - (sent as f64 / original_tokens as f64)
    } else {
        0.0
    };
    crate::core::adaptive_thresholds::report_bandit_outcome_for_path(
        file_path,
        compression_ratio > 0.15,
    );
    (
        append_compressed_hint(&format!("{output}\n{savings}"), file_path),
        sent,
    )
}

/// Renders a task-mode selection as numbered lines with explicit gap markers.
///
/// Task mode returns *fragments*. Delivered bare they read like a small,
/// complete file: nothing says where a fragment sits or that anything was
/// dropped between two adjacent lines, so the view gives a false picture of the
/// source (#1589). `N|` matches the numbering `lines:N-M` and `anchored` use,
/// so the numbers double as coordinates for the follow-up read or patch.
fn render_task_selection(keywords: &[String], selected: &[(usize, &str)]) -> String {
    let mut out = format!("[task: {}]", keywords.join(", "));
    let mut prev_line: Option<usize> = None;
    for (idx, line) in selected {
        let lineno = idx + 1;
        if let Some(prev) = prev_line
            && lineno > prev + 1
        {
            let skipped = lineno - prev - 1;
            out.push_str(&format!("\n… {skipped}L"));
        }
        out.push_str(&format!("\n{lineno}|{line}"));
        prev_line = Some(lineno);
    }
    out
}

fn render_task_mode(content: &str, ctx: RenderCtx<'_>, tuning: &ReadTuning<'_>) -> (String, usize) {
    let RenderCtx {
        file_ref,
        short,
        ext,
        file_path,
        original_tokens,
        line_count,
        task,
        ..
    } = ctx;
    let task_str = task.unwrap_or("");
    if task_str.is_empty() {
        let header = build_header(file_ref, short, ext, content, line_count, true);
        let out = format!("{header}\n{content}\n[task mode: no task set — returned full]");
        let sent = count_tokens(&out);
        return (out, sent);
    }
    let (_files, keywords) = crate::core::task_relevance::parse_task_hints(task_str);
    if keywords.is_empty() {
        let header = build_header(file_ref, short, ext, content, line_count, true);
        let out =
            format!("{header}\n{content}\n[task mode: no keywords extracted — returned full]");
        let sent = count_tokens(&out);
        return (out, sent);
    }
    // #840: small files save too few tokens to justify the risk of dropping
    // relevant content. Degrade to full — the agent gets the complete file
    // for a negligible token cost and avoids the "had to retry with mode=full"
    // round-trip that the issue reported.
    const TASK_MIN_LINES: usize = 250;
    if line_count < TASK_MIN_LINES {
        let header = build_header(file_ref, short, ext, content, line_count, true);
        let out = format!(
            "{header}\n{content}\n[task mode: file below {TASK_MIN_LINES}L threshold — returned full]"
        );
        let sent = count_tokens(&out);
        return (out, sent);
    }
    // Aggressiveness tightens the IB keep-budget; default 0.3 preserved
    // when the knob is unset.
    let ib_budget = tuning.aggressiveness.map_or(0.3, |a| {
        AggressivenessProfile::from_level(a).ib_budget_ratio
    });
    let is_markdown = matches!(ext, "md" | "markdown" | "mdx" | "rst");
    let filtered = if is_markdown {
        crate::core::task_relevance::information_bottleneck_filter_with_headers(
            content,
            &keywords,
            ib_budget,
            tuning.protect,
            true,
        )
    } else {
        // #1589: code fragments need coordinates. Prose does not — markdown
        // keeps its section-header reconstruction instead.
        let selected = crate::core::task_relevance::ib_select(
            content,
            &keywords,
            ib_budget,
            None,
            tuning.protect,
        );
        render_task_selection(&keywords, &selected)
    };
    let filtered_lines = filtered.lines().count();
    let header = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
        format!("{file_ref}={short} {line_count}L [task-filtered: {line_count}→{filtered_lines}]")
    } else {
        format!("{short} {line_count}L [task-filtered: {line_count}→{filtered_lines}]")
    };
    let graph_ctx = if crate::core::profiles::active_profile()
        .output_hints
        .graph_context_block()
    {
        let project_root = detect_project_root(file_path);
        crate::core::graph_context::build_graph_context(
            file_path,
            &project_root,
            Some(crate::core::graph_context::GraphContextOptions::default()),
        )
        .map(|c| crate::core::graph_context::format_graph_context(&c))
        .unwrap_or_default()
    } else {
        String::new()
    };

    let sent = count_tokens(&filtered) + count_tokens(&header) + count_tokens(&graph_ctx);
    if !monotonic_check(original_tokens, sent) {
        return raw_fallback(file_path, content, original_tokens, sent);
    }
    let savings = protocol::format_savings(original_tokens, sent);
    (
        append_compressed_hint(
            &format!("{header}\n{filtered}{graph_ctx}\n{savings}"),
            file_path,
        ),
        sent,
    )
}
