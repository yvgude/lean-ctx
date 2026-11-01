//! Persistent anonymous installation identifier for default-on telemetry.
//!
//! Generates a random UUID v4 on first call and persists it as a plain-text
//! file in the data directory. The ID is never derived from hardware, OS
//! fingerprints, or user identity — it is pure randomness.

use std::path::PathBuf;

use fs2::FileExt;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TelemetryIdentity {
    installation_id: String,
    deletion_token: String,
}

#[cfg(test)]
fn id_path() -> Result<PathBuf, String> {
    crate::core::paths::data_dir().map(|d| d.join(ID_FILE))
}

#[cfg(test)]
fn deletion_token_path() -> Result<PathBuf, String> {
    crate::core::paths::data_dir().map(|d| d.join(TOKEN_FILE))
}

fn identity_path() -> Result<PathBuf, String> {
    crate::core::paths::data_dir().map(|d| d.join(IDENTITY_FILE))
}

const ID_FILE: &str = "installation_id";
const TOKEN_FILE: &str = "telemetry_deletion_token";
const IDENTITY_FILE: &str = "telemetry_identity.json";

/// When the current identity was created: the file's birth time where the
/// filesystem records one, otherwise its last write (identity files are only
/// rewritten on creation, migration or `telemetry reset-id`). An identity
/// adopted from a sibling data directory keeps the sibling's age.
pub(crate) fn identity_created_at() -> Option<std::time::SystemTime> {
    let path = identity_path().ok()?;
    let own = read_identity(&path)?;
    std::iter::once(path.clone())
        .chain(sibling_identity_paths(&path))
        .filter(|candidate| {
            read_identity(candidate)
                .is_some_and(|other| other.installation_id == own.installation_id)
        })
        .filter_map(|candidate| file_created_at(&candidate))
        .min()
}

fn file_created_at(path: &std::path::Path) -> Option<std::time::SystemTime> {
    let metadata = std::fs::metadata(path).ok()?;
    metadata.created().or_else(|_| metadata.modified()).ok()
}

fn read_identity(path: &std::path::Path) -> Option<TelemetryIdentity> {
    let identity: TelemetryIdentity =
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    (is_valid_uuid(&identity.installation_id) && is_lower_hex_256(&identity.deletion_token))
        .then_some(identity)
}

/// The identity files of the same user's other LeanCTX data directories:
/// legacy `~/.lean-ctx`, mixed `$XDG_CONFIG_HOME/lean-ctx` and
/// `$XDG_DATA_HOME/lean-ctx`. Processes started with a different data-dir
/// resolution (an agent host with its own `XDG_*`, an upgrade across the
/// #408 layout change) would otherwise mint a second identity for one
/// installation. An explicit `LEAN_CTX_DATA_DIR` pins exactly one directory.
fn sibling_identity_paths(current: &std::path::Path) -> Vec<PathBuf> {
    if std::env::var_os("LEAN_CTX_DATA_DIR").is_some() {
        return Vec::new();
    }
    let canonical = |path: &std::path::Path| std::fs::canonicalize(path).ok();
    let current_dir = current.parent().and_then(canonical);
    crate::core::data_dir::candidate_data_dirs()
        .into_iter()
        .filter(|dir| current_dir.is_none() || canonical(dir) != current_dir)
        .map(|dir| dir.join("telemetry_identity.json"))
        .filter(|path| path.is_file())
        .collect()
}

/// The oldest valid identity among the sibling data directories, if any.
fn sibling_identity(current: &std::path::Path) -> Option<TelemetryIdentity> {
    sibling_identity_paths(current)
        .into_iter()
        .filter_map(|path| Some((file_created_at(&path)?, read_identity(&path)?)))
        .min_by_key(|(created, _)| *created)
        .map(|(_, identity)| identity)
}

