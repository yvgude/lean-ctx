//! The rules/skill payloads lean-ctx injects: shared + dedicated markdown,
//! Cursor MDC frontmatter, and per-agent rule paths.
//!
//! Rule CONTENT is delegated to `core::rules_canonical` — this module only
//! handles format dispatch and the Cursor-specific frontmatter.

use std::path::PathBuf;

use super::RulesFormat;
use crate::core::config::CompressionLevel;
use crate::core::rules_canonical::{self as rc, Wrapper};

pub(super) fn rules_content(format: &RulesFormat, level: CompressionLevel) -> String {
    let shadow = crate::core::config::Config::load().shadow_mode;
    match format {
        RulesFormat::SharedMarkdown => rc::render(shadow, Wrapper::Shared, level),
        RulesFormat::DedicatedMarkdown => rc::render(shadow, Wrapper::Dedicated, level),
        RulesFormat::CursorMdc => {
            let body = rc::render(shadow, Wrapper::Dedicated, level);
            format!(
                "---\n\
                 description: \"lean-ctx: context compression layer. \
                 Tools replace native Read/Grep/Shell — see rule body.\"\n\
                 globs: **/*\n\
                 alwaysApply: true\n\
                 ---\n\n\
                 {body}"
            )
        }
    }
}

#[must_use]
pub fn opencode_dedicated_rules_path(home: &std::path::Path) -> PathBuf {
    home.join(".config/opencode/rules/lean-ctx.md")
}

#[must_use]
pub fn gemini_dedicated_rules_path(home: &std::path::Path) -> PathBuf {
    home.join(".gemini").join(GEMINI_DEDICATED_CONTEXT_FILENAME)
}

/// The `context.fileName` entry registered for Gemini in dedicated mode.
pub const GEMINI_DEDICATED_CONTEXT_FILENAME: &str = "LEANCTX.md";
