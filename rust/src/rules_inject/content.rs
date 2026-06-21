//! The rules/skill payloads lean-ctx injects: shared + dedicated markdown,
//! Cursor MDC, the dedicated-mode session summary, and per-agent rule paths.
//!
//! Rule CONTENT is delegated to `core::rules_canonical` — this module only
//! handles file paths, format dispatch, and the session summary.

use std::path::PathBuf;

use super::RulesFormat;

/// Returns rules content for the given format, delegating to the canonical source.
/// Respects `shadow_mode` (native tools transparently intercepted by the plugin):
/// when active, the tool-mapping table is omitted in favour of workflow principles.
pub(super) fn rules_content(format: &RulesFormat) -> String {
    let shadow = crate::core::config::Config::load().shadow_mode;
    match format {
        RulesFormat::SharedMarkdown => {
            crate::core::rules_canonical::shared_rules_with_shadow(
                crate::core::rules_canonical::Mode::Mcp,
                shadow,
            )
        }
        RulesFormat::DedicatedMarkdown => {
            crate::core::rules_canonical::dedicated_rules_with_shadow(
                crate::core::rules_canonical::Mode::Mcp,
                shadow,
            )
        }
        RulesFormat::CursorMdc => RULES_CURSOR_MDC.to_owned(),
    }
}

/// Compact, agent-agnostic tool-mapping summary injected as `SessionStart`
/// `additionalContext` in `rules_injection = "dedicated"` mode.
pub fn dedicated_session_summary() -> &'static str {
    DEDICATED_SESSION_SUMMARY
}

const DEDICATED_SESSION_SUMMARY: &str =
    "lean-ctx is active \u{2014} prefer its tools over native equivalents:
- ctx_read  -> instead of Read/cat/head/tail (cached, 10 modes)
- ctx_shell -> instead of bash/Shell (95+ output-compression patterns)
- ctx_search -> instead of Grep/rg
- ctx_glob   -> instead of Glob/find
- ctx_tree  -> instead of ls/find
Native Edit/Write/Delete stay as-is.
Fire independent ctx_* calls in parallel. ctx_compose FIRST for understanding code.
NEVER use native Read/Grep/Shell when a ctx_* equivalent exists.";

/// Dedicated-mode rules file path for OpenCode.
pub fn opencode_dedicated_rules_path(home: &std::path::Path) -> PathBuf {
    home.join(".config/opencode/rules/lean-ctx.md")
}

/// Dedicated-mode rules file path for Gemini CLI.
pub fn gemini_dedicated_rules_path(home: &std::path::Path) -> PathBuf {
    home.join(".gemini").join(GEMINI_DEDICATED_CONTEXT_FILENAME)
}

/// The `context.fileName` entry registered for Gemini in dedicated mode.
pub const GEMINI_DEDICATED_CONTEXT_FILENAME: &str = "LEANCTX.md";

// Cursor MDC format (dedicated file with frontmatter). Loaded from template.
pub(super) const RULES_CURSOR_MDC: &str = include_str!("../templates/lean-ctx.mdc");
