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

/// The kinds only LeanCTX Code may ask for.
pub(crate) const APP_ONLY: &[&str] = &[super::decisions_part::SEND];

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
    fn the_send_to_agents_route_is_app_only() {
        assert!(guards(super::super::decisions_part::SEND));
        assert!(!guards(super::super::decisions_part::KIND));
        assert!(!guards(super::super::decisions_part::CREATE));
        assert_eq!(refusal().code.as_str(), "app_only");
    }
}
