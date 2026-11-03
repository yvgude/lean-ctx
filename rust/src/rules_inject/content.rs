//! The rules/skill payloads lean-ctx injects: shared + dedicated markdown,
//! Cursor MDC frontmatter, and per-agent rule paths.
//!
//! Rule CONTENT is delegated to `core::rules_canonical` — this module only
//! handles format dispatch and the Cursor-specific frontmatter.

use std::path::{Path, PathBuf};

use super::RulesFormat;
use crate::core::config::CompressionLevel;
use crate::core::rules_canonical::{self as rc, Wrapper};

/// The wrapper a Cursor mdc at `mdc_path` should carry (GL #1153): the
/// honest `HookCovered` profile when the sibling `hooks.json` (always under
/// the same `.cursor/` dir) shows lean-ctx compressing the native tools,
/// otherwise the full `Dedicated` mapping. Path-derived so isolated test
/// homes and the real `~/.cursor` behave identically.
pub(super) fn cursor_wrapper_for_mdc(mdc_path: &Path) -> Wrapper {
    let covered = mdc_path
        .ancestors()
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name == std::ffi::OsStr::new(".cursor"))
        })
        .is_some_and(|cursor_dir| {
            crate::core::rules_channel::cursor_hooks_json_covers(&cursor_dir.join("hooks.json"))
        });
    if covered {
        Wrapper::HookCovered
    } else {
        Wrapper::Dedicated
    }
}

/// Wrap a rendered rules body in the Cursor mdc frontmatter. The description
/// stays profile-neutral: whether native tools are replaced (Dedicated) or
/// hook-covered (GL #1153) is stated by the body itself.
pub fn cursor_mdc_document(body: &str) -> String {
    format!(
        "---\n\
         description: \"lean-ctx: context compression layer — \
         tool guidance in rule body.\"\n\
         globs: **/*\n\
         alwaysApply: true\n\
         ---\n\n\
         {body}"
    )
}

pub(super) fn rules_content(
    format: &RulesFormat,
    level: CompressionLevel,
    wrapper: Wrapper,
    tool_profile: &crate::core::tool_profiles::ToolProfile,
) -> String {
    let shadow = crate::core::config::Config::load().shadow_mode;
    match format {
        RulesFormat::SharedMarkdown => rc::render(shadow, Wrapper::Shared, level, tool_profile),
        RulesFormat::DedicatedMarkdown => {
            rc::render(shadow, Wrapper::Dedicated, level, tool_profile)
        }
        RulesFormat::CursorMdc => {
            cursor_mdc_document(&rc::render(shadow, wrapper, level, tool_profile))
        }
    }
}

pub fn opencode_dedicated_rules_path(home: &std::path::Path) -> PathBuf {
    home.join(".config/opencode/rules/lean-ctx.md")
}

pub fn gemini_dedicated_rules_path(home: &std::path::Path) -> PathBuf {
    home.join(".gemini").join(GEMINI_DEDICATED_CONTEXT_FILENAME)
}

/// The `context.fileName` entry registered for Gemini in dedicated mode.
pub const GEMINI_DEDICATED_CONTEXT_FILENAME: &str = "LEANCTX.md";
