// SPDX-License-Identifier: Apache-2.0
//! OAuth 2.1 for remote MCP clients (Masterplan ux16).
//!
//! `lean-ctx serve --oauth` turns the instance into its own authorization
//! server, as the MCP authorization specification describes: a remote client
//! (ChatGPT, Claude, an IDE on another machine) discovers the protected
//! resource metadata (RFC 9728) from the `WWW-Authenticate` header of a 401,
//! reads the authorization server metadata (RFC 8414), registers itself
//! (RFC 7591, public clients only) and runs the authorization code flow with
//! PKCE S256. The owner approves each client in the browser with the
//! instance's own bearer token, which therefore never has to be pasted into a
//! third-party client; the client receives short-lived access tokens and
//! rotating refresh tokens instead.
//!
//! Security properties (see the tests in `oauth_tests.rs`):
//! - only S256 PKCE, codes are single use, live 60 s, and a replayed code
//!   revokes every token issued from it;
//! - refresh tokens rotate; presenting a used one revokes its whole family;
//! - redirect URIs are matched exactly (loopback: any port, RFC 8252);
//! - tokens and the instance token fingerprint are stored only as
//!   HMAC-SHA256 under a per-install secret (`oauth-secret.key`), so a copied
//!   store cannot be checked against guessed tokens offline; the store and
//!   the secret are owner-only files (0600 on Unix, a protected
//!   current-user-only DACL on Windows); the secret is created exclusively,
//!   and one that is not owner-only is replaced, which ends every grant;
//! - repeated failed token or consent attempts lock that client out with a
//!   growing backoff, kept in a bounded table; the lockout refuses only
//!   invalid credentials, so it cannot lock a legitimate client out;
//! - stored token hashes are compared in constant time;
//! - an OAuth token reaches only the MCP endpoint and read-only discovery
//!   routes (an allowlist); everything else needs the instance token;
//! - a changed explicit instance token, or a lost secret, drops every grant;
//! - errors are redirected only to clients the owner approved before.

use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use axum::{
    Json, Router,
    extract::{Extension, Form, Path, Query, State},
    http::{HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The one scope an OAuth client is granted: the MCP tools of this instance.
pub(super) const SCOPE: &str = "mcp";

const ACCESS_TTL_SECS: u64 = 3600;
const REFRESH_TTL_SECS: u64 = 30 * 24 * 3600;
const CODE_TTL_SECS: u64 = 60;
const PENDING_TTL_SECS: u64 = 600;
const MAX_CLIENTS: usize = 100;
const MAX_TOKENS: usize = 2000;
const MAX_PENDING: usize = 64;
const MAX_CODES: usize = 256;
const MAX_REDIRECT_URIS: usize = 8;
const MAX_URI_LEN: usize = 2048;
const MAX_CLIENT_NAME: usize = 100;
const OWNER_FAILURE_WINDOW_SECS: u64 = 600;
const OWNER_FAILURE_LIMIT: usize = 10;
/// Failed token or consent attempts of one client before it is locked out.
const CLIENT_FAILURE_LIMIT: u32 = 5;
/// A client's failures are forgotten after this long without another one.
const CLIENT_FAILURE_WINDOW_SECS: u64 = 600;
/// The first lockout; each further failure doubles it, up to the maximum.
const CLIENT_BACKOFF_BASE_SECS: u64 = 30;
const CLIENT_BACKOFF_MAX_SECS: u64 = 900;
/// Bound of the per-client failure table.
const MAX_FAILURE_ENTRIES: usize = 256;
/// The per-install HMAC key, beside the stores of every issuer.
const SECRET_FILE: &str = "oauth-secret.key";
const MAX_STORE_BYTES: usize = 8 * 1024 * 1024;
/// 2: hashes are HMAC-SHA256 under the install secret. 1 held plain
/// SHA-256 hashes; its tokens cannot be converted and are dropped.
const STORE_VERSION: u32 = 2;
const LEGACY_STORE_VERSION: u32 = 1;

/// How the bearer of a request was authenticated; set by the auth
/// middleware for every authenticated request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum AuthKind {
    /// The instance's own bearer token.
    Owner,
    /// An OAuth access token issued to this client.
    OAuth { client_id: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Client {
    client_id: String,
    client_name: String,
    redirect_uris: Vec<String>,
    created_at: u64,
    /// The owner approved this client at least once. Until then an
    /// authorization error is shown on our page and never redirected, so
    /// a self-registered client cannot use `/oauth/authorize` as an open
    /// redirector (RFC 9700 §4.11.2).
    #[serde(default)]
    approved: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TokenKind {
    Access,
    Refresh,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TokenRecord {
    /// HMAC-SHA256 of the token under the install secret, hex. The token
    /// itself is never stored.
    hash: String,
    kind: TokenKind,
    /// One authorization grant: the code and every refresh after it.
    family: String,
    client_id: String,
    expires_at: u64,
    #[serde(default)]
    used: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Persisted {
    version: u32,
    /// Fingerprint of the explicitly configured instance token the grants
    /// were approved under; a different token drops every grant.
    #[serde(default)]
    owner_fingerprint: Option<String>,
    clients: Vec<Client>,
    tokens: Vec<TokenRecord>,
}

struct Pending {
    client_id: String,
    redirect_uri: String,
    redirect_uri_given: bool,
    code_challenge: String,
    state: Option<String>,
    expires_at: u64,
}

struct CodeRecord {
    client_id: String,
    redirect_uri: String,
    redirect_uri_given: bool,
    code_challenge: String,
    expires_at: u64,
    /// Set once redeemed; a second redemption revokes this family.
    redeemed_family: Option<String>,
}

/// Failed attempts of one client (token endpoint or consent).
#[derive(Clone, Copy, Debug, Default)]
struct ClientFailures {
    count: u32,
    last_at: u64,
    locked_until: u64,
}

struct Inner {
    persisted: Persisted,
    pending: HashMap<String, Pending>,
    codes: HashMap<String, CodeRecord>,
    owner_failures: Vec<u64>,
    /// Bounded by [`MAX_FAILURE_ENTRIES`]; memory only.
    client_failures: HashMap<String, ClientFailures>,
}

/// The authorization server of one `serve` instance.
pub(super) struct OAuthState {
    issuer: String,
    owner_token: String,
    store: Option<PathBuf>,
    /// HMAC key of every stored hash: the per-install secret, or a random
    /// one per process when nothing is persisted.
    secret: [u8; 32],
    inner: Mutex<Inner>,
}

/// A token pair handed to a client.
#[derive(Debug)]
pub(super) struct IssuedTokens {
    pub access_token: String,
    pub refresh_token: String,
}

/// An OAuth error as RFC 6749 §5.2 spells it.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct OAuthError {
    pub status: StatusCode,
    pub code: &'static str,
    pub description: String,
    /// Seconds until a locked-out client may try again (`Retry-After`).
    pub retry_after: Option<u64>,
}

impl OAuthError {
    fn new(status: StatusCode, code: &'static str, description: impl Into<String>) -> Self {
        Self {
            status,
            code,
            description: description.into(),
            retry_after: None,
        }
    }
    fn locked(retry_after: u64) -> Self {
        Self {
            retry_after: Some(retry_after),
            ..Self::new(
                StatusCode::TOO_MANY_REQUESTS,
                "temporarily_unavailable",
                format!(
                    "too many failed attempts for this client; try again in {retry_after} seconds"
                ),
            )
        }
    }
    fn grant(description: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_grant", description)
    }
    fn request(description: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", description)
    }
}

impl IntoResponse for OAuthError {
    fn into_response(self) -> Response {
        let mut resp = (
            self.status,
            Json(serde_json::json!({
                "error": self.code,
                "error_description": self.description,
            })),
        )
            .into_response();
        no_store(&mut resp);
        if let Some(seconds) = self.retry_after
            && let Ok(value) = HeaderValue::from_str(&seconds.to_string())
        {
            resp.headers_mut().insert(header::RETRY_AFTER, value);
        }
        resp
    }
}

pub(super) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn random_token(prefix: &str) -> String {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("getrandom failed");
    format!(
        "{prefix}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
    )
}

/// HMAC-SHA256 (RFC 2104) of the concatenated `parts` under `key`, from the
/// vetted `hmac` crate.
fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    use hmac::{Hmac, KeyInit as _, Mac as _};
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes a key of any length");
    for part in parts {
        mac.update(part);
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&mac.finalize().into_bytes());
    out
}

fn random_secret() -> [u8; 32] {
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).expect("getrandom failed");
    secret
}

/// The owner fingerprint of store version 1 (plain SHA-256), only read to
/// carry the clients of a version 1 store over.
fn legacy_owner_fingerprint(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"lean-ctx oauth owner token v1\0");
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

fn keyed_owner_fingerprint(secret: &[u8; 32], token: &str) -> String {
    hex::encode(hmac_sha256(
        secret,
        &[b"lean-ctx oauth owner token v2\0", token.as_bytes()],
    ))
}

/// Owner-only files: 0600 in a 0700 directory on Unix (`secure_fs`), a
/// protected current-user-only DACL on Windows (`windows_private`, as the
/// personal policy and learning stores use it).
mod private_io {
    use std::io;
    use std::path::Path;

    #[cfg(windows)]
    fn directory(
        dir: &Path,
        create: bool,
    ) -> io::Result<lean_ctx::core::windows_private::Directory> {
        use lean_ctx::core::windows_private::{Directory, Privacy};
        match Directory::open(dir, Privacy::Writable) {
            Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                Directory::create_chain(dir)
            }
            result => result,
        }
    }

    /// Reads a file only when it is private to the current user.
    #[cfg(windows)]
    pub(super) fn read(dir: &Path, name: &str, limit: usize) -> io::Result<Vec<u8>> {
        directory(dir, false)?.read_file(name, limit)
    }

    /// Writes atomically with the private ACL set at creation.
    #[cfg(windows)]
    pub(super) fn write(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
        directory(dir, true)?.atomic_write(name, bytes, true)
    }

    /// Creates `name` only when it does not exist yet: the private file is
    /// written whole and renamed into place without replacing, so another
    /// process's file stays untouched and this fails with `AlreadyExists`.
    #[cfg(windows)]
    pub(super) fn create_new(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
        directory(dir, true)?.atomic_write(name, bytes, false)
    }

    /// Reads a secret: `read_file` already refuses a file that is not
    /// private to the current user.
    #[cfg(windows)]
    pub(super) fn read_secret(dir: &Path, name: &str, limit: usize) -> io::Result<Vec<u8>> {
        read(dir, name, limit)
    }

    /// Reads at most `limit` bytes; a file others may read is narrowed to
    /// 0600 first (on the open handle, so a swapped path is not touched).
    #[cfg(not(windows))]
    pub(super) fn read(dir: &Path, name: &str, limit: usize) -> io::Result<Vec<u8>> {
        let file = std::fs::File::open(dir.join(name))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if file.metadata()?.permissions().mode() & 0o077 != 0 {
                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }
        }
        read_limited(file, limit)
    }

    /// Reads a secret only when it is owner-only already. A file others may
    /// read, or one another user owns, is refused with `PermissionDenied`:
    /// the key may have leaked, so the caller replaces it instead of
    /// narrowing its mode and trusting it.
    #[cfg(not(windows))]
    pub(super) fn read_secret(dir: &Path, name: &str, limit: usize) -> io::Result<Vec<u8>> {
        read_limited(open_owner_only(&dir.join(name))?, limit)
    }

    #[cfg(unix)]
    fn open_owner_only(path: &Path) -> io::Result<std::fs::File> {
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let metadata = file.metadata()?;
        // SAFETY: geteuid has no preconditions and only reads identity.
        let owner = unsafe { libc::geteuid() };
        if !metadata.is_file() || metadata.uid() != owner || metadata.mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the file is not owner-only",
            ));
        }
        Ok(file)
    }

    #[cfg(not(any(unix, windows)))]
    fn open_owner_only(path: &Path) -> io::Result<std::fs::File> {
        std::fs::File::open(path)
    }

    /// Creates `name` only when it does not exist yet, never half-written:
    /// the bytes go to a temporary file that is 0600 from its creation, which
    /// is then hard-linked into place. The link fails with `AlreadyExists`
    /// when another process created the file first, and leaves it untouched.
    #[cfg(not(windows))]
    pub(super) fn create_new(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
        use std::io::Write as _;
        lean_ctx::core::secure_fs::private_dir(dir)?;
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).map_err(io::Error::other)?;
        let temp = dir.join(format!(
            ".{name}.new.{}.{}",
            std::process::id(),
            hex::encode(nonce)
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let result = options.open(&temp).and_then(|mut file| {
            file.write_all(bytes)?;
            file.sync_all()?;
            std::fs::hard_link(&temp, dir.join(name))
        });
        let _ = std::fs::remove_file(&temp);
        result
    }

    #[cfg(not(windows))]
    fn read_limited(file: std::fs::File, limit: usize) -> io::Result<Vec<u8>> {
        use std::io::Read as _;
        let mut bytes = Vec::new();
        file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "file exceeds the read limit",
            ));
        }
        Ok(bytes)
    }

    #[cfg(not(windows))]
    pub(super) fn write(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
        lean_ctx::core::secure_fs::write_private(dir, name, bytes)
    }
}

