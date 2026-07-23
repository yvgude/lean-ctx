//! Addon bootstrap engine (#1105, Phase 2): install an addon's upstream package
//! through a *real* package manager as part of `addon add`, idempotently and
//! with mandatory version pinning — then uninstall it on `addon remove`.
//!
//! Security model — the engine **never** goes through a shell. Each supported
//! [`Manager`] owns its argv template; the manifest only supplies `package` +
//! `version` (validated to be pinned and free of shell metacharacters), and the
//! engine inserts them as *discrete* argv elements via [`std::process::Command`].
//! Because there is no string interpolation into a shell, a hostile registry
//! entry cannot inject a command — the worst it can do is name a different
//! package, which is already disclosed in the install preview and audited.
//!
//! The manager binary is resolved from `PATH` by default, or pinned to an exact
//! path via `LEANCTX_BOOTSTRAP_<MANAGER>` (e.g. `LEANCTX_BOOTSTRAP_UV=/opt/uv`)
//! for locked-down / enterprise environments.

use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

/// A supported package manager. The set is closed on purpose: the engine only
/// runs managers whose install/uninstall argv it fully controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Manager {
    /// Astral `uv` — `uv tool install <pkg>==<ver>` (Python CLIs).
    Uv,
    /// `pip` — `pip install --user <pkg>==<ver>` (Python libraries/CLIs).
    Pip,
    /// `cargo` — `cargo install <pkg> --version <ver>` (Rust binaries).
    Cargo,
    /// `npm` — `npm install -g <pkg>@<ver>` (Node CLIs).
    Npm,
    /// Homebrew — `brew install <formula>` (version pinned via the formula name,
    /// e.g. `node@22`; the `version` field documents the expected version).
    Brew,
    /// .NET SDK — `dotnet tool install --global <pkg> --version <ver>` (.NET
    /// global tools published to NuGet, e.g. `CodeCompress.Server`).
    Dotnet,
}

impl Manager {
    /// Parse a manager slug from a manifest, case-insensitively. Unknown → `None`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "uv" => Some(Self::Uv),
            "pip" | "pip3" => Some(Self::Pip),
            "cargo" => Some(Self::Cargo),
            "npm" => Some(Self::Npm),
            "brew" | "homebrew" => Some(Self::Brew),
            "dotnet" => Some(Self::Dotnet),
            _ => None,
        }
    }

    /// Canonical slug (also the default executable name).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Uv => "uv",
            Self::Pip => "pip",
            Self::Cargo => "cargo",
            Self::Npm => "npm",
            Self::Brew => "brew",
            Self::Dotnet => "dotnet",
        }
    }

    /// The executable to invoke — the `LEANCTX_BOOTSTRAP_<MANAGER>` override if
    /// set + non-empty, otherwise the manager's name (resolved via `PATH`).
    #[must_use]
    pub fn program(self) -> String {
        let key = format!("LEANCTX_BOOTSTRAP_{}", self.as_str().to_ascii_uppercase());
        std::env::var(&key)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| self.as_str().to_string())
    }

    /// A short, actionable hint for installing this manager when it is absent —
    /// surfaced by the `addon add` pre-flight so a missing manager fails with a
    /// "here's how to get it" message rather than a raw spawn error.
    #[must_use]
    pub fn install_hint(self) -> &'static str {
        match self {
            Self::Uv => "install uv → https://docs.astral.sh/uv/getting-started/installation/",
            Self::Pip => "install Python & pip → https://pip.pypa.io/en/stable/installation/",
            Self::Cargo => "install Rust (cargo) → https://rustup.rs",
            Self::Npm => "install Node.js (ships npm) → https://nodejs.org/",
            Self::Brew => "install Homebrew → https://brew.sh",
            Self::Dotnet => "install the .NET SDK → https://dotnet.microsoft.com/download",
        }
    }

    /// Whether the manager's executable can actually be launched: its
    /// `LEANCTX_BOOTSTRAP_<MANAGER>` override path is executable, or its bare
    /// name resolves on `PATH`. Lets `addon add` pre-flight a missing manager.
    #[must_use]
    pub fn is_available(self) -> bool {
        let prog = self.program();
        if prog.contains('/') || prog.contains('\\') {
            is_executable(std::path::Path::new(&prog))
        } else {
            binary_on_path(&prog)
        }
    }

    /// argv to install `package` pinned to `version`. Engine-owned; `package`
    /// and `version` are discrete elements (never concatenated into a shell).
    #[must_use]
    fn install_argv(self, package: &str, version: &str) -> Vec<String> {
        let pkg = package.trim();
        let ver = version.trim();
        match self {
            Self::Uv => vec!["tool".into(), "install".into(), format!("{pkg}=={ver}")],
            Self::Pip => vec!["install".into(), "--user".into(), format!("{pkg}=={ver}")],
            Self::Cargo => vec![
                "install".into(),
                package_base(pkg).into(),
                "--version".into(),
                ver.into(),
            ],
            Self::Npm => vec!["install".into(), "-g".into(), format!("{pkg}@{ver}")],
            // Homebrew cannot install an arbitrary historical version; the
            // formula name carries the pin (`node@22`), so we install it verbatim.
            Self::Brew => vec!["install".into(), pkg.into()],
            Self::Dotnet => vec![
                "tool".into(),
                "install".into(),
                "--global".into(),
                package_base(pkg).into(),
                "--version".into(),
                ver.into(),
            ],
        }
    }

    /// argv to uninstall the package previously installed by [`Self::install_argv`].
    #[must_use]
    fn uninstall_argv(self, package: &str) -> Vec<String> {
        let base = package_base(package.trim());
        match self {
            Self::Uv => vec!["tool".into(), "uninstall".into(), base.into()],
            Self::Pip => vec!["uninstall".into(), "-y".into(), base.into()],
            Self::Npm => vec!["rm".into(), "-g".into(), base.into()],
            Self::Cargo | Self::Brew => vec!["uninstall".into(), base.into()],
            Self::Dotnet => vec![
                "tool".into(),
                "uninstall".into(),
                "--global".into(),
                base.into(),
            ],
        }
    }
}

