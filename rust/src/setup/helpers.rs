//! Setup helper routines (skill install, TOML key upserts, profile + premium
//! feature configuration). Split out of `setup/mod.rs` for focus.

#[allow(clippy::wildcard_imports)]
use super::*;

#[must_use]
pub fn install_skill_files(home: &std::path::Path) -> Vec<(String, bool)> {
    crate::rules_inject::install_all_skills(home)
}

pub(crate) fn install_kiro_steering(home: &std::path::Path) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| home.to_path_buf());
    let steering_dir = cwd.join(".kiro").join("steering");
    let steering_file = steering_dir.join("lean-ctx.md");

    if steering_file.exists()
        && std::fs::read_to_string(&steering_file)
            .unwrap_or_default()
            .contains("lean-ctx")
    {
        println!("  Kiro steering file already exists at .kiro/steering/lean-ctx.md");
        return;
    }

    let _ = std::fs::create_dir_all(&steering_dir);
    let _ = std::fs::write(&steering_file, crate::hooks::kiro_steering_content());
    println!(
        "  \x1b[32m✓\x1b[0m Created .kiro/steering/lean-ctx.md (Kiro will now prefer lean-ctx tools)"
    );
}

pub(crate) fn configure_plan_mode_settings(newly_configured: &[&str], already_configured: &[&str]) {
    use crate::terminal_ui;

    let all_configured: Vec<&str> = newly_configured
        .iter()
        .chain(already_configured.iter())
        .copied()
        .collect();

    let has_vscode = all_configured.contains(&"VS Code");
    let has_claude = all_configured.contains(&"Claude Code");
    let has_codebuddy = all_configured.contains(&"CodeBuddy");

    if !has_vscode && !has_claude && !has_codebuddy {
        return;
    }

    if has_vscode {
        match crate::core::editor_registry::plan_mode::write_vscode_plan_settings() {
            Ok(r) if r.action == WriteAction::Already => {
                terminal_ui::print_status_ok(
                    "VS Code            \x1b[2mplan mode already configured\x1b[0m",
                );
            }
            Ok(_) => {
                terminal_ui::print_status_new(
                    "VS Code            \x1b[2mplan mode tools configured\x1b[0m",
                );
            }
            Err(e) => {
                terminal_ui::print_status_warn(&format!("VS Code plan mode: {e}"));
            }
        }
    }

    if has_claude {
        match crate::core::editor_registry::plan_mode::write_claude_code_plan_permissions() {
            Ok(r) if r.action == WriteAction::Already => {
                terminal_ui::print_status_ok(
                    "Claude Code        \x1b[2mplan mode permissions present\x1b[0m",
                );
            }
            Ok(_) => {
                terminal_ui::print_status_new(
                    "Claude Code        \x1b[2mplan mode permissions added\x1b[0m",
                );
            }
            Err(e) => {
                terminal_ui::print_status_warn(&format!("Claude Code plan mode: {e}"));
            }
        }
    }

    if has_codebuddy {
        match crate::core::editor_registry::plan_mode::write_claude_code_plan_permissions() {
            Ok(r) if r.action == WriteAction::Already => {
                terminal_ui::print_status_ok(
                    "CodeBuddy          \x1b[2mplan mode permissions present\x1b[0m",
                );
            }
            Ok(_) => {
                terminal_ui::print_status_new(
                    "CodeBuddy          \x1b[2mplan mode permissions added\x1b[0m",
                );
            }
            Err(e) => {
                terminal_ui::print_status_warn(&format!("CodeBuddy plan mode: {e}"));
            }
        }
    }
}

pub(crate) fn shorten_path(path: &str, home: &str) -> String {
    if let Some(stripped) = path.strip_prefix(home) {
        format!("~{stripped}")
    } else {
        path.to_string()
    }
}

fn upsert_toml_key(content: &mut String, key: &str, value: &str) {
    let pattern = format!("{key} = ");
    if let Some(start) = content.find(&pattern) {
        let line_end = content[start..]
            .find('\n')
            .map_or(content.len(), |p| start + p);
        content.replace_range(start..line_end, &format!("{key} = \"{value}\""));
    } else {
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&format!("{key} = \"{value}\"\n"));
    }
}

fn remove_toml_key(content: &mut String, key: &str) {
    let pattern = format!("{key} = ");
    if let Some(start) = content.find(&pattern) {
        let line_end = content[start..]
            .find('\n')
            .map_or(content.len(), |p| start + p + 1);
        content.replace_range(start..line_end, "");
    }
}

