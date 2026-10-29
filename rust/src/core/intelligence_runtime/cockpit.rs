// SPDX-License-Identifier: Apache-2.0
//! The Cockpit web UI: a free, separately licensed, signed package kind.
//!
//! It travels through the runtime channel like the Intelligence Runtime and
//! is authenticated just as strictly (manifest, signature, archive, catalog
//! and delegation), but it is never licence-bound, lives in its own store
//! (`cockpit-runtime[-production]`) with its own activation record, and is
//! re-authenticated as a Cockpit on every start.
//!
//! One Cockpit per Engine dashboard process. It is started from a private
//! copy of the authenticated bytes with a cleared environment and receives
//! one JSON line on stdin (the start contract `leanctx.cockpit-start/v1`).
//! It binds 127.0.0.1:0 and names its port as the first stdout line,
//! `{"listening":<port>}`. The supervisor keeps stdin open for the Engine's
//! lifetime; the Cockpit ends at stdin EOF, at the latest when this process
//! ends. A crash is restarted on the next request after a bounded backoff.
//! No token is ever logged.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::{InstallError, Result, RuntimeKind, VerifiedPackage, bootstrap, install};

/// The activation record in the Cockpit store: which installed package the
/// dashboard starts, under which artifact key, and whether it is enabled.
const ACTIVATION: &str = "activation.json";
/// The name of the private copy the Cockpit runs as.
const EXECUTABLE: &str = if cfg!(windows) {
    "leanctx-cockpit.exe"
} else {
    "leanctx-cockpit"
};
/// How long a start may take to name its port.
const HANDSHAKE: Duration = Duration::from_secs(10);
/// How long a Cockpit may take to end after its stdin closed.
const EXIT_GRACE: Duration = Duration::from_secs(2);
/// Waits before the first, second and every further restart after a
/// failure in a row. A Cockpit that ran for `STABLE` ends the row.
const RESTART_BACKOFF: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(5),
    Duration::from_secs(30),
];
const STABLE: Duration = Duration::from_mins(1);
/// The port line is short; anything longer is not the start contract.
const MAX_PORT_LINE: u64 = 256;

/// What the dashboard knows about itself when it asks for the Cockpit.
pub(crate) struct CockpitLaunch {
    /// The port of the Engine dashboard on 127.0.0.1.
    pub(crate) engine_port: u16,
    /// The dashboard bearer token the Cockpit uses to call the Engine.
    pub(crate) engine_token: String,
    pub(crate) project_root: PathBuf,
}

/// Where the running Cockpit listens, and the token the dashboard's proxy
/// presents to it.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CockpitEndpoint {
    pub(crate) port: u16,
    /// 32 random bytes, lowercase hex.
    pub(crate) cockpit_token: String,
}

impl std::fmt::Debug for CockpitEndpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CockpitEndpoint")
            .field("port", &self.port)
            .field("cockpit_token", &"<redacted>")
            .finish()
    }
}

/// Why no Cockpit is available; the dashboard shows the matching fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CockpitUnavailable {
    /// No Cockpit is installed and activated in this host's Cockpit store.
    NotInstalled,
    /// The activation or the installed package did not authenticate.
    VerificationFailed,
    /// The authenticated Cockpit did not start, did not name its port in
    /// time, or crashed and waits for its restart.
    StartFailed,
    /// The Cockpit is installed and was deactivated.
    Disabled,
    /// A source build: no Cockpit channel is compiled in and none is
    /// installed. The Cockpit ships with the official installs.
    SourceBuild,
}

impl CockpitUnavailable {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::NotInstalled => "not_installed",
            Self::VerificationFailed => "verification_failed",
            Self::StartFailed => "start_failed",
            Self::Disabled => "disabled",
            Self::SourceBuild => "source_build",
        }
    }

    /// One line for a user who opens the Cockpit while it cannot be served.
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::NotInstalled => {
                "The Cockpit is not installed. Install it with: lean-ctx cockpit install"
            }
            Self::VerificationFailed => {
                "The installed Cockpit did not verify and was not started. Reinstall it with: lean-ctx cockpit install"
            }
            Self::StartFailed => {
                "The Cockpit did not start. It is retried automatically; in the terminal: lean-ctx gain"
            }
            Self::Disabled => {
                "The Cockpit is deactivated. Activate it again with: lean-ctx cockpit install"
            }
            Self::SourceBuild => {
                "Source build: the Cockpit ships with official installs (install.sh, Homebrew, npm, AUR); in the terminal: lean-ctx gain"
            }
        }
    }
}

