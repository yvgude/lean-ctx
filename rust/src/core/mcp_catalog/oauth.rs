// SPDX-License-Identifier: Apache-2.0
//! OAuth for HTTP MCP servers (#1391).
//!
//! Some servers — TwinMind is the reported one — accept only a token obtained
//! through a browser login. The protocol work (protected-resource and
//! authorization-server discovery, dynamic client registration, PKCE, code
//! exchange, refresh) is `rmcp::transport::auth`. This module adds what a CLI
//! needs around it: a loopback redirect listener, a credential store that
//! survives restarts, and the lookup the gateway uses to attach the token.
//!
//! Storage: each server's credentials are encrypted with XChaCha20-Poly1305
//! (the server URL is authenticated as associated data) into
//! `<data_dir>/mcp-oauth/<id>.bin`, mode 0600. The 32-byte key lives in the OS
//! keychain on macOS and Windows. Elsewhere — or when the keychain refuses
//! access — it sits in a 0600 key file in the same directory, which protects
//! the tokens exactly as well as the file permissions do.

use std::path::PathBuf;
use std::time::Duration;

use rmcp::transport::auth::{
    AuthClient, AuthError, AuthorizationManager, CredentialStore, OAuthState, StoredCredentials,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Name lean-ctx registers under with the authorization server.
const CLIENT_NAME: &str = "lean-ctx";
/// Path of the loopback redirect URI.
const CALLBACK_PATH: &str = "/callback";
/// First byte of a credential file: the storage format version.
const FORMAT_V1: u8 = 1;
const NONCE_LEN: usize = 24;
/// Upper bound on the browser's callback request head.
const MAX_REQUEST_BYTES: usize = 16 * 1024;

/// A browser-authorised HTTP client for `url`, or an error that tells the user
/// how to authorise. Refresh happens inside [`AuthClient`] when the access
/// token expires, and the refreshed credentials are written back to the store.
pub(super) async fn authorized_client(
    url: &str,
    server: &str,
) -> Result<AuthClient<reqwest::Client>, String> {
    let mut manager = AuthorizationManager::new(url)
        .await
        .map_err(|e| format!("OAuth setup for `{server}` failed: {e}"))?;
    manager.set_credential_store(PersistentCredentialStore::new(url));
    let authorised = manager
        .initialize_from_store()
        .await
        .map_err(|e| format!("reading OAuth credentials for `{server}` failed: {e}"))?;
    if !authorised {
        return Err(format!(
            "`{server}` needs a browser login first: run  lean-ctx addon auth {server}"
        ));
    }
    Ok(AuthClient::new(reqwest::Client::default(), manager))
}

/// Run the browser login for `url` and store the resulting credentials.
///
/// `show_url` receives the authorization URL once it is known, so the caller
/// can print it and open a browser. The call returns after the browser has
/// been redirected back with a code and the code has been exchanged, or after
/// `timeout`.
pub(crate) async fn authorize(
    url: &str,
    scopes: &[String],
    timeout: Duration,
    show_url: impl FnOnce(&str),
) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| format!("cannot open a local port for the OAuth redirect: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("local OAuth redirect port: {e}"))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}{CALLBACK_PATH}");

    let mut state = OAuthState::new(url, None).await.map_err(describe)?;
    if let OAuthState::Unauthorized(manager) = &mut state {
        manager.set_credential_store(PersistentCredentialStore::new(url));
    }
    let scope_refs: Vec<&str> = scopes.iter().map(String::as_str).collect();
    state
        .start_authorization(&scope_refs, &redirect_uri, Some(CLIENT_NAME))
        .await
        .map_err(describe)?;
    let auth_url = match &state {
        OAuthState::Session(session) => session.get_authorization_url().to_string(),
        _ => return Err("OAuth did not start an authorization session".to_string()),
    };
    show_url(&auth_url);

    let path = tokio::time::timeout(timeout, wait_for_callback(&listener))
        .await
        .map_err(|_| {
            format!(
                "no browser login within {}s — run the command again to retry",
                timeout.as_secs()
            )
        })??;
    state
        .handle_callback_url(&format!("http://127.0.0.1:{port}{path}"))
        .await
        .map_err(describe)
}

