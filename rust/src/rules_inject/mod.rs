//! Rules + SKILL.md injection for every supported agent, split by concern
//! (GL#440): `content` (payloads), `targets` (agent catalog), `detect`
//! (installation checks), `write` (atomic file surgery), `skills` (SKILL.md).
//! This hub owns the shared types and the orchestration entry points.
//!
//! Content is delegated to `core::rules_canonical` — all rule text lives
//! there as `pub const` sections.  Markers (`START_MARK`, `END_MARK`,
//! `RULES_VERSION`) are also re-exported from canonical.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::core::rules_canonical::RulesFile;
pub use crate::core::rules_canonical::{END_MARK, RULES_VERSION, START_MARK};

mod content;
mod detect;
mod skills;
mod targets;
#[cfg(test)]
mod tests;
mod write;

pub(crate) use content::cursor_mdc_document;
pub use content::{
    GEMINI_DEDICATED_CONTEXT_FILENAME, gemini_dedicated_rules_path, opencode_dedicated_rules_path,
};
pub use skills::{install_all_skills, install_skill_for_agent};

/// Forwarding functions — content is delegated to `core::rules_canonical`.
pub fn canonical_rules_block() -> String {
    let cfg = crate::core::config::Config::load();
    let shadow = cfg.shadow_mode;
    let level = crate::core::config::CompressionLevel::effective(&cfg);
    let profile = crate::core::tool_profiles::ToolProfile::from_config(&cfg);
    crate::core::rules_canonical::render(
        shadow,
        crate::core::rules_canonical::Wrapper::Shared,
        level,
        &profile,
    )
}
pub fn rules_shared_content() -> String {
    canonical_rules_block()
}
pub fn rules_dedicated_markdown() -> String {
    let cfg = crate::core::config::Config::load();
    let shadow = cfg.shadow_mode;
    let level = crate::core::config::CompressionLevel::effective(&cfg);
    let profile = crate::core::tool_profiles::ToolProfile::from_config(&cfg);
    crate::core::rules_canonical::render(
        shadow,
        crate::core::rules_canonical::Wrapper::Dedicated,
        level,
        &profile,
    )
}

/// Long-form rules for the on-demand project `LEAN-CTX.md` (#578). Same
/// config-driven shadow/compression handling as the dedicated render, but the
/// LONGFORM profile with the verbose teaching sections.
pub fn rules_longform_markdown() -> String {
    let cfg = crate::core::config::Config::load();
    let shadow = cfg.shadow_mode;
    let level = crate::core::config::CompressionLevel::effective(&cfg);
    let profile = crate::core::tool_profiles::ToolProfile::from_config(&cfg);
    crate::core::rules_canonical::render(
        shadow,
        crate::core::rules_canonical::Wrapper::Longform,
        level,
        &profile,
    )
}

/// The canonical rules block lean-ctx would write for each target, keyed by the
/// target's display name.
///
/// Drift detection compares against this instead of guessing shared-vs-dedicated
/// from a target's on-disk contents: a freshly synced `SharedMarkdown` file (e.g.
/// Copilot CLI, Codex CLI) carries no user text, which a content heuristic
/// mistook for the dedicated layout and then flagged as drifted on every sync.
/// Keying by the real `RulesFormat` keeps `sync` and `diff` in agreement (#548).
pub fn expected_blocks_by_target(
    home: &std::path::Path,
) -> std::collections::HashMap<String, String> {
    let injection = crate::core::config::Config::load().rules_injection_effective();
    let cfg = crate::core::config::Config::load();
    let shadow = cfg.shadow_mode;
    let level = crate::core::config::CompressionLevel::effective(&cfg);
    let shared = canonical_rules_block();
    let dedicated = rules_dedicated_markdown();
    build_rules_targets(home, injection)
        .into_iter()
        .map(|target| {
            let expected = match target.format {
                RulesFormat::SharedMarkdown => shared.clone(),
                RulesFormat::DedicatedMarkdown => dedicated.clone(),
                // CursorMdc embeds the render verbatim between the markers
                // (frontmatter lives outside them); the wrapper is dynamic —
                // HookCovered on hook-covered installs (GL #1153).
                RulesFormat::CursorMdc => {
                    let profile = crate::core::tool_profiles::ToolProfile::from_config(&cfg);
                    crate::core::rules_canonical::render(
                        shadow,
                        content::cursor_wrapper_for_mdc(&target.path),
                        level,
                        &profile,
                    )
                }
            };
            (target.name.to_string(), expected)
        })
        .collect()
}

use detect::is_tool_detected;
use targets::build_rules_targets;
use write::inject_rules;

struct RulesTarget {
    name: &'static str,
    path: PathBuf,
    format: RulesFormat,
}