/// The five files of a Cockpit bundle and their size bounds.
const BUNDLE_FILES: [(&str, usize); 5] = [
    ("channel.json", 65_536),
    ("channel.sig", 64),
    ("manifest.json", 65_536),
    ("manifest.sig", 64),
    ("archive.tar.gz", super::manifest::MAX_ARCHIVE),
];

/// A bundle: a directory holding [`BUNDLE_FILES`], or a `.tar.gz` holding
/// them directly or in one top-level directory. Nothing else is read.
fn read_bundle(path: &Path) -> Result<super::catalog::Bundle> {
    let mut files: [Option<Vec<u8>>; 5] = Default::default();
    if std::fs::symlink_metadata(path)?.is_dir() {
        for (slot, (name, limit)) in files.iter_mut().zip(BUNDLE_FILES) {
            *slot = Some(super::read_regular(&path.join(name), limit)?);
        }
    } else {
        let packed = super::read_regular(path, super::manifest::MAX_ARCHIVE + (1 << 20))?;
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(packed.as_slice()));
        for entry in archive.entries().map_err(|_| InstallError::Archive)? {
            let entry = entry.map_err(|_| InstallError::Archive)?;
            let kind = entry.header().entry_type();
            if kind.is_dir() {
                continue;
            }
            let entry_path = entry
                .path()
                .map_err(|_| InstallError::Archive)?
                .into_owned();
            let parts: Vec<_> = entry_path.components().collect();
            if !kind.is_file()
                || parts.len() > 2
                || parts
                    .iter()
                    .any(|part| !matches!(part, std::path::Component::Normal(_)))
            {
                return Err(InstallError::Archive);
            }
            let name = entry_path.file_name().and_then(|name| name.to_str());
            let Some(index) = BUNDLE_FILES
                .iter()
                .position(|(known, _)| Some(*known) == name)
            else {
                return Err(InstallError::Archive);
            };
            let limit = BUNDLE_FILES[index].1;
            let mut bytes = Vec::new();
            entry
                .take(limit as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| InstallError::Archive)?;
            if bytes.len() > limit || files[index].replace(bytes).is_some() {
                return Err(InstallError::Archive);
            }
        }
    }
    let [
        Some(catalog),
        Some(catalog_signature),
        Some(manifest),
        Some(signature),
        Some(archive),
    ] = files
    else {
        return Err(InstallError::Archive);
    };
    Ok(super::catalog::Bundle {
        catalog,
        catalog_signature,
        archive,
        manifest,
        signature,
    })
}

/// Verify `bundle` against this host's compiled-in Cockpit channel root,
/// install it and activate it. No network. An older channel sequence than
/// the active one installs only with `downgrade`.
fn import(bundle: &super::catalog::Bundle, downgrade: bool) -> Result<serde_json::Value> {
    let channel = bootstrap::cockpit_channel()?.ok_or(InstallError::Policy)?;
    let root = bootstrap::cockpit_root()?;
    install::prepare_parent_chain(root.parent().ok_or(InstallError::Directory)?)?;
    let _guard = install::configuration_guard(&root)?;
    let expected = install::active_manifest(&root)?;
    let installation = super::catalog::import_kind(
        RuntimeKind::Cockpit,
        bundle,
        &channel.key,
        &root,
        &expected,
        downgrade,
    )?;
    activate_installation(&root, &installation, "bundle")
}

/// `lean-ctx cockpit install --from <dir|file.tar.gz> [--allow-downgrade]`.
pub(crate) fn install_from(path: &Path, downgrade: bool) -> Result<serde_json::Value> {
    import(&read_bundle(path)?, downgrade)
}

/// The `--from` form of `lean-ctx cockpit install` (`args` follow
/// `install`); `false` when the arguments do not name one, so the caller
/// installs from the channel. Exits non-zero on failure.
pub(crate) fn install_from_cli(args: &[String]) -> bool {
    if args.is_empty() {
        return false;
    }
    let downgrade = args.iter().any(|arg| arg == "--allow-downgrade");
    let rest: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|arg| *arg != "--allow-downgrade")
        .collect();
    let path = match rest.as_slice() {
        ["--from", path] => *path,
        [flag] if flag.starts_with("--from=") => &flag["--from=".len()..],
        _ => {
            eprintln!(
                "Usage: lean-ctx cockpit install [--from <dir|file.tar.gz> [--allow-downgrade]]"
            );
            std::process::exit(2);
        }
    };
    match install_from(Path::new(path), downgrade) {
        Ok(result) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string())
            );
            println!("Open it with: lean-ctx cockpit");
        }
        Err(InstallError::Downgrade) => {
            eprintln!(
                "lean-ctx cockpit install --from: this bundle is older than the active Cockpit; \
                 add --allow-downgrade to install it"
            );
            std::process::exit(1);
        }
        Err(error) => {
            eprintln!("lean-ctx cockpit install --from: the bundle was not installed ({error:?})");
            std::process::exit(1);
        }
    }
    true
}

