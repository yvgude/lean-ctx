use std::path::PathBuf;

fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("LEAN_CTX_DATA_DIR") {
        return PathBuf::from(dir).join("cloud");
    }
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".lean-ctx").join("cloud")
}

fn credentials_path() -> PathBuf {
    config_dir().join("credentials.json")
}

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
    let data = std::fs::read_to_string(credentials_path()).ok()?;
    serde_json::from_str(&data).ok()
}

fn write_credentials(creds: &Credentials) -> std::io::Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    let json = serde_json::to_string_pretty(creds).map_err(std::io::Error::other)?;
    std::fs::write(credentials_path(), json)
}

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
        ) {
            if exp > now + 10 {
                return Ok(token);
            }
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

pub fn push_knowledge(entries: &[serde_json::Value]) -> Result<String, String> {
    let bearer = auth_bearer_token()?;
    let url = format!("{}/api/sync/knowledge", api_url());

    let body = serde_json::json!({ "entries": entries });

    let resp = ureq::post(&url)
        .header("Authorization", &format!("Bearer {bearer}"))
        .header("Content-Type", "application/json")
        .send(&serde_json::to_vec(&body).map_err(|e| format!("JSON error: {e}"))?)
        .map_err(|e| format!("Push failed: {e}"))?;

    let resp_body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;

    let json: serde_json::Value =
        serde_json::from_str(&resp_body).map_err(|e| format!("Invalid JSON: {e}"))?;

    Ok(format!(
        "{} entries synced",
        json["synced"].as_i64().unwrap_or(0)
    ))
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

pub fn is_cloud_user() -> bool {
    let path = config_dir().join("plan.txt");
    std::fs::read_to_string(path).is_ok_and(|p| matches!(p.trim(), "cloud" | "pro"))
}

pub fn save_plan(plan: &str) -> std::io::Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("plan.txt"), plan)
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

pub fn push_commands(entries: &[serde_json::Value]) -> Result<String, String> {
    let bearer = auth_bearer_token()?;
    let url = format!("{}/api/sync/commands", api_url());
    let body = serde_json::json!({ "commands": entries });
    let resp = ureq::post(&url)
        .header("Authorization", &format!("Bearer {bearer}"))
        .header("Content-Type", "application/json")
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

pub fn push_gotchas(entries: &[serde_json::Value]) -> Result<String, String> {
    let bearer = auth_bearer_token()?;
    let url = format!("{}/api/sync/gotchas", api_url());
    let body = serde_json::json!({ "gotchas": entries });
    let resp = ureq::post(&url)
        .header("Authorization", &format!("Bearer {bearer}"))
        .header("Content-Type", "application/json")
        .send(&serde_json::to_vec(&body).map_err(|e| format!("JSON error: {e}"))?)
        .map_err(|e| format!("Push failed: {e}"))?;
    let resp_body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;
    let json: serde_json::Value =
        serde_json::from_str(&resp_body).map_err(|e| format!("Invalid JSON: {e}"))?;
    Ok(format!(
        "{} gotchas synced",
        json["synced"].as_i64().unwrap_or(0)
    ))
}

pub fn push_buddy(data: &serde_json::Value) -> Result<String, String> {
    let bearer = auth_bearer_token()?;
    let url = format!("{}/api/sync/buddy", api_url());
    let resp = ureq::post(&url)
        .header("Authorization", &format!("Bearer {bearer}"))
        .header("Content-Type", "application/json")
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

pub fn pull_knowledge() -> Result<Vec<serde_json::Value>, String> {
    let bearer = auth_bearer_token()?;
    let url = format!("{}/api/sync/knowledge", api_url());

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
