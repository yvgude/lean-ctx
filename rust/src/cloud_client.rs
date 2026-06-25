use std::path::PathBuf;

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

#[must_use]
pub fn api_url() -> String {
    std::env::var("LEAN_CTX_API_URL").unwrap_or_else(|_| "https://api.leanctx.com".to_string())
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

#[must_use]
pub fn load_api_key() -> Option<String> {
    load_credentials().map(|c| c.api_key)
}

#[must_use]
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

/// This machine's display label for the device overview (GL #387): the
/// hostname, attached as `X-Device-Label` to every sync push. Display
/// metadata only — the server treats it as an opaque, sanitized string and
/// silently skips tracking when it is empty.
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
    let mut body = serde_json::json!({ "email": email });
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
    let body = serde_json::json!({ "email": email, "password": password });

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

/// Result of a successful Wrapped publish (`POST /api/wrapped`). The `edit_token` is returned
/// (and must be stored to delete/claim later) only on a *fresh* insert; on a signed re-publish
/// the server updates the existing card in place and omits it (the client keeps the stored one).
#[derive(serde::Deserialize)]
pub struct PublishedCard {
    pub id: String,
    #[serde(default)]
    pub edit_token: Option<String>,
    pub url: String,
}

/// Publish a whitelisted Wrapped payload. Accepts either a bare payload (legacy anonymous) or a
/// signed envelope `{payload_json, public_key, signature}` (login-less identity → server upsert).
/// No account auth; the server rate-limits per IP. Contract: `docs/contracts/wrapped-permalink-v1.md`.
pub fn publish_wrapped(payload: &serde_json::Value) -> Result<PublishedCard, String> {
    let url = format!("{}/api/wrapped", api_url());

    let resp = ureq::post(&url)
        .header("Content-Type", "application/json")
        .send(&serde_json::to_vec(payload).map_err(|e| format!("JSON error: {e}"))?)
        .map_err(|e| format!("Publish failed: {e}"))?;

    let resp_body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;

    serde_json::from_str(&resp_body).map_err(|e| format!("Invalid response: {e}"))
}

/// Delete a previously published card using its one-time `edit_token` (sent as `X-Edit-Token`).
pub fn unpublish_wrapped(id: &str, edit_token: &str) -> Result<(), String> {
    let url = format!("{}/api/wrapped/{id}", api_url());

    ureq::delete(&url)
        .header("X-Edit-Token", edit_token)
        .call()
        .map_err(|e| format!("Unpublish failed: {e}"))?;
    Ok(())
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

#[must_use]
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

#[must_use]
pub fn is_cloud_user() -> bool {
    let path = config_dir().join("plan.txt");
    std::fs::read_to_string(path).is_ok_and(|p| matches!(p.trim(), "cloud" | "pro"))
}

/// Days a cached plan keeps granting its hosted entitlements while the billing
/// backend is unreachable. Generous on purpose: a network blip or a weekend
/// offline must never silently demote a paying user to Free.
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
#[must_use]
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
    /// Cached confirmation is past the grace window → demoted to Free.
    Expired,
    /// No cached plan at all (never logged in / never synced) → Free.
    None,
}

/// A resolved plan plus its provenance. The plan here is only ever used for
/// *display* and for gating **hosted** surfaces — it never gates a local
/// capability (Local-Free Invariant; the local engine has no entitlement checks).
#[derive(Debug, Clone)]
pub struct EffectivePlan {
    pub plan: crate::core::billing::Plan,
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
            plan: crate::core::billing::Plan::Free,
            source: PlanSource::None,
            verified_at: None,
            grace_days,
        };
    };
    let (fresh, _age) = plan_within_grace(cache.verified_at, now_unix(), grace_days);
    if fresh {
        EffectivePlan {
            plan: crate::core::billing::Plan::parse(&cache.plan),
            source: PlanSource::Cached,
            verified_at: Some(cache.verified_at),
            grace_days,
        }
    } else {
        // Fail closed for *hosted* entitlements once grace lapses. Local features
        // remain unaffected — they are never gated.
        EffectivePlan {
            plan: crate::core::billing::Plan::Free,
            source: PlanSource::Expired,
            verified_at: Some(cache.verified_at),
            grace_days,
        }
    }
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
        return EffectivePlan {
            plan: crate::core::billing::Plan::parse(&plan_str),
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

    Ok(json["plan"].as_str().unwrap_or("free").to_string())
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

    json["url"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "Billing did not return a checkout URL.".to_string())
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
#[must_use]
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
            ureq::Error::StatusCode(402) => {
                "Hosted index requires lean-ctx Pro. Run: lean-ctx upgrade".to_string()
            }
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
            ureq::Error::StatusCode(402) => {
                "Hosted index requires lean-ctx Pro. Run: lean-ctx upgrade".to_string()
            }
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
mod tests {
    use super::*;
    use crate::core::billing::Plan;
    // Only the `#[cfg(unix)]` credential-permission tests still take the env lock
    // directly; the plan-resolver tests use `isolated_data_dir()` (which locks
    // internally). Gating the import keeps the Windows cross-compile warning-free.
    #[cfg(unix)]
    use crate::core::data_dir::test_env_lock;

    #[test]
    fn grace_window_boundaries_are_inclusive_and_skew_safe() {
        let now = 1_000_000_000;
        let day = 86_400;
        assert_eq!(plan_within_grace(now, now, 14), (true, 0));
        // Exactly at the edge stays valid (inclusive).
        assert_eq!(plan_within_grace(now - 14 * day, now, 14), (true, 14));
        // One day past → expired.
        assert_eq!(plan_within_grace(now - 15 * day, now, 14), (false, 15));
        // Clock skew (future timestamp) is clamped to age 0, never negative.
        assert_eq!(plan_within_grace(now + day, now, 14), (true, 0));
    }

    #[test]
    fn plan_cache_roundtrips_through_json() {
        let c = PlanCache {
            plan: "pro".into(),
            verified_at: 42,
        };
        let back: PlanCache = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back.plan, "pro");
        assert_eq!(back.verified_at, 42);
    }

    #[test]
    fn cached_resolve_grants_within_grace_then_expires_to_free() {
        // Isolate all dirs (config + cache) so the resolver reads only the cache
        // this test writes, not a developer's real plan cache.
        let _iso = crate::core::data_dir::isolated_data_dir();

        // A fresh save is served from cache, within grace, at full plan.
        save_plan("pro").unwrap();
        let eff = resolve_effective_plan_cached();
        assert_eq!(eff.plan, Plan::Pro);
        assert_eq!(eff.source, PlanSource::Cached);

        // Backdate beyond grace → hosted entitlements fail closed to Free.
        let stale = PlanCache {
            plan: "pro".into(),
            verified_at: now_unix() - (PLAN_GRACE_DAYS + 1) * 86_400,
        };
        std::fs::write(plan_cache_path(), serde_json::to_string(&stale).unwrap()).unwrap();
        let eff = resolve_effective_plan_cached();
        assert_eq!(eff.plan, Plan::Free);
        assert_eq!(eff.source, PlanSource::Expired);
    }

    #[test]
    fn no_cache_resolves_to_free_none() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let eff = resolve_effective_plan_cached();
        assert_eq!(eff.plan, Plan::Free);
        assert_eq!(eff.source, PlanSource::None);
    }

    // P0-2 (#414): credentials must be owner-only on disk.
    #[cfg(unix)]
    #[test]
    fn credentials_are_written_owner_only_and_atomic() {
        use std::os::unix::fs::PermissionsExt;
        let _env = test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", tmp.path());

        save_credentials("sk-test-key", "user-1", "a@b.c").unwrap();

        let path = credentials_path();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "credentials.json must be 0o600");

        let dir_mode = std::fs::metadata(config_dir())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            dir_mode & 0o077,
            0,
            "cloud dir must not be group/world accessible"
        );

        // No tmp file leftovers from the atomic write.
        let leftovers: Vec<_> = std::fs::read_dir(config_dir())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "atomic write must not leak tmp files");

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }

    // P0-2 (#414): pre-existing world-readable credentials are tightened on load.
    #[cfg(unix)]
    #[test]
    fn loose_credential_permissions_are_tightened_on_load() {
        use std::os::unix::fs::PermissionsExt;
        let _env = test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", tmp.path());

        std::fs::create_dir_all(config_dir()).unwrap();
        let path = credentials_path();
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let _ = load_credentials();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "legacy file must be tightened to 0o600"
        );

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }

    #[test]
    fn legacy_plan_txt_is_migrated_but_treated_as_stale() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        // Only the legacy flat file exists (no timestamp) → past grace until refresh.
        std::fs::create_dir_all(config_dir()).unwrap();
        std::fs::write(config_dir().join("plan.txt"), "team").unwrap();
        let cache = cached_plan().unwrap();
        assert_eq!(cache.plan, "team");
        assert_eq!(cache.verified_at, 0);
        assert_eq!(resolve_effective_plan_cached().source, PlanSource::Expired);
    }
}