/// Whether credentials are stored for `url`.
pub(crate) fn has_credentials(url: &str) -> Result<bool, String> {
    Ok(load(url)?.is_some_and(|stored| stored.token_response.is_some()))
}

/// Delete the stored credentials for `url`. Returns whether any were there.
pub(crate) fn forget(url: &str) -> Result<bool, String> {
    let path = credential_path(url)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("remove {}: {e}", path.display())),
    }
}

/// Where the encryption key lives, for the CLI to tell the user.
pub(crate) fn key_location() -> String {
    match keychain::NAME {
        Some(name) if keychain::load_or_create().is_ok() => name.to_string(),
        _ => store_dir().map_or_else(
            |_| "a 0600 key file in the lean-ctx data directory".to_string(),
            |dir| format!("{} (mode 0600)", dir.join(KEY_FILE).display()),
        ),
    }
}

fn describe(error: AuthError) -> String {
    match error {
        AuthError::NoAuthorizationSupport => {
            "the server does not advertise OAuth (no authorization-server metadata)".to_string()
        }
        other => format!("OAuth failed: {other}"),
    }
}

/// Accept connections on the loopback listener until the browser requests the
/// callback path; answer it with a short page and return its path and query.
/// Anything else (a favicon request) gets a 404 and the wait continues.
async fn wait_for_callback(listener: &tokio::net::TcpListener) -> Result<String, String> {
    loop {
        let (mut stream, _) = listener
            .accept()
            .await
            .map_err(|e| format!("OAuth redirect listener failed: {e}"))?;
        let Some(target) = read_request_target(&mut stream).await else {
            continue;
        };
        if target.split('?').next() != Some(CALLBACK_PATH) {
            let _ = respond(&mut stream, "404 Not Found", "Not found.").await;
            continue;
        }
        if let Some(error) = query_param(&target, "error") {
            let detail = query_param(&target, "error_description").unwrap_or_default();
            let _ = respond(
                &mut stream,
                "200 OK",
                "Login was not completed. You can close this tab.",
            )
            .await;
            return Err(
                format!("the authorization server refused: {error} {detail}")
                    .trim_end()
                    .to_string(),
            );
        }
        let _ = respond(
            &mut stream,
            "200 OK",
            "lean-ctx is authorised. You can close this tab.",
        )
        .await;
        return Ok(target);
    }
}

/// The request target of a `GET` request head, or `None` for anything else.
async fn read_request_target(stream: &mut tokio::net::TcpStream) -> Option<String> {
    let mut head = Vec::new();
    let mut buf = [0u8; 2048];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < MAX_REQUEST_BYTES {
        let n = stream.read(&mut buf).await.ok()?;
        if n == 0 {
            break;
        }
        head.extend_from_slice(&buf[..n]);
    }
    let head = String::from_utf8_lossy(&head);
    let mut parts = head.lines().next()?.split_whitespace();
    (parts.next()? == "GET").then(|| parts.next().map(str::to_string))?
}

async fn respond(
    stream: &mut tokio::net::TcpStream,
    status: &str,
    message: &str,
) -> std::io::Result<()> {
    let body =
        format!("<!doctype html><meta charset=\"utf-8\"><title>lean-ctx</title><p>{message}</p>");
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await
}

fn query_param(target: &str, name: &str) -> Option<String> {
    let query = target.split_once('?')?.1;
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        (key == name).then(|| {
            urlencoding::decode(&value.replace('+', " "))
                .map_or_else(|_| value.to_string(), std::borrow::Cow::into_owned)
        })
    })
}

// --- storage ---------------------------------------------------------------

const KEY_FILE: &str = "key";

