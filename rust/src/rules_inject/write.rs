//! Write primitives: version-based detection, content merge, atomic writes.
//! All writes go through `config_io::write_atomic_with_backup`.
//!
//! File parsing and merge logic is delegated to `RulesFile` in
//! `core::rules_canonical` — the single source of truth for marker/version
//! detection and content boundary management.

use crate::core::config::CompressionLevel;
use crate::core::rules_canonical::{RulesFile, Wrapper};

use super::RulesFormat;
use super::content::rules_content;

pub(super) fn inject_rules(target: &RulesTarget) -> Result<RulesResult, String> {
    let cfg = crate::core::config::Config::load();
    let shadow = cfg.shadow_mode;
    let level = CompressionLevel::effective(&cfg);
    let wrapper = match target.format {
        RulesFormat::SharedMarkdown => Wrapper::Shared,
        RulesFormat::DedicatedMarkdown | RulesFormat::CursorMdc => Wrapper::Dedicated,
    };

    let new_content = if target.path.exists() {
        let content = std::fs::read_to_string(&target.path).map_err(|e| e.to_string())?;
        let file = RulesFile::parse(&content);

        // Skip the rewrite only when the on-disk block is BOTH version-current
        // AND already byte-identical to a fresh render. A version-only check
        // would leave a `compression_level` / `shadow_mode` / canonical-text
        // change unpropagated whenever RULES_VERSION happens to be unchanged
        // (#548).
        if file.has_content()
            && file.is_current()
            && file.block_matches_render(shadow, wrapper, level)
        {
            return Ok(RulesResult::AlreadyPresent);
        }

        file.merged(shadow, wrapper, level)
    } else {
        // Cursor MDC needs frontmatter; others use canonical directly.
        if matches!(target.format, RulesFormat::CursorMdc) {
            rules_content(&target.format, level)
        } else {
            RulesFile::initial(shadow, wrapper, level)
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
