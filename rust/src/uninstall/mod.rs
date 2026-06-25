mod agents;
mod binary;
mod parsers;

use std::fs;
use std::path::{Path, PathBuf};

use agents::{
    remove_hook_files, remove_mcp_configs, remove_plan_mode_settings, remove_project_agent_files,
    remove_rules_files, remove_shell_hook,
};

pub(super) fn backup_before_modify(path: &Path, dry_run: bool) {
    if dry_run {
        return;
    }
    if path.exists() {
        let bak = bak_path_for(path);
        let _ = fs::copy(path, &bak);
    }
}

#[must_use]
pub fn bak_path_for(path: &Path) -> PathBuf {
    let filename = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!("{filename}.lean-ctx.bak"))
}

fn cleanup_bak(path: &Path) {
    let bak = bak_path_for(path);
    if bak.exists() {
        let _ = fs::remove_file(&bak);
    }
}

pub(super) fn shorten(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rel) => format!("~/{}", rel.display()),
        Err(_) => path.display().to_string(),
    }
}

pub(super) fn copilot_instructions_path(home: &Path) -> PathBuf {
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

/// Write `content` to `path` only if not in dry-run mode.
pub(super) fn safe_write(path: &Path, content: &str, dry_run: bool) -> Result<(), std::io::Error> {
    if dry_run {
        return Ok(());
    }
    fs::write(path, content)?;
    // If we successfully wrote the cleaned file, the backup is no longer needed.
    cleanup_bak(path);
    Ok(())
}

/// Remove `path` only if not in dry-run mode.
pub(super) fn safe_remove(path: &Path, dry_run: bool) -> Result<(), std::io::Error> {
    if dry_run {
        return Ok(());
    }
    fs::remove_file(path)?;
    // If we successfully removed the file, also remove its backup.
    cleanup_bak(path);
    Ok(())
}

// ---------------------------------------------------------------------------
// Help
// ---------------------------------------------------------------------------

/// Print usage for `lean-ctx uninstall`.
///
/// This MUST stay side-effect free: `lean-ctx uninstall --help` previously fell
/// through to [`run`] and removed everything, so help is now short-circuited in
/// the CLI dispatch before any removal happens.
pub fn print_help() {
    println!(
        "\
lean-ctx uninstall — remove lean-ctx cleanly

USAGE:
    lean-ctx uninstall [OPTIONS]

OPTIONS:
    --dry-run        Preview every change without modifying anything
    --keep-config    Preserve MCP configs and rules (for a later reinstall)
    --keep-binary    Leave the lean-ctx binary in place
    -h, --help       Show this help and exit (does NOT uninstall)

WHAT IT REMOVES:
    • Running processes (daemon, proxy) and autostart entries
    • Shell hooks and proxy environment from your shell rc files
    • MCP server configs and rules from every detected AI tool/IDE
    • Skill directories and project integration files
    • The data directory and the lean-ctx binary

    Modified files are backed up as <file>.lean-ctx.bak before removal.

EXAMPLES:
    lean-ctx uninstall --dry-run     # see exactly what would change
    lean-ctx uninstall               # full clean removal"
    );
}

// ---------------------------------------------------------------------------
// Main entry
// ---------------------------------------------------------------------------

pub fn run(dry_run: bool, keep_config: bool, keep_binary: bool) {
    let Some(home) = dirs::home_dir() else {
        tracing::warn!("Could not determine home directory");
        return;
    };

    let mode_label = if keep_config {
        "uninstall --keep-config"
    } else {
        "uninstall"
    };

    if dry_run {
        println!("\n  lean-ctx {mode_label} --dry-run\n  ──────────────────────────────────\n");
        println!("  Preview mode — no files will be modified.\n");
    } else {
        println!("\n  lean-ctx {mode_label}\n  ──────────────────────────────────\n");
    }

    if keep_config {
        println!("  Mode: keep-config (MCP configs and rules preserved for reinstall)\n");
    }

    // Stop everything first so nothing respawns or holds the files/data we remove next.
    binary::stop_processes(dry_run);

    let mut removed_any = false;

    removed_any |= remove_shell_hook(&home, dry_run);
    if dry_run {
        crate::proxy_setup::preview_proxy_cleanup(&home);
    } else {
        crate::proxy_setup::uninstall_proxy_env(&home, false);
    }

    if keep_config {
        println!("  · Skipped: MCP configs (--keep-config)");
        println!("  · Skipped: Rules files (--keep-config)");
    } else {
        removed_any |= remove_mcp_configs(&home, dry_run);
        removed_any |= remove_rules_files(&home, dry_run);
        if !dry_run {
            try_claude_mcp_remove();
        }
    }

    removed_any |= remove_hook_files(&home, dry_run);
    removed_any |= remove_plan_mode_settings(&home, dry_run);
    removed_any |= remove_skill_dirs(&home, dry_run);
    removed_any |= remove_project_agent_files(dry_run);

    if dry_run {
        println!("  Would remove proxy autostart (LaunchAgent/systemd)");
        println!("  Would remove daemon autostart (LaunchAgent/systemd)");
        println!("  Would remove auto-update schedule (LaunchAgent/systemd/Task)");
    } else {
        crate::proxy_autostart::uninstall(true);
        crate::daemon_autostart::uninstall(true);
        // The 6-hourly self-update agent (com.leanctx.autoupdate) is a *separate*
        // autostart entry from daemon/proxy. Without this it survives uninstall and
        // keeps relaunching the now-deleted binary every 6h. remove_schedule() is the
        // same idempotent routine used elsewhere (macOS/Linux/Windows aware).
        let had_schedule = crate::core::update_scheduler::schedule_status().enabled;
        match crate::core::update_scheduler::remove_schedule() {
            Ok(()) if had_schedule => {
                println!("  ✓ Auto-update schedule removed");
                removed_any = true;
            }
            Ok(()) => {}
            Err(e) => tracing::warn!("Failed to remove auto-update schedule: {e}"),
        }
    }

    if !dry_run {
        cleanup_bak_files(&home);
    }

    removed_any |= remove_data_dir(&home, dry_run);

    // Last filesystem step: every file-removal pass above has run, so
    // installer-created directories that are empty now stay empty.
    if !dry_run {
        sweep_empty_installer_dirs(&home);
    }

    // Remove the binary itself last: once it's gone we can't re-exec, and on Unix the
    // running process keeps working until exit.
    removed_any |= binary::remove_binaries(&home, dry_run, keep_binary);

    println!();

    if removed_any {
        println!("  ──────────────────────────────────");
        if dry_run {
            println!(
                "  The above changes WOULD be applied.\n  Run `lean-ctx {mode_label}` to execute.\n"
            );
        } else if keep_config {
            println!(
                "  Runtime data removed. MCP configs preserved for reinstall.\n  \
                 Reinstall with: cargo install lean-ctx\n"
            );
        } else {
            println!(
                "  lean-ctx fully removed. Restart your shell to drop stale aliases.\n  \
                 Verify with: command -v lean-ctx   # should print nothing\n"
            );
        }
    } else {
        println!("  Nothing to remove — lean-ctx was not configured.\n");
    }
}

// ---------------------------------------------------------------------------
// Marked block removal (for AGENTS.md, SharedMarkdown)
// ---------------------------------------------------------------------------

pub(super) fn remove_marked_block(content: &str, start: &str, end: &str) -> String {
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
// Skill directories: lean-ctx SKILL.md + scripts
// ---------------------------------------------------------------------------

fn remove_skill_dirs(home: &Path, dry_run: bool) -> bool {
    let claude_state = crate::core::editor_registry::claude_state_dir(home);
    let codebuddy_state = crate::core::editor_registry::codebuddy_state_dir(home);
    let mut skill_dirs: Vec<(&str, PathBuf)> = vec![
        ("Claude Code", claude_state.join("skills/lean-ctx")),
        ("CodeBuddy", codebuddy_state.join("skills/lean-ctx")),
        ("Cursor", home.join(".cursor/skills/lean-ctx")),
        (
            "Codex CLI",
            crate::core::home::resolve_codex_dir()
                .unwrap_or_else(|| home.join(".codex"))
                .join("skills/lean-ctx"),
        ),
        ("Copilot", home.join(".copilot/skills/lean-ctx")),
        ("OpenClaw", home.join(".openclaw/skills/lean-ctx")),
    ];

    // If CLAUDE_CONFIG_DIR differs from ~/.claude, also clean default path
    let default_claude_skill = home.join(".claude/skills/lean-ctx");
    if !skill_dirs.iter().any(|(_, p)| *p == default_claude_skill) {
        skill_dirs.push(("Claude Code (default)", default_claude_skill));
    }

    // If CODEBUDDY_CONFIG_DIR differs from ~/.codebuddy, also clean default path
    let default_codebuddy_skill = home.join(".codebuddy/skills/lean-ctx");
    if !skill_dirs
        .iter()
        .any(|(_, p)| *p == default_codebuddy_skill)
    {
        skill_dirs.push(("CodeBuddy (default)", default_codebuddy_skill));
    }

    let mut removed = false;
    for (name, dir) in &skill_dirs {
        if !dir.exists() {
            continue;
        }
        if dry_run {
            println!("  Would remove {name} skill directory");
            removed = true;
        } else if let Err(e) = fs::remove_dir_all(dir) {
            tracing::warn!("Failed to remove {name} skill dir: {e}");
        } else {
            println!("  ✓ {name} skill directory removed");
            removed = true;
        }
    }
    removed
}

// ---------------------------------------------------------------------------
// Data directory
// ---------------------------------------------------------------------------

/// Every lean-ctx directory an uninstall must delete, de-duplicated and
/// order-preserving.
///
/// Historically this used `dirs::data_dir()` / `dirs::data_local_dir()`, which on
/// macOS both resolve to `~/Library/Application Support` — so the *real* runtime
/// dirs (`~/.local/share`, `~/.local/state`, `~/.cache`) were never removed and a
/// "full" uninstall left >150 MB of data + cache behind. We now resolve through the
/// exact same [`core::paths`](crate::core::paths) functions the daemon/proxy use,
/// so every XDG category (config/data/state/cache, honoring `LEAN_CTX_*_DIR` and
/// `XDG_*` overrides) is covered, plus the legacy single-dir and macOS
/// Application Support locations for older installs.
fn data_dirs_to_remove(home: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![home.join(".lean-ctx"), home.join(".config/lean-ctx")];

    let push = |dirs: &mut Vec<PathBuf>, p: PathBuf| {
        if !dirs.contains(&p) {
            dirs.push(p);
        }
    };

    // Canonical XDG categories actually written at runtime.
    for resolved in [
        crate::core::paths::config_dir(),
        crate::core::paths::data_dir(),
        crate::core::paths::state_dir(),
        crate::core::paths::cache_dir(),
    ]
    .into_iter()
    .flatten()
    {
        push(&mut dirs, resolved);
    }

    // Older installs (and Windows %LOCALAPPDATA%) may have used the platform dir.
    for platform_dir in [dirs::data_local_dir(), dirs::data_dir()]
        .into_iter()
        .flatten()
    {
        push(&mut dirs, platform_dir.join("lean-ctx"));
    }

    dirs
}

fn remove_data_dir(home: &Path, dry_run: bool) -> bool {
    let mut removed = false;

    let dirs_to_remove = data_dirs_to_remove(home);

    for data_dir in &dirs_to_remove {
        if !data_dir.exists() {
            continue;
        }
        let short = shorten(data_dir, home);
        if dry_run {
            println!("  Would remove data directory ({short})");
            removed = true;
            continue;
        }
        match fs::remove_dir_all(data_dir) {
            Ok(()) => {
                println!("  ✓ Data directory removed ({short})");
                removed = true;
            }
            Err(e) => tracing::warn!("Failed to remove {short}: {e}"),
        }
    }

    // Project-local .lean-ctx/ and .lean-ctx-id in CWD
    if let Ok(cwd) = std::env::current_dir() {
        let project_dir = cwd.join(".lean-ctx");
        let project_id = cwd.join(".lean-ctx-id");
        for p in [&project_dir, &project_id] {
            if p.exists() {
                if dry_run {
                    println!("  Would remove {}", p.display());
                    removed = true;
                } else if p.is_dir() {
                    if fs::remove_dir_all(p).is_ok() {
                        println!("  ✓ Removed {}", p.display());
                        removed = true;
                    }
                } else if fs::remove_file(p).is_ok() {
                    println!("  ✓ Removed {}", p.display());
                    removed = true;
                }
            }
        }
    }

    if !removed {
        println!("  · No data directory found");
    }
    removed
}

fn try_claude_mcp_remove() {
    let result = std::process::Command::new("claude")
        .args(["mcp", "remove", "lean-ctx", "--scope", "user"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    match result {
        Ok(s) if s.success() => println!("  ✓ Removed lean-ctx from Claude MCP registry"),
        _ => {} // claude CLI not available or already removed
    }
}

// ---------------------------------------------------------------------------
// .bak cleanup: remove orphaned backup files after successful surgical removal
// ---------------------------------------------------------------------------

/// Every directory the installer may have written backups or files into:
/// agent config roots, their well-known subdirectories, and the project-local
/// config dirs in CWD.
fn scan_dirs(home: &Path) -> Vec<PathBuf> {
    let base_dirs: Vec<PathBuf> = vec![
        home.join(".cursor"),
        home.join(".claude"),
        crate::core::editor_registry::claude_state_dir(home),
        home.join(".codebuddy"),
        crate::core::editor_registry::codebuddy_state_dir(home),
        crate::core::editor_registry::zed_config_dir(home),
        home.join(".gemini"),
        home.join(".gemini/antigravity"),
        home.join(".gemini/antigravity-cli"),
        crate::core::home::resolve_codex_dir().unwrap_or_else(|| home.join(".codex")),
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
        home.join(".ampcoder"),
        home.join(".pi"),
        home.join(".pi/agent"),
        home.join(".hermes"),
        home.join(".verdent"),
        home.join(".cline"),
        home.join(".roo"),
        home.join(".continue"),
        home.join(".jb-rules"),
        home.join(".openclaw"),
        home.join(".augment"),
        home.join(".qoder"),
        home.join(".qoderwork"),
        home.join(".aider"),
        home.join(".emacs.d"),
        home.join(".copilot"),
        home.join(".github"),
        home.join(".config/mcphub"),
        home.join(".config/sublime-text"),
    ];

    // Installers write into well-known subdirectories (hook scripts, rules
    // files, steering docs, …). read_dir below is non-recursive, so backups in
    // those subdirectories were previously missed (GL #558).
    const KNOWN_SUBDIRS: [&str; 6] = ["hooks", "rules", "skills", "steering", "settings", "User"];
    let mut dirs_to_scan: Vec<PathBuf> = Vec::with_capacity(base_dirs.len() * 4);
    for dir in base_dirs {
        for sub in KNOWN_SUBDIRS {
            let p = dir.join(sub);
            if p.is_dir() {
                dirs_to_scan.push(p);
            }
        }
        dirs_to_scan.push(dir);
    }

    // Project-local config dirs in CWD get the same backup treatment as HOME:
    // setup writes (and uninstall removes) rules/hooks there too.
    if let Ok(cwd) = std::env::current_dir() {
        for rel in [
            ".cursor/rules",
            ".claude",
            ".claude/rules",
            ".claude/hooks",
            ".codebuddy",
            ".codebuddy/rules",
            ".codebuddy/hooks",
            ".kiro/steering",
            ".github",
            ".github/hooks",
            ".vscode",
        ] {
            let p = cwd.join(rel);
            if p.is_dir() {
                dirs_to_scan.push(p);
            }
        }
    }

    dirs_to_scan
}

fn cleanup_bak_files(home: &Path) {
    let dirs_to_scan = scan_dirs(home);
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
                // Backups of our own hook scripts / rules files
                // (lean-ctx-rewrite.sh.bak, lean-ctx.mdc.bak, …): the originals
                // are lean-ctx-owned and already removed at this point, so the
                // backups are pure leftovers.
                if name_str.ends_with(".bak")
                    && (name_str.starts_with("lean-ctx-") || name_str.starts_with("lean-ctx."))
                {
                    let _ = fs::remove_file(entry.path());
                    cleaned += 1;
                    continue;
                }
                if name_str.contains(".lean-ctx.invalid.") && name_str.ends_with(".bak") {
                    let _ = fs::remove_file(entry.path());
                    cleaned += 1;
                    continue;
                }
                if name_str.ends_with(".lean-ctx.bak") {
                    let original_name = name_str.trim_end_matches(".lean-ctx.bak");
                    let original = entry.path().with_file_name(original_name);
                    if original.exists() {
                        match fs::read_to_string(&original) {
                            Ok(c) if !c.contains("lean-ctx") => {
                                let _ = fs::remove_file(entry.path());
                                cleaned += 1;
                            }
                            _ => {}
                        }
                    } else {
                        let _ = fs::remove_file(entry.path());
                        cleaned += 1;
                    }
                    continue;
                }
                // Plain .bak files next to known config files (created by
                // config_io). Removed whether or not the original still exists:
                // when uninstall deletes a config file that only contained
                // lean-ctx content, its backup would otherwise be orphaned.
                if name_str.ends_with(".bak")
                    && !name_str.contains(".lean-ctx")
                    && let Ok(bak_content) = fs::read_to_string(entry.path())
                    && bak_content.contains("lean-ctx")
                {
                    let _ = fs::remove_file(entry.path());
                    cleaned += 1;
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
                if let Ok(c) = fs::read_to_string(&original)
                    && !c.contains("lean-ctx")
                {
                    let _ = fs::remove_file(bak);
                    cleaned += 1;
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

/// Sweep now-empty installer-created directories (hooks/, rules/, skills/,
/// steering/). `fs::remove_dir` refuses to delete non-empty directories, so
/// anything still holding user content survives untouched. Runs as the last
/// filesystem step of `run()` — after every file-removal pass has finished.
fn sweep_empty_installer_dirs(home: &Path) {
    let mut swept = 0;
    for dir in scan_dirs(home) {
        let is_installer_dir = dir
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| matches!(n, "hooks" | "rules" | "skills" | "steering"));
        if is_installer_dir && fs::remove_dir(&dir).is_ok() {
            swept += 1;
        }
    }
    if swept > 0 {
        println!("  ✓ Removed {swept} empty installer director(y/ies)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn data_dirs_to_remove_covers_canonical_xdg_categories() {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/home/tester"));
        let dirs = data_dirs_to_remove(&home);

        // Legacy single-dir + pre-split config dir are always targeted.
        assert!(dirs.contains(&home.join(".lean-ctx")));
        assert!(dirs.contains(&home.join(".config/lean-ctx")));

        // Regression guard for the macOS data/state/cache leak (#uninstall-completeness):
        // the set MUST include whatever core::paths actually resolves for every XDG
        // category. The old dirs::data_dir()-based code missed these — on macOS they
        // collapse onto Application Support — leaving the real ~/.local/share +
        // ~/.local/state + ~/.cache (>150 MB) behind after a "full" uninstall.
        for resolved in [
            crate::core::paths::config_dir(),
            crate::core::paths::data_dir(),
            crate::core::paths::state_dir(),
            crate::core::paths::cache_dir(),
        ]
        .into_iter()
        .flatten()
        {
            assert!(
                dirs.contains(&resolved),
                "uninstall would NOT remove canonical dir: {}",
                resolved.display()
            );
        }

        // Each directory is listed exactly once (removed once, no churn).
        let mut seen = HashSet::new();
        for d in &dirs {
            assert!(seen.insert(d.clone()), "duplicate dir: {}", d.display());
        }
    }
}
