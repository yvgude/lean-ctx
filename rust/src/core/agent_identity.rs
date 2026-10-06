// SPDX-License-Identifier: Apache-2.0

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Canonical resolver for the current agent identity. Reads `LEAN_CTX_AGENT_ID`
/// (or legacy `LCTX_AGENT_ID`), falling back to `"local"`. Resolved once per
/// process and cached, so all subsystems (heatmap, savings ledger, audit)
/// attribute traces to the same identity.
#[must_use]
pub(crate) fn current_agent_id() -> &'static str {
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE.get_or_init(|| {
        std::env::var("LEAN_CTX_AGENT_ID")
            .or_else(|_| std::env::var("LCTX_AGENT_ID"))
            .unwrap_or_else(|_| "local".to_string())
    })
}

/// Identity of this process in the cross-agent delivery registry (#1904).
///
/// Unlike [`current_agent_id`] (a stable, shared attribution id), delivery
/// needs one id per *consumer*: the registry skips records whose `agent_id`
/// matches the requester, so two consumers sharing an id could never see each
/// other's deliveries, and a consumer matching a foreign id would be treated as
/// the reader. `CLAUDECODE` is the constant `"1"` in every Claude Code process
/// and was used verbatim before, which collapsed all Claude Code clients on a
/// machine into a single "agent".
///
/// Priority (first non-empty wins):
/// 1. `LEAN_CTX_AGENT_ID` / legacy `LCTX_AGENT_ID` — explicit operator identity,
///    shared with [`current_agent_id`]
/// 2. `CURSOR_TASK_ID` — Cursor's per-subagent id
/// 3. `claude-{pid}` when `CLAUDECODE` is set
/// 4. `codex-{pid}` when `CODEX_THREAD_ID` is set
/// 5. `local-{pid}`
///
/// Resolved once per process, so the id is stable for the process lifetime.
#[must_use]
pub(crate) fn delivery_agent_id() -> &'static str {
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE.get_or_init(|| {
        resolve_delivery_agent_id(|key| std::env::var(key).ok(), std::process::id())
    })
}

/// Pure core of [`delivery_agent_id`]: every input explicit, so the priority
/// matrix is testable without touching the process environment.
fn resolve_delivery_agent_id(env: impl Fn(&str) -> Option<String>, pid: u32) -> String {
    let non_empty = |key: &str| {
        env(key)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    if let Some(id) = non_empty("LEAN_CTX_AGENT_ID").or_else(|| non_empty("LCTX_AGENT_ID")) {
        return id;
    }
    if let Some(task) = non_empty("CURSOR_TASK_ID") {
        return task;
    }
    if non_empty("CLAUDECODE").is_some() {
        return format!("claude-{pid}");
    }
    if non_empty("CODEX_THREAD_ID").is_some() {
        return format!("codex-{pid}");
    }
    format!("local-{pid}")
}

pub(crate) fn get_or_create_keypair(agent_id: &str) -> Result<SigningKey, String> {
    let path = key_path(agent_id)?;
    if path.exists() {
        load_key(&path)
    } else {
        generate_and_save(agent_id)
    }
}

pub(crate) fn get_public_key(agent_id: &str) -> Result<VerifyingKey, String> {
    let key = get_or_create_keypair(agent_id)?;
    Ok(key.verifying_key())
}

/// Signing consumers must not create authority as a side effect of a request.
pub(crate) fn get_stored_signing_key(agent_id: &str) -> Result<SigningKey, String> {
    load_key(&key_path(agent_id)?)
}

/// Resolve an existing trust key without creating identity state.
pub(crate) fn get_stored_public_key(agent_id: &str) -> Result<VerifyingKey, String> {
    let bytes = read_key_file(&pub_key_path(agent_id)?, "trusted public key")?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "invalid public key file (expected 32 bytes)".to_string())?;
    VerifyingKey::from_bytes(&bytes).map_err(|error| format!("invalid public key: {error}"))
}

