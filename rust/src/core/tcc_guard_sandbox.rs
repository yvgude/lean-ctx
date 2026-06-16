//! macOS Seatbelt self-sandbox for launchd-owned lean-ctx processes (#356).
//!
//! The daemon, proxy and auto-updater run as LaunchAgents — i.e. under
//! `launchd` (`ppid 1`) with their own TCC identity. Any `stat`/`read_dir`/
//! `realpath` they perform under `~/Documents`, `~/Desktop` or `~/Downloads`
//! pops the macOS privacy prompt *in lean-ctx's own name*, and because every
//! release re-signs the binary (new cdhash) the grant is invalidated on each
//! update, so it re-prompts forever.
//!
//! The opt-out path guards in [`crate::core::pathutil`] avoid those accesses
//! per call site, but that is fragile: one forgotten probe — or a dependency
//! that walks the filesystem — reintroduces the prompt. This module adds a
//! hard, kernel-enforced backstop: the LaunchAgent `ProgramArguments` are
//! wrapped in `sandbox-exec` with a profile that *denies* file access under the
//! three TCC-protected home directories. If any code path touches them anyway,
//! the kernel refuses with `EPERM` silently — the TCC subsystem is never
//! consulted, so no prompt can appear. Everything else is permitted
//! (`allow default`), so the processes keep full functionality.
//!
//! The deny is silent (no `(with send-signal SIGKILL)`): a production process
//! must survive a stray access, losing only that one read. The SIGKILL variant
//! lives solely in `tests/tcc_sandbox.sh`, where dying is how the regression
//! test *detects* an access.

use std::path::Path;
use std::process::Command;

/// Absolute path to the system `sandbox-exec`. Hard-coded rather than resolved
/// via `PATH` so the LaunchAgent invocation never depends on the environment.
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// The macOS "magic" home subdirectories whose mere enumeration trips the TCC
/// privacy prompt (#356).
const TCC_PROTECTED_SUBDIRS: [&str; 3] = ["Documents", "Desktop", "Downloads"];

/// Build the inline Seatbelt (SBPL) profile that denies all file access under
/// the three TCC-protected home directories while allowing everything else.
///
/// Returns `None` when the home directory cannot be resolved (the caller then
/// falls back to an unwrapped invocation). The home path is canonicalized: the
/// kernel matches sandbox `subpath` filters against the *canonical* path, so a
/// symlinked home would otherwise make the deny rule silently miss. This runs
/// in the CLI/setup context (which holds the TCC grant) and only stats the home
/// directory itself — never a protected subdir — so it cannot trip the prompt.
pub fn tcc_deny_profile() -> Option<String> {
    let home = dirs::home_dir()?;
    let home = std::fs::canonicalize(&home).unwrap_or(home);
    Some(build_profile(&home))
}

/// Assemble the single-line SBPL profile for a concrete home directory.
fn build_profile(home: &Path) -> String {
    let mut subpaths = String::new();
    for sub in TCC_PROTECTED_SUBDIRS {
        let p = home.join(sub);
        subpaths.push_str(&format!(
            " (subpath \"{}\")",
            sbpl_escape(&p.to_string_lossy())
        ));
    }
    // SBPL evaluates last-match-wins, so the deny must follow the allow-default.
    format!("(version 1) (allow default) (deny file-read* file-write*{subpaths})")
}

/// Escape a path for embedding inside an SBPL double-quoted string literal.
/// SBPL uses C-style escaping, so backslashes and double quotes must be escaped.
fn sbpl_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Wrap launchd `ProgramArguments` so the spawned process runs under the
/// deny-`~/Documents` Seatbelt sandbox (#356).
///
/// Returns `[SANDBOX_EXEC, "-p", <profile>, binary, args…]` when `sandbox-exec`
/// is present and accepts the generated profile (smoke-tested against
/// `/usr/bin/true`, so a malformed profile can never wedge the LaunchAgent in a
/// `KeepAlive` crash-loop). Otherwise returns the plain `[binary, args…]`: the
/// binary always launches, with the path guards as the remaining safety layer.
pub fn wrap_launchd_args(binary: &str, args: &[&str]) -> Vec<String> {
    if let Some(profile) = tcc_deny_profile() {
        if sandbox_exec_usable(&profile) {
            let mut wrapped = vec![
                SANDBOX_EXEC.to_string(),
                "-p".to_string(),
                profile,
                binary.to_string(),
            ];
            wrapped.extend(args.iter().map(|a| (*a).to_string()));
            return wrapped;
        }
    }
    unwrapped_args(binary, args)
}

