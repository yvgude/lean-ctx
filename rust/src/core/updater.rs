use std::io::Read;

const GITHUB_API_RELEASES: &str = "https://api.github.com/repos/yvgude/lean-ctx/releases/latest";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn run(args: &[String]) {
    let mut check_only = args.iter().any(|a| a == "--check");
    let insecure = args.iter().any(|a| a == "--insecure");
    let quiet = args.iter().any(|a| a == "--quiet");
    let skip_rules = args.iter().any(|a| a == "--skip-rules");
    // The scheduler invokes `update --quiet --scheduled`. `--quiet` alone also
    // marks an automatic run for backward compatibility with schedulers that
    // were installed before `--scheduled` existed.
    let scheduled = args.iter().any(|a| a == "--scheduled");

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

    // #447: `lean-ctx update <version>` installs a specific tagged release
    // instead of the latest (e.g. to compare against an older build). Only the
    // binary is swapped — data, config and logs are left untouched, exactly
    // like a normal update.
    let target_version: Option<String> = match parse_target_version(args) {
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
    let pinned = target_version.is_some();

    // #335: An automatic run (`--quiet`/`--scheduled`) must obey config.toml.
    // A user who sets `updates.auto_update = false` after a scheduler was
    // installed expects auto-updates to stop. Since editing config doesn't
    // uninstall the scheduler, the next scheduled tick re-checks config here,
    // self-heals (removes the orphaned scheduler) and bails. `notify_only`
    // downgrades the run to a check (never installs). Manual `lean-ctx update`
    // (no `--quiet`/`--scheduled`) is an explicit action and always proceeds.
    if (quiet || scheduled) && !check_only {
        let cfg = crate::core::config::Config::load();
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
        println!("  \x1b[1m◆ lean-ctx updater\x1b[0m  \x1b[2mv{CURRENT_VERSION}\x1b[0m");
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

    if target_tag == CURRENT_VERSION {
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
        } else {
            println!("  Update available: v{CURRENT_VERSION} → \x1b[1;32mv{target_tag}\x1b[0m");
        }
    }

    if check_only {
        if pinned {
            println!("Run 'lean-ctx update {target_tag}' to install.");
        } else {
            println!("Run 'lean-ctx update' to install.");
        }
        return;
    }

    let asset_name = platform_asset_name();
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

    if let Err(e) = verify_download_integrity(&release, &asset_name, &bytes) {
        if insecure {
            tracing::warn!("Integrity verification failed: {e}");
            tracing::warn!("Proceeding due to --insecure");
        } else {
            tracing::error!("Integrity verification failed: {e}");
            tracing::error!(
                "Refusing to install an unverifiable binary. Re-run with `lean-ctx update --insecure` or download manually: https://github.com/yvgude/lean-ctx/releases"
            );
            std::process::exit(1);
        }
    }

    let current_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Cannot locate current executable: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = replace_binary(&bytes, &asset_name, &current_exe) {
        tracing::error!("Failed to replace binary: {e}");
        tracing::warn!("Continuing with a setup refresh so your wiring stays correct");
        post_update_rewire(skip_rules);
        std::process::exit(1);
    }

    if quiet {
        println!("  lean-ctx v{CURRENT_VERSION} → v{target_tag}");
    } else {
        println!();
        if pinned {
            println!("  \x1b[1;32m✓ Now running lean-ctx v{target_tag}\x1b[0m");
        } else {
            println!("  \x1b[1;32m✓ Updated to lean-ctx v{target_tag}\x1b[0m");
        }
        println!("  \x1b[2mBinary: {}\x1b[0m", current_exe.display());
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

/// Outcome of the config gate applied to automatic (scheduled) update runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoUpdateGate {
    /// Install normally.
    Proceed,
    /// Auto-update disabled in config — skip and clean up the scheduler.
    Skip,
    /// Notify-only — check for a newer version but never install.
    NotifyOnly,
}

/// Decide what a scheduled `update` run should do based on config. Pure helper
/// so the precedence (`auto_update` wins over `notify_only`) is unit-testable.
fn automatic_update_gate(auto_update: bool, notify_only: bool) -> AutoUpdateGate {
    if !auto_update {
        AutoUpdateGate::Skip
    } else if notify_only {
        AutoUpdateGate::NotifyOnly
    } else {
        AutoUpdateGate::Proceed
    }
}

fn verify_download_integrity(
    release: &serde_json::Value,
    asset_name: &str,
    bytes: &[u8],
) -> Result<(), String> {
    #[cfg(not(feature = "secure-update"))]
    {
        let _ = (release, asset_name, bytes);
        return Err("secure-update feature disabled (sha256 verification unavailable)".to_string());
    }

    #[cfg(feature = "secure-update")]
    {
        let computed = sha256_hex(bytes);

        let Some((checksum_url, kind)) = find_checksum_asset_url(release, asset_name) else {
            return Err(
                "no checksum asset found for this release (expected SHA256SUMS or *.sha256)"
                    .to_string(),
            );
        };
        let checksum_bytes = download_bytes(&checksum_url)?;
        let checksum_text = String::from_utf8_lossy(&checksum_bytes).to_string();

        let expected = match kind {
            ChecksumAssetKind::SingleSha256 => parse_single_sha256(&checksum_text),
            ChecksumAssetKind::Sha256Sums => parse_sha256sums(&checksum_text, asset_name),
        }
        .ok_or_else(|| format!("checksum file did not contain an entry for {asset_name}"))?;

        if !constant_time_eq(computed.as_bytes(), expected.as_bytes()) {
            return Err(format!(
                "sha256 mismatch for {asset_name}: expected {expected}, got {computed}"
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
enum ChecksumAssetKind {
    Sha256Sums,
    SingleSha256,
}

fn find_checksum_asset_url(
    release: &serde_json::Value,
    asset_name: &str,
) -> Option<(String, ChecksumAssetKind)> {
    // Prefer per-asset checksum (asset.ext.sha256) if present.
    let candidates = [
        format!("{asset_name}.sha256"),
        format!("{asset_name}.sha256.txt"),
        "SHA256SUMS".to_string(),
        "SHA256SUMS.txt".to_string(),
        "sha256sums.txt".to_string(),
        "checksums.txt".to_string(),
    ];

    for c in candidates {
        if let Some(url) = find_asset_url(release, &c) {
            let kind = if c.to_lowercase().contains("sha256sums")
                || c.to_uppercase() == "SHA256SUMS"
                || c.to_lowercase().contains("checksums")
            {
                ChecksumAssetKind::Sha256Sums
            } else {
                ChecksumAssetKind::SingleSha256
            };
            return Some((url, kind));
        }
    }
    None
}

fn parse_single_sha256(text: &str) -> Option<String> {
    let t = text.trim();
    let first = t.split_whitespace().next().unwrap_or("").trim();
    if first.len() == 64 && first.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(first.to_ascii_lowercase())
    } else {
        None
    }
}

fn parse_sha256sums(text: &str, asset_name: &str) -> Option<String> {
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let mut parts = l.split_whitespace();
        let hash = parts.next().unwrap_or("");
        let file = parts.next().unwrap_or("");
        if file == asset_name && hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    let out = h.finalize();
    hex_lower(&out)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn post_update_rewire(skip_rules: bool) {
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
    let opts = crate::setup::SetupOptions {
        non_interactive: true,
        yes: true,
        fix: true,
        skip_proxy: !proxy_active,
        skip_rules: effective_skip_rules,
        ..Default::default()
    };
    if let Err(e) = crate::setup::run_setup_with_options(opts) {
        tracing::error!("Setup refresh error: {e}");
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

/// Builds the GitHub Releases API URL: the latest release when `version` is
/// `None`, or a specific tag (`v{version}`) when pinned (#447). The leading
/// `v` is normalised so both `3.8.5` and `v3.8.5` resolve to the `v3.8.5` tag.
fn release_api_url(version: Option<&str>) -> String {
    match version {
        None => GITHUB_API_RELEASES.to_string(),
        Some(v) => {
            let core = v.trim_start_matches('v');
            format!("https://api.github.com/repos/yvgude/lean-ctx/releases/tags/v{core}")
        }
    }
}

/// Fetches release metadata from GitHub. `version = None` returns the latest
/// release; `Some(v)` returns the specific tagged release for version pinning.
fn fetch_release(version: Option<&str>) -> Result<serde_json::Value, String> {
    let response = ureq::get(&release_api_url(version))
        .header("User-Agent", &format!("lean-ctx/{CURRENT_VERSION}"))
        .header("Accept", "application/vnd.github.v3+json")
        .call()
        .map_err(|e| e.to_string())?;

    response
        .into_body()
        .read_to_string()
        .map_err(|e| e.to_string())
        .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
}

/// Extracts an explicit version argument from `update` args, if present.
/// Returns the first positional token (one that is not a `--flag`); the
/// `--schedule` subcommand is consumed earlier so it never reaches here.
fn parse_target_version(args: &[String]) -> Option<&str> {
    args.iter()
        .map(String::as_str)
        .find(|a| !a.starts_with('-'))
}

/// True if `s` looks like a release version (optionally `v`-prefixed, e.g.
/// `3.8.5` / `v3.8.5` / `3.8.5-rc1`), so typos are rejected before the API call.
fn looks_like_version(s: &str) -> bool {
    let core = s.strip_prefix('v').unwrap_or(s);
    core.contains('.')
        && core.starts_with(|c: char| c.is_ascii_digit())
        && core
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == '-' || c.is_ascii_alphabetic())
}

fn find_asset_url(release: &serde_json::Value, asset_name: &str) -> Option<String> {
    release["assets"]
        .as_array()?
        .iter()
        .find(|a| a["name"].as_str() == Some(asset_name))
        .and_then(|a| a["browser_download_url"].as_str())
        .map(std::string::ToString::to_string)
}

fn download_bytes(url: &str) -> Result<Vec<u8>, String> {
    let response = ureq::get(url)
        .header("User-Agent", &format!("lean-ctx/{CURRENT_VERSION}"))
        .call()
        .map_err(|e| e.to_string())?;

    let mut bytes = Vec::new();
    response
        .into_body()
        .into_reader()
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}

fn replace_binary(
    archive_bytes: &[u8],
    asset_name: &str,
    current_exe: &std::path::Path,
) -> Result<(), String> {
    let binary_bytes = if std::path::Path::new(asset_name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
    {
        extract_from_zip(archive_bytes)?
    } else {
        extract_from_tar_gz(archive_bytes)?
    };

    let tmp_path = current_exe.with_extension("tmp");
    std::fs::write(&tmp_path, &binary_bytes).map_err(|e| e.to_string())?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o755));
    }

    // On Windows, a running executable can be renamed but not overwritten.
    // Move the current binary out of the way first, then move the new one in.
    // If the file is locked (MCP server running), try stopping managed processes
    // first, then schedule a deferred update as last resort.
    #[cfg(windows)]
    {
        let old_path = current_exe.with_extension("old.exe");
        let _ = std::fs::remove_file(&old_path);

        match std::fs::rename(current_exe, &old_path) {
            Ok(()) => {
                if let Err(e) = std::fs::rename(&tmp_path, current_exe) {
                    let _ = std::fs::rename(&old_path, current_exe);
                    let _ = std::fs::remove_file(&tmp_path);
                    return Err(format!("Cannot place new binary: {e}"));
                }
                let _ = std::fs::remove_file(&old_path);
                return Ok(());
            }
            Err(_) => {
                // Binary is locked. Try to stop managed processes first.
                eprintln!("\nBinary is locked. Stopping managed lean-ctx processes...");
                stop_managed_windows_processes();

                // Brief wait for processes to release file handles.
                std::thread::sleep(std::time::Duration::from_millis(1500));

                // Retry after stopping.
                let _ = std::fs::remove_file(&old_path);
                match std::fs::rename(current_exe, &old_path) {
                    Ok(()) => {
                        if let Err(e) = std::fs::rename(&tmp_path, current_exe) {
                            let _ = std::fs::rename(&old_path, current_exe);
                            let _ = std::fs::remove_file(&tmp_path);
                            return Err(format!("Cannot place new binary: {e}"));
                        }
                        let _ = std::fs::remove_file(&old_path);
                        return Ok(());
                    }
                    Err(_) => {
                        // Still locked (likely MCP server held by editor).
                        print_blocking_processes(current_exe);
                        return deferred_windows_update(&tmp_path, current_exe);
                    }
                }
            }
        }
    }

    #[cfg(not(windows))]
    {
        // On macOS, rename-over-running-binary causes SIGKILL because the kernel
        // re-validates code pages against the (now different) on-disk file.
        // Unlinking first is safe: the kernel keeps the old memory-mapped pages
        // from the deleted inode, while the new file gets a fresh inode at the path.
        #[cfg(target_os = "macos")]
        {
            let _ = std::fs::remove_file(current_exe);
        }

        std::fs::rename(&tmp_path, current_exe).map_err(|e| {
            let _ = std::fs::remove_file(&tmp_path);
            format!("Cannot replace binary (permission denied?): {e}")
        })?;

        // #356: re-sign with the persistent identity when available so the
        // macOS TCC grant survives the update; ad-hoc fallback keeps it runnable.
        #[cfg(target_os = "macos")]
        {
            let _ = crate::core::codesign::sign_binary(current_exe);
        }

        Ok(())
    }
}

/// Try to stop managed lean-ctx processes (proxy, serve, daemon) on Windows
/// before attempting a deferred update.
#[cfg(windows)]
fn stop_managed_windows_processes() {
    // Try `lean-ctx stop` first — it's the cleanest shutdown path.
    let stop_result = std::process::Command::new("lean-ctx").arg("stop").output();

    match stop_result {
        Ok(out) if out.status.success() => {
            eprintln!("  Managed processes stopped.");
        }
        _ => {
            // Fallback: taskkill for known process types (proxy, serve).
            // MCP servers managed by editors can't be killed safely.
            for pattern in &["proxy start", "serve "] {
                let _ = std::process::Command::new("taskkill")
                    .args([
                        "/F",
                        "/FI",
                        &format!("WINDOWTITLE eq *{pattern}*"),
                        "/IM",
                        "lean-ctx.exe",
                    ])
                    .output();
            }
            eprintln!("  Attempted to stop lean-ctx processes via taskkill.");
        }
    }
}

/// Print which lean-ctx.exe processes are blocking the update on Windows.
#[cfg(windows)]
fn print_blocking_processes(target_exe: &std::path::Path) {
    let target_name = target_exe
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("lean-ctx.exe");

    let output = std::process::Command::new("tasklist")
        .args([
            "/FI",
            &format!("IMAGENAME eq {target_name}"),
            "/V",
            "/FO",
            "CSV",
        ])
        .output();

    if let Ok(out) = output {
        let stdout = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = stdout.lines().skip(1).collect(); // skip CSV header
        if !lines.is_empty() {
            eprintln!("\n  Blocking lean-ctx processes:");
            for line in &lines {
                // CSV: "Image Name","PID","Session Name","Session#","Mem Usage","Status","User Name","CPU Time","Window Title"
                let fields: Vec<&str> = line.split(',').collect();
                if fields.len() >= 2 {
                    let pid = fields[1].trim_matches('"');
                    eprintln!("    PID {pid}");
                }
            }
            eprintln!("\n  To stop manually: taskkill /F /PID <pid>  (or close your editor)");
        }
    }
}

/// On Windows, when the binary is locked by an MCP server, we can't rename it.
/// Instead, stage the new binary and spawn a background cmd process that waits
/// for the lock to be released (with a timeout), then performs the swap.
#[cfg(windows)]
fn deferred_windows_update(
    staged_path: &std::path::Path,
    target_exe: &std::path::Path,
) -> Result<(), String> {
    let pending_path = target_exe.with_file_name("lean-ctx-pending.exe");
    std::fs::rename(staged_path, &pending_path).map_err(|e| {
        let _ = std::fs::remove_file(staged_path);
        format!("Cannot stage update: {e}")
    })?;

    let target_str = target_exe.display().to_string();
    let pending_str = pending_path.display().to_string();
    let old_str = target_exe.with_extension("old.exe").display().to_string();
    let max_retries = 60;

    let script = generate_deferred_bat_script(&target_str, &pending_str, &old_str, max_retries);

    let script_path = target_exe.with_file_name("lean-ctx-update.bat");
    std::fs::write(&script_path, &script)
        .map_err(|e| format!("Cannot write update script: {e}"))?;

    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "/MIN", &script_path.display().to_string()])
        .spawn();

    println!("\nThe binary is still in use (likely by your editor's MCP server).");
    println!("A background update has been scheduled (timeout: {max_retries}s).");
    println!("Close your editor and the update will complete automatically.");
    println!("\nIf it times out, run: lean-ctx update");
    println!("Update script: {}", script_path.display());

    Ok(())
}

fn extract_from_tar_gz(data: &[u8]) -> Result<Vec<u8>, String> {
    use flate2::read::GzDecoder;

    let gz = GzDecoder::new(data);
    let mut archive = tar::Archive::new(gz);

    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?;
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

        if name == "lean-ctx" || name == "lean-ctx.exe" {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
            return Ok(bytes);
        }
    }
    Err("lean-ctx binary not found inside archive".to_string())
}

fn extract_from_zip(data: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Cursor;

    let cursor = Cursor::new(data);
    let mut zip = zip::ZipArchive::new(cursor).map_err(|e| e.to_string())?;

    for i in 0..zip.len() {
        let mut file = zip.by_index(i).map_err(|e| e.to_string())?;
        let name = file.name().to_string();
        if name == "lean-ctx.exe" || name == "lean-ctx" {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
            return Ok(bytes);
        }
    }
    Err("lean-ctx binary not found inside zip archive".to_string())
}

/// Generate the deferred update batch script content (extracted for testability).
#[cfg(any(windows, test))]
fn generate_deferred_bat_script(
    target: &str,
    pending: &str,
    old: &str,
    max_retries: u32,
) -> String {
    format!(
        r#"@echo off
setlocal
set "RETRIES=0"
set "MAX_RETRIES={max_retries}"

echo lean-ctx update: waiting for binary to be released (timeout: %MAX_RETRIES%s)...
echo.
echo Blocking processes:
tasklist /FI "IMAGENAME eq lean-ctx.exe" /V /NH 2>nul
echo.
echo Close your editor (Cursor, VS Code, etc.) to release the binary,
echo or stop manually:  lean-ctx stop
echo.

:retry
if %RETRIES% GEQ %MAX_RETRIES% goto timeout
set /a RETRIES+=1
timeout /t 1 /nobreak >nul
move /Y "{target}" "{old}" >nul 2>&1
if errorlevel 1 (
    if %RETRIES% EQU 10 echo   Still waiting... (%RETRIES%/%MAX_RETRIES%s)
    if %RETRIES% EQU 30 echo   Still waiting... (%RETRIES%/%MAX_RETRIES%s) — try closing your editor
    if %RETRIES% EQU 50 echo   Still waiting... (%RETRIES%/%MAX_RETRIES%s) — timeout approaching
    goto retry
)

move /Y "{pending}" "{target}" >nul 2>&1
if errorlevel 1 (
    move /Y "{old}" "{target}" >nul 2>&1
    echo.
    echo Update failed: could not place new binary.
    echo Please close all editors and run: lean-ctx update
    pause
    exit /b 1
)
del /f "{old}" >nul 2>&1
echo.
echo Updated successfully!
goto cleanup

:timeout
echo.
echo Update timed out after %MAX_RETRIES% seconds.
echo The new binary is staged at: {pending}
echo.
echo To complete the update manually:
echo   1. Close your editor (Cursor, VS Code, etc.)
echo   2. Run: move /Y "{pending}" "{target}"
echo.
echo Or run: lean-ctx update --force
echo.
pause
exit /b 1

:cleanup
del "%~f0" >nul 2>&1
"#
    )
}

fn detect_linux_libc() -> &'static str {
    let output = std::process::Command::new("ldd").arg("--version").output();
    if let Ok(out) = output {
        let text = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let combined = format!("{text}{stderr}");
        for line in combined.lines() {
            if let Some(ver) = line.split_whitespace().last() {
                let parts: Vec<&str> = ver.split('.').collect();
                if parts.len() == 2
                    && let (Ok(major), Ok(minor)) =
                        (parts[0].parse::<u32>(), parts[1].parse::<u32>())
                {
                    if major > 2 || (major == 2 && minor >= 35) {
                        return "gnu";
                    }
                    return "musl";
                }
            }
        }
    }
    "musl"
}

fn platform_asset_name() -> String {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    let target = match (os, arch) {
        ("macos", "aarch64") => "aarch64-apple-darwin".to_string(),
        ("macos", "x86_64") => "x86_64-apple-darwin".to_string(),
        ("linux", "x86_64") => format!("x86_64-unknown-linux-{}", detect_linux_libc()),
        ("linux", "aarch64") => format!("aarch64-unknown-linux-{}", detect_linux_libc()),
        ("windows", "x86_64") => "x86_64-pc-windows-msvc".to_string(),
        _ => {
            tracing::error!(
                "Unsupported platform: {os}/{arch}. Download manually from \
                https://github.com/yvgude/lean-ctx/releases/latest"
            );
            std::process::exit(1);
        }
    };

    if os == "windows" {
        format!("lean-ctx-{target}.zip")
    } else {
        format!("lean-ctx-{target}.tar.gz")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_update_disabled_skips_and_cleans_up() {
        // #335: auto_update=false → scheduled run must not install.
        assert_eq!(automatic_update_gate(false, false), AutoUpdateGate::Skip);
        // auto_update=false wins even if notify_only is also set.
        assert_eq!(automatic_update_gate(false, true), AutoUpdateGate::Skip);
    }

    #[test]
    fn notify_only_downgrades_to_check() {
        assert_eq!(
            automatic_update_gate(true, true),
            AutoUpdateGate::NotifyOnly
        );
    }

    #[test]
    fn auto_update_enabled_proceeds() {
        assert_eq!(automatic_update_gate(true, false), AutoUpdateGate::Proceed);
    }

    #[test]
    fn bat_script_has_timeout_guard() {
        let script = generate_deferred_bat_script(
            r"C:\bin\lean-ctx.exe",
            r"C:\bin\lean-ctx-pending.exe",
            r"C:\bin\lean-ctx.old.exe",
            60,
        );
        assert!(script.contains("set \"MAX_RETRIES=60\""));
        assert!(script.contains(":timeout"), "must have timeout label");
        assert!(
            script.contains("timed out after"),
            "must show timeout message"
        );
    }

    #[test]
    fn bat_script_shows_blocking_processes() {
        let script = generate_deferred_bat_script("t", "p", "o", 30);
        assert!(script.contains("tasklist"), "must list blocking processes");
        assert!(
            script.contains("lean-ctx stop"),
            "must suggest lean-ctx stop"
        );
    }

    #[test]
    fn bat_script_has_progress_indicators() {
        let script = generate_deferred_bat_script("t", "p", "o", 60);
        assert!(script.contains("Still waiting"));
        assert!(script.contains("RETRIES"));
    }

    #[test]
    fn bat_script_provides_manual_recovery() {
        let script = generate_deferred_bat_script(
            r"C:\bin\lean-ctx.exe",
            r"C:\bin\lean-ctx-pending.exe",
            r"C:\bin\lean-ctx.old.exe",
            60,
        );
        assert!(script.contains(r"move /Y"));
        assert!(
            script.contains("lean-ctx-pending.exe"),
            "must show where the pending binary is"
        );
        assert!(
            script.contains("lean-ctx update"),
            "must suggest re-running update"
        );
    }

    #[test]
    fn bat_script_no_infinite_loop() {
        let script = generate_deferred_bat_script("t", "p", "o", 10);
        assert!(script.contains("if %RETRIES% GEQ %MAX_RETRIES% goto timeout"));
        assert!(
            !script.contains(":retry\ntimeout"),
            "must not be an infinite loop"
        );
    }

    #[test]
    fn release_url_latest_when_no_version() {
        // #447: no pin → the canonical "latest" endpoint.
        assert_eq!(release_api_url(None), GITHUB_API_RELEASES);
    }

    #[test]
    fn release_url_pins_specific_tag() {
        // #447: a bare version pins the `v`-prefixed tag …
        assert_eq!(
            release_api_url(Some("3.8.5")),
            "https://api.github.com/repos/yvgude/lean-ctx/releases/tags/v3.8.5"
        );
        // … and an already-`v`-prefixed version is normalised, not doubled.
        assert_eq!(
            release_api_url(Some("v3.8.5")),
            "https://api.github.com/repos/yvgude/lean-ctx/releases/tags/v3.8.5"
        );
    }

    #[test]
    fn parse_target_version_peels_positional_only() {
        let flags_only = [String::from("--check"), String::from("--quiet")];
        assert_eq!(parse_target_version(&flags_only), None);

        let with_version = [String::from("3.8.5"), String::from("--check")];
        assert_eq!(parse_target_version(&with_version), Some("3.8.5"));

        // Order-independent: the positional is found after leading flags.
        let flag_then_version = [String::from("--insecure"), String::from("v3.8.5")];
        assert_eq!(parse_target_version(&flag_then_version), Some("v3.8.5"));
    }

    #[test]
    fn looks_like_version_accepts_releases_rejects_typos() {
        assert!(looks_like_version("3.8.5"));
        assert!(looks_like_version("v3.8.5"));
        assert!(looks_like_version("3.8.5-rc1"));
        // Not versions: flags, words, and bare majors (too ambiguous to pin).
        assert!(!looks_like_version("--check"));
        assert!(!looks_like_version("latest"));
        assert!(!looks_like_version("3"));
    }
}
