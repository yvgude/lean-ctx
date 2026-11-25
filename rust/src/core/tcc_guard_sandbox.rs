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
//! hard, kernel-enforced backstop: at start-up a launchd-standalone process
//! re-execs itself under `sandbox-exec` with a profile that *denies* file access
//! under the three TCC-protected home directories
//! ([`reexec_under_seatbelt_if_needed`]). If any code path touches them anyway,
//! the kernel refuses with `EPERM` silently — the TCC subsystem is never
//! consulted, so no prompt can appear. Everything else is permitted
//! (`allow default`), so the processes keep full functionality.
//!
//! The LaunchAgent plists name the lean-ctx binary itself as
//! `ProgramArguments[0]` (rc2-2): macOS lists a login item by that executable,
//! so a `sandbox-exec` prefix there made Login Items and the "can run in the
//! background" notice say "sandbox-exec" instead of LeanCTX.
//!
//! The deny is silent (no `(with send-signal SIGKILL)`): a production process
//! must survive a stray access, losing only that one read. The SIGKILL variant
//! lives solely in `tests/tcc_sandbox.sh`, where dying is how the regression
//! test *detects* an access.

use std::path::Path;
use std::process::Command;

/// Absolute path to the system `sandbox-exec`. Hard-coded rather than resolved
/// via `PATH` so the seatbelt re-exec never depends on the environment.
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// The macOS "magic" home subdirectories whose mere enumeration trips the TCC
/// privacy prompt (#356).
const TCC_PROTECTED_SUBDIRS: [&str; 3] = ["Documents", "Desktop", "Downloads"];

/// Env sentinel marking that this process is already running under the
/// deny-`~/Documents` seatbelt. Set by the self re-exec
/// ([`reexec_under_seatbelt_if_needed`]) and inherited by its children, so the
/// re-exec can never loop. The LaunchAgent plists deliberately do *not* carry
/// it: its absence is what makes a launchd-started process re-exec itself.
pub const SEATBELT_SENTINEL: &str = "LEAN_CTX_SEATBELT";

/// Bundle identifier of the LeanCTX desktop app. LaunchAgent plists name it in
/// `AssociatedBundleIdentifiers` so macOS attributes the background items to
/// LeanCTX in System Settings → Login Items (rc2-2).
pub const APP_BUNDLE_ID: &str = "com.leanctx.workspace";

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

/// launchd `ProgramArguments` for a lean-ctx LaunchAgent: the plain
/// `[binary, args…]`. The binary must come first so macOS names the login item
/// after lean-ctx (rc2-2); the deny-`~/Documents` seatbelt (#356) is applied by
/// the process itself at start-up ([`reexec_under_seatbelt_if_needed`]).
pub fn launchd_program_args(binary: &str, args: &[&str]) -> Vec<String> {
    let mut out = vec![binary.to_string()];
    out.extend(args.iter().map(|a| (*a).to_string()));
    out
}

/// Pure decision: should a launchd-standalone process re-exec itself under the
/// seatbelt? Only when it is its own TCC identity *and* not already marked.
/// Factored out so the policy is unit-testable without spawning processes.
#[cfg(target_os = "macos")]
fn should_reexec_under_seatbelt(standalone: bool, sentinel_present: bool) -> bool {
    standalone && !sentinel_present
}

