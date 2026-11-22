// SPDX-License-Identifier: Apache-2.0
//! The SSH side of `lean-ctx cloud computer`: the local registry of cloud
//! computers, their pinned host keys, the `~/.ssh/leanctx/` config and
//! known_hosts files, and the `ssh` arguments that enforce the pin.
//!
//! Code never travels through LeanCTX Cloud: this module only ever talks to
//! the computer itself over SSH. Nothing here contacts the Cloud API.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// The schema of the local registry file.
pub(crate) const REGISTRY_SCHEMA: &str = "leanctx.cloud.computers.local/v1";
/// The line that makes `~/.ssh/config` read the LeanCTX entries.
pub(crate) const INCLUDE_LINE: &str = "Include ~/.ssh/leanctx/config";

/// Host key algorithms a computer may be pinned to.
const KEY_TYPES: &[&str] = &[
    "ssh-ed25519",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
    "ssh-rsa",
    "sk-ssh-ed25519@openssh.com",
    "sk-ecdsa-sha2-nistp256@openssh.com",
];

/// One public host key, checked: its algorithm matches the key blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostKey {
    pub key_type: String,
    blob: Vec<u8>,
}

impl HostKey {
    /// `<type> <base64> [comment]`, as in a `.pub` file or `ssh-keyscan`
    /// output without its host field.
    pub(crate) fn parse(line: &str) -> Result<Self, String> {
        let mut parts = line.split_whitespace();
        let (Some(key_type), Some(encoded)) = (parts.next(), parts.next()) else {
            return Err("a host key is \"<type> <base64>\"".into());
        };
        if !KEY_TYPES.contains(&key_type) {
            return Err(format!("unsupported host key type '{key_type}'"));
        }
        let blob = STANDARD
            .decode(encoded)
            .map_err(|_| format!("the {key_type} host key is not valid base64"))?;
        if blob_type(&blob) != Some(key_type.as_bytes()) {
            return Err(format!("the host key blob is not a {key_type} key"));
        }
        Ok(Self {
            key_type: key_type.to_owned(),
            blob,
        })
    }

    /// `SHA256:<base64 without padding>`, as `ssh-keygen -l` prints it.
    pub(crate) fn fingerprint(&self) -> String {
        format!(
            "SHA256:{}",
            STANDARD_NO_PAD.encode(&Sha256::digest(&self.blob)[..])
        )
    }

    /// `<type> <base64>`, without a comment.
    pub(crate) fn openssh(&self) -> String {
        format!("{} {}", self.key_type, STANDARD.encode(&self.blob))
    }
}

/// The algorithm name an SSH key blob starts with (a length-prefixed string).
fn blob_type(blob: &[u8]) -> Option<&[u8]> {
    let len = u32::from_be_bytes(blob.get(..4)?.try_into().ok()?) as usize;
    blob.get(4..4usize.checked_add(len)?)
}

/// `SHA256:` followed by 43 base64 characters (a SHA-256 digest).
pub(crate) fn check_fingerprint(text: &str) -> Result<(), String> {
    let digest = text
        .strip_prefix("SHA256:")
        .ok_or_else(|| format!("'{text}' is not a SHA256: fingerprint"))?;
    match STANDARD_NO_PAD.decode(digest) {
        Ok(bytes) if bytes.len() == 32 => Ok(()),
        _ => Err(format!("'{text}' is not a SHA256: fingerprint")),
    }
}

/// The keys of `ssh-keyscan` output (`<host> <type> <base64>` lines; `#`
/// comments and unknown types are skipped) whose fingerprint is one of
/// `wanted`. Every wanted fingerprint must be found, or nothing is pinned.
pub(crate) fn keys_matching(keyscan: &str, wanted: &[String]) -> Result<Vec<HostKey>, String> {
    let mut found = Vec::new();
    let mut seen = BTreeSet::new();
    for line in keyscan.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((_host, key)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let Ok(key) = HostKey::parse(key) else {
            continue;
        };
        let fingerprint = key.fingerprint();
        if wanted.contains(&fingerprint) && seen.insert(fingerprint) {
            found.push(key);
        }
    }
    let missing: Vec<&str> = wanted
        .iter()
        .filter(|f| !seen.contains(*f))
        .map(String::as_str)
        .collect();
    if missing.is_empty() {
        Ok(found)
    } else {
        Err(format!(
            "the computer did not offer a host key with fingerprint {}; nothing was pinned",
            missing.join(", ")
        ))
    }
}