/// The `[install]` block — how `addon add` provisions the addon's upstream
/// package before wiring its `[mcp]` server. Absent (all-empty) ⇒ the addon is
/// either an ephemeral runner (`npx`/`uvx`, installs lazily on first spawn) or
/// already on the host; no bootstrap runs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AddonInstall {
    /// Package manager: `uv` | `pip` | `cargo` | `npm` | `brew` | `dotnet`.
    pub manager: String,
    /// Package/formula to install (may carry extras, e.g. `headroom-ai[all]`).
    pub package: String,
    /// Exact pinned version (mandatory; floating/`latest` is rejected).
    pub version: String,
    /// Executable the install provides — the `[mcp].command`. Used for the
    /// idempotency check (skip if already on PATH). Defaults to the package name.
    pub bin: String,
    /// Optional explicit "is it installed?" probe (argv; exit 0 ⇒ installed).
    /// Overrides the default PATH check; never run through a shell.
    pub verify: Vec<String>,
}

impl AddonInstall {
    /// `true` when the block actually requests a bootstrap install.
    #[must_use]
    pub fn is_declared(&self) -> bool {
        !self.manager.trim().is_empty() && !self.package.trim().is_empty()
    }

    /// Inverse of [`Self::is_declared`] — for `#[serde(skip_serializing_if)]`.
    #[must_use]
    pub fn is_absent(&self) -> bool {
        !self.is_declared()
    }

    /// The parsed manager, if declared and supported.
    #[must_use]
    pub fn manager(&self) -> Option<Manager> {
        self.is_declared()
            .then(|| Manager::parse(&self.manager))
            .flatten()
    }

    /// The executable name to probe on PATH — explicit `bin`, else the package
    /// base name (extras + version stripped).
    #[must_use]
    pub fn bin(&self) -> &str {
        let b = self.bin.trim();
        if b.is_empty() {
            package_base(self.package.trim())
        } else {
            b
        }
    }

