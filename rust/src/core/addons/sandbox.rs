//! Opt-in OS sandbox for the stdio MCP servers an addon spawns (#865).
//!
//! A stdio addon is a child process with the user's full privileges. When
//! `addons.sandbox` is enabled, lean-ctx wraps that child in an OS-native
//! sandbox launcher before spawning it (the single spawn point is
//! [`crate::core::gateway::client`]):
//!
//! - **macOS** → `sandbox-exec` with a generated SBPL profile,
//! - **Linux** → `bwrap` (bubblewrap) with a read-only root + network unshare.
//!
//! Local stdio tools rarely need the network, so the highest-value, lowest-
//! breakage control is **outbound-network isolation** (`auto`); `strict` also
//! makes the filesystem read-only except a scratch tmp and **refuses to spawn**
//! if no launcher is available (fail-closed). Default is [`SandboxMode::Off`]
//! → zero behavioural change. The argv-building is pure + unit-tested; the
//! enforcement is delegated to the OS launcher.
//!
//! The OS sandbox enforces two dimensions — outbound network and filesystem
//! writes — and child processes **inherit** the profile, so any subprocess an
//! addon spawns is bound by the same network/filesystem restrictions. The
//! declared `exec` capability is therefore *not* an OS control here: it is
//! disclosed, audited and surfaced for consent (see [`super::capabilities`] /
//! [`super::audit`]), while the data-safety guarantees come from the inherited
//! network/filesystem profile. Path-allowlisting `execve` is also not portable
//! (`bwrap`/seccomp cannot do it) and breaks interpreted servers (the
//! interpreter chain is itself a `process-exec`), so lean-ctx does not attempt
//! it.

use std::path::Path;

use super::capabilities::AddonCapabilities;

/// The two enforceable dimensions of an OS sandbox profile. Both the legacy
/// global [`SandboxMode`] and a per-addon [`AddonCapabilities`] declaration are
/// projected onto these, so one set of pure profile builders serves both paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dims {
    /// Allow outbound network when `true`; otherwise the sandbox blocks egress.
    pub network_allowed: bool,
    /// Allow filesystem writes when `true`; otherwise read-only (+ scratch tmp).
    pub fs_writable: bool,
}

impl Dims {
    /// Nothing left to enforce at the OS level (everything is permitted).
    #[must_use]
    fn is_noop(self) -> bool {
        self.network_allowed && self.fs_writable
    }
}

/// Project a legacy [`SandboxMode`] onto sandbox [`Dims`]. `Off` is permissive
/// (callers short-circuit before wrapping); `Auto` blocks network; `Strict`
/// also makes the filesystem read-only.
#[must_use]
fn dims_for_mode(mode: SandboxMode) -> Dims {
    match mode {
        SandboxMode::Off => Dims {
            network_allowed: true,
            fs_writable: true,
        },
        SandboxMode::Auto => Dims {
            network_allowed: false,
            fs_writable: true,
        },
        SandboxMode::Strict => Dims {
            network_allowed: false,
            fs_writable: false,
        },
    }
}

/// Project declared [`AddonCapabilities`] onto sandbox [`Dims`].
#[must_use]
fn dims_for_caps(caps: &AddonCapabilities) -> Dims {
    Dims {
        network_allowed: caps.network_allowed(),
        fs_writable: caps.filesystem_writable(),
    }
}

/// How aggressively to sandbox a spawned stdio server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SandboxMode {
    /// No sandbox — spawn the command directly (default).
    #[default]
    Off,
    /// Best-effort: wrap if a launcher exists, else run directly with a warning.
    /// Blocks outbound network.
    Auto,
    /// Network blocked + read-only filesystem; **refuses** to spawn if no
    /// launcher is available.
    Strict,
}

impl SandboxMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Auto => "auto",
            Self::Strict => "strict",
        }
    }

    /// Parse from config text; unknown / empty → [`Self::Off`].
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Self::Auto,
            "strict" => Self::Strict,
            _ => Self::Off,
        }
    }
}