/// A computer name: 1-31 of `a-z 0-9 -`, starting with a letter or digit
/// (the cloud's own rule, `^[a-z0-9][a-z0-9-]{0,30}$`).
pub(crate) fn check_name(name: &str) -> Result<(), String> {
    let ok = (1..=31).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "computer name '{name}': use 1-31 of a-z, 0-9 and -, not starting with -"
        ))
    }
}

/// A DNS name, IPv4 or IPv6 address; never an option or an SSH pattern.
pub(crate) fn check_hostname(host: &str) -> Result<(), String> {
    let ok = (1..=253).contains(&host.len())
        && !host.starts_with('-')
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':'));
    if ok {
        Ok(())
    } else {
        Err(format!("host '{host}' is not a host name or IP address"))
    }
}

/// A POSIX user name.
pub(crate) fn check_user(user: &str) -> Result<(), String> {
    let mut bytes = user.bytes();
    let ok = (1..=32).contains(&user.len())
        && bytes
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b == b'_')
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(format!("user '{user}' is not a valid user name"))
    }
}

/// A path that can stand in an SSH config line (quoted): no quote, no
/// control character.
pub(crate) fn check_config_path(path: &str) -> Result<(), String> {
    if path.is_empty() || path.chars().any(|c| c == '"' || c.is_control()) {
        Err(format!(
            "path '{path}' cannot be written into an SSH config"
        ))
    } else {
        Ok(())
    }
}

/// One registered computer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Computer {
    pub name: String,
    pub hostname: String,
    pub user: String,
    pub port: u16,
    /// The private key SSH offers, when the person named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_file: Option<String>,
    /// Pinned public host keys, `<type> <base64>`.
    pub host_keys: Vec<String>,
}

impl Computer {
    /// The SSH host alias, `leanctx-<name>`.
    pub(crate) fn alias(&self) -> String {
        alias(&self.name)
    }

    /// The fingerprints of the pinned host keys, in pin order.
    pub(crate) fn fingerprints(&self) -> Vec<String> {
        self.host_keys
            .iter()
            .filter_map(|k| HostKey::parse(k).ok())
            .map(|k| k.fingerprint())
            .collect()
    }
}

/// The SSH host alias of computer `name`.
pub(crate) fn alias(name: &str) -> String {
    format!("leanctx-{name}")
}

/// The local registry of computers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Registry {
    pub schema: String,
    pub computers: Vec<Computer>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            schema: REGISTRY_SCHEMA.into(),
            computers: Vec::new(),
        }
    }
}

/// What [`Registry::upsert`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Upsert {
    Added,
    Updated,
    Unchanged,
}

impl Registry {
    pub(crate) fn get(&self, name: &str) -> Option<&Computer> {
        self.computers.iter().find(|c| c.name == name)
    }

    /// Add `computer`, or replace the one with its name. Replacing a
    /// computer whose host, port or pinned keys differ needs `replace`:
    /// a changed host key is exactly what the pin exists to refuse.
    pub(crate) fn upsert(&mut self, computer: Computer, replace: bool) -> Result<Upsert, String> {
        let Some(existing) = self.computers.iter_mut().find(|c| c.name == computer.name) else {
            self.computers.push(computer);
            self.computers.sort_by(|a, b| a.name.cmp(&b.name));
            return Ok(Upsert::Added);
        };
        if *existing == computer {
            return Ok(Upsert::Unchanged);
        }
        let repinned = existing.hostname != computer.hostname
            || existing.port != computer.port
            || existing.host_keys != computer.host_keys;
        if repinned && !replace {
            return Err(format!(
                "computer '{}' is already pinned to {}:{} with other host keys; \
                 pass --replace only if you know why the computer changed",
                existing.name, existing.hostname, existing.port
            ));
        }
        *existing = computer;
        Ok(Upsert::Updated)
    }

