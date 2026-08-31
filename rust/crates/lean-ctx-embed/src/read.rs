//! Read-mode selection — the SDK's stable mirror of the engine's `ctx_read`
//! mode surface. The result type is the shared [`crate::Output`].

use std::fmt;

/// How [`crate::Engine::read`] should render a file.
///
/// Each variant maps to the engine's canonical `mode` string. `Auto` lets the
/// engine pick based on session/task context (and the shared cache, so a
/// re-read collapses to a delta).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ReadMode {
    /// Smart selection from task + cache context. The recommended default.
    #[default]
    Auto,
    /// Verbatim, edit-ready content (framed).
    Full,
    /// Exact bytes, no framing, always a fresh disk read.
    Raw,
    /// API surface only (signatures), via tree-sitter.
    Signatures,
    /// Structural map (headings/symbols), via tree-sitter.
    Map,
    /// Git delta against the working tree.
    Diff,
    /// Quote-friendly reference view.
    Reference,
    /// Task-focused compression.
    Task,
    /// A 1-based inclusive line window.
    Lines { start: u32, end: u32 },
}

impl fmt::Display for ReadMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadMode::Auto => f.write_str("auto"),
            ReadMode::Full => f.write_str("full"),
            ReadMode::Raw => f.write_str("raw"),
            ReadMode::Signatures => f.write_str("signatures"),
            ReadMode::Map => f.write_str("map"),
            ReadMode::Diff => f.write_str("diff"),
            ReadMode::Reference => f.write_str("reference"),
            ReadMode::Task => f.write_str("task"),
            ReadMode::Lines { start, end } => write!(f, "lines:{start}-{end}"),
        }
    }
}
