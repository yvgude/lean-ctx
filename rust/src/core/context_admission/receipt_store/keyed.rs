// SPDX-License-Identifier: Apache-2.0
//! Keyed references in receipt evidence (AP10-C6, AP10-C2).
//!
//! Some things a receipt should name are secrets themselves or digests of
//! them: the credential a caller presented, the request head that carries
//! it. An unkeyed digest of such a value confirms a guess about it. They are
//! recorded as keyed BLAKE3 under this installation's receipt key instead:
//! the same value maps to the same reference on this installation; without
//! the key a reference confirms nothing.
//!
//! - `principal_ref`: the caller the proxy told apart — the digest of the
//!   presented credential or the gateway person (`forward::principal_of`).
//!   The receipt's V1 principal is the agent id a caller claims, which the
//!   proxy cannot check.
//! - `request_head_ref`: the method, the final URL and the header set a
//!   request is bound to be sent with.
//!
//! The key is 32 random bytes in `<data>/gateway/receipt.key`, created once
//! and never rotated by the Engine: written to a private temporary file in
//! the same directory and published with a hard link, which never replaces
//! a key another process published first. A key file that is not exactly 32
//! bytes is unusable ([`KEY_UNUSABLE`]) and is never replaced — that would
//! orphan every reference made under it; the operator decides. On Unix the
//! file must be 0600: a wider mode is narrowed when the key is loaded, and a
//! key whose mode cannot be narrowed is unusable. On Windows the file
//! inherits the ACL of the user's data directory; no ACL is set or checked.
//!
//! No key, no reference: a reference is never computed under a key that is
//! not stored. Where the receipt is owed, a missing reference refuses the
//! delivery (the caller's choice, see `proxy::admitted`).

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const KEY_FILE: &str = "receipt.key";
const PREFIX: &str = "blake3k:";

/// Reason code of a delivery whose owed keyed reference could not be made.
pub(crate) const KEY_UNUSABLE: &str = "receipt.key_unusable";

/// The key could not be read or created: the evidence store itself is not
/// usable, as for a receipt that cannot be stored.
const STORE_UNAVAILABLE: &str = "receipt.unavailable";

/// The keyed references an egress decision is recorded with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct KeyedRefs {
    pub(crate) principal: Option<String>,
    pub(crate) request_head: Option<String>,
}

impl KeyedRefs {
    /// Field by field, `self` where it has a value, `earlier` otherwise.
    #[must_use]
    pub(crate) fn or(self, earlier: Self) -> Self {
        Self {
            principal: self.principal.or(earlier.principal),
            request_head: self.request_head.or(earlier.request_head),
        }
    }
}

/// The keyed reference of the sealed principal `principal`; `Err` with
/// [`KEY_UNUSABLE`] when the installation's receipt key is not available.
#[cfg(any(test, feature = "http-server"))]
pub(crate) fn principal_ref(principal: &str) -> Result<String, &'static str> {
    keyed(b"leanctx.receipt.principal-ref.v1", &[principal.as_bytes()])
}

/// The MAC binding a human's egress grant (`lean-ctx gateway allow-once`)
/// to this installation's receipt key.
pub(crate) fn grant_ref(parts: &[&[u8]]) -> Result<String, &'static str> {
    keyed(b"leanctx.gateway.grant.v1", parts)
}

/// The keyed reference of a request head: `method`, the final `url` and
/// every header as `(name, value)`. Header order and name case do not
/// matter; a repeated header counts with each of its values.
#[cfg(any(test, feature = "http-server"))]
pub(crate) fn request_head_ref<'a>(
    method: &str,
    url: &str,
    headers: impl IntoIterator<Item = (&'a str, &'a [u8])>,
) -> Result<String, &'static str> {
    let mut lines: Vec<(String, &[u8])> = headers
        .into_iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value))
        .collect();
    lines.sort_unstable();
    let mut parts: Vec<&[u8]> = vec![method.as_bytes(), url.as_bytes()];
    for (name, value) in &lines {
        parts.push(name.as_bytes());
        parts.push(value);
    }
    keyed(b"leanctx.receipt.request-head-ref.v1", &parts)
}