fn decode_secret(bytes: &[u8]) -> Option<[u8; 32]> {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| hex::decode(text.trim()).ok())
        .and_then(|raw| <[u8; 32]>::try_from(raw).ok())
}

/// The per-install HMAC key in `dir`, created on first use. `true` when it
/// was created now, so hashes stored before cannot match.
///
/// A missing secret is created exclusively: of two processes starting at
/// once, the second finds the first one's file and uses it instead of
/// replacing it. A secret that is malformed or not owner-only (readable by
/// others, or owned by another user) is replaced, which ends every grant:
/// it may have leaked, so narrowing its mode would not be enough.
fn load_or_create_secret(dir: &FsPath) -> ([u8; 32], bool) {
    let path = dir.join(SECRET_FILE);
    let replace = match private_io::read_secret(dir, SECRET_FILE, 256) {
        Ok(bytes) => {
            if let Some(secret) = decode_secret(&bytes) {
                return (secret, false);
            }
            tracing::warn!(
                "OAuth secret {} is malformed; a new one revokes every grant",
                path.display()
            );
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            tracing::warn!(
                "OAuth secret {} is unreadable or not owner-only ({error}); a new one revokes every grant",
                path.display()
            );
            true
        }
    };
    let secret = random_secret();
    let encoded = hex::encode(secret);
    let written = if replace {
        private_io::write(dir, SECRET_FILE, encoded.as_bytes())
    } else {
        private_io::create_new(dir, SECRET_FILE, encoded.as_bytes())
    };
    match written {
        Ok(()) => {}
        Err(error) if !replace && error.kind() == std::io::ErrorKind::AlreadyExists => {
            // Another process created it first: share its secret.
            if let Some(existing) = private_io::read_secret(dir, SECRET_FILE, 256)
                .ok()
                .and_then(|bytes| decode_secret(&bytes))
            {
                return (existing, true);
            }
            tracing::error!(
                "OAuth secret {} appeared but is not usable; grants last until the next start",
                path.display()
            );
        }
        Err(error) => {
            tracing::error!("OAuth secret not written ({error}); grants last until the next start");
        }
    }
    (secret, true)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

/// RFC 7636 §4.1: 43–128 characters of the unreserved set.
fn valid_pkce_value(value: &str) -> bool {
    (43..=128).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

/// `BASE64URL(SHA256(verifier)) == challenge`, in constant time.
pub(super) fn pkce_s256_matches(verifier: &str, challenge: &str) -> bool {
    if !valid_pkce_value(verifier) {
        return false;
    }
    let computed = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    constant_time_eq(computed.as_bytes(), challenge.as_bytes())
}

fn is_loopback_host(host: &str) -> bool {
    matches!(
        host.trim_start_matches('[').trim_end_matches(']'),
        "127.0.0.1" | "localhost" | "::1"
    )
}

/// Schemes a redirect may never use, whatever a client registers.
const FORBIDDEN_SCHEMES: &[&str] = &[
    "javascript",
    "data",
    "file",
    "vbscript",
    "about",
    "blob",
    "ftp",
    "ws",
    "wss",
];

/// A redirect URI a client may register: `https`, `http` on a loopback
/// host only, or a private-use scheme of a native app (RFC 8252 §7.1). No
/// fragment, no user info.
pub(super) fn validate_redirect_uri(raw: &str) -> Result<reqwest::Url, String> {
    if raw.len() > MAX_URI_LEN {
        return Err("redirect_uri is too long".into());
    }
    let url = reqwest::Url::parse(raw).map_err(|_| "redirect_uri is not an absolute URI")?;
    if url.fragment().is_some() {
        return Err("redirect_uri must not contain a fragment".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("redirect_uri must not contain user info".into());
    }
    match url.scheme() {
        "https" => {
            if url.host_str().is_none_or(str::is_empty) {
                return Err("redirect_uri needs a host".into());
            }
        }
        "http" => {
            if !url.host_str().is_some_and(is_loopback_host) {
                return Err("http redirect_uri must use a loopback host".into());
            }
        }
        scheme if FORBIDDEN_SCHEMES.contains(&scheme) => {
            return Err(format!("redirect_uri scheme {scheme} is not allowed"));
        }
        _ => {}
    }
    Ok(url)
}

/// Exact match, except that a loopback `http` redirect may use any port
/// (RFC 8252 §7.3: a native app picks a free port per authorization).
fn redirect_matches(registered: &str, requested: &str) -> bool {
    if registered == requested {
        return true;
    }
    let (Ok(a), Ok(b)) = (
        reqwest::Url::parse(registered),
        reqwest::Url::parse(requested),
    ) else {
        return false;
    };
    a.scheme() == "http"
        && b.scheme() == "http"
        && a.host_str().is_some_and(is_loopback_host)
        && a.host_str() == b.host_str()
        && a.path() == b.path()
        && a.query() == b.query()
        && b.fragment().is_none()
}

/// The issuer as the canonical origin: `https://host[:port]`, or `http` on a
/// loopback host. A path is refused, since the metadata routes are served at
/// the root of the server.
pub(super) fn canonical_issuer(raw: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(raw.trim())
        .map_err(|_| format!("OAuth issuer {raw:?} is not an absolute URL"))?;
    let loopback = url.host_str().is_some_and(is_loopback_host);
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        _ => {
            return Err(format!(
                "OAuth issuer {raw:?} must use https (http only on a loopback host)"
            ));
        }
    }
    if url.host_str().is_none_or(str::is_empty)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(format!(
            "OAuth issuer {raw:?} must be an origin only (scheme, host, optional port)"
        ));
    }
    Ok(url.origin().ascii_serialization())
}

fn lock(m: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl OAuthState {
    /// The fingerprint of an explicitly configured instance token: an
    /// HMAC under the install secret, so the store never holds the token
    /// and a copied store cannot be tested against guesses offline.
    #[cfg(test)]
    pub(super) fn owner_fingerprint(&self, token: &str) -> String {
        keyed_owner_fingerprint(&self.secret, token)
    }

    /// The stored form of a token, code or refresh token.
    fn hash_token(&self, token: &str) -> String {
        hex::encode(hmac_sha256(
            &self.secret,
            &[b"lean-ctx oauth token v2\0", token.as_bytes()],
        ))
    }

    /// Open the authorization server of `issuer`. `store` is the owner-only
    /// JSON file that keeps clients and token hashes across restarts; the
    /// install secret lives beside it. `None` keeps both in memory.
    ///
    /// `explicit_owner` is set when the instance token was configured
    /// explicitly: if the store was approved under another token, every
    /// client and grant is dropped, so rotating a leaked token also ends
    /// what was approved with it. The token auto-generated per start on
    /// loopback passes `false` and keeps the grants across restarts.
    pub(super) fn open(
        issuer: String,
        owner_token: String,
        store: Option<PathBuf>,
        explicit_owner: bool,
    ) -> Self {
        let (secret, fresh_secret) = match store.as_deref().and_then(FsPath::parent) {
            Some(dir) => load_or_create_secret(dir),
            None => (random_secret(), true),
        };
        let mut changed = false;
        let mut persisted = store
            .as_deref()
            .and_then(|path| {
                let (dir, name) = (path.parent()?, path.file_name()?.to_str()?);
                match private_io::read(dir, name, MAX_STORE_BYTES) {
                    Ok(bytes) => match serde_json::from_slice::<Persisted>(&bytes) {
                        Ok(p) if p.version == STORE_VERSION || p.version == LEGACY_STORE_VERSION => {
                            Some(p)
                        }
                        Ok(_) | Err(_) => {
                            tracing::warn!(
                                "OAuth store {} is unreadable; starting without registered clients",
                                path.display()
                            );
                            None
                        }
                    },
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Err(error) => {
                        tracing::warn!(
                            "OAuth store {} is unreadable or not owner-only ({error}); starting without registered clients",
                            path.display()
                        );
                        None
                    }
                }
            })
            .unwrap_or_default();
        if persisted.version == LEGACY_STORE_VERSION {
            // Plain SHA-256 hashes cannot be re-keyed: clients stay (when
            // the explicit token is unchanged), their tokens go once.
            let same_owner = !explicit_owner
                || persisted.owner_fingerprint.as_deref()
                    == Some(legacy_owner_fingerprint(&owner_token).as_str());
            if !persisted.tokens.is_empty() {
                tracing::warn!(
                    "OAuth store upgraded: every client signs in once more ({} grant token(s) dropped)",
                    persisted.tokens.len()
                );
            }
            persisted.tokens.clear();
            if !same_owner {
                persisted.clients.clear();
            }
            persisted.owner_fingerprint = (explicit_owner && same_owner)
                .then(|| keyed_owner_fingerprint(&secret, &owner_token));
            changed = true;
        } else if fresh_secret && !persisted.tokens.is_empty() {
            tracing::warn!(
                "the OAuth secret is new: {} stored grant token(s) can no longer be checked and were dropped",
                persisted.tokens.len()
            );
            persisted.tokens.clear();
            changed = true;
        }
        persisted.version = STORE_VERSION;
        let now = unix_now();
        persisted.tokens.retain(|t| t.expires_at > now);
        if explicit_owner {
            let fingerprint = keyed_owner_fingerprint(&secret, &owner_token);
            if persisted.owner_fingerprint.as_deref() != Some(fingerprint.as_str()) {
                if !persisted.clients.is_empty() || !persisted.tokens.is_empty() {
                    tracing::warn!(
                        "the instance token changed: {} OAuth client(s) and their grants were revoked",
                        persisted.clients.len()
                    );
                }
                persisted.clients.clear();
                persisted.tokens.clear();
                persisted.owner_fingerprint = Some(fingerprint);
                changed = true;
            }
        }
        let state = Self {
            issuer,
            owner_token,
            store,
            secret,
            inner: Mutex::new(Inner {
                persisted,
                pending: HashMap::new(),
                codes: HashMap::new(),
                owner_failures: Vec::new(),
                client_failures: HashMap::new(),
            }),
        };
        if changed {
            state.persist(&lock(&state.inner));
        }
        state
    }

    /// The store file of `issuer` under `dir`: one file per issuer, so two
    /// instances on one machine never share clients.
    pub(super) fn store_path(dir: &std::path::Path, issuer: &str) -> PathBuf {
        let digest = hex::encode(Sha256::digest(issuer.as_bytes()));
        dir.join(format!("oauth-{}.json", &digest[..16]))
    }

    pub(super) fn issuer(&self) -> &str {
        &self.issuer
    }

    pub(super) fn resource_metadata_url(&self) -> String {
        format!("{}/.well-known/oauth-protected-resource", self.issuer)
    }

    /// The `WWW-Authenticate` value of a 401 (MCP authorization, RFC 9728 §5.1).
    pub(super) fn challenge(&self, error: Option<&str>) -> String {
        let mut value = format!(
            "Bearer resource_metadata=\"{}\", scope=\"{SCOPE}\"",
            self.resource_metadata_url()
        );
        if let Some(error) = error {
            value.push_str(&format!(", error=\"{error}\""));
        }
        value
    }

    fn persist(&self, inner: &Inner) {
        let Some(path) = self.store.as_deref() else {
            return;
        };
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
            return;
        };
        let bytes = match serde_json::to_vec(&inner.persisted) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::error!("OAuth store not serialised: {error}");
                return;
            }
        };
        if let Err(error) = private_io::write(dir, &name.to_string_lossy(), &bytes) {
            tracing::error!("OAuth store not written: {error}");
        }
    }

    fn prune(inner: &mut Inner, now: u64) {
        inner.persisted.tokens.retain(|t| t.expires_at > now);
        inner.pending.retain(|_, p| p.expires_at > now);
        inner.codes.retain(|_, c| c.expires_at > now);
        inner
            .owner_failures
            .retain(|at| at.saturating_add(OWNER_FAILURE_WINDOW_SECS) > now);
        inner.client_failures.retain(|_, f| {
            f.locked_until > now || f.last_at.saturating_add(CLIENT_FAILURE_WINDOW_SECS) > now
        });
    }

    /// Seconds until `client_id` may try again, if it is locked out.
    fn client_locked(inner: &Inner, client_id: &str, now: u64) -> Option<u64> {
        inner
            .client_failures
            .get(client_id)
            .filter(|f| f.locked_until > now)
            .map(|f| f.locked_until - now)
    }

    /// Count a failed attempt of a registered client. From the
    /// [`CLIENT_FAILURE_LIMIT`]th failure in a window on, the client is
    /// locked out, twice as long for each further failure. Unknown client
    /// ids are not tracked, so the table stays bounded by the clients.
    fn record_client_failure(inner: &mut Inner, client_id: &str, now: u64) {
        if !inner
            .persisted
            .clients
            .iter()
            .any(|c| c.client_id == client_id)
        {
            return;
        }
        if !inner.client_failures.contains_key(client_id)
            && inner.client_failures.len() >= MAX_FAILURE_ENTRIES
            && let Some(oldest) = inner
                .client_failures
                .iter()
                .min_by_key(|(_, f)| f.last_at)
                .map(|(id, _)| id.clone())
        {
            inner.client_failures.remove(&oldest);
        }
        let entry = inner
            .client_failures
            .entry(client_id.to_string())
            .or_default();
        if entry.last_at.saturating_add(CLIENT_FAILURE_WINDOW_SECS) <= now {
            entry.count = 0;
        }
        entry.count = entry.count.saturating_add(1);
        entry.last_at = now;
        if entry.count >= CLIENT_FAILURE_LIMIT {
            let doublings = (entry.count - CLIENT_FAILURE_LIMIT).min(16);
            let backoff = CLIENT_BACKOFF_BASE_SECS
                .saturating_mul(1_u64 << doublings)
                .min(CLIENT_BACKOFF_MAX_SECS);
            entry.locked_until = now.saturating_add(backoff);
        }
    }

    /// The refusal of an invalid credential. While the client is locked out
    /// it is answered with 429 and not counted; otherwise it counts towards
    /// the lockout. A valid credential never reaches this, so the lockout
    /// cannot keep a legitimate client out.
    fn refuse_grant(
        inner: &mut Inner,
        client_id: &str,
        locked: Option<u64>,
        now: u64,
        message: &'static str,
    ) -> OAuthError {
        if let Some(wait) = locked {
            return OAuthError::locked(wait);
        }
        Self::record_client_failure(inner, client_id, now);
        OAuthError::grant(message)
    }

    /// The id of a live access token's client, else `None`.
    pub(super) fn verify_access(&self, token: &str, now: u64) -> Option<String> {
        if token.is_empty() {
            return None;
        }
        let hash = self.hash_token(token);
        let inner = lock(&self.inner);
        inner
            .persisted
            .tokens
            .iter()
            .find(|t| {
                t.kind == TokenKind::Access
                    && t.expires_at > now
                    && constant_time_eq(t.hash.as_bytes(), hash.as_bytes())
            })
            .map(|t| t.client_id.clone())
    }

    /// RFC 7591 registration of a public client.
    pub(super) fn register(
        &self,
        redirect_uris: &[String],
        client_name: Option<&str>,
        now: u64,
    ) -> Result<(String, String, Vec<String>), OAuthError> {
        let invalid =
            |msg: String| OAuthError::new(StatusCode::BAD_REQUEST, "invalid_redirect_uri", msg);
        if redirect_uris.is_empty() {
            return Err(invalid("at least one redirect_uri is required".into()));
        }
        if redirect_uris.len() > MAX_REDIRECT_URIS {
            return Err(invalid(format!(
                "at most {MAX_REDIRECT_URIS} redirect_uris are accepted"
            )));
        }
        for uri in redirect_uris {
            validate_redirect_uri(uri).map_err(invalid)?;
        }
        let name: String = client_name
            .unwrap_or("")
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_CLIENT_NAME)
            .collect::<String>()
            .trim()
            .to_string();
        let name = if name.is_empty() {
            "Unnamed client".to_string()
        } else {
            name
        };
        let mut inner = lock(&self.inner);
        Self::prune(&mut inner, now);
        if inner.persisted.clients.len() >= MAX_CLIENTS {
            // Make room by dropping the oldest client that holds no live token.
            let tokens = &inner.persisted.tokens;
            let idle = inner
                .persisted
                .clients
                .iter()
                .enumerate()
                .filter(|(_, c)| !tokens.iter().any(|t| t.client_id == c.client_id))
                .min_by_key(|(_, c)| c.created_at)
                .map(|(i, _)| i);
            match idle {
                Some(i) => {
                    inner.persisted.clients.remove(i);
                }
                None => {
                    return Err(OAuthError::new(
                        StatusCode::TOO_MANY_REQUESTS,
                        "temporarily_unavailable",
                        "too many registered clients; revoke one first",
                    ));
                }
            }
        }
        let client = Client {
            client_id: random_token("lcx_client_"),
            client_name: name,
            redirect_uris: redirect_uris.to_vec(),
            created_at: now,
            approved: false,
        };
        let out = (
            client.client_id.clone(),
            client.client_name.clone(),
            client.redirect_uris.clone(),
        );
        inner.persisted.clients.push(client);
        self.persist(&inner);
        Ok(out)
    }

    fn client(inner: &Inner, client_id: &str) -> Option<Client> {
        inner
            .persisted
            .clients
            .iter()
            .find(|c| c.client_id == client_id)
            .cloned()
    }

    fn issue_family(
        &self,
        inner: &mut Inner,
        client_id: &str,
        family: &str,
        now: u64,
    ) -> IssuedTokens {
        let access_token = random_token("lcx_at_");
        let refresh_token = random_token("lcx_rt_");
        if inner.persisted.tokens.len() + 2 > MAX_TOKENS {
            // Oldest-expiring first: bounded store, never unbounded growth.
            inner.persisted.tokens.sort_by_key(|t| t.expires_at);
            let excess = inner.persisted.tokens.len() + 2 - MAX_TOKENS;
            inner.persisted.tokens.drain(..excess);
        }
        for (token, kind, ttl) in [
            (&access_token, TokenKind::Access, ACCESS_TTL_SECS),
            (&refresh_token, TokenKind::Refresh, REFRESH_TTL_SECS),
        ] {
            inner.persisted.tokens.push(TokenRecord {
                hash: self.hash_token(token),
                kind,
                family: family.to_string(),
                client_id: client_id.to_string(),
                expires_at: now + ttl,
                used: false,
            });
        }
        IssuedTokens {
            access_token,
            refresh_token,
        }
    }

    fn revoke_family(inner: &mut Inner, family: &str) {
        inner.persisted.tokens.retain(|t| t.family != family);
    }

    /// Exchange an authorization code (RFC 6749 §4.1.3 + RFC 7636 §4.6).
    pub(super) fn exchange_code(
        &self,
        code: &str,
        client_id: &str,
        redirect_uri: Option<&str>,
        code_verifier: &str,
        now: u64,
    ) -> Result<IssuedTokens, OAuthError> {
        let mut inner = lock(&self.inner);
        Self::prune(&mut inner, now);
        if Self::client(&inner, client_id).is_none() {
            return Err(OAuthError::new(
                StatusCode::UNAUTHORIZED,
                "invalid_client",
                "unknown client_id",
            ));
        }
        // A locked client still redeems a valid code with its verifier; only
        // invalid attempts are refused with 429 (`refuse_grant`).
        let locked = Self::client_locked(&inner, client_id, now);
        let presented = self.hash_token(code);
        let Some(hash) = inner
            .codes
            .keys()
            .find(|stored| constant_time_eq(stored.as_bytes(), presented.as_bytes()))
            .cloned()
        else {
            return Err(Self::refuse_grant(
                &mut inner,
                client_id,
                locked,
                now,
                "authorization code is invalid or expired",
            ));
        };
        let record = &inner.codes[&hash];
        if let Some(family) = record.redeemed_family.clone() {
            // A replayed code: whoever holds it is not the client that
            // redeemed it first, so every token of that grant goes.
            inner.codes.remove(&hash);
            Self::revoke_family(&mut inner, &family);
            self.persist(&inner);
            return Err(Self::refuse_grant(
                &mut inner,
                client_id,
                locked,
                now,
                "authorization code was already used",
            ));
        }
        let mismatch = record.client_id != client_id
            || match redirect_uri {
                Some(uri) => uri != record.redirect_uri,
                None => record.redirect_uri_given,
            }
            || !pkce_s256_matches(code_verifier, &record.code_challenge);
        if mismatch {
            // One attempt per code: a failed exchange burns it.
            inner.codes.remove(&hash);
            return Err(Self::refuse_grant(
                &mut inner,
                client_id,
                locked,
                now,
                "client_id, redirect_uri or code_verifier does not match the authorization",
            ));
        }
        let family = random_token("fam_");
        let tokens = self.issue_family(&mut inner, client_id, &family, now);
        if let Some(record) = inner.codes.get_mut(&hash) {
            record.redeemed_family = Some(family);
        }
        inner.client_failures.remove(client_id);
        self.persist(&inner);
        Ok(tokens)
    }

    /// Rotate a refresh token (OAuth 2.1 §4.3.1: public clients rotate).
    pub(super) fn refresh(
        &self,
        refresh_token: &str,
        client_id: &str,
        now: u64,
    ) -> Result<IssuedTokens, OAuthError> {
        let mut inner = lock(&self.inner);
        Self::prune(&mut inner, now);
        // A locked client still rotates a valid refresh token; only invalid
        // attempts are refused with 429 (`refuse_grant`).
        let locked = Self::client_locked(&inner, client_id, now);
        let hash = self.hash_token(refresh_token);
        let Some(index) = inner.persisted.tokens.iter().position(|t| {
            t.kind == TokenKind::Refresh && constant_time_eq(t.hash.as_bytes(), hash.as_bytes())
        }) else {
            return Err(Self::refuse_grant(
                &mut inner,
                client_id,
                locked,
                now,
                "refresh token is invalid or expired",
            ));
        };
        let record = inner.persisted.tokens[index].clone();
        if record.client_id != client_id {
            return Err(Self::refuse_grant(
                &mut inner,
                client_id,
                locked,
                now,
                "refresh token was issued to another client",
            ));
        }
        if record.used {
            Self::revoke_family(&mut inner, &record.family);
            self.persist(&inner);
            return Err(Self::refuse_grant(
                &mut inner,
                client_id,
                locked,
                now,
                "refresh token was already used",
            ));
        }
        inner.persisted.tokens[index].used = true;
        let tokens = self.issue_family(&mut inner, client_id, &record.family, now);
        inner.client_failures.remove(client_id);
        self.persist(&inner);
        Ok(tokens)
    }

    /// RFC 7009: revoke the grant a token belongs to. Unknown tokens are
    /// not an error.
    pub(super) fn revoke(&self, token: &str, client_id: Option<&str>) {
        let hash = self.hash_token(token);
        let mut inner = lock(&self.inner);
        let family = inner
            .persisted
            .tokens
            .iter()
            .find(|t| {
                constant_time_eq(t.hash.as_bytes(), hash.as_bytes())
                    && client_id.is_none_or(|id| id == t.client_id)
            })
            .map(|t| t.family.clone());
        if let Some(family) = family {
            Self::revoke_family(&mut inner, &family);
            self.persist(&inner);
        }
    }

    /// The owner's view of registered clients.
    fn list_clients(&self, now: u64) -> Vec<serde_json::Value> {
        let mut inner = lock(&self.inner);
        Self::prune(&mut inner, now);
        inner
            .persisted
            .clients
            .iter()
            .map(|c| {
                let active = inner
                    .persisted
                    .tokens
                    .iter()
                    .filter(|t| t.client_id == c.client_id && t.kind == TokenKind::Refresh)
                    .filter(|t| !t.used)
                    .count();
                serde_json::json!({
                    "client_id": c.client_id,
                    "client_name": c.client_name,
                    "redirect_uris": c.redirect_uris,
                    "created_at": c.created_at,
                    "approved": c.approved,
                    "active_grants": active,
                })
            })
            .collect()
    }

    /// Remove a client and every token it holds.
    fn remove_client(&self, client_id: &str) -> bool {
        let mut inner = lock(&self.inner);
        let before = inner.persisted.clients.len();
        inner.persisted.clients.retain(|c| c.client_id != client_id);
        let removed = inner.persisted.clients.len() != before;
        inner.persisted.tokens.retain(|t| t.client_id != client_id);
        inner.pending.retain(|_, p| p.client_id != client_id);
        inner.codes.retain(|_, c| c.client_id != client_id);
        inner.client_failures.remove(client_id);
        if removed {
            self.persist(&inner);
        }
        removed
    }
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

