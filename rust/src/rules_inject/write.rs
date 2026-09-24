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
use super::content::rules_content;

/// Resolve the effective profile for a rules-injection target, filtering tools
/// that the target's MCP surface hides. Prevents rules from advertising tools
/// (like `ctx_patch`) that the agent cannot call via `tools/list` (#1008) —
/// including those the user disabled (#1849). Shared with the MCP
/// instructions and with drift detection, so all three name the same tools.
pub(super) fn profile_for_target(cfg: &Config, target_name: &str) -> ToolProfile {
    crate::server::tool_visibility::guidance_profile(cfg, target_name)
}

pub(super) fn inject_rules(target: &RulesTarget) -> Result<RulesResult, String> {
    let cfg = crate::core::config::Config::load();
    let shadow = cfg.shadow_mode;
    let level = CompressionLevel::effective(&cfg);
    let profile = profile_for_target(&cfg, target.name);
    let wrapper = match target.format {
        RulesFormat::SharedMarkdown => Wrapper::Shared,
        RulesFormat::DedicatedMarkdown => Wrapper::Dedicated,
        RulesFormat::CursorMdc => super::content::cursor_wrapper_for_mdc(&target.path),
    };

    let new_content = if target.path.exists() {
        let content = std::fs::read_to_string(&target.path).map_err(|e| e.to_string())?;
        let file = RulesFile::parse(&content);

        if file.has_content()
            && file.is_current()
            && file.block_matches_render(shadow, wrapper, level, &profile)
        {
            return Ok(RulesResult::AlreadyPresent);
        }

        file.merged(shadow, wrapper, level, &profile)
    } else if matches!(target.format, RulesFormat::CursorMdc) {
        rules_content(&target.format, level, wrapper, &profile)
    } else {
        RulesFile::initial(shadow, wrapper, level, &profile)
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