/// The Cockpit bundle an official install ships beside the Engine.
fn bundled_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let prefix = exe.parent()?.parent()?;
    ["share", "libexec"]
        .iter()
        .map(|place| prefix.join(place).join("lean-ctx").join("cockpit"))
        .find(|dir| dir.join("manifest.json").is_file())
}

/// Import the shipped Cockpit when none is active or it ships a different
/// package; one older than the active install (by channel sequence) is left
/// alone. Never fails a start.
fn import_bundled() {
    let Some(dir) = bundled_dir() else {
        return;
    };
    let Ok(manifest) = super::read_regular(&dir.join("manifest.json"), 65_536) else {
        return;
    };
    let shipped = super::sha256(&manifest);
    let active = bootstrap::cockpit_root()
        .ok()
        .and_then(|root| read_activation(&root).ok().flatten());
    // The shipped package is active, or the user deactivated the Cockpit
    // and keeps it deactivated.
    if active
        .as_ref()
        .is_some_and(|active| !active.enabled || active.manifest_sha256 == shipped)
    {
        return;
    }
    match read_bundle(&dir).and_then(|bundle| import(&bundle, false)) {
        Ok(_) => tracing::info!("imported the bundled cockpit"),
        Err(InstallError::Downgrade) => {
            tracing::info!("bundled cockpit is older than the active one; kept the active one");
        }
        Err(error) => tracing::debug!("bundled cockpit not imported: {error:?}"),
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Activation {
    schema_version: u32,
    enabled: bool,
    manifest_sha256: String,
    trust_key_hex: String,
}

fn read_activation(root: &Path) -> Result<Option<Activation>> {
    let path = root.join(ACTIVATION);
    if !path.try_exists()? {
        return Ok(None);
    }
    let activation: Activation = serde_json::from_slice(&super::read_regular(&path, 4096)?)
        .map_err(|_| InstallError::State)?;
    if activation.schema_version != 1
        || !super::is_digest(&activation.manifest_sha256, 64)
        || !super::is_digest(&activation.trust_key_hex, 64)
    {
        return Err(InstallError::State);
    }
    Ok(Some(activation))
}

/// The caller holds the store's configuration guard.
fn write_activation(root: &Path, activation: &Activation) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(activation).map_err(|_| InstallError::State)?;
    crate::core::updater::atomic_write_bytes(&root.join(ACTIVATION), &bytes)
        .map_err(|_| InstallError::Commit)
}

/// Authenticate the selected package as a Cockpit and record it as the one
/// the dashboard starts. The caller holds the store's configuration guard.
fn activate_locked(root: &Path, selected: &str, key_hex: &str) -> Result<serde_json::Value> {
    let key = super::trust_key(key_hex)?;
    let package = install::verified_active_kind(RuntimeKind::Cockpit, root, selected, &key)?;
    write_activation(
        root,
        &Activation {
            schema_version: 1,
            enabled: true,
            manifest_sha256: selected.to_owned(),
            trust_key_hex: key_hex.to_owned(),
        },
    )?;
    Ok(serde_json::json!({"status": "activated", "kind": "cockpit",
        "receipt": package.receipt, "runtime_started": false, "release_approved": false}))
}

/// The activated Cockpit of this host, authenticated now.
fn authenticated() -> std::result::Result<(PathBuf, VerifiedPackage), CockpitUnavailable> {
    let root = bootstrap::cockpit_root().map_err(|_| CockpitUnavailable::NotInstalled)?;
    let activation = match read_activation(&root) {
        Ok(Some(activation)) => activation,
        Ok(None) => return Err(CockpitUnavailable::NotInstalled),
        Err(_) => return Err(CockpitUnavailable::VerificationFailed),
    };
    if !activation.enabled {
        return Err(CockpitUnavailable::Disabled);
    }
    let package = super::trust_key(&activation.trust_key_hex)
        .and_then(|key| {
            install::verified_active_kind(
                RuntimeKind::Cockpit,
                &root,
                &activation.manifest_sha256,
                &key,
            )
        })
        .map_err(|_| CockpitUnavailable::VerificationFailed)?;
    Ok((root, package))
}

/// Download, verify, install and activate the Cockpit from this host's
/// bundled Cockpit channel. `staging`, when given, must name that channel's
/// track. Network access happens only here, on an explicit request.
fn install_configured(staging: Option<bool>) -> Result<serde_json::Value> {
    let channel = bootstrap::cockpit_channel()?.ok_or(InstallError::Policy)?;
    if staging.is_some_and(|staging| staging != channel.staging) {
        return Err(InstallError::Usage);
    }
    let root = bootstrap::cockpit_root()?;
    install::prepare_parent_chain(root.parent().ok_or(InstallError::Directory)?)?;
    let _guard = install::configuration_guard(&root)?;
    let expected = install::active_manifest(&root)?;
    let installation = super::catalog::sync_kind(
        RuntimeKind::Cockpit,
        channel.urls,
        &channel.key,
        &root,
        &expected,
    )?;
    activate_installation(&root, &installation, "channel")
}

/// Activate what an install selected. The caller holds the store's
/// configuration guard.
fn activate_installation(
    root: &Path,
    installation: &serde_json::Value,
    source: &str,
) -> Result<serde_json::Value> {
    let selected = installation["receipt"]["manifest_sha256"]
        .as_str()
        .ok_or(InstallError::State)?;
    let key = installation["artifact_key_hex"]
        .as_str()
        .ok_or(InstallError::State)?;
    let activation = activate_locked(root, selected, key)?;
    retire();
    Ok(
        serde_json::json!({"status": "installed_and_activated", "kind": "cockpit", "source": source,
        "installation": installation, "activation": activation, "release_approved": false}),
    )
}

/// Install-on-demand for `lean-ctx cockpit`: the same path as
/// `setup runtime sync-configured --kind cockpit`. The caller has the
/// user's request to install; this downloads from the bundled channel.
pub(crate) fn install_cockpit() -> std::result::Result<serde_json::Value, CockpitUnavailable> {
    install_configured(None).map_err(|error| match error {
        InstallError::Manifest
        | InstallError::Signature
        | InstallError::Archive
        | InstallError::Size
        | InstallError::State
        | InstallError::Downgrade => CockpitUnavailable::VerificationFailed,
        _ => CockpitUnavailable::NotInstalled,
    })
}

/// Whether a Cockpit could be started now, decided exactly as a start
/// decides it (shipped bundle imported first, then authenticated), without
/// starting one and without network. Lets a command fall back to its
/// terminal report before it opens a browser on the install page.
pub(crate) fn availability() -> std::result::Result<(), CockpitUnavailable> {
    import_bundled();
    match authenticated() {
        Ok(_) => Ok(()),
        Err(CockpitUnavailable::NotInstalled)
            if bootstrap::cockpit_channel().ok().flatten().is_none() =>
        {
            Err(CockpitUnavailable::SourceBuild)
        }
        Err(reason) => Err(reason),
    }
}

/// `LEAN_CTX_NO_COCKPIT=1` (as for install.sh) keeps every automatic install off.
fn installs_opted_out() -> bool {
    std::env::var_os("LEAN_CTX_NO_COCKPIT").is_some_and(|value| !value.is_empty() && value != "0")
}

/// `availability`, but a missing Cockpit of an official build is installed
/// first: the person asked to open it, and the Cockpit ships with LeanCTX.
pub(crate) fn available_or_installed() -> std::result::Result<(), CockpitUnavailable> {
    match availability() {
        Err(CockpitUnavailable::NotInstalled) if !installs_opted_out() => {
            eprintln!(
                "Installing the LeanCTX Cockpit (signed, free) from the LeanCTX runtime channel…"
            );
            install_cockpit()?;
            availability()
        }
        other => other,
    }
}

/// After a failed automatic install, the next automatic attempt waits a day.
const AUTO_RETRY: Duration = Duration::from_hours(24);

fn auto_attempt_marker() -> Option<PathBuf> {
    crate::core::data_dir::resolve_data_dir()
        .ok()
        .map(|dir| dir.join("cockpit-auto-install.failed"))
}

/// Whether a failed attempt recorded at `failed_at` still blocks one at `now`.
fn retry_blocked(failed_at: Option<std::time::SystemTime>, now: std::time::SystemTime) -> bool {
    failed_at
        .and_then(|failed_at| now.duration_since(failed_at).ok())
        .is_some_and(|elapsed| elapsed < AUTO_RETRY)
}

/// Install the Cockpit without being asked when an official build has none:
/// after an update from a release without it, or an install path that ships
/// only the Engine, nobody has to run a command. Only when none is installed
/// (a deactivated Cockpit stays deactivated), outside CI, and after a failure
/// at most once a day unless `retry_now`. Returns whether it installed one.
pub(crate) fn install_if_missing(retry_now: bool) -> bool {
    if installs_opted_out()
        || crate::core::telemetry_consent::ci_detected(crate::core::host_env::var)
    {
        return false;
    }
    if !matches!(availability(), Err(CockpitUnavailable::NotInstalled)) {
        return false;
    }
    let marker = auto_attempt_marker();
    let failed_at = marker
        .as_deref()
        .and_then(|path| std::fs::metadata(path).ok())
        .and_then(|metadata| metadata.modified().ok());
    if !retry_now && retry_blocked(failed_at, std::time::SystemTime::now()) {
        return false;
    }
    match install_cockpit() {
        Ok(_) => {
            if let Some(path) = &marker {
                let _ = std::fs::remove_file(path);
            }
            tracing::info!("installed the cockpit automatically");
            true
        }
        Err(reason) => {
            if let Some(path) = &marker {
                let _ = std::fs::write(path, reason.as_str());
            }
            tracing::debug!("automatic cockpit install failed: {}", reason.as_str());
            false
        }
    }
}

/// `install_if_missing` on its own thread: an MCP or daemon start never waits
/// for the download.
pub(crate) fn install_if_missing_in_background() {
    let _ = std::thread::Builder::new()
        .name("cockpit-install".into())
        .spawn(|| {
            install_if_missing(false);
        });
}

/// What `setup runtime status --kind cockpit` reports; starts nothing and
/// uses no network.
pub(crate) fn status() -> serde_json::Value {
    let (status, version) = match authenticated() {
        Ok((_, package)) => ("installed", Some(package.receipt.version)),
        Err(reason) => (reason.as_str(), None),
    };
    let channel = bootstrap::cockpit_channel().ok().flatten();
    serde_json::json!({"schema_version": 1, "kind": "cockpit", "status": status,
        "version": version, "channel_available": channel.is_some(),
        "staging": channel.map(|channel| channel.staging),
        "runtime_started": false, "release_approved": false})
}

fn deactivate() -> Result<serde_json::Value> {
    let root = bootstrap::cockpit_root()?;
    if !root.try_exists()? {
        return Ok(serde_json::json!({"status": "not_installed", "kind": "cockpit"}));
    }
    let _guard = install::configuration_guard(&root)?;
    let Some(mut activation) = read_activation(&root)? else {
        return Ok(serde_json::json!({"status": "not_installed", "kind": "cockpit"}));
    };
    activation.enabled = false;
    write_activation(&root, &activation)?;
    retire();
    Ok(serde_json::json!({"status": "disabled", "kind": "cockpit", "release_approved": false}))
}

/// `lean-ctx setup runtime <operation> --kind cockpit`: the Cockpit's own
/// `status`, `sync-configured`, `activate` and `deactivate`. Package
/// operations (`verify`, `install`, `download`, `sync`) run the shared path.
pub(super) fn run_cli(
    operation: &str,
    staging: bool,
    accepted: bool,
    flags: &std::collections::BTreeMap<&str, &str>,
) -> Result<serde_json::Value> {
    match operation {
        "status" if flags.is_empty() => Ok(status()),
        "deactivate" if flags.is_empty() => deactivate(),
        "sync-configured" if flags.is_empty() && accepted => install_configured(Some(staging)),
        // Explicit staging activation of a package installed with
        // `install --staging --kind cockpit` into this host's Cockpit store.
        "activate" if staging && accepted && flags.len() == 4 => {
            let get = |name| flags.get(name).copied().ok_or(InstallError::Usage);
            let root = Path::new(get("--root")?);
            let selected = get("--manifest-sha256")?;
            if get("--expected-active")? != selected
                || root != bootstrap::cockpit_root()?
                || bootstrap::production_key(root)?.is_some()
            {
                return Err(InstallError::Usage);
            }
            let _guard = install::configuration_guard(root)?;
            let result = activate_locked(root, selected, get("--trust-key-hex")?)?;
            retire();
            Ok(result)
        }
        _ => Err(InstallError::Usage),
    }
}

/// One started Cockpit process: its stdin stays open while it runs.
pub(super) struct Process {
    child: Child,
    stdin: Option<ChildStdin>,
    port: u16,
}

impl Process {
    /// Close stdin, give the Cockpit `grace` to end, then kill and collect
    /// it. True when it ended by itself. Repeated calls do nothing.
    pub(super) fn stop(&mut self, grace: Duration) -> bool {
        drop(self.stdin.take());
        let deadline = Instant::now() + grace;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return true,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    return false;
                }
            }
        }
    }

    fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.stop(EXIT_GRACE);
    }
}