/// Run `operation` under the credential lock, handing it the data directory
/// the lock lives in. Every identity file the operation touches must come from
/// that directory: resolving it again inside could name a different one (the
/// data-dir env changed meanwhile), and two writers would then interleave the
/// sidecar retirement unlocked — minting a second ID next to the old token.
fn with_deletion_lock<T>(
    operation: impl FnOnce(&std::path::Path) -> Result<T, String>,
) -> Result<T, String> {
    let dir = crate::core::paths::data_dir()?;
    let lock_path = dir.join("telemetry_deletion_token.lock");
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Cannot create data dir: {e}"))?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options
        .open(lock_path)
        .map_err(|e| format!("Cannot open telemetry credential lock: {e}"))?;
    lock.lock_exclusive()
        .map_err(|e| format!("Cannot lock telemetry credential: {e}"))?;
    let result = operation(&dir);
    FileExt::unlock(&lock).map_err(|e| format!("Cannot unlock telemetry credential: {e}"))?;
    result
}

/// The installation ID `get_or_create` would return, if one exists, without
/// creating, migrating or adopting anything: for read-only views such as the
/// dashboard. `None` while no ID was ever created.
pub(crate) fn existing() -> Option<String> {
    let path = identity_path().ok()?;
    if let Some(identity) = read_identity(&path) {
        return Some(identity.installation_id);
    }
    let dir = path.parent()?;
    // A legacy sidecar is this directory's own ID; an invalid one is replaced
    // on creation, so no ID exists yet.
    if let Ok(raw) = std::fs::read_to_string(dir.join(ID_FILE)) {
        let id = raw.trim();
        return is_valid_uuid(id).then(|| id.to_string());
    }
    sibling_identity(&path).map(|identity| identity.installation_id)
}

/// Return the existing installation ID or generate a fresh UUID v4.
pub(crate) fn get_or_create() -> Result<String, String> {
    Ok(get_or_create_identity()?.0)
}

/// Read one lock-consistent installation ID and deletion credential snapshot.
pub(crate) fn get_or_create_identity() -> Result<(String, String), String> {
    with_deletion_lock(get_or_create_identity_locked)
}

/// [`get_or_create_identity`] for a caller that already holds the credential
/// lock of `dir`.
fn get_or_create_identity_locked(dir: &std::path::Path) -> Result<(String, String), String> {
    let path = dir.join(IDENTITY_FILE);
    if let Ok(raw) = std::fs::read_to_string(&path) {
        if let Ok(identity) = serde_json::from_str::<TelemetryIdentity>(&raw) {
            if is_valid_uuid(&identity.installation_id)
                && is_lower_hex_256(&identity.deletion_token)
            {
                tighten_secret_permissions(&path)?;
                retire_legacy_sidecars(dir)?;
                return Ok((identity.installation_id, identity.deletion_token));
            }
        }
    }
    // A legacy sidecar in this directory is this directory's own identity;
    // only a directory without one adopts the user's identity from a
    // sibling data directory instead of minting a second one.
    let own_sidecar = dir.join(ID_FILE).is_file();
    if !own_sidecar && let Some(identity) = sibling_identity(&path) {
        persist_identity(&path, &identity)?;
        return Ok((identity.installation_id, identity.deletion_token));
    }
    let identity = TelemetryIdentity {
        installation_id: load_or_create_id(dir)?,
        deletion_token: load_or_create_token(dir)?,
    };
    persist_identity(&path, &identity)?;
    retire_legacy_sidecars(dir)?;
    Ok((identity.installation_id, identity.deletion_token))
}

fn load_or_create_id(dir: &std::path::Path) -> Result<String, String> {
    let path = dir.join(ID_FILE);
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let trimmed = existing.trim().to_string();
        if is_valid_uuid(&trimmed) {
            return Ok(trimmed);
        }
    }
    let id = generate_uuid_v4();
    persist(&path, &id)?;
    Ok(id)
}