    /// Validate the block: known manager, non-empty package, an exact pin, and
    /// no shell metacharacters anywhere (defence-in-depth). A no-op when absent.
    pub fn validate(&self) -> Result<(), String> {
        if !self.is_declared() {
            return Ok(());
        }
        if self.manager().is_none() {
            return Err(format!(
                "[install] manager `{}` is not supported — use one of: uv, pip, cargo, npm, brew, dotnet",
                self.manager.trim()
            ));
        }
        let ver = self.version.trim();
        if ver.is_empty() {
            return Err(format!(
                "[install] `{}` must pin an exact `version` — floating installs are rejected",
                self.package.trim()
            ));
        }
        if mentions_latest(ver) {
            return Err("[install] `version` must be an exact pin, not `latest`".into());
        }
        for (field, val) in [
            ("package", self.package.as_str()),
            ("version", self.version.as_str()),
            ("bin", self.bin.as_str()),
        ] {
            if has_shell_meta(val) {
                return Err(format!(
                    "[install] `{field}` contains shell metacharacters (| ; & $ ` > <) — rejected"
                ));
            }
        }
        if self.verify.iter().any(|a| has_shell_meta(a)) {
            return Err("[install] `verify` argv contains shell metacharacters — rejected".into());
        }
        Ok(())
    }

    /// A receipt recording exactly what was installed, for a clean uninstall.
    #[must_use]
    pub fn to_receipt(&self) -> InstallReceipt {
        InstallReceipt {
            manager: self.manager.trim().to_ascii_lowercase(),
            package: self.package.trim().to_string(),
            version: self.version.trim().to_string(),
            bin: self.bin().to_string(),
        }
    }

    /// The exact install argv (for the disclosure preview). Empty if unsupported.
    #[must_use]
    pub fn install_argv(&self) -> Vec<String> {
        self.manager()
            .map(|m| m.install_argv(&self.package, &self.version))
            .unwrap_or_default()
    }

    /// The exact uninstall argv (for the disclosure preview). Empty if unsupported.
    #[must_use]
    pub fn uninstall_argv(&self) -> Vec<String> {
        self.manager()
            .map(|m| m.uninstall_argv(&self.package))
            .unwrap_or_default()
    }

    /// Whether the package already appears installed: the explicit `verify`
    /// probe (exit 0), else the `bin` resolving on PATH.
    #[must_use]
    fn already_satisfied(&self) -> bool {
        if let Some((prog, rest)) = self.verify.split_first() {
            return probe_ok(prog, rest);
        }
        binary_on_path(self.bin())
    }
}

/// What `[install]` actually did, persisted in `installed.json` so `remove` can
/// uninstall exactly what `add` installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallReceipt {
    pub manager: String,
    pub package: String,
    pub version: String,
    pub bin: String,
}

impl InstallReceipt {
    fn manager(&self) -> Option<Manager> {
        Manager::parse(&self.manager)
    }
}

/// Outcome of [`ensure_installed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapStatus {
    /// The package was already present — nothing ran (idempotent).
    AlreadyPresent,
    /// The package manager ran and installed it.
    Installed,
}

/// Result of a successful [`ensure_installed`].
#[derive(Debug, Clone)]
pub struct BootstrapOutcome {
    pub status: BootstrapStatus,
    pub receipt: InstallReceipt,
    /// A non-fatal note (e.g. installed but the bin is not yet on PATH).
    pub warning: Option<String>,
}

/// Provision `install`'s package idempotently. Returns immediately if it is
/// already satisfied; otherwise runs the manager (streaming its output so the
/// user sees real progress) and re-checks. A non-zero manager exit is an error;
/// a clean exit whose binary is not yet on PATH is a non-fatal warning.
///
/// Spawns a subprocess — only ever called from the interactive CLI layer after
/// the user has consented; the core `install::install` stays pure.
pub fn ensure_installed(install: &AddonInstall) -> Result<BootstrapOutcome, String> {
    install.validate()?;
    let manager = install
        .manager()
        .ok_or_else(|| format!("unsupported package manager `{}`", install.manager.trim()))?;
    let receipt = install.to_receipt();

    if install.already_satisfied() {
        return Ok(BootstrapOutcome {
            status: BootstrapStatus::AlreadyPresent,
            receipt,
            warning: None,
        });
    }

    // Pre-flight: the package is absent, so the manager must run — verify it
    // exists first and fail with an install hint instead of a raw spawn error.
    if !manager.is_available() {
        return Err(format!(
            "the `{mgr}` package manager is not installed (or not on PATH), so `{pkg}` cannot be \
             installed.\n  → {hint}\n  Or install `{bin}` yourself, then re-run — `addon add` \
             detects it and skips the bootstrap.",
            mgr = manager.as_str(),
            pkg = install.package.trim(),
            hint = manager.install_hint(),
            bin = install.bin(),
        ));
    }

    run(
        &manager.program(),
        &manager.install_argv(&install.package, &install.version),
    )?;

    let warning = (!install.already_satisfied()).then(|| {
        format!(
            "`{}` installed but `{}` is not on your PATH yet — add the manager's bin directory \
             (e.g. ~/.local/bin) to PATH so the MCP server can launch.",
            install.package.trim(),
            install.bin()
        )
    });

    Ok(BootstrapOutcome {
        status: BootstrapStatus::Installed,
        receipt,
        warning,
    })
}

