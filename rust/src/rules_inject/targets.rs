//! The rules-target catalog: every supported agent and where its rules live.

use std::path::PathBuf;

use super::content::{gemini_dedicated_rules_path, opencode_dedicated_rules_path};
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
    vec![
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
        RulesTarget {
            name: "Antigravity",
            path: home.join(".gemini/antigravity/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "Pi Coding Agent",
            path: home.join(".pi/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
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
            name: "Codex CLI",
            path: crate::core::home::resolve_codex_dir()
                .unwrap_or_else(|| home.join(".codex"))
                .join("instructions.md"),
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
    ]
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
