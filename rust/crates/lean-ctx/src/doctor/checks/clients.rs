// SPDX-License-Identifier: Apache-2.0
//! Per-client instruction delivery (Claude Code / CodeBuddy 2048-char
//! MCP caps and their CLAUDE.md/CODEBUDDY.md + skill layouts).

#[allow(clippy::wildcard_imports)]
use crate::doctor::common::*;
use crate::doctor::{BOLD, DIM, GREEN, Outcome, RST, YELLOW};

pub(crate) fn claude_truncation_outcome() -> Option<Outcome> {
    let home = dirs::home_dir()?;
    let claude_detected = lean_ctx::core::editor_registry::claude_mcp_json_path(&home).exists()
        || lean_ctx::core::editor_registry::claude_state_dir(&home).exists()
        || claude_binary_exists();

    if !claude_detected {
        return None;
    }

    let cfg = lean_ctx::core::config::Config::load();
    Some(claude_instructions_check(
        &home,
        cfg.rules_scope_effective(),
        cfg.rules_injection_effective(),
    ))
}

/// The "connected without a ctx call" lines: one per client whose last
/// `SESSIONS_PER_CLIENT` MCP sessions all ran at least a minute and ended
/// without a tool call. Empty when no client has such a streak, and for every
/// client when the user declined rule steering (a global setting, not one
/// per client).
pub(crate) fn empty_session_lines() -> Vec<String> {
    let Ok(dir) = lean_ctx::core::data_dir::lean_ctx_data_dir() else {
        return Vec::new();
    };
    let streaks = lean_ctx::core::activity::ClientSessions::in_data_dir(&dir).empty_streaks();
    if streaks.is_empty() {
        return Vec::new();
    }
    let declined = lean_ctx::core::config::Config::load().declines_rule_steering();
    streaks
        .iter()
        .filter_map(|streak| {
            let agent = streak.target()?.agent;
            streak.line(lean_ctx::core::activity::Steering {
                declined,
                shell_hooks: lean_ctx::core::hook_mode::recommend_hook_mode(agent)
                    != lean_ctx::core::hook_mode::HookMode::Mcp,
            })
        })
        .collect()
}

/// [`empty_session_lines`] as unscored advisory doctor lines (`!`, ok in
/// `--json`: an observation, never a failed check).
pub(crate) fn empty_session_outcomes() -> Vec<Outcome> {
    empty_session_lines()
        .into_iter()
        .map(|line| Outcome {
            ok: true,
            line: format!("{BOLD}Client usage{RST}  {YELLOW}{line}{RST}"),
        })
        .collect()
}

