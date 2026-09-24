/// Verbatim hook-binary override (#708), or `None` when unset/blank.
///
/// Users who sync agent settings (`~/.claude/settings.json`, …) across
/// machines with different usernames set `LEAN_CTX_HOOK_BINARY` (env, wins)
/// or `hook_binary` (config.toml) to an env-based form like
/// `$HOME/.local/bin/lean-ctx`. Hook hosts execute commands through a shell,
/// which expands the variable at run time. The value is emitted verbatim by
/// every hook writer and accepted verbatim by `doctor`'s staleness check —
/// scoped to hook/agent artifacts only, never to launchd/systemd units
/// (which do not expand shell variables).
pub(crate) fn hook_binary_override() -> Option<String> {
    std::env::var("LEAN_CTX_HOOK_BINARY")
        .ok()
        .or_else(|| crate::core::config::Config::load().hook_binary.clone())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub(crate) fn resolve_portable_binary() -> String {
    let current = std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().into_owned());

    let which_cmd = if cfg!(windows) { "where" } else { "which" };
    let which_raw = std::process::Command::new(which_cmd)
        .arg("lean-ctx")
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());

    choose_binary_path(current.as_deref(), which_raw.as_deref())
}

/// File names a `lean-ctx` launcher can have inside a `PATH` directory.
const LAUNCHER_NAMES: &[&str] = if cfg!(windows) {
    &["lean-ctx.exe", "lean-ctx.cmd"]
} else {
    &["lean-ctx"]
};