pub(crate) fn configure_tool_profile() {
    use crate::terminal_ui;
    use std::io::Write;

    let cfg = crate::core::config::Config::load();
    let current = cfg.tool_profile_effective();
    let pinned = cfg.tool_profile.is_some() || std::env::var("LEAN_CTX_TOOL_PROFILE").is_ok();

    // An explicitly pinned non-power profile is a deliberate, bounded choice —
    // don't re-nag. Power (pinned or legacy fallback) re-prompts because it
    // advertises every tool schema, the single largest fixed cost (#575).
    if pinned && !matches!(current, crate::core::tool_profiles::ToolProfile::Power) {
        terminal_ui::print_status_ok(&format!(
            "Tool profile: {} ({} tools)",
            current.as_str(),
            current.tool_count()
        ));
        return;
    }

    let dim = "\x1b[2m";
    let bold = "\x1b[1m";
    let cyan = "\x1b[36m";
    let rst = "\x1b[0m";

    let registry_count = crate::server::registry::tool_count();
    let lazy_count = crate::tool_defs::core_tool_names().len();

    println!("  {dim}Control how many MCP tool schemas your AI agent sees.{rst}");
    println!("  {dim}Fewer advertised tools = less context overhead. Every tool stays{rst}");
    println!("  {dim}callable through ctx_call, even when its schema is not advertised.{rst}");
    println!();
    println!(
        "  {cyan}lean{rst}      — {lazy_count} tools  {dim}(lazy core, recommended — lowest token overhead){rst}"
    );
    println!(
        "  {cyan}minimal{rst}   — 6 tools  {dim}(ctx_read, ctx_shell, ctx_search, ctx_glob, ctx_tree, ctx_symbol){rst}"
    );
    println!("  {cyan}standard{rst}  — 17 tools  {dim}(balanced set for most workflows){rst}");
    println!(
        "  {cyan}power{rst}     — {registry_count} tools  {dim}(everything advertised, costs the most context){rst}"
    );
    println!();
    print!("  Tool profile? {bold}[lean/minimal/standard/power]{rst} {dim}(default: lean){rst} ");
    std::io::stdout().flush().ok();

    let mut profile_input = String::new();
    let profile_name = if std::io::stdin().read_line(&mut profile_input).is_ok() {
        let trimmed = profile_input.trim().to_lowercase();
        match trimmed.as_str() {
            "minimal" | "min" => "minimal",
            "standard" | "std" => "standard",
            "power" | "full" | "all" => "power",
            _ => "lean",
        }
    } else {
        "lean"
    };

    if profile_name == "lean" {
        match crate::core::tool_profiles::clear_profile_in_config() {
            Ok(()) => terminal_ui::print_status_ok(&format!(
                "Tool profile: lean ({lazy_count} tools advertised, all reachable via ctx_call)"
            )),
            Err(e) => terminal_ui::print_status_warn(&format!("Could not save tool profile: {e}")),
        }
        return;
    }

    match crate::core::tool_profiles::set_profile_in_config(profile_name) {
        Ok(()) => {
            let profile = crate::core::tool_profiles::ToolProfile::parse(profile_name)
                .unwrap_or(crate::core::tool_profiles::ToolProfile::Standard);
            let count = match &profile {
                crate::core::tool_profiles::ToolProfile::Power => registry_count,
                other => other.tool_count(),
            };
            terminal_ui::print_status_ok(&format!("Tool profile: {profile_name} ({count} tools)"));
        }
        Err(e) => {
            terminal_ui::print_status_warn(&format!("Could not save tool profile: {e}"));
        }
    }
}