    /// Remove computer `name`; whether it existed.
    pub(crate) fn remove(&mut self, name: &str) -> bool {
        let before = self.computers.len();
        self.computers.retain(|c| c.name != name);
        self.computers.len() != before
    }

    /// The registry at `path`; empty when the file does not exist.
    pub(crate) fn load(path: &Path) -> Result<Self, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        let registry: Self = serde_json::from_str(&text)
            .map_err(|e| format!("{} is not a computer registry: {e}", path.display()))?;
        if registry.schema != REGISTRY_SCHEMA {
            return Err(format!(
                "{} has schema '{}', expected '{REGISTRY_SCHEMA}'",
                path.display(),
                registry.schema
            ));
        }
        Ok(registry)
    }

    pub(crate) fn save(&self, path: &Path) -> Result<(), String> {
        let mut text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        text.push('\n');
        write_private(path, text.as_bytes())
    }
}

/// Where the files live.
#[derive(Debug, Clone)]
pub(crate) struct Paths {
    /// `<data dir>/cloud/computers.json`.
    pub registry: PathBuf,
    /// `~/.ssh/config`.
    pub ssh_config: PathBuf,
    /// `~/.ssh/leanctx/config`.
    pub leanctx_config: PathBuf,
    /// `~/.ssh/leanctx/known_hosts`.
    pub known_hosts: PathBuf,
}

impl Paths {
    pub(crate) fn new(data_dir: &Path, home: &Path) -> Self {
        let ssh = home.join(".ssh");
        Self {
            registry: data_dir.join("cloud").join("computers.json"),
            ssh_config: ssh.join("config"),
            leanctx_config: ssh.join("leanctx").join("config"),
            known_hosts: ssh.join("leanctx").join("known_hosts"),
        }
    }
}

/// The known_hosts file: one line per pinned key, keyed by the host alias
/// (`HostKeyAlias`), so the pin does not depend on the address.
pub(crate) fn render_known_hosts(registry: &Registry) -> String {
    let mut out = String::from("# Written by lean-ctx cloud computer; edits are replaced.\n");
    for computer in &registry.computers {
        for key in computer
            .host_keys
            .iter()
            .filter_map(|k| HostKey::parse(k).ok())
        {
            let _ = writeln!(out, "{} {}", computer.alias(), key.openssh());
        }
    }
    out
}

/// One `Host leanctx-<name>` block (spec §1a).
pub(crate) fn render_host_block(computer: &Computer, known_hosts: &Path) -> String {
    let alias = computer.alias();
    let mut out = format!("Host {alias}\n");
    let _ = writeln!(out, "  HostName {}", computer.hostname);
    let _ = writeln!(out, "  User {}", computer.user);
    let _ = writeln!(out, "  Port {}", computer.port);
    if let Some(identity) = &computer.identity_file {
        let _ = writeln!(out, "  IdentityFile \"{identity}\"");
        out.push_str("  IdentitiesOnly yes\n");
    }
    out.push_str("  StrictHostKeyChecking yes\n");
    let _ = writeln!(out, "  UserKnownHostsFile \"{}\"", known_hosts.display());
    let _ = writeln!(out, "  HostKeyAlias {alias}");
    out.push_str("  ForwardAgent no\n");
    out
}

/// `~/.ssh/leanctx/config`: every registered computer.
pub(crate) fn render_config(registry: &Registry, known_hosts: &Path) -> String {
    let mut out = String::from("# Written by lean-ctx cloud computer; edits are replaced.\n");
    for computer in &registry.computers {
        out.push('\n');
        out.push_str(&render_host_block(computer, known_hosts));
    }
    out
}

/// `~/.ssh/config` with [`INCLUDE_LINE`] as its first line, or `None` when
/// it already includes the LeanCTX config. It must come first: below a
/// `Host` line an `Include` only applies to that host.
pub(crate) fn with_include(ssh_config: &str) -> Option<String> {
    if has_include(ssh_config) {
        return None;
    }
    Some(format!("{INCLUDE_LINE}\n{ssh_config}"))
}

/// Whether `ssh_config` includes the LeanCTX config at top level.
pub(crate) fn has_include(ssh_config: &str) -> bool {
    for line in ssh_config.lines().map(str::trim) {
        if line == INCLUDE_LINE {
            return true;
        }
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("host ") || lower.starts_with("match ") {
            return false;
        }
    }
    false
}

