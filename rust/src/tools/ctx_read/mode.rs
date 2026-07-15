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

impl std::error::Error for ParseModeError {}

/// Parse the payload of a `lines:` mode (`"5-10"`, `"5-999999"`, or a bare
/// `"5"` meaning "from line 5 to EOF").
fn parse_line_range(payload: &str) -> Result<LineRange, ParseModeError> {
    let malformed = || ParseModeError::Malformed(format!("lines:{payload}"));
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
            "task" => ReadMode::Task,
            "reference" => ReadMode::Reference,
            "auto" => ReadMode::Auto,
            "diff" => ReadMode::Diff,
            other => {
                if let Some(payload) = other.strip_prefix("lines:") {
                    ReadMode::Lines(parse_line_range(payload)?)
                } else if let Some(payload) = other.strip_prefix("anchored:") {
                    ReadMode::Anchored(Some(parse_line_range(payload)?))
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
            ReadMode::Task => "task",
            ReadMode::Reference => "reference",
            ReadMode::Auto => "auto",
            ReadMode::Diff => "diff",
            ReadMode::Anchored(None) => "anchored",
            ReadMode::Anchored(Some(range)) => return write!(f, "anchored:{range}"),
            ReadMode::Lines(range) => return write!(f, "lines:{range}"),
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
        matches!(self, ReadMode::Map | ReadMode::Signatures)
    }

    /// Whole-file views the `#361` anti-inflation raw cap applies to. Selection
    /// and delta views (`lines:`, `reference`, `diff`, `raw`) have view-specific
    /// semantics and are never capped. Replaces `mode_allows_raw_cap`.
    #[must_use]
    pub(crate) fn allows_raw_cap(&self) -> bool {
        !matches!(
            self,
            ReadMode::Lines(_)
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
            ReadMode::Diff | ReadMode::Lines(_) | ReadMode::Anchored(_)
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
        "task",
        "reference",
        "auto",
        "diff",
        "lines:5-10",
        "lines:5-999999",
        "density:0.40",
    ];

    // --- Legacy predicates being replaced (kept verbatim so the equivalence
    // tests below pin behaviour to the exact prior semantics). ---

    fn legacy_is_cacheable(mode: &str) -> bool {
        ["map", "signatures"].contains(&mode)
    }

    fn legacy_allows_raw_cap(mode: &str) -> bool {
        !(mode.starts_with("lines:") || matches!(mode, "reference" | "diff" | "raw" | "anchored"))
    }

    fn legacy_is_lossy_summary(mode: &str) -> bool {
        matches!(
            mode,
            "map" | "signatures" | "aggressive" | "entropy" | "task"
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
