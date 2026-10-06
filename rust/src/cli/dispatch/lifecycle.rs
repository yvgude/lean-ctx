// Auto-split from the former monolithic dispatch.rs. run() (the command
// match) stays in mod.rs; standalone helpers grouped by concern.

use crate::core;

pub(super) fn cmd_stop() {
    use crate::daemon;
    use crate::ipc;

    eprintln!("Stopping all lean-ctx processes…");

    crate::proxy_autostart::stop();
    crate::daemon_autostart::stop();
    if cfg!(target_os = "macos") {
        eprintln!("  Unloaded autostart (LaunchAgent).");
    } else if cfg!(target_os = "linux") {
        eprintln!("  Unloaded autostart (systemd).");
    }

    // 2. Stop daemon via IPC
    if let Err(e) = daemon::stop_daemon() {
        eprintln!("  Warning: daemon stop: {e}");
    }

    // 3. SIGTERM all remaining lean-ctx processes
    let killed = ipc::process::kill_all_by_name("lean-ctx");
    if killed > 0 {
        eprintln!("  Sent SIGTERM to {killed} process(es).");
    }

    std::thread::sleep(std::time::Duration::from_millis(500));

    // 4. Force-kill stragglers (but never MCP servers — IDE will respawn them)
    let remaining = ipc::process::find_killable_pids("lean-ctx");
    if !remaining.is_empty() {
        eprintln!("  Force-killing {} stubborn process(es)…", remaining.len());
        for &pid in &remaining {
            let _ = ipc::process::force_kill(pid);
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }

    daemon::cleanup_daemon_files();

    let final_check = ipc::process::find_killable_pids("lean-ctx");
    if final_check.is_empty() {
        eprintln!("  ✓ All lean-ctx processes stopped.");
    } else {
        eprintln!(
            "  ✗ {} process(es) could not be killed: {:?}",
            final_check.len(),
            final_check
        );
        #[cfg(unix)]
        eprintln!(
            "    Try: sudo kill -9 {}",
            final_check
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>()
                .join(" ")
        );
        #[cfg(windows)]
        eprintln!(
            "    Try: taskkill /F /PID {}",
            final_check
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>()
                .join(" /PID ")
        );
        std::process::exit(1);
    }
}

pub(super) fn cmd_restart() {
    use crate::daemon;
    use crate::ipc;

    eprintln!("Restarting lean-ctx…");

    crate::proxy_autostart::stop();
    crate::daemon_autostart::stop();

    if let Err(e) = daemon::stop_daemon() {
        eprintln!("  Warning: daemon stop: {e}");
    }

    let orphans = ipc::process::kill_all_by_name("lean-ctx");
    if orphans > 0 {
        eprintln!("  Terminated {orphans} orphan process(es).");
    }

    std::thread::sleep(std::time::Duration::from_millis(500));

    let remaining = ipc::process::find_killable_pids("lean-ctx");
    if !remaining.is_empty() {
        eprintln!(
            "  Force-killing {} stubborn process(es): {:?}",
            remaining.len(),
            remaining
        );
        for &pid in &remaining {
            let _ = ipc::process::force_kill(pid);
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }

    daemon::cleanup_daemon_files();

    crate::proxy_autostart::start();

    if crate::daemon_autostart::is_installed() {
        crate::daemon_autostart::start();
        eprintln!("  ✓ Daemon restarted via autostart.");
    } else {
        match daemon::start_daemon(&[]) {
            Ok(()) => eprintln!("  ✓ Daemon restarted."),
            Err(e) => {
                eprintln!("  ✗ Daemon start failed: {e}");
                std::process::exit(1);
            }
        }
    }
}

pub(super) fn cmd_dev_install() {
    use crate::ipc;

    let cargo_root = find_cargo_project_root();
    let Some(cargo_root) = cargo_root else {
        eprintln!("Error: No Cargo.toml found. Run from the lean-ctx project directory.");
        std::process::exit(1);
    };

    // `dev-install` builds from source (contributor workflow). Set expectations
    // up front so the multi-minute cargo build is never mistaken for a hang, and
    // point end-users at the fast binary self-updater instead.
    eprintln!("\x1b[1m◆ lean-ctx dev-install\x1b[0m  \x1b[2m(builds from source)\x1b[0m");
    eprintln!(
        "  \x1b[2mCompiles the binary from source — this can take several minutes the\n  \
         first time while cargo fetches and builds the dependency tree. The live\n  \
         build output below is normal progress, not a hang.\x1b[0m"
    );
    eprintln!(
        "  \x1b[2mJust want the latest release? Run\x1b[0m \x1b[1mlean-ctx update\x1b[0m \x1b[2m— it \
         downloads a prebuilt\n  binary in seconds, no toolchain required.\x1b[0m"
    );
    eprintln!();
    eprintln!("\x1b[2m→ cargo build --release\x1b[0m");
    let build = std::process::Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(&cargo_root)
        .status();

    match build {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!("  Build failed with exit code {}", s.code().unwrap_or(-1));
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("  Build failed: {e}");
            std::process::exit(1);
        }
    }

    let target_dir = resolve_cargo_target_dir(&cargo_root);
    let built_binary = target_dir
        .join("release")
        .join(format!("lean-ctx{}", std::env::consts::EXE_SUFFIX));
    if !built_binary.exists() {
        eprintln!(
            "  Error: Built binary not found at {}",
            built_binary.display()
        );
        eprintln!(
            "  Hint: is CARGO_TARGET_DIR or a [build] target-dir override pointing elsewhere?"
        );
        std::process::exit(1);
    }

    let install_path = resolve_install_path(&target_dir);
    eprintln!("Installing to {}…", install_path.display());

    eprintln!("  Stopping all lean-ctx processes…");
    crate::proxy_autostart::stop();
    crate::daemon_autostart::stop();
    let _ = crate::daemon::stop_daemon();
    ipc::process::kill_all_by_name("lean-ctx");
    std::thread::sleep(std::time::Duration::from_millis(500));

    // #1036: force-kill the SAME MCP-safe set as `cmd_stop` (`find_killable_pids`),
    // never the raw `find_pids_by_name`. The latter includes the IDE-owned MCP
    // stdio server (bare `lean-ctx`); SIGKILLing it drops the editor's MCP
    // connection for minutes (until the IDE respawns it) — the binary the IDE
    // respawns is the freshly installed one anyway, so killing it only hurts.
    let remaining = ipc::process::find_killable_pids("lean-ctx");
    if !remaining.is_empty() {
        eprintln!("  Force-killing {} stubborn process(es)…", remaining.len());
        for &pid in &remaining {
            let _ = ipc::process::force_kill(pid);
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    if let Err(e) = atomic_install_binary(&built_binary, &install_path) {
        eprintln!("  Error: {e}");
        std::process::exit(1);
    }
    eprintln!("  ✓ Binary installed.");

    // #356: a fresh ad-hoc cdhash voids the macOS TCC grant on every build.
    // Point users at the one-time fix so the Documents prompt stops returning.
    #[cfg(target_os = "macos")]
    if !crate::core::codesign::is_ready() {
        eprintln!(
            "  ⚠ macOS: run `lean-ctx codesign-setup` once to stop the recurring\n    \
             \"lean-ctx wants to access your Documents\" prompt after updates (#356)."
        );
    }

    // Kill binary drift: repoint any stale Homebrew shim at the fresh binary (#559).
    reconcile_binary_drift(&install_path);

    // Verify under a hard timeout — a broken/hanging binary must never wedge
    // the install (which previously left users having to reboot).
    let mut verify = std::process::Command::new(&install_path);
    verify.arg("--version");
    let version = ipc::process::run_with_timeout(verify, std::time::Duration::from_secs(10))
        .filter(|o| o.status.success())
        .map_or_else(
            || "unknown (version check timed out)".to_string(),
            |o| String::from_utf8_lossy(&o.stdout).trim().to_string(),
        );

    eprintln!("  ✓ dev-install complete: {version}");

    eprintln!("  Re-enabling autostart…");
    // #356: re-install (not just bootstrap) so the LaunchAgent plists are
    // regenerated with the current deny-~/Documents seatbelt wrapper — a plain
    // restart would keep the previous, unwrapped plist.
    if crate::proxy_autostart::is_installed() {
        crate::proxy_autostart::install(crate::proxy_setup::default_port(), true);
    }

    if crate::daemon_autostart::is_installed() {
        crate::daemon_autostart::install(true);
        eprintln!("  ✓ Daemon restarted via autostart.");
    } else {
        eprintln!("  Starting daemon…");
        match crate::daemon::start_daemon(&[]) {
            Ok(()) => {}
            Err(e) => eprintln!("  Warning: daemon start: {e} (will be started by editor)"),
        }
    }

    // #828: enable shadow_mode for users who never set it.
    crate::core::updater::migrate_shadow_mode_default_public();

    // Resync agent rules after install so a RULES_VERSION bump is propagated
    // without requiring a separate `lean-ctx setup` or `init` call.
    let cfg = crate::core::config::Config::load();
    if cfg.setup.should_inject_rules()
        && let Some(home) = dirs::home_dir()
    {
        let result = crate::rules_inject::inject_all_rules(&home);
        if !result.updated.is_empty() {
            eprintln!("  ✓ Rules updated: {}", result.updated.join(", "));
        }
    }
}

/// One-time setup of the persistent macOS code-signing identity (#356).
///
/// Stops the "lean-ctx wants to access your Documents folder" prompt from
/// returning after every update: ad-hoc signatures change the binary's cdhash
/// each build, voiding the TCC grant; a stable identity keeps it.
#[cfg(target_os = "macos")]
pub(super) fn cmd_codesign_setup() {
    use crate::core::codesign::{SetupOutcome, setup_identity, sign_binary};

    eprintln!("Setting up a stable code-signing identity for lean-ctx (#356)…");
    eprintln!(
        "  This stops the recurring macOS \"access to your Documents folder\" prompt.\n  \
         macOS will ask ONCE to authorize the trust setting — confirm with Touch ID\n  \
         or your login password.\n"
    );

    match setup_identity() {
        Ok(SetupOutcome::AlreadyReady) => {
            eprintln!("  ✓ Identity already set up and trusted. Nothing to do.");
        }
        Ok(SetupOutcome::Created) => {
            eprintln!("  ✓ Signing identity created and trusted.");
            // Re-sign the installed binary now so this grant applies immediately.
            if let Ok(exe) = std::env::current_exe()
                && sign_binary(&exe) == crate::core::codesign::SignKind::Stable
            {
                eprintln!("  ✓ Re-signed {} with the stable identity.", exe.display());
            }
            eprintln!(
                "\n  Done. `dev-install` and self-updates now reuse this identity.\n  \
                 Click \"Allow\" on the next Documents prompt — it won't come back."
            );
        }
        Err(e) => {
            eprintln!("  ✗ Setup failed: {e}");
            eprintln!(
                "  The binary still works (ad-hoc signed); the prompt may recur until\n  \
                 setup succeeds. Re-run `lean-ctx codesign-setup` to retry."
            );
            std::process::exit(1);
        }
    }
}

/// Non-macOS stub: the persistent identity only matters for macOS TCC.
#[cfg(not(target_os = "macos"))]
pub(super) fn cmd_codesign_setup() {
    eprintln!("codesign-setup is only needed on macOS.");
}

/// Atomically install `src` to `dst`, staging through a temp file in the same
/// directory so readers never observe a half-written binary.
///
/// On macOS the destination inode is unlinked first: running processes keep
/// their already-mapped pages from the deleted inode, while the fresh file lands
/// at the path with a new inode. Overwriting a running Mach-O in place (e.g.
/// plain `cp`) instead triggers an `ETXTBSY`/SIGKILL crash-loop — the root cause
/// of the "everything hangs after a binary update" reboots. The new binary is
/// re-codesigned (persistent identity when set up, else ad-hoc) so Gatekeeper
/// accepts it and the macOS TCC grant survives the update (#356).
///
/// On Windows a running process's image cannot be replaced in place: the
/// IDE-owned MCP stdio server (deliberately never killed, #1036) holds the old
/// binary open for the whole session, so a bare rename fails with
/// `ACCESS_DENIED` no matter the retry budget (GH #691 measured 60s of retries
/// failing identically). Renaming the running image ASIDE is allowed though —
/// the rustup/self-replace swap: move `dst` → `dst.old` (the running process
/// keeps executing the renamed file), then move the staged binary into place.
/// The `.old` sidecar is cleaned up best-effort on the next install once its
/// holder exits.
fn atomic_install_binary(src: &std::path::Path, dst: &std::path::Path) -> Result<(), String> {
    let staged = dst.with_extension("new");
    let _ = std::fs::remove_file(&staged);
    std::fs::copy(src, &staged).map_err(|e| format!("staging copy failed: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod failed: {e}"))?;
    }

    #[cfg(target_os = "macos")]
    let _ = std::fs::remove_file(dst);

    #[cfg(windows)]
    move_locked_destination_aside(dst);

    if let Err(e) = std::fs::rename(&staged, dst) {
        let _ = std::fs::remove_file(&staged);
        let hint = if cfg!(windows) {
            "\n  Another process still has the old binary open and its lock even blocks\n  \
             the rename-aside swap (rare — usually AV/EDR or a debugger, not the MCP\n  \
             server). Disconnect MCP clients (e.g. `/mcp` in Claude Code), wait a\n  \
             moment, and re-run the install."
        } else {
            ""
        };
        return Err(format!("atomic rename failed: {e}{hint}"));
    }

    // #356: prefer the persistent identity (stable cdhash anchor → TCC grant
    // survives updates); ad-hoc fallback keeps the binary launchable regardless.
    #[cfg(target_os = "macos")]
    {
        let _ = crate::core::codesign::sign_binary(dst);
    }

    Ok(())
}

/// Windows half of the rustup-style swap (GH #691): clear a leftover `.old`
/// sidecar from a previous install (succeeds once its holder exited), then
/// rename the current — possibly still-executing — binary onto the sidecar
/// name so the destination path is free for the fresh binary. Best-effort by
/// design: when nothing holds `dst`, the plain rename in the caller works
/// even if this did nothing.
#[cfg(windows)]
fn move_locked_destination_aside(dst: &std::path::Path) {
    // Same sidecar convention as the self-updater (`updater.rs::replace_binary`).
    let old = dst.with_extension("old.exe");
    let _ = std::fs::remove_file(&old);
    if dst.exists() && !old.exists() {
        let _ = std::fs::rename(dst, &old);
    }
}

/// Resolve cargo's real target directory for the project at `cargo_root`.
///
/// A hardcoded `target/` breaks setups that redirect the target dir via
/// `CARGO_TARGET_DIR` or a `~/.cargo/config.toml` `[build] target-dir`
/// override (e.g. one shared build cache across worktrees, as recommended in
/// CONTRIBUTING.md) — dev-install then silently installed a stale or missing
/// binary (#671). `cargo metadata` is the canonical answer: it folds in env,
/// config files and workspace settings. Runs under a hard timeout (cargo may
/// touch the network lock) and falls back to `<root>/target` on any failure.
fn resolve_cargo_target_dir(cargo_root: &std::path::Path) -> std::path::PathBuf {
    // Prefer the explicit override: it is unambiguous and avoids asking Cargo
    // for a potentially large metadata document just to recover one path.
    if let Some(configured) = std::env::var_os("CARGO_TARGET_DIR") {
        let path = std::path::PathBuf::from(configured);
        return if path.is_absolute() {
            path
        } else {
            cargo_root.join(path)
        };
    }

    // `cargo metadata` can emit more than a pipe buffer for a workspace. The
    // generic process helper intentionally reads only after exit and therefore
    // can deadlock on that output; drain stdout on a reader thread while the
    // child runs, preserving the hard timeout without truncating metadata.
    let mut cmd = std::process::Command::new("cargo");
    cmd.args(["metadata", "--no-deps", "--format-version=1"])
        .current_dir(cargo_root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let Ok(mut child) = cmd.spawn() else {
        return cargo_root.join("target");
    };
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return cargo_root.join("target");
    };
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut output = Vec::new();
        let _ = stdout.read_to_end(&mut output);
        output
    });
    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if start.elapsed() >= std::time::Duration::from_secs(15) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(_) => break None,
        }
    };
    let metadata = reader.join().ok().and_then(|output| {
        status
            .filter(std::process::ExitStatus::success)
            .and_then(|_| target_dir_from_metadata(&String::from_utf8_lossy(&output)))
    });
    metadata.unwrap_or_else(|| cargo_root.join("target"))
}