/// Uninstall a previously bootstrapped package (best-effort; the caller logs a
/// note on failure rather than blocking the unwire).
pub fn uninstall(receipt: &InstallReceipt) -> Result<(), String> {
    let manager = receipt.manager().ok_or_else(|| {
        format!(
            "unsupported package manager `{}` in receipt",
            receipt.manager
        )
    })?;
    run(
        &manager.program(),
        &manager.uninstall_argv(&receipt.package),
    )
}

/// Strip a package spec down to the bare name a manager uninstalls by: drop
/// extras (`pkg[all]` → `pkg`) and any inline version (`pkg==1` / `pkg@1`).
fn package_base(package: &str) -> &str {
    let p = package.trim();
    let p = p.split('[').next().unwrap_or(p);
    let p = p.split("==").next().unwrap_or(p);
    // Trim a trailing `@version` but keep an npm scope (`@scope/pkg`).
    match p.rsplit_once('@') {
        Some((head, _)) if !head.is_empty() => head,
        _ => p,
    }
    .trim()
}

/// Run a manager command, inheriting stdio so the user sees live progress.
fn run(program: &str, argv: &[String]) -> Result<(), String> {
    let status = Command::new(program).args(argv).status().map_err(|e| {
        format!("could not launch `{program}`: {e} — is it installed and on your PATH?")
    })?;
    if status.success() {
        return Ok(());
    }
    Err(format!(
        "`{program} {}` failed ({})",
        argv.join(" "),
        status.code().map_or_else(
            || "terminated by signal".to_string(),
            |c| format!("exit {c}")
        )
    ))
}

/// Run a quiet probe (stdio suppressed); `true` iff it exits 0.
fn probe_ok(program: &str, argv: &[String]) -> bool {
    Command::new(program)
        .args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Whether `bin` resolves to an executable on `PATH`.
fn binary_on_path(bin: &str) -> bool {
    if bin.is_empty() {
        return false;
    }
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| is_executable(&dir.join(bin)))
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    path.is_file()
}

/// Shell metacharacters that would matter if (and only if) the value ever
/// reached a shell. We never use a shell — this is defence-in-depth so a
/// hostile registry entry is rejected loudly rather than silently tolerated.
fn has_shell_meta(s: &str) -> bool {
    s.chars()
        .any(|c| matches!(c, '|' | ';' | '&' | '`' | '>' | '<' | '\n' | '\r'))
        || s.contains("$(")
}

