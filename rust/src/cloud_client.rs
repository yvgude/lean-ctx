use std::path::PathBuf;

#[allow(dead_code, unreachable_pub)]
mod entitlement_cache;
pub use entitlement_cache::EffectivePlan as VerifiedEffectivePlan;

pub const ENTITLEMENT_DENIAL_PREFIX: &str = "Signed entitlement does not allow ";

fn config_dir() -> PathBuf {
    // GH #439: data_dir() already honors LEAN_CTX_DATA_DIR + legacy/XDG, so the
    // cloud cache follows the migration instead of pinning ~/.lean-ctx.
    crate::core::paths::data_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("cloud")
}

fn credentials_path() -> PathBuf {
    config_dir().join("credentials.json")
}

/// Base URL of the lean-ctx cloud API. Unit tests and cargo-launched binaries
/// get the discard port instead of any non-loopback endpoint, so test fixtures
/// (telemetry probes, feedback, stats, wrapped cards) can never reach
/// production; their requests fail fast with "connection refused".
pub fn api_url() -> String {
    const LOOPBACK_SINK: &str = "http://127.0.0.1:9";
    let configured =
        std::env::var("LEAN_CTX_API_URL").unwrap_or_else(|_| "https://api.leanctx.com".to_string());
    if cloud_endpoint_allowed(&configured, launched_by_cargo()) {
        configured
    } else {
        LOOPBACK_SINK.to_string()
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Credentials {
    api_key: String,
    user_id: String,
    email: String,
    #[serde(default)]
    oauth_client_id: Option<String>,
    #[serde(default)]
    oauth_client_secret: Option<String>,
    #[serde(default)]
    oauth_access_token: Option<String>,
    #[serde(default)]
    oauth_expires_at_unix: Option<i64>,
}

fn load_credentials() -> Option<Credentials> {
    let path = credentials_path();
    // One-time migration for files written before permissions were enforced:
    // tighten anything looser than owner-only on every load.
    tighten_secret_permissions(&path);
    let data = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

fn write_credentials(creds: &Credentials) -> std::io::Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    restrict_dir_permissions(&dir);
    let json = serde_json::to_string_pretty(creds).map_err(std::io::Error::other)?;
    write_secret_file(&credentials_path(), json.as_bytes())
}

/// Writes a secret file atomically (tmp + rename) with owner-only permissions
/// (0o600 on Unix), so credentials are never world-readable — not even
/// transiently between create and chmod.
fn write_secret_file(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("credentials path has no parent directory"))?;
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("credentials path has no file name"))?
        .to_string_lossy();
    let tmp = parent.join(format!(".{name}.tmp.{}", std::process::id()));

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }

    let result = (|| {
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        #[cfg(windows)]
        {
            if path.exists() {
                std::fs::remove_file(path)?;
            }
        }
        std::fs::rename(&tmp, path)
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(unix)]
fn restrict_dir_permissions(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn restrict_dir_permissions(_dir: &std::path::Path) {}

#[cfg(unix)]
fn tighten_secret_permissions(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path)
        && meta.permissions().mode() & 0o077 != 0
    {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
}

#[cfg(not(unix))]
fn tighten_secret_permissions(_path: &std::path::Path) {}

pub fn save_credentials(api_key: &str, user_id: &str, email: &str) -> std::io::Result<()> {
    let mut creds = load_credentials().unwrap_or(Credentials {
        api_key: api_key.to_string(),
        user_id: user_id.to_string(),
        email: email.to_string(),
        oauth_client_id: None,
        oauth_client_secret: None,
        oauth_access_token: None,
        oauth_expires_at_unix: None,
    });
    creds.api_key = api_key.to_string();
    creds.user_id = user_id.to_string();
    creds.email = email.to_string();
    // Access tokens are bound to a client and should be re-fetched after login changes.
    creds.oauth_access_token = None;
    creds.oauth_expires_at_unix = None;
    write_credentials(&creds)
}

pub fn load_api_key() -> Option<String> {
    load_credentials().map(|c| c.api_key)
}

pub fn is_logged_in() -> bool {
    load_credentials().is_some()
}

fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// This machine's legacy account-key label, sent with login/registration and
/// attached as `X-Device-Label` to sync pushes. It lets the account service
/// replace this machine's key without replacing the account's other keys.
/// This display label is not the authenticated device identity of a v2 lease.
fn device_label() -> String {
    gethostname::gethostname().to_string_lossy().into_owned()
}

fn auth_bearer_token() -> Result<String, String> {
    let mut creds = load_credentials().ok_or("Not logged in. Run: lean-ctx login")?;

    if let (Some(client_id), Some(client_secret)) = (
        creds.oauth_client_id.clone(),
        creds.oauth_client_secret.clone(),
    ) {
        let now = now_unix();
        if let (Some(token), Some(exp)) = (
            creds.oauth_access_token.clone(),
            creds.oauth_expires_at_unix,
        ) && exp > now + 10
        {
            return Ok(token);
        }

        let url = format!("{}/oauth/token", api_url());
        let resp = ureq::post(&url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send_form([
                ("grant_type", "client_credentials"),
                ("client_id", client_id.as_str()),
                ("client_secret", client_secret.as_str()),
            ])
            .map_err(|e| format!("OAuth token request failed: {e}"))?;

        let resp_body = resp
            .into_body()
            .read_to_string()
            .map_err(|e| format!("Failed to read OAuth response: {e}"))?;

        let json: serde_json::Value =
            serde_json::from_str(&resp_body).map_err(|e| format!("Invalid JSON: {e}"))?;

        let token = json["access_token"]
            .as_str()
            .ok_or("Missing access_token in response")?
            .to_string();
        let expires_in = json["expires_in"].as_i64().unwrap_or(3600);
        let exp = now + expires_in.saturating_sub(30);

        creds.oauth_access_token = Some(token.clone());
        creds.oauth_expires_at_unix = Some(exp);
        let _ = write_credentials(&creds);

        return Ok(token);
    }

    Ok(creds.api_key)
}

pub fn oauth_register_client(client_name: Option<&str>) -> Result<String, String> {
    let mut creds = load_credentials().ok_or("Not logged in. Run: lean-ctx login")?;
    if creds.oauth_client_id.is_some() && creds.oauth_client_secret.is_some() {
        return Ok("OAuth client already registered.".to_string());
    }

    let url = format!("{}/oauth/register", api_url());
    let body = if let Some(name) = client_name {
        serde_json::json!({ "client_name": name })
    } else {
        serde_json::json!({})
    };

    let resp = ureq::post(&url)
        .header("Authorization", &format!("Bearer {}", creds.api_key))
        .header("Content-Type", "application/json")
        .send(&serde_json::to_vec(&body).map_err(|e| format!("JSON error: {e}"))?)
        .map_err(|e| format!("OAuth register failed: {e}"))?;

    let resp_body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;

    let json: serde_json::Value =
        serde_json::from_str(&resp_body).map_err(|e| format!("Invalid JSON: {e}"))?;

    creds.oauth_client_id = Some(
        json["client_id"]
            .as_str()
            .ok_or("Missing client_id in response")?
            .to_string(),
    );
    creds.oauth_client_secret = Some(
        json["client_secret"]
            .as_str()
            .ok_or("Missing client_secret in response")?
            .to_string(),
    );
    creds.oauth_access_token = None;
    creds.oauth_expires_at_unix = None;
    write_credentials(&creds).map_err(|e| format!("Failed to persist OAuth credentials: {e}"))?;

    Ok("OAuth client registered. Cloud requests will use short-lived access tokens.".to_string())
}

pub struct RegisterResult {
    pub api_key: String,
    pub user_id: String,
    pub email_verified: bool,
    pub verification_sent: bool,
}

pub fn register(email: &str, password: Option<&str>) -> Result<RegisterResult, String> {
    let url = format!("{}/api/auth/register", api_url());
    // The label names the key this call issues, so the account page can tell
    // one machine from another. Keys are per-device and additive.
    let mut body = serde_json::json!({ "email": email, "device_label": device_label() });
    if let Some(pw) = password {
        body["password"] = serde_json::Value::String(pw.to_string());
    }

    let resp = ureq::post(&url)
        .header("Content-Type", "application/json")
        .send(&serde_json::to_vec(&body).map_err(|e| format!("JSON error: {e}"))?)
        .map_err(|e| format!("Request failed: {e}"))?;

    let resp_body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;

    let json: serde_json::Value =
        serde_json::from_str(&resp_body).map_err(|e| format!("Invalid JSON: {e}"))?;

    Ok(RegisterResult {
        api_key: json["api_key"]
            .as_str()
            .ok_or("Missing api_key in response")?
            .to_string(),
        user_id: json["user_id"]
            .as_str()
            .ok_or("Missing user_id in response")?
            .to_string(),
        email_verified: json["email_verified"].as_bool().unwrap_or(false),
        verification_sent: json["verification_sent"].as_bool().unwrap_or(false),
    })
}

pub fn forgot_password(email: &str) -> Result<String, String> {
    let url = format!("{}/api/auth/forgot-password", api_url());
    let body = serde_json::json!({ "email": email });

    let resp = ureq::post(&url)
        .header("Content-Type", "application/json")
        .send(&serde_json::to_vec(&body).map_err(|e| format!("JSON error: {e}"))?)
        .map_err(|e| format!("Request failed: {e}"))?;

    let resp_body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;

    let json: serde_json::Value =
        serde_json::from_str(&resp_body).map_err(|e| format!("Invalid JSON: {e}"))?;

    Ok(json["message"]
        .as_str()
        .unwrap_or("If an account exists, a reset email has been sent.")
        .to_string())
}

pub fn login(email: &str, password: &str) -> Result<RegisterResult, String> {
    let url = format!("{}/api/auth/login", api_url());
    let body = serde_json::json!({
        "email": email,
        "password": password,
        "device_label": device_label(),
    });

    let resp = ureq::post(&url)
        .header("Content-Type", "application/json")
        .send(&serde_json::to_vec(&body).map_err(|e| format!("JSON error: {e}"))?)
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("401") {
                "Invalid email or password".to_string()
            } else {
                format!("Request failed: {e}")
            }
        })?;

    let resp_body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;

    let json: serde_json::Value =
        serde_json::from_str(&resp_body).map_err(|e| format!("Invalid JSON: {e}"))?;

    Ok(RegisterResult {
        api_key: json["api_key"]
            .as_str()
            .ok_or("Missing api_key in response")?
            .to_string(),
        user_id: json["user_id"]
            .as_str()
            .ok_or("Missing user_id in response")?
            .to_string(),
        email_verified: json["email_verified"].as_bool().unwrap_or(false),
        verification_sent: false,
    })
}

pub fn sync_stats(stats: &[serde_json::Value]) -> Result<String, String> {
    let bearer = auth_bearer_token()?;
    let url = format!("{}/api/stats", api_url());

    let body = serde_json::json!({ "stats": stats });

    let resp = ureq::post(&url)
        .header("Authorization", &format!("Bearer {bearer}"))
        .header("Content-Type", "application/json")
        .header("X-Device-Label", &device_label())
        .send(&serde_json::to_vec(&body).map_err(|e| format!("JSON error: {e}"))?)
        .map_err(|e| format!("Sync failed: {e}"))?;

    let resp_body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;

    let json: serde_json::Value =
        serde_json::from_str(&resp_body).map_err(|e| format!("Invalid JSON: {e}"))?;

    Ok(json["message"].as_str().unwrap_or("Synced").to_string())
}

pub fn contribute(entries: &[serde_json::Value]) -> Result<String, String> {
    let url = format!("{}/api/contribute", api_url());

    let body = serde_json::json!({ "entries": entries });

    let resp = ureq::post(&url)
        .header("Content-Type", "application/json")
        .send(&serde_json::to_vec(&body).map_err(|e| format!("JSON error: {e}"))?)
        .map_err(|e| format!("Contribute failed: {e}"))?;

    let resp_body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;

    let json: serde_json::Value =
        serde_json::from_str(&resp_body).map_err(|e| format!("Invalid JSON: {e}"))?;

    Ok(json["message"]
        .as_str()
        .unwrap_or("Contributed")
        .to_string())
}

/// Send an anonymous telemetry heartbeat. No authentication required.
/// Payload: installation_id (random UUID), version, OS, arch — nothing else.
pub fn heartbeat(payload: &serde_json::Value) -> Result<String, String> {
    let url = format!("{}/api/telemetry/heartbeat", api_url());

    let resp = ureq::post(&url)
        .header("Content-Type", "application/json")
        .send(&serde_json::to_vec(payload).map_err(|e| format!("JSON error: {e}"))?)
        .map_err(|e| format!("Heartbeat failed: {e}"))?;

    let resp_body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;

    let json: serde_json::Value =
        serde_json::from_str(&resp_body).map_err(|e| format!("Invalid JSON: {e}"))?;

    Ok(json["message"].as_str().unwrap_or("OK").to_string())
}

/// Send one volunteered product-feedback submission. No authentication.
///
/// Anonymous like [`heartbeat`], and for the same reason: there is no account to
/// attach this to, and requiring one would silence exactly the users worth
/// hearing from. The payload carries only what the person typed into the form,
/// plus the installation id and version so answers can be read in context —
/// never anything derived from their code, paths or usage.
///
/// The server may answer 429 when one installation has already sent several
/// today; that is surfaced to the caller rather than swallowed, so the UI can
/// say what happened instead of reporting a send that was dropped.
pub fn submit_product_feedback(payload: &serde_json::Value) -> Result<String, String> {
    let url = format!("{}/api/feedback/product", api_url());

    let resp = ureq::post(&url)
        .header("Content-Type", "application/json")
        .send(&serde_json::to_vec(payload).map_err(|e| format!("JSON error: {e}"))?)
        .map_err(|e| format!("Could not send feedback: {e}"))?;

    let body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;

    let json: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("Invalid JSON: {e}"))?;

    Ok(json["message"].as_str().unwrap_or("Thanks").to_string())
}