/// Paths served without a bearer token while OAuth is on.
pub(super) fn is_public_path(path: &str) -> bool {
    path == "/.well-known/oauth-authorization-server"
        || path == "/.well-known/oauth-protected-resource"
        || path.starts_with("/.well-known/oauth-protected-resource/")
        || matches!(
            path,
            "/oauth/register" | "/oauth/authorize" | "/oauth/token" | "/oauth/revoke"
        )
}

/// Read-only routes an OAuth token may call besides the MCP endpoint.
const OAUTH_READ_ONLY: &[&str] = &[
    "/v1/health",
    "/v1/manifest",
    "/v1/capabilities",
    "/v1/openapi.json",
    "/v1/tools",
    "/v1/a2a/agent-card",
    "/.well-known/agent.json",
    "/.well-known/mcp-server.json",
];

/// The allowlist of an OAuth token (scope `mcp`): the Streamable HTTP MCP
/// endpoint (`/` or `/mcp`) and the read-only discovery routes above, with
/// GET only. Every other route, including any added later, needs the
/// instance token.
pub(super) fn oauth_may_call(method: &Method, path: &str) -> bool {
    if path == "/" || path == "/mcp" {
        return true;
    }
    (method == Method::GET || method == Method::HEAD) && OAUTH_READ_ONLY.contains(&path)
}

