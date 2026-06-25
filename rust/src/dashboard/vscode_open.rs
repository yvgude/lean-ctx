//! `lean-ctx dashboard --vscode`: open the dashboard as a native editor tab.
//!
//! A VS Code-family editor can only create a webview from *inside* a running
//! extension, so the CLI hands off to the lean-ctx editor extension through its
//! registered URI handler (`<scheme>://LeanCTX.lean-ctx/dashboard`). The
//! extension then owns the dashboard server **and** the tab — which is exactly
//! why, on a successful hand-off, the caller must NOT start a server of its own
//! (doing so would leave a second, orphaned listener behind).
//!
//! Detection is env-based: the integrated terminal exports identifying
//! variables, so a hand-off only ever targets the editor that launched the
//! current shell. When no such editor is found (or the extension isn't
//! installed) the caller falls back to serving + the browser, so the command is
//! never a silent no-op.

/// Extension identifier (`publisher.name`) — the URI authority the editor routes
/// the deep link to. Must stay in sync with `vscode-extension/package.json`.
const EXTENSION_ID: &str = "LeanCTX.lean-ctx";

/// Outcome of a hand-off attempt, so the caller knows whether it still needs to
/// serve the dashboard itself.
pub(crate) enum EditorOpen {
    /// Deep link issued to an editor that has the extension. The caller must
    /// return without starting a server — the extension owns it. Carries the
    /// editor label for the confirmation message.
    Handed(&'static str),
    /// An editor was detected but the lean-ctx extension isn't installed (or the
    /// link could not be fired). The caller should serve + open the browser and
    /// nudge the user to install the extension. Carries the editor label.
    NeedsExtension(&'static str),
    /// Not running inside a VS Code-family editor — the caller should serve
    /// normally (browser).
    NoEditor,
}

/// A VS Code-family editor and how to reach it from the shell.
#[derive(Clone, Copy)]
struct Editor {
    /// Human label for messages, e.g. `"Cursor"`.
    label: &'static str,
    /// CLI binary that accepts `--open-url`, e.g. `"code"`, `"cursor"`.
    bin: &'static str,
    /// URL scheme its URI handler listens on, e.g. `"vscode"`, `"cursor"`.
    scheme: &'static str,
    /// Per-user extensions directory (relative to `$HOME`) used to detect the
    /// extension on disk.
    ext_dir: &'static str,
}

const CURSOR: Editor = Editor {
    label: "Cursor",
    bin: "cursor",
    scheme: "cursor",
    ext_dir: ".cursor/extensions",
};
const WINDSURF: Editor = Editor {
    label: "Windsurf",
    bin: "windsurf",
    scheme: "windsurf",
    ext_dir: ".windsurf/extensions",
};
const VSCODIUM: Editor = Editor {
    label: "VSCodium",
    bin: "codium",
    scheme: "vscodium",
    ext_dir: ".vscode-oss/extensions",
};
const INSIDERS: Editor = Editor {
    label: "VS Code Insiders",
    bin: "code-insiders",
    scheme: "vscode-insiders",
    ext_dir: ".vscode-insiders/extensions",
};
const VSCODE: Editor = Editor {
    label: "VS Code",
    bin: "code",
    scheme: "vscode",
    ext_dir: ".vscode/extensions",
};

/// Pure mapping from environment signals to an editor. Split out from
/// [`detect_editor`] so it can be unit-tested without mutating process env.
///
/// Order matters: every fork also matches the generic `code` substring (their
/// app paths contain "Code"), so the specific forks are checked first.
fn classify(
    askpass: &str,
    bundle: &str,
    term: &str,
    cursor_trace: bool,
    vscode_injection: bool,
) -> Option<Editor> {
    let bundle = bundle.to_ascii_lowercase();
    // macOS exports `__CFBundleIdentifier` to every child of the GUI app, so it
    // identifies the editor even when the shell isn't a freshly-spawned
    // integrated terminal (which is what sets TERM_PROGRAM / the askpass vars).
    // Cursor ships via ToDesktop (`com.todesktop.*`); a wrong guess here is safe
    // because `extension_installed` then gates the hand-off and falls back to
    // the browser.
    let bundle_family = bundle.starts_with("com.microsoft.vscode")
        || bundle.starts_with("com.todesktop.")
        || bundle.contains("vscodium")
        || bundle.contains("windsurf")
        || bundle.contains("visualstudio.code");
    let in_family = vscode_injection
        || cursor_trace
        || !askpass.is_empty()
        || term.eq_ignore_ascii_case("vscode")
        || bundle_family;
    if !in_family {
        return None;
    }

    // Order matters: every fork also matches the generic `code` / VS Code id, so
    // the specific forks are checked first.
    let hay = format!("{} {bundle}", askpass.to_ascii_lowercase());
    if cursor_trace || hay.contains("cursor") || bundle.starts_with("com.todesktop.") {
        Some(CURSOR)
    } else if hay.contains("windsurf") {
        Some(WINDSURF)
    } else if hay.contains("vscodium") || hay.contains("codium") {
        Some(VSCODIUM)
    } else if hay.contains("insiders") {
        Some(INSIDERS)
    } else {
        Some(VSCODE)
    }
}

/// Identify the editor hosting the current integrated terminal from the env vars
/// VS Code-family editors export. `None` when not run inside such a terminal.
fn detect_editor() -> Option<Editor> {
    classify(
        &std::env::var("VSCODE_GIT_ASKPASS_MAIN").unwrap_or_default(),
        &std::env::var("__CFBundleIdentifier").unwrap_or_default(),
        &std::env::var("TERM_PROGRAM").unwrap_or_default(),
        std::env::var_os("CURSOR_TRACE_ID").is_some(),
        std::env::var_os("VSCODE_INJECTION").is_some(),
    )
}

/// Best-effort: is the lean-ctx extension present in `editor`'s extensions dir?
///
/// Installed extensions live in `<ext_dir>/<publisher>.<name>-<version>/`, with
/// the id lowercased on disk (e.g. `leanctx.lean-ctx-0.2.0`). A missing or
/// unreadable directory yields `false`, which intentionally degrades to the
/// browser path rather than firing a link nothing will handle.
fn extension_installed(editor: Editor) -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let needle = EXTENSION_ID.to_ascii_lowercase();
    // The fork's own dir, plus the Remote/WSL/SSH server dir (extensions install
    // there regardless of which local fork opened the window).
    for dir in [
        home.join(editor.ext_dir),
        home.join(".vscode-server/extensions"),
    ] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry
                .file_name()
                .to_string_lossy()
                .to_ascii_lowercase()
                .starts_with(&needle)
            {
                return true;
            }
        }
    }
    false
}

