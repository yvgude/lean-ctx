//! Layer 4: MCP tool description compression.
//!
//! Compresses lean-ctx's 56+ tool descriptions to reduce the token overhead
//! of the initial `tools/list` response. Two modes:
//! - Terse: Natural-language descriptions shortened via abbreviations
//! - Lazy: Only tool name + 1-line summary, full description on-demand

use super::dictionaries::{self, DictLevel};
use crate::core::config::CompressionLevel;

/// Compression mode for tool descriptions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DescriptionMode {
    Full,
    Terse,
    Lazy,
}

impl DescriptionMode {
    #[must_use]
    pub fn from_compression_level(level: &CompressionLevel) -> Self {
        match level {
            CompressionLevel::Off | CompressionLevel::Lite => Self::Full,
            CompressionLevel::Standard => Self::Terse,
            CompressionLevel::Max => Self::Lazy,
        }
    }
}

/// Compresses a single tool description according to the mode.
#[must_use]
pub fn compress_description(name: &str, description: &str, mode: DescriptionMode) -> String {
    match mode {
        DescriptionMode::Full => description.to_string(),
        DescriptionMode::Terse => terse_description(description),
        DescriptionMode::Lazy => lazy_description(name, description),
    }
}

fn terse_description(desc: &str) -> String {
    let abbreviated = dictionaries::apply_dictionaries(desc, DictLevel::General);

    let mut lines: Vec<&str> = abbreviated.lines().collect();

    lines.retain(|line| {
        let trimmed = line.trim();
        !trimmed.is_empty()
            && !trimmed.starts_with("Example")
            && !trimmed.starts_with("Note:")
            && !trimmed.starts_with("See also")
    });

    if lines.len() > 3 {
        lines.truncate(3);
    }

    lines.join("\n")
}

fn lazy_description(name: &str, desc: &str) -> String {
    let mut lines = desc.lines();
    let first_line = lines.next().unwrap_or(name);
    let has_more_lines = lines.next().is_some();
    let truncated = first_line.len() > 80;
    let summary = if truncated {
        format!("{}…", &first_line[..first_line.floor_char_boundary(77)])
    } else {
        first_line.to_string()
    };
    // The "full docs" pointer only earns its tokens when there is actually more
    // to fetch: a truncated first line or additional lines. For a description
    // that already fits on one line, the summary IS the full text (and
    // `ctx_discover_tools` would return the very same first line), so the
    // suffix is pure per-tool overhead and is omitted (#680). With 14 lazy-core
    // tools this trims the fixed default prefix every session pays.
    if truncated || has_more_lines {
        format!("{summary} (use ctx_discover_tools for full docs)")
    } else {
        summary
    }
}

/// Estimates token savings from compressing all tool descriptions.
#[must_use]
pub fn estimate_savings(descriptions: &[(&str, &str)], mode: DescriptionMode) -> (u32, u32) {
    let mut total_before = 0u32;
    let mut total_after = 0u32;

    for (name, desc) in descriptions {
        let before = super::counter::count(desc);
        let compressed = compress_description(name, desc, mode);
        let after = super::counter::count(&compressed);
        total_before += before;
        total_after += after;
    }

    (total_before, total_after)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_mode_unchanged() {
        let desc = "Read a file from disk with caching.";
        assert_eq!(
            compress_description("ctx_read", desc, DescriptionMode::Full),
            desc
        );
    }

    #[test]
    fn terse_mode_abbreviates() {
        let desc = "Read a configuration file from the directory.";
        let result = compress_description("ctx_read", desc, DescriptionMode::Terse);
        assert!(
            result.contains("cfg") || result.contains("dir"),
            "should abbreviate: {result}"
        );
    }

    #[test]
    fn lazy_mode_short() {
        let desc = "Read a file from disk with intelligent caching and compression modes.\nSupports 10 different read modes for optimal token efficiency.";
        let result = compress_description("ctx_read", desc, DescriptionMode::Lazy);
        assert!(
            result.contains("ctx_discover_tools"),
            "lazy should reference ctx_discover_tools"
        );
        assert!(result.lines().count() == 1, "lazy should be 1 line");
    }

    #[test]
    fn lazy_mode_single_line_omits_docs_suffix() {
        // A description that already fits on one line has no hidden docs to
        // fetch, so the "(use ctx_discover_tools…)" pointer is pure overhead and
        // must be dropped — the summary IS the full text (#680).
        let desc = "Directory tree (replaces ls/find). Compact maps.";
        let result = compress_description("ctx_tree", desc, DescriptionMode::Lazy);
        assert_eq!(result, desc, "single-line lazy desc must be verbatim");
        assert!(
            !result.contains("ctx_discover_tools"),
            "single-line lazy desc must not append the docs suffix: {result}"
        );
    }

    #[test]
    fn lazy_mode_truncated_single_line_keeps_suffix() {
        // A first line longer than 80 chars IS truncated, so the pointer to the
        // full docs still earns its place even without a second line.
        let desc = "This is a deliberately long single-line tool description that exceeds eighty characters to force truncation.";
        let result = compress_description("ctx_x", desc, DescriptionMode::Lazy);
        assert!(
            result.contains('…') && result.contains("ctx_discover_tools"),
            "truncated lazy desc keeps the docs suffix: {result}"
        );
    }

    #[test]
    fn mode_from_compression_level() {
        assert_eq!(
            DescriptionMode::from_compression_level(&CompressionLevel::Off),
            DescriptionMode::Full
        );
        assert_eq!(
            DescriptionMode::from_compression_level(&CompressionLevel::Standard),
            DescriptionMode::Terse
        );
        assert_eq!(
            DescriptionMode::from_compression_level(&CompressionLevel::Max),
            DescriptionMode::Lazy
        );
    }

    #[test]
    fn estimate_savings_returns_values() {
        let descs = vec![
            (
                "ctx_read",
                "Read a configuration file from the directory with caching.",
            ),
            (
                "ctx_shell",
                "Execute a shell command with pattern compression.",
            ),
        ];
        let (before, after) = estimate_savings(&descs, DescriptionMode::Terse);
        assert!(before > 0);
        assert!(after > 0);
        assert!(after <= before);
    }
}