pub(super) fn router<S>(state: Arc<OAuthState>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get(protected_resource_root).options(preflight),
        )
        .route(
            "/.well-known/oauth-protected-resource/{*resource}",
            get(protected_resource_path).options(preflight),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(authorization_server).options(preflight),
        )
        .route("/oauth/register", post(register).options(preflight))
        .route(
            "/oauth/authorize",
            get(authorize_page).post(authorize_decide),
        )
        .route("/oauth/token", post(token).options(preflight))
        .route("/oauth/revoke", post(revoke).options(preflight))
        .route("/v1/oauth/clients", get(admin_list_clients))
        .route(
            "/v1/oauth/clients/{client_id}/revoke",
            post(admin_revoke_client),
        )
        .with_state(state)
}

fn no_store(resp: &mut Response) {
    let headers = resp.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
}

/// Metadata, registration, token and revocation carry no cookie and no
/// ambient authority, so a browser-based MCP client may call them.
fn cors(mut resp: Response) -> Response {
    resp.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    resp
}

async fn preflight() -> Response {
    let mut resp = StatusCode::NO_CONTENT.into_response();
    let headers = resp.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("content-type, mcp-protocol-version"),
    );
    headers.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static("600"),
    );
    cors(resp)
}

fn protected_resource_document(state: &OAuthState, resource: &str) -> Response {
    cors(
        Json(serde_json::json!({
            "resource": resource,
            "authorization_servers": [state.issuer()],
            "scopes_supported": [SCOPE],
            "bearer_methods_supported": ["header"],
            "resource_name": "LeanCTX",
        }))
        .into_response(),
    )
}

