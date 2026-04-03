use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub email: String,
    pub api_key: String,
    pub plan: String,
    pub is_admin: bool,
    pub stripe_customer_id: Option<String>,
    pub stripe_subscription_id: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DayStats {
    pub date: String,
    pub tokens_original: i64,
    pub tokens_compressed: i64,
    pub tokens_saved: i64,
    pub tool_calls: i64,
    pub cache_hits: i64,
    pub cache_misses: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Team {
    pub id: String,
    pub name: String,
    pub owner_id: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMember {
    pub team_id: String,
    pub user_id: String,
    pub role: String,
    pub email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedKnowledgeEntry {
    pub category: String,
    pub key: String,
    pub value: String,
    pub updated_by: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectiveEntry {
    pub file_ext: String,
    pub size_bucket: String,
    pub best_mode: String,
    pub compression_ratio: f64,
    pub language: Option<String>,
}
