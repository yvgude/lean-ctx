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
            r"
CREATE TABLE IF NOT EXISTS users (
  id UUID PRIMARY KEY,
  email TEXT NOT NULL UNIQUE,
  password_hash TEXT,
  email_verified_at TIMESTAMPTZ,
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
  compression_ratio DOUBLE PRECISION NOT NULL
);

CREATE TABLE IF NOT EXISTS magic_links (
  token_sha256 TEXT PRIMARY KEY,
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  expires_at TIMESTAMPTZ NOT NULL,
  consumed_at TIMESTAMPTZ
);

CREATE TABLE IF NOT EXISTS email_verifications (
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

CREATE TABLE IF NOT EXISTS command_stats (
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  command TEXT NOT NULL,
  source TEXT NOT NULL DEFAULT 'unknown',
  count BIGINT NOT NULL DEFAULT 0,
  input_tokens BIGINT NOT NULL DEFAULT 0,
  output_tokens BIGINT NOT NULL DEFAULT 0,
  tokens_saved BIGINT NOT NULL DEFAULT 0,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (user_id, command)
);

CREATE TABLE IF NOT EXISTS cep_scores (
  id UUID PRIMARY KEY,
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  recorded_at TIMESTAMPTZ NOT NULL,
  score DOUBLE PRECISION NOT NULL,
  cache_hit_rate DOUBLE PRECISION,
  mode_diversity DOUBLE PRECISION,
  compression_rate DOUBLE PRECISION,
  tool_calls BIGINT,
  tokens_saved BIGINT,
  complexity DOUBLE PRECISION
);

CREATE TABLE IF NOT EXISTS gain_scores (
  id UUID PRIMARY KEY,
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  recorded_at TIMESTAMPTZ NOT NULL,
  total DOUBLE PRECISION NOT NULL,
  compression DOUBLE PRECISION NOT NULL,
  cost_efficiency DOUBLE PRECISION NOT NULL,
  quality DOUBLE PRECISION NOT NULL,
  consistency DOUBLE PRECISION NOT NULL,
  trend TEXT,
  avoided_usd DOUBLE PRECISION,
  tool_spend_usd DOUBLE PRECISION,
  model_key TEXT
);

CREATE TABLE IF NOT EXISTS gotchas (
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  pattern TEXT NOT NULL,
  fix TEXT NOT NULL,
  severity TEXT,
  category TEXT,
  occurrences BIGINT NOT NULL DEFAULT 0,
  prevented_count BIGINT NOT NULL DEFAULT 0,
  confidence DOUBLE PRECISION,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (user_id, pattern)
);

CREATE TABLE IF NOT EXISTS buddy_state (
  user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  name TEXT,
  species TEXT,
  level INTEGER NOT NULL DEFAULT 1,
  xp BIGINT NOT NULL DEFAULT 0,
  mood TEXT,
  streak INTEGER NOT NULL DEFAULT 0,
  rarity TEXT,
  state_json TEXT,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS feedback_thresholds (
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  language TEXT NOT NULL,
  entropy DOUBLE PRECISION NOT NULL,
  jaccard DOUBLE PRECISION NOT NULL,
  sample_count INTEGER NOT NULL DEFAULT 0,
  avg_efficiency DOUBLE PRECISION NOT NULL DEFAULT 0.0,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (user_id, language)
);

DROP TABLE IF EXISTS team_invites CASCADE;
DROP TABLE IF EXISTS team_members CASCADE;
DROP TABLE IF EXISTS teams CASCADE;

DO $$ BEGIN
  ALTER TABLE users ADD COLUMN IF NOT EXISTS password_hash TEXT;
  ALTER TABLE users ADD COLUMN IF NOT EXISTS email_verified_at TIMESTAMPTZ;
  ALTER TABLE buddy_state ADD COLUMN IF NOT EXISTS state_json TEXT;
EXCEPTION WHEN others THEN NULL;
END $$;
",
        )
        .await?;

    Ok(())
}
