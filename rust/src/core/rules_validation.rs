//! Rules-file parsing, validation, and merge helpers.
use crate::core::config::CompressionLevel;

use super::rules_canonical::{END_MARK, RULES_VERSION, START_MARK, Wrapper, render};

/// A parsed lean-ctx rules section from a file on disk.
///
/// Handles version detection, content boundary discovery, and prefix/suffix
/// extraction. This is the only place that parses rule markers.
#[derive(Debug)]
pub struct RulesFile<'a> {
    content: &'a str,
    /// Byte offset of `START_MARK` (or the first old-format marker found).
    start: Option<usize>,
    /// Byte offset of `END_MARK`.
    end: Option<usize>,
    /// Parsed version number (0 if no version comment found).
    version: usize,
}

/// Parse the version number from the first version comment found.
fn parse_version_number(s: &str) -> Option<usize> {
    let prefix = "<!-- version: ";
    let vs = s.find(prefix)?;
    let num_start = vs + prefix.len();
    let end = s[num_start..].find(" -->")?;
    s[num_start..num_start + end].parse().ok()
}

impl<'a> RulesFile<'a> {
    /// Parse `content`, scanning for `START_MARK` and version comment.
    ///
    /// Markers count only as whole lines (GL #1158), the form the writers
    /// emit: a prose mention of `<!-- lean-ctx-rules -->` is user text. The
    /// end marker is the first marker line after the start marker.
    pub fn parse(content: &'a str) -> Self {
        let start_line = crate::marked_block::marker_line_span(content, START_MARK);
        let start = start_line.map(|(s, _)| s);
        let version = start
            .and_then(|s| parse_version_number(&content[s + START_MARK.len()..]))
            .unwrap_or(0);
        let end = start_line.and_then(|(_, after)| {
            crate::marked_block::marker_line_span(&content[after..], END_MARK)
                .map(|(e, _)| after + e)
        });
        RulesFile {
            content,
            start,
            end,
            version,
        }
    }

    /// Whether the file carries any lean-ctx rules content.
    pub fn has_content(&self) -> bool {
        self.start.is_some()
    }

    /// The detected version (0 if no version marker).
    pub fn version(&self) -> usize {
        self.version
    }

    /// A start marker without an end marker after it. Merging such a file
    /// cannot tell the old block from the user text after it, so writers
    /// refuse it instead of guessing.
    pub fn is_unterminated(&self) -> bool {
        self.start.is_some() && self.end.is_none()
    }

    /// Whether the file's version is at least `RULES_VERSION`.
    pub fn is_current(&self) -> bool {
        self.version >= RULES_VERSION
    }

    /// Content before the first `START_MARK`.
    pub fn prefix(&self) -> &'a str {
        self.start.map_or("", |s| self.content[..s].trim())
    }

    /// Content after the block's `END_MARK`. Without an end marker this is
    /// everything after the start marker, so no caller can drop user text.
    pub fn suffix(&self) -> &'a str {
        match (self.start, self.end) {
            (_, Some(e)) => self.content[e + END_MARK.len()..].trim(),
            (Some(s), None) => self.content[s + START_MARK.len()..].trim(),
            (None, None) => "",
        }
    }

    /// The lean-ctx block on disk, when both markers are present.
    fn block(&self) -> Option<&'a str> {
        match (self.start, self.end) {
            (Some(s), Some(e)) if e >= s => Some(&self.content[s..e + END_MARK.len()]),
            _ => None,
        }
    }

    /// Whether the on-disk block matches a fresh `render`.
    pub fn block_matches_render(
        &self,
        shadow: bool,
        wrapper: Wrapper,
        level: CompressionLevel,
        tool_profile: &super::tool_profiles::ToolProfile,
    ) -> bool {
        self.block_matches(&render(shadow, wrapper, level, tool_profile))
    }

    /// Whether the on-disk block equals `fresh`, an already rendered block.
    pub fn block_matches(&self, fresh: &str) -> bool {
        self.block()
            .is_some_and(|block| block.replace("\r\n", "\n").trim() == fresh.trim())
    }

    /// Merge freshly-rendered rules into this file.
    pub fn merged(
        &self,
        shadow: bool,
        wrapper: Wrapper,
        level: CompressionLevel,
        tool_profile: &super::tool_profiles::ToolProfile,
    ) -> String {
        self.merged_with(&render(shadow, wrapper, level, tool_profile))
    }

    /// Merge `fresh`, an already rendered block, into this file, in the
    /// file's line endings.
    pub fn merged_with(&self, fresh: &str) -> String {
        crate::marked_block::with_line_endings_of(self.content, self.merged_lf(fresh))
    }

    fn merged_lf(&self, fresh: &str) -> String {
        if self.start.is_some() {
            let before = self.prefix();
            let after = self.suffix();
            let mut out = String::new();
            if !before.is_empty() {
                out.push_str(before);
                out.push('\n');
                out.push('\n');
            }
            out.push_str(fresh);
            if !after.is_empty() {
                out.push('\n');
                out.push('\n');
                out.push_str(after);
            }
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out
        } else {
            let trimmed = self.content.trim_end();
            let mut out = trimmed.to_string();
            if !out.is_empty() {
                out.push('\n');
                out.push('\n');
            }
            out.push_str(fresh);
            out
        }
    }

    /// Create initial rules content.
    pub fn initial(
        shadow: bool,
        wrapper: Wrapper,
        level: CompressionLevel,
        tool_profile: &super::tool_profiles::ToolProfile,
    ) -> String {
        render(shadow, wrapper, level, tool_profile)
    }

    /// Strip the lean-ctx section, keeping user content before/after (in the
    /// file's line endings).
    pub fn without_section(&self) -> String {
        crate::marked_block::with_line_endings_of(self.content, self.without_section_lf())
    }

    fn without_section_lf(&self) -> String {
        if let Some(start_pos) = self.start {
            let before = self.content[..start_pos].trim();
            let after = self.suffix();
            let mut out = String::new();
            if !before.is_empty() {
                out.push_str(before);
                out.push('\n');
            }
            if !after.is_empty() {
                out.push('\n');
                out.push_str(after);
            }
            out
        } else {
            self.content.to_string()
        }
    }
}
