use deadpool_postgres::{Manager, ManagerConfig, Pool, RecyclingMethod};
use tokio_postgres::NoTls;

pub type DbPool = Pool;

pub fn pool_from_database_url(database_url: &str) -> anyhow::Result<DbPool> {
    let pg_cfg: tokio_postgres::Config = database_url.parse()?;
    let mgr_config = ManagerConfig {
        recycling_method: RecyclingMethod::Fast,
    };
    let mgr = Manager::from_config(pg_cfg, NoTls, mgr_config);
    Ok(Pool::builder(mgr).max_size(16).build()?)
}

pub async fn init_schema(pool: &DbPool) -> anyhow::Result<()> {
    let client = pool.get().await?;

    client
        .batch_execute(
            r#"
CREATE TABLE IF NOT EXISTS users (
  id UUID PRIMARY KEY,
  email TEXT NOT NULL UNIQUE,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS api_keys (
  id UUID PRIMARY KEY,
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  api_key_sha256 TEXT NOT NULL UNIQUE,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  last_used_at TIMESTAMPTZ
);

CREATE TABLE IF NOT EXISTS stats_daily (
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  date DATE NOT NULL,
  tokens_original BIGINT NOT NULL DEFAULT 0,
  tokens_compressed BIGINT NOT NULL DEFAULT 0,
  tokens_saved BIGINT NOT NULL DEFAULT 0,
  tool_calls BIGINT NOT NULL DEFAULT 0,
  cache_hits BIGINT NOT NULL DEFAULT 0,
  cache_misses BIGINT NOT NULL DEFAULT 0,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (user_id, date)
);

CREATE TABLE IF NOT EXISTS knowledge_entries (
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  category TEXT NOT NULL,
  key TEXT NOT NULL,
  value TEXT NOT NULL,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (user_id, category, key)
);

CREATE TABLE IF NOT EXISTS contribute_entries (
  id UUID PRIMARY KEY,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  file_ext TEXT NOT NULL,
  size_bucket TEXT NOT NULL,
  best_mode TEXT NOT NULL,
  compression_ratio DOUBLE PRECISION NOT NULL,
  device_hash TEXT
);

CREATE TABLE IF NOT EXISTS magic_links (
  token_sha256 TEXT PRIMARY KEY,
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  expires_at TIMESTAMPTZ NOT NULL,
  consumed_at TIMESTAMPTZ
);

CREATE TABLE IF NOT EXISTS models_snapshot (
  id UUID PRIMARY KEY,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  payload_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS teams (
  id UUID PRIMARY KEY,
  name TEXT NOT NULL,
  owner_id UUID NOT NULL REFERENCES users(id),
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS user_profiles (
  user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  display_hash TEXT NOT NULL UNIQUE,
  username TEXT,
  total_tokens_saved BIGINT NOT NULL DEFAULT 0,
  badge_flags INTEGER NOT NULL DEFAULT 0,
  invite_code TEXT NOT NULL UNIQUE,
  invited_by UUID REFERENCES users(id),
  team_id UUID REFERENCES teams(id) ON DELETE SET NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS global_counters (
  key TEXT PRIMARY KEY,
  value BIGINT NOT NULL DEFAULT 0,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

INSERT INTO global_counters (key, value) VALUES ('total_tokens_saved', 0)
  ON CONFLICT (key) DO NOTHING;
INSERT INTO global_counters (key, value) VALUES ('total_users', 0)
  ON CONFLICT (key) DO NOTHING;
INSERT INTO global_counters (key, value) VALUES ('total_contributions', 0)
  ON CONFLICT (key) DO NOTHING;

CREATE UNIQUE INDEX IF NOT EXISTS idx_contribute_device_day
  ON contribute_entries (device_hash, (created_at::date))
  WHERE device_hash IS NOT NULL;
"#,
        )
        .await?;

    Ok(())
}