/// Read-only compatibility entry point; never creates missing verifier state.
pub(crate) fn load_public_key(agent_id: &str) -> Result<VerifyingKey, String> {
    get_stored_public_key(agent_id)
}
pub(crate) fn sign_bytes(agent_id: &str, data: &[u8]) -> Result<Vec<u8>, String> {
    let key = get_or_create_keypair(agent_id)?;
    let sig = key.sign(data);
    Ok(sig.to_bytes().to_vec())
}

/// Sign `data` and return the signature together with the verifying key of
/// the SAME keypair — one atomic key-store resolution.
///
/// Callers that embed both the signature and the public key MUST use this
/// instead of separate `sign_bytes` + `get_public_key` calls: those perform
/// two independent store reads, and when the store location or key file
/// changes in between (env-driven data-dir moves under test, key
/// regeneration by a concurrent process), the embedded public key belongs to
/// a different keypair than the signature — which then can never verify.
pub(crate) fn sign_with_public_key(
    agent_id: &str,
    data: &[u8],
) -> Result<(Vec<u8>, VerifyingKey), String> {
    let key = get_or_create_keypair(agent_id)?;
    let sig = key.sign(data);
    Ok((sig.to_bytes().to_vec(), key.verifying_key()))
}

/// Sign with an already-resolved keypair (no store access). Pair with
/// [`get_or_create_keypair`] when the public key must be embedded in the
/// payload *before* the signature is computed over it.
#[must_use]
pub(crate) fn sign_bytes_with(key: &SigningKey, data: &[u8]) -> Vec<u8> {
    key.sign(data).to_bytes().to_vec()
}

pub(crate) fn verify_signature(
    public_key_bytes: &[u8],
    data: &[u8],
    signature_bytes: &[u8],
) -> bool {
    let pk_bytes: [u8; 32] = match public_key_bytes.try_into() {
        Ok(b) => b,
        Err(_) => return false,
    };
    let Ok(verifying_key) = VerifyingKey::from_bytes(&pk_bytes) else {
        return false;
    };
    let sig_bytes: [u8; 64] = match signature_bytes.try_into() {
        Ok(b) => b,
        Err(_) => return false,
    };
    let signature = Signature::from_bytes(&sig_bytes);
    verifying_key.verify(data, &signature).is_ok()
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Reject malformed external signature material without slicing inside UTF-8.
pub(crate) fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err("odd-length hex string".to_string());
    }
    if !s.is_ascii() {
        return Err("non-ASCII hex string".to_string());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

fn key_path(agent_id: &str) -> Result<PathBuf, String> {
    validate_agent_id(agent_id)?;
    let base = crate::core::data_dir::lean_ctx_data_dir()?;
    Ok(base.join("keys").join(format!("{agent_id}.key")))
}

fn pub_key_path(agent_id: &str) -> Result<PathBuf, String> {
    validate_agent_id(agent_id)?;
    let base = crate::core::data_dir::lean_ctx_data_dir()?;
    Ok(base.join("keys").join(format!("{agent_id}.pub")))
}

/// Validate the untrusted identity component before constructing any key path.
/// IDs are deliberately a single bounded filename component: sanitizing path
/// separators would turn an invalid identity into a different trusted identity.
fn validate_agent_id(agent_id: &str) -> Result<(), String> {
    if agent_id.is_empty()
        || agent_id.len() > 128
        || !agent_id.is_ascii()
        || !agent_id.as_bytes()[0].is_ascii_alphanumeric()
        || !agent_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        || agent_id.as_bytes().windows(2).any(|window| window == b"..")
    {
        return Err("invalid agent identity id".to_string());
    }
    Ok(())
}

fn generate_and_save(agent_id: &str) -> Result<SigningKey, String> {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| format!("CSPRNG unavailable: {e}"))?;
    let signing_key = SigningKey::from_bytes(&seed);

    let key_file = key_path(agent_id)?;
    let pub_file = pub_key_path(agent_id)?;

    if let Some(parent) = key_file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir keys: {e}"))?;
    }

    std::fs::write(&key_file, signing_key.to_bytes()).map_err(|e| format!("write key: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        let _ = std::fs::set_permissions(&key_file, perms);
    }

    let pub_bytes = signing_key.verifying_key().to_bytes();
    std::fs::write(&pub_file, pub_bytes).map_err(|e| format!("write pub: {e}"))?;

    Ok(signing_key)
}

