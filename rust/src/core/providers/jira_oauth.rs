//! Jira Cloud OAuth 2.0 (3LO) client.
//!
//! Atlassian's 3LO is a *confidential* client flow: the token exchange requires a
//! `client_id` **and** `client_secret`. lean-ctx ships no hosted backend and
//! embeds no secrets, so each user registers their own free Atlassian OAuth 2.0
//! (3LO) app (developer.atlassian.com → "OAuth 2.0 integration") and points
//! lean-ctx at it via environment variables:
//!
//!   - `JIRA_OAUTH_CLIENT_ID`     — the app's client id
//!   - `JIRA_OAUTH_CLIENT_SECRET` — the app's client secret
//!   - `JIRA_OAUTH_SCOPES`        — optional, space-separated; defaults below
//!
//! Run once to grant consent:
//!
//! ```text
//! lean-ctx provider auth jira [--data-source <id>]
//! ```
//!
//! Tokens are stored in `~/.lean-ctx/credentials/jira-oauth.json` (file mode
//! `0600`), keyed by data-source id so multiple Jira tenants / custom Jira data
//! sources can coexist. Access tokens are refreshed automatically using
//! Atlassian's **rotating** refresh-token flow: every refresh response that
//! carries a new refresh token replaces the stored one. When the refresh token
//! is itself revoked or expired, callers receive a clear "reconnect" error.
//!
//! ## Minimal scopes
//!
//! - `read:jira-work` — read issues, projects, boards, and sprints
//! - `read:jira-user` — resolve reporter / assignee display names
//! - `offline_access` — receive a refresh token for unattended refresh
//!
//! Add more (e.g. `write:jira-work`) only if a future action needs them.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const AUTHORIZE_URL: &str = "https://auth.atlassian.com/authorize";
const TOKEN_URL: &str = "https://auth.atlassian.com/oauth/token";
const RESOURCES_URL: &str = "https://api.atlassian.com/oauth/token/accessible-resources";
/// Per-cloud API prefix; the full base is `{API_BASE}/{cloud_id}`.
pub const API_BASE: &str = "https://api.atlassian.com/ex/jira";
const DEFAULT_SCOPES: &str = "read:jira-work read:jira-user offline_access";
/// Refresh this many seconds *before* the real expiry to absorb clock skew and
/// in-flight request latency.
const EXPIRY_SKEW_SECS: u64 = 60;
/// How long the loopback listener waits for the browser redirect before aborting.
const AUTH_REDIRECT_TIMEOUT_SECS: u64 = 300;

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

// ---------------------------------------------------------------------------
// App configuration (the user's own Atlassian 3LO app)
// ---------------------------------------------------------------------------

/// The user-registered Atlassian OAuth 2.0 (3LO) application credentials.
#[derive(Debug, Clone)]
pub struct OAuthApp {
    pub client_id: String,
    pub client_secret: String,
    pub scopes: String,
}

impl OAuthApp {
    /// Reads the app credentials from the environment. Returns a descriptive
    /// error (with setup guidance) when they are missing.
    pub fn from_env() -> Result<Self, String> {
        let client_id = std::env::var("JIRA_OAUTH_CLIENT_ID")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| {
                "JIRA_OAUTH_CLIENT_ID not set. Register a free Atlassian OAuth 2.0 (3LO) app at \
                 https://developer.atlassian.com/console/myapps/ and export JIRA_OAUTH_CLIENT_ID \
                 and JIRA_OAUTH_CLIENT_SECRET."
                    .to_string()
            })?;
        let client_secret = std::env::var("JIRA_OAUTH_CLIENT_SECRET")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| {
                "JIRA_OAUTH_CLIENT_SECRET not set (from your Atlassian 3LO app).".to_string()
            })?;
        let scopes = std::env::var("JIRA_OAUTH_SCOPES")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_SCOPES.to_string());
        Ok(Self {
            client_id: client_id.trim().to_string(),
            client_secret: client_secret.trim().to_string(),
            scopes,
        })
    }
}

// ---------------------------------------------------------------------------
// Stored credentials (per data-source)
// ---------------------------------------------------------------------------

