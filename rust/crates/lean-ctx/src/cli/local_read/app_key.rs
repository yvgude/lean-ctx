// SPDX-License-Identifier: Apache-2.0
//! The app key: what tells a Local Service request of LeanCTX Code apart
//! from one of any other process of the same user.
//!
//! The daemon socket is 0600, so every process of the person can reach it,
//! the agents it runs included. A few writes act only after the person's
//! confirmed click in LeanCTX Code (`decisions.send`, "Send to agents").
//! Those routes are app-only:
//!
//! - At each launch LeanCTX Code writes a fresh random key (64 lowercase hex
//!   characters) to `<data dir>/agents/app-launch.key`, mode 0600, and sends
//!   it in the `x-leanctx-app-key` header of an app-only request.
//! - On each app-only request the daemon reads that file and compares it
//!   with the header in constant time. No file, no header, a key that is no
//!   key, or another key: the request is refused `403 app_only` before
//!   anything is done. A running daemon takes a new key without a restart.
//! - The daemon takes the key only from the person's own private regular
//!   file (mode 0600, no symlink, no FIFO, at most 65 bytes).
//! - The engine's read, search and listing tools never open the file, under
//!   every role and boundary mode, admin and warn included
//!   ([`lean_ctx::core::io_boundary::app_only_secret`]). It lies in the agent
//!   records, which the shell policy and the native write guard keep agents
//!   out of. The CLI has no verb for these routes.

use std::path::{Path, PathBuf};

use super::{Code, Failure};

/// The request header that carries the app key.
pub(crate) const HEADER: &str = "x-leanctx-app-key";

/// The key file's name in the agent records directory.
pub(crate) const FILE: &str = lean_ctx::core::io_boundary::APP_KEY_FILE;

/// The kinds only LeanCTX Code may ask for. `chat.prepare` (rc4-32): no
/// other process may fill the prepared-frame store and push the app's
/// frames out of it. The person task kinds (rc4-67): no other process may
/// propose a task in the person's name.
pub(crate) const APP_ONLY: &[&str] = &[
    super::decisions_part::SEND,
    super::decisions_part::SUPERSEDE,
    super::decisions_part::REVOKE,
    super::tasks_part::PERSON_PROPOSE,
    super::tasks_part::PERSON_PREPARE,
    super::chat_sign::PREPARE,
    super::pickup_part::STATUS,
    super::pickup_part::SET,
    super::pickup_part::KEY,
    super::unlink_part::KIND,
];

/// Whether `kind` is answered only for a request with the app key.
pub(crate) fn guards(kind: &str) -> bool {
    APP_ONLY.contains(&kind)
}

/// The key file of this data directory.
pub(crate) fn path() -> Option<PathBuf> {
    lean_ctx::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|dir| dir.join("agents").join(FILE))
}

/// The most a key file may hold: the key and one newline.
const MAX_FILE: u64 = 65;

/// Whether `given` (the header's value) is the key in `file`. Blocking
/// (file I/O): the Local Service calls it off its async loop.
pub(crate) fn admits_in(file: &Path, given: Option<&[u8]>) -> bool {
    let Some(given) = given else {
        return false;
    };
    let Some(held) = read_held(file) else {
        return false;
    };
    let held = held.trim_ascii();
    if !is_key(held) {
        return false;
    }
    constant_time_eq(held, given.trim_ascii())
}

/// Whether `given` is the key of this data directory.
pub(crate) fn admits(given: Option<&[u8]>) -> bool {
    path().is_some_and(|file| admits_in(&file, given))
}

/// The refusal of an app-only request without the app key.
pub(crate) fn refusal() -> Failure {
    Failure::new(
        Code::AppOnly,
        "only LeanCTX Code sends this, after the person confirms it",
    )
}

/// The environment variable LeanCTX Code passes its launch key in when it
/// runs a CLI command with an app-only flag (rc4-32 `cloud connect
/// --chat-key`, `--task-propose-key`, `--app-keys`).
pub(crate) const ENV: &str = "LEAN_CTX_APP_KEY";

/// The app key of this run, removed from the environment whether or not
/// one was set, so no later step and no child inherits it. Called first
/// thing by a command with app-only flags, before any refusal can return.
pub(crate) fn take_env_key() -> Option<Vec<u8>> {
    let given = std::env::var_os(ENV);
    // SAFETY: removed on the CLI's main thread before the command spawns
    // anything, as the enrollment token is.
    unsafe { std::env::remove_var(ENV) };
    given.map(|given| given.to_string_lossy().into_owned().into_bytes())
}

