//! Type-safe `ctx_read` modes (#528 / #509 Phase 2).
//!
//! Historically the read mode travelled through the whole pipeline as a bare
//! `&str` (`"full"`, `"map"`, `"lines:5-10"`, `"density:0.40"`, …) and the
//! knowledge of *which* modes exist — and how each one is classified (cacheable?
//! lossy summary? counts as compressed?) — was duplicated across the registered
//! handler, the read core and `render.rs`. That stringly-typed design let invalid
//! states slip through (silently falling back to `full`) and let the duplicated
//! classifications drift.
//!
//! [`ReadMode`] is the single source of truth for the mode vocabulary:
//!
//! * [`ReadMode::from_str`] parses (and *validates*) the canonical strings.
//! * [`Display`](std::fmt::Display) round-trips **byte-identically** to those
//!   same strings, so the type can be threaded through the typed decision points
//!   without touching the string-mode MCP boundary or `render.rs` (back-compat).
//! * the classification methods ([`ReadMode::is_compressed_cacheable`],
//!   [`ReadMode::allows_raw_cap`], [`ReadMode::is_lossy_summary`],
//!   [`ReadMode::counts_as_compressed`]) replace the scattered `matches!(mode,
//!   …)` predicates, and the test module locks each one to the legacy predicate
//!   it replaces so behaviour can never silently change.

use std::fmt;
use std::str::FromStr;

/// Sentinel `end` meaning "to end of file" — preserved from the historical
/// `lines:N-999999` form so [`Display`](std::fmt::Display) stays byte-stable.
pub(crate) const LINE_RANGE_EOF: u32 = 999_999;

/// A 1-based, inclusive line window (`lines:start-end`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LineRange {
    pub(crate) start: u32,
    pub(crate) end: u32,
}

impl LineRange {
    /// Window `start..=end`. `start` is clamped to ≥ 1 to mirror the handler's
    /// historical `start.max(1)` behaviour (#253).
    #[must_use]
    pub(crate) fn new(start: u32, end: u32) -> Self {
        Self {
            start: start.max(1),
            end,
        }
    }

    /// Window from `start` to end of file (the `lines:N-999999` form).
    #[must_use]
    pub(crate) fn to_eof(start: u32) -> Self {
        Self::new(start, LINE_RANGE_EOF)
    }
}

impl fmt::Display for LineRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.start, self.end)
    }
}

/// The mode a `ctx_read` call resolves to.
///
/// `Density` carries an `f64`, so the enum is `PartialEq` but not `Eq`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ReadMode {
    /// Verbatim, edit-ready (framed) — `"full"`.
    Full,
    /// Headerless, trailing-whitespace-stripped verbatim — `"full-compact"`.
    /// Used by the Read redirect to produce temp files faithful to the
    /// original line structure while saving framing overhead.
    FullCompact,
    /// Verbatim + per-line `N:hh|` hash anchors, edit-ready for `ctx_patch`
    /// (epic #1008) — `"anchored"`, or windowed as `"anchored:N-M"` (#811) so a
    /// bounded anchored read never has to materialize/anchor the whole file.
    Anchored(Option<LineRange>),
    /// Exact bytes, no framing — `"raw"`.
    Raw,
    /// API surface — `"signatures"`.
    Signatures,
    /// Structural outline — `"map"`.
    Map,
    /// Aggressive lossy summary — `"aggressive"`.
    Aggressive,
    /// Entropy-pruned summary — `"entropy"`.
    Entropy,
    /// Semantic chunking (7±2 chunks via Miller's Law) — `"cognitive"`.
    Cognitive,
    /// MDL structural description — `"mdl"`.
    Mdl,
    /// Task-focused summary — `"task"`.
    Task,
    /// One-line pointer/quote — `"reference"`.
    Reference,
    /// Learned/auto mode selection — `"auto"`.
    Auto,
    /// Git delta vs the cached copy — `"diff"`.
    Diff,
    /// Line window — `"lines:start-end"`.
    Lines(LineRange),
    /// Comma multi-select — `"lines:5,10-20"` (#971).
    ///
    /// The parts are validated but kept verbatim rather than re-derived into
    /// `Vec<LineRange>`. The window semantics live in
    /// `render::extract_line_range`, which reads the raw mode string, so a
    /// structured payload here would have no consumer — and it could not
    /// round-trip: a bare `5` selects *line 5*, which no `LineRange` spells
    /// without becoming `5-5`. Validation is what this type owes its callers;
    /// interpretation belongs to the renderer.
    LinesMulti(String),
    /// Tail window — `"lines:-N"`, the last `N` lines of the file (#1759).
    ///
    /// This is what the Bash-hook rewrite turns `tail -N file` into. It used
    /// to be rejected as malformed, so every rewritten `tail` failed with a
    /// parse error instead of returning the last N lines. Like `LinesMulti`
    /// the window is resolved by `render::extract_line_range` — it needs the
    /// file's line count — so only the count is carried here.
    LinesTail(u32),
    /// Target-density compression — `"density:0.NN"`.
    Density(f64),
}