/// Keyed BLAKE3 over the length-prefixed `parts`, separated by `domain`.
fn keyed(domain: &[u8], parts: &[&[u8]]) -> Result<String, &'static str> {
    let key = key()?;
    let mut hasher = blake3::Hasher::new_keyed(&key);
    hasher.update(domain);
    for part in parts {
        hasher.update(&(part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    Ok(format!("{PREFIX}{}", hasher.finalize().to_hex()))
}

/// The receipt key as it digests decision objects (AP10-C4): a version 2
/// receipt names each object by `HMAC-SHA256` under the installation's
/// receipt key instead of the plain SHA-256 a version 1 receipt names, which
/// confirmed a guess of the text. Same key and same object, same digest.
pub(crate) struct ObjectKey {
    key: [u8; 32],
}

const OBJECT_DOMAIN: &[u8] = b"leanctx.receipt.object.v2";
const KEY_ID_DOMAIN: &[u8] = b"leanctx.receipt.key-id.v2";

/// The installation's object key; `Err` with [`KEY_UNUSABLE`] (or the store
/// code) when the receipt key is not available — no key, no v2 receipt.
pub(crate) fn object_key() -> Result<ObjectKey, &'static str> {
    key().map(|key| ObjectKey { key })
}

impl ObjectKey {
    fn mac(&self, parts: &[&[u8]]) -> [u8; 32] {
        use hmac::{Hmac, KeyInit as _, Mac as _};
        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(&self.key)
            .expect("HMAC-SHA256 takes a key of any length");
        for part in parts {
            mac.update(part);
        }
        mac.finalize().into_bytes().into()
    }

    /// The keyed form of the plain object digest `plain` (`sha256:<hex>`).
    pub(crate) fn object(
        &self,
        plain: &lean_ctx_protocol::Sha256Digest,
    ) -> lean_ctx_protocol::Sha256Digest {
        let mac = self.mac(&[OBJECT_DOMAIN, b"\0", plain.as_str().as_bytes()]);
        let hex = crate::core::agent_identity::hex_encode(&mac);
        lean_ctx_protocol::Sha256Digest::new(format!("sha256:{hex}"))
            .expect("HMAC-SHA256 output is a canonical digest")
    }

    /// What a version 2 receipt states about this key: algorithm and id.
    pub(crate) fn descriptor(&self) -> lean_ctx_protocol::context_gateway::ReceiptObjectDigestV2 {
        let id = crate::core::agent_identity::hex_encode(&self.mac(&[KEY_ID_DOMAIN]));
        lean_ctx_protocol::context_gateway::ReceiptObjectDigestV2 {
            algorithm: lean_ctx_protocol::context_gateway::ObjectDigestAlgorithmV2::HmacSha256,
            key_id: format!("rk-{}", &id[..32]),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_bytes(key: [u8; 32]) -> Self {
        Self { key }
    }
}

/// How long a failure to load or create the key is answered from memory: no
/// file I/O, staging or warning per request while the store stays broken.
const FAILURE_HOLD: Duration = Duration::from_secs(30);

/// The key of the current data directory: loaded once per directory; a
/// failure is reported once and held for [`FAILURE_HOLD`].
fn key() -> Result<[u8; 32], &'static str> {
    type Loaded = HashMap<PathBuf, Result<[u8; 32], (&'static str, Instant)>>;
    static LOADED: OnceLock<Mutex<Loaded>> = OnceLock::new();
    let path = super::gateway_dir()
        .ok_or(STORE_UNAVAILABLE)?
        .join(KEY_FILE);
    let mut loaded = LOADED
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let reported = match loaded.get(&path) {
        Some(Ok(key)) => return Ok(*key),
        Some(Err((code, since))) if since.elapsed() < FAILURE_HOLD => return Err(code),
        Some(Err((code, _))) => Some(*code),
        None => None,
    };
    let outcome = load_or_create(&path);
    if let Err(code) = outcome
        && reported != Some(code)
    {
        tracing::warn!("gateway receipt key unavailable ({code}); keyed references withheld");
    }
    loaded.insert(path, outcome.map_err(|code| (code, Instant::now())));
    outcome
}

/// For `lean-ctx doctor`: `Err` when a receipt key file exists that is not a
/// usable key. Reads only; an absent key is created on first use.
pub(crate) fn key_status() -> Result<(), &'static str> {
    let Some(dir) = super::gateway_dir() else {
        return Ok(());
    };
    match std::fs::read(dir.join(KEY_FILE)) {
        Ok(bytes) if bytes.len() == 32 => Ok(()),
        Ok(_) => Err(KEY_UNUSABLE),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(STORE_UNAVAILABLE),
    }
}

fn load_or_create(path: &Path) -> Result<[u8; 32], &'static str> {
    if let Some(key) = load(path)? {
        return Ok(key);
    }
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|_| STORE_UNAVAILABLE)?;
    let dir = path.parent().ok_or(STORE_UNAVAILABLE)?;
    std::fs::create_dir_all(dir).map_err(|_| STORE_UNAVAILABLE)?;
    let mut nonce = [0u8; 8];
    getrandom::fill(&mut nonce).map_err(|_| STORE_UNAVAILABLE)?;
    let staged = dir.join(format!(
        ".{KEY_FILE}.{}.{}.tmp",
        std::process::id(),
        u64::from_be_bytes(nonce)
    ));
    let written = write_private(&staged, &key);
    // Published by a hard link: atomic, and never over a key another
    // process published first.
    let published = written.and_then(|()| std::fs::hard_link(&staged, path));
    let _ = std::fs::remove_file(&staged);
    match published {
        Ok(()) => Ok(key),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            load(path)?.ok_or(KEY_UNUSABLE)
        }
        Err(_) => Err(STORE_UNAVAILABLE),
    }
}