/// The directories on `PATH`, in lookup order.
pub(crate) fn path_dirs() -> Vec<std::path::PathBuf> {
    std::env::var_os("PATH")
        .map(|p| {
            std::env::split_paths(&p)
                .filter(|d| !d.as_os_str().is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// True when `dir` is one of `path_dirs`, directly or through a symlink.
pub(crate) fn dir_on_path(dir: &std::path::Path, path_dirs: &[std::path::PathBuf]) -> bool {
    let canonical = std::fs::canonicalize(dir).ok();
    path_dirs
        .iter()
        .any(|d| d == dir || (canonical.is_some() && std::fs::canonicalize(d).ok() == canonical))
}

/// Every `lean-ctx` launcher on `PATH`, in lookup order: the entry a shell
/// runs for `lean-ctx`, followed by the ones it shadows.
pub(crate) fn launchers_on_path(path_dirs: &[std::path::PathBuf]) -> Vec<std::path::PathBuf> {
    path_dirs
        .iter()
        .filter_map(|d| {
            LAUNCHER_NAMES
                .iter()
                .map(|name| d.join(name))
                .find(|p| p.is_file())
        })
        .collect()
}

/// The binary path to embed in machine-local shell artifacts — the shell
/// hooks, `env.sh` and the `_lc` shims (#1851).
///
/// Package managers expose `lean-ctx` on `PATH` through a stable entry (a
/// Homebrew symlink, a scoop shim, an npm or mise launcher) while the binary
/// itself lives in a versioned directory off `PATH`. `current_exe` resolves to
/// that versioned directory, which the next update removes. So when `binary`
/// is not in a `PATH` directory, the first launcher on `PATH` is embedded
/// instead: it is what the user runs for `lean-ctx`, and it follows updates.
pub(crate) fn stable_shell_binary(binary: &str) -> String {
    stable_shell_binary_in(binary, &path_dirs())
}

fn stable_shell_binary_in(binary: &str, path_dirs: &[std::path::PathBuf]) -> String {
    let path = std::path::Path::new(binary);
    if !path.is_absolute() || path.parent().is_some_and(|d| dir_on_path(d, path_dirs)) {
        return binary.to_string();
    }
    launchers_on_path(path_dirs).first().map_or_else(
        || binary.to_string(),
        |p| sanitize_exe_path(&p.to_string_lossy()),
    )
}

/// Decide which `lean-ctx` path to bake into generated artifacts (autostart
/// plists, daemon spawn, MCP server command, agent/shell hooks, update
/// scheduler). The chosen path must be the *exact build the user is running*, so
/// every artifact agrees and a single `setup`/`dev-install` can never leave the
/// daemon on a different build than the proxy/MCP config.
///
/// Preference order:
/// 1. `current_exe` when absolute and not inside a transient Cargo build dir —
///    by construction the build currently in use.
/// 2. `which lean-ctx` — the installed copy on PATH; used when the running binary
///    lives in `target/{debug,release}` (`cargo run -- setup`), where the
///    installed copy is the intended target.
/// 3. an absolute `current_exe` even from a build dir — still better than a bare
///    name (keeps generated hooks absolute, see #367).
/// 4. bare `lean-ctx`.
///
/// Prior to #2444 this preferred `which` first, making the baked path depend on
/// ambient PATH ordering at generation time. That was non-deterministic: the
/// daemon autostart could capture a stale Homebrew copy shadowing `~/.local/bin`
/// while the proxy/MCP config captured the fresh build — silently running two
/// different builds at once.
fn choose_binary_path(current_exe: Option<&str>, which_raw: Option<&str>) -> String {
    let is_build_artifact = |p: &str| {
        p.contains("/target/debug/")
            || p.contains("/target/release/")
            || p.contains("\\target\\debug\\")
            || p.contains("\\target\\release\\")
    };

    // 1. Prefer the running binary when it lives in a stable install location.
    if let Some(exe) = current_exe
        && std::path::Path::new(exe).is_absolute()
        && !is_build_artifact(exe)
    {
        return sanitize_exe_path(exe);
    }

    // 2. Otherwise fall back to the installed copy on PATH.
    if let Some(raw) = which_raw {
        let path = pick_best_binary_line(raw);
        if std::path::Path::new(&path).is_absolute() {
            return sanitize_exe_path(&path);
        }
    }

    // 3. An absolute build-artifact path still beats a bare name.
    if let Some(exe) = current_exe
        && std::path::Path::new(exe).is_absolute()
    {
        return sanitize_exe_path(exe);
    }

    // 4. Last resort.
    "lean-ctx".to_string()
}

/// On Windows, `where lean-ctx` returns multiple lines (e.g. `lean-ctx` and
/// `lean-ctx.cmd`). Pick the `.cmd`/`.exe` variant if available, otherwise
/// the first line.
fn pick_best_binary_line(raw: &str) -> String {
    let lines: Vec<&str> = raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.len() <= 1 {
        return lines.first().unwrap_or(&"lean-ctx").to_string();
    }
    if cfg!(windows)
        && let Some(cmd) = lines.iter().find(|l| {
            std::path::Path::new(*l).extension().is_some_and(|ext| {
                ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("exe")
            })
        })
    {
        return cmd.to_string();
    }
    lines[0].to_string()
}

fn sanitize_exe_path(path: &str) -> String {
    let cleaned = path.trim_end_matches(" (deleted)");
    if cfg!(windows) {
        super::pathutil::normalize_tool_path(cleaned)
    } else {
        cleaned.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_line_returns_as_is() {
        assert_eq!(
            pick_best_binary_line("/usr/bin/lean-ctx"),
            "/usr/bin/lean-ctx"
        );
    }

    #[test]
    fn multiline_returns_first_line() {
        let raw = "/usr/bin/lean-ctx\n/usr/local/bin/lean-ctx";
        let result = pick_best_binary_line(raw);
        assert_eq!(result, "/usr/bin/lean-ctx");
    }

    #[test]
    fn empty_returns_fallback() {
        assert_eq!(pick_best_binary_line(""), "lean-ctx");
    }

    #[test]
    fn sanitize_removes_deleted_suffix() {
        assert_eq!(
            sanitize_exe_path("/usr/bin/lean-ctx (deleted)"),
            "/usr/bin/lean-ctx"
        );
    }

    #[test]
    fn whitespace_lines_are_filtered() {
        let raw = "  /usr/bin/lean-ctx  \n  \n  /usr/local/bin/lean-ctx  ";
        assert_eq!(pick_best_binary_line(raw), "/usr/bin/lean-ctx");
    }

    #[cfg(windows)]
    #[test]
    fn sanitize_normalizes_msys_path_on_windows() {
        assert_eq!(
            sanitize_exe_path("/c/Users/ABC/.local/bin/lean-ctx"),
            "C:/Users/ABC/.local/bin/lean-ctx"
        );
    }

    #[cfg(windows)]
    #[test]
    fn sanitize_keeps_native_windows_path() {
        assert_eq!(
            sanitize_exe_path(r"C:\Users\ABC\lean-ctx.exe"),
            "C:/Users/ABC/lean-ctx.exe"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn sanitize_unix_path_unchanged() {
        assert_eq!(
            sanitize_exe_path("/usr/local/bin/lean-ctx"),
            "/usr/local/bin/lean-ctx"
        );
    }

    /// #708: the env override is emitted verbatim — no absolutization, no
    /// rewriting — so synced settings keep `$HOME/...` forms; blank means
    /// unset so an empty export cannot wedge hook generation.
    #[test]
    fn hook_binary_override_env_is_verbatim_and_blank_is_none() {
        let _lock = crate::core::data_dir::test_env_lock();
        // SAFETY: serialized by test_env_lock.
        unsafe { std::env::set_var("LEAN_CTX_HOOK_BINARY", "$HOME/.local/bin/lean-ctx") };
        assert_eq!(
            hook_binary_override().as_deref(),
            Some("$HOME/.local/bin/lean-ctx")
        );
        // SAFETY: serialized by test_env_lock.
        unsafe { std::env::set_var("LEAN_CTX_HOOK_BINARY", "   ") };
        assert_eq!(hook_binary_override(), None);
        // SAFETY: serialized by test_env_lock.
        unsafe { std::env::remove_var("LEAN_CTX_HOOK_BINARY") };
    }

    #[test]
    fn resolve_portable_binary_is_absolute() {
        // #367: generated hook commands must use an absolute binary path, never
        // a bare `lean-ctx`, because agents run hooks under non-login shells
        // without the install dir on PATH. `which`/`current_exe()` both yield
        // an absolute path in any normal environment (incl. the test harness).
        let resolved = resolve_portable_binary();
        assert!(
            std::path::Path::new(&resolved).is_absolute(),
            "resolve_portable_binary must return an absolute path, got: {resolved}"
        );
    }

    #[test]
    fn nothing_resolvable_returns_bare_name() {
        // #2444: neither a usable running binary nor a PATH hit -> bare name.
        assert_eq!(choose_binary_path(None, None), "lean-ctx");
        // A relative current_exe is not a usable absolute path.
        assert_eq!(choose_binary_path(Some("lean-ctx"), None), "lean-ctx");
    }

    // Unix absolute paths (the `/...` form is not absolute on Windows).
    #[cfg(not(windows))]
    mod unix_paths {
        use super::*;

        #[test]
        fn current_exe_beats_path_lookup() {
            // The core of #2444: the *running* build wins over a divergent PATH
            // entry (e.g. a stale Homebrew copy shadowing ~/.local/bin).
            let chosen = choose_binary_path(
                Some("/Users/dev/.local/bin/lean-ctx"),
                Some("/opt/homebrew/bin/lean-ctx"),
            );
            assert_eq!(chosen, "/Users/dev/.local/bin/lean-ctx");
        }

        #[test]
        fn release_build_artifact_falls_back_to_path() {
            // `cargo run --release -- setup`: bake the installed copy, not the
            // transient build output.
            let chosen = choose_binary_path(
                Some("/work/lean-ctx/rust/target/release/lean-ctx"),
                Some("/Users/dev/.local/bin/lean-ctx"),
            );
            assert_eq!(chosen, "/Users/dev/.local/bin/lean-ctx");
        }

        #[test]
        fn debug_build_artifact_falls_back_to_path() {
            let chosen = choose_binary_path(
                Some("/work/lean-ctx/rust/target/debug/deps/lean_ctx-abc123"),
                Some("/usr/local/bin/lean-ctx"),
            );
            assert_eq!(chosen, "/usr/local/bin/lean-ctx");
        }

        #[test]
        fn build_artifact_without_path_keeps_absolute_current_exe() {
            // No installed copy on PATH -> an absolute build path still beats the
            // bare name, so generated hooks stay absolute (#367).
            let chosen =
                choose_binary_path(Some("/work/lean-ctx/rust/target/release/lean-ctx"), None);
            assert_eq!(chosen, "/work/lean-ctx/rust/target/release/lean-ctx");
        }

        #[test]
        fn relative_current_exe_falls_back_to_path() {
            let chosen = choose_binary_path(Some("lean-ctx"), Some("/usr/bin/lean-ctx"));
            assert_eq!(chosen, "/usr/bin/lean-ctx");
        }

        #[test]
        fn path_lookup_multiline_picks_first() {
            let chosen = choose_binary_path(
                None,
                Some("/Users/dev/.local/bin/lean-ctx\n/opt/homebrew/bin/lean-ctx"),
            );
            assert_eq!(chosen, "/Users/dev/.local/bin/lean-ctx");
        }
    }

    /// #1851: package-manager layouts, a versioned install dir off PATH and a
    /// stable launcher dir on it.
    #[cfg(unix)]
    mod stable_shell_binary {
        use super::super::*;
        use std::path::{Path, PathBuf};

        fn launcher(dir: &Path) -> PathBuf {
            std::fs::create_dir_all(dir).unwrap();
            let path = dir.join("lean-ctx");
            std::fs::write(&path, "#!/bin/sh\n").unwrap();
            path
        }

        #[test]
        fn binary_in_a_path_dir_is_kept() {
            let tmp = tempfile::tempdir().unwrap();
            let bin = launcher(&tmp.path().join("bin"));
            let other = launcher(&tmp.path().join("other"));
            let dirs: Vec<PathBuf> =
                vec![other.parent().unwrap().into(), bin.parent().unwrap().into()];
            let bin = bin.to_string_lossy();
            assert_eq!(stable_shell_binary_in(&bin, &dirs), bin);
        }

        #[test]
        fn versioned_install_dir_resolves_to_the_path_launcher() {
            let tmp = tempfile::tempdir().unwrap();
            let cellar = launcher(&tmp.path().join("Cellar/lean-ctx/3.10.2/bin"));
            let shims = launcher(&tmp.path().join("shims"));
            let dirs = vec![tmp.path().join("empty"), shims.parent().unwrap().into()];
            assert_eq!(
                stable_shell_binary_in(&cellar.to_string_lossy(), &dirs),
                shims.to_string_lossy()
            );
        }

        #[test]
        fn symlinked_path_dir_counts_as_on_path() {
            let tmp = tempfile::tempdir().unwrap();
            let bin = launcher(&tmp.path().join("real"));
            let link = tmp.path().join("link");
            std::os::unix::fs::symlink(bin.parent().unwrap(), &link).unwrap();
            let bin = bin.to_string_lossy();
            assert_eq!(stable_shell_binary_in(&bin, &[link]), bin);
        }

        #[test]
        fn no_launcher_on_path_keeps_the_binary() {
            let tmp = tempfile::tempdir().unwrap();
            let bin = launcher(&tmp.path().join("apps/3.10.2"));
            let bin = bin.to_string_lossy();
            assert_eq!(stable_shell_binary_in(&bin, &[tmp.path().join("x")]), bin);
            assert_eq!(stable_shell_binary_in("lean-ctx", &[]), "lean-ctx");
        }
    }
}