/// Spawn `command`, hand it the start line and read its port within
/// `deadline`. A start that does not name its port in time is killed.
pub(super) fn handshake(
    mut command: Command,
    start: &[u8],
    deadline: Duration,
) -> std::result::Result<Process, CockpitUnavailable> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Nothing the Cockpit writes there is shown; it could repeat a token.
        .stderr(Stdio::null());
    let child = command
        .spawn()
        .map_err(|_| CockpitUnavailable::StartFailed)?;
    let mut process = Process {
        child,
        stdin: None,
        port: 0,
    };
    let (Some(mut stdin), Some(stdout)) = (process.child.stdin.take(), process.child.stdout.take())
    else {
        process.stop(Duration::ZERO);
        return Err(CockpitUnavailable::StartFailed);
    };
    if stdin.write_all(start).and_then(|()| stdin.flush()).is_err() {
        process.stop(Duration::ZERO);
        return Err(CockpitUnavailable::StartFailed);
    }
    process.stdin = Some(stdin);
    let (sender, receiver) = mpsc::sync_channel(1);
    let reader = std::thread::Builder::new()
        .name("cockpit-output".to_owned())
        .spawn(move || {
            let mut output = BufReader::new(stdout);
            let mut line = Vec::new();
            let read = (&mut output)
                .take(MAX_PORT_LINE)
                .read_until(b'\n', &mut line);
            let _ = sender.send(read.ok().map(|_| line));
            // Drained to its end, so the Cockpit never blocks on a full pipe.
            let _ = std::io::copy(&mut output, &mut std::io::sink());
        });
    let port = reader
        .ok()
        .and_then(|_| receiver.recv_timeout(deadline).ok().flatten())
        .and_then(|line| listening(&line));
    if let Some(port) = port {
        process.port = port;
        Ok(process)
    } else {
        process.stop(Duration::ZERO);
        Err(CockpitUnavailable::StartFailed)
    }
}