/// A persisted Jira OAuth credential for one data-source id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredCredential {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds at which `access_token` expires.
    pub expires_at: u64,
    /// Atlassian cloud id, used in `https://api.atlassian.com/ex/jira/{cloud_id}`.
    pub cloud_id: String,
    /// The site URL (e.g. `https://your-site.atlassian.net`) for `/browse` links.
    pub cloud_url: String,
    pub scopes: String,
}

impl StoredCredential {
    /// True if the access token is expired or within the skew window.
    #[must_use]
    pub fn needs_refresh(&self, now: u64) -> bool {
        now.saturating_add(EXPIRY_SKEW_SECS) >= self.expires_at
    }

    /// The per-cloud Jira API base URL for this credential.
    #[must_use]
    pub fn api_base(&self) -> String {
        format!("{API_BASE}/{}", self.cloud_id)
    }
}

/// The on-disk credential store: `{ data_source_id -> StoredCredential }`.
type Store = HashMap<String, StoredCredential>;

fn credentials_path() -> Result<PathBuf, String> {
    // GH #439: store under the typed data resolver (doctor --fix categorizes
    // `credentials/` as data) so a split install doesn't re-create ~/.lean-ctx.
    Ok(crate::core::paths::data_dir()?
        .join("credentials")
        .join("jira-oauth.json"))
}

fn load_store() -> Store {
    let Ok(path) = credentials_path() else {
        return Store::new();
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return Store::new();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

fn save_store(store: &Store) -> Result<(), String> {
    let path = credentials_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let json = serde_json::to_vec_pretty(store).map_err(|e| format!("serialize error: {e}"))?;
    // Write atomically with restrictive permissions so tokens are not
    // world-readable. The temp file is created with 0600 up front on Unix.
    let tmp = path.with_extension("json.tmp");
    write_private(&tmp, &json)?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("cannot persist credentials: {e}"))?;
    Ok(())
}

#[cfg(unix)]
fn write_private(path: &PathBuf, bytes: &[u8]) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    f.write_all(bytes)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &PathBuf, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Returns the stored credential for `data_source`, if any.
#[must_use]
pub fn get_credential(data_source: &str) -> Option<StoredCredential> {
    load_store().get(data_source).cloned()
}

/// Persists (or replaces) the credential for `data_source`.
pub fn put_credential(data_source: &str, cred: StoredCredential) -> Result<(), String> {
    let mut store = load_store();
    store.insert(data_source.to_string(), cred);
    save_store(&store)
}

/// Removes the credential for `data_source`. Returns true if one existed.
pub fn remove_credential(data_source: &str) -> Result<bool, String> {
    let mut store = load_store();
    let existed = store.remove(data_source).is_some();
    save_store(&store)?;
    Ok(existed)
}

/// Lists the data-source ids that currently have a stored credential.
#[must_use]
pub fn list_connections() -> Vec<String> {
    let mut keys: Vec<String> = load_store().into_keys().collect();
    keys.sort();
    keys
}

// ---------------------------------------------------------------------------
// Token endpoint payloads
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

/// One Atlassian cloud site the consenting user can access.
#[derive(Debug, Clone, Deserialize)]
pub struct CloudResource {
    pub id: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub name: String,
}

// ---------------------------------------------------------------------------
// Pure URL/body builders (unit-tested)
// ---------------------------------------------------------------------------

/// Builds the Atlassian consent URL for the authorization-code flow.
#[must_use]
pub fn authorize_url(app: &OAuthApp, redirect_uri: &str, state: &str) -> String {
    format!(
        "{AUTHORIZE_URL}?audience=api.atlassian.com&client_id={cid}&scope={scope}&redirect_uri={redirect}&state={state}&response_type=code&prompt=consent",
        cid = urlencoding::encode(&app.client_id),
        scope = urlencoding::encode(&app.scopes),
        redirect = urlencoding::encode(redirect_uri),
        state = urlencoding::encode(state),
    )
}

fn form_encode(pairs: &[(&str, &str)]) -> Vec<u8> {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&")
        .into_bytes()
}

// ---------------------------------------------------------------------------
// HTTP calls
// ---------------------------------------------------------------------------

fn post_token(body: &[u8]) -> Result<TokenResponse, String> {
    let text = ureq::post(TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .send(body)
        .map_err(|e| format!("Jira OAuth token request failed: {e}"))?
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Jira OAuth token read error: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("Jira OAuth token parse error: {e}"))
}

fn exchange_code(app: &OAuthApp, code: &str, redirect_uri: &str) -> Result<TokenResponse, String> {
    let body = form_encode(&[
        ("grant_type", "authorization_code"),
        ("client_id", &app.client_id),
        ("client_secret", &app.client_secret),
        ("code", code),
        ("redirect_uri", redirect_uri),
    ]);
    post_token(&body)
}

fn refresh_tokens(app: &OAuthApp, refresh_token: &str) -> Result<TokenResponse, String> {
    let body = form_encode(&[
        ("grant_type", "refresh_token"),
        ("client_id", &app.client_id),
        ("client_secret", &app.client_secret),
        ("refresh_token", refresh_token),
    ]);
    post_token(&body)
}

/// Fetches the cloud sites the consenting user can access.
pub fn accessible_resources(access_token: &str) -> Result<Vec<CloudResource>, String> {
    let text = ureq::get(RESOURCES_URL)
        .header("Authorization", &format!("Bearer {access_token}"))
        .header("Accept", "application/json")
        .call()
        .map_err(|e| format!("Jira accessible-resources request failed: {e}"))?
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Jira accessible-resources read error: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("Jira accessible-resources parse error: {e}"))
}

