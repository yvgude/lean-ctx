//! The rules/skill payloads lean-ctx injects: shared + dedicated markdown,
//! Cursor MDC frontmatter, and per-agent rule paths.
//!
//! Rule CONTENT is delegated to `core::rules_canonical` — this module only
//! handles format dispatch and the Cursor-specific frontmatter.

use std::path::{Path, PathBuf};

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

/// Frontmatter every Antigravity modular rule needs: a file without a valid
/// `trigger` is silently discarded (antigravity.google/docs/rules).
pub(super) const ANTIGRAVITY_RULE_FRONTMATTER: &str = "---\n\
     trigger: always_on\n\
     description: \"lean-ctx: context compression layer, tool guidance in rule body.\"\n\
     ---\n\n";

/// Pi reaches lean-ctx through the pi-lean-ctx extension, whose own
/// (CLI-backed, always present) tools are `ctx_grep`, `ctx_find` and `ctx_ls`;
/// `ctx_search`, `ctx_glob` and `ctx_tree` exist there only when its MCP
/// bridge lists them. Oh My Pi registers the lean-ctx MCP server itself, so
/// its tool names match the canonical block and it gets no note.
const PI_TOOL_NAMES: &str = "Pi (pi-lean-ctx), for each tool it lists: \
Grep -> ctx_grep(pattern, path), Glob/find -> ctx_find(pattern, path), \
ls -> ctx_ls(path); ctx_search, ctx_glob and ctx_tree also only when listed.";

/// The rendered block `block` with the tool names `target_name`'s host
/// actually exposes, when they differ from the canonical ones. A fixed line
/// per host, so the block stays a deterministic function of the render.
pub(super) fn with_host_tool_names(target_name: &str, block: String) -> String {
    if target_name != "Pi Coding Agent" {
        return block;
    }
    match block.rfind(rc::END_MARK) {
        Some(end) => format!(
            "{}\n{PI_TOOL_NAMES}\n{}",
            block[..end].trim_end(),
            &block[end..]
        ),
        None => block,
    }
}

/// Whether `content` opens with a YAML frontmatter fence. Editors on Windows
/// save CRLF line ends and sometimes a byte-order mark; both still count.
pub(super) fn starts_with_frontmatter(content: &str) -> bool {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    content.starts_with("---\n") || content.starts_with("---\r\n")
}

/// Prefix `content` with the Antigravity frontmatter unless it already
/// starts with a frontmatter block (a user may have tuned the trigger).
pub(super) fn with_antigravity_frontmatter(content: String) -> String {
    if starts_with_frontmatter(&content) {
        content
    } else {
        format!("{ANTIGRAVITY_RULE_FRONTMATTER}{content}")
    }
}

pub fn opencode_dedicated_rules_path(home: &std::path::Path) -> PathBuf {
    home.join(".config/opencode/rules/lean-ctx.md")
}

pub fn antigravity_dedicated_rules_path(home: &std::path::Path) -> PathBuf {
    home.join(".gemini/config/rules/lean-ctx.md")
}

pub fn gemini_dedicated_rules_path(home: &std::path::Path) -> PathBuf {
    home.join(".gemini").join(GEMINI_DEDICATED_CONTEXT_FILENAME)
}

/// The `context.fileName` entry registered for Gemini in dedicated mode.
pub const GEMINI_DEDICATED_CONTEXT_FILENAME: &str = "LEANCTX.md";