/// An OS sandbox launcher available on this host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launcher {
    /// macOS `sandbox-exec` (SBPL profile via `-p`).
    SandboxExec,
    /// Linux `bwrap` (bubblewrap).
    Bwrap,
}

/// What to do for a given (mode, launcher) pair — pure, so it is fully tested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Spawn the command unchanged.
    Direct,
    /// Wrap the command with `launcher`.
    Wrap(Launcher),
    /// Refuse to spawn (strict mode, no launcher). Carries the reason.
    Refuse(String),
}

/// Decide the plan for `mode` given whether a launcher was detected. Pure.
#[must_use]
pub fn plan(mode: SandboxMode, launcher: Option<Launcher>) -> Plan {
    match (mode, launcher) {
        (SandboxMode::Off, _) | (SandboxMode::Auto, None) => Plan::Direct,
        (_, Some(l)) => Plan::Wrap(l),
        (SandboxMode::Strict, None) => Plan::Refuse(
            "addons.sandbox = strict but no OS sandbox launcher (sandbox-exec / bwrap) is available"
                .to_string(),
        ),
    }
}

/// Detect an available launcher for the current OS, or `None`.
#[must_use]
pub fn detect_launcher() -> Option<Launcher> {
    if cfg!(target_os = "macos") && which("sandbox-exec") {
        Some(Launcher::SandboxExec)
    } else if cfg!(target_os = "linux") && which("bwrap") {
        Some(Launcher::Bwrap)
    } else {
        None
    }
}

/// Build the final `(command, args)` for a [`Plan::Wrap`], prefixing the
/// original invocation with the launcher + a profile derived from `mode`. Pure.
#[must_use]
pub fn wrap_argv(
    launcher: Launcher,
    mode: SandboxMode,
    command: &str,
    args: &[String],
) -> (String, Vec<String>) {
    wrap_argv_dims(launcher, dims_for_mode(mode), command, args)
}

/// Build the final `(command, args)` for a [`Plan::Wrap`] from explicit
/// [`Dims`] (network + filesystem). The OS sandbox enforces exactly these two
/// dimensions; child processes inherit the profile, so a subprocess the addon
/// spawns is bound by the same network/filesystem restrictions. Pure.
#[must_use]
fn wrap_argv_dims(
    launcher: Launcher,
    dims: Dims,
    command: &str,
    args: &[String],
) -> (String, Vec<String>) {
    match launcher {
        Launcher::SandboxExec => {
            let profile = sbpl_profile_dims(dims);
            let mut v = vec!["-p".to_string(), profile, command.to_string()];
            v.extend(args.iter().cloned());
            ("sandbox-exec".to_string(), v)
        }
        Launcher::Bwrap => {
            let mut v = bwrap_flags_dims(dims);
            v.push(command.to_string());
            v.extend(args.iter().cloned());
            ("bwrap".to_string(), v)
        }
    }
}

/// macOS SBPL profile for `mode` (test-only wrapper over [`sbpl_profile_dims`];
/// the runtime path goes through [`wrap_argv`] → [`wrap_argv_dims`]).
#[cfg(test)]
fn sbpl_profile(mode: SandboxMode) -> String {
    sbpl_profile_dims(dims_for_mode(mode))
}

/// macOS SBPL profile for explicit [`Dims`]. `allow default` keeps the tool
/// working; the denies are the security wins. Last-match-wins, so the tmp
/// re-allow follows the deny.
fn sbpl_profile_dims(dims: Dims) -> String {
    let mut p = String::from("(version 1)\n(allow default)\n");
    if !dims.network_allowed {
        p.push_str("(deny network*)\n");
    }
    if !dims.fs_writable {
        p.push_str("(deny file-write*)\n");
        p.push_str("(allow file-write* (subpath \"/tmp\") (subpath \"/private/tmp\") (subpath \"/var/folders\"))\n");
    }
    p
}

/// bubblewrap flags for `mode` (test-only wrapper over [`bwrap_flags_dims`];
/// the runtime path goes through [`wrap_argv`] → [`wrap_argv_dims`]).
#[cfg(test)]
fn bwrap_flags(mode: SandboxMode) -> Vec<String> {
    bwrap_flags_dims(dims_for_mode(mode))
}