/// Error returned when a string is not a recognised [`ReadMode`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParseModeError {
    /// The string is not any known mode keyword or prefix.
    Unknown(String),
    /// A known prefix (`lines:` / `density:`) with an unparseable payload.
    Malformed(String),
}

impl fmt::Display for ParseModeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseModeError::Unknown(s) => write!(f, "unknown read mode '{s}'"),
            ParseModeError::Malformed(s) => write!(f, "malformed read mode '{s}'"),
        }
    }
}

impl ParseModeError {
    /// Actionable, caller-facing message for a rejected mode string (#1209).
    ///
    /// `Display` states *what* is wrong; this also says *what to type instead* —
    /// critical for the `lines:N:M` colon-typo trap, where a second colon reads
    /// natural (muscle memory from `file:line:col`) but is malformed. Without
    /// this the range failed silently: a small file returned an empty window
    /// ("No lines matched"), a large one bounced to a full-file dump.
    #[must_use]
    pub(crate) fn user_message(&self) -> String {
        match self {
            ParseModeError::Malformed(s) if s.starts_with("lines:") => format!(
                "invalid read mode \"{s}\": expected lines:START-END (dash), e.g. lines:72-92, \
                 lines:-N for the last N lines, or a comma multi-select like lines:5,10-20"
            ),
            ParseModeError::Malformed(s) if s.starts_with("anchored:") => format!(
                "invalid read mode \"{s}\": expected anchored:START-END (dash), e.g. anchored:72-92"
            ),
            ParseModeError::Malformed(s) if s.starts_with("density:") => {
                format!("invalid read mode \"{s}\": expected density:0.NN, e.g. density:0.40")
            }
            ParseModeError::Malformed(s) | ParseModeError::Unknown(s) => format!(
                "invalid read mode \"{s}\": expected one of full, signatures, map, cognitive, mdl, \
                 auto, raw, anchored, reference, diff, lines:N-M, lines:-N, anchored:N-M, \
                 density:0.NN"
            ),
        }
    }
}

impl std::error::Error for ParseModeError {}

/// Validate a comma multi-select payload (`"5,10-20"`) and return it verbatim
/// (#971).
///
/// Each part must be a bare line `N` or a span `N-M`, so garbage still fails
/// exactly as it does for a single range. Before this existed, a comma payload
/// hit [`parse_line_range`]'s `split_once('-')` and left `"622,1214-1218"` for
/// `parse::<u32>()`, making the whole mode unparseable — which silently cost it
/// [`ReadMode::is_precise_pinned_read`] and let bounce-prevention rewrite a
/// window request to `full`.
fn parse_line_multi(payload: &str) -> Result<String, ParseModeError> {
    let malformed = || ParseModeError::Malformed(format!("lines:{payload}"));
    for part in payload.split(',') {
        let part = part.trim();
        if let Some(count) = part.strip_prefix('-') {
            // #1759: a `-N` part is a tail window (the last N lines). It must
            // be recognised before `split_once('-')`, which would otherwise
            // read `-3` as the span `""-"3"` and reject it.
            parse_tail_count(count).ok_or_else(malformed)?;
        } else if let Some((start, end)) = part.split_once('-') {
            start.trim().parse::<u32>().map_err(|_| malformed())?;
            end.trim().parse::<u32>().map_err(|_| malformed())?;
        } else {
            part.parse::<u32>().map_err(|_| malformed())?;
        }
    }
    Ok(payload.to_string())
}

/// Parse the `N` of a tail window (`lines:-N`, #1759).
///
/// Zero is rejected: "the last 0 lines" is never what a caller meant, and a
/// rewritten `tail -0` would otherwise silently return an empty window.
fn parse_tail_count(payload: &str) -> Option<u32> {
    payload.trim().parse::<u32>().ok().filter(|n| *n >= 1)
}

