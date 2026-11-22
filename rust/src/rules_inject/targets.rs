//! The rules-target catalog: every supported agent and where its rules live.

use std::path::PathBuf;

use super::content::{
    antigravity_dedicated_rules_path, gemini_dedicated_rules_path, opencode_dedicated_rules_path,
};
use super::{RulesFormat, RulesTarget};

pub(super) fn build_rules_targets(
    home: &std::path::Path,
    injection: crate::core::config::RulesInjection,
) -> Vec<RulesTarget> {
    use crate::core::config::RulesInjection;

    // In dedicated mode the two AGENTS.md/GEMINI.md consumers write to a
    // lean-ctx-owned file instead of the user's shared instruction file;
    // discovery is wired up separately via opencode.json instructions[] /
    // .gemini/settings.json context.fileName (#343).
    // `Off` never reaches here (the inject entry points short-circuit before
    // building targets), but the match must stay exhaustive — treat it as the
    // shared default layout.
    let (gemini_path, gemini_format) = match injection {
        RulesInjection::Dedicated => (
            gemini_dedicated_rules_path(home),
            RulesFormat::DedicatedMarkdown,
        ),
        RulesInjection::Shared | RulesInjection::Off => {
            (home.join(".gemini/GEMINI.md"), RulesFormat::SharedMarkdown)
        }
    };
    let (opencode_path, opencode_format) = match injection {
        RulesInjection::Dedicated => (
            opencode_dedicated_rules_path(home),
            RulesFormat::DedicatedMarkdown,
        ),
        RulesInjection::Shared | RulesInjection::Off => (
            home.join(".config/opencode/AGENTS.md"),
            RulesFormat::SharedMarkdown,
        ),
    };

    // Antigravity: shared mode merges into `~/.gemini/AGENTS.md`, a documented
    // always-on global file (antigravity.google/docs/rules). Dedicated mode
    // never touches that user file: it writes the lean-ctx-owned modular rule
    // `~/.gemini/config/rules/lean-ctx.md`, which needs `trigger: always_on`
    // frontmatter (Antigravity silently drops a rule file without it). The
    // former `~/.gemini/antigravity/rules/` dir is not a documented location.
    let (antigravity_path, antigravity_format) = match injection {
        RulesInjection::Dedicated => (
            antigravity_dedicated_rules_path(home),
            RulesFormat::AntigravityRule,
        ),
        RulesInjection::Shared | RulesInjection::Off => {
            (home.join(".gemini/AGENTS.md"), RulesFormat::SharedMarkdown)
        }
    };

    // NOTE: Claude Code intentionally has NO rules target. Claude loads every
    // rules file without `paths:` frontmatter unconditionally at session start,
    // which duplicated the CLAUDE.md block in every session (12k+ token memory
    // footprints, GL #555). Claude guidance lives in the CLAUDE.md block
    // (hooks/agents/claude.rs) + the on-demand skill; uninstall still removes
    // legacy ~/.claude/rules/lean-ctx.md files from older installs.
    //
    // CodeBuddy follows the exact same pattern as Claude Code: NO rules target.
    // CodeBuddy installs (and auto-loads) the CODEBUDDY.md block every session,
    // so a separate ~/.codebuddy/rules/lean-ctx.md would duplicate it (GL #555/#558).
    // Guidance lives in the CODEBUDDY.md block + the on-demand skill; uninstall
    // still removes legacy ~/.codebuddy/rules/lean-ctx.md files from older installs.
    //
    // CodeWhale (#1402) also has NO rules target, for a different reason: it
    // auto-loads the *project* `AGENTS.md` (plus `CLAUDE.md` /
    // `.claude/instructions.md` as compatibility fallbacks) and has no
    // lean-ctx-writable global instruction file. Its only global hook,
    // `instructions = [...]` in `~/.codewhale/config.toml`, is explicitly
    // user-owned upstream, so lean-ctx must not claim it. Guidance therefore
    // reaches CodeWhale through the shared project `AGENTS.md` block that
    // `core::rules_channel::AGENTS_MD_READERS` already accounts for — writing a
    // `~/.codewhale/AGENTS.md` would create a file CodeWhale never reads.
    //
    // Codex CLI has NO rules target either: Codex reads `$CODEX_HOME/AGENTS.md`
    // (pointer block, hooks/agents/codex.rs), never the legacy `instructions.md`
    // older installs wrote; setup strips that stale block and uninstall removes it.
    let mut targets = vec![
        // --- Shared config files (append-only) ---
        RulesTarget {
            name: "Gemini CLI",
            path: gemini_path,
            format: gemini_format,
        },
        RulesTarget {
            name: "VS Code",
            path: copilot_instructions_path(home),
            format: RulesFormat::SharedMarkdown,
        },
        RulesTarget {
            name: "Copilot CLI",
            path: home.join(".copilot/instructions.md"),
            format: RulesFormat::SharedMarkdown,
        },
        // --- Dedicated lean-ctx rule files ---
        RulesTarget {
            name: "Cursor",
            path: home.join(".cursor/rules/lean-ctx.mdc"),
            format: RulesFormat::CursorMdc,
        },
        RulesTarget {
            name: "Windsurf",
            path: home.join(".codeium/windsurf/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "Zed",
            // OS-aware: Zed's config dir is platform-specific (macOS uses
            // Application Support); keep rules co-located with the MCP config.
            path: crate::core::editor_registry::zed_config_dir(home).join("rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "Cline",
            path: home.join(".cline/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "Roo Code",
            path: home.join(".roo/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "OpenCode",
            path: opencode_path,
            format: opencode_format,
        },
        RulesTarget {
            name: "Continue",
            path: home.join(".continue/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "Amp",
            path: home.join(".ampcoder/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "Qwen Code",
            path: home.join(".qwen/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "Trae",
            path: home.join(".trae/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "Amazon Q Developer",
            path: home.join(".aws/amazonq/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "JetBrains IDEs",
            path: home.join(".jb-rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        // GEMINI.md stays Gemini CLI's carrier, so Antigravity's shared block
        // goes to AGENTS.md, which Gemini CLI does not read by default.
        RulesTarget {
            name: "Antigravity",
            path: antigravity_path,
            format: antigravity_format,
        },
        RulesTarget {
            name: "AWS Kiro",
            path: home.join(".kiro/steering/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "Verdent",
            path: home.join(".verdent/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "Crush",
            path: home.join(".config/crush/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "Augment",
            path: home.join(".augment/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "OpenClaw",
            path: home.join(".openclaw/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "Grok",
            path: home.join(".grok/AGENTS.md"),
            format: RulesFormat::SharedMarkdown,
        },
        // Oh My Pi ships its own user instruction file under its agent dir;
        // lean-ctx merges a marker-delimited block into it and never owns the
        // whole file (that would clobber the user's own guidance).
        RulesTarget {
            name: "Oh My Pi",
            path: crate::core::editor_registry::omp_agents_path(home),
            format: RulesFormat::SharedMarkdown,
        },
        RulesTarget {
            name: "Hermes Agent",
            path: home.join(".hermes/HERMES.md"),
            format: RulesFormat::SharedMarkdown,
        },
        RulesTarget {
            name: "Qoder",
            path: home.join(".qoder/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
    ];
    // Pi loads `<agent-dir>/AGENTS.md` as global context (agent dir
    // `~/.pi/agent`, or `PI_CODING_AGENT_DIR`); it never read `~/.pi/rules/`.
    // Pi loads no lean-ctx-owned file (`APPEND_SYSTEM.md` is user-owned too),
    // so dedicated mode, which never edits shared files, skips Pi entirely.
    if !matches!(injection, RulesInjection::Dedicated) {
        targets.push(RulesTarget {
            name: "Pi Coding Agent",
            path: crate::core::editor_registry::pi_agents_path(home),
            format: RulesFormat::SharedMarkdown,
        });
    }
    targets
}

fn copilot_instructions_path(home: &std::path::Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        return home.join("Library/Application Support/Code/User/github-copilot-instructions.md");
    }
    #[cfg(target_os = "linux")]
    {
        let user_dirs = [
            home.join(".config/Code/User"),
            home.join(".config/Code - Insiders/User"),
            home.join(".vscode-server/data/User"),
        ];
        let user_dir = user_dirs
            .iter()
            .find(|p| p.exists())
            .cloned()
            .unwrap_or_else(|| user_dirs[0].clone());
        return user_dir.join("github-copilot-instructions.md");
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata).join("Code/User/github-copilot-instructions.md");
        }
    }
    #[allow(unreachable_code)]
    home.join(".config/Code/User/github-copilot-instructions.md")
}
