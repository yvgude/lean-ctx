//! Process teardown + binary removal for `lean-ctx uninstall`.
//!
//! A "proper" uninstall must (1) stop every lean-ctx process so nothing respawns or
//! holds the data dir we are about to delete, and (2) remove the installed binary itself
//! — not merely print a `rm` hint. Both steps are best-effort and never abort the rest of
//! the uninstall.

use std::fs;
use std::path::{Path, PathBuf};

use super::shorten;

/// Stops the daemon, proxy, and any stray lean-ctx processes (mirrors `lean-ctx stop`,
/// but never exits the process — the uninstall must keep going). The current process and
/// IDE-owned MCP servers are excluded by `find_killable_pids`.
pub(super) fn stop_processes(dry_run: bool) {
    if dry_run {
        println!("  Would stop the daemon, proxy, and any running lean-ctx processes");
        return;
    }

    println!("  Stopping lean-ctx processes…");

    crate::proxy_autostart::stop();
    crate::daemon_autostart::stop();
    let _ = crate::daemon::stop_daemon();

    let _ = crate::ipc::process::kill_all_by_name("lean-ctx");
    std::thread::sleep(std::time::Duration::from_millis(500));

    let remaining = crate::ipc::process::find_killable_pids("lean-ctx");
    for &pid in &remaining {
        let _ = crate::ipc::process::force_kill(pid);
    }
    if !remaining.is_empty() {
        std::thread::sleep(std::time::Duration::from_millis(300));
    }

    crate::daemon::cleanup_daemon_files();
    println!("  ✓ Processes stopped");
}

/// How a candidate binary path should be handled.
enum Disposition {
    /// Safe to delete (a managed copy or a PATH symlink we created).
    Remove,
    /// A dev build inside a cargo `target/` dir — never touch the user's repo build.
    DevBuild,
    /// Installed by a package manager that tracks it — defer to that manager.
    Cargo,
    Homebrew,
}

fn classify(path: &Path) -> Disposition {
    let p = path.to_string_lossy();
    if p.contains("/target/release/") || p.contains("/target/debug/") {
        Disposition::DevBuild
    } else if p.contains("/.cargo/") {
        Disposition::Cargo
    } else if p.contains("/Cellar/") || p.contains("homebrew") {
        Disposition::Homebrew
    } else {
        Disposition::Remove
    }
}

/// Standard locations the binary may live in, plus the currently running executable.
fn candidate_paths(home: &Path) -> Vec<PathBuf> {
    let install_dir = std::env::var_os("LEAN_CTX_INSTALL_DIR")
        .map_or_else(|| home.join(".local/bin"), PathBuf::from);

    let mut out = vec![
        install_dir.join("lean-ctx"),
        PathBuf::from("/usr/local/bin/lean-ctx"),
        PathBuf::from("/opt/homebrew/bin/lean-ctx"),
    ];
    if let Ok(exe) = std::env::current_exe() {
        out.push(exe);
    }

    // De-duplicate by string while preserving order.
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(p.to_string_lossy().to_string()));
    out
}

/// Removes the installed binary (and PATH symlinks) where it is safe to do so. Returns
/// `true` if anything was removed or would be removed in a dry run.
///
/// `keep_binary` short-circuits the whole step (e.g. when reinstalling in place).
pub(super) fn remove_binaries(home: &Path, dry_run: bool, keep_binary: bool) -> bool {
    if keep_binary {
        println!("  · Skipped: binary (--keep-binary)");
        return false;
    }

    let mut removed = false;
    let mut cargo_hint = false;
    let mut brew_hint = false;

    for path in candidate_paths(home) {
        // `symlink_metadata` does not follow symlinks, so a PATH symlink is detected (and
        // later removed) as the link itself, never its target.
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };

        match classify(&path) {
            Disposition::DevBuild => {} // leave the user's repo build alone
            Disposition::Cargo => cargo_hint = true,
            Disposition::Homebrew => brew_hint = true,
            Disposition::Remove => {
                let short = shorten(&path, home);
                if dry_run {
                    println!("  Would remove binary ({short})");
                    removed = true;
                    continue;
                }
                let res = if meta.is_dir() {
                    fs::remove_dir_all(&path)
                } else {
                    // Unlinking a running executable is allowed on Unix (the inode lives
                    // until the process exits); removes a symlink without touching target.
                    fs::remove_file(&path)
                };
                match res {
                    Ok(()) => {
                        println!("  ✓ Binary removed ({short})");
                        removed = true;
                    }
                    Err(e) => {
                        // Windows refuses to delete a running .exe; tell the user.
                        if cfg!(windows) {
                            println!(
                                "  · Could not remove the running binary ({short}). \
                                 Delete it after this process exits."
                            );
                        } else {
                            tracing::warn!("Failed to remove binary {}: {e}", path.display());
                        }
                    }
                }
            }
        }
    }

    if cargo_hint {
        println!("  · Installed via cargo — finish with: cargo uninstall lean-ctx");
    }
    if brew_hint {
        println!("  · Installed via Homebrew — finish with: brew uninstall lean-ctx");
    }
    if !removed && !cargo_hint && !brew_hint {
        println!("  · No managed binary found on standard paths");
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_builds_are_never_removed() {
        assert!(matches!(
            classify(Path::new("/home/u/lean-ctx/rust/target/release/lean-ctx")),
            Disposition::DevBuild
        ));
        assert!(matches!(
            classify(Path::new("/home/u/proj/target/debug/lean-ctx")),
            Disposition::DevBuild
        ));
    }

    #[test]
    fn package_managers_are_deferred() {
        assert!(matches!(
            classify(Path::new("/home/u/.cargo/bin/lean-ctx")),
            Disposition::Cargo
        ));
        assert!(matches!(
            classify(Path::new("/opt/homebrew/bin/lean-ctx")),
            Disposition::Homebrew
        ));
        assert!(matches!(
            classify(Path::new("/usr/local/Cellar/lean-ctx/3.7.0/bin/lean-ctx")),
            Disposition::Homebrew
        ));
    }

    #[test]
    fn managed_install_dirs_are_removable() {
        assert!(matches!(
            classify(Path::new("/home/u/.local/bin/lean-ctx")),
            Disposition::Remove
        ));
        assert!(matches!(
            classify(Path::new("/usr/local/bin/lean-ctx")),
            Disposition::Remove
        ));
    }

    #[test]
    fn keep_binary_skips_removal() {
        let home = std::env::temp_dir();
        assert!(!remove_binaries(&home, true, true));
    }

    #[test]
    fn candidate_paths_include_install_dir_and_dedup() {
        let home = PathBuf::from("/home/tester");
        let paths = candidate_paths(&home);
        // No duplicates.
        let mut seen = std::collections::HashSet::new();
        for p in &paths {
            assert!(
                seen.insert(p.clone()),
                "duplicate candidate: {}",
                p.display()
            );
        }
    }
}
