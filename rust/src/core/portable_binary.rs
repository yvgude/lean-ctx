#[must_use]
pub fn resolve_portable_binary() -> String {
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
}