/// Cargo exports `CARGO_PKG_NAME` to `cargo test` / `cargo run` processes and
/// every child they spawn, so integration tests driving the binary carry it too.
fn launched_by_cargo() -> bool {
    std::env::var("CARGO_PKG_NAME").is_ok_and(|name| name == env!("CARGO_PKG_NAME"))
}

/// Unit tests and cargo-launched binaries may only talk to a loopback
/// endpoint; otherwise test fixtures would pollute production data.
fn cloud_endpoint_allowed(base_url: &str, launched_by_cargo: bool) -> bool {
    if !cfg!(test) && !launched_by_cargo {
        return true;
    }
    let Some(authority) = base_url
        .strip_prefix("http://")
        .or_else(|| base_url.strip_prefix("https://"))
        .and_then(|rest| rest.split(['/', '?', '#']).next())
    else {
        return false;
    };
    if authority.contains('@') {
        return false;
    }
    let host = match authority.strip_prefix('[') {
        Some(bracketed) => bracketed.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Send one validated telemetry-v2 daily batch. No authentication required.
pub fn telemetry_v2_batch(
    batch: &crate::core::telemetry_v2::TelemetryBatchV2,
) -> Result<String, String> {
    telemetry_v2_batch_with_timeout(batch, std::time::Duration::from_secs(10))
}

pub(crate) fn telemetry_v2_batch_with_timeout(
    batch: &crate::core::telemetry_v2::TelemetryBatchV2,
    timeout: std::time::Duration,
) -> Result<String, String> {
    // A sender may have waited for another lease since its initial precheck.
    let config = crate::core::config::Config::try_load_global()
        .map_err(|_| "Telemetry configuration unavailable".to_string())?;
    let do_not_track = crate::core::host_env::var("DO_NOT_TRACK");
    let telemetry_override = crate::core::host_env::var("LEAN_CTX_TELEMETRY");
    if !config
        .telemetry
        .send_eligible(do_not_track.as_deref(), telemetry_override.as_deref())
    {
        return Err("Telemetry sending is disabled".to_string());
    }
    if crate::core::telemetry_consent::running_in_ci() {
        return Err("Telemetry is not sent from CI".to_string());
    }
    batch
        .validate()
        .map_err(|error| format!("Telemetry validation failed: {error:?}"))?;
    let url = format!("{}/api/telemetry/v2/batch", api_url());
    let response = ureq::post(&url)
        .config()
        .timeout_global(Some(timeout))
        .build()
        .header("Content-Type", "application/json")
        .send(&serde_json::to_vec(batch).map_err(|error| format!("JSON error: {error}"))?)
        .map_err(|error| format!("Telemetry v2 batch failed: {error}"))?;
    let body = response
        .into_body()
        .read_to_string()
        .map_err(|error| format!("Failed to read response: {error}"))?;
    let json: serde_json::Value =
        serde_json::from_str(&body).map_err(|error| format!("Invalid JSON: {error}"))?;
    crate::core::telemetry_notices::remember(crate::core::telemetry_notices::from_response(&json));
    Ok(json["message"].as_str().unwrap_or("OK").to_string())
}

/// Delete every server-side aggregate associated with one installation ID.
pub fn delete_remote_telemetry(
    installation_id: &str,
    deletion_token: &str,
) -> Result<bool, String> {
    let installation_id = uuid::Uuid::parse_str(installation_id)
        .map_err(|_| "Invalid installation ID".to_string())?;
    let url = format!(
        "{}/api/telemetry/v2/installations/{installation_id}",
        api_url()
    );
    if deletion_token.len() != 64
        || !deletion_token
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err("Invalid telemetry deletion credential".to_string());
    }
    let response = ureq::delete(&url)
        .header(
            "Authorization",
            &format!("TelemetryDelete v1.{deletion_token}"),
        )
        .call()
        .map_err(|error| format!("Remote telemetry deletion failed: {error}"))?;
    let status = response.status().as_u16();
    let body = response
        .into_body()
        .read_to_string()
        .map_err(|error| format!("Failed to read deletion response: {error}"))?;
    deletion_confirmed(status, &body)
}

fn deletion_confirmed(status: u16, body: &str) -> Result<bool, String> {
    if status == 204 {
        return Ok(false);
    }
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|error| format!("Invalid deletion response: {error}"))?;
    Ok(value.get("deleted").and_then(serde_json::Value::as_bool) == Some(true))
}

