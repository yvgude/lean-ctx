use std::path::PathBuf;

fn config_dir() -> PathBuf {
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
}

pub fn save_credentials(api_key: &str, user_id: &str, email: &str) -> std::io::Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    let creds = Credentials {
        api_key: api_key.to_string(),
        user_id: user_id.to_string(),
        email: email.to_string(),
    };
    let json = serde_json::to_string_pretty(&creds).map_err(std::io::Error::other)?;
    std::fs::write(credentials_path(), json)
}

pub fn load_api_key() -> Option<String> {
    let data = std::fs::read_to_string(credentials_path()).ok()?;
    let creds: Credentials = serde_json::from_str(&data).ok()?;
    Some(creds.api_key)
}

pub fn is_logged_in() -> bool {
    load_api_key().is_some()
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
        .send(serde_json::to_vec(&body).unwrap().as_slice())
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
        .send(serde_json::to_vec(&body).unwrap().as_slice())
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
        .send(serde_json::to_vec(&body).unwrap().as_slice())
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
    let api_key = load_api_key().ok_or("Not logged in. Run: lean-ctx login")?;
    let url = format!("{}/api/stats", api_url());

    let body = serde_json::json!({ "stats": stats });

    let resp = ureq::post(&url)
        .header("Authorization", &format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .send(serde_json::to_vec(&body).unwrap().as_slice())
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
        .send(serde_json::to_vec(&body).unwrap().as_slice())
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
    let api_key = load_api_key().ok_or("Not logged in. Run: lean-ctx login")?;
    let url = format!("{}/api/sync/knowledge", api_url());

    let body = serde_json::json!({ "entries": entries });

    let resp = ureq::post(&url)
        .header("Authorization", &format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .send(serde_json::to_vec(&body).unwrap().as_slice())
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
    let api_key = load_api_key().ok_or("Not logged in. Run: lean-ctx login <email>")?;
    let url = format!("{}/api/cloud/models", api_url());

    let resp = ureq::get(&url)
        .header("Authorization", &format!("Bearer {api_key}"))
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
    std::fs::read_to_string(path)
        .map(|p| matches!(p.trim(), "cloud" | "pro"))
        .unwrap_or(false)
}

pub fn save_plan(plan: &str) -> std::io::Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("plan.txt"), plan)
}

pub fn fetch_plan() -> Result<String, String> {
    let api_key = load_api_key().ok_or("Not logged in")?;
    let url = format!("{}/api/auth/me", api_url());

    let resp = ureq::get(&url)
        .header("Authorization", &format!("Bearer {api_key}"))
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
    let api_key = load_api_key().ok_or("Not logged in. Run: lean-ctx login")?;
    let url = format!("{}/api/sync/commands", api_url());
    let body = serde_json::json!({ "commands": entries });
    let resp = ureq::post(&url)
        .header("Authorization", &format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .send(serde_json::to_vec(&body).unwrap().as_slice())
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
    let api_key = load_api_key().ok_or("Not logged in. Run: lean-ctx login")?;
    let url = format!("{}/api/sync/cep", api_url());
    let body = serde_json::json!({ "scores": entries });
    let resp = ureq::post(&url)
        .header("Authorization", &format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .send(serde_json::to_vec(&body).unwrap().as_slice())
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
    let api_key = load_api_key().ok_or("Not logged in. Run: lean-ctx login")?;
    let url = format!("{}/api/sync/gain", api_url());
    let body = serde_json::json!({ "scores": entries });
    let resp = ureq::post(&url)
        .header("Authorization", &format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .send(serde_json::to_vec(&body).unwrap().as_slice())
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
    let api_key = load_api_key().ok_or("Not logged in. Run: lean-ctx login")?;
    let url = format!("{}/api/sync/gotchas", api_url());
    let body = serde_json::json!({ "gotchas": entries });
    let resp = ureq::post(&url)
        .header("Authorization", &format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .send(serde_json::to_vec(&body).unwrap().as_slice())
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
    let api_key = load_api_key().ok_or("Not logged in. Run: lean-ctx login")?;
    let url = format!("{}/api/sync/buddy", api_url());
    let resp = ureq::post(&url)
        .header("Authorization", &format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .send(serde_json::to_vec(data).unwrap().as_slice())
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
    let api_key = load_api_key().ok_or("Not logged in. Run: lean-ctx login")?;
    let url = format!("{}/api/sync/feedback", api_url());
    let resp = ureq::post(&url)
        .header("Authorization", &format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .send(serde_json::to_vec(entries).unwrap().as_slice())
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
    let api_key = load_api_key().ok_or("Not logged in. Run: lean-ctx login")?;
    let url = format!("{}/api/sync/knowledge", api_url());

    let resp = ureq::get(&url)
        .header("Authorization", &format!("Bearer {api_key}"))
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