fn load_key(path: &Path) -> Result<SigningKey, String> {
    let bytes = read_key_file(path, "key")?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "invalid key file (expected 32 bytes)".to_string())?;
    Ok(SigningKey::from_bytes(&arr))
}

/// Read one key file from a stable directory/file handle without following a
/// symlink introduced between validation and the read. Unix uses an O_NOFOLLOW
/// directory anchor plus openat; Windows uses a held directory handle and
/// handle-relative NtCreateFile. Unsupported targets fail closed.
fn read_key_file(path: &Path, label: &str) -> Result<Vec<u8>, String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{label} directory is unavailable"))?;

    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::io::Read;
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::OpenOptionsExt;

        let directory = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(parent)
            .map_err(|error| format!("open {label} directory without symlinks: {error}"))?;
        if !directory
            .metadata()
            .map_err(|error| format!("inspect {label} directory: {error}"))?
            .is_dir()
        {
            return Err(format!("{label} directory must be a directory"));
        }
        let file_name = path
            .file_name()
            .ok_or_else(|| format!("{label} filename is unavailable"))?;
        let file_name = CString::new(file_name.as_bytes())
            .map_err(|_| format!("{label} filename contains NUL"))?;
        // SAFETY: the held directory fd and NUL-free filename are valid;
        // O_NOFOLLOW rejects symlink traversal.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                file_name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(format!(
                "open {label} without symlinks: {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: successful openat returned this uniquely-owned descriptor.
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        if !file
            .metadata()
            .map_err(|error| format!("inspect {label}: {error}"))?
            .is_file()
        {
            return Err(format!("{label} must be a regular file"));
        }
        let mut bytes = Vec::new();
        // Both stored Ed25519 keys have exactly 32 bytes. One excess byte lets
        // the caller reject oversize input without reading an unbounded file.
        file.take(33)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("read {label}: {error}"))?;
        Ok(bytes)
    }

    #[cfg(windows)]
    {
        use std::io::Read;
        use std::os::windows::ffi::OsStrExt;
        use std::os::windows::fs::MetadataExt;
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Wdk::Storage::FileSystem::{
            FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT,
            FILE_SYNCHRONOUS_IO_NONALERT,
        };
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES,
            FILE_READ_DATA, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
            SYNCHRONIZE,
        };

        const TRAVERSE_DIRECTORY_ACCESS: u32 =
            FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE;
        const READ_KEY_ACCESS: u32 = FILE_READ_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE;

        let wide_parent = parent
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        // SAFETY: wide_parent is NUL-terminated and remains live through the call;
        // security attributes and template handle are null.
        let directory_handle = unsafe {
            CreateFileW(
                wide_parent.as_ptr(),
                TRAVERSE_DIRECTORY_ACCESS,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if directory_handle == INVALID_HANDLE_VALUE || directory_handle.is_null() {
            let error = std::io::Error::last_os_error();
            return Err(format!(
                "open {label} directory without reparse points: {error}"
            ));
        }
        // SAFETY: successful CreateFileW returned an owned handle.
        let directory = unsafe { std::fs::File::from_raw_handle(directory_handle) };
        let directory_metadata = directory
            .metadata()
            .map_err(|error| format!("inspect {label} directory: {error}"))?;
        if directory_metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(format!("{label} directory reparse point rejected"));
        }
        if !directory_metadata.is_dir() {
            return Err(format!("{label} directory must be a directory"));
        }

        let file_name = path
            .file_name()
            .ok_or_else(|| format!("{label} filename is unavailable"))?
            .encode_wide()
            .collect::<Vec<_>>();
        // NtCreateFile resolves this child against the held directory handle
        // (RootDirectory), so a rename or replacement of the parent pathname
        // cannot redirect the lookup. The shared opener adds OBJ_DONT_REPARSE;
        // FILE_OPEN_REPARSE_POINT keeps the final reparse object unopened as a
        // target so its handle attributes can be rejected below.
        let file = leanctx_native_storage::windows_file::open_relative(
            &directory,
            &file_name,
            READ_KEY_ACCESS,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
        )
        .map_err(|status| {
            use leanctx_native_storage::windows_file::OpenError;
            let reason = match status {
                OpenError::Missing => "Missing (NotFound)",
                OpenError::Reparse => "Reparse",
                OpenError::Unsupported => "Unsupported",
                OpenError::Failure | OpenError::Collision => "Failure",
            };
            format!("open {label} without reparse traversal: {reason}")
        })?;
        let metadata = file
            .metadata()
            .map_err(|error| format!("inspect {label}: {error}"))?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(format!("{label} reparse point rejected"));
        }
        if !metadata.is_file() {
            return Err(format!("{label} must be a regular file"));
        }
        let mut bytes = Vec::new();
        // Stored Ed25519 keys have exactly 32 bytes; the excess byte lets the
        // caller reject oversize input without reading an unbounded file.
        file.take(33)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("read {label}: {error}"))?;
        Ok(bytes)
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (parent, path);
        Err(format!(
            "secure {label} reads are unavailable on this platform; refusing a race-prone key lookup"
        ))
    }
}