// ---------------------------------------------------------------------------
// Resolver used by the provider on every API call
// ---------------------------------------------------------------------------

/// A ready-to-use bearer token plus the cloud routing info for a data-source.
#[derive(Debug, Clone)]
pub struct ResolvedToken {
    pub access_token: String,
    pub cloud_id: String,
    pub cloud_url: String,
}

/// Returns a valid access token for `data_source`, refreshing (and persisting
/// the rotated refresh token) if the stored token is expired.
///
/// Errors clearly instruct the user to (re)connect when no credential exists or
/// the refresh token is no longer valid.
pub fn ensure_valid_access_token(data_source: &str) -> Result<ResolvedToken, String> {
    let cred = get_credential(data_source).ok_or_else(|| {
        format!(
            "Jira data source '{data_source}' is not connected. Run: lean-ctx provider auth jira \
             --data-source {data_source}"
        )
    })?;

    if !cred.needs_refresh(now_secs()) {
        return Ok(ResolvedToken {
            access_token: cred.access_token,
            cloud_id: cred.cloud_id,
            cloud_url: cred.cloud_url,
        });
    }

    // Expired: refresh requires the app credentials.
    let app = OAuthApp::from_env().map_err(|e| {
        format!("Jira access token for '{data_source}' expired and cannot refresh: {e}")
    })?;

    let tok = refresh_tokens(&app, &cred.refresh_token).map_err(|e| {
        format!(
            "Jira token refresh for '{data_source}' failed ({e}). The refresh token may be \
             revoked or expired — reconnect with: lean-ctx provider auth jira --data-source {data_source}"
        )
    })?;

    // Atlassian rotates refresh tokens: keep the new one if returned, else reuse.
    let new_refresh = tok.refresh_token.unwrap_or(cred.refresh_token);
    let updated = StoredCredential {
        access_token: tok.access_token.clone(),
        refresh_token: new_refresh,
        expires_at: now_secs().saturating_add(tok.expires_in),
        cloud_id: cred.cloud_id.clone(),
        cloud_url: cred.cloud_url.clone(),
        scopes: tok.scope.unwrap_or(cred.scopes),
    };
    put_credential(data_source, updated.clone())?;

    Ok(ResolvedToken {
        access_token: updated.access_token,
        cloud_id: updated.cloud_id,
        cloud_url: updated.cloud_url,
    })
}

// ---------------------------------------------------------------------------
// Interactive authorization-code flow (CLI)
// ---------------------------------------------------------------------------