/// Parse the payload of a line-range mode (`"5-10"`, `"5-999999"`, or a bare
/// `"5"` meaning "from line N to EOF"). `whole` is the full original mode string
/// (`"lines:…"` or `"anchored:…"`) so a rejection names the mode the caller
/// actually typed — #1209: an `anchored:` typo must not be reported as `lines:`.
fn parse_line_range(payload: &str, whole: &str) -> Result<LineRange, ParseModeError> {
    let malformed = || ParseModeError::Malformed(whole.to_string());
    if let Some((a, b)) = payload.split_once('-') {
        let start = a.trim().parse::<u32>().map_err(|_| malformed())?;
        let end = b.trim().parse::<u32>().map_err(|_| malformed())?;
        Ok(LineRange::new(start, end))
    } else {
        // A bare `lines:N` means "from line N to EOF".
        let start = payload.trim().parse::<u32>().map_err(|_| malformed())?;
        Ok(LineRange::to_eof(start))
    }
}

impl FromStr for ReadMode {
    type Err = ParseModeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "full" => ReadMode::Full,
            "full-compact" => ReadMode::FullCompact,
            "anchored" => ReadMode::Anchored(None),
            "raw" => ReadMode::Raw,
            "signatures" => ReadMode::Signatures,
            "map" => ReadMode::Map,
            "aggressive" => ReadMode::Aggressive,
            "entropy" => ReadMode::Entropy,
            "cognitive" => ReadMode::Cognitive,
            "mdl" => ReadMode::Mdl,
            "task" => ReadMode::Task,
            "reference" => ReadMode::Reference,
            "auto" => ReadMode::Auto,
            "diff" => ReadMode::Diff,
            other => {
                if let Some(payload) = other.strip_prefix("lines:") {
                    // #971: a comma payload is a multi-select, not a span.
                    if payload.contains(',') {
                        ReadMode::LinesMulti(parse_line_multi(payload)?)
                    } else if let Some(count) = payload.trim().strip_prefix('-') {
                        // #1759: `lines:-N` is the last N lines — the mode the
                        // Bash-hook rewrite emits for `tail -N file`.
                        ReadMode::LinesTail(
                            parse_tail_count(count)
                                .ok_or_else(|| ParseModeError::Malformed(other.to_string()))?,
                        )
                    } else {
                        ReadMode::Lines(parse_line_range(payload, other)?)
                    }
                } else if let Some(count) = other.trim().strip_prefix('-')
                    && count.chars().all(|c| c.is_ascii_digit())
                    && !count.is_empty()
                {
                    // #1813: the schema documents a bare `-N` tail form, but only
                    // the `lines:-N` spelling was implemented. A bare `-3` fell
                    // through to `Unknown` and was answered with the *head* of
                    // the file — the caller read the wrong end, and on a long
                    // file truncation hid that the tail never arrived. Honour
                    // the documented spelling instead of removing it: `tail -N`
                    // is what a caller reaches for, and `-N` is how they write it.
                    ReadMode::LinesTail(
                        parse_tail_count(count)
                            .ok_or_else(|| ParseModeError::Malformed(other.to_string()))?,
                    )
                } else if let Some(payload) = other.strip_prefix("anchored:") {
                    ReadMode::Anchored(Some(parse_line_range(payload, other)?))
                } else if let Some(payload) = other.strip_prefix("density:") {
                    let target = payload
                        .trim()
                        .parse::<f64>()
                        .map_err(|_| ParseModeError::Malformed(other.to_string()))?;
                    ReadMode::Density(target)
                } else {
                    return Err(ParseModeError::Unknown(other.to_string()));
                }
            }
        })
    }
}