/// Derive an Ed25519 keypair deterministically from a recovery phrase.
/// Same phrase always produces the same key, enabling identity recovery
/// across reinstalls and machines.
pub(crate) fn derive_keypair_from_phrase(phrase: &str) -> Result<SigningKey, String> {
    use argon2::Argon2;

    let normalized = phrase.trim().to_lowercase();
    let salt = b"lean-ctx-leaderboard-v1";
    let params =
        argon2::Params::new(65536, 3, 1, Some(32)).map_err(|e| format!("argon2 params: {e}"))?;
    let argon2 = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut seed = [0u8; 32];
    argon2
        .hash_password_into(normalized.as_bytes(), salt, &mut seed)
        .map_err(|e| format!("argon2 hash: {e}"))?;
    Ok(SigningKey::from_bytes(&seed))
}

/// Generate a 4-word recovery phrase from the BIP39 wordlist.
pub(crate) fn generate_recovery_phrase() -> String {
    let mut buf = [0u8; 8];
    getrandom::fill(&mut buf).expect("CSPRNG unavailable");
    (0..4)
        .map(|i| {
            let idx = u16::from_le_bytes([buf[i * 2], buf[i * 2 + 1]]) as usize % 2048;
            super::wordlist::WORDLIST[idx]
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Import a phrase-derived identity: derives the keypair and saves it to disk,
/// replacing any existing key for the given agent_id.
pub(crate) fn import_phrase_identity(agent_id: &str, phrase: &str) -> Result<SigningKey, String> {
    let signing_key = derive_keypair_from_phrase(phrase)?;

    let kp = key_path(agent_id)?;
    let pp = pub_key_path(agent_id)?;
    if let Some(parent) = kp.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir keys: {e}"))?;
    }
    std::fs::write(&kp, signing_key.to_bytes()).map_err(|e| format!("write key: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&kp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::write(&pp, signing_key.verifying_key().to_bytes())
        .map_err(|e| format!("write pub: {e}"))?;

    // Persist phrase for dashboard "Show phrase" feature.
    let phrase_path = kp.with_extension("phrase");
    let _ = std::fs::write(&phrase_path, phrase.trim());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&phrase_path, std::fs::Permissions::from_mode(0o600));
    }

    Ok(signing_key)
}

/// Read the stored recovery phrase for an agent, if one exists.
pub(crate) fn stored_recovery_phrase(agent_id: &str) -> Option<String> {
    let kp = key_path(agent_id).ok()?;
    let phrase_path = kp.with_extension("phrase");
    std::fs::read_to_string(phrase_path)
        .ok()
        .filter(|s| !s.trim().is_empty())
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key| owned.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    #[test]
    fn delivery_id_two_claude_processes_are_distinct() {
        // #1904: both processes see CLAUDECODE=1; the id must still differ.
        let env = env_of(&[("CLAUDECODE", "1")]);
        let a = resolve_delivery_agent_id(&env, 100);
        let b = resolve_delivery_agent_id(&env, 200);
        assert_eq!(a, "claude-100");
        assert_ne!(a, b, "two Claude Code clients must not share an identity");
        assert_ne!(a, "1", "the constant CLAUDECODE value is not an identity");
    }

    #[test]
    fn delivery_id_priority_matrix() {
        let explicit = env_of(&[
            ("LEAN_CTX_AGENT_ID", "ops-agent"),
            ("CURSOR_TASK_ID", "t1"),
            ("CLAUDECODE", "1"),
        ]);
        assert_eq!(resolve_delivery_agent_id(&explicit, 7), "ops-agent");
        let legacy = env_of(&[("LCTX_AGENT_ID", "legacy"), ("CLAUDECODE", "1")]);
        assert_eq!(resolve_delivery_agent_id(&legacy, 7), "legacy");
        let cursor = env_of(&[("CURSOR_TASK_ID", "t1"), ("CLAUDECODE", "1")]);
        assert_eq!(resolve_delivery_agent_id(&cursor, 7), "t1");
        let codex = env_of(&[("CODEX_THREAD_ID", "th")]);
        assert_eq!(resolve_delivery_agent_id(&codex, 7), "codex-7");
        assert_eq!(resolve_delivery_agent_id(env_of(&[]), 7), "local-7");
    }

    #[test]
    fn delivery_id_ignores_blank_values() {
        let env = env_of(&[("LEAN_CTX_AGENT_ID", "  "), ("CURSOR_TASK_ID", "")]);
        assert_eq!(resolve_delivery_agent_id(env, 9), "local-9");
    }

    #[test]
    fn delivery_id_is_stable_within_process() {
        assert_eq!(delivery_agent_id(), delivery_agent_id());
    }
    #[test]
    fn sign_and_verify_roundtrip() {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).unwrap();
        let key = SigningKey::from_bytes(&seed);
        let data = b"test payload";
        let sig = key.sign(data);

        let pub_bytes = key.verifying_key().to_bytes();
        assert!(verify_signature(&pub_bytes, data, &sig.to_bytes()));
    }

    #[test]
    fn verify_rejects_tampered_data() {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).unwrap();
        let key = SigningKey::from_bytes(&seed);
        let sig = key.sign(b"original");

        let pub_bytes = key.verifying_key().to_bytes();
        assert!(!verify_signature(&pub_bytes, b"tampered", &sig.to_bytes()));
    }

    #[test]
    fn hex_roundtrip() {
        let data = vec![0xde, 0xad, 0xbe, 0xef];
        let encoded = hex_encode(&data);
        assert_eq!(encoded, "deadbeef");
        let decoded = hex_decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn hex_decode_accepts_all_bytes_and_rejects_malformed_text() {
        let bytes: Vec<u8> = (0..=255).collect();
        let encoded = hex_encode(&bytes);
        assert_eq!(hex_decode(&encoded).unwrap(), bytes);
        assert_eq!(hex_decode(&encoded.to_uppercase()).unwrap(), bytes);
        assert_eq!(hex_decode("").unwrap(), Vec::<u8>::new());
        for malformed in ["0", "gg", " 0", "\u{1f512}", "0\u{20ac}", "\u{e9}\u{e9}"] {
            assert!(hex_decode(malformed).is_err(), "{malformed:?}");
        }
        assert_eq!(hex_decode("0").unwrap_err(), "odd-length hex string");
        assert_eq!(hex_decode("0\u{20ac}").unwrap_err(), "non-ASCII hex string");
    }

    #[test]
    fn phrase_derivation_is_deterministic() {
        let phrase = "abandon ability able about";
        let key1 = derive_keypair_from_phrase(phrase).unwrap();
        let key2 = derive_keypair_from_phrase(phrase).unwrap();
        assert_eq!(key1.to_bytes(), key2.to_bytes());
        assert_eq!(
            key1.verifying_key().to_bytes(),
            key2.verifying_key().to_bytes()
        );
    }

    #[test]
    fn phrase_derivation_is_case_insensitive() {
        let lower = derive_keypair_from_phrase("abandon ability able about").unwrap();
        let upper = derive_keypair_from_phrase("ABANDON ABILITY ABLE ABOUT").unwrap();
        let mixed = derive_keypair_from_phrase("Abandon Ability Able About").unwrap();
        assert_eq!(lower.to_bytes(), upper.to_bytes());
        assert_eq!(lower.to_bytes(), mixed.to_bytes());
    }

    #[test]
    fn phrase_derivation_trims_whitespace() {
        let clean = derive_keypair_from_phrase("abandon ability able about").unwrap();
        let padded = derive_keypair_from_phrase("  abandon ability able about  ").unwrap();
        assert_eq!(clean.to_bytes(), padded.to_bytes());
    }

    #[test]
    fn different_phrases_produce_different_keys() {
        let key1 = derive_keypair_from_phrase("abandon ability able about").unwrap();
        let key2 = derive_keypair_from_phrase("zoo zero zone youth").unwrap();
        assert_ne!(key1.to_bytes(), key2.to_bytes());
    }

    #[test]
    fn generated_phrase_has_four_words() {
        let phrase = generate_recovery_phrase();
        let words: Vec<_> = phrase.split_whitespace().collect();
        assert_eq!(words.len(), 4);
        for word in &words {
            assert!(
                crate::core::wordlist::WORDLIST.contains(word),
                "word \"{word}\" not in BIP39 wordlist"
            );
        }
    }

    #[test]
    fn phrase_sign_verify_roundtrip() {
        let phrase = "abandon ability able about";
        let key = derive_keypair_from_phrase(phrase).unwrap();
        let data = b"test data for leaderboard";
        let sig = sign_bytes_with(&key, data);
        let pub_bytes = key.verifying_key().to_bytes();
        assert!(verify_signature(&pub_bytes, data, &sig));
    }

    #[test]
    fn import_phrase_identity_roundtrip() {
        let _isolated = crate::core::data_dir::isolated_data_dir();
        let phrase = "abandon ability able about";
        let key = import_phrase_identity("test-rejoin", phrase).unwrap();

        // Stored phrase is readable
        let stored = stored_recovery_phrase("test-rejoin");
        assert_eq!(stored.as_deref(), Some(phrase));

        // Loading the key from disk produces the same keypair
        let loaded = get_or_create_keypair("test-rejoin").unwrap();
        assert_eq!(key.to_bytes(), loaded.to_bytes());
    }

    #[test]
    fn stored_public_key_is_read_only_and_ids_are_path_safe() {
        let _isolated = crate::core::data_dir::isolated_data_dir();
        assert!(get_stored_public_key("missing").is_err());
        assert!(get_stored_public_key("../outside").is_err());
        assert!(get_stored_public_key("/tmp/outside").is_err());
        assert!(get_or_create_keypair("agent/child").is_err());

        let expected = get_or_create_keypair("root-agent").unwrap().verifying_key();
        assert_eq!(get_stored_public_key("root-agent").unwrap(), expected);
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn stored_public_key_rejects_symlinks() {
        let _isolated = crate::core::data_dir::isolated_data_dir();
        let keys_dir = crate::core::data_dir::lean_ctx_data_dir()
            .unwrap()
            .join("keys");
        std::fs::create_dir_all(&keys_dir).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_key = outside.path().join("outside.pub");
        std::fs::write(&outside_key, [7_u8; 32]).unwrap();
        create_file_symlink(&outside_key, &keys_dir.join("linked.pub"));
        assert!(get_stored_public_key("linked").is_err());
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn stored_public_key_rejects_traversal_absolute_and_symlink_paths() {
        let _isolated = crate::core::data_dir::isolated_data_dir();
        assert!(get_stored_public_key("../outside").is_err());
        assert!(get_stored_public_key("/tmp/outside").is_err());
        assert!(get_or_create_keypair("../outside").is_err());
        assert!(import_phrase_identity("/tmp/outside", "abandon ability able about").is_err());

        let data_dir = crate::core::data_dir::lean_ctx_data_dir().unwrap();
        let keys_dir = data_dir.join("keys");
        std::fs::create_dir_all(&keys_dir).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_key = outside.path().join("outside.pub");
        std::fs::write(
            &outside_key,
            SigningKey::from_bytes(&[19_u8; 32])
                .verifying_key()
                .to_bytes(),
        )
        .unwrap();
        create_file_symlink(&outside_key, &keys_dir.join("linked.pub"));
        assert!(get_stored_public_key("linked").is_err());
        create_file_symlink(&outside_key, &keys_dir.join("linked.key"));
        assert!(get_or_create_keypair("linked").is_err());

        std::fs::remove_file(keys_dir.join("linked.pub")).unwrap();
        std::fs::remove_file(keys_dir.join("linked.key")).unwrap();
        std::fs::create_dir(keys_dir.join("directory.pub")).unwrap();
        assert!(get_stored_public_key("directory").is_err());
        std::fs::remove_dir(keys_dir.join("directory.pub")).unwrap();
        std::fs::create_dir(keys_dir.join("directory.key")).unwrap();
        assert!(get_or_create_keypair("directory").is_err());
        std::fs::remove_dir(keys_dir.join("directory.key")).unwrap();

        std::fs::remove_dir(&keys_dir).unwrap();
        create_directory_symlink(outside.path(), &keys_dir);
        assert!(get_stored_public_key("outside").is_err());
    }

    #[test]
    fn agent_id_is_one_bounded_canonical_filename_component() {
        assert!(validate_agent_id("agent-01.alpha").is_ok());
        assert!(validate_agent_id("../outside").is_err());
        assert!(validate_agent_id("agent/child").is_err());
        assert!(validate_agent_id("agent\\child").is_err());
        assert!(validate_agent_id("agent:key").is_err());
        assert!(validate_agent_id("agent..child").is_err());
        assert!(validate_agent_id(&"a".repeat(129)).is_err());
    }

    #[test]
    fn stored_key_reads_are_bounded_and_reject_directories() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("key");
        std::fs::write(&path, [7_u8; 32]).unwrap();
        assert!(load_key(&path).is_ok());
        std::fs::write(&path, vec![7_u8; 1024 * 1024]).unwrap();
        assert_eq!(read_key_file(&path, "key").unwrap().len(), 33);
        assert!(load_key(&path).is_err());

        let directory = root.path().join("directory.key");
        std::fs::create_dir(&directory).unwrap();
        assert!(load_key(&directory).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn stored_key_reads_reject_fifos_without_a_writer() {
        use std::os::unix::ffi::OsStrExt;
        let root = tempfile::tempdir().unwrap();
        let fifo = root.path().join("fifo");
        let raw = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: NUL-free owned path inside this test's fresh directory.
        assert_eq!(unsafe { libc::mkfifo(raw.as_ptr(), 0o600) }, 0);
        assert!(load_key(&fifo).is_err());
        assert!(load_key(&fifo.join("child")).is_err());
    }

    #[cfg(unix)]
    fn create_file_symlink(target: &Path, link: &Path) {
        std::os::unix::fs::symlink(target, link).unwrap();
    }

    #[cfg(windows)]
    fn create_file_symlink(target: &Path, link: &Path) {
        std::os::windows::fs::symlink_file(target, link).unwrap();
    }

    #[cfg(unix)]
    fn create_directory_symlink(target: &Path, link: &Path) {
        std::os::unix::fs::symlink(target, link).unwrap();
    }

    #[cfg(windows)]
    fn create_directory_symlink(target: &Path, link: &Path) {
        std::os::windows::fs::symlink_dir(target, link).unwrap();
    }
}
