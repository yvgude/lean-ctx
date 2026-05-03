use std::fs;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn backup_before_modify(path: &Path, dry_run: bool) {
    if dry_run {
        return;
    }
    if path.exists() {
        let bak = bak_path_for(path);
        let _ = fs::copy(path, &bak);
    }
}

fn bak_path_for(path: &Path) -> PathBuf {
    let filename = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!("{filename}.lean-ctx.bak"))
}

fn cleanup_bak(path: &Path) {
    let bak = bak_path_for(path);
    if bak.exists() {
        let _ = fs::remove_file(&bak);
    }
}

fn shorten(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rel) => format!("~/{}", rel.display()),
        Err(_) => path.display().to_string(),
    }
}

fn copilot_instructions_path(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        return home.join("Library/Application Support/Code/User/github-copilot-instructions.md");
    }
    #[cfg(target_os = "linux")]
    {
        return home.join(".config/Code/User/github-copilot-instructions.md");
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

/// Write `content` to `path` only if not in dry-run mode.
fn safe_write(path: &Path, content: &str, dry_run: bool) -> Result<(), std::io::Error> {
    if dry_run {
        return Ok(());
    }
    fs::write(path, content)?;
    // If we successfully wrote the cleaned file, the backup is no longer needed.
    cleanup_bak(path);
    Ok(())
}

/// Remove `path` only if not in dry-run mode.
fn safe_remove(path: &Path, dry_run: bool) -> Result<(), std::io::Error> {
    if dry_run {
        return Ok(());
    }
    fs::remove_file(path)?;
    // If we successfully removed the file, also remove its backup.
    cleanup_bak(path);
    Ok(())
}

// ---------------------------------------------------------------------------
// Main entry
// ---------------------------------------------------------------------------

pub fn run(dry_run: bool) {
    let Some(home) = dirs::home_dir() else {
        tracing::warn!("Could not determine home directory");
        return;
    };

    if dry_run {
        println!("\n  lean-ctx uninstall --dry-run\n  ──────────────────────────────────\n");
        println!("  Preview mode — no files will be modified.\n");
    } else {
        println!("\n  lean-ctx uninstall\n  ──────────────────────────────────\n");
    }

    let mut removed_any = false;

    removed_any |= remove_shell_hook(&home, dry_run);
    if !dry_run {
        crate::proxy_setup::uninstall_proxy_env(&home, false);
    }
    removed_any |= remove_mcp_configs(&home, dry_run);
    removed_any |= remove_rules_files(&home, dry_run);
    removed_any |= remove_hook_files(&home, dry_run);
    removed_any |= remove_project_agent_files(dry_run);

    if !dry_run {
        cleanup_bak_files(&home);
    }

    removed_any |= remove_data_dir(&home, dry_run);

    println!();

    if removed_any {
        println!("  ──────────────────────────────────");
        if dry_run {
            println!(
                "  The above changes WOULD be applied.\n  Run `lean-ctx uninstall` to execute.\n"
            );
        } else {
            println!("  lean-ctx configuration removed.\n");
        }
    } else {
        println!("  Nothing to remove — lean-ctx was not configured.\n");
    }

    if !dry_run {
        print_binary_removal_instructions();
    }
}

// ---------------------------------------------------------------------------
// Project-level agent files (cwd)
// ---------------------------------------------------------------------------

fn remove_project_agent_files(dry_run: bool) -> bool {
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
fn remove_lean_ctx_section_from_rules(content: &str) -> String {
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
// Marked block removal (for AGENTS.md, SharedMarkdown)
// ---------------------------------------------------------------------------

fn remove_marked_block(content: &str, start: &str, end: &str) -> String {
    let s = content.find(start);
    let e = content.find(end);
    match (s, e) {
        (Some(si), Some(ei)) if ei >= si => {
            let after_end = ei + end.len();
            let before = &content[..si];
            let after = &content[after_end..];
            let mut out = String::new();
            out.push_str(before.trim_end_matches('\n'));
            out.push('\n');
            if !after.trim().is_empty() {
                out.push('\n');
                out.push_str(after.trim_start_matches('\n'));
            }
            out
        }
        _ => content.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Shell hook removal
// ---------------------------------------------------------------------------

fn remove_shell_hook(home: &Path, dry_run: bool) -> bool {
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

fn remove_mcp_configs(home: &Path, dry_run: bool) -> bool {
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
        ("Codex CLI", home.join(".codex/config.toml")),
        ("OpenCode", home.join(".config/opencode/opencode.json")),
        ("Qwen Code", home.join(".qwen/mcp.json")),
        ("Trae", home.join(".trae/mcp.json")),
        ("Amazon Q Developer", home.join(".aws/amazonq/mcp.json")),
        ("JetBrains IDEs", home.join(".jb-mcp.json")),
        ("AWS Kiro", home.join(".kiro/settings/mcp.json")),
        ("Verdent", home.join(".verdent/mcp.json")),
        ("Aider", home.join(".aider/mcp.json")),
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

fn remove_rules_files(home: &Path, dry_run: bool) -> bool {
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
        ("Codex CLI", home.join(".codex/LEAN-CTX.md")),
        ("Windsurf", home.join(".codeium/windsurf/rules/lean-ctx.md")),
        ("Zed", home.join(".config/zed/rules/lean-ctx.md")),
        ("Cline", home.join(".cline/rules/lean-ctx.md")),
        ("Roo Code", home.join(".roo/rules/lean-ctx.md")),
        ("OpenCode", home.join(".config/opencode/rules/lean-ctx.md")),
        ("Continue", home.join(".continue/rules/lean-ctx.md")),
        ("Aider", home.join(".aider/rules/lean-ctx.md")),
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
        ("Codex CLI", home.join(".codex/instructions.md")),
        ("VS Code / Copilot", copilot_instructions_path(home)),
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

fn remove_hook_files(home: &Path, dry_run: bool) -> bool {
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
        home.join(".codex/hooks/lean-ctx-rewrite-codex.sh"),
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
        ("Codex", home.join(".codex/hooks.json")),
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
fn remove_lean_ctx_from_hooks_json(content: &str) -> Option<String> {
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

// ---------------------------------------------------------------------------
// Data directory
// ---------------------------------------------------------------------------

fn remove_data_dir(home: &Path, dry_run: bool) -> bool {
    let data_dir = home.join(".lean-ctx");
    if !data_dir.exists() {
        println!("  · No data directory found");
        return false;
    }

    if dry_run {
        println!("  Would remove Data directory (~/.lean-ctx/)");
        return true;
    }

    match fs::remove_dir_all(&data_dir) {
        Ok(()) => {
            println!("  ✓ Data directory removed (~/.lean-ctx/)");
            true
        }
        Err(e) => {
            tracing::warn!("Failed to remove ~/.lean-ctx/: {e}");
            false
        }
    }
}

// ---------------------------------------------------------------------------
// .bak cleanup: remove orphaned backup files after successful surgical removal
// ---------------------------------------------------------------------------

fn cleanup_bak_files(home: &Path) {
    let dirs_to_scan: Vec<PathBuf> = vec![
        home.join(".cursor"),
        home.join(".claude"),
        crate::core::editor_registry::claude_state_dir(home),
        home.join(".gemini"),
        home.join(".gemini/antigravity"),
        home.join(".codex"),
        home.join(".codeium"),
        home.join(".codeium/windsurf"),
        home.join(".config/opencode"),
        home.join(".config/amp"),
        home.join(".config/crush"),
        home.join(".config/zed"),
        home.join(".qwen"),
        home.join(".trae"),
        home.join(".aws/amazonq"),
        home.join(".kiro"),
        home.join(".kiro/settings"),
        home.join(".aider"),
        home.join(".ampcoder"),
        home.join(".pi"),
        home.join(".pi/agent"),
        home.join(".hermes"),
        home.join(".verdent"),
        home.join(".cline"),
        home.join(".roo"),
        home.join(".continue"),
        home.join(".jb-rules"),
    ];

    let mut cleaned = 0;
    for dir in &dirs_to_scan {
        if !dir.exists() {
            continue;
        }
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.ends_with(".lean-ctx.tmp") {
                    let _ = fs::remove_file(entry.path());
                    cleaned += 1;
                    continue;
                }
                if name_str.ends_with(".lean-ctx.bak") {
                    let original_name = name_str.trim_end_matches(".lean-ctx.bak");
                    let original = entry.path().with_file_name(original_name);
                    if original.exists() {
                        // Only remove backups if the original is already clean.
                        match fs::read_to_string(&original) {
                            Ok(c) if !c.contains("lean-ctx") => {
                                let _ = fs::remove_file(entry.path());
                                cleaned += 1;
                            }
                            _ => {}
                        }
                    } else {
                        // If the original is gone, the backup is no longer needed.
                        let _ = fs::remove_file(entry.path());
                        cleaned += 1;
                    }
                }
            }
        }
    }

    // Also clean shell RC backups
    let rc_baks = [
        home.join(".zshrc.lean-ctx.bak"),
        home.join(".zshenv.lean-ctx.bak"),
        home.join(".bashrc.lean-ctx.bak"),
        home.join(".bashenv.lean-ctx.bak"),
    ];
    for bak in &rc_baks {
        if bak.exists() {
            let original_name = bak
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .trim_end_matches(".lean-ctx.bak")
                .to_string();
            let original = bak.with_file_name(original_name);
            if original.exists() {
                if let Ok(c) = fs::read_to_string(&original) {
                    if !c.contains("lean-ctx") {
                        let _ = fs::remove_file(bak);
                        cleaned += 1;
                    }
                }
            } else {
                let _ = fs::remove_file(bak);
                cleaned += 1;
            }
        }
    }

    if cleaned > 0 {
        println!("  ✓ Cleaned up {cleaned} backup file(s)");
    }
}

// ---------------------------------------------------------------------------
// Binary removal instructions
// ---------------------------------------------------------------------------

fn print_binary_removal_instructions() {
    let binary_path = std::env::current_exe()
        .map_or_else(|_| "lean-ctx".to_string(), |p| p.display().to_string());

    println!("  To complete uninstallation, remove the binary:\n");

    if binary_path.contains(".cargo") {
        println!("    cargo uninstall lean-ctx\n");
    } else if binary_path.contains("homebrew") || binary_path.contains("Cellar") {
        println!("    brew uninstall lean-ctx\n");
    } else {
        println!("    rm {binary_path}\n");
    }

    println!("  Then restart your shell.\n");
}

// ---------------------------------------------------------------------------
// Shell block removal
// ---------------------------------------------------------------------------

fn remove_lean_ctx_block(content: &str) -> String {
    if content.contains("# lean-ctx shell hook — end") {
        return remove_lean_ctx_block_by_marker(content);
    }
    remove_lean_ctx_block_legacy(content)
}

fn remove_lean_ctx_block_by_marker(content: &str) -> String {
    let mut result = String::new();
    let mut in_block = false;

    for line in content.lines() {
        if !in_block && line.contains("lean-ctx shell hook") && !line.contains("end") {
            in_block = true;
            continue;
        }
        if in_block {
            if line.trim() == "# lean-ctx shell hook — end" {
                in_block = false;
            }
            continue;
        }
        result.push_str(line);
        result.push('\n');
    }
    result
}

fn remove_lean_ctx_block_legacy(content: &str) -> String {
    let mut result = String::new();
    let mut in_block = false;

    for line in content.lines() {
        if line.contains("lean-ctx shell hook") {
            in_block = true;
            continue;
        }
        if in_block {
            if line.trim() == "fi" || line.trim() == "end" || line.trim().is_empty() {
                if line.trim() == "fi" || line.trim() == "end" {
                    in_block = false;
                }
                continue;
            }
            if !line.starts_with("alias ") && !line.starts_with('\t') && !line.starts_with("if ") {
                in_block = false;
                result.push_str(line);
                result.push('\n');
            }
            continue;
        }
        result.push_str(line);
        result.push('\n');
    }
    result
}

// ---------------------------------------------------------------------------
// JSON removal — textual approach preserving comments and formatting
// ---------------------------------------------------------------------------

fn remove_lean_ctx_from_json(content: &str) -> Option<String> {
    // Try textual removal first (preserves comments, formatting, key order)
    if let Some(result) = remove_lean_ctx_from_json_textual(content) {
        return Some(result);
    }

    // Fallback to serde-based approach for edge cases
    remove_lean_ctx_from_json_serde(content)
}

/// Textual JSON key removal: finds `"lean-ctx"` key-value pairs and removes
/// them from the raw text without re-serializing. Preserves JSONC comments,
/// formatting, trailing commas, and key ordering.
fn remove_lean_ctx_from_json_textual(content: &str) -> Option<String> {
    let mut result = content.to_string();
    let mut modified = false;

    // Repeatedly find and remove "lean-ctx" entries until none remain.
    // Each iteration rescans because positions shift after removal.
    while let Some(key_start) = find_json_key_position(result.as_bytes(), "lean-ctx") {
        let Some(new_result) = remove_json_entry_at(&result, key_start) else {
            break;
        };

        result = new_result;
        modified = true;
    }

    // Also handle array-style entries: {"name": "lean-ctx", ...}
    loop {
        let bytes = result.as_bytes();
        let Some(pos) = find_named_array_entry(bytes, "lean-ctx") else {
            break;
        };
        let Some(new_result) = remove_array_entry_at(&result, pos) else {
            break;
        };
        result = new_result;
        modified = true;
    }

    if modified {
        // Validate the result is still valid JSON(C) if the input was valid
        if crate::core::jsonc::parse_jsonc(&result).is_ok() {
            Some(result)
        } else if crate::core::jsonc::parse_jsonc(content).is_ok() {
            // Input was valid but our textual removal broke it — don't use this result
            None
        } else {
            // Input was already invalid, return our best effort
            Some(result)
        }
    } else {
        None
    }
}

/// Find the byte position of a JSON key `"key_name"` that is followed by `:`.
fn find_json_key_position(bytes: &[u8], key_name: &str) -> Option<usize> {
    let needle = format!("\"{key_name}\"");
    let needle_bytes = needle.as_bytes();
    let mut i = 0;

    while i + needle_bytes.len() <= bytes.len() {
        if &bytes[i..i + needle_bytes.len()] == needle_bytes {
            // Check it's followed by `:` (after optional whitespace)
            let after = i + needle_bytes.len();
            let mut j = after;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b':' {
                // Make sure we're not inside a string by checking if we have
                // an even number of unescaped quotes before this position
                if !is_inside_string(bytes, i) {
                    return Some(i);
                }
            }
        }
        i += 1;
    }
    None
}

/// Check if position `pos` is inside a JSON string literal.
fn is_inside_string(bytes: &[u8], pos: usize) -> bool {
    let mut in_string = false;
    let mut i = 0;
    while i < pos {
        match bytes[i] {
            b'"' if !in_string => in_string = true,
            b'"' if in_string => in_string = false,
            b'\\' if in_string => {
                i += 1; // skip escaped char
            }
            b'/' if !in_string && i + 1 < bytes.len() => {
                if bytes[i + 1] == b'/' {
                    // Line comment — skip to end of line
                    while i < pos && i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                } else if bytes[i + 1] == b'*' {
                    // Block comment — skip to */
                    i += 2;
                    while i + 1 < bytes.len() {
                        if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    in_string
}

/// Remove a JSON key-value entry starting at `key_start` position.
/// Handles surrounding commas and whitespace.
fn remove_json_entry_at(content: &str, key_start: usize) -> Option<String> {
    let bytes = content.as_bytes();

    // Find the colon after the key
    let key_name_end = content[key_start + 1..].find('"')? + key_start + 2;
    let mut colon_pos = key_name_end;
    while colon_pos < bytes.len() && bytes[colon_pos] != b':' {
        colon_pos += 1;
    }
    if colon_pos >= bytes.len() {
        return None;
    }

    // Skip the value
    let value_start = colon_pos + 1;
    let value_end = skip_json_value(bytes, value_start)?;

    // Determine the range to remove, including surrounding comma and whitespace.
    // Scan backwards from key_start to find leading comma or whitespace.
    let mut remove_start = key_start;

    // Look backwards for a comma (we might be after a comma)
    let mut scan_back = key_start;
    while scan_back > 0 {
        scan_back -= 1;
        let ch = bytes[scan_back];
        if ch == b',' {
            remove_start = scan_back;
            break;
        }
        if ch == b'{' || ch == b'[' {
            break;
        }
        if !ch.is_ascii_whitespace() {
            break;
        }
    }

    // Extend remove_start back to include the newline before the comma/key
    if remove_start > 0 && remove_start == key_start {
        let mut ns = remove_start;
        while ns > 0 && bytes[ns - 1].is_ascii_whitespace() && bytes[ns - 1] != b'\n' {
            ns -= 1;
        }
        if ns > 0 && bytes[ns - 1] == b'\n' {
            remove_start = ns;
        }
    }

    let mut remove_end = value_end;

    // Look forward for a trailing comma
    let mut scan_fwd = value_end;
    while scan_fwd < bytes.len() && bytes[scan_fwd].is_ascii_whitespace() {
        scan_fwd += 1;
    }
    if scan_fwd < bytes.len() && bytes[scan_fwd] == b',' {
        // If we already consumed a leading comma, don't consume trailing too
        if remove_start < key_start && remove_start < bytes.len() && bytes[remove_start] == b',' {
            // Already have leading comma removed, skip trailing
        } else {
            remove_end = scan_fwd + 1;
        }
    }

    // Skip trailing whitespace/newline after the removed entry
    while remove_end < bytes.len()
        && (bytes[remove_end] == b' ' || bytes[remove_end] == b'\t' || bytes[remove_end] == b'\r')
    {
        remove_end += 1;
    }
    if remove_end < bytes.len() && bytes[remove_end] == b'\n' {
        remove_end += 1;
    }

    let mut result = String::with_capacity(content.len());
    result.push_str(&content[..remove_start]);
    result.push_str(&content[remove_end..]);
    Some(result)
}

/// Find an array entry like `{"name": "lean-ctx", ...}` and return its start position.
fn find_named_array_entry(bytes: &[u8], name: &str) -> Option<usize> {
    let needle = format!("\"{name}\"");
    let needle_bytes = needle.as_bytes();
    let mut i = 0;

    while i + needle_bytes.len() <= bytes.len() {
        if &bytes[i..i + needle_bytes.len()] == needle_bytes && !is_inside_string(bytes, i) {
            // Check this is a value (preceded by `:` after `"name"`)
            // Scan backwards to check if the key is "name"
            let mut j = i;
            while j > 0 && bytes[j - 1].is_ascii_whitespace() {
                j -= 1;
            }
            if j > 0 && bytes[j - 1] == b':' {
                j -= 1;
                while j > 0 && bytes[j - 1].is_ascii_whitespace() {
                    j -= 1;
                }
                if j >= 6 && &bytes[j - 6..j] == b"\"name\"" {
                    // Found "name": "lean-ctx" — now find the enclosing object `{`
                    let mut obj_start = j - 6;
                    while obj_start > 0 {
                        if bytes[obj_start] == b'{' && !is_inside_string(bytes, obj_start) {
                            return Some(obj_start);
                        }
                        obj_start -= 1;
                    }
                }
            }
        }
        i += 1;
    }
    None
}

/// Remove an array entry (object) starting at `entry_start`, handling commas.
fn remove_array_entry_at(content: &str, entry_start: usize) -> Option<String> {
    let bytes = content.as_bytes();
    if bytes[entry_start] != b'{' {
        return None;
    }
    let entry_end = skip_json_value(bytes, entry_start)?;

    let mut remove_start = entry_start;
    let mut remove_end = entry_end;

    // Handle leading whitespace
    while remove_start > 0 && (bytes[remove_start - 1] == b' ' || bytes[remove_start - 1] == b'\t')
    {
        remove_start -= 1;
    }

    // Handle trailing comma
    let mut fwd = entry_end;
    while fwd < bytes.len() && bytes[fwd].is_ascii_whitespace() {
        fwd += 1;
    }
    if fwd < bytes.len() && bytes[fwd] == b',' {
        remove_end = fwd + 1;
    } else {
        // No trailing comma — check for leading comma
        let mut back = remove_start;
        while back > 0 && bytes[back - 1].is_ascii_whitespace() {
            back -= 1;
        }
        if back > 0 && bytes[back - 1] == b',' {
            remove_start = back - 1;
        }
    }

    // Skip trailing newline
    while remove_end < bytes.len()
        && (bytes[remove_end] == b' ' || bytes[remove_end] == b'\t' || bytes[remove_end] == b'\r')
    {
        remove_end += 1;
    }
    if remove_end < bytes.len() && bytes[remove_end] == b'\n' {
        remove_end += 1;
    }

    let mut result = String::with_capacity(content.len());
    result.push_str(&content[..remove_start]);
    result.push_str(&content[remove_end..]);
    Some(result)
}

/// Skip over a JSON value (object, array, string, number, boolean, null)
/// starting from `start`. Returns the position after the value.
fn skip_json_value(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start;

    // Skip whitespace
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }

    match bytes[i] {
        b'{' | b'[' => {
            let open = bytes[i];
            let close = if open == b'{' { b'}' } else { b']' };
            let mut depth = 1;
            i += 1;
            while i < bytes.len() && depth > 0 {
                match bytes[i] {
                    c if c == open => depth += 1,
                    c if c == close => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(i + 1);
                        }
                    }
                    b'"' => {
                        i += 1;
                        while i < bytes.len() {
                            if bytes[i] == b'\\' {
                                i += 1;
                            } else if bytes[i] == b'"' {
                                break;
                            }
                            i += 1;
                        }
                    }
                    b'/' if i + 1 < bytes.len() => {
                        if bytes[i + 1] == b'/' {
                            while i < bytes.len() && bytes[i] != b'\n' {
                                i += 1;
                            }
                            continue;
                        } else if bytes[i + 1] == b'*' {
                            i += 2;
                            while i + 1 < bytes.len() {
                                if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                                    i += 1;
                                    break;
                                }
                                i += 1;
                            }
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            Some(i)
        }
        b'"' => {
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i += 1;
                } else if bytes[i] == b'"' {
                    return Some(i + 1);
                }
                i += 1;
            }
            None
        }
        _ => {
            // Number, boolean, null
            while i < bytes.len() && !matches!(bytes[i], b',' | b'}' | b']' | b'\n' | b'\r') {
                i += 1;
            }
            Some(i)
        }
    }
}

/// Fallback: serde-based JSON removal (destroys comments/formatting).
fn remove_lean_ctx_from_json_serde(content: &str) -> Option<String> {
    let mut parsed: serde_json::Value = crate::core::jsonc::parse_jsonc(content).ok()?;
    let mut modified = false;

    if let Some(servers) = parsed.get_mut("mcpServers").and_then(|s| s.as_object_mut()) {
        modified |= servers.remove("lean-ctx").is_some();
    }

    if let Some(servers) = parsed.get_mut("servers").and_then(|s| s.as_object_mut()) {
        modified |= servers.remove("lean-ctx").is_some();
    }

    if let Some(servers) = parsed.get_mut("servers").and_then(|s| s.as_array_mut()) {
        let before = servers.len();
        servers.retain(|entry| entry.get("name").and_then(|n| n.as_str()) != Some("lean-ctx"));
        modified |= servers.len() < before;
    }

    if let Some(mcp) = parsed.get_mut("mcp").and_then(|s| s.as_object_mut()) {
        modified |= mcp.remove("lean-ctx").is_some();
    }

    if let Some(amp) = parsed
        .get_mut("amp.mcpServers")
        .and_then(|s| s.as_object_mut())
    {
        modified |= amp.remove("lean-ctx").is_some();
    }

    if modified {
        Some(serde_json::to_string_pretty(&parsed).ok()? + "\n")
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// YAML removal
// ---------------------------------------------------------------------------

fn remove_lean_ctx_from_yaml(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut skip_depth: Option<usize> = None;

    for line in content.lines() {
        if let Some(depth) = skip_depth {
            let indent = line.len() - line.trim_start().len();
            if indent > depth || line.trim().is_empty() {
                continue;
            }
            skip_depth = None;
        }

        let trimmed = line.trim();
        if trimmed == "lean-ctx:" || trimmed.starts_with("lean-ctx:") {
            let indent = line.len() - line.trim_start().len();
            skip_depth = Some(indent);
            continue;
        }

        out.push_str(line);
        out.push('\n');
    }

    out
}

// ---------------------------------------------------------------------------
// TOML removal
// ---------------------------------------------------------------------------

fn remove_lean_ctx_from_toml(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut skip = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let section = trimmed.trim_start_matches('[').trim_end_matches(']').trim();
            if section == "mcp_servers.lean-ctx"
                || section == "mcp_servers.\"lean-ctx\""
                || section.starts_with("mcp_servers.lean-ctx.")
                || section.starts_with("mcp_servers.\"lean-ctx\".")
            {
                skip = true;
                continue;
            }
            skip = false;
        }

        if skip {
            continue;
        }

        if trimmed.contains("codex_hooks") && trimmed.contains("true") {
            out.push_str(&line.replace("true", "false"));
            out.push('\n');
            continue;
        }

        out.push_str(line);
        out.push('\n');
    }

    let cleaned: String = out
        .lines()
        .filter(|l| l.trim() != "[]")
        .collect::<Vec<_>>()
        .join("\n");
    if cleaned.is_empty() {
        cleaned
    } else {
        cleaned + "\n"
    }
}

// moved to core/editor_registry/paths.rs

#[cfg(test)]
mod tests {
    use super::*;

    // --- TOML tests ---

    #[test]
    fn remove_toml_mcp_server_section() {
        let input = "\
[features]
codex_hooks = true

[mcp_servers.lean-ctx]
command = \"/usr/local/bin/lean-ctx\"
args = []

[mcp_servers.other-tool]
command = \"/usr/bin/other\"
";
        let result = remove_lean_ctx_from_toml(input);
        assert!(
            !result.contains("lean-ctx"),
            "lean-ctx section should be removed"
        );
        assert!(
            result.contains("[mcp_servers.other-tool]"),
            "other sections should be preserved"
        );
        assert!(
            result.contains("codex_hooks = false"),
            "codex_hooks should be set to false"
        );
    }

    #[test]
    fn remove_toml_only_lean_ctx() {
        let input = "\
[mcp_servers.lean-ctx]
command = \"lean-ctx\"
";
        let result = remove_lean_ctx_from_toml(input);
        assert!(
            result.trim().is_empty(),
            "should produce empty output: {result}"
        );
    }

    #[test]
    fn remove_toml_no_lean_ctx() {
        let input = "\
[mcp_servers.other]
command = \"other\"
";
        let result = remove_lean_ctx_from_toml(input);
        assert!(
            result.contains("[mcp_servers.other]"),
            "other content should be preserved"
        );
    }

    // --- JSON textual removal tests ---

    #[test]
    fn json_textual_removes_key_from_object() {
        let input = r#"{
  "mcpServers": {
    "other-tool": {
      "command": "other"
    },
    "lean-ctx": {
      "command": "/usr/bin/lean-ctx",
      "args": []
    }
  }
}
"#;
        let result = remove_lean_ctx_from_json(input).expect("should find lean-ctx");
        assert!(!result.contains("lean-ctx"), "lean-ctx should be removed");
        assert!(
            result.contains("other-tool"),
            "other-tool should be preserved"
        );
        // Verify valid JSON
        assert!(
            crate::core::jsonc::parse_jsonc(&result).is_ok(),
            "result should be valid JSON: {result}"
        );
    }

    #[test]
    fn json_textual_preserves_comments() {
        let input = r#"{
  // This is a user comment
  "mcpServers": {
    "lean-ctx": {
      "command": "lean-ctx"
    },
    "my-tool": {
      "command": "my-tool"
    }
  }
}
"#;
        let result = remove_lean_ctx_from_json(input).expect("should find lean-ctx");
        assert!(!result.contains("lean-ctx"), "lean-ctx should be removed");
        assert!(
            result.contains("// This is a user comment"),
            "comment should be preserved: {result}"
        );
        assert!(result.contains("my-tool"), "my-tool should be preserved");
    }

    #[test]
    fn json_textual_only_lean_ctx() {
        let input = r#"{
  "mcpServers": {
    "lean-ctx": {
      "command": "lean-ctx"
    }
  }
}
"#;
        let result = remove_lean_ctx_from_json(input).expect("should find lean-ctx");
        assert!(!result.contains("lean-ctx"), "lean-ctx should be removed");
    }

    #[test]
    fn json_no_lean_ctx_returns_none() {
        let input = r#"{"mcpServers": {"other": {"command": "other"}}}"#;
        assert!(remove_lean_ctx_from_json(input).is_none());
    }

    // --- Shared rules (SharedMarkdown) tests ---

    #[test]
    fn shared_markdown_surgical_removal() {
        let input = "# My custom rules\n\nDo this and that.\n\n\
                      # lean-ctx — Context Engineering Layer\n\
                      <!-- lean-ctx-rules-v9 -->\n\n\
                      Use ctx_read instead of Read.\n\
                      <!-- /lean-ctx -->\n\n\
                      # Other section\n\nMore user content.\n";

        let cleaned = remove_marked_block(
            input,
            "# lean-ctx — Context Engineering Layer",
            "<!-- /lean-ctx -->",
        );

        assert!(
            !cleaned.contains("lean-ctx"),
            "lean-ctx block should be removed"
        );
        assert!(
            cleaned.contains("My custom rules"),
            "user content before should be preserved"
        );
        assert!(
            cleaned.contains("Other section"),
            "user content after should be preserved"
        );
        assert!(
            cleaned.contains("More user content"),
            "user content after should be preserved"
        );
    }

    #[test]
    fn shared_markdown_only_lean_ctx() {
        let input = "# lean-ctx — Context Engineering Layer\n\
                      <!-- lean-ctx-rules-v9 -->\n\
                      content\n\
                      <!-- /lean-ctx -->\n";

        let cleaned = remove_marked_block(
            input,
            "# lean-ctx — Context Engineering Layer",
            "<!-- /lean-ctx -->",
        );

        assert!(
            cleaned.trim().is_empty() || !cleaned.contains("lean-ctx"),
            "should be empty or without lean-ctx: '{cleaned}'"
        );
    }

    // --- Project files (.cursorrules) tests ---

    #[test]
    fn cursorrules_surgical_removal() {
        let input = "# My project rules\n\n\
                      Always use TypeScript.\n\n\
                      # lean-ctx — Context Engineering Layer\n\n\
                      PREFER lean-ctx MCP tools over native equivalents.\n";

        let cleaned = remove_lean_ctx_section_from_rules(input);

        assert!(
            !cleaned.contains("lean-ctx"),
            "lean-ctx section should be removed"
        );
        assert!(
            cleaned.contains("My project rules"),
            "user rules should be preserved"
        );
        assert!(
            cleaned.contains("Always use TypeScript"),
            "user content should be preserved"
        );
    }

    #[test]
    fn cursorrules_only_lean_ctx() {
        let input = "# lean-ctx — Context Engineering Layer\n\n\
                      PREFER lean-ctx MCP tools.\n";

        let cleaned = remove_lean_ctx_section_from_rules(input);
        assert!(
            cleaned.trim().is_empty(),
            "should be empty when only lean-ctx content: '{cleaned}'"
        );
    }

    // --- hooks.json tests ---

    #[test]
    fn hooks_json_preserves_other_hooks() {
        let input = r#"{
  "version": 1,
  "hooks": {
    "preToolUse": [
      {
        "matcher": "Shell",
        "command": "lean-ctx hook rewrite"
      },
      {
        "matcher": "Shell",
        "command": "my-other-tool hook"
      }
    ]
  }
}"#;
        let result = remove_lean_ctx_from_hooks_json(input).expect("should return cleaned JSON");
        assert!(!result.contains("lean-ctx"), "lean-ctx should be removed");
        assert!(
            result.contains("my-other-tool"),
            "other hooks should be preserved"
        );
    }

    #[test]
    fn hooks_json_returns_none_when_only_lean_ctx() {
        let input = r#"{
  "version": 1,
  "hooks": {
    "preToolUse": [
      {
        "matcher": "Shell",
        "command": "lean-ctx hook rewrite"
      },
      {
        "matcher": "Read|Grep",
        "command": "lean-ctx hook redirect"
      }
    ]
  }
}"#;
        assert!(
            remove_lean_ctx_from_hooks_json(input).is_none(),
            "should return None when all hooks are lean-ctx"
        );
    }

    // --- Marked block tests ---

    #[test]
    fn marked_block_preserves_surrounding() {
        let content = "before\n<!-- lean-ctx -->\nhook content\n<!-- /lean-ctx -->\nafter\n";
        let cleaned = remove_marked_block(content, "<!-- lean-ctx -->", "<!-- /lean-ctx -->");
        assert!(!cleaned.contains("hook content"));
        assert!(cleaned.contains("before"));
        assert!(cleaned.contains("after"));
    }

    #[test]
    fn marked_block_preserves_when_missing() {
        let content = "no hook here\n";
        let cleaned = remove_marked_block(content, "<!-- lean-ctx -->", "<!-- /lean-ctx -->");
        assert_eq!(cleaned, content);
    }

    #[test]
    fn backup_before_modify_respects_dry_run() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(&path, "hello").unwrap();

        backup_before_modify(&path, true);
        assert!(
            !bak_path_for(&path).exists(),
            "dry-run must not create backups"
        );

        backup_before_modify(&path, false);
        assert!(
            bak_path_for(&path).exists(),
            "non-dry-run should create backups"
        );
    }
}