/// Generates a cryptographically-random URL-safe state token for CSRF defense.
fn random_state() -> String {
    let mut buf = [0u8; 24];
    if getrandom::fill(&mut buf).is_err() {
        // Extremely unlikely; fall back to a time-derived value. Still unguessable
        // enough for a single short-lived loopback exchange, and the redirect is
        // bound to a freshly-bound local port.
        let n = now_secs();
        for (i, b) in buf.iter_mut().enumerate() {
            *b = ((n >> (i % 8)) as u8) ^ (i as u8).wrapping_mul(31);
        }
    }
    use std::fmt::Write as _;
    buf.iter()
        .fold(String::with_capacity(buf.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

fn open_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let cmd = ("open", vec![url.to_string()]);
    #[cfg(target_os = "windows")]
    let cmd = (
        "cmd",
        vec![
            "/C".to_string(),
            "start".to_string(),
            String::new(),
            url.to_string(),
        ],
    );
    #[cfg(all(unix, not(target_os = "macos")))]
    let cmd = ("xdg-open", vec![url.to_string()]);

    let _ = std::process::Command::new(cmd.0)
        .args(cmd.1)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Parses `code` and `state` from a raw HTTP request line like
/// `GET /callback?code=XXX&state=YYY HTTP/1.1`.
fn parse_callback(request_line: &str) -> Option<(String, String)> {
    let path = request_line.split_whitespace().nth(1)?;
    let query = path.split_once('?')?.1;
    let mut code = None;
    let mut state = None;
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            let decoded = urlencoding::decode(v)
                .map(std::borrow::Cow::into_owned)
                .ok()?;
            match k {
                "code" => code = Some(decoded),
                "state" => state = Some(decoded),
                _ => {}
            }
        }
    }
    Some((code?, state?))
}

fn await_redirect(listener: &TcpListener, timeout: Duration) -> Result<(String, String), String> {
    listener
        .set_nonblocking(false)
        .map_err(|e| format!("listener error: {e}"))?;
    let deadline = std::time::Instant::now() + timeout;
    // A single browser redirect; loop only to skip favicon/preflight noise.
    loop {
        if std::time::Instant::now() >= deadline {
            return Err("timed out waiting for the Atlassian redirect (5 min)".to_string());
        }
        let (mut stream, _) = listener
            .accept()
            .map_err(|e| format!("failed to accept redirect: {e}"))?;
        stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
        let mut buf = [0u8; 4096];
        let n = stream.read(&mut buf).unwrap_or(0);
        let request = String::from_utf8_lossy(&buf[..n]);
        let first_line = request.lines().next().unwrap_or("");

        if let Some((code, state)) = parse_callback(first_line) {
            let html = "<html><body style=\"font-family:sans-serif\"><h2>lean-ctx connected to Jira ✓</h2><p>You can close this tab and return to your terminal.</p></body></html>";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                html.len(),
                html
            );
            let _ = stream.write_all(resp.as_bytes());
            return Ok((code, state));
        }
        // Not the callback (e.g. favicon) — respond 204 and keep waiting.
        let _ = stream.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n");
    }
}

fn pick_resource(resources: Vec<CloudResource>) -> Result<CloudResource, String> {
    match resources.len() {
        0 => Err(
            "no accessible Jira Cloud sites for this account — check the app scopes and that you \
             selected a site during consent"
                .to_string(),
        ),
        1 => Ok(resources.into_iter().next().unwrap()),
        _ => {
            println!("\nMultiple Jira sites are accessible — choose one:");
            for (i, r) in resources.iter().enumerate() {
                println!("  [{}] {} ({})", i + 1, r.url, r.name);
            }
            print!("Enter number: ");
            let _ = std::io::stdout().flush();
            let mut line = String::new();
            std::io::stdin()
                .read_line(&mut line)
                .map_err(|e| format!("input error: {e}"))?;
            let idx: usize = line
                .trim()
                .parse()
                .map_err(|_| "invalid selection".to_string())?;
            resources
                .into_iter()
                .nth(idx.saturating_sub(1))
                .ok_or_else(|| "selection out of range".to_string())
        }
    }
}