/// Result of a successful Wrapped publish (`POST /api/wrapped`). The `edit_token` is returned
/// (and must be stored to delete/claim later) only on a *fresh* insert; on a signed re-publish
/// the server updates the existing card in place and omits it (the client keeps the stored one).
#[derive(serde::Deserialize)]
pub struct PublishedCard {
    pub id: String,
    #[serde(default)]
    pub edit_token: Option<String>,
    #[serde(default)]
    pub edit_token_challenge: Option<String>,
    #[serde(default)]
    pub challenge_expires_in_secs: Option<i64>,
    pub url: String,
    #[serde(skip)]
    pub account_claimed: bool,
}

/// Publish a whitelisted Wrapped payload. Accepts either a bare payload (legacy anonymous) or a
/// signed envelope `{payload_json, public_key, signature}` (login-less identity → server upsert).
/// If the user is logged in, attaches a Bearer token so the server can auto-claim
/// the card (leaderboard consolidation).
pub fn publish_wrapped(payload: &serde_json::Value) -> Result<PublishedCard, String> {
    let url = format!("{}/api/wrapped", api_url());

    let mut req = ureq::post(&url).header("Content-Type", "application/json");
    if let Ok(token) = auth_bearer_token() {
        req = req.header("Authorization", &format!("Bearer {token}"));
    }
    let resp = req
        .send(&serde_json::to_vec(payload).map_err(|e| format!("JSON error: {e}"))?)
        .map_err(|e| format!("Publish failed: {e}"))?;

    let resp_body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;

    serde_json::from_str(&resp_body).map_err(|e| format!("Invalid response: {e}"))
}