async fn protected_resource_root(State(state): State<Arc<OAuthState>>) -> Response {
    protected_resource_document(&state, state.issuer())
}

/// RFC 9728 §3.1: the metadata of `https://host/mcp` lives at
/// `/.well-known/oauth-protected-resource/mcp` and names that resource.
async fn protected_resource_path(
    State(state): State<Arc<OAuthState>>,
    Path(resource): Path<String>,
) -> Response {
    let resource = resource.trim_matches('/');
    let named = if resource.is_empty() {
        state.issuer().to_string()
    } else {
        format!("{}/{resource}", state.issuer())
    };
    protected_resource_document(&state, &named)
}

pub(super) fn authorization_server_document(issuer: &str) -> serde_json::Value {
    serde_json::json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/oauth/authorize"),
        "token_endpoint": format!("{issuer}/oauth/token"),
        "registration_endpoint": format!("{issuer}/oauth/register"),
        "revocation_endpoint": format!("{issuer}/oauth/revoke"),
        "response_types_supported": ["code"],
        "response_modes_supported": ["query"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "revocation_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": [SCOPE],
        "authorization_response_iss_parameter_supported": true,
    })
}

async fn authorization_server(State(state): State<Arc<OAuthState>>) -> Response {
    cors(Json(authorization_server_document(state.issuer())).into_response())
}

