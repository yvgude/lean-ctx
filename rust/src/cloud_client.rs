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

pub fn register(email: &str) -> Result<(String, String), String> {
    let url = format!("{}/api/auth/register", api_url());
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

    let api_key = json["api_key"]
        .as_str()
        .ok_or("Missing api_key in response")?
        .to_string();
    let user_id = json["user_id"]
        .as_str()
        .ok_or("Missing user_id in response")?
        .to_string();

    Ok((api_key, user_id))
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