/// `rmcp` credential store backed by the encrypted per-server file.
struct PersistentCredentialStore {
    url: String,
}

impl PersistentCredentialStore {
    fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
        }
    }
}

#[async_trait::async_trait]
impl CredentialStore for PersistentCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        load(&self.url).map_err(AuthError::InternalError)
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        save(&self.url, &credentials).map_err(AuthError::InternalError)
    }

    async fn clear(&self) -> Result<(), AuthError> {
        forget(&self.url)
            .map(|_| ())
            .map_err(AuthError::InternalError)
    }
}

fn normalize_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

fn store_dir() -> Result<PathBuf, String> {
    let dir = crate::core::data_dir::lean_ctx_data_dir()?.join("mcp-oauth");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(dir)
}

fn credential_path(url: &str) -> Result<PathBuf, String> {
    let id = blake3::hash(normalize_url(url).as_bytes()).to_hex();
    Ok(store_dir()?.join(format!("{}.bin", &id[..24])))
}

fn load(url: &str) -> Result<Option<StoredCredentials>, String> {
    let path = credential_path(url)?;
    let blob = match std::fs::read(&path) {
        Ok(blob) => blob,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let plaintext = open(&data_key()?, url, &blob).map_err(|e| {
        format!(
            "{} cannot be decrypted ({e}); run `lean-ctx addon auth <name>` again",
            path.display()
        )
    })?;
    serde_json::from_slice(&plaintext)
        .map(Some)
        .map_err(|e| format!("{} is not valid credentials: {e}", path.display()))
}

fn save(url: &str, credentials: &StoredCredentials) -> Result<(), String> {
    let path = credential_path(url)?;
    let plaintext = serde_json::to_vec(credentials).map_err(|e| e.to_string())?;
    let blob = seal(&data_key()?, url, &plaintext)?;
    write_private(&path, &blob)
}

fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    #[cfg(unix)]
    let permissions = {
        use std::os::unix::fs::PermissionsExt;
        Some(std::fs::Permissions::from_mode(0o600))
    };
    #[cfg(not(unix))]
    let permissions = None;
    crate::core::atomic_fs::try_atomic_write(path, bytes, permissions.as_ref())
        .map_err(|e| format!("write {}: {e}", path.display()))
}

/// `version || nonce || ciphertext`, with the normalised URL as associated
/// data so a credential file cannot be replayed for another server.
fn seal(key: &[u8; 32], url: &str, plaintext: &[u8]) -> Result<Vec<u8>, String> {
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    use chacha20poly1305::{XChaCha20Poly1305, XNonce};

    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(|e| format!("nonce generation: {e}"))?;
    let aad = normalize_url(url);
    let ciphertext = XChaCha20Poly1305::new(key.into())
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| "encryption failed".to_string())?;
    let mut out = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
    out.push(FORMAT_V1);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

fn open(key: &[u8; 32], url: &str, blob: &[u8]) -> Result<Vec<u8>, String> {
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    use chacha20poly1305::{XChaCha20Poly1305, XNonce};

    let Some((&FORMAT_V1, rest)) = blob.split_first() else {
        return Err("unknown storage format".to_string());
    };
    if rest.len() <= NONCE_LEN {
        return Err("truncated".to_string());
    }
    let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
    let aad = normalize_url(url);
    XChaCha20Poly1305::new(key.into())
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| "wrong key or tampered file".to_string())
}

/// The storage key: from the OS keychain where there is one, else the 0600
/// key file (created on first use).
fn data_key() -> Result<[u8; 32], String> {
    if keychain::NAME.is_some()
        && let Ok(key) = keychain::load_or_create()
    {
        return Ok(key);
    }
    file_key()
}