/// Is `bin` runnable from `PATH`? Integrated terminals usually inject their
/// editor's CLI, but a GUI-launched editor with a stripped `PATH` may not.
fn bin_on_path(bin: &str) -> bool {
    let which = if cfg!(windows) { "where" } else { "which" };
    std::process::Command::new(which)
        .arg(bin)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn run_opener(bin: &str, args: &[&str]) -> bool {
    std::process::Command::new(bin)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Hand `uri` to the OS default protocol handler (the scheme is registered to
/// the editor), used when the editor's own CLI isn't on `PATH`.
fn os_open(uri: &str) -> bool {
    #[cfg(target_os = "macos")]
    let ok = run_opener("open", &[uri]);
    #[cfg(target_os = "linux")]
    let ok = run_opener("xdg-open", &[uri]);
    #[cfg(target_os = "windows")]
    let ok = run_opener("cmd", &["/C", "start", "", uri]);
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let ok = {
        let _ = uri;
        false
    };
    ok
}

/// Fire the deep link at `editor`. Prefer the editor's own CLI (`--open-url`
/// targets the active window per the VS Code docs); fall back to the OS opener.
fn issue_deep_link(editor: Editor) -> bool {
    let uri = format!("{}://{}/dashboard", editor.scheme, EXTENSION_ID);
    if bin_on_path(editor.bin) && run_opener(editor.bin, &["--open-url", uri.as_str()]) {
        return true;
    }
    os_open(&uri)
}

/// Try to open the dashboard as a native editor tab. See [`EditorOpen`].
pub(crate) fn open_in_editor() -> EditorOpen {
    let Some(editor) = detect_editor() else {
        return EditorOpen::NoEditor;
    };
    if !extension_installed(editor) {
        return EditorOpen::NeedsExtension(editor.label);
    }
    if issue_deep_link(editor) {
        EditorOpen::Handed(editor.label)
    } else {
        // Extension is there but the link wouldn't fire — let the caller serve +
        // browser so the user still gets the dashboard.
        EditorOpen::NeedsExtension(editor.label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_outside_an_editor_is_none() {
        assert!(classify("", "", "Apple_Terminal", false, false).is_none());
        assert!(classify("", "", "iTerm.app", false, false).is_none());
        assert!(classify("", "", "", false, false).is_none());
    }

    #[test]
    fn classify_detects_each_fork() {
        let scheme = |a: &str, t: &str, cur: bool| classify(a, "", t, cur, false).map(|e| e.scheme);
        assert_eq!(
            scheme(
                "/Applications/Cursor.app/Contents/.../askpass.js",
                "vscode",
                false
            ),
            Some("cursor")
        );
        // Cursor also exports CURSOR_TRACE_ID even when the path is unhelpful.
        assert_eq!(scheme("", "vscode", true), Some("cursor"));
        assert_eq!(
            scheme("/Applications/Windsurf.app/.../askpass.js", "vscode", false),
            Some("windsurf")
        );
        assert_eq!(
            scheme("/Applications/VSCodium.app/.../askpass.js", "vscode", false),
            Some("vscodium")
        );
        assert_eq!(
            scheme(
                "/Applications/Visual Studio Code.app/.../askpass.js",
                "vscode",
                false
            ),
            Some("vscode")
        );
    }

    #[test]
    fn classify_insiders_wins_over_generic_code() {
        // "Code - Insiders" contains "code"; it must resolve to Insiders, never
        // to stable VS Code.
        let e = classify(
            "/Applications/Visual Studio Code - Insiders.app/.../askpass.js",
            "",
            "vscode",
            false,
            false,
        )
        .expect("in editor family");
        assert_eq!(e.scheme, "vscode-insiders");
        assert_eq!(e.bin, "code-insiders");
    }

    #[test]
    fn classify_falls_back_to_vscode_on_bare_term_program() {
        // A plain VS Code integrated terminal may expose only TERM_PROGRAM.
        let e = classify("", "", "vscode", false, false).expect("in editor family");
        assert_eq!(e.scheme, "vscode");
        assert_eq!(e.bin, "code");
    }

    #[test]
    fn classify_uses_injection_signal_when_term_program_missing() {
        // VSCODE_INJECTION alone is enough to know we're in the family.
        assert!(classify("", "", "", false, true).is_some());
    }

    #[test]
    fn classify_detects_via_macos_bundle_id() {
        // On macOS the bundle id alone identifies the editor (no integrated
        // terminal vars). Cursor ships via ToDesktop.
        let scheme = |b: &str| classify("", b, "", false, false).map(|e| e.scheme);
        assert_eq!(scheme("com.todesktop.230313mzl4w4u92"), Some("cursor"));
        assert_eq!(scheme("com.microsoft.VSCode"), Some("vscode"));
        assert_eq!(
            scheme("com.microsoft.VSCodeInsiders"),
            Some("vscode-insiders")
        );
        // A plain (non-editor) bundle id must not be treated as an editor.
        assert!(classify("", "com.apple.Terminal", "", false, false).is_none());
    }
}