/// The one root an app-only command was admitted for (bd 38415/38432): the
/// config directory (its `cloud/` holds the Cloud records) and the data
/// directory (its `agents/` holds the key file), both with their symlinks
/// resolved. The command opens its records here and nowhere else: nothing is
/// resolved a second time after the gate, so markers or variables changed in
/// between cannot move it to other records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppRoot {
    pub(crate) config: PathBuf,
    pub(crate) data: PathBuf,
}

impl AppRoot {
    /// The key file of this root.
    pub(crate) fn key_file(&self) -> PathBuf {
        self.data.join("agents").join(FILE)
    }

    /// The Cloud records of this root: `<config>/cloud` and `<data>/cloud`,
    /// as [`lean_ctx_cloud::CloudStore::open`] would name them.
    pub(crate) fn store(&self) -> lean_ctx_cloud::CloudStore {
        lean_ctx_cloud::CloudStore::at(self.config.join("cloud"), self.data.join("cloud"))
    }
}

/// The shared gate of a CLI command's app-only flags (bd 38415/38432):
/// `Ok(None)` when `args` name none of `flags`; `Ok(Some(root))` when the
/// key file and the Cloud records share one root (`layout`, in production
/// [`checked_root`]) and `given` is the key in that root's key file
/// (`admits`, in production [`admits_in`]); otherwise [`refusal`]. A
/// command calls it before it reads stdin, makes a key or sends anything,
/// and then uses only the returned root.
pub(crate) fn app_only_gate(
    args: &[String],
    flags: &[&str],
    given: Option<&[u8]>,
    layout: impl FnOnce() -> Option<AppRoot>,
    admits: impl FnOnce(&Path, Option<&[u8]>) -> bool,
) -> Result<Option<AppRoot>, Failure> {
    if !args.iter().any(|a| flags.contains(&a.as_str())) {
        return Ok(None);
    }
    // The layout first: a key file somewhere else is not even read.
    let root = layout().ok_or_else(refusal)?;
    if admits(&root.key_file(), given) {
        Ok(Some(root))
    } else {
        Err(refusal())
    }
}

/// This process' root, when the key file ([`path`], under the data
/// directory) and the Cloud records (instance keys and chat key, under the
/// config directory) resolve under one root, so whoever wrote the key file
/// is the one the records belong to. Each directory is resolved once, here,
/// and taken as it really is (symlinks resolved). One root is either a
/// single directory for both (an isolated instance with its own
/// `LEAN_CTX_DATA_DIR`, a legacy `~/.lean-ctx` install) or the default split
/// under one home (`~/.config/lean-ctx` and `~/.local/share/lean-ctx`). A
/// data directory moved away from the config directory
/// (`LEAN_CTX_DATA_DIR`, `LEAN_CTX_CONFIG_DIR`, `XDG_DATA_HOME`,
/// `XDG_CONFIG_HOME`, `HOME` set apart) is refused: its key file would let
/// any process admit itself to the real records. So is a
/// `LEAN_CTX_DATA_DIR` naming the standard config directory, which would
/// collapse the real records onto a key file of its choosing.
pub(crate) fn checked_root() -> Option<AppRoot> {
    let config = lean_ctx::core::paths::config_dir_read_only().ok()?;
    let data = lean_ctx::core::data_dir::lean_ctx_data_dir().ok()?;
    let home = dirs::home_dir();
    let data_pin = std::env::var_os("LEAN_CTX_DATA_DIR")
        .map(PathBuf::from)
        .filter(|pin| !pin.as_os_str().to_string_lossy().trim().is_empty());
    let mut standard_configs: Vec<PathBuf> = home
        .iter()
        .map(|home| home.join(".config").join("lean-ctx"))
        .collect();
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        standard_configs.push(PathBuf::from(xdg).join("lean-ctx"));
    }
    checked_root_at(
        &config,
        &data,
        home.as_deref(),
        data_pin.as_deref(),
        &standard_configs,
    )
}