/// Rewrite the documented bare tail spelling `-N` to its canonical `lines:-N`
/// form (#1813).
///
/// The schema has always advertised `-N=tail`, but only `lines:-N` was ever
/// implemented. A bare `-3` therefore failed to parse as a mode and was
/// answered with the *head* of the file, with no header and no warning — the
/// caller read the wrong end, and on a long file the token cap hid that the
/// tail had never been delivered. Silently returning the opposite end is the
/// worst of the available answers, so the documented spelling is honoured
/// rather than removed: `tail -N` is what a caller reaches for, and `-N` is how
/// they write it.
///
/// This lives beside [`ReadMode`] because spelling is what that type owns, and
/// because both entry points — the MCP handler and `lean-ctx read` — must agree.
/// Canonicalizing to one internal form also avoids teaching a second spelling
/// to the render path and cache key, which branch on `starts_with("lines:")` in
/// several places each.
///
/// Only a pure `-<digits>` payload is rewritten. Anything else (`-N-M`, `-x`, a
/// bare `-`) is left alone so it still reaches the normal unknown-mode warning
/// instead of being silently reinterpreted — which is the failure this exists
/// to remove.
#[must_use]
pub(crate) fn canonicalize_tail_mode(mode: &str) -> Option<String> {
    let count = mode.trim().strip_prefix('-')?;
    (!count.is_empty() && count.chars().all(|c| c.is_ascii_digit()))
        .then(|| format!("lines:-{count}"))
}

impl fmt::Display for ReadMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let keyword = match self {
            ReadMode::Full => "full",
            ReadMode::FullCompact => "full-compact",
            ReadMode::Raw => "raw",
            ReadMode::Signatures => "signatures",
            ReadMode::Map => "map",
            ReadMode::Aggressive => "aggressive",
            ReadMode::Entropy => "entropy",
            ReadMode::Cognitive => "cognitive",
            ReadMode::Mdl => "mdl",
            ReadMode::Task => "task",
            ReadMode::Reference => "reference",
            ReadMode::Auto => "auto",
            ReadMode::Diff => "diff",
            ReadMode::Anchored(None) => "anchored",
            ReadMode::Anchored(Some(range)) => return write!(f, "anchored:{range}"),
            ReadMode::Lines(range) => return write!(f, "lines:{range}"),
            ReadMode::LinesMulti(payload) => return write!(f, "lines:{payload}"),
            ReadMode::LinesTail(count) => return write!(f, "lines:-{count}"),
            // Matches the handler's historical `format!("density:{:.2}", …)`.
            ReadMode::Density(target) => return write!(f, "density:{target:.2}"),
        };
        f.write_str(keyword)
    }
}

impl ReadMode {
    /// `map`/`signatures` — the lossy summaries whose rendered body is stored in
    /// the per-file `compressed_outputs` cache. Replaces `is_cacheable_mode`.
    #[must_use]
    pub(crate) fn is_compressed_cacheable(&self) -> bool {
        matches!(self, ReadMode::Map | ReadMode::Signatures | ReadMode::Mdl)
    }

    /// Whole-file views the `#361` anti-inflation raw cap applies to. Selection
    /// and delta views (`lines:`, `reference`, `diff`, `raw`) have view-specific
    /// semantics and are never capped. Replaces `mode_allows_raw_cap`.
    #[must_use]
    pub(crate) fn allows_raw_cap(&self) -> bool {
        !matches!(
            self,
            ReadMode::Lines(_)
                | ReadMode::LinesMulti(_)
                | ReadMode::LinesTail(_)
                | ReadMode::Reference
                | ReadMode::Diff
                | ReadMode::Raw
                // Anchored carries per-line anchors the agent edits against;
                // collapsing to bare bytes on a small file would strip them and
                // defeat the mode, so it opts out of the #361 raw cap.
                | ReadMode::Anchored(_)
        )
    }

    /// Lossy summaries eligible for cross-file block dedup (#…): the body is a
    /// summary, so shared blocks can be elided. Replaces the inline
    /// `dedup_allowed` match.
    #[must_use]
    pub(crate) fn is_lossy_summary(&self) -> bool {
        matches!(
            self,
            ReadMode::Map
                | ReadMode::Signatures
                | ReadMode::Aggressive
                | ReadMode::Entropy
                | ReadMode::Cognitive
                | ReadMode::Mdl
                | ReadMode::Task
        )
    }

    /// Whether a read in this mode counts as "compressed" for bounce/quality
    /// tracking (#538). Only verbatim `full` and the `diff` delta are *not*
    /// compressed. Replaces the inline `!matches!(mode, "full"|"diff"|"lines")`
    /// predicate — a resolved line window is the string `"lines:N-M"`, never the
    /// bare `"lines"`, so that arm was dead and `Lines` stays compressed here.
    #[must_use]
    pub(crate) fn counts_as_compressed(&self) -> bool {
        // `anchored` is lossless (verbatim + anchors), so like `full` it must not
        // count as a "compressed" read for bounce/quality tracking.
        // `full-compact` only strips trailing whitespace — functionally verbatim.
        !matches!(
            self,
            ReadMode::Full | ReadMode::FullCompact | ReadMode::Diff | ReadMode::Anchored(_)
        )
    }