/// Whether the SSH entry of `name` is installed: the LeanCTX config names
/// its host and `~/.ssh/config` includes that config.
pub(crate) fn config_written(paths: &Paths, name: &str) -> bool {
    let block = format!("Host {}", alias(name));
    let config = std::fs::read_to_string(&paths.leanctx_config).unwrap_or_default();
    let main = std::fs::read_to_string(&paths.ssh_config).unwrap_or_default();
    config.lines().any(|l| l.trim() == block) && has_include(&main)
}

/// Whether the SSH entry of `name` is installed and points at `hostname`
/// as `user` (and at `port`, when given): a local entry of the same name
/// for another host or user does not count.
pub(super) fn config_written_to(
    paths: &Paths,
    name: &str,
    hostname: &str,
    user: &str,
    port: Option<u16>,
) -> bool {
    let Ok(registry) = Registry::load(&paths.registry) else {
        return false;
    };
    registry.get(name).is_some_and(|c| {
        c.hostname.eq_ignore_ascii_case(hostname)
            && c.user == user
            && port.is_none_or(|p| p == c.port)
    }) && config_written(paths, name)
}

/// Rewrite `~/.ssh/leanctx/known_hosts` from the registry (our own file).
pub(crate) fn write_known_hosts(paths: &Paths, registry: &Registry) -> Result<(), String> {
    write_private(&paths.known_hosts, render_known_hosts(registry).as_bytes())
}

/// Install the SSH entries: our config and known_hosts, and the `Include`
/// line at the top of `~/.ssh/config`.
pub(crate) fn write_ssh_config(paths: &Paths, registry: &Registry) -> Result<(), String> {
    write_known_hosts(paths, registry)?;
    write_private(
        &paths.leanctx_config,
        render_config(registry, &paths.known_hosts).as_bytes(),
    )?;
    let main = read_or_empty(&paths.ssh_config)?;
    if let Some(updated) = with_include(&main) {
        write_user_file(&paths.ssh_config, &main, updated.as_bytes())?;
    }
    Ok(())
}

/// Keep installed SSH entries in step with the registry: known_hosts
/// always, our config only once the person installed it.
pub(crate) fn refresh(paths: &Paths, registry: &Registry) -> Result<(), String> {
    write_known_hosts(paths, registry)?;
    if paths.leanctx_config.exists() {
        write_private(
            &paths.leanctx_config,
            render_config(registry, &paths.known_hosts).as_bytes(),
        )?;
    }
    Ok(())
}

/// The `ssh` arguments that reach `computer` with the pin enforced,
/// whatever `~/.ssh/config` says (command line options win), followed by
/// the remote command, if any.
pub(crate) fn ssh_args(computer: &Computer, known_hosts: &Path, command: &[String]) -> Vec<String> {
    let alias = computer.alias();
    let mut args = vec![
        "-o".to_owned(),
        "StrictHostKeyChecking=yes".to_owned(),
        "-o".to_owned(),
        format!("UserKnownHostsFile=\"{}\"", known_hosts.display()),
        "-o".to_owned(),
        format!("HostKeyAlias={alias}"),
        "-o".to_owned(),
        "ForwardAgent=no".to_owned(),
        "-p".to_owned(),
        computer.port.to_string(),
    ];
    if let Some(identity) = &computer.identity_file {
        args.extend([
            "-i".to_owned(),
            identity.clone(),
            "-o".to_owned(),
            "IdentitiesOnly=yes".to_owned(),
        ]);
    }
    args.push(format!("{}@{}", computer.user, computer.hostname));
    if !command.is_empty() {
        args.push("--".to_owned());
        args.extend(command.iter().cloned());
    }
    args
}

/// The remote command of `deploy-key`: create an ed25519 key on the
/// computer once (never overwritten) and print its public half.
pub(crate) fn deploy_key_command(name: &str) -> String {
    format!(
        "set -eu; umask 077; k=\"$HOME/.ssh/leanctx_deploy_ed25519\"; mkdir -p \"$HOME/.ssh\"; \
         [ -f \"$k\" ] || ssh-keygen -q -t ed25519 -N '' -C 'leanctx-deploy@{name}' -f \"$k\"; \
         cat \"$k.pub\""
    )
}

