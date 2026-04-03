use axum::extract::Request;
use axum::http::StatusCode;
use sha2::{Digest, Sha256};

use crate::db::{self, DbPool};
use crate::db::schema::User;

pub fn hash_api_key(key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    hex::encode(hasher.finalize())
}

pub fn generate_api_key() -> String {
    format!("lctx_{}", uuid::Uuid::new_v4().to_string().replace('-', ""))
}

pub fn extract_api_key(req: &Request) -> Option<String> {
    req.headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.to_string())
}

pub fn authenticate(db: &DbPool, api_key: &str) -> Option<User> {
    let hash = hash_api_key(api_key);
    db::queries::find_user_by_api_key_hash(db, &hash)
}

pub fn require_plan(user: &User, required: &str) -> Result<(), StatusCode> {
    let plan_level = match user.plan.as_str() {
        "team" => 3,
        "pro" => 2,
        "free" => 1,
        _ => 0,
    };
    let required_level = match required {
        "team" => 3,
        "pro" => 2,
        "free" => 1,
        _ => 0,
    };
    if plan_level >= required_level {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}