/// bubblewrap flags for explicit [`Dims`]: unshare the network unless allowed;
/// bind the root read-only (with a writable tmpfs at `/tmp`) unless writable.
fn bwrap_flags_dims(dims: Dims) -> Vec<String> {
    let mut f: Vec<String> = vec!["--die-with-parent".into()];
    if !dims.network_allowed {
        f.push("--unshare-net".into());
    }
    if dims.fs_writable {
        f.extend(
            ["--bind", "/", "/", "--dev", "/dev", "--proc", "/proc"]
                .iter()
                .map(|s| (*s).to_string()),
        );
    } else {
        f.extend(
            [
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--proc",
                "/proc",
                "--tmpfs",
                "/tmp",
            ]
            .iter()
            .map(|s| (*s).to_string()),
        );
    }
    f
}

/// Resolve the configured sandbox mode and rewrite `(command, args)` for the
/// gateway spawn point. Returns the original invocation when sandboxing is off
/// or unavailable in `auto`; an `Err` when `strict` cannot be honoured (the
/// caller must then refuse to spawn). Reads the global-only `[addons]` config.
pub fn apply(command: &str, args: &[String]) -> Result<(String, Vec<String>), String> {
    let mode = crate::core::config::Config::load().addons.sandbox_mode();
    if mode == SandboxMode::Off {
        return Ok((command.to_string(), args.to_vec()));
    }
    match plan(mode, detect_launcher()) {
        Plan::Direct => {
            if mode != SandboxMode::Off {
                tracing::warn!(
                    "addons.sandbox = {} but no OS sandbox launcher is available — \
                     spawning `{command}` UNSANDBOXED",
                    mode.as_str()
                );
            }
            Ok((command.to_string(), args.to_vec()))
        }
        Plan::Wrap(launcher) => {
            tracing::debug!(
                "sandboxing `{command}` via {:?} ({} mode)",
                launcher,
                mode.as_str()
            );
            Ok(wrap_argv(launcher, mode, command, args))
        }
        Plan::Refuse(reason) => Err(reason),
    }
}

/// Resolve the sandbox for a spawn, preferring per-addon declared
/// [`AddonCapabilities`] over the legacy global `addons.sandbox` mode.
///
/// - `Some(caps)` → enforce exactly the declared network + filesystem profile
///   (secure-by-default for the platform/marketplace path); child processes
///   inherit it. `exec` is disclosed/audited, not OS-enforced (see module docs).
///   If the profile restricts anything but no OS launcher is available, fail
///   closed when `addons.enforce_capabilities` is set, otherwise run unsandboxed.
/// - `None` → fall back to [`apply`] (the legacy `addons.sandbox` behaviour), so
///   addons that predate the capability model keep working unchanged.
pub fn apply_for(
    command: &str,
    args: &[String],
    capabilities: Option<&AddonCapabilities>,
) -> Result<(String, Vec<String>), String> {
    match capabilities {
        Some(caps) => apply_caps(command, args, caps),
        None => apply(command, args),
    }
}