#[derive(Deserialize)]
struct RegisterBody {
    #[serde(default)]
    redirect_uris: Vec<String>,
    #[serde(default)]
    client_name: Option<String>,
}

async fn register(
    State(state): State<Arc<OAuthState>>,
    body: Result<Json<RegisterBody>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(body)) = body else {
        return cors(
            OAuthError::new(
                StatusCode::BAD_REQUEST,
                "invalid_client_metadata",
                "the body must be a JSON client metadata document",
            )
            .into_response(),
        );
    };
    let now = unix_now();
    match state.register(&body.redirect_uris, body.client_name.as_deref(), now) {
        Ok((client_id, client_name, redirect_uris)) => {
            let mut resp = (
                StatusCode::CREATED,
                Json(serde_json::json!({
                    "client_id": client_id,
                    "client_id_issued_at": now,
                    "client_name": client_name,
                    "redirect_uris": redirect_uris,
                    "token_endpoint_auth_method": "none",
                    "grant_types": ["authorization_code", "refresh_token"],
                    "response_types": ["code"],
                    "scope": SCOPE,
                })),
            )
                .into_response();
            no_store(&mut resp);
            cors(resp)
        }
        Err(error) => cors(error.into_response()),
    }
}

#[derive(Deserialize, Default)]
struct AuthorizeQuery {
    response_type: Option<String>,
    client_id: Option<String>,
    redirect_uri: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    state: Option<String>,
    resource: Option<String>,
}

fn html_escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// The `form-action` source that lets the browser follow the redirect after
/// the owner's decision (Chromium applies `form-action` to it).
fn form_action_source(redirect_uri: &str) -> String {
    match reqwest::Url::parse(redirect_uri) {
        Ok(url) if matches!(url.scheme(), "http" | "https") => url.origin().ascii_serialization(),
        Ok(url) => format!("{}:", url.scheme()),
        Err(_) => String::new(),
    }
}