/// Whether a version string is a floating/`latest` tag rather than an exact pin.
fn mentions_latest(version: &str) -> bool {
    let v = version.trim().to_ascii_lowercase();
    v == "latest" || v.ends_with("@latest") || v.ends_with(":latest") || v == "*"
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::sync::Mutex;

    /// Serialises the few tests that set a `LEANCTX_BOOTSTRAP_*` override, since
    /// process environment is global. No other test reads these vars. Unix-only:
    /// the env-override tests it guards are themselves `#[cfg(unix)]`.
    #[cfg(unix)]
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn declared(manager: &str, package: &str, version: &str) -> AddonInstall {
        AddonInstall {
            manager: manager.into(),
            package: package.into(),
            version: version.into(),
            ..Default::default()
        }
    }

    #[test]
    fn absent_block_is_a_noop() {
        let empty = AddonInstall::default();
        assert!(!empty.is_declared());
        assert!(empty.is_absent());
        assert!(empty.validate().is_ok());
        assert!(empty.manager().is_none());
    }

    #[test]
    fn validate_requires_known_manager_and_pin() {
        assert!(declared("uv", "pkg", "1.2.3").validate().is_ok());
        assert!(declared("conda", "pkg", "1.2.3").validate().is_err());
        assert!(declared("uv", "pkg", "").validate().is_err());
        assert!(declared("uv", "pkg", "latest").validate().is_err());
        assert!(declared("npm", "pkg", "*").validate().is_err());
    }

    #[test]
    fn validate_rejects_shell_metacharacters() {
        assert!(declared("uv", "pkg; rm -rf /", "1.0.0").validate().is_err());
        assert!(declared("uv", "pkg", "1.0.0 && evil").validate().is_err());
        assert!(declared("uv", "pkg`whoami`", "1.0.0").validate().is_err());
        // Extras brackets are not shell metacharacters → accepted.
        assert!(
            declared("uv", "headroom-ai[all]", "1.4.2")
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn install_argv_is_pinned_per_manager() {
        assert_eq!(
            declared("uv", "headroom-ai[all]", "1.4.2").install_argv(),
            ["tool", "install", "headroom-ai[all]==1.4.2"]
        );
        assert_eq!(
            declared("pip", "cognee", "0.1.0").install_argv(),
            ["install", "--user", "cognee==0.1.0"]
        );
        assert_eq!(
            declared("cargo", "ripgrep", "14.1.0").install_argv(),
            ["install", "ripgrep", "--version", "14.1.0"]
        );
        assert_eq!(
            declared("npm", "@scope/cli", "2.0.0").install_argv(),
            ["install", "-g", "@scope/cli@2.0.0"]
        );
        assert_eq!(
            declared("brew", "node@22", "22.0.0").install_argv(),
            ["install", "node@22"]
        );
        assert_eq!(
            declared("dotnet", "CodeCompress.Server", "0.15.0").install_argv(),
            [
                "tool",
                "install",
                "--global",
                "CodeCompress.Server",
                "--version",
                "0.15.0"
            ]
        );
    }

    #[test]
    fn uninstall_argv_targets_the_base_name() {
        assert_eq!(
            declared("uv", "headroom-ai[all]", "1.4.2").uninstall_argv(),
            ["tool", "uninstall", "headroom-ai"]
        );
        assert_eq!(
            declared("npm", "@scope/cli", "2.0.0").uninstall_argv(),
            ["rm", "-g", "@scope/cli"]
        );
        assert_eq!(
            declared("pip", "cognee==0.1.0", "0.1.0").uninstall_argv(),
            ["uninstall", "-y", "cognee"]
        );
        assert_eq!(
            declared("dotnet", "CodeCompress.Server", "0.15.0").uninstall_argv(),
            ["tool", "uninstall", "--global", "CodeCompress.Server"]
        );
    }

    #[test]
    fn bin_defaults_to_package_base_else_explicit() {
        assert_eq!(
            declared("uv", "headroom-ai[all]", "1.0.0").bin(),
            "headroom-ai"
        );
        let mut with_bin = declared("uv", "headroom-ai[all]", "1.0.0");
        with_bin.bin = "headroom".into();
        assert_eq!(with_bin.bin(), "headroom");
    }

    #[test]
    fn package_base_strips_extras_version_and_keeps_npm_scope() {
        assert_eq!(package_base("headroom-ai[all]==1.4.2"), "headroom-ai");
        assert_eq!(package_base("pkg@1.2.3"), "pkg");
        assert_eq!(package_base("@scope/pkg"), "@scope/pkg");
        assert_eq!(package_base("@scope/pkg@1.0.0"), "@scope/pkg");
    }

    #[test]
    fn receipt_round_trips_and_normalises_manager() {
        let r = declared("UV", "pkg", "1.0.0").to_receipt();
        assert_eq!(r.manager, "uv");
        assert_eq!(r.manager().unwrap(), Manager::Uv);
        let json = serde_json::to_string(&r).unwrap();
        let back: InstallReceipt = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn binary_on_path_finds_a_standard_tool() {
        // A tool present on every host of the platform; a random name never does.
        #[cfg(unix)]
        assert!(binary_on_path("sh"));
        assert!(!binary_on_path("lean-ctx-definitely-not-a-real-binary-xyz"));
        assert!(!binary_on_path(""));
    }

    /// Write an executable shell script at `path` with `body`. Unix-only: the
    /// bootstrap executor tests it drives are themselves `#[cfg(unix)]`.
    #[cfg(unix)]
    fn write_script(path: &std::path::Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn ensure_installed_runs_manager_then_verifies() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp = std::env::temp_dir().join(format!("leanctx-boot-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let marker = tmp.join("installed.marker");
        let fake_uv = tmp.join("uv");
        // Fake `uv`: create the marker the verify probe checks for.
        write_script(&fake_uv, &format!("touch '{}'", marker.display()));

        let mut install = declared("uv", "demo-pkg", "1.0.0");
        install.verify = vec!["test".into(), "-f".into(), marker.display().to_string()];

        // SAFETY: guarded by ENV_LOCK; restored below.
        unsafe { std::env::set_var("LEANCTX_BOOTSTRAP_UV", &fake_uv) };
        let _ = std::fs::remove_file(&marker);

        let out = ensure_installed(&install).expect("install");
        assert_eq!(out.status, BootstrapStatus::Installed);
        assert!(out.warning.is_none(), "verify passed → no warning");
        assert!(marker.exists(), "fake manager ran");

        // Second run is idempotent — marker already there, manager not re-run.
        std::fs::remove_file(&fake_uv).unwrap(); // would error if invoked
        let out2 = ensure_installed(&install).expect("idempotent");
        assert_eq!(out2.status, BootstrapStatus::AlreadyPresent);

        // SAFETY: guarded by ENV_LOCK; clears the override set above.
        unsafe { std::env::remove_var("LEANCTX_BOOTSTRAP_UV") };
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    #[cfg(unix)]
    fn ensure_installed_propagates_manager_failure() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp = std::env::temp_dir().join(format!("leanctx-boot-fail-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let fake_uv = tmp.join("uv");
        write_script(&fake_uv, "exit 1");

        let mut install = declared("uv", "demo-pkg", "1.0.0");
        install.verify = vec!["false".into()]; // never satisfied

        // SAFETY: guarded by ENV_LOCK; restored below.
        unsafe { std::env::set_var("LEANCTX_BOOTSTRAP_UV", &fake_uv) };
        let err = ensure_installed(&install).expect_err("manager failed");
        assert!(err.contains("failed"), "got: {err}");

        // SAFETY: guarded by ENV_LOCK; clears the override set above.
        unsafe { std::env::remove_var("LEANCTX_BOOTSTRAP_UV") };
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn install_hint_is_present_for_every_manager() {
        for m in [
            Manager::Uv,
            Manager::Pip,
            Manager::Cargo,
            Manager::Npm,
            Manager::Brew,
            Manager::Dotnet,
        ] {
            assert!(!m.install_hint().is_empty(), "{m:?} needs an install hint");
        }
    }

    #[test]
    #[cfg(unix)]
    fn ensure_installed_preflights_a_missing_manager() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp = std::env::temp_dir().join(format!("leanctx-boot-miss-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let missing = tmp.join("uv-not-here");

        let mut install = declared("uv", "demo-pkg", "1.0.0");
        install.verify = vec!["false".into()]; // never satisfied → must install

        // SAFETY: guarded by ENV_LOCK; points the manager at a non-existent path
        // so the pre-flight must reject *before* any spawn is attempted.
        unsafe { std::env::set_var("LEANCTX_BOOTSTRAP_UV", &missing) };
        let err = ensure_installed(&install).expect_err("missing manager rejected");

        // SAFETY: guarded by ENV_LOCK; clears the override set above.
        unsafe { std::env::remove_var("LEANCTX_BOOTSTRAP_UV") };
        let _ = std::fs::remove_dir_all(&tmp);

        assert!(err.contains("uv"), "names the manager: {err}");
        assert!(err.contains("not installed"), "explains why: {err}");
        assert!(err.contains("astral.sh"), "gives an install hint: {err}");
    }
}
