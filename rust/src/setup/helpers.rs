//! Setup helper routines (skill install, TOML key upserts, profile + premium
//! feature configuration). Split out of `setup/mod.rs` for focus.

#[allow(clippy::wildcard_imports)]
use super::*;

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
    let _ = std::fs::write(&steering_file, crate::hooks::KIRO_STEERING_TEMPLATE);
    println!("  \x1b[32m✓\x1b[0m Created .kiro/steering/lean-ctx.md (Kiro will now prefer lean-ctx tools)");
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

    if !has_vscode && !has_claude {
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

    if !matches!(current, crate::core::tool_profiles::ToolProfile::Power)
        && cfg.tool_profile.is_some()
    {
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

    println!("  {dim}Control how many MCP tools your AI agent sees.{rst}");
    println!("  {dim}Fewer tools = less context overhead, faster agent responses.{rst}");
    println!();
    println!(
        "  {cyan}minimal{rst}   — 6 tools   {dim}(ctx_read, ctx_shell, shell, ctx_search, ctx_tree, ctx_session){rst}"
    );
    println!("  {cyan}standard{rst}  — 21 tools  {dim}(balanced set for most workflows){rst}");
    println!(
        "  {cyan}power{rst}     — {registry_count} tools  {dim}(everything, for power users){rst}"
    );
    println!();
    print!("  Tool profile? {bold}[minimal/standard/power]{rst} {dim}(default: standard){rst} ");
    std::io::stdout().flush().ok();

    let mut profile_input = String::new();
    let profile_name = if std::io::stdin().read_line(&mut profile_input).is_ok() {
        let trimmed = profile_input.trim().to_lowercase();
        match trimmed.as_str() {
            "minimal" | "min" => "minimal",
            "power" | "full" | "all" => "power",
            _ => "standard",
        }
    } else {
        "standard"
    };

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

    let config_dir = crate::core::data_dir::lean_ctx_data_dir()
        .unwrap_or_else(|_| home.join(".config/lean-ctx"));
    let _ = std::fs::create_dir_all(&config_dir);
    let config_path = config_dir.join("config.toml");
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
    println!("  {cyan}lite{rst}     — Light: concise output, basic terse filtering {dim}(~25% savings){rst}");
    println!("  {cyan}standard{rst} — Dense output + compact protocol + pattern-aware {dim}(~45% savings){rst}");
    println!("  {cyan}max{rst}      — Expert mode: TDD protocol, all layers active {dim}(~65% savings){rst}");
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

    let effective_level = if level != "off" {
        upsert_toml_key(&mut config_content, "compression_level", level);
        remove_toml_key(&mut config_content, "terse_agent");
        remove_toml_key(&mut config_content, "output_density");
        terminal_ui::print_status_ok(&format!("Compression: {level}"));
        crate::core::config::CompressionLevel::from_str_label(level)
    } else if config_content.contains("compression_level") {
        upsert_toml_key(&mut config_content, "compression_level", "off");
        terminal_ui::print_status_ok("Compression: off");
        Some(crate::core::config::CompressionLevel::Off)
    } else {
        terminal_ui::print_status_skip(
            "Compression: off (change later with: lean-ctx compression <level>)",
        );
        Some(crate::core::config::CompressionLevel::Off)
    };

    if let Some(lvl) = effective_level {
        let n = crate::core::terse::rules_inject::inject(&lvl);
        if n > 0 {
            terminal_ui::print_status_ok(&format!(
                "Updated {n} rules file(s) with compression prompt"
            ));
        }
    }

    // Tool Result Archive (unchanged)
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

    if archive_on && !config_content.contains("[archive]") {
        if !config_content.is_empty() && !config_content.ends_with('\n') {
            config_content.push('\n');
        }
        config_content.push_str("\n[archive]\nenabled = true\n");
        terminal_ui::print_status_ok("Tool Result Archive: enabled");
    } else if !archive_on {
        terminal_ui::print_status_skip("Archive: off (enable later in config.toml)");
    }

    let _ = crate::config_io::write_atomic_with_backup(&config_path, &config_content);
}