#[derive(serde::Deserialize)]
struct RecoveredEditToken {
    edit_token: String,
}

/// Exchange a one-time server challenge for a rotated edit token after the
/// caller proves possession of the card's persistent publisher key.
pub fn recover_wrapped_edit_token(
    id: &str,
    nonce: &str,
    public_key: &str,
    signature: &str,
) -> Result<String, String> {
    let url = format!("{}/api/wrapped/{id}/edit-token/recover", api_url());
    let body = serde_json::json!({
        "nonce": nonce,
        "public_key": public_key,
        "signature": signature,
    });
    let resp = ureq::post(&url)
        .header("Content-Type", "application/json")
        .send(&serde_json::to_vec(&body).map_err(|e| format!("JSON error: {e}"))?)
        .map_err(|e| format!("Edit-token recovery failed: {e}"))?;
    let response = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;
    let recovered: RecoveredEditToken =
        serde_json::from_str(&response).map_err(|e| format!("Invalid recovery response: {e}"))?;
    if recovered.edit_token.is_empty() {
        return Err("Invalid recovery response: empty edit token".to_string());
    }
    Ok(recovered.edit_token)
}

/// Delete a previously published card using its one-time `edit_token` (sent as `X-Edit-Token`).
///
/// Idempotent: a card the server no longer has (404/410) counts as removed.
/// The caller's goal is "this page is not public any more", and failing a
/// takedown because it already succeeded would strand the local record with no
/// way to clear it (#1726).
pub fn unpublish_wrapped(id: &str, edit_token: &str) -> Result<(), String> {
    let url = format!("{}/api/wrapped/{id}", api_url());

    match ureq::delete(&url).header("X-Edit-Token", edit_token).call() {
        // Removed now, or already gone (404/410) — either way it is not public.
        Ok(_) | Err(ureq::Error::StatusCode(404 | 410)) => Ok(()),
        Err(e) => Err(format!("Unpublish failed: {e}")),
    }
}

/// Bind a published card to the logged-in account so the leaderboard stacks all of the
/// user's machines under one entry (#488). Auth: account Bearer + the card's `edit_token`
/// (`X-Edit-Token`). Server: `POST /api/wrapped/:id/claim`. Requires being logged in.
pub fn claim_wrapped(id: &str, edit_token: &str) -> Result<(), String> {
    let bearer = auth_bearer_token()?;
    let url = format!("{}/api/wrapped/{id}/claim", api_url());

    ureq::post(&url)
        .header("Authorization", &format!("Bearer {bearer}"))
        .header("X-Edit-Token", edit_token)
        .send_empty()
        .map_err(|e| format!("Claim failed: {e}"))?;
    Ok(())
}

/// A freshly minted pairing code for login-less machine linking (GH #736).
#[derive(serde::Deserialize)]
pub struct LinkCode {
    pub code: String,
    pub expires_in_secs: i64,
}

/// Start a login-less machine link: mint a short-lived pairing code for this card.
/// Auth: the card's `edit_token` only — no account. Server: `POST /api/wrapped/:id/link/start`.
pub fn link_wrapped_start(id: &str, edit_token: &str) -> Result<LinkCode, String> {
    let url = format!("{}/api/wrapped/{id}/link/start", api_url());

    let resp = ureq::post(&url)
        .header("X-Edit-Token", edit_token)
        .send_empty()
        .map_err(|e| format!("Link start failed: {e}"))?;
    let body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;
    serde_json::from_str(&body).map_err(|e| format!("Invalid response: {e}"))
}

/// Complete a login-less machine link on the second machine: join this card into
/// the pairing code's group. Auth: this card's `edit_token` — no account.
/// Server: `POST /api/wrapped/:id/link/complete`.
pub fn link_wrapped_complete(id: &str, edit_token: &str, code: &str) -> Result<(), String> {
    let url = format!("{}/api/wrapped/{id}/link/complete", api_url());
    let body = serde_json::json!({ "code": code });

    ureq::post(&url)
        .header("X-Edit-Token", edit_token)
        .header("Content-Type", "application/json")
        .send(&serde_json::to_vec(&body).map_err(|e| format!("JSON error: {e}"))?)
        .map_err(|e| match e {
            ureq::Error::StatusCode(404) => {
                "code invalid or expired — mint a fresh one with  lean-ctx gain --link".to_string()
            }
            other => format!("Link failed: {other}"),
        })?;
    Ok(())
}

/// Push the knowledge store as a zero-knowledge vault (GL #467): entries are
/// sealed client-side (XChaCha20-Poly1305, domain-separated HKDF key) — the
/// backend stores ciphertext and can never read them. The first vault push
/// also purges the account's legacy plaintext rows server-side.
pub fn push_knowledge(entries: &[serde_json::Value]) -> Result<String, String> {
    let bearer = auth_bearer_token()?;
    let key = knowledge_vault_key()?;
    let blob = crate::core::knowledge_vault::seal(entries, &key).map_err(|e| e.to_string())?;
    let url = format!("{}/api/sync/knowledge", api_url());

    let resp = ureq::post(&url)
        .header("Authorization", &format!("Bearer {bearer}"))
        .header("Content-Type", "application/octet-stream")
        .header("X-Entry-Count", &entries.len().to_string())
        .header("X-Device-Label", &device_label())
        .send(blob.as_slice())
        .map_err(|e| format!("Push failed: {e}"))?;

    let resp_body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;

    let json: serde_json::Value =
        serde_json::from_str(&resp_body).map_err(|e| format!("Invalid JSON: {e}"))?;

    Ok(format!(
        "{} entries synced (end-to-end encrypted)",
        json["entry_count"].as_i64().unwrap_or(entries.len() as i64)
    ))
}

