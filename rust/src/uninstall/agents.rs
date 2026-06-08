use std::fs;
use std::path::{Path, PathBuf};

use super::parsers::{
    remove_lean_ctx_block, remove_lean_ctx_from_json, remove_lean_ctx_from_toml,
    remove_lean_ctx_from_yaml, remove_lean_ctx_login_block,
};
use super::{
    backup_before_modify, copilot_instructions_path, remove_marked_block, safe_remove, safe_write,
    shorten,
};

pub(super) fn remove_project_agent_files(dry_run: bool) -> bool {
    let cwd = std::env::current_dir().unwrap_or_default();
    let agents = cwd.join("AGENTS.md");
    let lean_ctx_md = cwd.join("LEAN-CTX.md");

    const START: &str = "<!-- lean-ctx -->";
    const END: &str = "<!-- /lean-ctx -->";
    const OWNED: &str = "<!-- lean-ctx-owned: PROJECT-LEAN-CTX.md v1 -->";

    let mut removed = false;

    // AGENTS.md: surgical marker-based removal (already correct)
    if agents.exists() {
        if let Ok(content) = fs::read_to_string(&agents) {
            if content.contains(START) {
                let cleaned = remove_marked_block(&content, START, END);
                if cleaned != content {
                    backup_before_modify(&agents, dry_run);
                    if let Err(e) = safe_write(&agents, &cleaned, dry_run) {
                        tracing::warn!("Failed to update project AGENTS.md: {e}");
                    } else {
                        let verb = if dry_run { "Would remove" } else { "✓" };
                        println!("  {verb} Project: removed lean-ctx block from AGENTS.md");
                        removed = true;
                    }
                }
            }
        }
    }

    // LEAN-CTX.md: only delete if we own it
    if lean_ctx_md.exists() {
        if let Ok(content) = fs::read_to_string(&lean_ctx_md) {
            if content.contains(OWNED) {
                if let Err(e) = safe_remove(&lean_ctx_md, dry_run) {
                    tracing::warn!("Failed to remove project LEAN-CTX.md: {e}");
                } else {
                    let verb = if dry_run { "Would remove" } else { "✓" };
                    println!("  {verb} Project: removed LEAN-CTX.md");
                    removed = true;
                }
            }
        }
    }

    // Dedicated lean-ctx files in project: safe to delete entirely
    let dedicated_project_files = [
        ".kiro/steering/lean-ctx.md",
        ".cursor/rules/lean-ctx.mdc",
        ".claude/rules/lean-ctx.md",
    ];
    for rel in &dedicated_project_files {
        let path = cwd.join(rel);
        if path.exists() {
            if let Ok(content) = fs::read_to_string(&path) {
                if content.contains("lean-ctx") {
                    let _ = safe_remove(&path, dry_run);
                    let verb = if dry_run { "Would remove" } else { "✓" };
                    println!("  {verb} Project: removed {rel}");
                    removed = true;
                }
            }
        }
    }

    // Shared project files: surgically remove lean-ctx content, keep user content
    let shared_project_files = [".cursorrules", ".windsurfrules", ".clinerules"];
    for rel in &shared_project_files {
        let path = cwd.join(rel);
        if !path.exists() {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        if !content.contains("lean-ctx") {
            continue;
        }

        let cleaned = remove_lean_ctx_section_from_rules(&content);
        if cleaned.trim().is_empty() {
            backup_before_modify(&path, dry_run);
            let _ = safe_remove(&path, dry_run);
            let verb = if dry_run { "Would remove" } else { "✓" };
            println!("  {verb} Project: removed {rel}");
        } else {
            backup_before_modify(&path, dry_run);
            let _ = safe_write(&path, &cleaned, dry_run);
            let verb = if dry_run { "Would clean" } else { "✓" };
            println!("  {verb} Project: removed lean-ctx content from {rel}");
        }
        removed = true;
    }

    // Project-level MCP/hook JSON files: surgically remove lean-ctx entries
    for (rel, label) in [
        (".vscode/mcp.json", "Project .vscode/mcp.json"),
        (".github/mcp.json", "Project .github/mcp.json"),
        (
            ".github/hooks/hooks.json",
            "Project .github/hooks/hooks.json",
        ),
    ] {
        let path = cwd.join(rel);
        if !path.exists() {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        if !content.contains("lean-ctx") {
            continue;
        }
        backup_before_modify(&path, dry_run);
        // These files use standard MCP JSON format — try hook cleanup which handles both
        removed |= apply_hook_cleanup(&path, label, &content, dry_run);
    }

    // Project-level .claude/settings.local.json: surgically remove lean-ctx hooks
    let claude_settings = cwd.join(".claude/settings.local.json");
    if claude_settings.exists() {
        if let Ok(content) = fs::read_to_string(&claude_settings) {
            if content.contains("lean-ctx") {
                backup_before_modify(&claude_settings, dry_run);
                removed |= apply_hook_cleanup(
                    &claude_settings,
                    "Project .claude/settings.local.json",
                    &content,
                    dry_run,
                );
            }
        }
    }

    removed
}

/// Remove the lean-ctx section from .cursorrules / .windsurfrules / .clinerules.
/// These files have lean-ctx content appended starting with `# lean-ctx`.
/// The content has no end marker, so we remove from the heading to the end of
/// the lean-ctx block (next non-lean-ctx heading or end of file).
pub(super) fn remove_lean_ctx_section_from_rules(content: &str) -> String {
    // If the file has the markdown markers, use marker-based removal
    const MARKER_START: &str = "<!-- lean-ctx -->";
    const MARKER_END: &str = "<!-- /lean-ctx -->";
    if content.contains(MARKER_START) {
        return remove_marked_block(content, MARKER_START, MARKER_END);
    }

    // Otherwise, remove from `# lean-ctx` heading to end of file or next
    // non-lean-ctx heading.
    let mut out = String::with_capacity(content.len());
    let mut in_block = false;

    for line in content.lines() {
        if !in_block && line.starts_with('#') && line.to_lowercase().contains("lean-ctx") {
            in_block = true;
            continue;
        }
        if in_block {
            if line.starts_with('#') && !line.to_lowercase().contains("lean-ctx") {
                in_block = false;
                out.push_str(line);
                out.push('\n');
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }

    // Trim trailing whitespace added by separation
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

// ---------------------------------------------------------------------------
// Shell hook removal
// ---------------------------------------------------------------------------

pub(super) fn remove_shell_hook(home: &Path, dry_run: bool) -> bool {
    let shell = std::env::var("SHELL").unwrap_or_default();
    let mut removed = false;

    if dry_run {
        println!("  Would remove shell hook dropin files (.zshenv.d, .bashenv.d)");
    } else {
        crate::shell_hook::uninstall_all(false);
    }

    let rc_files: Vec<PathBuf> = vec![
        home.join(".zshrc"),
        home.join(".bashrc"),
        // Bash login profiles: may carry the "source ~/.bashrc" snippet init_posix adds.
        home.join(".bash_profile"),
        home.join(".bash_login"),
        home.join(".profile"),
        home.join(".config/fish/config.fish"),
        #[cfg(windows)]
        home.join("Documents/PowerShell/Microsoft.PowerShell_profile.ps1"),
    ];

    for rc in &rc_files {
        if !rc.exists() {
            continue;
        }
        let Ok(content) = fs::read_to_string(rc) else {
            continue;
        };
        if !content.contains("lean-ctx") {
            continue;
        }

        let is_legacy = !content.contains("# lean-ctx shell hook — end");
        let mut cleaned = remove_lean_ctx_block(&content);
        cleaned = remove_source_lines(&cleaned);
        cleaned = remove_lean_ctx_login_block(&cleaned);
        if cleaned.trim() != content.trim() {
            let bak = rc.with_extension("lean-ctx.bak");
            if !dry_run {
                let _ = fs::copy(rc, &bak);
            }
            if let Err(e) = safe_write(rc, &cleaned, dry_run) {
                tracing::warn!("Failed to update {}: {}", rc.display(), e);
            } else {
                let short = shorten(rc, home);
                let verb = if dry_run { "Would remove" } else { "✓" };
                println!("  {verb} Shell hook removed from {short}");
                if !dry_run {
                    println!("    Backup: {}", shorten(&bak, home));
                }
                if is_legacy {
                    println!("    ⚠ Legacy hook (no end marker) — please review {short} manually");
                }
                removed = true;
            }
        }
    }

    let hook_files = [
        "shell-hook.zsh",
        "shell-hook.bash",
        "shell-hook.fish",
        "shell-hook.ps1",
    ];
    let lc_dir = home.join(".lean-ctx");
    for f in &hook_files {
        let path = lc_dir.join(f);
        if path.exists() {
            let _ = safe_remove(&path, dry_run);
            let verb = if dry_run { "Would remove" } else { "✓" };
            println!("  {verb} Removed ~/.lean-ctx/{f}");
            removed = true;
        }
    }

    if !removed && !shell.is_empty() {
        println!("  · No shell hook found");
    }

    removed
}

fn remove_source_lines(content: &str) -> String {
    content
        .lines()
        .filter(|line| {
            !line.contains("lean-ctx/shell-hook.") && !line.contains("lean-ctx\\shell-hook.")
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

// ---------------------------------------------------------------------------
// MCP config removal (JSON / YAML / TOML)
// ---------------------------------------------------------------------------

pub(super) fn remove_mcp_configs(home: &Path, dry_run: bool) -> bool {
    let claude_cfg_dir_json = std::env::var("CLAUDE_CONFIG_DIR").ok().map_or_else(
        || PathBuf::from("/nonexistent"),
        |d| PathBuf::from(d).join(".claude.json"),
    );
    let mut configs: Vec<(&str, PathBuf)> = vec![
        ("Cursor", home.join(".cursor/mcp.json")),
        ("Claude Code (config dir)", claude_cfg_dir_json),
        ("Claude Code (home)", home.join(".claude.json")),
        ("Windsurf", home.join(".codeium/windsurf/mcp_config.json")),
        ("Gemini CLI", home.join(".gemini/settings.json")),
        (
            "Gemini CLI (legacy)",
            home.join(".gemini/settings/mcp.json"),
        ),
        (
            "Antigravity",
            home.join(".gemini/antigravity/mcp_config.json"),
        ),
        (
            "Antigravity CLI",
            home.join(".gemini/antigravity-cli/mcp_config.json"),
        ),
        (
            "Codex CLI",
            crate::core::home::resolve_codex_dir()
                .unwrap_or_else(|| home.join(".codex"))
                .join("config.toml"),
        ),
        ("OpenCode", home.join(".config/opencode/opencode.json")),
        ("Qwen Code", home.join(".qwen/settings.json")),
        ("Qwen Code (legacy)", home.join(".qwen/mcp.json")),
        ("Trae", home.join(".trae/mcp.json")),
        ("Amazon Q Developer", home.join(".aws/amazonq/default.json")),
        (
            "Amazon Q Developer (legacy)",
            home.join(".aws/amazonq/mcp.json"),
        ),
        ("JetBrains IDEs", home.join(".jb-mcp.json")),
        ("AWS Kiro", home.join(".kiro/settings/mcp.json")),
        ("Verdent", home.join(".verdent/mcp.json")),
        ("Amp", home.join(".config/amp/settings.json")),
        ("Crush", home.join(".config/crush/crush.json")),
        ("Pi Coding Agent", home.join(".pi/agent/mcp.json")),
        ("Cline", crate::core::editor_registry::cline_mcp_path()),
        ("Roo Code", crate::core::editor_registry::roo_mcp_path()),
        ("Hermes Agent", home.join(".hermes/config.yaml")),
        ("OpenClaw", home.join(".openclaw/openclaw.json")),
        ("Augment CLI", home.join(".augment/settings.json")),
        (
            "Augment VS Code",
            crate::core::editor_registry::augment_vscode_mcp_path(home),
        ),
        ("Qoder", home.join(".qoder/mcp.json")),
        ("QoderWork", home.join(".qoderwork/mcp.json")),
        ("Aider", home.join(".aider/mcp.json")),
        ("Continue", home.join(".continue/mcp.json")),
        ("Neovim (mcphub)", home.join(".config/mcphub/servers.json")),
        ("Emacs", home.join(".emacs.d/mcp.json")),
        ("Sublime Text", home.join(".config/sublime-text/mcp.json")),
        ("Copilot CLI", home.join(".copilot/mcp-config.json")),
    ];

    // Add platform-specific paths (Qoder macOS Application Support etc.)
    for path in crate::core::editor_registry::qoder_all_mcp_paths(home) {
        if !configs.iter().any(|(_, p)| *p == path) {
            configs.push(("Qoder", path));
        }
    }

    let mut removed = false;

    for (name, path) in &configs {
        if !path.exists() {
            continue;
        }
        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        if !content.contains("lean-ctx") {
            continue;
        }

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let is_yaml = ext == "yaml" || ext == "yml";
        let is_toml = ext == "toml";

        let cleaned = if is_yaml {
            Some(remove_lean_ctx_from_yaml(&content))
        } else if is_toml {
            Some(remove_lean_ctx_from_toml(&content))
        } else {
            remove_lean_ctx_from_json(&content)
        };

        if let Some(cleaned) = cleaned {
            backup_before_modify(path, dry_run);
            if let Err(e) = safe_write(path, &cleaned, dry_run) {
                tracing::warn!("Failed to update {} config: {}", name, e);
            } else {
                let verb = if dry_run { "Would update" } else { "✓" };
                println!("  {verb} MCP config removed from {name}");
                removed = true;
            }
        }
    }

    // Zed: uses `context_servers` key — handled by remove_lean_ctx_from_json
    let zed_path = crate::core::editor_registry::zed_settings_path(home);
    if zed_path.exists() {
        if let Ok(content) = fs::read_to_string(&zed_path) {
            if content.contains("lean-ctx") {
                backup_before_modify(&zed_path, dry_run);
                if let Some(cleaned) = remove_lean_ctx_from_json(&content) {
                    if let Err(e) = safe_write(&zed_path, &cleaned, dry_run) {
                        tracing::warn!("Failed to update Zed config: {e}");
                    } else {
                        let verb = if dry_run { "Would update" } else { "✓" };
                        println!("  {verb} MCP config removed from Zed");
                        removed = true;
                    }
                }
            }
        }
    }

    let vscode_path = crate::core::editor_registry::vscode_mcp_path();
    if vscode_path.exists() {
        if let Ok(content) = fs::read_to_string(&vscode_path) {
            if content.contains("lean-ctx") {
                if let Some(cleaned) = remove_lean_ctx_from_json(&content) {
                    backup_before_modify(&vscode_path, dry_run);
                    if let Err(e) = safe_write(&vscode_path, &cleaned, dry_run) {
                        tracing::warn!("Failed to update VS Code config: {e}");
                    } else {
                        let verb = if dry_run { "Would update" } else { "✓" };
                        println!("  {verb} MCP config removed from VS Code / Copilot");
                        removed = true;
                    }
                }
            }
        }
    }

    removed
}

// ---------------------------------------------------------------------------
// Plan mode settings cleanup
// ---------------------------------------------------------------------------

pub(super) fn remove_plan_mode_settings(_home: &Path, dry_run: bool) -> bool {
    let mut removed = false;

    // VS Code settings.json: remove lean-ctx plan tools from additionalTools array
    if let Some(vscode_settings) = crate::core::editor_registry::plan_mode::vscode_settings_path() {
        if vscode_settings.exists() {
            if let Ok(content) = fs::read_to_string(&vscode_settings) {
                if content.contains("lean-ctx") {
                    if let Ok(mut parsed) = crate::core::jsonc::parse_jsonc(&content) {
                        let mut modified = false;
                        let key = "github.copilot.chat.planAgent.additionalTools";
                        if let Some(tools) = parsed.get_mut(key).and_then(|t| t.as_array_mut()) {
                            let before = tools.len();
                            tools.retain(|t| !t.as_str().is_some_and(|s| s.contains("lean-ctx")));
                            if tools.len() < before {
                                modified = true;
                            }
                        }
                        if modified {
                            backup_before_modify(&vscode_settings, dry_run);
                            if let Ok(cleaned) = serde_json::to_string_pretty(&parsed) {
                                let _ = safe_write(&vscode_settings, &(cleaned + "\n"), dry_run);
                                let verb = if dry_run { "Would clean" } else { "✓" };
                                println!(
                                    "  {verb} VS Code plan mode tools cleaned (other tools preserved)"
                                );
                                removed = true;
                            }
                        }
                    }
                }
            }
        }
    }

    // Claude Code: permissions.allow cleaned via hook cleanup (already handled there)

    removed
}

// ---------------------------------------------------------------------------
// Rules files removal (shared vs dedicated)
// ---------------------------------------------------------------------------

pub(super) fn remove_rules_files(home: &Path, dry_run: bool) -> bool {
    // Dedicated files: entirely owned by lean-ctx — safe to delete
    let dedicated_files: Vec<(&str, PathBuf)> = vec![
        (
            "Claude Code",
            crate::core::editor_registry::claude_rules_dir(home).join("lean-ctx.md"),
        ),
        ("Cursor", home.join(".cursor/rules/lean-ctx.mdc")),
        (
            "Gemini CLI (legacy)",
            home.join(".gemini/rules/lean-ctx.md"),
        ),
        (
            "Gemini CLI (dedicated)",
            crate::rules_inject::gemini_dedicated_rules_path(home),
        ),
        (
            "Codex CLI",
            crate::core::home::resolve_codex_dir()
                .unwrap_or_else(|| home.join(".codex"))
                .join("LEAN-CTX.md"),
        ),
        ("Windsurf", home.join(".codeium/windsurf/rules/lean-ctx.md")),
        (
            "Zed",
            crate::core::editor_registry::zed_config_dir(home).join("rules/lean-ctx.md"),
        ),
        ("Cline", home.join(".cline/rules/lean-ctx.md")),
        ("Roo Code", home.join(".roo/rules/lean-ctx.md")),
        (
            "OpenCode (legacy)",
            home.join(".config/opencode/rules/lean-ctx.md"),
        ),
        ("Continue", home.join(".continue/rules/lean-ctx.md")),
        ("Amp", home.join(".ampcoder/rules/lean-ctx.md")),
        ("Qwen Code", home.join(".qwen/rules/lean-ctx.md")),
        ("Trae", home.join(".trae/rules/lean-ctx.md")),
        (
            "Amazon Q Developer",
            home.join(".aws/amazonq/rules/lean-ctx.md"),
        ),
        ("JetBrains IDEs", home.join(".jb-rules/lean-ctx.md")),
        (
            "Antigravity",
            home.join(".gemini/antigravity/rules/lean-ctx.md"),
        ),
        ("Pi Coding Agent", home.join(".pi/rules/lean-ctx.md")),
        ("AWS Kiro", home.join(".kiro/steering/lean-ctx.md")),
        ("Verdent", home.join(".verdent/rules/lean-ctx.md")),
        ("Crush", home.join(".config/crush/rules/lean-ctx.md")),
        ("OpenClaw", home.join(".openclaw/rules/lean-ctx.md")),
        ("Augment", home.join(".augment/rules/lean-ctx.md")),
        ("Qoder", home.join(".qoder/rules/lean-ctx.md")),
        ("Hermes Agent", home.join(".hermes/rules/lean-ctx.md")),
        (
            "OpenCode Plugin",
            home.join(".config/opencode/plugins/lean-ctx.ts"),
        ),
    ];

    // Shared files: contain user content + lean-ctx block with markers.
    // Only remove the <!-- lean-ctx --> ... <!-- /lean-ctx --> section.
    let shared_files: Vec<(&str, PathBuf)> = vec![
        (
            "Claude Code (legacy)",
            crate::core::editor_registry::claude_state_dir(home).join("CLAUDE.md"),
        ),
        ("Claude Code (legacy home)", home.join(".claude/CLAUDE.md")),
        ("Gemini CLI", home.join(".gemini/GEMINI.md")),
        (
            "Codex CLI",
            crate::core::home::resolve_codex_dir()
                .unwrap_or_else(|| home.join(".codex"))
                .join("instructions.md"),
        ),
        ("VS Code", copilot_instructions_path(home)),
        ("Copilot CLI", home.join(".copilot/instructions.md")),
        ("OpenCode", home.join(".config/opencode/AGENTS.md")),
        (
            "Codex CLI",
            crate::core::home::resolve_codex_dir()
                .unwrap_or_else(|| home.join(".codex"))
                .join("AGENTS.md"),
        ),
        ("Hermes Agent", home.join(".hermes/HERMES.md")),
    ];

    let mut removed = false;

    // --- Dedicated-mode config registrations (#343) ---
    // Remove the auto-load entries we may have written into agent config files
    // (opencode.json `instructions[]`, .gemini/settings.json `context.fileName`).
    // Always attempt regardless of the current rules_injection mode, since a prior
    // dedicated install could have left these behind.
    if dry_run {
        let opencode_cfg = home.join(".config/opencode/opencode.json");
        if fs::read_to_string(&opencode_cfg)
            .is_ok_and(|c| c.contains("lean-ctx") && c.contains("instructions"))
        {
            println!("  Would remove lean-ctx instructions[] entry from OpenCode");
            removed = true;
        }
        let gemini_cfg = home.join(".gemini/settings.json");
        if fs::read_to_string(&gemini_cfg)
            .is_ok_and(|c| c.contains(crate::rules_inject::GEMINI_DEDICATED_CONTEXT_FILENAME))
        {
            println!("  Would remove lean-ctx context.fileName entry from Gemini CLI");
            removed = true;
        }
    } else {
        crate::hooks::agents::unregister_opencode_instructions(home);
        crate::hooks::agents::unregister_gemini_context_filename(home);
    }

    // --- Dedicated: delete if contains lean-ctx ---
    for (name, path) in &dedicated_files {
        if !path.exists() {
            continue;
        }
        if let Ok(content) = fs::read_to_string(path) {
            if content.contains("lean-ctx") {
                if let Err(e) = safe_remove(path, dry_run) {
                    tracing::warn!("Failed to remove {name} rules: {e}");
                } else {
                    let verb = if dry_run { "Would remove" } else { "✓" };
                    println!("  {verb} Rules removed from {name}");
                    removed = true;
                }
            }
        }
    }

    // --- Shared: surgically remove lean-ctx section, keep user content ---
    // Two marker styles exist:
    //   1. Heading-based: `# lean-ctx — Context Engineering Layer` … `<!-- /lean-ctx -->`
    //   2. HTML comments: `<!-- lean-ctx -->` … `<!-- /lean-ctx -->`
    const HEADING_MARKER: &str = "# lean-ctx — Context Engineering Layer";
    const HTML_START: &str = "<!-- lean-ctx -->";
    const HTML_END: &str = "<!-- /lean-ctx -->";

    for (name, path) in &shared_files {
        if !path.exists() {
            continue;
        }
        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        if !content.contains("lean-ctx") {
            continue;
        }

        let cleaned = if content.contains(HEADING_MARKER) && content.contains(HTML_END) {
            remove_marked_block(&content, HEADING_MARKER, HTML_END)
        } else if content.contains(HTML_START) && content.contains(HTML_END) {
            remove_marked_block(&content, HTML_START, HTML_END)
        } else {
            remove_lean_ctx_block_from_md(&content)
        };

        if cleaned.trim().is_empty() {
            backup_before_modify(path, dry_run);
            let _ = safe_remove(path, dry_run);
            let verb = if dry_run { "Would remove" } else { "✓" };
            println!("  {verb} Rules removed from {name} (file was lean-ctx only)");
        } else if cleaned.trim() != content.trim() {
            backup_before_modify(path, dry_run);
            let _ = safe_write(path, &cleaned, dry_run);
            let verb = if dry_run { "Would clean" } else { "✓" };
            println!("  {verb} Rules removed from {name} (user content preserved)");
        }
        removed = true;
    }

    // --- Hermes Agent: block-based removal from shared HERMES.md ---
    let hermes_md = home.join(".hermes/HERMES.md");
    if hermes_md.exists() {
        if let Ok(content) = fs::read_to_string(&hermes_md) {
            if content.contains("lean-ctx") {
                let cleaned = remove_lean_ctx_block_from_md(&content);
                backup_before_modify(&hermes_md, dry_run);
                if cleaned.trim().is_empty() {
                    let _ = safe_remove(&hermes_md, dry_run);
                } else {
                    let _ = safe_write(&hermes_md, &cleaned, dry_run);
                }
                let verb = if dry_run { "Would clean" } else { "✓" };
                println!("  {verb} Rules removed from Hermes Agent");
                removed = true;
            }
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        let project_hermes = cwd.join(".hermes.md");
        if project_hermes.exists() {
            if let Ok(content) = fs::read_to_string(&project_hermes) {
                if content.contains("lean-ctx") {
                    let cleaned = remove_lean_ctx_block_from_md(&content);
                    backup_before_modify(&project_hermes, dry_run);
                    if cleaned.trim().is_empty() {
                        let _ = safe_remove(&project_hermes, dry_run);
                    } else {
                        let _ = safe_write(&project_hermes, &cleaned, dry_run);
                    }
                    let verb = if dry_run { "Would clean" } else { "✓" };
                    println!("  {verb} Rules removed from .hermes.md");
                    removed = true;
                }
            }
        }
    }

    if !removed {
        println!("  · No rules files found");
    }
    removed
}

fn remove_lean_ctx_block_from_md(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut in_block = false;

    for line in content.lines() {
        if !in_block && line.contains("lean-ctx") && line.starts_with('#') {
            in_block = true;
            continue;
        }
        if in_block {
            if line.starts_with('#') && !line.contains("lean-ctx") {
                in_block = false;
                out.push_str(line);
                out.push('\n');
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }

    while out.starts_with('\n') {
        out.remove(0);
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

// ---------------------------------------------------------------------------
// Hook files removal
// ---------------------------------------------------------------------------

/// Apply hook cleanup result to a file: write cleaned content, remove if entirely
/// lean-ctx, or leave untouched on parse error / no changes.
fn apply_hook_cleanup(path: &Path, label: &str, content: &str, dry_run: bool) -> bool {
    let verb = if dry_run { "Would" } else { "✓" };
    match remove_lean_ctx_from_hooks_json(content) {
        HookCleanupResult::Cleaned(cleaned) => {
            if let Err(e) = safe_write(path, &cleaned, dry_run) {
                tracing::warn!("Failed to update {label}: {e}");
                return false;
            }
            println!("  {verb} {label} cleaned (user settings preserved)");
            true
        }
        HookCleanupResult::EntirelyLeanCtx => {
            if let Err(e) = safe_remove(path, dry_run) {
                tracing::warn!("Failed to remove {label}: {e}");
                return false;
            }
            println!("  {verb} {label} removed");
            true
        }
        HookCleanupResult::Unchanged => false,
        HookCleanupResult::ParseError => {
            tracing::warn!("Could not parse {label}, leaving untouched");
            false
        }
    }
}

pub(super) fn remove_hook_files(home: &Path, dry_run: bool) -> bool {
    let claude_hooks_dir = crate::core::editor_registry::claude_state_dir(home).join("hooks");
    let hook_files: Vec<PathBuf> = vec![
        claude_hooks_dir.join("lean-ctx-rewrite.sh"),
        claude_hooks_dir.join("lean-ctx-redirect.sh"),
        claude_hooks_dir.join("lean-ctx-rewrite-native"),
        claude_hooks_dir.join("lean-ctx-redirect-native"),
        home.join(".cursor/hooks/lean-ctx-rewrite.sh"),
        home.join(".cursor/hooks/lean-ctx-redirect.sh"),
        home.join(".cursor/hooks/lean-ctx-rewrite-native"),
        home.join(".cursor/hooks/lean-ctx-redirect-native"),
        home.join(".gemini/hooks/lean-ctx-rewrite-gemini.sh"),
        home.join(".gemini/hooks/lean-ctx-redirect-gemini.sh"),
        home.join(".gemini/hooks/lean-ctx-hook-gemini.sh"),
        crate::core::home::resolve_codex_dir()
            .unwrap_or_else(|| home.join(".codex"))
            .join("hooks/lean-ctx-rewrite-codex.sh"),
        home.join(".codeium/windsurf/hooks/lean-ctx-rewrite.sh"),
        home.join(".codeium/windsurf/hooks/lean-ctx-redirect.sh"),
        home.join(".github/hooks/lean-ctx-rewrite.sh"),
        home.join(".github/hooks/lean-ctx-redirect.sh"),
        home.join(".qoder/hooks/lean-ctx-rewrite.sh"),
        home.join(".qoder/hooks/lean-ctx-redirect.sh"),
    ];

    let mut removed = false;
    for path in &hook_files {
        if path.exists() {
            if let Err(e) = safe_remove(path, dry_run) {
                tracing::warn!("Failed to remove hook {}: {e}", path.display());
            } else {
                removed = true;
            }
        }
    }

    if removed {
        let verb = if dry_run { "Would remove" } else { "✓" };
        println!("  {verb} Hook scripts removed");
    }

    // Claude Code global settings: surgically remove lean-ctx hook entries
    // Both settings.json and settings.local.json can contain hooks
    for claude_settings_name in ["settings.json", "settings.local.json"] {
        let claude_settings =
            crate::core::editor_registry::claude_state_dir(home).join(claude_settings_name);
        if !claude_settings.exists() {
            continue;
        }
        let Ok(content) = fs::read_to_string(&claude_settings) else {
            continue;
        };
        if !content.contains("lean-ctx") {
            continue;
        }
        backup_before_modify(&claude_settings, dry_run);
        removed |= apply_hook_cleanup(
            &claude_settings,
            &format!("Claude Code {claude_settings_name}"),
            &content,
            dry_run,
        );
    }

    // Antigravity CLI (`agy`) installs hooks as a *plugin* under
    // ~/.gemini/config/plugins/lean-ctx (registered in import_manifest.json),
    // not as a hooks block in any settings.json (GH #284). Remove that plugin
    // and its manifest entry surgically.
    let plugin_present = crate::hooks::agents::antigravity_cli_plugin_dir(home).exists();
    if dry_run {
        if plugin_present {
            removed = true;
            println!("  Would remove Antigravity CLI plugin");
        }
    } else if crate::hooks::agents::uninstall_antigravity_cli_plugin(home) {
        removed = true;
        println!("  ✓ Antigravity CLI plugin removed");
    }

    // hooks.json: surgically remove lean-ctx entries instead of deleting
    for (label, hj_path) in [
        ("Cursor", home.join(".cursor/hooks.json")),
        (
            "Codex",
            crate::core::home::resolve_codex_dir()
                .unwrap_or_else(|| home.join(".codex"))
                .join("hooks.json"),
        ),
        ("Windsurf", home.join(".codeium/windsurf/hooks.json")),
        ("Qoder", home.join(".qoder/settings.json")),
        ("Copilot (global)", home.join(".github/hooks/hooks.json")),
        ("Gemini CLI", home.join(".gemini/settings.json")),
    ] {
        if !hj_path.exists() {
            continue;
        }
        let Ok(content) = fs::read_to_string(&hj_path) else {
            continue;
        };
        if !content.contains("lean-ctx") {
            continue;
        }

        backup_before_modify(&hj_path, dry_run);
        removed |= apply_hook_cleanup(&hj_path, label, &content, dry_run);
    }

    removed
}

/// Result of attempting to remove lean-ctx from a JSON config file.
#[derive(Debug)]
pub(super) enum HookCleanupResult {
    /// No lean-ctx references found; file unchanged.
    Unchanged,
    /// lean-ctx entries removed; cleaned JSON content with remaining settings returned.
    Cleaned(String),
    /// File is entirely lean-ctx-only; safe to delete.
    EntirelyLeanCtx,
    /// JSON parse failed; file should NOT be touched.
    ParseError,
}

/// Check if a single string value references lean-ctx.
fn str_is_lean_ctx(s: &str) -> bool {
    s.contains("lean-ctx")
}

/// For flat hook entries: check `command`, `bash`, or any string field for lean-ctx.
fn flat_entry_is_lean_ctx(entry: &serde_json::Value) -> bool {
    let Some(obj) = entry.as_object() else {
        return false;
    };
    for key in ["command", "bash"] {
        if let Some(serde_json::Value::String(s)) = obj.get(key) {
            if str_is_lean_ctx(s) {
                return true;
            }
        }
    }
    false
}

/// For nested hook entries (`{matcher, hooks: [{command: ...}]}`):
/// Remove only lean-ctx sub-hooks, preserving user hooks in the same group.
/// Returns true if the entry was modified or should be removed entirely.
fn clean_nested_entry(entry: &mut serde_json::Value) -> bool {
    let Some(obj) = entry.as_object_mut() else {
        return false;
    };
    let Some(sub_hooks) = obj.get_mut("hooks").and_then(|h| h.as_array_mut()) else {
        return false;
    };
    let before = sub_hooks.len();
    sub_hooks.retain(|h| !flat_entry_is_lean_ctx(h));
    sub_hooks.len() < before
}

/// Remove lean-ctx hook entries from hooks/settings JSON, preserving other entries.
///
/// Handles multiple formats:
/// - Flat: `{command: "lean-ctx ..."}` (Cursor hooks.json)
/// - Nested: `{matcher: "...", hooks: [{type: "command", command: "lean-ctx ..."}]}` (Claude/Codex)
/// - Copilot: `{bash: "lean-ctx ..."}` (Copilot hooks)
pub(super) fn remove_lean_ctx_from_hooks_json(content: &str) -> HookCleanupResult {
    let Ok(mut parsed) = crate::core::jsonc::parse_jsonc(content) else {
        return HookCleanupResult::ParseError;
    };
    let mut modified = false;

    // Clean permissions.allow AND permissions.deny entries like "mcp__lean-ctx__*"
    for perm_key in ["allow", "deny"] {
        if let Some(perms) = parsed
            .get_mut("permissions")
            .and_then(|p| p.get_mut(perm_key))
            .and_then(|a| a.as_array_mut())
        {
            let before = perms.len();
            perms.retain(|p| !p.as_str().is_some_and(|s| s.contains("lean-ctx")));
            if perms.len() < before {
                modified = true;
            }
        }
    }

    if let Some(hooks) = parsed.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        for entries in hooks.values_mut() {
            if let Some(arr) = entries.as_array_mut() {
                let before = arr.len();

                // First: clean sub-hooks inside nested entries
                for entry in arr.iter_mut() {
                    if clean_nested_entry(entry) {
                        modified = true;
                    }
                }

                // Remove entries that are now empty nested groups
                arr.retain(|entry| {
                    if let Some(sub) = entry.get("hooks").and_then(|h| h.as_array()) {
                        if sub.is_empty() {
                            return false;
                        }
                    }
                    true
                });

                // Then: remove flat entries that are lean-ctx
                arr.retain(|entry| {
                    if entry.get("hooks").is_some() {
                        return true; // nested — already handled above
                    }
                    !flat_entry_is_lean_ctx(entry)
                });

                if arr.len() < before {
                    modified = true;
                }
            }
        }
    }

    if !modified {
        return HookCleanupResult::Unchanged;
    }

    // Remove empty event arrays after cleanup
    if let Some(hooks) = parsed.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        hooks.retain(|_, v| v.as_array().is_none_or(|a| !a.is_empty()));
    }

    // Remove empty permissions arrays and empty permissions object
    if let Some(perms) = parsed
        .get_mut("permissions")
        .and_then(|p| p.as_object_mut())
    {
        for key in ["allow", "deny"] {
            if perms
                .get(key)
                .and_then(|a| a.as_array())
                .is_some_and(Vec::is_empty)
            {
                perms.remove(key);
            }
        }
    }
    if parsed
        .get("permissions")
        .and_then(|p| p.as_object())
        .is_some_and(serde_json::Map::is_empty)
    {
        parsed.as_object_mut().map(|o| o.remove("permissions"));
    }

    // Check if any meaningful content remains
    let has_remaining = parsed.as_object().is_some_and(|obj| {
        obj.iter().any(|(key, val)| {
            if key == "hooks" {
                val.as_object().is_some_and(|h| !h.is_empty())
            } else {
                !val.is_null()
            }
        })
    });

    let pretty = match serde_json::to_string_pretty(&parsed) {
        Ok(s) => s + "\n",
        Err(_) => return HookCleanupResult::ParseError,
    };

    if has_remaining {
        HookCleanupResult::Cleaned(pretty)
    } else {
        HookCleanupResult::EntirelyLeanCtx
    }
}