fn write_private(path: &Path, key: &[u8; 32]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(key)?;
    file.sync_all()
}

/// `Ok(None)`: no key file. `Err`: a key file that is not a usable key.
fn load(path: &Path) -> Result<Option<[u8; 32]>, &'static str> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(STORE_UNAVAILABLE),
    };
    let key: [u8; 32] = bytes.try_into().map_err(|_| KEY_UNUSABLE)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(path)
            .map_err(|_| KEY_UNUSABLE)?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(|_| KEY_UNUSABLE)?;
            tracing::warn!("gateway receipt key mode narrowed to 0600");
        }
    }
    Ok(Some(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AP10-C4: a v2 object digest is keyed — the plain SHA-256 of a guess
    /// does not match it, another key gives another digest, the same key the
    /// same one. The vector is the one every SDK checks
    /// (`sdk/fixtures/gateway-preview-v1/manifest.json`, `keyed_v2`).
    #[test]
    fn object_digests_are_keyed_and_match_the_sdk_vector() {
        use sha2::Digest as _;
        let text = "deploy fails on host build-7";
        let plain = lean_ctx_protocol::Sha256Digest::new(format!(
            "sha256:{}",
            crate::core::agent_identity::hex_encode(&sha2::Sha256::digest(text.as_bytes()))
        ))
        .unwrap();
        let key = ObjectKey::from_bytes(std::array::from_fn(|i| i as u8));
        let keyed = key.object(&plain);
        assert_eq!(
            keyed.as_str(),
            "sha256:f0bba5eded0463a0091123d2479ab779ecd3fa3fc78eec5d7551979f3dda4729"
        );
        assert_eq!(
            key.descriptor().key_id,
            "rk-d9dc58b396ceb54bb5f4ae02d2bcb002"
        );
        assert_ne!(keyed, plain, "a plain guess does not confirm the object");
        assert_eq!(key.object(&plain), keyed, "deterministic");
        let other = ObjectKey::from_bytes([7; 32]);
        assert_ne!(other.object(&plain), keyed);
        assert_ne!(other.descriptor().key_id, key.descriptor().key_id);
    }

    /// AP10 review: the receipt key is published once, whole and private.
    /// Concurrent first uses agree on one key; a short or corrupt key file
    /// is reported unusable and left in place, never replaced.
    #[test]
    fn the_receipt_key_is_one_whole_private_key_or_unusable() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let path = super::super::gateway_dir()
            .expect("gateway dir")
            .join(KEY_FILE);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let created: Vec<[u8; 32]> = (0..8)
            .map(|_| {
                let barrier = barrier.clone();
                let path = path.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    load_or_create(&path).expect("a key")
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|thread| thread.join().expect("thread"))
            .collect();
        assert!(created.iter().all(|key| *key == created[0]));
        assert_eq!(std::fs::read(&path).expect("published"), created[0]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(load(&path), Ok(Some(created[0])));
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "narrowed");
        }
        let leftovers = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .is_ok_and(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            })
            .count();
        assert_eq!(leftovers, 0, "no staged key is left behind");

        std::fs::write(&path, &created[0][..20]).expect("partial key");
        assert_eq!(load_or_create(&path), Err(KEY_UNUSABLE));
        assert_eq!(
            std::fs::read(&path).unwrap().len(),
            20,
            "an unusable key is reported, not replaced"
        );
    }
}