/// The account's knowledge-vault key — same stable-API-key derivation rule as
/// [`index_bundle_key`], different HKDF domain (`knowledge-vault-v1`).
fn knowledge_vault_key() -> Result<[u8; 32], String> {
    let api_key = load_api_key().ok_or("Not logged in. Run: lean-ctx login")?;
    if api_key.trim().is_empty() {
        return Err("Not logged in. Run: lean-ctx login".into());
    }
    Ok(crate::core::knowledge_vault::derive_vault_key(&api_key))
}

pub fn pull_cloud_models() -> Result<serde_json::Value, String> {
    let bearer = auth_bearer_token()?;
    let url = format!("{}/api/cloud/models", api_url());

    let resp = ureq::get(&url)
        .header("Authorization", &format!("Bearer {bearer}"))
        .call()
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("403") {
                "This feature is not available for your account.".to_string()
            } else {
                format!("Connection failed. Check your internet connection. ({e})")
            }
        })?;

    let resp_body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;

    serde_json::from_str(&resp_body).map_err(|e| format!("Invalid response: {e}"))
}

pub fn save_cloud_models(data: &serde_json::Value) -> std::io::Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    let json = serde_json::to_string_pretty(data).map_err(std::io::Error::other)?;
    std::fs::write(dir.join("cloud_models.json"), json)
}

pub fn load_cloud_models() -> Option<serde_json::Value> {
    let path = config_dir().join("cloud_models.json");
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Fetch the public community leaderboard as JSON (`{ "entries": [ … ] }`).
///
/// Public, login-less endpoint (`GET /api/leaderboard`, contract:
/// `docs/contracts/wrapped-permalink-v1.md`). The dashboard proxies it
/// same-origin (#466) so the browser never reaches `api.leanctx.com` directly —
/// the dashboard CSP pins `connect-src` to `'self'`. A 10s global timeout keeps
/// a slow upstream from tying up a dashboard request thread.
pub fn fetch_leaderboard() -> Result<serde_json::Value, String> {
    let url = format!("{}/api/leaderboard", api_url());
    let resp = ureq::get(&url)
        .config()
        .timeout_global(Some(std::time::Duration::from_secs(10)))
        .build()
        .call()
        .map_err(|e| format!("Could not reach the leaderboard service: {e}"))?;
    let body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read leaderboard response: {e}"))?;
    serde_json::from_str(&body).map_err(|e| format!("Invalid leaderboard JSON: {e}"))
}

pub fn is_cloud_user() -> bool {
    let path = config_dir().join("plan.txt");
    std::fs::read_to_string(path).is_ok_and(|value| plan_value_has_cloud_access(&value))
}

fn plan_value_has_cloud_access(value: &str) -> bool {
    value.trim().eq_ignore_ascii_case("cloud")
        || crate::core::billing::Plan::parse_known(value)
            .is_some_and(|plan| plan.rank() >= crate::core::billing::Plan::Pro.rank())
}

/// Days a cached plan keeps granting its hosted entitlements while the billing
/// backend is unreachable. Generous on purpose: a network blip or a weekend
/// offline must never silently demote a paying user to Community.
pub const PLAN_GRACE_DAYS: i64 = 14;

fn plan_cache_path() -> PathBuf {
    config_dir().join("plan.json")
}

/// The locally cached plan plus *when* it was last confirmed against the billing
/// backend. The timestamp is what powers offline grace.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlanCache {
    pub plan: String,
    /// Unix seconds of the last successful backend confirmation.
    pub verified_at: i64,
}

pub fn save_plan(plan: &str) -> std::io::Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    // Legacy flat file kept for back-compat (`is_cloud_user` still reads it).
    std::fs::write(dir.join("plan.txt"), plan)?;
    // Structured cache carrying the verification time for offline grace.
    let cache = PlanCache {
        plan: plan.to_string(),
        verified_at: now_unix(),
    };
    let json = serde_json::to_string_pretty(&cache).map_err(std::io::Error::other)?;
    std::fs::write(plan_cache_path(), json)
}

/// The cached plan, if any. Prefers the structured `plan.json`; falls back to a
/// legacy `plan.txt` (no timestamp → `verified_at = 0`, i.e. immediately past
/// grace until the next successful refresh re-stamps it).
pub fn cached_plan() -> Option<PlanCache> {
    if let Ok(data) = std::fs::read_to_string(plan_cache_path())
        && let Ok(cache) = serde_json::from_str::<PlanCache>(&data)
    {
        return Some(cache);
    }
    let legacy = std::fs::read_to_string(config_dir().join("plan.txt")).ok()?;
    Some(PlanCache {
        plan: legacy.trim().to_string(),
        verified_at: 0,
    })
}

/// Where an effective plan came from — drives the wording in `billing status`
/// and the dashboard badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanSource {
    /// Just confirmed against the backend this run.
    Live,
    /// Served from the local cache and still within the grace window.
    Cached,
    /// Cached confirmation is past the grace window → demoted to Community.
    Expired,
    /// No cached plan at all (never logged in / never synced) → Community.
    None,
}

/// A resolved plan plus provenance and donation/support recognition.
#[derive(Debug, Clone)]
pub struct EffectivePlan {
    pub plan: crate::core::billing::Plan,
    pub supporter_recognition: bool,
    pub source: PlanSource,
    pub verified_at: Option<i64>,
    pub grace_days: i64,
}

/// Pure grace check (no clock/IO) so it is unit-testable: is a plan confirmed at
/// `verified_at` still within `grace_days` of `now`? Returns the age in days too.
#[must_use]
pub fn plan_within_grace(verified_at: i64, now: i64, grace_days: i64) -> (bool, i64) {
    let age_days = (now - verified_at).max(0) / 86_400;
    (age_days <= grace_days, age_days)
}

