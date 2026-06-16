//! Rules + SKILL.md injection for every supported agent, split by concern
//! (GL#440): `content` (payloads), `targets` (agent catalog), `detect`
//! (installation checks), `write` (atomic file surgery), `skills` (SKILL.md).
//! This hub owns the shared types and the orchestration entry points.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const MARKER: &str = "# lean-ctx — Context Engineering Layer";
const END_MARKER: &str = "<!-- /lean-ctx -->";
const RULES_VERSION: &str = "lean-ctx-rules-v12";

pub const RULES_MARKER: &str = MARKER;
pub const RULES_END_MARKER: &str = END_MARKER;
pub const RULES_VERSION_STR: &str = RULES_VERSION;

mod content;
mod detect;
mod skills;
mod targets;
#[cfg(test)]
mod tests;
mod write;

pub use content::{
    GEMINI_DEDICATED_CONTEXT_FILENAME, canonical_rules_block, dedicated_session_summary,
    gemini_dedicated_rules_path, opencode_dedicated_rules_path, rules_dedicated_markdown,
    rules_shared_content,
};
pub use skills::{install_all_skills, install_skill_for_agent};

use detect::is_tool_detected;
use targets::build_rules_targets;
use write::inject_rules;

// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Injection logic
// ---------------------------------------------------------------------------

enum RulesResult {
    Injected,
    Updated,
    AlreadyPresent,
}

pub fn inject_all_rules(home: &std::path::Path) -> InjectResult {
    let cfg = crate::core::config::Config::load();
    if cfg.rules_scope_effective() == crate::core::config::RulesScope::Project {
        return InjectResult::default();
    }
    // `Off`: the host supplies its own steering (or this is a phase-isolated /
    // non-caching harness) — write no rules file at all (#361).
    if cfg.rules_injection_effective() == crate::core::config::RulesInjection::Off {
        return InjectResult::default();
    }

    let targets = build_rules_targets(home, cfg.rules_injection_effective());

    let mut result = InjectResult::default();

    for target in &targets {
        if !is_tool_detected(target, home) {
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
            Ok(RulesResult::Injected) => result.injected.push(target.name.to_string()),
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
/// Used by `init --agent` to ensure global rules are written alongside MCP config.
pub fn inject_rules_for_agent(home: &std::path::Path, agent_key: &str) -> InjectResult {
    let cfg = crate::core::config::Config::load();
    if cfg.rules_scope_effective() == crate::core::config::RulesScope::Project {
        return InjectResult::default();
    }
    // `Off`: skip rule-file injection entirely (host-supplied workflow or
    // phase-isolated / non-caching harness, #361).
    if cfg.rules_injection_effective() == crate::core::config::RulesInjection::Off {
        return InjectResult::default();
    }

    let targets = build_rules_targets(home, cfg.rules_injection_effective());
    let mut result = InjectResult::default();

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
            Ok(RulesResult::Injected) => result.injected.push(target.name.to_string()),
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

/// Returns `true` if a lean-ctx rules marker is present in *any* supported
/// agent's rules file (checking both the shared and dedicated layouts).
///
/// Drift-proof by construction: the path catalog is derived from
/// `build_rules_targets`, the same source the injector writes to, so a newly
/// supported agent is covered automatically. This replaces a hand-maintained
/// list that silently omitted OpenCode and ~18 other agents (#442) — that gap
/// made `SetupConfig::should_inject_rules()` report "no rules present" for
/// OpenCode-only users, so their MCP got registered without the `ctx_*`
/// guidance that makes the model actually call the tools.
#[must_use]
pub fn any_rules_marker_present(home: &std::path::Path) -> bool {
    use crate::core::config::RulesInjection;
    let mut seen = std::collections::HashSet::new();
    for injection in [RulesInjection::Shared, RulesInjection::Dedicated] {
        for target in build_rules_targets(home, injection) {
            if !seen.insert(target.path.clone()) {
                continue;
            }
            if std::fs::read_to_string(&target.path).is_ok_and(|content| content.contains(MARKER)) {
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
        || (needle == "vscode" && (tn.contains("vs code") || tn.contains("vscode")))
}

/// Check if the rules file for a given MCP client is up-to-date.
/// Returns `Some(message)` if rules are stale/missing, `None` if current.
pub fn check_rules_freshness(client_name: &str) -> Option<String> {
    let home = dirs::home_dir()?;
    let injection = crate::core::config::Config::load().rules_injection_effective();
    // `Off`: lean-ctx does not manage a rules file, so it never nags about
    // staleness (#361).
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
        if content.contains(MARKER) && !content.contains(RULES_VERSION) {
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
                    if content.contains(MARKER) {
                        if content.contains(RULES_VERSION) {
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
