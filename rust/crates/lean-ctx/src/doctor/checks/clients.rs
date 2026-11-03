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