pub(crate) fn codebuddy_truncation_outcome() -> Option<Outcome> {
    let home = dirs::home_dir()?;
    let codebuddy_detected = lean_ctx::core::editor_registry::codebuddy_mcp_json_path(&home)
        .exists()
        || lean_ctx::core::editor_registry::codebuddy_state_dir(&home).exists()
        || codebuddy_binary_exists();

    if !codebuddy_detected {
        return None;
    }

    let cfg = lean_ctx::core::config::Config::load();
    Some(codebuddy_instructions_check(
        &home,
        cfg.rules_scope_effective(),
        cfg.rules_injection_effective(),
    ))
}
/// Verify Claude Code receives the full lean-ctx instructions despite the
/// 2048-char MCP instructions cap.
///
/// The v3 layout (GL #555) replaced the always-loaded `~/.claude/rules/lean-ctx.md`
/// with a CLAUDE.md block + on-demand skill — `setup` actively *removes* the rules
/// file. The check therefore accepts every layout `setup` can produce (GH #396:
/// the old check demanded the retired rules file right after setup deleted it,
/// and its suggested fix could not recreate one). Layout detection lives in
/// `common::claude_instructions_state`, shared with `doctor integrations`.
pub(crate) fn claude_instructions_check(
    home: &std::path::Path,
    scope: lean_ctx::core::config::RulesScope,
    injection: lean_ctx::core::config::RulesInjection,
) -> Outcome {
    use crate::doctor::common::ClaudeInstructionsState as S;

    let state = crate::doctor::common::claude_instructions_state(home, scope, injection);
    let line = match state {
        S::ProjectScope => format!(
            "{BOLD}Claude Code instructions{RST}  {GREEN}project scope{RST}  {DIM}(global instructions intentionally absent; project files carry them){RST}"
        ),
        S::InjectionOff => format!(
            "{BOLD}Claude Code instructions{RST}  {GREEN}rules injection off{RST}  {DIM}(instructions intentionally not installed — config rules_injection=off){RST}"
        ),
        S::DedicatedWithSkill => format!(
            "{BOLD}Claude Code instructions{RST}  {GREEN}dedicated injection + skill installed{RST}  {DIM}(SessionStart hook injects instructions){RST}"
        ),
        S::DedicatedMissingSkill => format!(
            "{BOLD}Claude Code instructions{RST}  {YELLOW}lean-ctx skill missing{RST}  {DIM}(run: lean-ctx setup){RST}"
        ),
        S::BlockAndSkill => format!(
            "{BOLD}Claude Code instructions{RST}  {GREEN}CLAUDE.md block + skill installed{RST}  {DIM}(MCP instructions capped at 2048 chars — full content via CLAUDE.md){RST}"
        ),
        S::BlockOnly => format!(
            "{BOLD}Claude Code instructions{RST}  {GREEN}CLAUDE.md block installed{RST}  {DIM}(MCP instructions capped at 2048 chars — full content via CLAUDE.md){RST}"
        ),
        S::LegacyRules => format!(
            "{BOLD}Claude Code instructions{RST}  {GREEN}legacy rules file installed{RST}  {DIM}(next `lean-ctx setup` migrates it to the CLAUDE.md block + skill){RST}"
        ),
        S::Governor => format!(
            "{BOLD}Claude Code instructions{RST}  {GREEN}governor setup{RST}  {DIM}(no MCP tools, no rules block or skill — zero prefix by design){RST}"
        ),
        S::Missing => format!(
            "{BOLD}Claude Code instructions{RST}  {YELLOW}no CLAUDE.md block or rules file found — MCP instructions truncated at 2048 chars{RST}  {DIM}(run: lean-ctx setup){RST}"
        ),
    };
    Outcome {
        ok: state.ok(),
        line,
    }
}
/// CodeBuddy instructions check — mirrors `claude_instructions_check` since
/// CodeBuddy uses the same CODEBUDDY.md block + skill pattern as Claude Code.
pub(crate) fn codebuddy_instructions_check(
    home: &std::path::Path,
    scope: lean_ctx::core::config::RulesScope,
    injection: lean_ctx::core::config::RulesInjection,
) -> Outcome {
    use crate::doctor::common::ClaudeInstructionsState as S;

    let state = crate::doctor::common::codebuddy_instructions_state(home, scope, injection);
    let line = match state {
        S::ProjectScope => format!(
            "{BOLD}CodeBuddy instructions{RST}  {GREEN}project scope{RST}  {DIM}(global instructions intentionally absent; project files carry them){RST}"
        ),
        S::InjectionOff => format!(
            "{BOLD}CodeBuddy instructions{RST}  {GREEN}rules injection off{RST}  {DIM}(instructions intentionally not installed — config rules_injection=off){RST}"
        ),
        S::DedicatedWithSkill => format!(
            "{BOLD}CodeBuddy instructions{RST}  {GREEN}dedicated injection + skill installed{RST}  {DIM}(SessionStart hook injects instructions){RST}"
        ),
        S::DedicatedMissingSkill => format!(
            "{BOLD}CodeBuddy instructions{RST}  {YELLOW}lean-ctx skill missing{RST}  {DIM}(run: lean-ctx setup){RST}"
        ),
        S::BlockAndSkill => format!(
            "{BOLD}CodeBuddy instructions{RST}  {GREEN}CODEBUDDY.md block + skill installed{RST}  {DIM}(MCP instructions capped at 2048 chars — full content via CODEBUDDY.md){RST}"
        ),
        S::BlockOnly => format!(
            "{BOLD}CodeBuddy instructions{RST}  {GREEN}CODEBUDDY.md block installed{RST}  {DIM}(MCP instructions capped at 2048 chars — full content via CODEBUDDY.md){RST}"
        ),
        S::LegacyRules => format!(
            "{BOLD}CodeBuddy instructions{RST}  {GREEN}legacy rules file installed{RST}  {DIM}(next `lean-ctx setup` migrates it to the CODEBUDDY.md block + skill){RST}"
        ),
        S::Governor => format!(
            "{BOLD}CodeBuddy instructions{RST}  {GREEN}governor setup{RST}  {DIM}(no MCP tools — zero prefix by design){RST}"
        ),
        S::Missing => format!(
            "{BOLD}CodeBuddy instructions{RST}  {YELLOW}no CODEBUDDY.md block or rules file found — MCP instructions truncated at 2048 chars{RST}  {DIM}(run: lean-ctx setup){RST}"
        ),
    };
    Outcome {
        ok: state.ok(),
        line,
    }
}
