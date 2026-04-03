use super::schema::*;
use super::DbPool;
use rusqlite::params;

pub fn create_user(db: &DbPool, email: &str, api_key: &str, api_key_hash: &str) -> Result<User, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let conn = db.lock().map_err(|e| e.to_string())?;

    let user_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))
        .unwrap_or(0);
    let is_admin = user_count == 0;

    conn.execute(
        "INSERT INTO users (id, email, api_key, api_key_hash, is_admin) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, email, api_key, api_key_hash, is_admin],
    ).map_err(|e| e.to_string())?;

    Ok(User {
        id,
        email: email.to_string(),
        api_key: api_key.to_string(),
        plan: "free".to_string(),
        is_admin,
        stripe_customer_id: None,
        stripe_subscription_id: None,
        created_at: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    })
}

pub fn find_user_by_api_key_hash(db: &DbPool, hash: &str) -> Option<User> {
    let conn = db.lock().ok()?;
    conn.query_row(
        "SELECT id, email, api_key, plan, is_admin, stripe_customer_id, stripe_subscription_id, created_at FROM users WHERE api_key_hash = ?1",
        params![hash],
        |row| Ok(User {
            id: row.get(0)?,
            email: row.get(1)?,
            api_key: row.get(2)?,
            plan: row.get(3)?,
            is_admin: row.get(4)?,
            stripe_customer_id: row.get(5)?,
            stripe_subscription_id: row.get(6)?,
            created_at: row.get(7)?,
        }),
    ).ok()
}

pub fn find_user_by_email(db: &DbPool, email: &str) -> Option<User> {
    let conn = db.lock().ok()?;
    conn.query_row(
        "SELECT id, email, api_key, plan, is_admin, stripe_customer_id, stripe_subscription_id, created_at FROM users WHERE email = ?1",
        params![email],
        |row| Ok(User {
            id: row.get(0)?,
            email: row.get(1)?,
            api_key: row.get(2)?,
            plan: row.get(3)?,
            is_admin: row.get(4)?,
            stripe_customer_id: row.get(5)?,
            stripe_subscription_id: row.get(6)?,
            created_at: row.get(7)?,
        }),
    ).ok()
}

pub fn update_user_plan(db: &DbPool, user_id: &str, plan: &str, stripe_sub_id: Option<&str>) -> Result<(), String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE users SET plan = ?1, stripe_subscription_id = ?2, updated_at = datetime('now') WHERE id = ?3",
        params![plan, stripe_sub_id, user_id],
    ).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn upsert_stats(db: &DbPool, user_id: &str, stats: &DayStats) -> Result<(), String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO stats (user_id, date, tokens_original, tokens_compressed, tokens_saved, tool_calls, cache_hits, cache_misses)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(user_id, date) DO UPDATE SET
           tokens_original = tokens_original + excluded.tokens_original,
           tokens_compressed = tokens_compressed + excluded.tokens_compressed,
           tokens_saved = tokens_saved + excluded.tokens_saved,
           tool_calls = tool_calls + excluded.tool_calls,
           cache_hits = cache_hits + excluded.cache_hits,
           cache_misses = cache_misses + excluded.cache_misses",
        params![
            user_id, stats.date, stats.tokens_original, stats.tokens_compressed,
            stats.tokens_saved, stats.tool_calls, stats.cache_hits, stats.cache_misses
        ],
    ).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn get_stats(db: &DbPool, user_id: &str, days: i64) -> Vec<DayStats> {
    let conn = match db.lock() {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut stmt = match conn.prepare(
        "SELECT date, tokens_original, tokens_compressed, tokens_saved, tool_calls, cache_hits, cache_misses
         FROM stats WHERE user_id = ?1 AND date >= date('now', ?2) ORDER BY date ASC"
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let days_str = format!("-{days} days");
    stmt.query_map(params![user_id, days_str], |row| {
        Ok(DayStats {
            date: row.get(0)?,
            tokens_original: row.get(1)?,
            tokens_compressed: row.get(2)?,
            tokens_saved: row.get(3)?,
            tool_calls: row.get(4)?,
            cache_hits: row.get(5)?,
            cache_misses: row.get(6)?,
        })
    }).ok()
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

pub fn insert_collective_data(db: &DbPool, entries: &[CollectiveEntry]) -> Result<usize, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    let mut count = 0;
    for entry in entries {
        conn.execute(
            "INSERT INTO collective_data (file_ext, size_bucket, best_mode, compression_ratio, language)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![entry.file_ext, entry.size_bucket, entry.best_mode, entry.compression_ratio, entry.language],
        ).map_err(|e| e.to_string())?;
        count += 1;
    }
    Ok(count)
}
