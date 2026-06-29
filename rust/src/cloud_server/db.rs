use deadpool_postgres::{Manager, ManagerConfig, Pool, RecyclingMethod};
use tokio_postgres::NoTls;

pub(super) type DbPool = Pool;

pub(super) fn pool_from_database_url(database_url: &str) -> anyhow::Result<DbPool> {
    let pg_cfg: tokio_postgres::Config = database_url.parse()?;
    let mgr_config = ManagerConfig {
        recycling_method: RecyclingMethod::Fast,
    };
    let mgr = Manager::from_config(pg_cfg, NoTls, mgr_config);
    Ok(Pool::builder(mgr).max_size(16).build()?)
}

pub(super) async fn init_schema(pool: &DbPool) -> anyhow::Result<()> {
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

CREATE TABLE IF NOT EXISTS oauth_clients (
  client_id UUID PRIMARY KEY,
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  client_name TEXT,
  client_secret_sha256 TEXT NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  revoked_at TIMESTAMPTZ
);

CREATE TABLE IF NOT EXISTS oauth_access_tokens (
  token_sha256 TEXT PRIMARY KEY,
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  client_id UUID NOT NULL REFERENCES oauth_clients(client_id) ON DELETE CASCADE,
  issued_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  expires_at TIMESTAMPTZ NOT NULL,
  last_used_at TIMESTAMPTZ,
  revoked_at TIMESTAMPTZ
);

-- OIDC SSO login flow (GL #482). One row per in-flight authorization
-- round-trip; consumed (deleted) on callback, swept by TTL otherwise. Only
-- hashes of state/handoff tokens are stored.
CREATE TABLE IF NOT EXISTS sso_login_states (
  state_sha256 TEXT PRIMARY KEY,
  email_domain TEXT NOT NULL,
  nonce TEXT NOT NULL,
  pkce_verifier TEXT NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- One-time handoff between the SSO callback redirect and the login page:
-- the api_key never appears in a URL. 60s TTL, single use.
CREATE TABLE IF NOT EXISTS sso_handoff_codes (
  code_sha256 TEXT PRIMARY KEY,
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  api_key TEXT NOT NULL,
  email TEXT NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
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

-- Zero-knowledge knowledge vault (GL #467): one client-side-encrypted blob
-- per account, replacing plaintext knowledge_entries for E2E clients. The
-- server never sees plaintext — entry_count is client-declared display
-- metadata only. Legacy knowledge_entries rows are deleted on first vault
-- push (the client re-encrypts its full local state by construction).
CREATE TABLE IF NOT EXISTS knowledge_blobs (
  user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  blob BYTEA NOT NULL,
  entry_count BIGINT NOT NULL DEFAULT 0,
  sha256 TEXT NOT NULL,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Zero-knowledge gotcha vault (GL #467 follow-up): same construction as
-- knowledge_blobs, own table + own HKDF domain (gotcha-vault-v1). Legacy
-- gotchas rows are deleted on first vault push.
CREATE TABLE IF NOT EXISTS gotcha_blobs (
  user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  blob BYTEA NOT NULL,
  entry_count BIGINT NOT NULL DEFAULT 0,
  sha256 TEXT NOT NULL,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Hosted Personal Index buckets (GL #392): one client-side-encrypted bundle
-- per (account, project). The server never sees plaintext — `bytes` is
-- XChaCha20-Poly1305 ciphertext; `sha256` covers the ciphertext for drift
-- detection. Quota is enforced per account from the plan's hosted_index_mb.
CREATE TABLE IF NOT EXISTS index_bundles (
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  project_hash TEXT NOT NULL,
  bytes BYTEA NOT NULL,
  size_bytes BIGINT NOT NULL,
  sha256 TEXT NOT NULL,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (user_id, project_hash)
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

-- Email digest preferences (GL #386). One row per user, created lazily on the
-- first digest. The opt-out token authorizes the one-click unsubscribe link in
-- every digest (no login required); only its SHA-256 is stored.
CREATE TABLE IF NOT EXISTS email_prefs (
  user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  digest_opt_out BOOLEAN NOT NULL DEFAULT FALSE,
  opt_out_token_sha256 TEXT UNIQUE NOT NULL,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Digest idempotency ledger (GL #386): one row per (user, kind, period) ever
-- sent. INSERT … ON CONFLICT DO NOTHING is the send gate, so a digest goes out
-- at most once per period even across restarts and concurrent ticks.
CREATE TABLE IF NOT EXISTS digest_log (
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL,
  period_key TEXT NOT NULL,
  sent_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (user_id, kind, period_key)
);

-- Device overview (GL #387): one row per (user, device label), upserted as a
-- side effect of every authenticated sync push that carries X-Device-Label.
-- Pure display metadata — labels are client-chosen hostnames, never identity.
CREATE TABLE IF NOT EXISTS devices (
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  device_label TEXT NOT NULL,
  first_seen TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  last_seen TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  last_surface TEXT,
  sync_count BIGINT NOT NULL DEFAULT 0,
  PRIMARY KEY (user_id, device_label)
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
  model_key TEXT,
  navigability DOUBLE PRECISION
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

CREATE TABLE IF NOT EXISTS wrapped_cards (
  id              TEXT PRIMARY KEY,
  edit_token_hash TEXT NOT NULL,
  user_id         UUID NULL REFERENCES users(id) ON DELETE SET NULL,
  payload_json    TEXT NOT NULL,
  created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  ip_hash         TEXT NULL,
  view_count      BIGINT NOT NULL DEFAULT 0,
  leaderboard_opt_in BOOLEAN NOT NULL DEFAULT FALSE,
  tokens_saved    BIGINT NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS wrapped_cards_ip_created ON wrapped_cards (ip_hash, created_at);
CREATE INDEX IF NOT EXISTS wrapped_cards_leaderboard ON wrapped_cards (leaderboard_opt_in, tokens_saved DESC);

-- Login-less publisher identity (sha256 of the client's Ed25519 public key) + period, so a
-- re-publish from the same machine UPSERTs its existing card instead of piling up duplicates.
-- Partial unique index: legacy anonymous rows (publisher_id NULL) never collide.
ALTER TABLE wrapped_cards ADD COLUMN IF NOT EXISTS publisher_id TEXT;
ALTER TABLE wrapped_cards ADD COLUMN IF NOT EXISTS period TEXT;
CREATE UNIQUE INDEX IF NOT EXISTS wrapped_cards_publisher_period
  ON wrapped_cards (publisher_id, period) WHERE publisher_id IS NOT NULL;

DROP TABLE IF EXISTS team_invites CASCADE;
DROP TABLE IF EXISTS team_members CASCADE;
DROP TABLE IF EXISTS teams CASCADE;

DO $$ BEGIN
  ALTER TABLE users ADD COLUMN IF NOT EXISTS password_hash TEXT;
  ALTER TABLE users ADD COLUMN IF NOT EXISTS email_verified_at TIMESTAMPTZ;
  ALTER TABLE buddy_state ADD COLUMN IF NOT EXISTS state_json TEXT;
  ALTER TABLE wrapped_cards ADD COLUMN IF NOT EXISTS leaderboard_opt_in BOOLEAN NOT NULL DEFAULT FALSE;
  ALTER TABLE wrapped_cards ADD COLUMN IF NOT EXISTS tokens_saved BIGINT NOT NULL DEFAULT 0;
  -- Code Health Engine navigability component (#1086); nullable for pre-existing rows.
  ALTER TABLE gain_scores ADD COLUMN IF NOT EXISTS navigability DOUBLE PRECISION;
EXCEPTION WHEN others THEN NULL;
END $$;
",
        )
        .await?;

    Ok(())
}