    /// Precise, pinned reads (#843): the caller asked for an exact byte-window
    /// or delta and must get exactly that back. Context-gate overrides
    /// (bounce-prevention, pressure-downgrade, graph/knowledge heuristics)
    /// must never silently reinterpret one of these to e.g. `full` — that
    /// discards the window/anchors/delta the caller explicitly asked for.
    /// Kept as an enum match (not a string-prefix check) so a future mode
    /// variant that's also precise/pinned has to be added here explicitly
    /// instead of silently falling through an allowlist.
    #[must_use]
    pub(crate) fn is_precise_pinned_read(&self) -> bool {
        matches!(
            self,
            ReadMode::Diff
                | ReadMode::Lines(_)
                | ReadMode::LinesMulti(_)
                | ReadMode::LinesTail(_)
                | ReadMode::Anchored(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every canonical mode string the handler/`render.rs` produce or accept.
    const CANONICAL: &[&str] = &[
        "full",
        "full-compact",
        "anchored",
        "raw",
        "signatures",
        "map",
        "aggressive",
        "entropy",
        "cognitive",
        "mdl",
        "task",
        "reference",
        "auto",
        "diff",
        "lines:5-10",
        "lines:5-999999",
        "lines:-3",
        "density:0.40",
    ];

    // --- Legacy predicates being replaced (kept verbatim so the equivalence
    // tests below pin behaviour to the exact prior semantics). ---

    fn legacy_is_cacheable(mode: &str) -> bool {
        ["map", "signatures", "mdl"].contains(&mode)
    }

    fn legacy_allows_raw_cap(mode: &str) -> bool {
        !(mode.starts_with("lines:") || matches!(mode, "reference" | "diff" | "raw" | "anchored"))
    }

    fn legacy_is_lossy_summary(mode: &str) -> bool {
        matches!(
            mode,
            "map" | "signatures" | "aggressive" | "entropy" | "cognitive" | "mdl" | "task"
        )
    }

    fn legacy_counts_as_compressed(mode: &str) -> bool {
        !matches!(
            mode,
            "full" | "full-compact" | "diff" | "lines" | "anchored"
        )
    }

    #[test]
    fn round_trips_every_canonical_mode() {
        for mode in CANONICAL {
            let parsed: ReadMode = mode.parse().expect("canonical mode parses");
            assert_eq!(
                parsed.to_string(),
                *mode,
                "Display must round-trip '{mode}' byte-identically"
            );
        }
    }

    #[test]
    fn classification_matches_legacy_predicates() {
        for mode in CANONICAL {
            let parsed: ReadMode = mode.parse().expect("canonical mode parses");
            assert_eq!(
                parsed.is_compressed_cacheable(),
                legacy_is_cacheable(mode),
                "is_compressed_cacheable diverged for '{mode}'"
            );
            assert_eq!(
                parsed.allows_raw_cap(),
                legacy_allows_raw_cap(mode),
                "allows_raw_cap diverged for '{mode}'"
            );
            assert_eq!(
                parsed.is_lossy_summary(),
                legacy_is_lossy_summary(mode),
                "is_lossy_summary diverged for '{mode}'"
            );
            assert_eq!(
                parsed.counts_as_compressed(),
                legacy_counts_as_compressed(mode),
                "counts_as_compressed diverged for '{mode}'"
            );
        }
    }

    #[test]
    fn unknown_mode_is_rejected_by_from_str() {
        assert_eq!(
            "wat".parse::<ReadMode>(),
            Err(ParseModeError::Unknown("wat".to_string()))
        );
        assert_eq!(
            "".parse::<ReadMode>(),
            Err(ParseModeError::Unknown(String::new()))
        );
    }

    #[test]
    fn malformed_parameterized_modes_are_rejected() {
        assert_eq!(
            "lines:abc".parse::<ReadMode>(),
            Err(ParseModeError::Malformed("lines:abc".to_string()))
        );
        assert_eq!(
            "lines:5-x".parse::<ReadMode>(),
            Err(ParseModeError::Malformed("lines:5-x".to_string()))
        );
        assert_eq!(
            "density:nope".parse::<ReadMode>(),
            Err(ParseModeError::Malformed("density:nope".to_string()))
        );
    }

    #[test]
    fn user_message_names_the_dash_form_for_colon_typo() {
        // #1209: the `lines:44:48` colon typo must yield an actionable message
        // naming the dash form, not a silent empty window or full-file bounce.
        let err = "lines:44:48".parse::<ReadMode>().unwrap_err();
        let msg = err.user_message();
        assert!(msg.contains("lines:44:48"), "echoes the offending input");
        assert!(msg.contains("lines:START-END"), "names the expected form");
        assert!(
            msg.contains("lines:72-92") || msg.contains("dash"),
            "shows a dash example"
        );

        // The same trap on the anchored line-range mode.
        let anchored = "anchored:44:48".parse::<ReadMode>().unwrap_err();
        assert!(anchored.user_message().contains("anchored:START-END"));

        // A wholly unknown keyword still lists the valid vocabulary.
        let unknown = "wat".parse::<ReadMode>().unwrap_err();
        assert!(unknown.user_message().contains("lines:N-M"));
    }

    #[test]
    fn line_range_parses_bounded_unbounded_and_bare() {
        assert_eq!(
            "lines:5-10".parse::<ReadMode>().unwrap(),
            ReadMode::Lines(LineRange::new(5, 10))
        );
        assert_eq!(
            "lines:5-999999".parse::<ReadMode>().unwrap(),
            ReadMode::Lines(LineRange::to_eof(5))
        );
        // A bare `lines:5` means "from line 5 to EOF".
        assert_eq!(
            "lines:5".parse::<ReadMode>().unwrap(),
            ReadMode::Lines(LineRange::to_eof(5))
        );
    }

    // --- #971: comma multi-select ---

    #[test]
    fn comma_multi_select_parses() {
        // The exact payload from #971, plus the form the tool description
        // documents (`lines:5,10-20`) — both used to be Malformed.
        assert_eq!(
            "lines:620-622,1214-1218".parse::<ReadMode>().unwrap(),
            ReadMode::LinesMulti("620-622,1214-1218".to_string())
        );
        assert_eq!(
            "lines:5,10-20".parse::<ReadMode>().unwrap(),
            ReadMode::LinesMulti("5,10-20".to_string())
        );
    }

    #[test]
    fn comma_multi_select_round_trips_byte_identically() {
        // The module contract: Display reproduces the canonical string exactly.
        for mode in ["lines:5,10-20", "lines:620-622,1214-1218", "lines:1,2,3"] {
            assert_eq!(mode.parse::<ReadMode>().unwrap().to_string(), mode);
        }
    }

    #[test]
    fn comma_multi_select_is_pinned_and_never_capped() {
        // The #971 regression in one place: an unparseable mode was treated as
        // not-pinned, which let bounce-prevention rewrite the window to `full`,
        // and opted it into the #361 raw cap that `lines:` must never get. Both
        // must hold for the comma form exactly as for `lines:N-M`.
        let multi: ReadMode = "lines:620-622,1214-1218".parse().unwrap();
        assert!(
            multi.is_precise_pinned_read(),
            "a comma multi-select is still a precise pinned read (#843)"
        );
        assert!(
            !multi.allows_raw_cap(),
            "a comma multi-select is a selection view and is never raw-capped (#361)"
        );
        // Classified identically to the single-range form.
        let single: ReadMode = "lines:620-622".parse().unwrap();
        assert_eq!(
            multi.is_precise_pinned_read(),
            single.is_precise_pinned_read()
        );
        assert_eq!(multi.allows_raw_cap(), single.allows_raw_cap());
        assert_eq!(multi.counts_as_compressed(), single.counts_as_compressed());
        assert_eq!(multi.is_lossy_summary(), single.is_lossy_summary());
        assert_eq!(
            multi.is_compressed_cacheable(),
            single.is_compressed_cacheable()
        );
    }

    #[test]
    fn malformed_multi_select_still_rejected() {
        // Validation must not weaken: garbage in a part fails as before.
        for mode in ["lines:5,abc", "lines:5-x,10", "lines:5,,10", "lines:5,10-"] {
            assert!(
                mode.parse::<ReadMode>().is_err(),
                "'{mode}' must not parse as a valid multi-select"
            );
        }
    }

    // --- #1759: tail window ---

    #[test]
    fn tail_window_parses_and_round_trips() {
        // The exact mode the Bash-hook rewrite emits for `tail -3 file`. It was
        // Malformed before, so the rewrite failed on every invocation.
        assert_eq!(
            "lines:-3".parse::<ReadMode>().unwrap(),
            ReadMode::LinesTail(3)
        );
        assert_eq!(
            "lines:-3".parse::<ReadMode>().unwrap().to_string(),
            "lines:-3"
        );
        assert_eq!(
            "lines:-250".parse::<ReadMode>().unwrap(),
            ReadMode::LinesTail(250)
        );
    }

    #[test]
    fn tail_window_is_pinned_and_never_capped() {
        // Same contract as `lines:N-M`: the caller asked for an exact window and
        // must get exactly that back (#843), and a selection view is never
        // raw-capped (#361).
        let tail: ReadMode = "lines:-3".parse().unwrap();
        let single: ReadMode = "lines:1-3".parse().unwrap();
        assert!(tail.is_precise_pinned_read());
        assert!(!tail.allows_raw_cap());
        assert_eq!(tail.counts_as_compressed(), single.counts_as_compressed());
        assert_eq!(tail.is_lossy_summary(), single.is_lossy_summary());
        assert_eq!(
            tail.is_compressed_cacheable(),
            single.is_compressed_cacheable()
        );
    }

    #[test]
    fn tail_window_rejects_zero_and_garbage() {
        for mode in ["lines:-0", "lines:-", "lines:-x", "lines:--3", "lines:-3-5"] {
            let err = mode
                .parse::<ReadMode>()
                .expect_err("a malformed tail window must not parse");
            assert_eq!(err, ParseModeError::Malformed(mode.to_string()), "{mode}");
            assert!(
                err.user_message().contains("lines:-N"),
                "the message must name the tail form for '{mode}'"
            );
        }
    }

    #[test]
    fn tail_window_is_accepted_inside_a_multi_select() {
        // A `-N` part goes through the same validation as `lines:-N`; it is
        // kept verbatim and interpreted by the renderer like every other part.
        assert_eq!(
            "lines:1-3,-2".parse::<ReadMode>().unwrap(),
            ReadMode::LinesMulti("1-3,-2".to_string())
        );
        assert!("lines:1-3,-0".parse::<ReadMode>().is_err());
    }

    #[test]
    fn line_range_clamps_start_to_one() {
        assert_eq!(LineRange::new(0, 10).start, 1);
    }

    #[test]
    fn anchored_window_parses_and_displays() {
        assert_eq!(
            "anchored:5-10".parse::<ReadMode>().unwrap(),
            ReadMode::Anchored(Some(LineRange::new(5, 10)))
        );
        assert_eq!(
            "anchored:5-10".parse::<ReadMode>().unwrap().to_string(),
            "anchored:5-10"
        );
        assert_eq!(
            "anchored".parse::<ReadMode>().unwrap(),
            ReadMode::Anchored(None)
        );
    }

    #[test]
    fn anchored_window_opts_out_of_raw_cap_and_compressed_count() {
        let windowed: ReadMode = "anchored:5-10".parse().unwrap();
        assert!(!windowed.allows_raw_cap());
        assert!(!windowed.counts_as_compressed());
    }

    #[test]
    fn density_display_normalizes_to_two_decimals() {
        // Parsing is lenient; Display normalizes to the handler's `{:.2}` form so
        // identical reads stay byte-stable (#498 determinism).
        let parsed: ReadMode = "density:0.5".parse().unwrap();
        assert_eq!(parsed, ReadMode::Density(0.5));
        assert_eq!(parsed.to_string(), "density:0.50");
    }

    #[test]
    fn is_precise_pinned_read_covers_diff_lines_and_anchored() {
        for mode in ["diff", "lines:5-10", "anchored", "anchored:5-10"] {
            let parsed: ReadMode = mode.parse().expect("canonical mode parses");
            assert!(
                parsed.is_precise_pinned_read(),
                "'{mode}' must be a precise pinned read"
            );
        }
        for mode in ["full", "map", "signatures", "auto", "task"] {
            let parsed: ReadMode = mode.parse().expect("canonical mode parses");
            assert!(
                !parsed.is_precise_pinned_read(),
                "'{mode}' must not be a precise pinned read"
            );
        }
    }
}