/// [`checked_root`] of explicit directories.
fn checked_root_at(
    config: &Path,
    data: &Path,
    home: Option<&Path>,
    data_pin: Option<&Path>,
    standard_configs: &[PathBuf],
) -> Option<AppRoot> {
    // A records or agents directory that is itself a link elsewhere is no root.
    if !plain_child(config, "cloud") || !plain_child(data, "agents") {
        return None;
    }
    let (config, data) = (real(config), real(data));
    // S1: a data pin on the standard config directory collapses the real
    // records onto it.
    if data_pin.is_some() && standard_configs.iter().any(|c| real(c) == data) {
        return None;
    }
    let split = home.map(real).is_some_and(|home| {
        config == home.join(".config").join("lean-ctx")
            && data == home.join(".local").join("share").join("lean-ctx")
    });
    (config == data || split).then_some(AppRoot { config, data })
}

/// `path` with its symlinks resolved. A part that does not exist yet keeps
/// its name below its nearest existing ancestor, which is resolved: a
/// directory made later lands there, and `/var/…` compares equal to
/// `/private/var/…` either way.
fn real(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut at = path;
    loop {
        if let Ok(found) = std::fs::canonicalize(at) {
            return rest.iter().rev().fold(found, |dir, name| dir.join(name));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                at = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Whether `dir/name`, if it exists, is that very directory and not a link.
fn plain_child(dir: &Path, name: &str) -> bool {
    let child = dir.join(name);
    match std::fs::symlink_metadata(&child) {
        Ok(meta) => !meta.file_type().is_symlink(),
        Err(_) => true,
    }
}

/// The key file's bytes, only when it is the person's own private regular
/// file: never through a symlink (`O_NOFOLLOW`), never waiting on a FIFO or
/// device (`O_NONBLOCK`, then a regular file only), mode 0600, owned by this
/// user, at most [`MAX_FILE`] bytes. Anything else is no key.
#[cfg(unix)]
fn read_held(file: &Path) -> Option<Vec<u8>> {
    use std::io::Read as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let opened = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(file)
        .ok()?;
    let meta = opened.metadata().ok()?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let me = unsafe { libc::geteuid() };
    if !meta.file_type().is_file()
        || meta.mode() & 0o7777 != 0o600
        || meta.uid() != me
        || meta.len() > MAX_FILE
    {
        return None;
    }
    let mut held = Vec::new();
    opened.take(MAX_FILE + 1).read_to_end(&mut held).ok()?;
    (held.len() as u64 <= MAX_FILE).then_some(held)
}

/// Windows: no `O_NOFOLLOW`; a symlink, junction or anything but a small
/// regular file is no key.
#[cfg(not(unix))]
fn read_held(file: &Path) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let meta = std::fs::symlink_metadata(file).ok()?;
    if lean_ctx::core::pathutil::is_symlink_or_reparse(&meta)
        || !meta.file_type().is_file()
        || meta.len() > MAX_FILE
    {
        return None;
    }
    let mut held = Vec::new();
    std::fs::File::open(file)
        .ok()?
        .take(MAX_FILE + 1)
        .read_to_end(&mut held)
        .ok()?;
    (held.len() as u64 <= MAX_FILE).then_some(held)
}

/// 64 lowercase hex characters: a key, never an empty or short file.
fn is_key(key: &[u8]) -> bool {
    key.len() == 64 && key.iter().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn key_file(content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(FILE);
        std::fs::write(&file, content).unwrap();
        private(&file);
        (dir, file)
    }

    #[cfg(unix)]
    fn private(file: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[cfg(not(unix))]
    fn private(_file: &Path) {}

    #[cfg(unix)]
    #[test]
    fn a_key_file_that_is_not_the_persons_private_file_admits_nothing() {
        use std::os::unix::fs::PermissionsExt as _;
        // Readable by others.
        let (_dir, file) = key_file(KEY);
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!admits_in(&file, Some(KEY.as_bytes())), "0644");
        // A symlink to a private key file.
        let (dir, target) = key_file(KEY);
        let link = dir.path().join("link.key");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(!admits_in(&link, Some(KEY.as_bytes())), "symlink");
        // Too long, though it starts with the key.
        let (_dir, file) = key_file(&format!("{KEY}\n\n\n"));
        assert!(!admits_in(&file, Some(KEY.as_bytes())), "oversize");
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_in_place_of_the_key_file_is_refused_without_waiting() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join(FILE);
        let c = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).unwrap();
        // SAFETY: a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(admits_in(&fifo, Some(KEY.as_bytes())));
        });
        let admitted = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("a FIFO never blocks the check");
        assert!(!admitted);
    }

    #[test]
    fn only_the_held_key_is_admitted() {
        let (_dir, file) = key_file(&format!("{KEY}\n"));
        assert!(admits_in(&file, Some(KEY.as_bytes())));
        assert!(!admits_in(&file, None), "no header");
        assert!(!admits_in(&file, Some(b"")), "an empty header");
        let other = KEY.replace('0', "1");
        assert!(!admits_in(&file, Some(other.as_bytes())), "another key");
        assert!(!admits_in(&file, Some(&KEY.as_bytes()[..63])), "a prefix");
    }

    #[test]
    fn without_a_key_file_nothing_is_admitted() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!admits_in(&dir.path().join(FILE), Some(KEY.as_bytes())));
        for held in ["", "\n", "short", &KEY.to_uppercase()] {
            let (_dir, file) = key_file(held);
            assert!(!admits_in(&file, Some(held.as_bytes())), "{held:?}");
        }
    }

    #[test]
    fn app_only_flags_need_one_root_for_the_key_file_and_the_records() {
        let base = tempfile::tempdir().unwrap();
        let home = base.path().join("home");
        let config = home.join(".config").join("lean-ctx");
        let data = home.join(".local").join("share").join("lean-ctx");
        let own = base.path().join("agent-tmp");
        for dir in [&config, &data, &own] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let standard = [config.clone()];
        let at = |c: &Path, d: &Path, h: Option<&Path>| checked_root_at(c, d, h, None, &standard);
        // The default split under one home, and one single directory.
        let split = at(&config, &data, Some(&home)).expect("default split");
        assert_eq!(split.config, real(&config));
        assert_eq!(split.data, real(&data));
        assert!(at(&own, &own, None).is_some());
        // bd 38415: LEAN_CTX_DATA_DIR=<own>, LEAN_CTX_CONFIG_DIR=<real>.
        assert!(at(&config, &own, Some(&home)).is_none());
        // HOME moved with XDG_CONFIG_HOME kept real.
        assert!(at(&config, &own.join("d"), Some(&own)).is_none());
        // S1: LEAN_CTX_DATA_DIR on the standard config directory collapses
        // the real records onto it.
        assert!(checked_root_at(&config, &config, Some(&home), Some(&config), &standard).is_none());
        #[cfg(unix)]
        {
            // A records directory linked to the real one is no root.
            std::os::unix::fs::symlink(&config, own.join("cloud")).unwrap();
            assert!(at(&own, &own, None).is_none());
            // Nor is a home whose `.config` links to the real one.
            let fake = base.path().join("fake");
            std::fs::create_dir_all(fake.join(".local/share/lean-ctx")).unwrap();
            std::os::unix::fs::symlink(home.join(".config"), fake.join(".config")).unwrap();
            assert!(
                at(
                    &fake.join(".config/lean-ctx"),
                    &fake.join(".local/share/lean-ctx"),
                    Some(&fake)
                )
                .is_none()
            );
        }
        // A directory not made yet is named below its resolved ancestor.
        let new = base.path().join("new").join("lean-ctx");
        assert_eq!(real(&new), real(base.path()).join("new").join("lean-ctx"));
    }

    /// bd 38432 S3: [`checked_root`] as the command runs it, from the
    /// variables, and the store opened from its root stays there whatever
    /// changes after the gate.
    ///
    /// Not on Windows: there `dirs::home_dir()` asks the system for the
    /// profile folder and ignores `HOME`, so the test cannot place the home
    /// it checks against. The rule itself is covered on every platform by the
    /// explicit-directory test above,
    /// `app_only_flags_need_one_root_for_the_key_file_and_the_records`.
    #[cfg(not(windows))]
    #[test]
    fn the_checked_root_follows_the_variables_once() {
        use lean_ctx::test_env::{VarGuard, set_var};
        let _env = lean_ctx::core::data_dir::test_env_lock();
        let base = tempfile::tempdir().unwrap();
        let home = base.path().join("home");
        let config = home.join(".config").join("lean-ctx");
        let data = home.join(".local").join("share").join("lean-ctx");
        let own = base.path().join("agent-tmp");
        for dir in [&config, &data, &own] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let _home = VarGuard::set("HOME", &home);
        let _xdg = VarGuard::set("XDG_CONFIG_HOME", "");
        // Restored when the test ends; changed below with `set_var`.
        let _config_dir = VarGuard::set("LEAN_CTX_CONFIG_DIR", &config);
        let _data_dir = VarGuard::set("LEAN_CTX_DATA_DIR", &data);
        // The default split under the home.
        let root = checked_root().expect("default split");
        assert_eq!(root.data, real(&data));
        // bd 38415: the records real, the key file the agent's own.
        set_var("LEAN_CTX_DATA_DIR", &own);
        assert!(checked_root().is_none(), "split roots");
        // S1: the data pin on the standard config directory.
        set_var("LEAN_CTX_DATA_DIR", &config);
        assert!(checked_root().is_none(), "collapsed onto the records");
        // An isolated instance: both on its own directory, even one not
        // made yet.
        let fresh = own.join("fresh");
        set_var("LEAN_CTX_CONFIG_DIR", &fresh);
        set_var("LEAN_CTX_DATA_DIR", &fresh);
        let root = checked_root().expect("one directory");
        assert_eq!(root.config, real(&own).join("fresh"));
        assert_eq!(root.key_file(), real(&own).join("fresh/agents").join(FILE));
        // M-toctou: whatever moves after the gate, the store opened from its
        // root keeps the gate's records.
        set_var("LEAN_CTX_DATA_DIR", &data);
        set_var("LEAN_CTX_CONFIG_DIR", &config);
        let store = root.store();
        assert_eq!(store.config_dir(), real(&own).join("fresh/cloud"));
        assert_eq!(store.data_dir(), real(&own).join("fresh/cloud"));
    }

    #[test]
    fn the_app_only_gate_checks_the_layout_before_the_key() {
        let args = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let flags = &["--chat-key", "--print-instance-key"];
        let key = Some(KEY.as_bytes());
        let root = AppRoot {
            config: PathBuf::from("/r"),
            data: PathBuf::from("/r"),
        };
        let one_root = || Some(root.clone());
        let right = |file: &Path, given: Option<&[u8]>| {
            file == root.key_file() && given == Some(KEY.as_bytes())
        };
        // No app-only flag: nothing checked.
        assert_eq!(
            app_only_gate(&args(&["--json"]), flags, None, || None, |_, _| false),
            Ok(None)
        );
        assert_eq!(
            app_only_gate(&args(&["--chat-key", "-"]), flags, key, one_root, right),
            Ok(Some(root.clone()))
        );
        // The attack: the right key of a key file moved away from the records.
        let mut asked = false;
        let refused = app_only_gate(
            &args(&["--chat-key", "-"]),
            flags,
            key,
            || None,
            |f, g| {
                asked = true;
                right(f, g)
            },
        )
        .unwrap_err();
        assert_eq!(refused.code.as_str(), "app_only");
        assert!(!asked, "the key file is not even read");
        assert!(
            app_only_gate(
                &args(&["--print-instance-key"]),
                flags,
                None,
                one_root,
                right
            )
            .is_err()
        );
    }

    #[test]
    fn the_env_key_is_taken_once_and_removed() {
        let _env = lean_ctx::core::data_dir::test_env_lock();
        lean_ctx::test_env::set_var(ENV, KEY);
        assert_eq!(take_env_key().as_deref(), Some(KEY.as_bytes()));
        assert!(std::env::var_os(ENV).is_none());
        assert_eq!(take_env_key(), None);
    }

    #[test]
    fn the_send_to_agents_route_is_app_only() {
        assert!(guards(super::super::decisions_part::SEND));
        assert!(guards(super::super::decisions_part::SUPERSEDE));
        assert!(guards(super::super::decisions_part::REVOKE));
        assert!(guards(super::super::unlink_part::KIND));
        assert!(!guards(super::super::decisions_part::KIND));
        assert!(!guards(super::super::decisions_part::CREATE));
        assert_eq!(refusal().code.as_str(), "app_only");
    }

    #[test]
    fn chat_prepare_is_app_only_and_the_signed_sends_are_not() {
        assert!(guards(super::super::chat_sign::PREPARE));
        // The sends check the prepared frame and the chat key's signature.
        for kind in ["chat.post", "chat.dm", "bus.post"] {
            assert!(!guards(kind), "{kind}");
        }
    }
}
