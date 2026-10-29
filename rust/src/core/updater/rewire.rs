// SPDX-License-Identifier: Apache-2.0
//! After an update: the one-time config migrations, LaunchAgent re-wrap,
//! proxy restart and the setup refresh (`post_update_rewire`), limited to the
//! recorded agent selection or the already-connected tools.

/// #828: One-time migration — enable `shadow_mode` for users who never
/// explicitly set it. Without shadow mode, most harnesses silently prefer
/// native tools, negating lean-ctx's token savings. Idempotent: a state-dir
/// marker prevents repeated flips if the user disables it after migration.
fn migrate_shadow_mode_default() {
    let Ok(state) = crate::core::paths::state_dir() else {
        return;
    };
    let marker = state.join("shadow_mode_migrated");
    if marker.exists() {
        return;
    }

    // Read raw TOML to distinguish "explicitly set to false" from "never set".
    let global = crate::core::config::Config::load_global();
    let raw_toml = crate::core::config::Config::path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();

    let explicitly_set = raw_toml.lines().any(|line| {
        let trimmed = line.trim();
        trimmed.starts_with("shadow_mode") && trimmed.contains('=')
    });

    if !explicitly_set && !global.shadow_mode {
        // User never touched shadow_mode — the old default was false.
        // Set it to true in the global config so it persists.
        match crate::core::config::Config::update_global(|c| c.shadow_mode = true) {
            Ok(_) => {
                eprintln!("  \u{2139} shadow_mode enabled (recommended default since v3.9.9).");
                eprintln!("    Disable: lean-ctx config set shadow_mode false");
            }
            Err(e) => tracing::warn!("could not enable shadow_mode during update: {e}"),
        }
    } else if explicitly_set && !global.shadow_mode {
        eprintln!("  \u{2139} shadow_mode is false in your config. Since v3.9.9, true is the");
        eprintln!("    recommended default (60%+ more token savings). Enable:");
        eprintln!("    lean-ctx config set shadow_mode true");
    }

    // Write marker so this migration runs only once.
    if let Some(parent) = marker.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&marker, "migrated");
}

/// Public entry point for the shadow_mode migration, callable from
/// `cmd_dev_install` and other lifecycle commands.
pub(crate) fn migrate_shadow_mode_default_public() {
    migrate_shadow_mode_default();
}

pub(super) fn post_update_rewire(skip_rules: bool, quiet: bool) {
    // #356: regenerate installed LaunchAgent plists so they adopt the new
    // deny-~/Documents seatbelt wrapper. A plist is only (re)written on install,
    // so without this an upgrade keeps the old unwrapped plist — and the TCC
    // prompt — until the next manual enable. Idempotent: install rewrites the
    // plist and re-bootstraps it.
    #[cfg(target_os = "macos")]
    rewrap_launchagents_for_tcc();

    // The persist decision reads the GLOBAL file only and writes via
    // update_global, so a project-local override is never leaked into the
    // global config (#443).
    if crate::core::config::Config::load_global()
        .proxy_enabled
        .is_none()
        && crate::proxy_autostart::is_installed()
    {
        match crate::core::config::Config::update_global(|c| c.proxy_enabled = Some(true)) {
            Ok(_) => {
                eprintln!("  \u{2139} Proxy was already active \u{2014} keeping enabled.");
                eprintln!("    Disable anytime: lean-ctx proxy disable");
            }
            Err(e) => tracing::warn!("could not persist proxy_enabled during update: {e}"),
        }
    }

    // #828: shadow_mode default changed to true. Migrate existing configs
    // that never explicitly set it (still at old default false).
    migrate_shadow_mode_default();

    // Runtime decisions use the effective (global + project-local) config.
    let cfg = crate::core::config::Config::load();
    let proxy_active = cfg.proxy_enabled == Some(true);

    // Determine whether rules should be injected during rewire.
    // CLI --skip-rules always wins. Otherwise, respect the config setting.
    let effective_skip_rules = if skip_rules {
        true
    } else {
        !cfg.setup.should_inject_rules()
    };

    // PHASE 1: Restart proxy BEFORE writing env vars.
    if proxy_active {
        restart_proxy_if_running();
        wait_for_proxy_health(crate::proxy_setup::default_port());
    }

    // PHASE 2: Run setup which writes MCP configs (always) and rules (if opted in).
    // #1026: shell hooks are refreshed here — but install_all_with_style
    // respects shell_hook_disabled, so users who opt out keep control.
    if !cfg.shell_hook_disabled_effective() {
        eprintln!("  \u{2139} Refreshing shell hooks in ~/.zshenv / ~/.bashenv.");
        eprintln!("    Set shell_hook_disabled=true to prevent this in future updates.");
    }
    // Only the recorded selection (setup.agents) or, without one, the tools
    // that are already connected: an update never connects a new tool.
    if let Err(e) = crate::setup::run_setup_with_options(&rewire_options(
        proxy_active,
        effective_skip_rules,
        false,
    )) {
        tracing::error!("Setup refresh error: {e}");
    }
    if !quiet {
        print_unconnected_tools_hint();
    }
}

fn rewire_options(
    proxy_active: bool,
    skip_rules: bool,
    dry_run: bool,
) -> crate::setup::SetupOptions {
    crate::setup::SetupOptions {
        non_interactive: true,
        yes: true,
        fix: true,
        skip_proxy: !proxy_active,
        skip_rules,
        refresh_only: true,
        dry_run,
        ..Default::default()
    }
}