/// Extract `target_directory` from `cargo metadata` JSON. Split out from
/// [`resolve_cargo_target_dir`] so the parsing is unit-testable without
/// invoking cargo.
fn target_dir_from_metadata(metadata_json: &str) -> Option<std::path::PathBuf> {
    let value: serde_json::Value = serde_json::from_str(metadata_json).ok()?;
    value
        .get("target_directory")?
        .as_str()
        .map(std::path::PathBuf::from)
}

pub(super) fn find_cargo_project_root() -> Option<std::path::PathBuf> {
    let current_dir = std::env::current_dir().ok()?;
    find_cargo_project_root_from(&current_dir)
}

/// Locate the Rust crate from either its own directory or the repository root.
///
/// The lean-ctx repository keeps its Rust crate in `rust/`; accepting that
/// layout makes `lean-ctx dev-install` work from the documented repository root
/// as well as from `rust/` and any nested project directory.
fn find_cargo_project_root_from(start: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        if dir.join("Cargo.toml").is_file() {
            return Some(dir);
        }
        let nested_crate = dir.join("rust");
        if nested_crate.join("Cargo.toml").is_file() {
            return Some(nested_crate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

pub(super) fn resolve_install_path(build_dir: &std::path::Path) -> std::path::PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Ok(canonical) = exe.canonicalize()
        && !is_build_artifact(&canonical, build_dir)
    {
        return canonical;
    }

    if let Ok(home) = std::env::var("HOME") {
        let local_bin = std::path::PathBuf::from(&home).join(".local/bin/lean-ctx");
        if local_bin.parent().is_some_and(std::path::Path::exists) {
            return local_bin;
        }
    }

    std::path::PathBuf::from("/usr/local/bin/lean-ctx")
}

/// A running binary inside the build output is never the install target:
/// dev-install would copy the fresh build onto itself. Besides the literal
/// `target` component, the resolved build dir covers a relocated
/// `[build] target-dir` (e.g. a machine-wide `~/.cargo/shared-target`).
fn is_build_artifact(exe: &std::path::Path, build_dir: &std::path::Path) -> bool {
    let build_dir = build_dir
        .canonicalize()
        .unwrap_or_else(|_| build_dir.to_path_buf());
    exe.starts_with(&build_dir) || exe.components().any(|c| c.as_os_str() == "target")
}

/// Returns true if a symlink target points into a Homebrew Cellar / linuxbrew
/// store — i.e. a `brew`-managed shim that can go stale and shadow the
/// dev-installed binary on PATH (#559). Unix-only: Homebrew shims do not exist
/// on Windows, where `reconcile_binary_drift` is a no-op.
#[cfg(unix)]
fn is_homebrew_cellar_link(target: &std::path::Path) -> bool {
    let s = target.to_string_lossy();
    s.contains("/Cellar/") || s.contains("/linuxbrew/")
}

/// Eliminate binary drift after a dev-install (#559).
///
/// A stale Homebrew shim (e.g. `/opt/homebrew/bin/lean-ctx ->
/// ../Cellar/lean-ctx/<old>/bin/lean-ctx`) silently shadows the freshly built
/// `~/.local/bin/lean-ctx` on PATH, so the daemon and the CLI can end up running
/// *different* builds (observed md5 drift in #559). Repoint any such shim at the
/// just-installed binary, and warn about any other PATH entry that still
/// resolves before it.
fn reconcile_binary_drift(install_path: &std::path::Path) {
    #[cfg(unix)]
    {
        let install_canon =
            std::fs::canonicalize(install_path).unwrap_or_else(|_| install_path.to_path_buf());

        for shim in [
            "/opt/homebrew/bin/lean-ctx",
            "/usr/local/bin/lean-ctx",
            "/home/linuxbrew/.linuxbrew/bin/lean-ctx",
        ] {
            let shim_path = std::path::Path::new(shim);
            // Only act on symlinks; a real file here is the install target itself.
            let Ok(target) = std::fs::read_link(shim_path) else {
                continue;
            };
            if !is_homebrew_cellar_link(&target) {
                continue;
            }
            // Already resolves to the fresh binary? Nothing to do.
            if std::fs::canonicalize(shim_path).is_ok_and(|c| c == install_canon) {
                continue;
            }
            // Atomically repoint: drop the stale link, recreate it at the fresh binary.
            let _ = std::fs::remove_file(shim_path);
            match std::os::unix::fs::symlink(install_path, shim_path) {
                Ok(()) => eprintln!(
                    "  ✓ Repointed stale Homebrew shim {shim} → {} (#559 drift fix)",
                    install_path.display()
                ),
                Err(e) => eprintln!(
                    "  ⚠ Stale Homebrew shim {shim} → {} couldn't be repointed ({e}). \
                     Run: brew unlink lean-ctx",
                    target.display()
                ),
            }
        }

        // Warn if a *different* lean-ctx still resolves before our install dir on PATH.
        if let Ok(path_var) = std::env::var("PATH") {
            for dir in std::env::split_paths(&path_var) {
                let cand = dir.join("lean-ctx");
                if !cand.exists() {
                    continue;
                }
                let cand_canon = std::fs::canonicalize(&cand).unwrap_or_else(|_| cand.clone());
                if cand_canon == install_canon {
                    break; // our binary wins on PATH — good
                }
                eprintln!(
                    "  ⚠ PATH shadow: {} resolves before {} — plain `lean-ctx` may run an older build.",
                    cand.display(),
                    install_path.display()
                );
                break;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = install_path;
    }
}

/// Whether this process, rather than a managed LaunchAgent/systemd unit, owns startup.
fn should_spawn_proxy_locally(
    proxy_enabled: bool,
    managed_service_loaded: bool,
    already_running: bool,
) -> bool {
    proxy_enabled && !managed_service_loaded && !already_running
}

pub(super) fn spawn_proxy_if_needed() {
    use std::net::TcpStream;

    let cfg = core::config::Config::load();
    if cfg.proxy_enabled != Some(true) {
        return;
    }

    let managed_service_loaded = crate::proxy_autostart::is_loaded();
    let port = crate::proxy_setup::default_port();
    let already_running = {
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
        TcpStream::connect_timeout(&addr, crate::proxy_setup::proxy_timeout()).is_ok()
    };

    if !should_spawn_proxy_locally(
        cfg.proxy_enabled == Some(true),
        managed_service_loaded,
        already_running,
    ) {
        if managed_service_loaded {
            tracing::debug!("managed proxy service owns startup on port {port}");
        } else if already_running {
            tracing::debug!("proxy already running on port {port}");
        }
        return;
    }

    let binary = core::portable_binary::resolve_portable_binary();

    let mut cmd = std::process::Command::new(&binary);
    cmd.args(["proxy", "start", &format!("--port={port}")])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Detached spawn: on Windows the proxy must escape the MCP process's
    // console/Job or it dies when the AI client recycles the MCP server.
    match crate::ipc::process::spawn_detached(&mut cmd) {
        Ok(_) => tracing::info!("auto-started proxy on port {port}"),
        Err(e) => tracing::debug!("could not auto-start proxy: {e}"),
    }
}

#[cfg(test)]
mod target_dir_tests {
    use super::{resolve_cargo_target_dir, should_spawn_proxy_locally, target_dir_from_metadata};
    use std::path::{Path, PathBuf};

    #[test]
    fn managed_proxy_service_prevents_a_duplicate_local_spawn() {
        assert!(should_spawn_proxy_locally(true, false, false));
        assert!(!should_spawn_proxy_locally(false, false, false));
        assert!(!should_spawn_proxy_locally(true, true, false));
        assert!(!should_spawn_proxy_locally(true, false, true));
    }
    #[test]
    fn extracts_target_directory_from_metadata_json() {
        let json = r#"{"packages":[],"target_directory":"/shared/build/target","version":1}"#;
        assert_eq!(
            target_dir_from_metadata(json),
            Some(PathBuf::from("/shared/build/target"))
        );
    }

    #[test]
    fn windows_paths_survive_json_unescaping() {
        // serde_json decodes the escaped backslashes; no manual munging needed.
        let json = r#"{"target_directory":"C:\\Users\\dev\\shared\\target"}"#;
        assert_eq!(
            target_dir_from_metadata(json),
            Some(PathBuf::from(r"C:\Users\dev\shared\target"))
        );
    }

    #[test]
    fn malformed_or_incomplete_metadata_yields_none() {
        assert_eq!(target_dir_from_metadata("not json"), None);
        assert_eq!(target_dir_from_metadata("{}"), None);
        assert_eq!(target_dir_from_metadata(r#"{"target_directory":42}"#), None);
    }

    #[test]
    fn resolve_falls_back_to_root_target_without_manifest() {
        // No Cargo.toml at / — `cargo metadata` fails, the fallback must kick in.
        let _env = crate::core::data_dir::test_env_lock();
        let previous_target_dir = std::env::var_os("CARGO_TARGET_DIR");
        crate::test_env::remove_var("CARGO_TARGET_DIR");
        let root = if cfg!(windows) { r"C:\" } else { "/" };
        let resolved = resolve_cargo_target_dir(Path::new(root));
        match previous_target_dir {
            Some(value) => crate::test_env::set_var("CARGO_TARGET_DIR", value),
            None => crate::test_env::remove_var("CARGO_TARGET_DIR"),
        }
        assert_eq!(resolved, Path::new(root).join("target"));
    }

    #[test]
    fn relocated_build_dir_is_never_an_install_target() {
        let shared = Path::new("/home/dev/.cargo/shared-target");
        assert!(super::is_build_artifact(
            &shared.join("release/lean-ctx"),
            shared
        ));
        assert!(super::is_build_artifact(
            Path::new("/src/lean-ctx/rust/target/release/lean-ctx"),
            shared
        ));
        assert!(!super::is_build_artifact(
            Path::new("/home/dev/.local/bin/lean-ctx"),
            shared
        ));
    }

    #[test]
    fn running_build_output_is_not_chosen_as_install_path() {
        // Test binaries always live in the resolved target dir, relocated or not.
        let _env = crate::core::data_dir::test_env_lock();
        let build_dir = resolve_cargo_target_dir(Path::new(env!("CARGO_MANIFEST_DIR")));
        let exe = std::env::current_exe().unwrap().canonicalize().unwrap();
        assert!(
            super::is_build_artifact(&exe, &build_dir),
            "{} not under {}",
            exe.display(),
            build_dir.display()
        );
        assert_ne!(super::resolve_install_path(&build_dir), exe);
    }
}

#[cfg(all(test, windows))]
mod windows_swap_tests {
    use super::atomic_install_binary;
    use std::fs;
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_DELETE: u32 = 0x4;

    /// A held-open destination that still permits renames (the sharing shape a
    /// running image effectively presents, GH #691): the swap moves it aside
    /// and installs the fresh binary at the path.
    #[test]
    fn swap_installs_over_destination_held_open_with_share_delete() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("built.exe");
        let dst = dir.path().join("lean-ctx.exe");
        fs::write(&src, b"new-binary").unwrap();
        fs::write(&dst, b"old-binary").unwrap();

        let _holder = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
            .open(&dst)
            .unwrap();

        atomic_install_binary(&src, &dst).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"new-binary");
        // The old image survives aside under `.old.exe` for its holder.
        assert_eq!(
            fs::read(dir.path().join("lean-ctx.old.exe")).unwrap(),
            b"old-binary"
        );
    }

    /// The `.old` sidecar from a previous swap is reclaimed on the next
    /// install once nothing holds it any more.
    #[test]
    fn stale_old_sidecar_is_reclaimed_on_next_install() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("built.exe");
        let dst = dir.path().join("lean-ctx.exe");
        let old = dir.path().join("lean-ctx.old.exe");
        fs::write(&src, b"v3").unwrap();
        fs::write(&dst, b"v2").unwrap();
        fs::write(&old, b"v1").unwrap();

        atomic_install_binary(&src, &dst).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"v3");
        assert_eq!(fs::read(&old).unwrap(), b"v2");
    }

    /// A zero-sharing lock (AV/EDR-style) blocks even the rename-aside swap —
    /// the error must carry the actionable hint instead of a bare OS code.
    #[test]
    fn exclusive_lock_yields_actionable_error() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("built.exe");
        let dst = dir.path().join("lean-ctx.exe");
        fs::write(&src, b"new-binary").unwrap();
        fs::write(&dst, b"old-binary").unwrap();

        let holder = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&dst)
            .unwrap();

        let err = atomic_install_binary(&src, &dst).unwrap_err();
        assert!(err.contains("atomic rename failed"), "got: {err}");
        assert!(err.contains("re-run the install"), "hint missing: {err}");
        // The zero-sharing lock blocks our own verification read too — release
        // it first, then prove the old binary survived untouched.
        drop(holder);
        assert_eq!(fs::read(&dst).unwrap(), b"old-binary");
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::{find_cargo_project_root_from, is_homebrew_cellar_link};
    use std::path::Path;

    #[test]
    fn cellar_and_linuxbrew_links_are_detected() {
        // macOS Apple Silicon + Intel relative/absolute Cellar targets.
        assert!(is_homebrew_cellar_link(Path::new(
            "../Cellar/lean-ctx/3.7.1/bin/lean-ctx"
        )));
        assert!(is_homebrew_cellar_link(Path::new(
            "/opt/homebrew/Cellar/lean-ctx/3.8.0/bin/lean-ctx"
        )));
        // Linuxbrew.
        assert!(is_homebrew_cellar_link(Path::new(
            "/home/linuxbrew/.linuxbrew/Cellar/lean-ctx/1.0/bin/lean-ctx"
        )));
    }

    #[test]
    fn non_brew_targets_are_left_alone() {
        assert!(!is_homebrew_cellar_link(Path::new(
            "/Users/me/.local/bin/lean-ctx"
        )));
        assert!(!is_homebrew_cellar_link(Path::new("/usr/local/bin/other")));
        assert!(!is_homebrew_cellar_link(Path::new("lean-ctx")));
    }

    #[test]
    fn dev_install_finds_nested_rust_crate_from_repository_root() {
        let repo = tempfile::tempdir().unwrap();
        let crate_root = repo.path().join("rust");
        std::fs::create_dir(&crate_root).unwrap();
        std::fs::write(
            crate_root.join("Cargo.toml"),
            "[package]\nname = \"lean-ctx\"\n",
        )
        .unwrap();

        assert_eq!(find_cargo_project_root_from(repo.path()), Some(crate_root));
        assert_eq!(
            find_cargo_project_root_from(&repo.path().join("docs").join("internal")),
            Some(repo.path().join("rust"))
        );
    }
}
// SPDX-License-Identifier: Apache-2.0