/// The port of a terminated `{"listening":<port>}` line.
fn listening(line: &[u8]) -> Option<u16> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Listening {
        listening: u16,
    }
    if line.last() != Some(&b'\n') {
        return None;
    }
    serde_json::from_slice::<Listening>(line)
        .ok()
        .map(|listening| listening.listening)
        .filter(|port| *port != 0)
}

struct Running {
    process: Process,
    /// The private directory the authenticated binary runs from.
    prepared: Option<super::health::Prepared>,
    cockpit_token: String,
    launch: [u8; 32],
    started: Instant,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.process.stop(EXIT_GRACE);
        if let Some(prepared) = self.prepared.take() {
            let _ = prepared.close();
        }
    }
}

struct State {
    running: Option<Running>,
    /// Failures in a row.
    failures: usize,
    retry_at: Option<Instant>,
    last_failure: CockpitUnavailable,
}

static STATE: Mutex<State> = Mutex::new(State {
    running: None,
    failures: 0,
    retry_at: None,
    last_failure: CockpitUnavailable::StartFailed,
});

fn launch_key(launch: &CockpitLaunch) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&launch.engine_port.to_le_bytes());
    hasher.update(&(launch.engine_token.len() as u64).to_le_bytes());
    hasher.update(launch.engine_token.as_bytes());
    hasher.update(launch.project_root.as_os_str().as_encoded_bytes());
    *hasher.finalize().as_bytes()
}