/// Plain, unwrapped invocation `[binary, args…]` used as the safe fallback.
fn unwrapped_args(binary: &str, args: &[&str]) -> Vec<String> {
    let mut out = vec![binary.to_string()];
    out.extend(args.iter().map(|a| (*a).to_string()));
    out
}

/// `true` if `sandbox-exec` exists and successfully runs a no-op under
/// `profile`. Guards against both a missing binary and an SBPL syntax error,
/// either of which would otherwise turn a `KeepAlive` LaunchAgent into a
/// crash-loop.
fn sandbox_exec_usable(profile: &str) -> bool {
    if !Path::new(SANDBOX_EXEC).exists() {
        return false;
    }
    Command::new(SANDBOX_EXEC)
        .args(["-p", profile, "/usr/bin/true"])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Render a ProgramArguments list as XML-escaped plist `<string>` lines, each
/// prefixed with `indent` and joined by newlines — ready to drop inside the
/// `<array>` body of a LaunchAgent plist.
pub fn program_args_xml(args: &[String], indent: &str) -> String {
    args.iter()
        .map(|a| format!("{indent}<string>{}</string>", xml_escape(a)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Minimal XML escaping for plist `<string>` bodies: `&`, `<` and `>` must be
/// encoded so the plist stays well-formed. Double quotes are valid in element
/// content and are left as-is (launchd's parser hands them through verbatim).
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_allows_default_and_denies_magic_dirs() {
        let profile = build_profile(Path::new("/Users/dev"));
        assert!(profile.contains("(version 1)"));
        assert!(profile.contains("(allow default)"));
        assert!(profile.contains("(deny file-read* file-write*"));
        assert!(profile.contains("(subpath \"/Users/dev/Documents\")"));
        assert!(profile.contains("(subpath \"/Users/dev/Desktop\")"));
        assert!(profile.contains("(subpath \"/Users/dev/Downloads\")"));
    }

    #[test]
    fn profile_is_single_line_and_deny_follows_allow() {
        // Last-match-wins SBPL: the deny must come after allow-default.
        let profile = build_profile(Path::new("/Users/dev"));
        assert!(!profile.contains('\n'));
        let allow_idx = profile.find("(allow default)").unwrap();
        let deny_idx = profile.find("(deny file-read*").unwrap();
        assert!(allow_idx < deny_idx);
    }

    #[test]
    fn tcc_deny_profile_names_all_three_dirs() {
        if let Some(profile) = tcc_deny_profile() {
            assert!(profile.contains("/Documents\")"));
            assert!(profile.contains("/Desktop\")"));
            assert!(profile.contains("/Downloads\")"));
        }
    }

    #[test]
    fn sbpl_escape_handles_quotes_and_backslashes() {
        assert_eq!(sbpl_escape(r#"/Users/a"b"#), r#"/Users/a\"b"#);
        assert_eq!(sbpl_escape(r"/Users/a\b"), r"/Users/a\\b");
        assert_eq!(sbpl_escape("/Users/normal"), "/Users/normal");
    }

    #[test]
    fn unwrapped_args_prepend_binary() {
        let got = unwrapped_args("/bin/lean-ctx", &["serve", "--_foreground-daemon"]);
        assert_eq!(got, vec!["/bin/lean-ctx", "serve", "--_foreground-daemon"]);
    }

    #[test]
    fn program_args_xml_escapes_and_indents() {
        let args = vec![
            "/usr/bin/sandbox-exec".to_string(),
            "-p".to_string(),
            "(deny a&b<c>)".to_string(),
        ];
        let xml = program_args_xml(&args, "        ");
        assert!(xml.contains("        <string>/usr/bin/sandbox-exec</string>"));
        assert!(xml.contains("&amp;"));
        assert!(xml.contains("&lt;"));
        assert!(xml.contains("&gt;"));
        // No raw ampersand may survive — that would break the plist XML.
        assert!(!xml.contains("a&b"));
    }

    #[test]
    fn wrap_includes_sandbox_exec_and_binary_when_usable() {
        // Assert the real wrapping only when sandbox-exec works on this host
        // (CI macOS runners have it); otherwise verify the safe fallback.
        let wrapped = wrap_launchd_args("/bin/lean-ctx", &["proxy", "start"]);
        if wrapped.first().map(String::as_str) == Some(SANDBOX_EXEC) {
            assert_eq!(wrapped[1], "-p");
            assert!(wrapped[2].contains("(deny file-read*"));
            assert_eq!(wrapped[3], "/bin/lean-ctx");
            assert_eq!(&wrapped[4..], &["proxy", "start"]);
        } else {
            assert_eq!(wrapped, vec!["/bin/lean-ctx", "proxy", "start"]);
        }
    }
}