fn file_key() -> Result<[u8; 32], String> {
    let path = store_dir()?.join(KEY_FILE);
    match std::fs::read(&path) {
        Ok(bytes) => bytes.try_into().map_err(|_| {
            format!(
                "{} is not a 32-byte key; delete it and run `lean-ctx addon auth <name>` again",
                path.display()
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let key = random_key()?;
            write_private(&path, &key)?;
            Ok(key)
        }
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
}

fn random_key() -> Result<[u8; 32], String> {
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|e| format!("key generation: {e}"))?;
    Ok(key)
}

/// OS keychain holding the storage key. Tests never touch the real keychain.
#[cfg(all(any(target_os = "macos", windows), not(test)))]
mod keychain {
    const SERVICE: &str = "lean-ctx";
    const ACCOUNT: &str = "mcp-oauth-storage-key";

    #[cfg(target_os = "macos")]
    pub(super) const NAME: Option<&str> = Some("the macOS Keychain");
    #[cfg(windows)]
    pub(super) const NAME: Option<&str> = Some("Windows Credential Manager");

    pub(super) fn load_or_create() -> Result<[u8; 32], String> {
        let entry = keyring::Entry::new(SERVICE, ACCOUNT).map_err(|e| e.to_string())?;
        match entry.get_secret() {
            Ok(bytes) => bytes
                .try_into()
                .map_err(|_| "keychain entry is not a 32-byte key".to_string()),
            Err(keyring::Error::NoEntry) => {
                let key = super::random_key()?;
                entry.set_secret(&key).map_err(|e| e.to_string())?;
                Ok(key)
            }
            Err(e) => Err(e.to_string()),
        }
    }
}

#[cfg(not(all(any(target_os = "macos", windows), not(test))))]
mod keychain {
    pub(super) const NAME: Option<&str> = None;

    pub(super) fn load_or_create() -> Result<[u8; 32], String> {
        Err("no OS keychain on this platform".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credentials(client_id: &str) -> StoredCredentials {
        StoredCredentials::new(
            client_id.to_string(),
            None,
            vec!["mcp:read".into()],
            Some(1),
        )
    }

    /// Stored credentials survive a reload, are bound to their server URL, and
    /// are gone after `forget`.
    #[test]
    fn credentials_are_encrypted_per_server_and_forgettable() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let url = "https://mcp.example.com/mcp";
        save(url, &credentials("client-1")).unwrap();

        let path = credential_path(url).unwrap();
        let raw = std::fs::read(&path).unwrap();
        assert!(
            !raw.windows(8).any(|w| w == b"client-1"),
            "credentials must not be stored in clear text"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "credential file must be owner-only");
        }

        let loaded = load(&format!("{url}/")).unwrap().expect("stored");
        assert_eq!(loaded.client_id, "client-1");

        // A file copied to another server's slot does not decrypt there.
        let other = "https://evil.example.com/mcp";
        std::fs::copy(&path, credential_path(other).unwrap()).unwrap();
        assert!(
            load(other).is_err(),
            "credentials must be bound to their URL"
        );

        assert!(forget(url).unwrap());
        assert!(load(url).unwrap().is_none());
        assert!(!forget(url).unwrap(), "second forget is a no-op");
    }

    /// The loopback listener hands back the callback target, ignores stray
    /// requests, and turns an authorization error into a readable failure.
    #[tokio::test]
    async fn callback_listener_returns_the_code_or_the_refusal() {
        async fn request(port: u16, target: &str) {
            let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap();
            stream
                .write_all(format!("GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut response = Vec::new();
            let _ = stream.read_to_end(&mut response).await;
        }

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = tokio::spawn(async move {
            request(port, "/favicon.ico").await;
            request(port, "/callback?code=abc&state=xyz").await;
        });
        assert_eq!(
            wait_for_callback(&listener).await.unwrap(),
            "/callback?code=abc&state=xyz"
        );
        client.await.unwrap();

        let client = tokio::spawn(async move {
            request(
                port,
                "/callback?error=access_denied&error_description=User+said+no",
            )
            .await;
        });
        let err = wait_for_callback(&listener).await.unwrap_err();
        assert!(err.contains("access_denied User said no"), "{err}");
        client.await.unwrap();
    }
}