/// The running Cockpit of this process, started when none runs. Holds the
/// supervisor lock across a start, so concurrent callers share one start.
pub(crate) fn cockpit_endpoint(
    launch: &CockpitLaunch,
) -> std::result::Result<CockpitEndpoint, CockpitUnavailable> {
    let key = launch_key(launch);
    let mut state = STATE.lock().unwrap_or_else(PoisonError::into_inner);
    let now = Instant::now();
    if let Some(running) = state.running.as_mut() {
        if running.process.running() {
            if running.launch == key {
                return Ok(CockpitEndpoint {
                    port: running.process.port,
                    cockpit_token: running.cockpit_token.clone(),
                });
            }
            // Started for another dashboard: replaced, nothing failed.
            state.running = None;
        } else {
            let stable = running.started.elapsed() >= STABLE;
            state.running = None;
            if stable {
                state.failures = 0;
            }
            tracing::warn!("cockpit exited; restarting after backoff");
            fail(&mut state, CockpitUnavailable::StartFailed, now);
        }
    }
    if state.retry_at.is_some_and(|at| now < at) {
        return Err(state.last_failure);
    }
    match start(launch, key) {
        Ok(running) => {
            let endpoint = CockpitEndpoint {
                port: running.process.port,
                cockpit_token: running.cockpit_token.clone(),
            };
            state.retry_at = None;
            state.running = Some(running);
            Ok(endpoint)
        }
        // Nothing to retry: the user installs or activates it.
        Err(
            reason @ (CockpitUnavailable::NotInstalled
            | CockpitUnavailable::Disabled
            | CockpitUnavailable::SourceBuild),
        ) => Err(reason),
        Err(reason) => {
            tracing::warn!("cockpit not started: {}", reason.as_str());
            fail(&mut state, reason, now);
            Err(reason)
        }
    }
}