/// Regenerate the installation ID (for `lean-ctx telemetry reset-id`).
pub(crate) fn reset() -> Result<String, String> {
    with_deletion_lock(|dir| {
        let identity = TelemetryIdentity {
            installation_id: generate_uuid_v4(),
            deletion_token: generate_deletion_token(),
        };
        persist_identity(&dir.join(IDENTITY_FILE), &identity)?;
        retire_legacy_sidecars(dir)?;
        Ok(identity.installation_id)
    })
}

/// Return the installation-scoped 256-bit secret proving deletion ownership.
#[cfg(test)]
pub(crate) fn deletion_token() -> Result<String, String> {
    Ok(get_or_create_identity()?.1)
}

fn load_or_create_token(dir: &std::path::Path) -> Result<String, String> {
    let path = dir.join(TOKEN_FILE);
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let token = existing.trim().to_string();
        if is_lower_hex_256(&token) {
            tighten_secret_permissions(&path)?;
            return Ok(token);
        }
    }
    let token = generate_deletion_token();
    persist_secret(&path, &token)?;
    Ok(token)
}

#[cfg(test)]
pub(crate) fn deletion_token_hash() -> Result<String, String> {
    use sha2::Digest;
    Ok(hex::encode(sha2::Sha256::digest(
        deletion_token()?.as_bytes(),
    )))
}

fn persist(path: &std::path::Path, id: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Cannot create data dir: {e}"))?;
    }
    crate::core::atomic_fs::write_bytes_with_fallback(path, format!("{id}\n").as_bytes(), None)
        .map_err(|e| format!("Cannot write installation ID: {e}"))
}

fn persist_secret(path: &std::path::Path, token: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Cannot create data dir: {e}"))?;
    }
    #[cfg(unix)]
    let permissions = {
        use std::os::unix::fs::PermissionsExt;
        Some(std::fs::Permissions::from_mode(0o600))
    };
    #[cfg(not(unix))]
    let permissions = None;
    crate::core::atomic_fs::try_atomic_write(
        path,
        format!("{token}\n").as_bytes(),
        permissions.as_ref(),
    )
    .map_err(|e| format!("Cannot write telemetry deletion credential: {e}"))?;
    tighten_secret_permissions(path)
}

fn retire_legacy_sidecars(dir: &std::path::Path) -> Result<(), String> {
    for path in [dir.join(ID_FILE), dir.join(TOKEN_FILE)] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "Cannot retire legacy telemetry identity {}: {error}",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

fn persist_identity(path: &std::path::Path, identity: &TelemetryIdentity) -> Result<(), String> {
    let encoded = serde_json::to_vec(identity)
        .map_err(|error| format!("Cannot encode telemetry identity: {error}"))?;
    persist_secret(path, std::str::from_utf8(&encoded).expect("JSON is UTF-8"))
}

#[cfg(unix)]
fn tighten_secret_permissions(path: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("Cannot secure telemetry deletion credential: {e}"))
}

#[cfg(not(unix))]
fn tighten_secret_permissions(_path: &std::path::Path) -> Result<(), String> {
    Ok(())
}

fn generate_deletion_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("getrandom failed");
    hex::encode(bytes)
}

fn is_lower_hex_256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn generate_uuid_v4() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("getrandom failed");
    // RFC 4122 variant and version bits
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // variant 1
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        u16::from_be_bytes([bytes[4], bytes[5]]),
        u16::from_be_bytes([bytes[6], bytes[7]]),
        u16::from_be_bytes([bytes[8], bytes[9]]),
        u64::from_be_bytes([
            0, 0, bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
        ]),
    )
}

fn is_valid_uuid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && parts[0].len() == 8
        && parts[1].len() == 4
        && parts[2].len() == 4
        && parts[3].len() == 4
        && parts[4].len() == 12
        && s.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