/// Runs the full interactive OAuth 2.0 3LO authorization-code flow and stores
/// the resulting credential under `data_source`.
pub fn run_auth_flow(data_source: &str) -> Result<(), String> {
    let app = OAuthApp::from_env()?;

    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("cannot bind loopback redirect listener: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("cannot read local port: {e}"))?
        .port();
    let redirect_uri = format!("http://localhost:{port}/callback");

    let state = random_state();
    let url = authorize_url(&app, &redirect_uri, &state);

    println!(
        "\nlean-ctx needs your consent to read Jira on your behalf.\n\
         Add this exact redirect URL to your Atlassian app's \"Callback URL\" list first:\n  {redirect_uri}\n\n\
         Then open this URL to authorize (it should open automatically):\n  {url}\n"
    );
    open_in_browser(&url);

    let (code, recv_state) =
        await_redirect(&listener, Duration::from_secs(AUTH_REDIRECT_TIMEOUT_SECS))?;
    if recv_state != state {
        return Err("state mismatch on redirect (possible CSRF) — aborting".to_string());
    }

    let tok = exchange_code(&app, &code, &redirect_uri)?;
    let resources = accessible_resources(&tok.access_token)?;
    let resource = pick_resource(resources)?;

    let cred = StoredCredential {
        access_token: tok.access_token,
        refresh_token: tok
            .refresh_token
            .ok_or("Atlassian did not return a refresh token — ensure the 'offline_access' scope is granted")?,
        expires_at: now_secs().saturating_add(tok.expires_in),
        cloud_id: resource.id,
        cloud_url: resource.url.clone(),
        scopes: tok.scope.unwrap_or(app.scopes),
    };
    put_credential(data_source, cred)?;

    println!(
        "✓ Connected Jira Cloud site {} as data source '{data_source}'.\n  Tokens stored in {}",
        resource.url,
        credentials_path()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> OAuthApp {
        OAuthApp {
            client_id: "abc 123".to_string(),
            client_secret: "secret".to_string(),
            scopes: "read:jira-work offline_access".to_string(),
        }
    }

    #[test]
    fn authorize_url_encodes_all_params() {
        let url = authorize_url(&app(), "http://localhost:5000/callback", "st/ate+1");
        assert!(url.starts_with("https://auth.atlassian.com/authorize?"));
        assert!(url.contains("audience=api.atlassian.com"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("prompt=consent"));
        assert!(url.contains("client_id=abc%20123"));
        assert!(url.contains("scope=read%3Ajira-work%20offline_access"));
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A5000%2Fcallback"));
        assert!(url.contains("state=st%2Fate%2B1"));
    }

    #[test]
    fn parse_callback_extracts_code_and_state() {
        let line = "GET /callback?code=AUTH%2FCODE&state=xyz HTTP/1.1";
        let (code, state) = parse_callback(line).unwrap();
        assert_eq!(code, "AUTH/CODE");
        assert_eq!(state, "xyz");
    }

    #[test]
    fn parse_callback_handles_missing_params() {
        assert!(parse_callback("GET /callback?code=only HTTP/1.1").is_none());
        assert!(parse_callback("GET /favicon.ico HTTP/1.1").is_none());
    }

    #[test]
    fn needs_refresh_respects_skew() {
        let now = 1_000_000;
        let mut cred = StoredCredential {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: now + EXPIRY_SKEW_SECS + 10,
            cloud_id: "cid".into(),
            cloud_url: "https://x.atlassian.net".into(),
            scopes: DEFAULT_SCOPES.into(),
        };
        assert!(!cred.needs_refresh(now), "valid token must not refresh");
        cred.expires_at = now + EXPIRY_SKEW_SECS - 1;
        assert!(cred.needs_refresh(now), "near-expiry token must refresh");
        cred.expires_at = now - 1;
        assert!(cred.needs_refresh(now), "expired token must refresh");
    }

    #[test]
    fn api_base_includes_cloud_id() {
        let cred = StoredCredential {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: 0,
            cloud_id: "11aa-22bb".into(),
            cloud_url: "https://x.atlassian.net".into(),
            scopes: DEFAULT_SCOPES.into(),
        };
        assert_eq!(
            cred.api_base(),
            "https://api.atlassian.com/ex/jira/11aa-22bb"
        );
    }

    #[test]
    fn form_encode_escapes_values() {
        let body = form_encode(&[("grant_type", "authorization_code"), ("code", "a/b c")]);
        let s = String::from_utf8(body).unwrap();
        assert_eq!(s, "grant_type=authorization_code&code=a%2Fb%20c");
    }

    #[test]
    fn pick_resource_auto_selects_single() {
        let r = pick_resource(vec![CloudResource {
            id: "cid".into(),
            url: "https://only.atlassian.net".into(),
            name: "Only".into(),
        }])
        .unwrap();
        assert_eq!(r.id, "cid");
    }

    #[test]
    fn pick_resource_errors_on_empty() {
        assert!(pick_resource(vec![]).is_err());
    }

    #[test]
    fn random_state_is_unique_and_hex() {
        let a = random_state();
        let b = random_state();
        assert_eq!(a.len(), 48, "24 bytes -> 48 hex chars");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b, "state tokens must differ");
    }
}