fn fail(state: &mut State, reason: CockpitUnavailable, now: Instant) {
    let wait = RESTART_BACKOFF[state.failures.min(RESTART_BACKOFF.len() - 1)];
    state.failures = state.failures.saturating_add(1);
    state.retry_at = Some(now + wait);
    state.last_failure = reason;
}

/// Stop this process's Cockpit, if any; the next request starts the
/// currently activated package without waiting for a backoff.
pub(crate) fn retire() {
    let mut state = STATE.lock().unwrap_or_else(PoisonError::into_inner);
    state.running = None;
    state.failures = 0;
    state.retry_at = None;
}

fn start(
    launch: &CockpitLaunch,
    key: [u8; 32],
) -> std::result::Result<Running, CockpitUnavailable> {
    import_bundled();
    let (root, package) = match authenticated() {
        Err(CockpitUnavailable::NotInstalled)
            if bootstrap::cockpit_channel().ok().flatten().is_none() =>
        {
            return Err(CockpitUnavailable::SourceBuild);
        }
        other => other?,
    };
    let mut token = [0_u8; 32];
    getrandom::fill(&mut token).map_err(|_| CockpitUnavailable::StartFailed)?;
    let cockpit_token = hex::encode(token);
    let start_line = start_line(launch, &cockpit_token).ok_or(CockpitUnavailable::StartFailed)?;
    let (prepared, command) = super::health::prepare_binary(&package.binary, &root, EXECUTABLE)
        .map_err(|_| CockpitUnavailable::StartFailed)?;
    match handshake(command, &start_line, HANDSHAKE) {
        Ok(process) => Ok(Running {
            process,
            prepared: Some(prepared),
            cockpit_token,
            launch: key,
            started: Instant::now(),
        }),
        Err(reason) => {
            let _ = prepared.close();
            Err(reason)
        }
    }
}