/// Resolve the effective plan from the **local cache only** (no network),
/// applying the offline-grace policy. Use this on hot paths (dashboard
/// requests); use [`refresh_effective_plan`] when a live confirmation is
/// acceptable.
///
/// Commercial entitlements (incl. any self-hosted offline Enterprise license)
/// are resolved by the control-plane and reach this client as the cached/live
/// plan — the open engine carries no licensing logic (oss-plane-separation-v1).
#[must_use]
pub fn resolve_effective_plan_cached() -> EffectivePlan {
    let grace_days = PLAN_GRACE_DAYS;
    let Some(cache) = cached_plan() else {
        return EffectivePlan {
            plan: crate::core::billing::Plan::Community,
            supporter_recognition: false,
            source: PlanSource::None,
            verified_at: None,
            grace_days,
        };
    };
    let (fresh, _age) = plan_within_grace(cache.verified_at, now_unix(), grace_days);
    if fresh {
        let selection = crate::core::billing::Plan::parse_selection(&cache.plan);
        EffectivePlan {
            plan: selection.plan,
            supporter_recognition: selection.supporter_recognition,
            source: PlanSource::Cached,
            verified_at: Some(cache.verified_at),
            grace_days,
        }
    } else {
        // Fail closed for paid entitlements once grace lapses. Explicit
        // Community/Trust Core capabilities remain unaffected.
        EffectivePlan {
            plan: crate::core::billing::Plan::Community,
            supporter_recognition: false,
            source: PlanSource::Expired,
            verified_at: Some(cache.verified_at),
            grace_days,
        }
    }
}

/// Authoritative, local-only paid-capability decision from a verified signed
/// entitlement. Unsigned plan caches are display state and never reach here.
#[must_use]
pub fn signed_entitlement_allows(capability: &str) -> bool {
    entitlement_cache::resolve_cached().allows(capability)
}

/// Resolve signed rights for reports without trusting unsigned display plans.
#[must_use]
pub fn resolve_verified_plan_cached() -> VerifiedEffectivePlan {
    entitlement_cache::resolve_cached()
}

/// Refresh signed authority, retaining only cryptographically valid grace.
#[must_use]
pub fn refresh_verified_plan() -> VerifiedEffectivePlan {
    entitlement_cache::refresh()
}

#[cfg(test)]
pub(crate) fn install_test_signed_entitlement_paths(
    trust: PathBuf,
    credentials: PathBuf,
    cache: PathBuf,
    anchors: Vec<(String, [u8; 32])>,
) -> impl Drop {
    entitlement_cache::install_test_paths(trust, credentials, cache, anchors)
}

#[cfg(test)]
pub(crate) fn accept_test_signed_entitlement(account: &str, bytes: &[u8]) {
    entitlement_cache::accept_test_cache(account, bytes);
}

/// Best-effort *live* resolve: try the backend (refreshing the cache on success),
/// otherwise fall back to the cached-with-grace plan. Suitable for explicit
/// commands like `lean-ctx billing status` where a network round-trip is fine.
#[must_use]
pub fn refresh_effective_plan() -> EffectivePlan {
    if is_logged_in()
        && let Ok(plan_str) = fetch_plan()
    {
        let _ = save_plan(&plan_str);
        let selection = crate::core::billing::Plan::parse_selection(&plan_str);
        return EffectivePlan {
            plan: selection.plan,
            supporter_recognition: selection.supporter_recognition,
            source: PlanSource::Live,
            verified_at: Some(now_unix()),
            grace_days: PLAN_GRACE_DAYS,
        };
    }
    resolve_effective_plan_cached()
}

pub fn fetch_plan() -> Result<String, String> {
    let bearer = auth_bearer_token()?;
    let url = format!("{}/api/auth/me", api_url());

    let resp = ureq::get(&url)
        .header("Authorization", &format!("Bearer {bearer}"))
        .call()
        .map_err(|e| format!("Failed to check plan: {e}"))?;

    let resp_body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;

    let json: serde_json::Value =
        serde_json::from_str(&resp_body).map_err(|e| format!("Invalid response: {e}"))?;

    Ok(json["plan"].as_str().unwrap_or("community").to_string())
}

/// Start a Stripe Checkout session for the logged-in account and return the
/// hosted URL to open. `plan` is e.g. `"pro"` or `"team"`; `interval` is
/// `"monthly"` or `"yearly"`. The open backend proxies this to the private
/// billing plane (which returns `503` when billing is not configured).
pub fn start_checkout(plan: &str, interval: &str) -> Result<String, String> {
    let bearer = auth_bearer_token()?;
    let url = format!("{}/api/account/checkout", api_url());
    let body = serde_json::json!({ "plan": plan, "interval": interval });

    let resp = ureq::post(&url)
        .header("Authorization", &format!("Bearer {bearer}"))
        .header("Content-Type", "application/json")
        .send(&serde_json::to_vec(&body).map_err(|e| format!("JSON error: {e}"))?)
        .map_err(|e| format!("Checkout request failed: {e}"))?;

    let resp_body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;

    let json: serde_json::Value =
        serde_json::from_str(&resp_body).map_err(|e| format!("Invalid response: {e}"))?;

    parse_checkout_url(&json)
}

fn parse_checkout_url(json: &serde_json::Value) -> Result<String, String> {
    let raw = json["url"]
        .as_str()
        .ok_or_else(|| "Billing did not return a checkout URL.".to_string())?;
    // The URL is printed to the terminal and handed to the browser opener:
    // control characters (CR/LF, ESC sequences) must never pass through.
    if raw.chars().any(char::is_control) {
        return Err("Billing returned an invalid checkout URL.".to_string());
    }
    let parsed = reqwest::Url::parse(raw)
        .map_err(|_| "Billing returned an invalid checkout URL.".to_string())?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err("Billing returned an invalid checkout URL.".to_string());
    }
    Ok(parsed.to_string())
}