enum RulesFormat {
    SharedMarkdown,
    DedicatedMarkdown,
    CursorMdc,
}

#[derive(Debug, Default)]
pub struct InjectResult {
    pub injected: Vec<String>,
    pub updated: Vec<String>,
    pub already: Vec<String>,
    pub errors: Vec<String>,
    pub backed_up: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RulesTargetStatus {
    pub name: String,
    pub detected: bool,
    pub path: String,
    pub state: String,
    pub note: Option<String>,
}

enum RulesResult {
    Updated,
    AlreadyPresent,
}

pub fn inject_all_rules(home: &std::path::Path) -> InjectResult {
    let cfg = crate::core::config::Config::load();
    if cfg.rules_scope_effective() == crate::core::config::RulesScope::Project {
        return InjectResult::default();
    }
    if cfg.rules_injection_effective() == crate::core::config::RulesInjection::Off {
        return InjectResult::default();
    }

    let targets = build_rules_targets(home, cfg.rules_injection_effective());

    let mut result = InjectResult::default();

    if cfg.rules_injection_effective() == crate::core::config::RulesInjection::Shared
        && detect::is_named_agent_ready("Claude Code", home)
    {
        sync_claude_pointer_block(home, &mut result);
    }

    for target in &targets {
        if !is_tool_detected(target, home) {
            continue;
        }
        // GH #1435: skip rules injection when the tool's MCP server is not
        // configured — a rules file that mandates ctx_* tools for an agent
        // that can't reach them is a self-contradiction.
        if !detect::is_mcp_configured(target, home) {
            continue;
        }

        let bak_path = target.path.with_extension(format!(
            "{}.bak",
            target
                .path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
        ));
        let bak_existed_before = bak_path.exists();
        let bak_mtime_before = bak_existed_before
            .then(|| {
                std::fs::metadata(&bak_path)
                    .ok()
                    .and_then(|m| m.modified().ok())
            })
            .flatten();

        match inject_rules(target) {
            Ok(RulesResult::Updated) => {
                result.updated.push(target.name.to_string());
                let bak_is_new = if bak_existed_before {
                    std::fs::metadata(&bak_path)
                        .ok()
                        .and_then(|m| m.modified().ok())
                        != bak_mtime_before
                } else {
                    bak_path.exists()
                };
                if bak_is_new {
                    result
                        .backed_up
                        .push(bak_path.to_string_lossy().to_string());
                }
            }
            Ok(RulesResult::AlreadyPresent) => result.already.push(target.name.to_string()),
            Err(e) => result.errors.push(format!("{}: {e}", target.name)),
        }
    }

    result
}

/// Inject global rules for a single agent (by CLI key like "opencode", "cursor", etc.).
pub fn inject_rules_for_agent(home: &std::path::Path, agent_key: &str) -> InjectResult {
    let cfg = crate::core::config::Config::load();
    if cfg.rules_scope_effective() == crate::core::config::RulesScope::Project {
        return InjectResult::default();
    }
    if cfg.rules_injection_effective() == crate::core::config::RulesInjection::Off {
        return InjectResult::default();
    }

    let targets = build_rules_targets(home, cfg.rules_injection_effective());
    let mut result = InjectResult::default();

    if cfg.rules_injection_effective() == crate::core::config::RulesInjection::Shared
        && is_claude_agent_key(agent_key)
    {
        sync_claude_pointer_block(home, &mut result);
        return result;
    }

    for target in &targets {
        if !match_agent_name(agent_key, target.name) {
            continue;
        }

        let bak_path = target.path.with_extension(format!(
            "{}.bak",
            target
                .path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
        ));
        let bak_existed_before = bak_path.exists();

        match inject_rules(target) {
            Ok(RulesResult::Updated) => {
                result.updated.push(target.name.to_string());
                if !bak_existed_before && bak_path.exists() {
                    result
                        .backed_up
                        .push(bak_path.to_string_lossy().to_string());
                }
            }
            Ok(RulesResult::AlreadyPresent) => result.already.push(target.name.to_string()),
            Err(e) => result.errors.push(format!("{}: {e}", target.name)),
        }
    }

    result
}

fn is_claude_agent_key(agent_key: &str) -> bool {
    matches!(
        agent_key.trim().to_ascii_lowercase().as_str(),
        "claude" | "claude-code" | "claude_code" | "claude code"
    )
}

fn sync_claude_pointer_block(home: &std::path::Path, result: &mut InjectResult) {
    let mode = crate::hooks::recommend_hook_mode("claude");
    match crate::hooks::agents::sync_claude_global_rules_block(home, mode) {
        Ok(true) => result.updated.push("Claude Code".to_string()),
        Ok(false) => result.already.push("Claude Code".to_string()),
        Err(error) => result.errors.push(format!("Claude Code: {error}")),
    }
}

/// Returns `true` if a lean-ctx rules marker is present in *any* supported
/// agent's rules file.
#[must_use]
pub fn any_rules_marker_present(home: &std::path::Path) -> bool {
    use crate::core::config::RulesInjection;
    let mut seen = std::collections::HashSet::new();
    for injection in [RulesInjection::Shared, RulesInjection::Dedicated] {
        for target in build_rules_targets(home, injection) {
            if !seen.insert(target.path.clone()) {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(&target.path)
                && RulesFile::parse(&content).has_content()
            {
                return true;
            }
        }
    }
    false
}

fn match_agent_name(cli_key: &str, target_name: &str) -> bool {
    let needle = cli_key.to_lowercase();
    let tn = target_name.to_lowercase();
    needle.contains(&tn)
        || tn.contains(&needle)
        || (needle.contains("cursor") && tn.contains("cursor"))
        || (needle.contains("claude") && tn.contains("claude"))
        || (needle.contains("codebuddy") && tn.contains("codebuddy"))
        || (needle.contains("windsurf") && tn.contains("windsurf"))
        || (needle.contains("codex") && tn.contains("claude"))
        || (needle.contains("zed") && tn.contains("zed"))
        || (needle.contains("copilot") && tn.contains("copilot"))
        || (needle.contains("jetbrains") && tn.contains("jetbrains"))
        || (needle.contains("kiro") && tn.contains("kiro"))
        || (needle.contains("gemini") && tn.contains("gemini"))
        || (needle == "opencode" && tn.contains("opencode"))
        || (needle == "cline" && tn.contains("cline"))
        || (needle == "roo" && tn.contains("roo"))
        || (needle == "amp" && tn.contains("amp"))
        || (needle == "trae" && tn.contains("trae"))
        || (needle == "amazonq" && tn.contains("amazon"))
        || (needle == "pi" && tn.contains("pi coding"))
        || (needle == "crush" && tn.contains("crush"))
        || (needle == "verdent" && tn.contains("verdent"))
        || (needle == "continue" && tn.contains("continue"))
        || (needle == "qwen" && tn.contains("qwen"))
        || (needle == "antigravity" && tn.contains("antigravity"))
        || (needle == "augment" && tn.contains("augment"))
        || (needle == "openclaw" && tn.contains("openclaw"))
        || (needle == "grok" && tn.contains("grok"))
        || (needle == "vscode" && (tn.contains("vs code") || tn.contains("vscode")))
}

/// Check if the rules file for a given MCP client is up-to-date.
pub fn check_rules_freshness(client_name: &str) -> Option<String> {
    let home = dirs::home_dir()?;
    let injection = crate::core::config::Config::load().rules_injection_effective();
    if injection == crate::core::config::RulesInjection::Off {
        return None;
    }
    let targets = build_rules_targets(&home, injection);

    let matched: Vec<&RulesTarget> = targets
        .iter()
        .filter(|t| match_agent_name(client_name, t.name))
        .collect();

    if matched.is_empty() {
        return None;
    }

    for target in &matched {
        if !target.path.exists() {
            continue;
        }
        let content = std::fs::read_to_string(&target.path).ok()?;
        let file = RulesFile::parse(&content);
        if file.has_content() && !file.is_current() {
            return Some(format!(
                "[RULES OUTDATED] Your {} rules were written by an older lean-ctx version. \
                 Re-read your rules file ({}) or run `lean-ctx setup` to update, \
                 then start a new session for full compatibility.",
                target.name,
                target.path.display()
            ));
        }
    }

    None
}

pub fn collect_rules_status(home: &std::path::Path) -> Vec<RulesTargetStatus> {
    let injection = crate::core::config::Config::load().rules_injection_effective();
    let targets = build_rules_targets(home, injection);
    let mut out = Vec::new();

    for target in &targets {
        let detected = is_tool_detected(target, home);
        let path = target.path.to_string_lossy().to_string();

        let state = if !detected {
            "not_detected".to_string()
        } else if !target.path.exists() {
            "missing".to_string()
        } else {
            match std::fs::read_to_string(&target.path) {
                Ok(content) => {
                    let file = RulesFile::parse(&content);
                    if file.has_content() {
                        if file.is_current() {
                            "up_to_date".to_string()
                        } else {
                            "outdated".to_string()
                        }
                    } else {
                        "present_without_marker".to_string()
                    }
                }
                Err(_) => "read_error".to_string(),
            }
        };

        out.push(RulesTargetStatus {
            name: target.name.to_string(),
            detected,
            path,
            state,
            note: None,
        });
    }

    out
}