/// The #356 seatbelt: if this is a launchd-standalone process (daemon / proxy /
/// auto-updater — `ppid 1`, its own TCC identity) that is not already running
/// under the seatbelt, re-exec self wrapped in `sandbox-exec` with the
/// deny-`~/Documents` profile. The LaunchAgent plists start the binary
/// directly (see [`launchd_program_args`]), so this is the one place the
/// seatbelt is applied; it also covers plists from any older release.
///
/// No-op unless macOS + standalone + sentinel-absent + `sandbox-exec` accepts
/// the profile. On `exec` failure it returns and the caller continues unwrapped
/// (the [`crate::core::pathutil`] path guards remain the safety net) rather than
/// aborting a `KeepAlive` daemon into a crash loop.
#[cfg(target_os = "macos")]
pub fn reexec_under_seatbelt_if_needed() {
    use std::os::unix::process::CommandExt;

    let sentinel = std::env::var_os(SEATBELT_SENTINEL).is_some();
    let standalone = crate::core::pathutil::process_is_tcc_standalone();
    if !should_reexec_under_seatbelt(standalone, sentinel) {
        return;
    }
    let Some(profile) = tcc_deny_profile() else {
        return;
    };
    if !sandbox_exec_usable(&profile) {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `exec` replaces the image; it only returns on failure.
    let err = Command::new(SANDBOX_EXEC)
        .arg("-p")
        .arg(&profile)
        .arg(&exe)
        .args(&args)
        .env(SEATBELT_SENTINEL, "1")
        .exec();
    tracing::warn!("#356 seatbelt re-exec failed, continuing unwrapped: {err}");
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

/// Build the `EnvironmentVariables` plist block that pins a launchd-spawned
/// lean-ctx process to the directory layout the *installing CLI* resolves (#449).
///
/// A LaunchAgent inherits only launchd's minimal environment (no `HOME`, no XDG
/// vars), so the proxy/daemon would otherwise resolve a *different* config/data
/// dir than the CLI that installed it: it never sees the user's `config.toml`
/// edits (the live-upstream reload reads an empty/foreign config) and derives a
/// mismatched session token. Baking the exact resolved dirs into the plist makes
/// the managed process always agree with the CLI, on every platform layout
/// (legacy `~/.lean-ctx`, mixed, or split XDG).
///
/// Returns the full `<key>EnvironmentVariables</key><dict>…</dict>` block
/// (4-space indented, trailing newline) or an empty string when nothing resolves.
pub fn pinned_layout_env_xml() -> String {
    let home = dirs::home_dir();
    // No #356 sentinel here: the launchd-started process must re-exec itself
    // under the seatbelt, which only happens while the sentinel is absent.
    without_seatbelt_sentinel(&layout_env_xml(
        home.as_deref(),
        &[
            ("LEAN_CTX_CONFIG_DIR", crate::core::paths::config_dir().ok()),
            ("LEAN_CTX_DATA_DIR", crate::core::paths::data_dir().ok()),
            ("LEAN_CTX_STATE_DIR", crate::core::paths::state_dir().ok()),
            ("LEAN_CTX_CACHE_DIR", crate::core::paths::cache_dir().ok()),
        ],
    ))
}

/// [`pinned_layout_env_xml`] as a pure function of the resolved layout, in the
/// shape a #356 `sandbox-exec`-wrapped plist carries it: the #356 sentinel
/// first, then `HOME`, then each `(key, dir)` in order; a `None` dir is left
/// out. The installers write it without the sentinel (rc2-2: they no longer
/// wrap); the re-pin keeps or restores the sentinel only in a plist that still
/// runs under `sandbox-exec` (see [`repin_layout_env`]).
pub fn layout_env_xml(home: Option<&Path>, dirs: &[(&str, Option<std::path::PathBuf>)]) -> String {
    let mut entries: Vec<(&str, String)> = Vec::new();
    // #356 sentinel: marks the spawned process as already running under the
    // seatbelt wrapper, so `reexec_under_seatbelt_if_needed` is a no-op and a
    // wrapped plist never double-wraps.
    entries.push((SEATBELT_SENTINEL, "1".to_string()));
    if let Some(home) = home {
        entries.push(("HOME", home.display().to_string()));
    }
    for (key, dir) in dirs {
        if let Some(p) = dir {
            entries.push((key, p.display().to_string()));
        }
    }
    let body = entries
        .iter()
        .map(|(k, v)| env_entry_xml(k, v))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{ENV_BLOCK_KEY}    <dict>\n{body}{ENV_BLOCK_END}")
}

/// One `<key>…</key><string>…</string>` pair of the pinned block.
fn env_entry_xml(key: &str, value: &str) -> String {
    format!(
        "        <key>{key}</key>\n        <string>{}</string>",
        xml_escape(value)
    )
}

const ENV_BLOCK_KEY: &str = "    <key>EnvironmentVariables</key>\n";
const ENV_BLOCK_END: &str = "\n    </dict>\n";
const ENV_BLOCK_ANCHOR: &str = "    <key>RunAtLoad</key>";

/// `true` when the plist's `ProgramArguments` start with [`SANDBOX_EXEC`], i.e.
/// launchd spawns the job inside the seatbelt the pre-rc2-2 installers rendered.
/// Only such a plist may carry the #356 sentinel: on any other (pre-#356, or a
/// `brew upgrade` install whose plist was never re-rendered) the sentinel would
/// suppress [`reexec_under_seatbelt_if_needed`] and leave the job unsandboxed.
pub fn plist_runs_under_seatbelt(plist: &str) -> bool {
    let Some(args) = plist
        .find("<key>ProgramArguments</key>")
        .map(|at| &plist[at..])
    else {
        return false;
    };
    let Some(array) = args.find("<array>").map(|at| &args[at..]) else {
        return false;
    };
    let array = array.find("</array>").map_or(array, |end| &array[..end]);
    array
        .split_once("<string>")
        .and_then(|(_, rest)| rest.split_once("</string>"))
        .is_some_and(|(first, _)| first.trim() == SANDBOX_EXEC)
}

/// Replace the pinned layout `EnvironmentVariables` block of a lean-ctx
/// LaunchAgent plist with `env_block` (from [`pinned_layout_env_xml`]) and leave
/// every other byte as it is. A stale pin is replaced, a key missing from
/// `env_block` is dropped, and a plist without a block gets it before
/// `RunAtLoad`, where the installers render it. The #356 sentinel is carried
/// (kept, or restored once) only when the plist itself runs under
/// `sandbox-exec` ([`plist_runs_under_seatbelt`]); otherwise it is dropped (and
/// a block left with no entry is removed), so the process re-execs under the
/// seatbelt at start-up as designed. Pure and idempotent: the same layout returns the input
/// unchanged. `None` when the plist is not in the shape lean-ctx writes; the
/// caller then leaves the file alone.
pub fn repin_layout_env(plist: &str, env_block: &str) -> Option<String> {
    let (head, tail) = if let Some(start) = plist.find(ENV_BLOCK_KEY) {
        let end = start + plist[start..].find(ENV_BLOCK_END)? + ENV_BLOCK_END.len();
        (&plist[..start], &plist[end..])
    } else {
        let at = plist.find(ENV_BLOCK_ANCHOR)?;
        (&plist[..at], &plist[at..])
    };
    let block = if plist_runs_under_seatbelt(plist) {
        with_seatbelt_sentinel(env_block)
    } else {
        without_seatbelt_sentinel(env_block)
    };
    Some(format!("{head}{block}{tail}"))
}

/// `env_block` without its #356 sentinel entry; `""` when nothing else is left.
fn without_seatbelt_sentinel(env_block: &str) -> String {
    let sentinel = env_entry_xml(SEATBELT_SENTINEL, "1");
    // Every entry is followed by a newline (the next entry or `ENV_BLOCK_END`).
    let stripped = env_block.replacen(&format!("{sentinel}\n"), "", 1);
    if stripped == format!("{ENV_BLOCK_KEY}    <dict>{ENV_BLOCK_END}") {
        String::new()
    } else {
        stripped
    }
}

/// `env_block` with exactly one #356 sentinel entry, first in the dict as
/// [`layout_env_xml`] renders it; a sentinel-only block when `env_block` is
/// empty. A block of any other shape is returned without the sentinel.
fn with_seatbelt_sentinel(env_block: &str) -> String {
    let stripped = without_seatbelt_sentinel(env_block);
    let head = format!("{ENV_BLOCK_KEY}    <dict>\n");
    let sentinel = env_entry_xml(SEATBELT_SENTINEL, "1");
    if stripped.is_empty() {
        return format!("{head}{sentinel}{ENV_BLOCK_END}");
    }
    match stripped.strip_prefix(&head) {
        Some(rest) => format!("{head}{sentinel}\n{rest}"),
        None => stripped,
    }
}

/// Re-pin the LaunchAgent at `path` to `env_block` (see [`repin_layout_env`]).
/// The file is rewritten (temp file + rename) only when its pin differs.
/// `Ok(true)`: rewritten, the job must be re-bootstrapped to see it;
/// `Ok(false)`: already current, or not a plist lean-ctx can re-pin.
pub fn repin_launchagent_file(path: &Path, env_block: &str) -> std::io::Result<bool> {
    let current = std::fs::read_to_string(path)?;
    let Some(next) = repin_layout_env(&current, env_block) else {
        return Ok(false);
    };
    if next == current {
        return Ok(false);
    }
    let tmp = path.with_extension("plist.repin");
    std::fs::write(&tmp, &next)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(true)
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

/// The `AssociatedBundleIdentifiers` plist block naming the LeanCTX app
/// ([`APP_BUNDLE_ID`]), with `indent` before the key and twice `indent` before
/// the array entry, ending in a newline.
pub fn associated_bundle_ids_xml(indent: &str) -> String {
    format!(
        "{indent}<key>AssociatedBundleIdentifiers</key>\n{indent}<array>\n{indent}{indent}<string>{APP_BUNDLE_ID}</string>\n{indent}</array>\n"
    )
}

/// Minimal XML escaping for plist `<string>` bodies: `&`, `<` and `>` must be
/// encoded so the plist stays well-formed. Double quotes are valid in element
/// content and are left as-is (launchd's parser hands them through verbatim).
pub fn xml_escape(s: &str) -> String {
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
    fn launchd_program_args_start_with_the_binary() {
        // rc2-2: macOS names the login item after ProgramArguments[0].
        let got = launchd_program_args("/bin/lean-ctx", &["serve", "--_foreground-daemon"]);
        assert_eq!(got, vec!["/bin/lean-ctx", "serve", "--_foreground-daemon"]);
    }

    #[test]
    fn program_args_xml_escapes_and_indents() {
        let args = vec![
            "/Applications/Lean CTX/lean-ctx".to_string(),
            "--x".to_string(),
            "a&b<c>".to_string(),
        ];
        let xml = program_args_xml(&args, "        ");
        assert!(xml.contains("        <string>/Applications/Lean CTX/lean-ctx</string>"));
        assert!(xml.contains("&amp;"));
        assert!(xml.contains("&lt;"));
        assert!(xml.contains("&gt;"));
        // No raw ampersand may survive — that would break the plist XML.
        assert!(!xml.contains("a&b"));
    }

    #[test]
    fn pinned_layout_env_pins_all_categories() {
        let xml = pinned_layout_env_xml();
        assert!(xml.starts_with("    <key>EnvironmentVariables</key>"));
        assert!(xml.contains("<dict>"));
        assert!(xml.trim_end().ends_with("</dict>"));
        for key in [
            "LEAN_CTX_CONFIG_DIR",
            "LEAN_CTX_DATA_DIR",
            "LEAN_CTX_STATE_DIR",
            "LEAN_CTX_CACHE_DIR",
        ] {
            assert!(xml.contains(&format!("<key>{key}</key>")), "missing {key}");
        }
    }

    #[test]
    fn pinned_layout_env_omits_seatbelt_sentinel() {
        // The plist must NOT mark the process: the sentinel's absence is what
        // makes a launchd-started process re-exec itself under the seatbelt.
        let xml = pinned_layout_env_xml();
        assert!(!xml.contains(SEATBELT_SENTINEL));
    }

    #[test]
    fn associated_bundle_ids_name_the_app() {
        let xml = associated_bundle_ids_xml("    ");
        assert!(xml.starts_with("    <key>AssociatedBundleIdentifiers</key>\n    <array>"));
        assert!(xml.contains("        <string>com.leanctx.workspace</string>"));
        assert!(xml.ends_with("    </array>\n"));
    }

    /// A daemon plist shaped like the installer renders it, pinned to
    /// `env_block`; `wrapped` runs it under `sandbox-exec` as #356 installs do.
    fn sample_plist(wrapped: bool, env_block: &str) -> String {
        let mut args = Vec::new();
        if wrapped {
            args.push(SANDBOX_EXEC.to_string());
            args.push("-p".to_string());
            args.push(build_profile(Path::new("/Users/u")));
        }
        args.extend(["/opt/lean-ctx", "serve", "--_foreground-daemon"].map(String::from));
        let program_args = program_args_xml(&args, "        ");
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n    <key>Label</key>\n    <string>com.leanctx.test</string>\n    <key>ProgramArguments</key>\n    <array>\n{program_args}\n    </array>\n{env_block}    <key>RunAtLoad</key>\n    <true/>\n    <key>KeepAlive</key>\n    <dict>\n        <key>SuccessfulExit</key>\n        <false/>\n    </dict>\n    <key>StandardOutPath</key>\n    <string>/old/state/logs/out.log</string>\n</dict>\n</plist>\n"
        )
    }

    fn sentinels(plist: &str) -> usize {
        plist
            .matches(&format!("<key>{SEATBELT_SENTINEL}</key>"))
            .count()
    }

    fn layout(state: Option<&str>, data: Option<&str>) -> String {
        layout_env_xml(
            Some(Path::new("/Users/u")),
            &[
                (
                    "LEAN_CTX_CONFIG_DIR",
                    Some("/Users/u/.config/lean-ctx".into()),
                ),
                ("LEAN_CTX_DATA_DIR", data.map(Into::into)),
                ("LEAN_CTX_STATE_DIR", state.map(Into::into)),
                (
                    "LEAN_CTX_CACHE_DIR",
                    Some("/Users/u/.cache/lean-ctx".into()),
                ),
            ],
        )
    }

    #[test]
    fn repin_replaces_changed_state_and_data_pins() {
        let old = sample_plist(true, &layout(Some("/old/state"), Some("/old/data")));
        let fresh = layout(Some("/new/state"), Some("/new/data"));
        let got = repin_layout_env(&old, &fresh).expect("lean-ctx plist shape");
        assert_eq!(got, sample_plist(true, &fresh));
        assert!(got.contains("<string>/new/state</string>"));
        assert!(got.contains("<string>/new/data</string>"));
        assert!(!got.contains("<string>/old/state</string>"));
        assert!(!got.contains("<string>/old/data</string>"));
        // Every other key is untouched, including the log path.
        assert!(got.contains("<string>/old/state/logs/out.log</string>"));
        assert_eq!(got.matches("<key>EnvironmentVariables</key>").count(), 1);
    }

    #[test]
    fn repin_sets_the_seatbelt_sentinel_only_under_sandbox_exec() {
        // #356 (bd M1 on !659): the sentinel tells the process it already runs
        // in the seatbelt and suppresses the start-up re-exec. A re-pin may
        // carry it only into a plist whose ProgramArguments run sandbox-exec.
        let fresh = layout(Some("/new/state"), Some("/d"));
        let pre_sentinel = without_seatbelt_sentinel(&layout(Some("/old/state"), Some("/d")));
        assert_eq!(sentinels(&pre_sentinel), 0);

        // Not wrapped (pre-#356 or brew-upgraded plist), with or without a
        // block: never gains the sentinel, so the re-exec path stays open.
        for old in [sample_plist(false, &pre_sentinel), sample_plist(false, "")] {
            assert!(!plist_runs_under_seatbelt(&old));
            let got = repin_layout_env(&old, &fresh).expect("lean-ctx plist shape");
            assert_eq!(sentinels(&got), 0);
            assert!(got.contains("<string>/new/state</string>"));
        }
        // Not wrapped but carrying the sentinel: dropped (the safer direction).
        let stale = sample_plist(false, &layout(Some("/old/state"), Some("/d")));
        let got = repin_layout_env(&stale, &fresh).expect("lean-ctx plist shape");
        assert_eq!(sentinels(&got), 0);
        assert_eq!(got, sample_plist(false, &without_seatbelt_sentinel(&fresh)));

        // Wrapped: kept exactly once, and restored once if the block lost it.
        for old in [
            sample_plist(true, &layout(Some("/old/state"), Some("/d"))),
            sample_plist(true, &pre_sentinel),
        ] {
            assert!(plist_runs_under_seatbelt(&old));
            let got = repin_layout_env(&old, &fresh).expect("lean-ctx plist shape");
            assert_eq!(sentinels(&got), 1);
            assert_eq!(got, sample_plist(true, &fresh));

            // The installers' block carries no sentinel since rc2-2
            // (`pinned_layout_env_xml`); a wrapped plist still gets it once.
            let installer_block = without_seatbelt_sentinel(&fresh);
            let got = repin_layout_env(&old, &installer_block).expect("lean-ctx plist shape");
            assert_eq!(got, sample_plist(true, &fresh));
        }
    }

    #[test]
    fn repin_drops_the_pin_of_an_unset_var() {
        let old = sample_plist(true, &layout(Some("/old/state"), Some("/old/data")));
        let fresh = layout(None, Some("/old/data"));
        let got = repin_layout_env(&old, &fresh).expect("lean-ctx plist shape");
        assert!(!got.contains("LEAN_CTX_STATE_DIR"));
        assert_eq!(got, sample_plist(true, &fresh));
        // Nothing resolved at all: a wrapped plist keeps only the #356
        // sentinel, an unwrapped one loses the whole block.
        let empty = layout_env_xml(None, &[]);
        let bare = repin_layout_env(&old, &empty).expect("lean-ctx plist shape");
        assert_eq!(bare, sample_plist(true, &empty));
        assert_eq!(sentinels(&bare), 1);
        assert!(!bare.contains("<key>HOME</key>"));
        assert!(!bare.contains("LEAN_CTX_DATA_DIR"));
        let unwrapped = sample_plist(false, &layout(Some("/s"), Some("/d")));
        let none = repin_layout_env(&unwrapped, &empty).expect("lean-ctx plist shape");
        assert_eq!(none, sample_plist(false, ""));
    }

    #[test]
    fn repin_is_idempotent_and_adds_a_missing_block() {
        let env = layout(Some("/s"), Some("/d"));
        // A fresh wrapped install re-pinned to the same layout: byte-identical.
        let pinned = sample_plist(true, &env);
        assert_eq!(
            repin_layout_env(&pinned, &env).as_deref(),
            Some(pinned.as_str())
        );
        // A pre-#449 plist without a block gets it before RunAtLoad.
        let added = repin_layout_env(&sample_plist(true, ""), &env).expect("anchor");
        assert_eq!(added, pinned);
        // Not a plist lean-ctx renders: left alone.
        assert_eq!(repin_layout_env("<plist/>", &env), None);
    }

    #[test]
    fn repin_launchagent_file_rewrites_only_a_stale_pin() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("com.leanctx.test.plist");
        let fresh = layout(Some("/new/state"), Some("/d"));
        std::fs::write(
            &path,
            sample_plist(true, &layout(Some("/old/state"), Some("/d"))),
        )
        .expect("write");
        assert!(repin_launchagent_file(&path, &fresh).expect("repin"));
        let first = std::fs::read(&path).expect("read");
        assert_eq!(first, sample_plist(true, &fresh).into_bytes());
        // Unchanged layout: no rewrite, byte-identical file, no temp left.
        assert!(!repin_launchagent_file(&path, &fresh).expect("repin"));
        assert_eq!(std::fs::read(&path).expect("read"), first);
        assert_eq!(std::fs::read_dir(dir.path()).expect("ls").count(), 1);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn reexec_policy_only_fires_for_unmarked_standalone() {
        // Standalone + no sentinel = launchd start → must re-exec under seatbelt.
        assert!(should_reexec_under_seatbelt(true, false));
        // Already marked by a prior re-exec → never loop.
        assert!(!should_reexec_under_seatbelt(true, true));
        // Terminal/editor child (host TCC grant) → never re-exec.
        assert!(!should_reexec_under_seatbelt(false, false));
        assert!(!should_reexec_under_seatbelt(false, true));
    }
}