pub(crate) fn configure_premium_features(home: &std::path::Path) {
    use crate::terminal_ui;
    use std::io::Write;

    let config_path = crate::core::config::Config::path()
        .unwrap_or_else(|| home.join(".config/lean-ctx").join("config.toml"));
    if let Some(dir) = config_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut config_content = std::fs::read_to_string(&config_path).unwrap_or_default();

    let dim = "\x1b[2m";
    let bold = "\x1b[1m";
    let cyan = "\x1b[36m";
    let rst = "\x1b[0m";

    // Unified Compression Level (replaces terse_agent + output_density)
    println!("\n  {bold}Compression Level{rst} {dim}(controls all token optimization layers){rst}");
    println!("  {dim}Applies to tool output, agent prompts, and protocol mode.{rst}");
    println!();
    println!("  {cyan}off{rst}      — No compression (full verbose output)");
    println!(
        "  {cyan}lite{rst}     — Light: concise output, basic terse filtering {dim}(~25% savings){rst}"
    );
    println!(
        "  {cyan}standard{rst} — Dense output + compact protocol + pattern-aware {dim}(~45% savings){rst}"
    );
    println!(
        "  {cyan}max{rst}      — Expert mode: TDD protocol, all layers active {dim}(~65% savings){rst}"
    );
    println!();
    print!("  Compression level? {bold}[off/lite/standard/max]{rst} {dim}(default: off){rst} ");
    std::io::stdout().flush().ok();

    let mut level_input = String::new();
    let level = if std::io::stdin().read_line(&mut level_input).is_ok() {
        match level_input.trim().to_lowercase().as_str() {
            "lite" => "lite",
            "standard" | "std" => "standard",
            "max" => "max",
            _ => "off",
        }
    } else {
        "off"
    };

    // Stage the compression change in the config text; the success line is only
    // emitted after the write below actually persists (#415).
    let (effective_level, compression_status) = if level != "off" {
        upsert_toml_key(&mut config_content, "compression_level", level);
        remove_toml_key(&mut config_content, "terse_agent");
        remove_toml_key(&mut config_content, "output_density");
        (
            crate::core::config::CompressionLevel::from_str_label(level),
            StatusLine::ok(format!("Compression: {level}")),
        )
    } else if config_content.contains("compression_level") {
        upsert_toml_key(&mut config_content, "compression_level", "off");
        (
            Some(crate::core::config::CompressionLevel::Off),
            StatusLine::ok("Compression: off".to_string()),
        )
    } else {
        (
            Some(crate::core::config::CompressionLevel::Off),
            StatusLine::skip(
                "Compression: off (change later with: lean-ctx compression <level>)".to_string(),
            ),
        )
    };

    // Tool Result Archive
    println!(
        "\n  {bold}Tool Result Archive{rst} {dim}(zero-loss: large outputs archived, retrievable via ctx_expand){rst}"
    );
    print!("  Enable auto-archive? {bold}[Y/n]{rst} ");
    std::io::stdout().flush().ok();

    let mut archive_input = String::new();
    let archive_on = if std::io::stdin().read_line(&mut archive_input).is_ok() {
        let a = archive_input.trim().to_lowercase();
        a.is_empty() || a == "y" || a == "yes"
    } else {
        true
    };

    let archive_status = if archive_on && !config_content.contains("[archive]") {
        if !config_content.is_empty() && !config_content.ends_with('\n') {
            config_content.push('\n');
        }
        config_content.push_str("\n[archive]\nenabled = true\n");
        Some(StatusLine::ok("Tool Result Archive: enabled".to_string()))
    } else if !archive_on {
        Some(StatusLine::skip(
            "Archive: off (enable later in config.toml)".to_string(),
        ))
    } else {
        None
    };

    // Single atomic write. Only claim success — and only inject the rules prompt —
    // once the config has genuinely been persisted; a swallowed write error here
    // is exactly what made setup report settings it never applied (#415).
    match crate::config_io::write_atomic_with_backup(&config_path, &config_content) {
        Ok(()) => {
            compression_status.emit();
            if effective_level.is_some() {
                let home = dirs::home_dir().unwrap_or_default();
                let result = crate::rules_inject::inject_all_rules(&home);
                if !result.updated.is_empty() {
                    terminal_ui::print_status_ok(&format!(
                        "Updated {} rules file(s) with compression prompt",
                        result.updated.len()
                    ));
                }
            }
            if let Some(status) = archive_status {
                status.emit();
            }
        }
        Err(e) => {
            terminal_ui::print_status_warn(&format!(
                "Could not save settings to {}: {e}",
                config_path.display()
            ));
            terminal_ui::print_status_warn(
                "Premium features were not applied — re-run `lean-ctx setup` or edit config.toml manually",
            );
        }
    }
}

/// A setup status line whose emission is deferred until the underlying config
/// write succeeds, so the wizard never reports a setting it failed to persist.
struct StatusLine {
    skip: bool,
    msg: String,
}

impl StatusLine {
    fn ok(msg: String) -> Self {
        Self { skip: false, msg }
    }
    fn skip(msg: String) -> Self {
        Self { skip: true, msg }
    }
    fn emit(&self) {
        if self.skip {
            crate::terminal_ui::print_status_skip(&self.msg);
        } else {
            crate::terminal_ui::print_status_ok(&self.msg);
        }
    }
}