/// The one start line of contract `leanctx.cockpit-start/v1`.
fn start_line(launch: &CockpitLaunch, cockpit_token: &str) -> Option<Vec<u8>> {
    let engine_exe = std::env::current_exe().ok()?;
    // The Cockpit runs with a cleared environment: these absolute paths,
    // resolved exactly as the Engine resolves them, are how it finds them.
    let data_dir = crate::core::data_dir::resolve_data_dir().ok()?;
    let state_dir = crate::core::paths::state_dir().ok()?;
    if !data_dir.is_absolute() || !state_dir.is_absolute() {
        return None;
    }
    // Optional: the host session logs it reads live under the user's home.
    let home_dir = dirs::home_dir()
        .filter(|home| home.is_absolute())
        .and_then(|home| home.to_str().map(str::to_owned));
    // Optional: Engine stores it reads (e.g. path_mode_memory.json) live here.
    let cache_dir = crate::core::paths::cache_dir()
        .ok()
        .filter(|cache| cache.is_absolute())
        .and_then(|cache| cache.to_str().map(str::to_owned));
    // Optional: where the Engine reads its config, and the PATH its own CLI
    // calls (`gh`, `git`) resolve against.
    let config_dir = crate::core::paths::config_dir_read_only()
        .ok()
        .filter(|config| config.is_absolute())
        .and_then(|config| config.to_str().map(str::to_owned));
    let search_path = std::env::var("PATH").ok();
    let mut line = serde_json::to_vec(&serde_json::json!({
        "home_dir": home_dir,
        "engine_cache_dir": cache_dir,
        "engine_config_dir": config_dir,
        "search_path": search_path,
        "cockpit_token": cockpit_token,
        "engine_url": format!("http://127.0.0.1:{}", launch.engine_port),
        "engine_token": launch.engine_token,
        "engine_exe": engine_exe.to_str()?,
        "project_root": launch.project_root.to_str()?,
        "data_dir": data_dir.to_str()?,
        "engine_state_dir": state_dir.to_str()?,
        "engine_version": env!("CARGO_PKG_VERSION"),
    }))
    .ok()?;
    line.push(b'\n');
    Some(line)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn script(body: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(body);
        command
    }

    #[test]
    fn handshake_reads_the_port_keeps_stdin_open_and_the_cockpit_ends_at_eof() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().display();
        let mut process = handshake(
            script(&format!(
                "IFS= read -r line; printf '%s' \"$line\" > '{path}/start'; \
                 echo '{{\"listening\":4242}}'; cat > /dev/null; touch '{path}/eof'"
            )),
            b"{\"cockpit_token\":\"t\"}\n",
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(process.port, 4242);
        assert_eq!(
            std::fs::read_to_string(directory.path().join("start")).unwrap(),
            "{\"cockpit_token\":\"t\"}"
        );
        // Alive while the supervisor holds stdin, not merely not yet ended.
        std::thread::sleep(Duration::from_millis(300));
        assert!(process.running());
        assert!(!directory.path().join("eof").exists());
        assert!(
            process.stop(Duration::from_secs(5)),
            "ended by itself at EOF"
        );
        assert!(directory.path().join("eof").exists());
    }

    fn tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (path, bytes) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, *bytes).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn a_bundle_is_read_from_its_directory_or_tarball_and_nothing_else() {
        let directory = tempfile::tempdir().unwrap();
        let files: Vec<(&str, Vec<u8>)> = BUNDLE_FILES
            .iter()
            .map(|(name, _)| (*name, name.as_bytes().to_vec()))
            .collect();
        let bundle = directory.path().join("lean-ctx-cockpit");
        std::fs::create_dir(&bundle).unwrap();
        for (name, bytes) in &files {
            std::fs::write(bundle.join(name), bytes).unwrap();
        }
        let from_dir = read_bundle(&bundle).unwrap();
        assert_eq!(
            (from_dir.catalog.as_slice(), from_dir.archive.as_slice()),
            (b"channel.json".as_slice(), b"archive.tar.gz".as_slice())
        );

        let tarball = directory.path().join("bundle.tar.gz");
        let nested: Vec<(String, &[u8])> = files
            .iter()
            .map(|(name, bytes)| (format!("lean-ctx-cockpit/{name}"), bytes.as_slice()))
            .collect();
        let entries: Vec<(&str, &[u8])> = nested.iter().map(|(p, b)| (p.as_str(), *b)).collect();
        std::fs::write(&tarball, tar_gz(&entries)).unwrap();
        let from_tar = read_bundle(&tarball).unwrap();
        assert_eq!(from_tar.manifest, b"manifest.json");

        // A missing, unknown, duplicated, oversized or deeper member is refused.
        let refused: [Vec<(&str, &[u8])>; 5] = [
            entries[..4].to_vec(),
            [
                entries.clone(),
                vec![("lean-ctx-cockpit/extra", b"x".as_slice())],
            ]
            .concat(),
            [
                entries.clone(),
                vec![("other/channel.json", b"y".as_slice())],
            ]
            .concat(),
            [
                vec![entries[0]],
                entries[2..].to_vec(),
                vec![("lean-ctx-cockpit/channel.sig", [0_u8; 65].as_slice())],
            ]
            .concat(),
            [
                entries[1..].to_vec(),
                vec![("a/b/channel.json", b"z".as_slice())],
            ]
            .concat(),
        ];
        for members in refused {
            std::fs::write(&tarball, tar_gz(&members)).unwrap();
            assert!(read_bundle(&tarball).is_err());
        }
    }

    #[test]
    fn a_start_that_never_names_its_port_fails_within_the_deadline() {
        for body in ["exec sleep 30", "echo 'listening on 4242'; exec sleep 30"] {
            let begun = Instant::now();
            let refused = handshake(script(body), b"{}\n", Duration::from_millis(300));
            assert_eq!(refused.err(), Some(CockpitUnavailable::StartFailed));
            assert!(begun.elapsed() < Duration::from_secs(5), "{body}");
        }
    }

    #[test]
    fn a_failed_automatic_install_is_retried_after_a_day() {
        let now = std::time::SystemTime::now();
        assert!(!retry_blocked(None, now));
        assert!(retry_blocked(Some(now - Duration::from_secs(60)), now));
        assert!(retry_blocked(
            Some(now - AUTO_RETRY + Duration::from_secs(1)),
            now
        ));
        assert!(!retry_blocked(Some(now - AUTO_RETRY), now));
        // A clock that went backwards never blocks.
        assert!(!retry_blocked(Some(now + Duration::from_secs(60)), now));
    }

    #[test]
    fn no_cockpit_opt_out_and_a_source_build_install_nothing() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _off = crate::test_env::VarGuard::set("LEAN_CTX_NO_COCKPIT", "1");
        assert!(installs_opted_out());
        assert!(!install_if_missing(true));
        drop(_off);
        let _on = crate::test_env::VarGuard::set("LEAN_CTX_NO_COCKPIT", "0");
        assert!(!installs_opted_out());
        // A test build carries no Cockpit channel: nothing to install from.
        assert!(!install_if_missing(true));
    }
}