/// Enforce a per-addon capability profile at the spawn point. The OS sandbox
/// enforces the network + filesystem dimensions (and child processes inherit
/// them); `exec` is a declared + audited + consented capability, not an OS
/// control — see the module docs for why path-allowlisting `execve` is neither
/// portable nor compatible with interpreted servers. Pure decision +
/// OS-launcher detection; the wrapping argv is unit-tested.
fn apply_caps(
    command: &str,
    args: &[String],
    caps: &AddonCapabilities,
) -> Result<(String, Vec<String>), String> {
    let dims = dims_for_caps(caps);

    // Network + filesystem unrestricted → nothing for the OS sandbox to add
    // (env scrubbing still happens at the spawn point).
    if dims.is_noop() {
        return Ok((command.to_string(), args.to_vec()));
    }

    let enforce = crate::core::config::Config::load()
        .addons
        .enforce_capabilities;

    let Some(launcher) = detect_launcher() else {
        // No OS launcher: fail closed only when the org opted in, else warn.
        if enforce {
            return Err(format!(
                "addons.enforce_capabilities = true but no OS sandbox launcher \
                 (sandbox-exec / bwrap) is available to honour `{command}`'s declared \
                 restricted capabilities"
            ));
        }
        tracing::warn!(
            "addon `{command}` declares restricted capabilities but no OS sandbox \
             launcher is available — running UNSANDBOXED (set \
             addons.enforce_capabilities = true to fail closed)"
        );
        return Ok((command.to_string(), args.to_vec()));
    };

    tracing::debug!(
        "sandboxing `{command}` via {:?} (net={}, fs_write={})",
        launcher,
        dims.network_allowed,
        dims.fs_writable
    );
    Ok(wrap_argv_dims(launcher, dims, command, args))
}