/// `update --dry-run`: the refresh an update would run, without writing.
pub(super) fn print_rewire_plan(skip_rules: bool) {
    let cfg = crate::core::config::Config::load();
    let skip_rules = skip_rules || !cfg.setup.should_inject_rules();
    match crate::setup::run_setup_with_options(&rewire_options(
        cfg.proxy_enabled == Some(true),
        skip_rules,
        true,
    )) {
        Ok(report) => crate::setup::print_plan(&report),
        Err(e) => eprintln!("  \x1b[31m✗\x1b[0m Cannot plan the setup refresh: {e}"),
    }
    print_unconnected_tools_hint();
}

/// Name installed tools the refresh left alone, so connecting one stays a
/// deliberate step.
fn print_unconnected_tools_hint() {
    use crate::setup::agent_selection as sel;
    let Some(home) = crate::core::home::resolve_home_dir() else {
        return;
    };
    let targets = crate::core::editor_registry::build_targets(&home);
    let scope = sel::scope_for_run(&home, &targets, None, true);
    let skipped: Vec<String> = sel::detected_agents(&home, &targets)
        .into_iter()
        .filter(|a| !scope.allows(&a.key))
        .map(|a| a.key)
        .collect();
    if !skipped.is_empty() {
        eprintln!(
            "  \u{2139} Not connected (left untouched): {}",
            skipped.join(", ")
        );
        eprintln!("    Connect one: lean-ctx init --agent <name>  ·  choose: lean-ctx setup");
    }
}

/// #356: rewrite every installed LaunchAgent plist (daemon, proxy, auto-updater)
/// so an upgrade re-emits them with the deny-~/Documents seatbelt wrapper.
/// plists are only generated on install, so an existing install would otherwise
/// keep the pre-wrapper plist — and the prompt — until the next manual `enable`.
/// Each `install` / `install_schedule` rewrites the plist and re-bootstraps it.
#[cfg(target_os = "macos")]
fn rewrap_launchagents_for_tcc() {
    if crate::proxy_autostart::is_installed() {
        crate::proxy_autostart::install(crate::proxy_setup::default_port(), true);
    }
    if crate::daemon_autostart::is_installed() {
        crate::daemon_autostart::install(true);
    }
    if crate::core::update_scheduler::schedule_status().enabled {
        let hours = crate::core::config::Config::load()
            .updates
            .check_interval_hours;
        if let Err(e) = crate::core::update_scheduler::install_schedule(hours) {
            tracing::warn!("#356 re-wrap of auto-update LaunchAgent failed: {e}");
        }
    }
}

fn wait_for_proxy_health(port: u16) {
    let max_attempts = 20;
    for i in 0..max_attempts {
        if is_proxy_reachable(port) {
            println!("  \x1b[32m✓\x1b[0m Proxy healthy on port {port}");
            return;
        }
        if i == 0 {
            print!("  \x1b[2mWaiting for proxy to become healthy");
        }
        print!(".");
        use std::io::Write;
        std::io::stdout().flush().ok();
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    println!();
    eprintln!(
        "  \x1b[33m⚠\x1b[0m Proxy did not respond within {}s — writing env vars anyway",
        max_attempts / 2
    );
    eprintln!("    If Claude Code shows connection errors, run: lean-ctx proxy start");
}

fn restart_proxy_if_running() {
    let port = crate::proxy_setup::default_port();

    if restart_managed_proxy() {
        return;
    }

    if is_proxy_reachable(port) {
        println!(
            "  \x1b[33m⟳\x1b[0m Proxy running on port {port} — restart it to use the new binary:"
        );
        println!("    \x1b[1mlean-ctx proxy start --port={port}\x1b[0m");
    }
}

/// Restart proxy managed by launchd (macOS) or systemd (Linux).
/// Returns `true` if a managed service was found and restarted.
fn restart_managed_proxy() -> bool {
    #[cfg(target_os = "macos")]
    {
        let plist_path = dirs::home_dir()
            .unwrap_or_default()
            .join("Library/LaunchAgents/com.leanctx.proxy.plist");
        if plist_path.exists() {
            if crate::core::launchd::bootstrap("com.leanctx.proxy", &plist_path) {
                println!("  \x1b[32m✓\x1b[0m Proxy restarted (LaunchAgent)");
            } else {
                println!("  \x1b[33m⚠\x1b[0m Could not restart proxy LaunchAgent");
            }
            return true;
        }
    }

    #[cfg(target_os = "linux")]
    {
        let service_path = dirs::home_dir()
            .unwrap_or_default()
            .join(".config/systemd/user/lean-ctx-proxy.service");
        if service_path.exists() {
            let result = std::process::Command::new("systemctl")
                .args(["--user", "restart", "lean-ctx-proxy"])
                .output();
            match result {
                Ok(o) if o.status.success() => {
                    println!("  \x1b[32m✓\x1b[0m Proxy restarted (systemd)");
                }
                _ => {
                    println!("  \x1b[33m⚠\x1b[0m Could not restart proxy systemd service");
                }
            }
            return true;
        }
    }

    false
}

fn is_proxy_reachable(port: u16) -> bool {
    ureq::get(&format!("http://127.0.0.1:{port}/health"))
        .call()
        .is_ok()
}
