//! SKILL.md installation for agents with a skills directory.

use std::path::PathBuf;

use super::detect::command_exists;

// ---------------------------------------------------------------------------
// SKILL.md installation
// ---------------------------------------------------------------------------

pub(super) const SKILL_TEMPLATE: &str = include_str!("../templates/SKILL.md");

pub(super) struct SkillTarget {
    agent_key: &'static str,
    display_name: &'static str,
    skill_dir: PathBuf,
}

pub(super) fn build_skill_targets(home: &std::path::Path) -> Vec<SkillTarget> {
    vec![
        SkillTarget {
            agent_key: "claude",
            display_name: "Claude Code",
            skill_dir: crate::core::editor_registry::claude_state_dir(home).join("skills/lean-ctx"),
        },
        SkillTarget {
            agent_key: "codebuddy",
            display_name: "CodeBuddy",
            skill_dir: crate::core::editor_registry::codebuddy_state_dir(home)
                .join("skills/lean-ctx"),
        },
        SkillTarget {
            agent_key: "cursor",
            display_name: "Cursor",
            skill_dir: home.join(".cursor/skills/lean-ctx"),
        },
        SkillTarget {
            agent_key: "codex",
            display_name: "Codex CLI",
            skill_dir: crate::core::home::resolve_codex_dir()
                .unwrap_or_else(|| home.join(".codex"))
                .join("skills/lean-ctx"),
        },
        SkillTarget {
            agent_key: "grok",
            display_name: "Grok",
            skill_dir: home.join(".grok/skills/lean-ctx"),
        },
        SkillTarget {
            agent_key: "copilot",
            display_name: "GitHub Copilot",
            skill_dir: home.join(".copilot/skills/lean-ctx"),
        },
        SkillTarget {
            agent_key: "openclaw",
            display_name: "OpenClaw",
            skill_dir: home.join(".openclaw/skills/lean-ctx"),
        },
        SkillTarget {
            agent_key: "opencode",
            display_name: "OpenCode",
            skill_dir: home.join(".config/opencode/skills/lean-ctx"),
        },
    ]
}

fn is_skill_agent_detected(agent_key: &str, home: &std::path::Path) -> bool {
    match agent_key {
        "claude" => {
            command_exists("claude")
                || crate::core::editor_registry::claude_mcp_json_path(home).exists()
                || crate::core::editor_registry::claude_state_dir(home).exists()
        }
        "codebuddy" => {
            command_exists("codebuddy")
                || crate::core::editor_registry::codebuddy_mcp_json_path(home).exists()
                || crate::core::editor_registry::codebuddy_state_dir(home).exists()
        }
        "cursor" => home.join(".cursor").exists(),
        "codex" => {
            let codex_dir =
                crate::core::home::resolve_codex_dir().unwrap_or_else(|| home.join(".codex"));
            codex_dir.exists() || command_exists("codex")
        }
        "grok" => home.join(".grok").exists() || command_exists("grok"),
        "copilot" => {
            home.join(".copilot").exists()
                || home.join(".copilot/mcp-config.json").exists()
                || command_exists("copilot")
        }
        "openclaw" => home.join(".openclaw").exists() || command_exists("openclaw"),
        "opencode" => home.join(".config/opencode").exists() || command_exists("opencode"),
        _ => false,
    }
}

/// Install SKILL.md for a specific agent. Returns the installed path.
pub fn install_skill_for_agent(home: &std::path::Path, agent_key: &str) -> Result<PathBuf, String> {
    // GH #1526: `rules_injection = off` opts out of every lean-ctx-authored
    // steering file. `install_all_skills` already honors this (GH #361); the
    // single-agent path used by `init --agent`/`setup` missed the same guard.
    if crate::core::config::Config::load().rules_injection_effective()
        == crate::core::config::RulesInjection::Off
    {
        return Err(format!(
            "rules_injection=off — skill install skipped for '{agent_key}'"
        ));
    }
    let targets = build_skill_targets(home);
    let target = targets
        .into_iter()
        .find(|t| t.agent_key == agent_key)
        .ok_or_else(|| format!("No skill target for agent '{agent_key}'"))?;

    let skill_path = target.skill_dir.join("SKILL.md");
    std::fs::create_dir_all(&target.skill_dir).map_err(|e| e.to_string())?;

    if skill_path.exists() {
        let existing = std::fs::read_to_string(&skill_path).unwrap_or_default();
        if existing == SKILL_TEMPLATE {
            return Ok(skill_path);
        }
    }

    crate::config_io::write_atomic_with_backup(&skill_path, SKILL_TEMPLATE)?;
    Ok(skill_path)
}

/// Install SKILL.md for all detected agents.
/// Returns `Vec<(display_name, was_new_or_updated)>`.
pub fn install_all_skills(home: &std::path::Path) -> Vec<(String, bool)> {
    install_skills_where(home, &|_| true)
}

/// SKILL.md files a setup run would write for the agents `allows` admits.
/// Writes nothing.
#[must_use]
pub fn planned_skill_files(
    home: &std::path::Path,
    allows: &dyn Fn(&str) -> bool,
) -> Vec<(String, PathBuf)> {
    if crate::core::config::Config::load().rules_injection_effective()
        == crate::core::config::RulesInjection::Off
    {
        return Vec::new();
    }
    build_skill_targets(home)
        .into_iter()
        .filter(|t| allows(t.agent_key) && is_skill_agent_detected(t.agent_key, home))
        .map(|t| (t.display_name.to_string(), t.skill_dir.join("SKILL.md")))
        .collect()
}

/// [`install_all_skills`] restricted to the agents `allows` admits.
pub fn install_skills_where(
    home: &std::path::Path,
    allows: &dyn Fn(&str) -> bool,
) -> Vec<(String, bool)> {
    // `rules_injection = off`: the user opted out of lean-ctx-authored steering
    // entirely (GH #361). The on-demand SKILL.md is part of that surface, so
    // write none — mirrors `inject_all_rules`'s early return.
    if crate::core::config::Config::load().rules_injection_effective()
        == crate::core::config::RulesInjection::Off
    {
        return Vec::new();
    }
    let targets = build_skill_targets(home);
    let mut results = Vec::new();

    for target in &targets {
        if !allows(target.agent_key) || !is_skill_agent_detected(target.agent_key, home) {
            continue;
        }

        let skill_path = target.skill_dir.join("SKILL.md");
        let already_current = skill_path.exists()
            && std::fs::read_to_string(&skill_path).is_ok_and(|c| c == SKILL_TEMPLATE);

        if already_current {
            results.push((target.display_name.to_string(), false));
            continue;
        }

        if let Err(e) = std::fs::create_dir_all(&target.skill_dir) {
            tracing::warn!(
                "Failed to create skill dir for {}: {e}",
                target.display_name
            );
            continue;
        }

        match crate::config_io::write_atomic_with_backup(&skill_path, SKILL_TEMPLATE) {
            Ok(()) => results.push((target.display_name.to_string(), true)),
            Err(e) => {
                tracing::warn!("Failed to write SKILL.md for {}: {e}", target.display_name);
            }
        }
    }

    results
}
