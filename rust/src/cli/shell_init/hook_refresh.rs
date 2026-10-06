// SPDX-License-Identifier: Apache-2.0
//! `_lc`/`_lc_compress` PATH shims and the refresh of installed shell hooks.
//!
//! Both keep aliases working where the hook's shell function is missing: the
//! shims cover shells that never load it (#1898), the refresh keeps the hook
//! file itself current across package-manager upgrades (#1959).

use super::{
    config_artifact_dir, generate_hook_fish, generate_hook_posix, generate_hook_powershell,
    hook_binary_for_shell, write_env_sh_for_containers,
};

/// Directories for the `_lc`/`_lc_compress` PATH shims, best first. A shim
/// only works in a directory that is on `PATH`, and `current_exe`'s directory
/// often is not (#1851): package managers keep the binary in a versioned
/// directory (scoop `apps/<ver>`, Homebrew `Cellar`, npm `node_modules`, mise
/// `installs`) and put a symlink or launcher on `PATH`. So, in order: the
/// running binary's directory when it is on `PATH`, every `PATH` directory
/// holding a `lean-ctx` launcher, then `~/.local/bin` when it is on `PATH`.
fn lc_shim_dirs(
    current_exe: Option<&std::path::Path>,
    path_dirs: &[std::path::PathBuf],
    home: Option<&std::path::Path>,
) -> Vec<std::path::PathBuf> {
    use crate::core::portable_binary::{dir_on_path, launchers_on_path};

    let exe_dir = current_exe
        .and_then(std::path::Path::parent)
        .filter(|d| dir_on_path(d, path_dirs))
        .map(std::path::Path::to_path_buf);
    let launcher_dirs = launchers_on_path(path_dirs)
        .into_iter()
        .filter_map(|p| p.parent().map(std::path::Path::to_path_buf));
    let user_bin = home
        .map(|h| h.join(".local").join("bin"))
        .filter(|d| dir_on_path(d, path_dirs));

    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    for dir in exe_dir.into_iter().chain(launcher_dirs).chain(user_bin) {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

/// Body of a `_lc`/`_lc_compress` PATH shim. Mirrors the hook's shell function
/// of the same name: honor the disable switches, pass through raw for a
/// non-TTY non-agent shell, otherwise route through the binary and fall back to
/// running the command directly if the binary itself cannot exec (126/127).
fn shim_script(name: &str, binary: &str, flag: &str) -> String {
    format!(
        "#!/bin/sh\n\
         # lean-ctx PATH fallback for the `{name}` shell function -- DO NOT EDIT.\n\
         # Shell resolves alias -> function -> PATH, so the hook's shell function\n\
         # shadows this whenever it is loaded (identical behavior there). This runs\n\
         # only where the function is absent: non-interactive subshells, scripts,\n\
         # xargs/find -exec, a pipeline's outer shell, and agent harnesses that\n\
         # snapshot+replay the shell and drop the function but keep the aliases\n\
         # that call it. Without it those contexts fail `{name}: command not found`.\n\
         if [ -n \"${{LEAN_CTX_DISABLED:-}}\" ] || [ -n \"${{LEAN_CTX_NO_HOOK:-}}\" ]; then\n\
         \texec \"$@\"\n\
         fi\n\
         if [ ! -t 1 ] && [ -z \"${{LEAN_CTX_AGENT:-}}\" ] && [ -z \"${{CURSOR_AGENT:-}}\" ] && [ -z \"${{CODEX_CLI_SESSION:-}}\" ] \\\n\
         \t&& [ -z \"${{CLAUDECODE:-}}\" ] && [ -z \"${{CODEBUDDY:-}}\" ] && [ -z \"${{GEMINI_SESSION:-}}\" ]; then\n\
         \texec \"$@\"\n\
         fi\n\
         '{binary}' {flag} \"$@\"\n\
         _lc_rc=$?\n\
         if [ \"$_lc_rc\" -eq 127 ] || [ \"$_lc_rc\" -eq 126 ]; then\n\
         \texec \"$@\"\n\
         fi\n\
         exit \"$_lc_rc\"\n"
    )
}

/// Write the `_lc`/`_lc_compress` PATH shims into `dir` (executable on Unix).
/// True when both were written.
fn write_lc_path_shims_in(dir: &std::path::Path, binary: &str) -> bool {
    let mut ok = true;
    for (name, flag) in [("_lc", "-t"), ("_lc_compress", "-c")] {
        let path = dir.join(name);
        if let Err(e) = std::fs::write(&path, shim_script(name, binary, flag)) {
            tracing::debug!("could not write shim {}: {e}", path.display());
            ok = false;
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
        }
    }
    ok
}

/// Install `_lc`/`_lc_compress` fallback executables on `PATH` so aliases never
/// break when the shell function is unavailable (see [`shim_script`]).
/// Self-contained: depends on no env wiring (BASH_ENV/env.sh) or snapshot
/// fidelity, and the same-named function shadows it where the hook is loaded.
/// Written to the first writable directory of [`lc_shim_dirs`].
pub(super) fn write_lc_path_shims(binary: &str) {
    let current_exe = std::env::current_exe().ok();
    let candidates = lc_shim_dirs(
        current_exe.as_deref(),
        &crate::core::portable_binary::path_dirs(),
        dirs::home_dir().as_deref(),
    );
    if candidates
        .iter()
        .any(|dir| write_lc_path_shims_in(dir, binary))
    {
        return;
    }
    tracing::warn!(
        "could not write the _lc/_lc_compress shims to a PATH directory; \
         aliases fail where the shell function is unavailable"
    );
}

/// Bring already-installed shell hooks up to this binary (#1959).
///
/// Only `init`, `setup`, `update` and `doctor --fix` write the hook files, so a
/// package-manager upgrade (FreeBSD ports, AUR, Homebrew, `cargo install`)
/// kept the hook an older build wrote — with aliases calling `_lc`, which
/// agent shells that drop `_`-prefixed functions fail on (#1898). Called on
/// MCP server start, next to the agent-hook refresh. Rewrites only hook files
/// that exist and are stale, never an rc file, and does nothing when the shell
/// hook is disabled.
pub fn refresh_installed_shell_hooks() {
    if crate::core::config::Config::load().shell_hook_disabled_effective() {
        return;
    }
    let Some(dir) = config_artifact_dir() else {
        return;
    };
    let binary = crate::core::portable_binary::stable_shell_binary(
        &crate::core::portable_binary::resolve_portable_binary(),
    );
    if refresh_shell_hooks_in(&dir, &binary) {
        let bash_binary = hook_binary_for_shell("bash", &binary);
        if dir.join("env.sh").exists() {
            write_env_sh_for_containers(&generate_hook_posix(&bash_binary));
        }
        write_lc_path_shims(&bash_binary);
    }
}

/// Rewrite each existing `shell-hook.*` in `dir` whose content differs from
/// what this build generates. Returns whether a bash/zsh hook was rewritten.
fn refresh_shell_hooks_in(dir: &std::path::Path, binary: &str) -> bool {
    let mut posix_refreshed = false;
    for (ext, shell) in [
        ("bash", "bash"),
        ("zsh", "zsh"),
        ("fish", "fish"),
        ("ps1", "powershell"),
    ] {
        let path = dir.join(format!("shell-hook.{ext}"));
        let Ok(current) = std::fs::read_to_string(&path) else {
            continue;
        };
        let shell_binary = hook_binary_for_shell(shell, binary);
        let fresh = match shell {
            "fish" => generate_hook_fish(&shell_binary),
            "powershell" => generate_hook_powershell(&shell_binary),
            _ => generate_hook_posix(&shell_binary),
        };
        if current == fresh {
            continue;
        }
        match std::fs::write(&path, fresh) {
            Ok(()) => {
                tracing::info!("refreshed stale shell hook {}", path.display());
                posix_refreshed |= matches!(shell, "bash" | "zsh");
            }
            Err(e) => tracing::debug!("could not refresh {}: {e}", path.display()),
        }
    }
    posix_refreshed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lc_shim_script_is_self_contained_fallback() {
        let s = shim_script("_lc", "/usr/bin/lean-ctx", "-t");
        assert!(s.starts_with("#!/bin/sh\n"), "needs a shebang: {s}");
        assert!(s.contains("'/usr/bin/lean-ctx' -t \"$@\""), "{s}");
        assert!(s.contains("exec \"$@\""), "{s}");
        assert!(s.contains("CLAUDECODE"), "{s}");
        assert!(s.contains("LEAN_CTX_DISABLED"), "{s}");
    }

    #[test]
    fn lc_compress_shim_uses_compress_flag() {
        let s = shim_script("_lc_compress", "/usr/bin/lean-ctx", "-c");
        assert!(s.contains("'/usr/bin/lean-ctx' -c \"$@\""), "{s}");
    }

    /// #1959: a hook written by an older build (aliases → `_lc`) is brought up
    /// to date without `init`; a shell the user never set up gets no hook.
    #[test]
    fn stale_installed_hooks_are_refreshed_and_absent_ones_left_alone() {
        // The generator bakes config values in; keep them stable for both calls.
        let _iso = crate::core::data_dir::isolated_data_dir();
        let tmp = tempfile::tempdir().expect("tempdir");
        let bin = "/usr/local/bin/lean-ctx";
        let hook = tmp.path().join("shell-hook.bash");
        std::fs::write(&hook, "alias grep='_lc grep'\n").expect("write stale hook");

        assert!(refresh_shell_hooks_in(tmp.path(), bin), "stale bash hook");
        assert_eq!(
            std::fs::read_to_string(&hook).expect("read hook"),
            generate_hook_posix(bin)
        );
        assert!(!tmp.path().join("shell-hook.zsh").exists());
        assert!(!tmp.path().join("shell-hook.fish").exists());
        assert!(
            !refresh_shell_hooks_in(tmp.path(), bin),
            "a current hook is not rewritten"
        );
    }

    #[test]
    fn write_lc_path_shims_writes_both_executables() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(write_lc_path_shims_in(tmp.path(), "/usr/bin/lean-ctx"));
        for name in ["_lc", "_lc_compress"] {
            assert!(tmp.path().join(name).exists(), "missing shim {name}");
        }
    }

    #[test]
    fn write_lc_path_shims_reports_an_unwritable_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(!write_lc_path_shims_in(
            &tmp.path().join("missing"),
            "/usr/bin/lean-ctx"
        ));
    }

    /// #1851: a package manager keeps the binary in a versioned directory off
    /// PATH and puts a launcher on PATH — the shims belong next to the launcher.
    #[cfg(unix)]
    mod lc_shim_dirs_1851 {
        use super::super::lc_shim_dirs;
        use std::path::{Path, PathBuf};

        fn launcher_dir(root: &Path, rel: &str) -> PathBuf {
            let dir = root.join(rel);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("lean-ctx"), "#!/bin/sh\n").unwrap();
            dir
        }

        #[test]
        fn versioned_install_dir_off_path_is_skipped() {
            let tmp = tempfile::tempdir().unwrap();
            let apps = launcher_dir(tmp.path(), "scoop/apps/lean-ctx/3.10.2");
            let shims = launcher_dir(tmp.path(), "scoop/shims");
            let dirs = lc_shim_dirs(
                Some(&apps.join("lean-ctx")),
                &[tmp.path().join("usr/bin"), shims.clone()],
                None,
            );
            assert_eq!(dirs, vec![shims]);
        }

        #[test]
        fn exe_dir_on_path_comes_first_then_launchers_then_user_bin() {
            let tmp = tempfile::tempdir().unwrap();
            let home = tmp.path().join("home");
            let user_bin = home.join(".local/bin");
            std::fs::create_dir_all(&user_bin).unwrap();
            let brew = launcher_dir(tmp.path(), "opt/homebrew/bin");
            let exe_dir = launcher_dir(tmp.path(), "exe");
            let dirs = lc_shim_dirs(
                Some(&exe_dir.join("lean-ctx")),
                &[brew.clone(), user_bin.clone(), exe_dir.clone()],
                Some(&home),
            );
            assert_eq!(dirs, vec![exe_dir, brew, user_bin]);
        }

        #[test]
        fn user_bin_off_path_is_not_a_candidate() {
            let tmp = tempfile::tempdir().unwrap();
            let home = tmp.path().join("home");
            std::fs::create_dir_all(home.join(".local/bin")).unwrap();
            let dirs = lc_shim_dirs(None, &[tmp.path().join("usr/bin")], Some(&home));
            assert!(dirs.is_empty(), "{dirs:?}");
        }
    }
}