fn which(bin: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let p = dir.join(bin);
        p.is_file() && is_executable(&p)
    })
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(_p: &Path) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parse_roundtrip() {
        assert_eq!(SandboxMode::parse("auto"), SandboxMode::Auto);
        assert_eq!(SandboxMode::parse("STRICT"), SandboxMode::Strict);
        assert_eq!(SandboxMode::parse(""), SandboxMode::Off);
        assert_eq!(SandboxMode::parse("nonsense"), SandboxMode::Off);
        assert_eq!(SandboxMode::Strict.as_str(), "strict");
    }

    #[test]
    fn plan_off_is_always_direct() {
        assert_eq!(plan(SandboxMode::Off, Some(Launcher::Bwrap)), Plan::Direct);
        assert_eq!(plan(SandboxMode::Off, None), Plan::Direct);
    }

    #[test]
    fn plan_auto_without_launcher_runs_direct() {
        assert_eq!(plan(SandboxMode::Auto, None), Plan::Direct);
    }

    #[test]
    fn plan_strict_without_launcher_refuses() {
        assert!(matches!(plan(SandboxMode::Strict, None), Plan::Refuse(_)));
    }

    #[test]
    fn plan_wraps_when_launcher_present() {
        assert_eq!(
            plan(SandboxMode::Auto, Some(Launcher::SandboxExec)),
            Plan::Wrap(Launcher::SandboxExec)
        );
    }

    #[test]
    fn sandbox_exec_argv_prepends_profile_and_command() {
        let (cmd, args) = wrap_argv(
            Launcher::SandboxExec,
            SandboxMode::Auto,
            "my-mcp",
            &["serve".into()],
        );
        assert_eq!(cmd, "sandbox-exec");
        assert_eq!(args[0], "-p");
        assert!(args[1].contains("(deny network*)"));
        assert_eq!(args[2], "my-mcp");
        assert_eq!(args[3], "serve");
    }

    #[test]
    fn strict_sbpl_restricts_writes() {
        let p = sbpl_profile(SandboxMode::Strict);
        assert!(p.contains("(deny file-write*)"));
        assert!(p.contains("/tmp"));
        let auto = sbpl_profile(SandboxMode::Auto);
        assert!(!auto.contains("(deny file-write*)"));
    }

    #[test]
    fn bwrap_argv_unshares_network() {
        let (cmd, args) = wrap_argv(Launcher::Bwrap, SandboxMode::Auto, "my-mcp", &["x".into()]);
        assert_eq!(cmd, "bwrap");
        assert!(args.iter().any(|a| a == "--unshare-net"));
        assert!(args.iter().any(|a| a == "my-mcp"));
        assert!(args.iter().any(|a| a == "x"));
    }

    #[test]
    fn bwrap_strict_is_readonly_root() {
        let (_c, args) = wrap_argv(Launcher::Bwrap, SandboxMode::Strict, "m", &[]);
        assert!(args.iter().any(|a| a == "--ro-bind"));
        assert!(args.iter().any(|a| a == "--tmpfs"));
    }

    // --- capability-derived profiles (P1) ---

    use super::super::capabilities::{
        AddonCapabilities, ExecAccess, FilesystemAccess, NetworkAccess,
    };

    #[test]
    fn minimal_caps_block_network_and_writes() {
        let dims = dims_for_caps(&AddonCapabilities::default());
        assert!(!dims.network_allowed);
        assert!(!dims.fs_writable);
        let sbpl = sbpl_profile_dims(dims);
        assert!(sbpl.contains("(deny network*)"));
        assert!(sbpl.contains("(deny file-write*)"));
    }

    #[test]
    fn full_network_caps_omit_network_deny() {
        let caps = AddonCapabilities {
            network: NetworkAccess::Full,
            filesystem: FilesystemAccess::ReadOnly,
            env: vec![],
            exec: ExecAccess::default(),
        };
        let dims = dims_for_caps(&caps);
        assert!(dims.network_allowed);
        let sbpl = sbpl_profile_dims(dims);
        assert!(!sbpl.contains("(deny network*)"));
        assert!(sbpl.contains("(deny file-write*)"));
        // bwrap must NOT unshare the network when egress is allowed.
        let flags = bwrap_flags_dims(dims);
        assert!(!flags.iter().any(|f| f == "--unshare-net"));
        assert!(flags.iter().any(|f| f == "--ro-bind"));
    }

    #[test]
    fn permissive_net_fs_is_a_noop_regardless_of_exec() {
        // exec is not an OS-sandbox dimension: an unrestricted network +
        // filesystem profile is a true no-op even when the exec declaration is
        // restricted (the addon — and its interpreter chain — must start).
        let caps = AddonCapabilities {
            network: NetworkAccess::Full,
            filesystem: FilesystemAccess::ReadWrite,
            env: vec![],
            exec: ExecAccess::default(), // `none` — a restricted declaration
        };
        assert!(caps.exec_restricted());
        assert!(dims_for_caps(&caps).is_noop());
        // apply_for returns the command unchanged — exec is never OS-enforced.
        let (cmd, args) = apply_for("my-mcp", &["serve".into()], Some(&caps)).expect("noop");
        assert_eq!(cmd, "my-mcp");
        assert_eq!(args, vec!["serve".to_string()]);
    }

    #[test]
    fn caps_wrap_argv_prepends_launcher() {
        let dims = dims_for_caps(&AddonCapabilities::default());
        let (cmd, args) = wrap_argv_dims(Launcher::SandboxExec, dims, "my-mcp", &["x".into()]);
        assert_eq!(cmd, "sandbox-exec");
        assert_eq!(args[0], "-p");
        assert!(args[1].contains("(deny network*)"));
        assert_eq!(args[2], "my-mcp");
        assert_eq!(args[3], "x");
    }

    // --- exec is declared/audited, NOT OS-enforced (see module docs) ---

    #[test]
    fn sandbox_profile_never_emits_process_exec() {
        // Whatever the exec declaration, the generated SBPL profile only ever
        // governs network + filesystem — never `process-exec`. Path-allowlisting
        // execve is not portable (bwrap/seccomp can't) and breaks interpreted
        // servers (the interpreter chain is itself a process-exec).
        let dims = dims_for_caps(&AddonCapabilities::default());
        let (_cmd, args) = wrap_argv_dims(Launcher::SandboxExec, dims, "my-mcp", &[]);
        assert!(args[1].contains("(deny network*)"));
        assert!(args[1].contains("(deny file-write*)"));
        assert!(!args[1].contains("process-exec"));
    }

    #[test]
    fn mode_path_unchanged_via_dims() {
        // Back-compat: the mode wrappers still produce the historical profiles.
        assert!(sbpl_profile(SandboxMode::Auto).contains("(deny network*)"));
        assert!(!sbpl_profile(SandboxMode::Auto).contains("(deny file-write*)"));
        assert!(sbpl_profile(SandboxMode::Strict).contains("(deny file-write*)"));
        assert!(
            bwrap_flags(SandboxMode::Auto)
                .iter()
                .any(|f| f == "--unshare-net")
        );
    }
}
