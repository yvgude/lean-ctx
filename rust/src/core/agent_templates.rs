// SPDX-License-Identifier: Apache-2.0
//! Agent instruction templates the hook installers write (crate
//! lean-ctx-hooks). They live with the engine's other templates
//! (`rust/src/templates`), so every crate that ships them embeds the same
//! bytes from inside its own package.

/// The lean-ctx skill (`SKILL.md`) installed for Claude Code and CodeBuddy.
pub const SKILL_MD: &str = include_str!("../templates/SKILL.md");
/// The skill's install helper script.
pub const SKILL_INSTALL_SH: &str = include_str!("../templates/skill_install.sh");
/// Pi agent rules for MCP and hybrid hook modes.
pub const PI_AGENTS_MD: &str = include_str!("../templates/PI_AGENTS.md");
/// Pi agent rules for replace hook mode.
pub const PI_AGENTS_REPLACE_MD: &str = include_str!("../templates/PI_AGENTS_REPLACE.md");
/// Windsurf rules file.
pub const WINDSURF_RULES: &str = include_str!("../templates/windsurfrules.txt");
