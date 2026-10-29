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

/// The connected client of this single-client MCP process, set once at
/// `initialize` (see [`set_session_attribution`]).
static SESSION_ATTRIBUTION: OnceLock<String> = OnceLock::new();
/// Set when a second, different client initialized in the same process: the
/// process-wide attribution is then ambiguous and is no longer used.
static SESSION_ATTRIBUTION_CONFLICT: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Attribution for savings-ledger events (`SavingsEvent::agent_id`), so
/// corrections and savings can be told apart per agent instead of all
/// reading `"local"`.
///
/// Priority (first available wins):
/// 1. `LEAN_CTX_AGENT_ID` / `LCTX_AGENT_ID` — explicit operator identity
///    (same value as [`current_agent_id`])
/// 2. this process's MCP session: `<client>:<bus agent id>`, e.g.
///    `claude-code:mcp-1274-…`, set at `initialize` for single-client servers
/// 3. the host the process runs under (`claude`, `codex`, `cursor`) — hook and
///    CLI processes spawned by an agent
/// 4. `"local"`
///
/// Only the attribution changes: signing keys stay bound to
/// [`current_agent_id`], and `agent_id` is an ordinary hashed field in every
/// canonical event version, so existing entries keep verifying unchanged.
#[must_use]
pub(crate) fn ledger_agent_id() -> String {
    let explicit = current_agent_id();
    if explicit != "local" {
        return explicit.to_string();
    }
    if !SESSION_ATTRIBUTION_CONFLICT.load(std::sync::atomic::Ordering::Relaxed)
        && let Some(session) = SESSION_ATTRIBUTION.get()
    {
        return session.clone();
    }
    host_from_env(|key| std::env::var(key).ok())
        .unwrap_or("local")
        .to_string()
}

/// Record the MCP client and local bus identity of this process. Called once
/// per `initialize` by single-client (personal) servers; a later initialize
/// with a different identity marks the attribution ambiguous instead of
/// silently re-labelling another client's events.
pub(crate) fn set_session_attribution(client_name: &str, bus_agent_id: Option<&str>) {
    let Some(value) = session_attribution_value(client_name, bus_agent_id) else {
        return;
    };
    if let Err(value) = SESSION_ATTRIBUTION.set(value)
        && SESSION_ATTRIBUTION.get() != Some(&value)
    {
        SESSION_ATTRIBUTION_CONFLICT.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Bounded, ledger-safe `<client>:<bus agent id>` label.
fn session_attribution_value(client_name: &str, bus_agent_id: Option<&str>) -> Option<String> {
    let client = attribution_component(client_name, 48);
    let agent = bus_agent_id.and_then(|id| attribution_component(id, 72));
    match (client, agent) {
        (Some(client), Some(agent)) => Some(format!("{client}:{agent}")),
        (Some(single), None) | (None, Some(single)) => Some(single),
        (None, None) => None,
    }
}

/// Lowercase `[a-z0-9._-]`, other characters folded to `-`, at most `max`
/// bytes; `None` when nothing meaningful remains.
fn attribution_component(raw: &str, max: usize) -> Option<String> {
    let folded: String = raw
        .trim()
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .take(max)
        .collect();
    let trimmed = folded.trim_matches('-');
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// The agent host this process was spawned by, from the markers each host
/// sets in its children's environment.
fn host_from_env(env: impl Fn(&str) -> Option<String>) -> Option<&'static str> {
    let set = |key: &str| env(key).is_some_and(|v| !v.trim().is_empty());
    if set("CLAUDECODE") {
        Some("claude")
    } else if set("CODEX_THREAD_ID") {
        Some("codex")
    } else if set("CURSOR_TASK_ID") {
        Some("cursor")
    } else {
        None
    }
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
    fn session_attribution_combines_client_and_bus_id() {
        assert_eq!(
            session_attribution_value("claude-code", Some("mcp-1274-fbba0c02")).as_deref(),
            Some("claude-code:mcp-1274-fbba0c02")
        );
        assert_eq!(
            session_attribution_value("Codex MCP Client", None).as_deref(),
            Some("codex-mcp-client")
        );
        assert_eq!(
            session_attribution_value("", Some("mcp-9-x")).as_deref(),
            Some("mcp-9-x")
        );
        assert_eq!(session_attribution_value("  ", None), None);
    }

    #[test]
    fn session_attribution_is_bounded_and_ledger_safe() {
        let long = "x".repeat(500);
        let value = session_attribution_value(&format!("{long}\n:/"), Some(&long)).unwrap();
        assert!(value.len() <= 48 + 1 + 72, "{}", value.len());
        assert!(
            value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':'))
        );
    }

    #[test]
    fn host_from_env_detects_agent_hosts() {
        assert_eq!(
            host_from_env(env_of(&[("CLAUDECODE", "1")])),
            Some("claude")
        );
        assert_eq!(
            host_from_env(env_of(&[("CODEX_THREAD_ID", "t-1")])),
            Some("codex")
        );
        assert_eq!(
            host_from_env(env_of(&[("CURSOR_TASK_ID", "task")])),
            Some("cursor")
        );
        assert_eq!(host_from_env(env_of(&[("CLAUDECODE", " ")])), None);
        assert_eq!(host_from_env(env_of(&[])), None);
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