fn html_page(status: StatusCode, body: &str, form_action: Option<&str>) -> Response {
    let page = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<title>Authorize client - LeanCTX</title><style>\
:root{{color-scheme:light dark;--bg:#fafafa;--fg:#1a1a1a;--muted:#5c5c5c;--card:#fff;--line:#d9d9d9;--accent:#1f5fbf}}\
@media (prefers-color-scheme:dark){{:root{{--bg:#141414;--fg:#ececec;--muted:#a3a3a3;--card:#1e1e1e;--line:#3a3a3a;--accent:#7aa7ec}}}}\
body{{margin:0;background:var(--bg);color:var(--fg);font:15px/1.5 system-ui,sans-serif}}\
main{{max-width:30rem;margin:3rem auto;padding:0 1rem}}\
section{{background:var(--card);border:1px solid var(--line);border-radius:8px;padding:1.5rem}}\
h1{{font-size:1.25rem;margin:0 0 1rem}}p{{margin:.5rem 0}}.muted{{color:var(--muted)}}\
code{{word-break:break-all}}label{{display:block;margin-top:1rem;font-weight:600}}\
input[type=password]{{width:100%;box-sizing:border-box;padding:.5rem;margin-top:.25rem;border:1px solid var(--line);border-radius:6px;background:var(--bg);color:var(--fg)}}\
.row{{display:flex;gap:.5rem;margin-top:1.25rem}}button{{flex:1;padding:.6rem;border-radius:6px;border:1px solid var(--line);background:var(--card);color:var(--fg);font:inherit;cursor:pointer}}\
button.primary{{background:var(--accent);border-color:var(--accent);color:#fff}}.error{{color:#c0392b}}\
</style></head><body><main><section>{body}</section></main></body></html>"
    );
    let mut resp = (status, page).into_response();
    let csp = format!(
        "default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'{}",
        form_action
            .filter(|s| !s.is_empty())
            .map(|s| format!(" {s}"))
            .unwrap_or_default()
    );
    let headers = resp.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    if let Ok(value) = HeaderValue::from_str(&csp) {
        headers.insert(header::CONTENT_SECURITY_POLICY, value);
    }
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    no_store(&mut resp);
    resp
}

fn error_page(status: StatusCode, message: &str) -> Response {
    html_page(
        status,
        &format!(
            "<h1>Authorization failed</h1><p class=\"error\">{}</p>\
<p class=\"muted\">Start the connection again from your client.</p>",
            html_escape(message)
        ),
        None,
    )
}

/// The redirect back to the client (RFC 6749 §4.1.2, RFC 9207 `iss`).
fn redirect_to_client(redirect_uri: &str, params: &[(&str, &str)], issuer: &str) -> Response {
    let Ok(mut url) = reqwest::Url::parse(redirect_uri) else {
        return error_page(StatusCode::BAD_REQUEST, "the redirect_uri is invalid");
    };
    {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in params {
            pairs.append_pair(key, value);
        }
        pairs.append_pair("iss", issuer);
    }
    let mut resp = StatusCode::SEE_OTHER.into_response();
    if let Ok(location) = HeaderValue::from_str(url.as_str()) {
        resp.headers_mut().insert(header::LOCATION, location);
    }
    no_store(&mut resp);
    resp
}

fn redirect_error(
    redirect_uri: &str,
    error: &str,
    description: &str,
    state: Option<&str>,
    issuer: &str,
) -> Response {
    let mut params = vec![("error", error), ("error_description", description)];
    if let Some(state) = state {
        params.push(("state", state));
    }
    redirect_to_client(redirect_uri, &params, issuer)
}

/// An authorization error: redirected to a client the owner approved
/// before, shown on our own page for one nobody approved yet.
fn refuse(
    client: &Client,
    redirect_uri: &str,
    error: &str,
    description: &str,
    state: Option<&str>,
    issuer: &str,
) -> Response {
    if client.approved {
        redirect_error(redirect_uri, error, description, state, issuer)
    } else {
        error_page(StatusCode::BAD_REQUEST, &format!("{description} ({error})"))
    }
}

fn consent_page(
    state: &OAuthState,
    request_id: &str,
    client: &Client,
    redirect_uri: &str,
    error: Option<&str>,
) -> Response {
    let redirect_host = reqwest::Url::parse(redirect_uri)
        .ok()
        .map(|u| match u.host_str() {
            Some(host) => host.to_string(),
            None => format!("{}:", u.scheme()),
        })
        .unwrap_or_default();
    let error_html = error
        .map(|e| format!("<p class=\"error\" role=\"alert\">{}</p>", html_escape(e)))
        .unwrap_or_default();
    let body = format!(
        "<h1>Allow {name} to use this LeanCTX instance?</h1>\
<p>The client will be able to call the MCP tools of <code>{issuer}</code> until you revoke it. \
Its access renews itself while the client keeps using it; a grant left unused for 30 days expires.</p>\
<p class=\"muted\">After you decide, the browser returns to <code>{host}</code> \
(<code>{uri}</code>). Only continue if you started this connection yourself.</p>\
{error_html}\
<form method=\"post\" action=\"/oauth/authorize\" autocomplete=\"off\">\
<input type=\"hidden\" name=\"request_id\" value=\"{request_id}\">\
<label for=\"owner_token\">Instance token</label>\
<input id=\"owner_token\" name=\"owner_token\" type=\"password\" autocomplete=\"current-password\">\
<p class=\"muted\">The bearer token this instance was started with. It is checked here and never sent to the client.</p>\
<div class=\"row\"><button type=\"submit\" name=\"decision\" value=\"deny\">Deny</button>\
<button class=\"primary\" type=\"submit\" name=\"decision\" value=\"allow\">Allow</button></div>\
</form>",
        name = html_escape(&client.client_name),
        issuer = html_escape(state.issuer()),
        host = html_escape(&redirect_host),
        uri = html_escape(redirect_uri),
        request_id = html_escape(request_id),
    );
    let status = if error.is_some() {
        StatusCode::UNAUTHORIZED
    } else {
        StatusCode::OK
    };
    html_page(status, &body, Some(&form_action_source(redirect_uri)))
}

fn same_origin(issuer: &str, resource: &str) -> bool {
    match (reqwest::Url::parse(issuer), reqwest::Url::parse(resource)) {
        (Ok(a), Ok(b)) => b.fragment().is_none() && a.origin() == b.origin(),
        _ => false,
    }
}

async fn authorize_page(
    State(state): State<Arc<OAuthState>>,
    query: Result<Query<AuthorizeQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let q = query.map(|Query(q)| q).unwrap_or_default();
    let now = unix_now();
    let Some(client_id) = q.client_id.as_deref().filter(|s| !s.is_empty()) else {
        return error_page(StatusCode::BAD_REQUEST, "client_id is missing");
    };
    let client = {
        let mut inner = lock(&state.inner);
        OAuthState::prune(&mut inner, now);
        OAuthState::client(&inner, client_id)
    };
    let Some(client) = client else {
        return error_page(StatusCode::BAD_REQUEST, "this client is not registered");
    };
    // Until the redirect URI is known to be the client's, every error is
    // shown here and never redirected (RFC 6749 §4.1.2.1).
    let (redirect_uri, redirect_uri_given) = match q.redirect_uri.as_deref() {
        Some(uri) => match client
            .redirect_uris
            .iter()
            .find(|registered| redirect_matches(registered, uri))
        {
            Some(_) => (uri.to_string(), true),
            None => {
                return error_page(
                    StatusCode::BAD_REQUEST,
                    "redirect_uri is not registered for this client",
                );
            }
        },
        None if client.redirect_uris.len() == 1 => (client.redirect_uris[0].clone(), false),
        None => return error_page(StatusCode::BAD_REQUEST, "redirect_uri is missing"),
    };
    let st = q.state.as_deref();
    let issuer = state.issuer();
    if q.response_type.as_deref() != Some("code") {
        return refuse(
            &client,
            &redirect_uri,
            "unsupported_response_type",
            "only response_type=code is supported",
            st,
            issuer,
        );
    }
    let Some(challenge) = q.code_challenge.as_deref().filter(|c| valid_pkce_value(c)) else {
        return refuse(
            &client,
            &redirect_uri,
            "invalid_request",
            "a PKCE code_challenge is required",
            st,
            issuer,
        );
    };
    if q.code_challenge_method.as_deref() != Some("S256") {
        return refuse(
            &client,
            &redirect_uri,
            "invalid_request",
            "code_challenge_method must be S256",
            st,
            issuer,
        );
    }
    if let Some(resource) = q.resource.as_deref()
        && !same_origin(issuer, resource)
    {
        return refuse(
            &client,
            &redirect_uri,
            "invalid_target",
            "the resource is not served by this authorization server",
            st,
            issuer,
        );
    }
    let request_id = random_token("req_");
    {
        let mut inner = lock(&state.inner);
        if inner.pending.len() >= MAX_PENDING {
            return error_page(
                StatusCode::TOO_MANY_REQUESTS,
                "too many authorizations are waiting; try again in a few minutes",
            );
        }
        inner.pending.insert(
            request_id.clone(),
            Pending {
                client_id: client.client_id.clone(),
                redirect_uri: redirect_uri.clone(),
                redirect_uri_given,
                code_challenge: challenge.to_string(),
                state: q.state.clone(),
                expires_at: now + PENDING_TTL_SECS,
            },
        );
    }
    consent_page(&state, &request_id, &client, &redirect_uri, None)
}

#[derive(Deserialize)]
struct DecisionForm {
    request_id: String,
    #[serde(default)]
    owner_token: String,
    #[serde(default)]
    decision: String,
}

async fn authorize_decide(
    State(state): State<Arc<OAuthState>>,
    form: Result<Form<DecisionForm>, axum::extract::rejection::FormRejection>,
) -> Response {
    let Ok(Form(form)) = form else {
        return error_page(StatusCode::BAD_REQUEST, "the form is incomplete");
    };
    let now = unix_now();
    let issuer = state.issuer().to_string();
    let mut inner = lock(&state.inner);
    OAuthState::prune(&mut inner, now);
    // Single use: the request leaves the table whatever the outcome.
    let Some(pending) = inner.pending.remove(&form.request_id) else {
        drop(inner);
        return error_page(
            StatusCode::BAD_REQUEST,
            "this authorization request expired or was already answered",
        );
    };
    let Some(client) = OAuthState::client(&inner, &pending.client_id) else {
        drop(inner);
        return error_page(
            StatusCode::BAD_REQUEST,
            "this client is no longer registered",
        );
    };
    if form.decision != "allow" {
        drop(inner);
        return refuse(
            &client,
            &pending.redirect_uri,
            "access_denied",
            "the owner denied the request",
            pending.state.as_deref(),
            &issuer,
        );
    }
    if inner.owner_failures.len() >= OWNER_FAILURE_LIMIT {
        drop(inner);
        return error_page(
            StatusCode::TOO_MANY_REQUESTS,
            "too many wrong instance tokens; wait ten minutes and start again",
        );
    }
    // The right instance token is accepted even while this client is locked
    // out, so wrong guesses cannot lock the owner out of approving it.
    if !constant_time_eq(form.owner_token.as_bytes(), state.owner_token.as_bytes()) {
        // Every wrong token counts towards the instance-wide limit, which
        // bounds guessing also while the client is locked out.
        inner.owner_failures.push(now);
        if let Some(wait) = OAuthState::client_locked(&inner, &client.client_id, now) {
            drop(inner);
            return error_page(
                StatusCode::TOO_MANY_REQUESTS,
                &format!(
                    "too many wrong instance tokens for this client; wait {wait} seconds and start again"
                ),
            );
        }
        OAuthState::record_client_failure(&mut inner, &client.client_id, now);
        let redirect_uri = pending.redirect_uri.clone();
        inner.pending.insert(form.request_id.clone(), pending);
        drop(inner);
        return consent_page(
            &state,
            &form.request_id,
            &client,
            &redirect_uri,
            Some("That is not the token of this instance."),
        );
    }
    inner.client_failures.remove(&client.client_id);
    let code = random_token("lcx_code_");
    if inner.codes.len() >= MAX_CODES {
        drop(inner);
        return error_page(
            StatusCode::TOO_MANY_REQUESTS,
            "too many authorizations are waiting; try again in a minute",
        );
    }
    inner.codes.insert(
        state.hash_token(&code),
        CodeRecord {
            client_id: pending.client_id.clone(),
            redirect_uri: pending.redirect_uri.clone(),
            redirect_uri_given: pending.redirect_uri_given,
            code_challenge: pending.code_challenge.clone(),
            expires_at: now + CODE_TTL_SECS,
            redeemed_family: None,
        },
    );
    if let Some(approved) = inner
        .persisted
        .clients
        .iter_mut()
        .find(|c| c.client_id == pending.client_id)
        && !approved.approved
    {
        approved.approved = true;
        state.persist(&inner);
    }
    drop(inner);
    let mut params = vec![("code", code.as_str())];
    if let Some(st) = pending.state.as_deref() {
        params.push(("state", st));
    }
    redirect_to_client(&pending.redirect_uri, &params, &issuer)
}

#[derive(Deserialize)]
struct TokenForm {
    grant_type: Option<String>,
    code: Option<String>,
    redirect_uri: Option<String>,
    client_id: Option<String>,
    code_verifier: Option<String>,
    refresh_token: Option<String>,
}

fn token_response(tokens: &IssuedTokens) -> Response {
    let mut resp = Json(serde_json::json!({
        "access_token": tokens.access_token,
        "token_type": "Bearer",
        "expires_in": ACCESS_TTL_SECS,
        "refresh_token": tokens.refresh_token,
        "scope": SCOPE,
    }))
    .into_response();
    no_store(&mut resp);
    resp
}

async fn token(
    State(state): State<Arc<OAuthState>>,
    form: Result<Form<TokenForm>, axum::extract::rejection::FormRejection>,
) -> Response {
    let Ok(Form(form)) = form else {
        return cors(
            OAuthError::request("the body must be application/x-www-form-urlencoded")
                .into_response(),
        );
    };
    let now = unix_now();
    let Some(client_id) = form.client_id.as_deref().filter(|s| !s.is_empty()) else {
        return cors(OAuthError::request("client_id is required").into_response());
    };
    let result = match form.grant_type.as_deref() {
        Some("authorization_code") => match (form.code.as_deref(), form.code_verifier.as_deref()) {
            (Some(code), Some(verifier)) => {
                state.exchange_code(code, client_id, form.redirect_uri.as_deref(), verifier, now)
            }
            _ => Err(OAuthError::request("code and code_verifier are required")),
        },
        Some("refresh_token") => match form.refresh_token.as_deref() {
            Some(refresh) => state.refresh(refresh, client_id, now),
            None => Err(OAuthError::request("refresh_token is required")),
        },
        _ => Err(OAuthError::new(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            "grant_type must be authorization_code or refresh_token",
        )),
    };
    cors(match result {
        Ok(tokens) => token_response(&tokens),
        Err(error) => error.into_response(),
    })
}

#[derive(Deserialize)]
struct RevokeForm {
    token: Option<String>,
    client_id: Option<String>,
}

async fn revoke(
    State(state): State<Arc<OAuthState>>,
    form: Result<Form<RevokeForm>, axum::extract::rejection::FormRejection>,
) -> Response {
    let Ok(Form(form)) = form else {
        return cors(OAuthError::request("token is required").into_response());
    };
    let Some(token) = form.token.as_deref().filter(|t| !t.is_empty()) else {
        return cors(OAuthError::request("token is required").into_response());
    };
    state.revoke(token, form.client_id.as_deref());
    cors(StatusCode::OK.into_response())
}

fn require_owner(auth: Option<&AuthKind>) -> Option<Response> {
    (auth != Some(&AuthKind::Owner)).then(|| {
        super::json_error(
            StatusCode::FORBIDDEN,
            "owner_required",
            "only the instance token may administer OAuth clients",
        )
    })
}

async fn admin_list_clients(
    State(state): State<Arc<OAuthState>>,
    auth: Option<Extension<AuthKind>>,
) -> Response {
    if let Some(refused) = require_owner(auth.as_ref().map(|Extension(a)| a)) {
        return refused;
    }
    let mut resp =
        Json(serde_json::json!({ "clients": state.list_clients(unix_now()) })).into_response();
    no_store(&mut resp);
    resp
}

async fn admin_revoke_client(
    State(state): State<Arc<OAuthState>>,
    auth: Option<Extension<AuthKind>>,
    Path(client_id): Path<String>,
) -> Response {
    if let Some(refused) = require_owner(auth.as_ref().map(|Extension(a)| a)) {
        return refused;
    }
    if state.remove_client(&client_id) {
        Json(serde_json::json!({ "revoked": true, "client_id": client_id })).into_response()
    } else {
        super::json_error(StatusCode::NOT_FOUND, "unknown_client", "no such client")
    }
}

#[cfg(test)]
#[path = "oauth_tests.rs"]
mod tests;
