//! Write primitives: version-based detection, content merge, atomic writes.
//! All writes go through `config_io::write_atomic_with_backup`.
//!
//! File parsing and merge logic is delegated to `RulesFile` in
//! `core::rules_canonical` — the single source of truth for marker/version
//! detection and content boundary management.

use crate::core::config::{CompressionLevel, Config};
use crate::core::rules_canonical::{RulesFile, Wrapper};
use crate::core::tool_profiles::ToolProfile;

use super::RulesFormat;

/// Resolve the effective profile for a rules-injection target, filtering tools
/// that the target's MCP surface hides. Prevents rules from advertising tools
/// (like `ctx_patch`) that the agent cannot call via `tools/list` (#1008) —
/// including those the user disabled (#1849). Shared with the MCP
/// instructions and with drift detection, so all three name the same tools.
pub(super) fn profile_for_target(cfg: &Config, target_name: &str) -> ToolProfile {
    crate::core::tool_visibility::guidance_profile(cfg, target_name)
}

fn wrapper_for(target: &RulesTarget) -> Wrapper {
    match target.format {
        RulesFormat::SharedMarkdown => Wrapper::Shared,
        RulesFormat::DedicatedMarkdown | RulesFormat::AntigravityRule => Wrapper::Dedicated,
        // CursorMdc embeds the render verbatim between the markers
        // (frontmatter lives outside them); the wrapper is dynamic —
        // HookCovered on hook-covered installs (GL #1153).
        RulesFormat::CursorMdc => super::content::cursor_wrapper_for_mdc(&target.path),
    }
}

/// The marked block lean-ctx writes for `target`. Setup, sync and drift
/// detection all use this one function, so `rules diff` never reports a
/// block that `rules sync` considers current.
pub(super) fn fresh_block(cfg: &Config, target: &RulesTarget) -> String {
    let profile = profile_for_target(cfg, target.name);
    super::content::with_host_tool_names(
        target.name,
        crate::core::rules_canonical::render(
            cfg.shadow_mode,
            wrapper_for(target),
            CompressionLevel::effective(cfg),
            &profile,
        ),
    )
}

pub(super) fn inject_rules(target: &RulesTarget) -> Result<RulesResult, String> {
    let cfg = crate::core::config::Config::load();
    let fresh = fresh_block(&cfg, target);

    let new_content = if target.path.exists() {
        let content = std::fs::read_to_string(&target.path).map_err(|e| e.to_string())?;
        let file = RulesFile::parse(&content);
        if file.is_unterminated() {
            return Err(format!(
                "{} has a lean-ctx start marker but no end marker; restore the line \
                 `{}` after the lean-ctx block (or delete the block), then rerun setup",
                target.path.display(),
                crate::core::rules_canonical::END_MARK
            ));
        }

        let frontmatter_ok = !matches!(target.format, RulesFormat::AntigravityRule)
            || super::content::starts_with_frontmatter(&content);
        if frontmatter_ok && file.has_content() && file.is_current() && file.block_matches(&fresh) {
            return Ok(RulesResult::AlreadyPresent);
        }

        let merged = file.merged_with(&fresh);
        let merged = if matches!(target.format, RulesFormat::AntigravityRule) {
            super::content::with_antigravity_frontmatter(merged)
        } else {
            merged
        };
        crate::marked_block::with_line_endings_of(&content, merged)
    } else {
        // New file: the same `fresh` block, in the format's document shape.
        match target.format {
            RulesFormat::CursorMdc => super::content::cursor_mdc_document(&fresh),
            RulesFormat::AntigravityRule => super::content::with_antigravity_frontmatter(fresh),
            RulesFormat::SharedMarkdown | RulesFormat::DedicatedMarkdown => fresh,
        }
    };

    ensure_parent(&target.path)?;
    crate::config_io::write_atomic_with_backup(&target.path, &new_content)?;

    Ok(RulesResult::Updated)
}

fn ensure_parent(path: &std::path::Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    Ok(())
}

use super::{RulesResult, RulesTarget};