/// Return the masked form `abcd…wxyz` for display.
pub(crate) fn masked(id: &str) -> String {
    if id.len() < 8 {
        return "********".to_string();
    }
    let clean: String = id.chars().filter(|c| *c != '-').collect();
    format!("{}…{}", &clean[..4], &clean[clean.len() - 4..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    #[cfg(unix)]
    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let previous = std::env::var(key).ok();
            match value {
                Some(value) => crate::test_env::set_var(key, value),
                None => crate::test_env::remove_var(key),
            }
            Self { key, previous }
        }
    }

    #[cfg(unix)]
    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(previous) => crate::test_env::set_var(self.key, previous),
                None => crate::test_env::remove_var(self.key),
            }
        }
    }

    /// An update never changes the tracked ID: a pre-3.11 sidecar ID becomes
    /// the identity, and every later start returns that same identity.
    #[test]
    fn updates_keep_the_tracked_installation_id() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let legacy_id = generate_uuid_v4();
        // Stage the pre-3.11 install and migrate it under the credential lock:
        // a detached telemetry thread of an earlier test resolves the data dir
        // when it runs and can mint an identity in this fresh dir first (CI
        // saw a random ID instead of the legacy one). Clearing any such stray
        // identity under the lock restores the exact pre-update state.
        let (migrated, token) = with_deletion_lock(|dir| {
            for stray in [IDENTITY_FILE, TOKEN_FILE] {
                let _ = std::fs::remove_file(dir.join(stray));
            }
            std::fs::write(dir.join(ID_FILE), format!("{legacy_id}\n"))
                .map_err(|e| e.to_string())?;
            get_or_create_identity_locked(dir)
        })
        .unwrap();
        assert_eq!(migrated, legacy_id, "the v1 ID survives the move to v2");
        for _ in 0..3 {
            assert_eq!(
                get_or_create_identity().unwrap(),
                (legacy_id.clone(), token.clone())
            );
        }
        let stored = read_identity(&identity_path().unwrap()).expect("identity persisted");
        assert_eq!(stored.installation_id, legacy_id);
    }

    #[cfg(unix)]
    #[test]
    fn a_new_data_dir_adopts_the_users_identity_from_a_sibling_dir() {
        let _lock = crate::core::data_dir::test_env_lock();
        let home = tempfile::tempdir().unwrap();
        let _home = EnvVarGuard::set("HOME", Some(home.path().to_str().unwrap()));
        let _pin = EnvVarGuard::set("LEAN_CTX_DATA_DIR", None);
        let _config = EnvVarGuard::set("XDG_CONFIG_HOME", None);
        let _data = EnvVarGuard::set("XDG_DATA_HOME", None);
        // Candidates under the temporary HOME: [legacy, mixed config, XDG data].
        let candidates = crate::core::data_dir::candidate_data_dirs();
        let legacy = candidates[0].clone();
        assert!(legacy.starts_with(home.path()));
        std::fs::create_dir_all(&legacy).unwrap();
        let identity = TelemetryIdentity {
            installation_id: generate_uuid_v4(),
            deletion_token: generate_deletion_token(),
        };
        persist_identity(&legacy.join("telemetry_identity.json"), &identity).unwrap();
        let xdg = candidates[2].clone();
        std::fs::create_dir_all(&xdg).unwrap();

        let adopted =
            sibling_identity(&xdg.join("telemetry_identity.json")).expect("sibling identity");
        assert_eq!(adopted.installation_id, identity.installation_id);
        assert_eq!(adopted.deletion_token, identity.deletion_token);
        // The directory never adopts itself, and an explicit pin adopts nothing.
        assert!(sibling_identity(&legacy.join("telemetry_identity.json")).is_none());
        let _pinned = EnvVarGuard::set("LEAN_CTX_DATA_DIR", Some(xdg.to_str().unwrap()));
        assert!(sibling_identity(&xdg.join("telemetry_identity.json")).is_none());
    }

    /// The credential lock and the files it guards come from one data-dir
    /// resolution. A writer whose env moved on mid-operation used to lock one
    /// directory and write another, racing an owner of that second directory
    /// through the sidecar retirement (CI: a fresh ID next to the old token).
    #[test]
    fn locked_operations_stay_in_the_directory_they_locked() {
        let iso = crate::core::data_dir::isolated_data_dir();
        let locked = iso.path().to_path_buf();
        let elsewhere = tempfile::tempdir().unwrap();

        let id = with_deletion_lock(|dir| {
            crate::test_env::set_var("LEAN_CTX_DATA_DIR", elsewhere.path());
            let id = load_or_create_id(dir);
            crate::test_env::set_var("LEAN_CTX_DATA_DIR", &locked);
            id
        })
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(locked.join(ID_FILE))
                .unwrap()
                .trim(),
            id
        );
        // Not "no file there": while the env pointed at `elsewhere`, a
        // background thread from another test may have minted its own
        // identity in it (exactly the writer this fix guards against). What
        // must hold is that *this* operation's ID never went there.
        let stray = std::fs::read_to_string(elsewhere.path().join(ID_FILE)).unwrap_or_default();
        assert_ne!(
            stray.trim(),
            id,
            "the locked operation wrote outside its lock"
        );
    }

    #[test]
    fn generated_uuid_is_valid() {
        let id = generate_uuid_v4();
        assert!(is_valid_uuid(&id), "invalid UUID: {id}");
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4", "version nibble must be 4");
        let variant_nibble = u8::from_str_radix(&id[19..20], 16).unwrap();
        assert!(
            (0x8..=0xb).contains(&variant_nibble),
            "variant must be 8-b, got {variant_nibble:x}"
        );
    }

    #[test]
    fn get_or_create_roundtrip() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let a = get_or_create().unwrap();
        let b = get_or_create().unwrap();
        assert_eq!(a, b, "must return the same ID on repeated calls");
    }

    #[test]
    fn reset_changes_id() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let a = get_or_create().unwrap();
        let old_token = deletion_token().unwrap();
        let b = reset().unwrap();
        assert_ne!(a, b, "reset must produce a new ID");
        let c = get_or_create().unwrap();
        assert_eq!(b, c, "get_or_create after reset must return the new ID");
        assert_ne!(old_token, deletion_token().unwrap());
    }

    #[test]
    fn deletion_credential_is_stable_and_only_hash_is_shareable() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let token = deletion_token().unwrap();
        assert_eq!(token, deletion_token().unwrap());
        assert!(is_lower_hex_256(&token));
        let hash = deletion_token_hash().unwrap();
        assert!(is_lower_hex_256(&hash));
        assert_ne!(token, hash);
    }

    #[test]
    fn concurrent_deletion_credential_creation_converges() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let workers: Vec<_> = (0..8).map(|_| std::thread::spawn(deletion_token)).collect();
        let tokens: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap().unwrap())
            .collect();
        assert!(tokens.iter().all(|token| token == &tokens[0]));
    }

    #[cfg(unix)]
    #[test]
    fn identity_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let _iso = crate::core::data_dir::isolated_data_dir();
        get_or_create_identity().unwrap();
        let mode = std::fs::metadata(identity_path().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn corrupt_identity_never_resurrects_retired_sidecars() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let original = get_or_create_identity().unwrap();
        let rotated = reset().unwrap();
        assert!(!id_path().unwrap().exists());
        assert!(!deletion_token_path().unwrap().exists());
        std::fs::write(identity_path().unwrap(), b"corrupt").unwrap();
        let recovered = get_or_create_identity().unwrap();
        assert_ne!(recovered.0, original.0);
        assert_ne!(recovered.0, rotated);
        assert_ne!(recovered.1, original.1);
    }

    #[test]
    fn masked_format() {
        let id = "abcdef01-2345-4678-9abc-def012345678";
        assert_eq!(masked(id), "abcd…5678");
    }
}