/// The text of `path`; empty when the file does not exist.
fn read_or_empty(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// Write `bytes` to `path` (one of LeanCTX's own files) through a new
/// temporary file and a rename, readable by the owner only; parent
/// directories are created owner-only.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    create_private_dir(parent)?;
    replace_with(path, path, bytes, 0o600, || Ok(()))
}

/// Write `bytes` over the person's own file `path` (`~/.ssh/config`),
/// whose text was `original` when it was read.
///
/// - A symlink (stow, chezmoi, home-manager) stays a symlink: its target
///   is written. A link that cannot be resolved is refused.
/// - The file keeps its mode (a new file gets 0600).
/// - The new text goes to a new temporary file (`O_EXCL`) next to the real
///   target and is renamed onto it, only if the file still reads
///   `original`; a concurrent edit is never overwritten.
pub(super) fn write_user_file(path: &Path, original: &str, bytes: &[u8]) -> Result<(), String> {
    let by_hand = || format!("add the line '{INCLUDE_LINE}' at the top of it by hand");
    let target = match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => std::fs::canonicalize(path).map_err(|e| {
            format!(
                "{} is a symbolic link whose target cannot be resolved ({e}); {}",
                path.display(),
                by_hand()
            )
        })?,
        Ok(_) => path.to_path_buf(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => path.to_path_buf(),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let mode = match std::fs::metadata(&target) {
        Ok(meta) if !meta.is_file() => {
            return Err(format!(
                "{} is not a regular file; {}",
                target.display(),
                by_hand()
            ));
        }
        Ok(meta) => file_mode(&meta),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let parent = target
                .parent()
                .ok_or_else(|| format!("{} has no parent directory", target.display()))?;
            create_private_dir(parent)?;
            0o600
        }
        Err(e) => return Err(format!("cannot read {}: {e}", target.display())),
    };
    replace_with(path, &target, bytes, mode, || {
        if read_or_empty(&target)? == original {
            Ok(())
        } else {
            Err(format!(
                "{} changed while lean-ctx was writing it; nothing was written, run the command again",
                path.display()
            ))
        }
    })
}

#[cfg(unix)]
fn file_mode(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    meta.permissions().mode() & 0o7777
}

#[cfg(not(unix))]
fn file_mode(_meta: &std::fs::Metadata) -> u32 {
    0o600
}

/// Write `bytes` to a new temporary file next to `target` with `mode`,
/// then, if `still_ok` agrees, rename it onto `target`. Errors name `shown`.
fn replace_with(
    shown: &Path,
    target: &Path,
    bytes: &[u8],
    mode: u32,
    still_ok: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    use std::io::Write as _;
    let (tmp, mut file) =
        new_temp(target, mode).map_err(|e| format!("cannot write {}: {e}", shown.display()))?;
    let written = (|| -> std::io::Result<()> {
        file.write_all(bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            // The umask may have narrowed the mode `open` was given.
            file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        }
        file.sync_all()
    })();
    drop(file);
    let result = written
        .map_err(|e| format!("cannot write {}: {e}", shown.display()))
        .and_then(|()| still_ok())
        .and_then(|()| {
            std::fs::rename(&tmp, target)
                .map_err(|e| format!("cannot write {}: {e}", shown.display()))
        });
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// A new file (`O_EXCL`) next to `target`, under a name no other writer
/// uses.
fn new_temp(target: &Path, mode: u32) -> std::io::Result<(PathBuf, std::fs::File)> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let file_name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut last = None;
    for _ in 0..16 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let tmp = parent.join(format!(
            ".{file_name}.tmp-{}-{}-{nanos:08x}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(mode);
        }
        #[cfg(not(unix))]
        let _ = mode;
        match options.open(&tmp) {
            Ok(file) => return Ok((tmp, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no free temporary file name")))
}

fn create_private_dir(dir: &Path) -> Result<(), String> {
    if dir.is_dir() {
        return Ok(());
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .map_err(|e| format!("cannot create {}: {e}", dir.display()))
}

#[cfg(test)]
#[path = "cloud_computer_ssh_tests.rs"]
mod tests;
