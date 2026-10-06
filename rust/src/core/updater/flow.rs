// SPDX-License-Identifier: Apache-2.0
use super::{
    AutoUpdateGate, CURRENT_VERSION, UpdateMode, acquire_update_lock, automatic_update_gate,
    constant_time_eq, download_bytes, execute_prepared_transaction, extract_binary, fetch_release,
    find_asset_url, gpu_next_steps, gpu_platform_asset_name, load_update_receipt,
    looks_like_version, parse_target_version, platform_asset_name, post_update_rewire,
    prepare_update_transaction, recover_pending_transaction, rollback_to_previous, sha256_hex,
    verify_download_integrity,
};

pub(super) fn run_with_mode(args: &[String], mode: UpdateMode) {
    let mut check_only = args.iter().any(|a| a == "--check");
    let quiet = args.iter().any(|a| a == "--quiet");
    let skip_rules = args.iter().any(|a| a == "--skip-rules");
    // The scheduler invokes `update --quiet --scheduled`. `--quiet` alone also
    // marks an automatic run for backward compatibility with schedulers that
    // were installed before `--scheduled` existed.
    let scheduled = args.iter().any(|a| a == "--scheduled");

    if args.iter().any(|a| a == "--insecure") {
        eprintln!(
            "  \x1b[31m✗\x1b[0m `--insecure` is no longer supported; refusing an unverifiable update."
        );
        std::process::exit(2);
    }

    // The Windows deferred helper invokes the newly installed binary with this
    // private recovery flag. It must never perform a network update. The helper
    // holds the updater lock while it swaps files, so it passes the private
    // lock-held marker to avoid trying to acquire the same lock recursively.
    if args.iter().any(|a| a == "--recover-update") {
        let lock_held = args.iter().any(|a| a == "--lock-held");
        let _lock = if lock_held {
            None
        } else {
            match acquire_update_lock() {
                Ok(lock) => Some(lock),
                Err(e) => {
                    eprintln!("  \x1b[31m✗\x1b[0m Cannot recover update state: {e}");
                    std::process::exit(1);
                }
            }
        };
        let current_exe = match std::env::current_exe() {
            Ok(path) => path,
            Err(e) => {
                eprintln!("  \x1b[31m✗\x1b[0m Cannot locate current executable: {e}");
                std::process::exit(1);
            }
        };
        if let Err(e) = recover_pending_transaction(&current_exe) {
            eprintln!("  \x1b[31m✗\x1b[0m Update recovery failed: {e}");
            std::process::exit(1);
        }
        return;
    }

    // Handle --schedule subcommand
    if let Some(pos) = args.iter().position(|a| a == "--schedule") {
        let sub = args.get(pos + 1).map_or("", String::as_str);
        match sub {
            "off" | "disable" => {
                if let Err(e) = crate::core::update_scheduler::remove_schedule() {
                    eprintln!("  \x1b[31m✗\x1b[0m Failed to disable auto-updates: {e}");
                    std::process::exit(1);
                }
                crate::core::update_scheduler::set_auto_update(false, false, 6);
                println!("  \x1b[32m✓\x1b[0m Auto-updates disabled.");
                println!("  \x1b[2mRe-enable anytime: lean-ctx update --schedule\x1b[0m");
                return;
            }
            "status" => {
                let info = crate::core::update_scheduler::schedule_status();
                println!();
                println!("  {info}");
                println!();
                return;
            }
            "notify" => {
                let cfg = crate::core::config::Config::load();
                let hours = cfg.updates.check_interval_hours;
                match crate::core::update_scheduler::install_schedule(hours) {
                    Ok(info) => {
                        crate::core::update_scheduler::set_auto_update(true, true, hours);
                        println!("  \x1b[32m✓\x1b[0m Update notifications enabled ({info})");
                        println!(
                            "  \x1b[2mYou'll be notified but updates won't install automatically.\x1b[0m"
                        );
                    }
                    Err(e) => {
                        eprintln!("  \x1b[31m✗\x1b[0m {e}");
                        std::process::exit(1);
                    }
                }
                return;
            }
            _ => {
                let hours = if sub.is_empty() {
                    6
                } else {
                    sub.trim_end_matches('h')
                        .parse::<u64>()
                        .unwrap_or(6)
                        .clamp(1, 168)
                };
                match crate::core::update_scheduler::install_schedule(hours) {
                    Ok(info) => {
                        crate::core::update_scheduler::set_auto_update(true, false, hours);
                        println!();
                        println!("  \x1b[32m✓\x1b[0m {info}");
                        println!("  \x1b[2mDisable anytime: lean-ctx update --schedule off\x1b[0m");
                        println!();
                    }
                    Err(e) => {
                        eprintln!("  \x1b[31m✗\x1b[0m Failed to enable auto-updates: {e}");
                        std::process::exit(1);
                    }
                }
                return;
            }
        }
    }

    if args.iter().any(|a| a == "--status") {
        let pin = crate::core::config::Config::load_global()
            .updates
            .pinned_version
            .unwrap_or_else(|| "<latest>".to_string());
        println!("Pinned version: {pin}");
        match load_update_receipt() {
            Ok(Some(receipt)) => println!(
                "{}",
                serde_json::to_string_pretty(&receipt).unwrap_or_default()
            ),
            Ok(None) => println!("No update receipt is recorded."),
            Err(e) => {
                eprintln!("  \x1b[31m✗\x1b[0m Cannot read update receipt: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    if args.iter().any(|a| a == "--rollback") {
        if let Err(e) = rollback_to_previous() {
            eprintln!("  \x1b[31m✗\x1b[0m Rollback failed: {e}");
            std::process::exit(1);
        }
        println!("  \x1b[32m✓\x1b[0m Rolled back to the retained verified binary.");
        return;
    }

    if args.iter().any(|a| a == "--unpin") {
        if let Err(e) = crate::core::config::Config::update_global(|cfg| {
            cfg.updates.pinned_version = None;
        }) {
            eprintln!("  \x1b[31m✗\x1b[0m Cannot clear update pin: {e}");
            std::process::exit(1);
        }
        println!("  \x1b[32m✓\x1b[0m Update pin cleared; scheduled updates follow latest.");
        return;
    }

    if let Some(pos) = args.iter().position(|a| a == "--pin") {
        let value = args.get(pos + 1).map(String::as_str).unwrap_or("");
        if !looks_like_version(value) {
            eprintln!("  \x1b[31m✗\x1b[0m `--pin` requires a release version such as 3.8.5.");
            std::process::exit(1);
        }
        let normalized = value.trim_start_matches('v').to_string();
        if let Err(e) = crate::core::config::Config::update_global(|cfg| {
            cfg.updates.pinned_version = Some(normalized.clone());
        }) {
            eprintln!("  \x1b[31m✗\x1b[0m Cannot persist update pin: {e}");
            std::process::exit(1);
        }
        println!("  \x1b[32m✓\x1b[0m Update pin set to v{normalized}.");
    }

    // #447: `lean-ctx update <version>` installs a specific tagged release
    // instead of the latest (e.g. to compare against an older build). Only the
    // binary is swapped — data, config and logs are left untouched, exactly
    // like a normal update.
    let explicit_target = match parse_target_version(args) {
        None => None,
        Some(v) if looks_like_version(v) => Some(v.trim_start_matches('v').to_string()),
        Some(other) => {
            eprintln!("  \x1b[31m✗\x1b[0m '{other}' is not a valid version.");
            eprintln!(
                "  \x1b[2mUsage: lean-ctx update [<version>]   (e.g. lean-ctx update 3.8.5)\x1b[0m"
            );
            eprintln!(
                "  \x1b[2mAvailable versions: https://github.com/yvgude/lean-ctx/releases\x1b[0m"
            );
            std::process::exit(1);
        }
    };
    let global_config = crate::core::config::Config::load_global();
    let config_for_run = Some(crate::core::config::Config::load());
    let target_version = explicit_target.or_else(|| global_config.updates.pinned_version.clone());
    let pinned = target_version.is_some();

    // #335: An automatic run (`--quiet`/`--scheduled`) must obey config.toml.
    // A user who sets `updates.auto_update = false` after a scheduler was
    // installed expects auto-updates to stop. Since editing config doesn't
    // uninstall the scheduler, the next scheduled tick re-checks config here,
    // self-heals (removes the orphaned scheduler) and bails. `notify_only`
    // downgrades the run to a check (never installs). Manual `lean-ctx update`
    // (no `--quiet`/`--scheduled`) is an explicit action and always proceeds.
    if (quiet || scheduled) && !check_only {
        let cfg = config_for_run
            .clone()
            .unwrap_or_else(crate::core::config::Config::load);
        match automatic_update_gate(cfg.updates.auto_update, cfg.updates.notify_only) {
            AutoUpdateGate::Skip => {
                if let Err(e) = crate::core::update_scheduler::remove_schedule() {
                    tracing::warn!(
                        "auto-update disabled in config; failed to remove orphaned scheduler: {e}"
                    );
                } else {
                    tracing::info!(
                        "auto-update disabled (updates.auto_update=false): skipped scheduled update and removed orphaned scheduler"
                    );
                }
                return;
            }
            AutoUpdateGate::NotifyOnly => {
                check_only = true;
            }
            AutoUpdateGate::Proceed => {}
        }
    }

    if !quiet {
        println!();
        let title = match mode {
            UpdateMode::Normal => "lean-ctx updater",
            UpdateMode::EnableGpu => "lean-ctx GPU enablement",
        };
        println!("  \x1b[1m◆ {title}\x1b[0m  \x1b[2mv{CURRENT_VERSION}\x1b[0m");
        println!("  \x1b[2mChecking github.com/yvgude/lean-ctx …\x1b[0m");
    }

    let release = match fetch_release(target_version.as_deref()) {
        Ok(r) => r,
        Err(e) => {
            if let Some(v) = &target_version {
                tracing::error!("Could not fetch lean-ctx v{v}: {e}");
                tracing::error!(
                    "Check the version exists: https://github.com/yvgude/lean-ctx/releases"
                );
            } else {
                tracing::error!("Error fetching release info: {e}");
            }
            std::process::exit(1);
        }
    };

    let target_tag = if let Some(t) = release["tag_name"].as_str() {
        t.trim_start_matches('v').to_string()
    } else {
        tracing::error!("Could not parse release tag from GitHub API.");
        std::process::exit(1);
    };

    if target_tag == CURRENT_VERSION && mode == UpdateMode::Normal {
        if quiet {
            return;
        }
        if pinned {
            println!("  \x1b[32m✓\x1b[0m Already on v{CURRENT_VERSION}.");
        } else {
            println!("  \x1b[32m✓\x1b[0m Already up to date (v{CURRENT_VERSION}).");
        }
        println!(
            "  \x1b[2mIf your IDE still uses an older version, restart it to reconnect the MCP server.\x1b[0m"
        );
        println!();
        if !check_only {
            if skip_rules {
                println!(
                    "  \x1b[36m\x1b[1mRefreshing setup (shell hook, MCP configs — rules skipped)…\x1b[0m"
                );
            } else {
                println!(
                    "  \x1b[36m\x1b[1mRefreshing setup (shell hook, MCP configs, rules)…\x1b[0m"
                );
            }
            post_update_rewire(skip_rules);
            println!();
        }
        return;
    }

    if !quiet {
        if pinned {
            println!(
                "  Switching: v{CURRENT_VERSION} → \x1b[1;36mv{target_tag}\x1b[0m  \x1b[2m(data & logs preserved)\x1b[0m"
            );
        } else if target_tag == CURRENT_VERSION {
            println!("  Installing GPU binary for v{CURRENT_VERSION}…");
        } else {
            println!("  Update available: v{CURRENT_VERSION} → \x1b[1;32mv{target_tag}\x1b[0m");
        }
    }

    let asset_name = match mode {
        UpdateMode::Normal => platform_asset_name(),
        UpdateMode::EnableGpu => match gpu_platform_asset_name() {
            Ok(name) => name,
            Err(e) => {
                tracing::error!("{e}");
                std::process::exit(1);
            }
        },
    };

    if check_only {
        match mode {
            UpdateMode::Normal if pinned => {
                println!("Run 'lean-ctx update {target_tag}' to install.");
            }
            UpdateMode::Normal => println!("Run 'lean-ctx update' to install."),
            UpdateMode::EnableGpu => println!("Run 'lean-ctx enable-gpu' to install {asset_name}."),
        }
        return;
    }

    let _update_lock = match acquire_update_lock() {
        Ok(lock) => lock,
        Err(e) => {
            tracing::error!("Cannot start update transaction: {e}");
            std::process::exit(1);
        }
    };
    let current_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Cannot locate current executable: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = recover_pending_transaction(&current_exe) {
        tracing::error!("Refusing update until pending transaction is resolved: {e}");
        std::process::exit(1);
    }

    if !quiet {
        println!("  \x1b[2mDownloading {asset_name} …\x1b[0m");
    }

    let Some(download_url) = find_asset_url(&release, &asset_name) else {
        tracing::error!(
            "No binary found for this platform ({asset_name}) in v{target_tag}. Download manually: https://github.com/yvgude/lean-ctx/releases"
        );
        std::process::exit(1);
    };

    let bytes = match download_bytes(&download_url) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!("Download failed: {e}");
            std::process::exit(1);
        }
    };

    let verified = match verify_download_integrity(&release, &asset_name, &bytes) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("Integrity verification failed: {e}");
            tracing::error!(
                "Refusing to install an unverifiable binary; obtain a release with a valid manifest and SHA256SUMS."
            );
            std::process::exit(1);
        }
    };

    let binary_bytes = match extract_binary(&bytes, &asset_name) {
        Ok(binary) if !binary.is_empty() => binary,
        Ok(_) => {
            tracing::error!("Release archive contained an empty lean-ctx binary");
            std::process::exit(1);
        }
        Err(e) => {
            tracing::error!("Failed to extract verified binary: {e}");
            std::process::exit(1);
        }
    };

    if !constant_time_eq(
        verified.payload_sha256.as_bytes(),
        sha256_hex(&binary_bytes).as_bytes(),
    ) {
        tracing::error!("Extracted binary digest does not match the signed release manifest");
        std::process::exit(1);
    }

    let transaction = match prepare_update_transaction(
        &current_exe,
        &asset_name,
        &target_tag,
        &verified,
        &binary_bytes,
    ) {
        Ok(transaction) => transaction,
        Err(e) => {
            tracing::error!("Refusing update because rollback state is not safe: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = execute_prepared_transaction(&transaction, &current_exe) {
        tracing::error!("Failed to complete durable update transaction: {e}");
        std::process::exit(1);
    }

    if quiet {
        println!("  lean-ctx v{CURRENT_VERSION} → v{target_tag}");
    } else {
        println!();
        if pinned {
            println!("  \x1b[1;32m✓ Now running lean-ctx v{target_tag}\x1b[0m");
        } else if mode == UpdateMode::EnableGpu {
            println!("  \x1b[1;32m✓ Enabled lean-ctx GPU binary v{target_tag}\x1b[0m");
        } else {
            println!("  \x1b[1;32m✓ Updated to lean-ctx v{target_tag}\x1b[0m");
        }
        println!("  \x1b[2mBinary: {}\x1b[0m", current_exe.display());
        if mode == UpdateMode::EnableGpu {
            for line in gpu_next_steps(std::env::consts::OS) {
                println!("  \x1b[2m{line}\x1b[0m");
            }
        }
    }

    if !quiet {
        println!();
        if skip_rules {
            println!(
                "  \x1b[36m\x1b[1mRefreshing setup (shell hook, MCP configs — rules skipped)…\x1b[0m"
            );
        } else {
            println!("  \x1b[36m\x1b[1mRefreshing setup (shell hook, MCP configs, rules)…\x1b[0m");
        }
    }
    post_update_rewire(skip_rules);

    if !quiet {
        println!();
        crate::terminal_ui::print_logo_animated();
        println!();
        println!(
            "  \x1b[33m\x1b[1m⟳ Restart your IDE and shell to activate the new version.\x1b[0m"
        );
        println!(
            "    \x1b[2mClose and re-open Cursor, VS Code, Claude Code, etc. completely.\x1b[0m"
        );
        println!("    \x1b[2mThe MCP server must reconnect to use the updated binary.\x1b[0m");
        println!(
            "    \x1b[2m{}\x1b[0m",
            crate::shell_hook::reload_aliases_hint()
        );
    }
    println!();

    if !quiet
        && !crate::core::update_scheduler::has_user_decided()
        && std::io::IsTerminal::is_terminal(&std::io::stdin())
    {
        print!("  Want to get updates like this automatically? \x1b[1m[y/N]\x1b[0m ");
        use std::io::Write;
        std::io::stdout().flush().ok();
        let mut input = String::new();
        if std::io::stdin().read_line(&mut input).is_ok() {
            let answer = input.trim().to_lowercase();
            if answer == "y" || answer == "yes" {
                let cfg = crate::core::config::Config::load();
                let hours = cfg.updates.check_interval_hours;
                match crate::core::update_scheduler::install_schedule(hours) {
                    Ok(info) => {
                        crate::core::update_scheduler::set_auto_update(true, false, hours);
                        println!("  \x1b[32m✓\x1b[0m {info}");
                        println!("  \x1b[2mDisable anytime: lean-ctx update --schedule off\x1b[0m");
                    }
                    Err(e) => println!("  \x1b[33m⚠\x1b[0m Could not set up scheduler: {e}"),
                }
            } else {
                crate::core::update_scheduler::set_auto_update(false, false, 6);
                println!("  \x1b[2m○ Skipped — enable later: lean-ctx update --schedule\x1b[0m");
            }
        }
    }
}
