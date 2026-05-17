use std::fs;
use std::path::{Path, PathBuf};

use super::parsers::{
    remove_lean_ctx_block, remove_lean_ctx_from_json, remove_lean_ctx_from_toml,
    remove_lean_ctx_from_yaml,
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

    // Project-level .claude/settings.local.json: surgically remove lean-ctx hooks
    let claude_settings = cwd.join(".claude/settings.local.json");
    if claude_settings.exists() {
        if let Ok(content) = fs::read_to_string(&claude_settings) {
            if content.contains("lean-ctx") {
                backup_before_modify(&claude_settings, dry_run);
                match remove_lean_ctx_from_hooks_json(&content) {
                    Some(cleaned) if !cleaned.trim().is_empty() => {
                        let _ = safe_write(&claude_settings, &cleaned, dry_run);
                        let verb = if dry_run { "Would clean" } else { "✓" };
                        println!(
                            "  {verb} Project: cleaned .claude/settings.local.json (user hooks preserved)"
                        );
                    }
                    _ => {
                        let _ = safe_remove(&claude_settings, dry_run);
                        let verb = if dry_run { "Would remove" } else { "✓" };
                        println!("  {verb} Project: removed .claude/settings.local.json");
                    }
                }
                removed = true;
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

    if !dry_run {
        crate::shell_hook::uninstall_all(false);
    }

    let rc_files: Vec<PathBuf> = vec![
        home.join(".zshrc"),
        home.join(".bashrc"),
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
    let configs: Vec<(&str, PathBuf)> = vec![
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
    ];

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

    let zed_path = crate::core::editor_registry::zed_settings_path(home);
    if zed_path.exists() {
        if let Ok(content) = fs::read_to_string(&zed_path) {
            if content.contains("lean-ctx") {
                println!(
                    "  ⚠ Zed: manually remove lean-ctx from {}",
                    shorten(&zed_path, home)
                );
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
            "Codex CLI",
            crate::core::home::resolve_codex_dir()
                .unwrap_or_else(|| home.join(".codex"))
                .join("LEAN-CTX.md"),
        ),
        ("Windsurf", home.join(".codeium/windsurf/rules/lean-ctx.md")),
        ("Zed", home.join(".config/zed/rules/lean-ctx.md")),
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
        ("VS Code / Copilot", copilot_instructions_path(home)),
        ("OpenCode", home.join(".config/opencode/AGENTS.md")),
    ];

    let mut removed = false;

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
    const RULES_MARKER: &str = "# lean-ctx — Context Engineering Layer";
    const RULES_END: &str = "<!-- /lean-ctx -->";

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

        let cleaned = if content.contains(RULES_END) {
            remove_marked_block(&content, RULES_MARKER, RULES_END)
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

    // hooks.json: surgically remove lean-ctx entries instead of deleting
    for (label, hj_path) in [
        ("Cursor", home.join(".cursor/hooks.json")),
        (
            "Codex",
            crate::core::home::resolve_codex_dir()
                .unwrap_or_else(|| home.join(".codex"))
                .join("hooks.json"),
        ),
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

        match remove_lean_ctx_from_hooks_json(&content) {
            Some(cleaned) if !cleaned.trim().is_empty() => {
                if let Err(e) = safe_write(&hj_path, &cleaned, dry_run) {
                    tracing::warn!("Failed to update {label} hooks.json: {e}");
                } else {
                    let verb = if dry_run { "Would clean" } else { "✓" };
                    println!("  {verb} {label} hooks.json cleaned (non-lean-ctx hooks preserved)");
                    removed = true;
                }
            }
            _ => {
                if let Err(e) = safe_remove(&hj_path, dry_run) {
                    tracing::warn!("Failed to remove {label} hooks.json: {e}");
                } else {
                    let verb = if dry_run { "Would remove" } else { "✓" };
                    println!("  {verb} {label} hooks.json removed");
                    removed = true;
                }
            }
        }
    }

    removed
}

/// Remove lean-ctx hook entries from hooks.json, preserving other hooks.
/// Returns `Some(cleaned_json)` if non-lean-ctx hooks remain, `None` if empty.
pub(super) fn remove_lean_ctx_from_hooks_json(content: &str) -> Option<String> {
    let mut parsed: serde_json::Value = crate::core::jsonc::parse_jsonc(content).ok()?;
    let mut modified = false;

    if let Some(hooks) = parsed.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        for entries in hooks.values_mut() {
            if let Some(arr) = entries.as_array_mut() {
                let before = arr.len();
                arr.retain(|entry| {
                    !entry
                        .get("command")
                        .and_then(|c| c.as_str())
                        .is_some_and(|cmd| cmd.contains("lean-ctx"))
                });
                if arr.len() < before {
                    modified = true;
                }
            }
        }
    }

    if !modified {
        return None;
    }

    let has_remaining_hooks =
        parsed
            .get("hooks")
            .and_then(|h| h.as_object())
            .is_some_and(|hooks| {
                hooks
                    .values()
                    .any(|entries| entries.as_array().is_some_and(|a| !a.is_empty()))
            });

    if has_remaining_hooks {
        Some(serde_json::to_string_pretty(&parsed).ok()? + "\n")
    } else {
        None
    }
}