pub fn push_commands(entries: &[serde_json::Value]) -> Result<String, String> {
    let bearer = auth_bearer_token()?;
    let url = format!("{}/api/sync/commands", api_url());
    let body = serde_json::json!({ "commands": entries });
    let resp = ureq::post(&url)
        .header("Authorization", &format!("Bearer {bearer}"))
        .header("Content-Type", "application/json")
        .header("X-Device-Label", &device_label())
        .send(&serde_json::to_vec(&body).map_err(|e| format!("JSON error: {e}"))?)
        .map_err(|e| format!("Push failed: {e}"))?;
    let resp_body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;
    let json: serde_json::Value =
        serde_json::from_str(&resp_body).map_err(|e| format!("Invalid JSON: {e}"))?;
    Ok(format!(
        "{} commands synced",
        json["synced"].as_i64().unwrap_or(0)
    ))
}

pub fn push_cep(entries: &[serde_json::Value]) -> Result<String, String> {
    let bearer = auth_bearer_token()?;
    let url = format!("{}/api/sync/cep", api_url());
    let body = serde_json::json!({ "scores": entries });
    let resp = ureq::post(&url)
        .header("Authorization", &format!("Bearer {bearer}"))
        .header("Content-Type", "application/json")
        .header("X-Device-Label", &device_label())
        .send(&serde_json::to_vec(&body).map_err(|e| format!("JSON error: {e}"))?)
        .map_err(|e| format!("Push failed: {e}"))?;
    let resp_body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;
    let json: serde_json::Value =
        serde_json::from_str(&resp_body).map_err(|e| format!("Invalid JSON: {e}"))?;
    Ok(format!(
        "{} sessions synced",
        json["synced"].as_i64().unwrap_or(0)
    ))
}

pub fn push_gain(entries: &[serde_json::Value]) -> Result<String, String> {
    let bearer = auth_bearer_token()?;
    let url = format!("{}/api/sync/gain", api_url());
    let body = serde_json::json!({ "scores": entries });
    let resp = ureq::post(&url)
        .header("Authorization", &format!("Bearer {bearer}"))
        .header("Content-Type", "application/json")
        .header("X-Device-Label", &device_label())
        .send(&serde_json::to_vec(&body).map_err(|e| format!("JSON error: {e}"))?)
        .map_err(|e| format!("Push failed: {e}"))?;
    let resp_body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;
    let json: serde_json::Value =
        serde_json::from_str(&resp_body).map_err(|e| format!("Invalid JSON: {e}"))?;
    Ok(format!(
        "{} gain scores synced",
        json["synced"].as_i64().unwrap_or(0)
    ))
}

/// Push gotchas as a zero-knowledge vault (GL #467 follow-up): sealed
/// client-side under the `gotcha-vault-v1` HKDF domain — the backend stores
/// ciphertext only and purges the account's legacy plaintext rows on the
/// first vault push.
pub fn push_gotchas(entries: &[serde_json::Value]) -> Result<String, String> {
    let bearer = auth_bearer_token()?;
    let key = gotcha_vault_key()?;
    let blob = crate::core::knowledge_vault::seal(entries, &key).map_err(|e| e.to_string())?;
    let url = format!("{}/api/sync/gotchas", api_url());

    let resp = ureq::post(&url)
        .header("Authorization", &format!("Bearer {bearer}"))
        .header("Content-Type", "application/octet-stream")
        .header("X-Entry-Count", &entries.len().to_string())
        .header("X-Device-Label", &device_label())
        .send(blob.as_slice())
        .map_err(|e| format!("Push failed: {e}"))?;
    let resp_body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;
    let json: serde_json::Value =
        serde_json::from_str(&resp_body).map_err(|e| format!("Invalid JSON: {e}"))?;
    Ok(format!(
        "{} gotchas synced (end-to-end encrypted)",
        json["entry_count"].as_i64().unwrap_or(entries.len() as i64)
    ))
}

/// The account's gotcha-vault key — own HKDF domain (`gotcha-vault-v1`),
/// derivation rule identical to [`knowledge_vault_key`].
fn gotcha_vault_key() -> Result<[u8; 32], String> {
    let api_key = load_api_key().ok_or("Not logged in. Run: lean-ctx login")?;
    if api_key.trim().is_empty() {
        return Err("Not logged in. Run: lean-ctx login".into());
    }
    Ok(crate::core::knowledge_vault::derive_gotcha_vault_key(
        &api_key,
    ))
}

pub fn push_buddy(data: &serde_json::Value) -> Result<String, String> {
    let bearer = auth_bearer_token()?;
    let url = format!("{}/api/sync/buddy", api_url());
    let resp = ureq::post(&url)
        .header("Authorization", &format!("Bearer {bearer}"))
        .header("Content-Type", "application/json")
        .header("X-Device-Label", &device_label())
        .send(&serde_json::to_vec(data).map_err(|e| format!("JSON error: {e}"))?)
        .map_err(|e| format!("Push failed: {e}"))?;
    let resp_body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;
    let _json: serde_json::Value =
        serde_json::from_str(&resp_body).map_err(|e| format!("Invalid JSON: {e}"))?;
    Ok("Buddy synced".to_string())
}

pub fn push_feedback(entries: &[serde_json::Value]) -> Result<String, String> {
    let bearer = auth_bearer_token()?;
    let url = format!("{}/api/sync/feedback", api_url());
    let resp = ureq::post(&url)
        .header("Authorization", &format!("Bearer {bearer}"))
        .header("Content-Type", "application/json")
        .header("X-Device-Label", &device_label())
        .send(&serde_json::to_vec(entries).map_err(|e| format!("JSON error: {e}"))?)
        .map_err(|e| format!("Push failed: {e}"))?;
    let resp_body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;
    let json: serde_json::Value =
        serde_json::from_str(&resp_body).map_err(|e| format!("Invalid JSON: {e}"))?;
    Ok(format!(
        "{} thresholds synced",
        json["synced"].as_i64().unwrap_or(0)
    ))
}

/// The signed-in account's email, for status displays.
pub fn account_email() -> Option<String> {
    load_credentials().map(|c| c.email)
}

/// `GET /api/account/cloud` — the Personal Cloud dashboard payload (entitlement
/// gate, per-bucket sync footprint, buddy, usage totals). Powers
/// `lean-ctx cloud status`, mirroring what leanctx.com/account/cloud shows.
pub fn fetch_account_cloud() -> Result<serde_json::Value, String> {
    let bearer = auth_bearer_token()?;
    let url = format!("{}/api/account/cloud", api_url());

    let resp = ureq::get(&url)
        .header("Authorization", &format!("Bearer {bearer}"))
        .call()
        .map_err(|e| format!("Status fetch failed: {e}"))?;

    let resp_body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;

    serde_json::from_str(&resp_body).map_err(|e| format!("Invalid JSON: {e}"))
}

/// Pull the knowledge store: vault-first (encrypted blob, decrypted locally),
/// with a legacy plaintext fallback for accounts that never pushed a vault.
pub fn pull_knowledge() -> Result<Vec<serde_json::Value>, String> {
    let bearer = auth_bearer_token()?;
    let url = format!("{}/api/sync/knowledge", api_url());

    // Vault path (GL #467).
    match ureq::get(&url)
        .header("Authorization", &format!("Bearer {bearer}"))
        .header("Accept", "application/octet-stream")
        .call()
    {
        Ok(resp) => {
            let is_blob = resp
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.starts_with("application/octet-stream"));
            if is_blob {
                let mut blob = Vec::new();
                use std::io::Read;
                resp.into_body()
                    .into_reader()
                    .read_to_end(&mut blob)
                    .map_err(|e| format!("Failed to read vault: {e}"))?;
                let key = knowledge_vault_key()?;
                return crate::core::knowledge_vault::open(&blob, &key).map_err(|e| e.to_string());
            }
            // Pre-vault server ignored the Accept header and answered with
            // the legacy JSON listing — parse it directly.
            let body = resp
                .into_body()
                .read_to_string()
                .map_err(|e| format!("Failed to read response: {e}"))?;
            return serde_json::from_str(&body).map_err(|e| format!("Invalid JSON: {e}"));
        }
        // No vault yet → fall through to the legacy listing.
        Err(ureq::Error::StatusCode(404)) => {}
        Err(e) => return Err(format!("Pull failed: {e}")),
    }

    let resp = ureq::get(&url)
        .header("Authorization", &format!("Bearer {bearer}"))
        .call()
        .map_err(|e| format!("Pull failed: {e}"))?;

    let resp_body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;

    let entries: Vec<serde_json::Value> =
        serde_json::from_str(&resp_body).map_err(|e| format!("Invalid JSON: {e}"))?;

    Ok(entries)
}

// ── Hosted Personal Index (GL #392) ──────────────────────────────────────────
// Contract: docs/contracts/hosted-personal-index-v1.md. Bundles are encrypted
// client-side (core::index_bundle); the backend only ever sees ciphertext.

/// The account's bundle encryption key, HKDF-derived from the stable API key
/// (never from the rotating OAuth token — the key must be identical on every
/// logged-in device).
fn index_bundle_key() -> Result<[u8; 32], String> {
    let api_key = load_api_key().ok_or("Not logged in. Run: lean-ctx login")?;
    if api_key.trim().is_empty() {
        return Err("Not logged in. Run: lean-ctx login".into());
    }
    Ok(crate::core::index_bundle::derive_key(&api_key))
}

/// Pack, encrypt and upload the project's index bundle.
/// Returns `(project_hash, encrypted_size_bytes)`.
pub fn push_index_bundle(project_root: &std::path::Path) -> Result<(String, u64), String> {
    let (container, manifest) =
        crate::core::index_bundle::pack(project_root).map_err(|e| e.to_string())?;
    let blob = crate::core::index_bundle::encrypt(&container, &index_bundle_key()?)
        .map_err(|e| e.to_string())?;

    let bearer = auth_bearer_token()?;
    let url = format!("{}/api/sync/index/{}", api_url(), manifest.project_hash);
    let resp = ureq::put(&url)
        .header("Authorization", &format!("Bearer {bearer}"))
        .header("Content-Type", "application/octet-stream")
        .header("X-Device-Label", &device_label())
        .send(blob.as_slice())
        .map_err(|e| match e {
            ureq::Error::StatusCode(402) => "Hosted index requires lean-ctx Pro. \
                 Run: lean-ctx cloud upgrade --plan pro"
                .to_string(),
            ureq::Error::StatusCode(413) => {
                "Quota exceeded — the push was blocked (nothing is billed). \
                 Free space with `lean-ctx sync index status` / delete, then retry."
                    .to_string()
            }
            other => format!("Push failed: {other}"),
        })?;

    let body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;
    let _ack: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("Invalid JSON: {e}"))?;
    Ok((manifest.project_hash, blob.len() as u64))
}

/// Download, decrypt and unpack the hosted bundle for this project.
/// Returns the bundle manifest on success.
pub fn pull_index_bundle(
    project_root: &std::path::Path,
) -> Result<crate::core::index_bundle::BundleManifest, String> {
    let project_hash = crate::core::index_namespace::namespace_hash(project_root);
    let bearer = auth_bearer_token()?;
    let url = format!("{}/api/sync/index/{project_hash}", api_url());

    let resp = ureq::get(&url)
        .header("Authorization", &format!("Bearer {bearer}"))
        .call()
        .map_err(|e| match e {
            ureq::Error::StatusCode(404) => format!(
                "No hosted index for this project yet ({project_hash}). \
                 Push one from a device with a built index: lean-ctx sync index push"
            ),
            ureq::Error::StatusCode(402) => "Hosted index requires lean-ctx Pro. \
                 Run: lean-ctx cloud upgrade --plan pro"
                .to_string(),
            other => format!("Pull failed: {other}"),
        })?;

    let mut blob = Vec::new();
    use std::io::Read;
    resp.into_body()
        .into_reader()
        .read_to_end(&mut blob)
        .map_err(|e| format!("Failed to read bundle: {e}"))?;

    let container = crate::core::index_bundle::decrypt(&blob, &index_bundle_key()?)
        .map_err(|e| e.to_string())?;
    crate::core::index_bundle::unpack(project_root, &container).map_err(|e| e.to_string())
}

/// `GET /api/sync/index` — hosted-bucket listing + quota usage for the account.
pub fn index_bundle_status() -> Result<serde_json::Value, String> {
    let bearer = auth_bearer_token()?;
    let url = format!("{}/api/sync/index", api_url());
    let resp = ureq::get(&url)
        .header("Authorization", &format!("Bearer {bearer}"))
        .call()
        .map_err(|e| format!("Status fetch failed: {e}"))?;
    let body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;
    serde_json::from_str(&body).map_err(|e| format!("Invalid JSON: {e}"))
}

#[cfg(test)]
#[path = "cloud_client/inline_tests.rs"]
mod tests;
