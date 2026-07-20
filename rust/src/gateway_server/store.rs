//! `usage_events` Postgres store (enterprise#17, baseline fields enterprise#18).
//!
//! One row per measured LLM turn: who (person/team/project, enterprise#11),
//! what (provider/model/tokens), what it cost (priced with the shared
//! `ModelPricing` table) and the counterfactual-baseline inputs that make the
//! success fee provable (`uncompressed_input_tokens`, `reference_model`,
//! `reference_cost_usd`, `is_local` — Doc 08 §2).
//!
//! Schema management follows the repo rule: `init_schema` is idempotent
//! `batch_execute` DDL (`CREATE TABLE IF NOT EXISTS …`), no migration files.
//!
//! The writer consumes the `proxy::usage_sink` stream: bounded channel, spawned
//! task, INSERT per event. Fail-open (enterprise#12): insert errors are logged
//! and counted, never propagated to the request path.

use deadpool_postgres::{Manager, ManagerConfig, Pool, RecyclingMethod};
use tokio_postgres::NoTls;
use tokio_postgres::config::SslMode;

use crate::core::config::BaselineConfig;
use crate::core::gain::model_pricing::ModelPricing;
use crate::proxy::usage::RealUsage;

/// Buffered events between the proxy choke-point and the Postgres writer.
/// Sized for bursts (a full channel drops events, counted in `usage_sink`).
pub const WRITER_QUEUE: usize = 4096;

/// Env var overriding the store pool's `max_size` (chart: `database.poolMaxSize`).
pub const POOL_MAX_SIZE_ENV: &str = "LEAN_CTX_PG_POOL_MAX_SIZE";

/// Default pool size. The writer is a single sequential task (one connection),
/// the rest serves the admin API/report queries — 8 is comfortable for one
/// replica; K8s replicas each get their own pool (load-test: deploy-repo
/// `docs/ops/load-test.md`).
const POOL_MAX_SIZE_DEFAULT: usize = 8;

/// Pool size from `LEAN_CTX_PG_POOL_MAX_SIZE`, clamped to a sane band.
/// Invalid/unset values fall back to the default — a typo can never produce
/// a 1-connection or 10k-connection pool.
fn pool_max_size() -> usize {
    std::env::var(POOL_MAX_SIZE_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .map_or(POOL_MAX_SIZE_DEFAULT, |n| n.clamp(2, 64))
}

/// Builds the store pool from a `DATABASE_URL`, honoring `sslmode` (#54/#58).
///
/// - `sslmode=disable`/`prefer`/unset: plain TCP (the pilot/in-cluster case;
///   `prefer`'s opportunistic upgrade would mask misconfiguration, so it stays
///   plain — deployments that need TLS must say `require`).
/// - `sslmode=require`: rustls with the webpki root store — the managed-
///   Postgres case (Azure/AWS/GCP enforce TLS). Unlike libpq's `require`,
///   certificate and hostname are **always verified** (verify-full rigor);
///   lean-ctx does not implement an unverified-TLS downgrade.
pub fn pool_from_database_url(database_url: &str) -> anyhow::Result<Pool> {
    let pg_cfg: tokio_postgres::Config = database_url.parse()?;
    let mgr_cfg = ManagerConfig {
        recycling_method: RecyclingMethod::Fast,
    };
    let mgr = if wants_tls(&pg_cfg) {
        // The pool is built before the proxy installs the process-default
        // CryptoProvider (#597) — make sure one exists (idempotent).
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let roots = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let tls_cfg = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        Manager::from_config(
            pg_cfg,
            tokio_postgres_rustls::MakeRustlsConnect::new(tls_cfg),
            mgr_cfg,
        )
    } else {
        Manager::from_config(pg_cfg, NoTls, mgr_cfg)
    };
    Ok(Pool::builder(mgr).max_size(pool_max_size()).build()?)
}

/// True when the URL's `sslmode` asks for TLS. tokio-postgres 0.7 models
/// `disable`/`prefer`/`require`; anything else fails URL parsing upstream.
fn wants_tls(cfg: &tokio_postgres::Config) -> bool {
    matches!(cfg.get_ssl_mode(), SslMode::Require)
}

/// Idempotent DDL (Doc 08 §2): `IF NOT EXISTS` only, run on every start.
/// Columns added after the first release ride along as idempotent
/// `ALTER TABLE … ADD COLUMN IF NOT EXISTS` — same rule, no migration files.
const USAGE_EVENTS_DDL: &str = r"
CREATE TABLE IF NOT EXISTS usage_events (
  id                 BIGSERIAL PRIMARY KEY,
  ts                 TIMESTAMPTZ      NOT NULL DEFAULT now(),
  person             TEXT             NOT NULL,
  team               TEXT,
  project            TEXT             NOT NULL,
  tool               TEXT,
  provider           TEXT             NOT NULL,
  model              TEXT             NOT NULL,
  routed_from        TEXT,
  input_tokens       BIGINT           NOT NULL,
  output_tokens      BIGINT           NOT NULL,
  cache_read_tokens  BIGINT           NOT NULL DEFAULT 0,
  cache_write_tokens BIGINT           NOT NULL DEFAULT 0,
  reasoning_tokens   BIGINT           NOT NULL DEFAULT 0,
  cost_usd           DOUBLE PRECISION NOT NULL,
  saved_tokens       BIGINT           NOT NULL DEFAULT 0,
  saved_usd          DOUBLE PRECISION NOT NULL DEFAULT 0,
  -- Avoided-cost baseline for the success fee (enterprise#18, Doc 04 §6):
  uncompressed_input_tokens BIGINT    NOT NULL DEFAULT 0,
  reference_model    TEXT,
  reference_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
  is_local           BOOLEAN          NOT NULL DEFAULT false,
  -- Cost provenance (#1179): provider | shadow | list | live | heuristic.
  cost_source        TEXT             NOT NULL DEFAULT 'list'
);
ALTER TABLE usage_events ADD COLUMN IF NOT EXISTS cost_source TEXT NOT NULL DEFAULT 'list';
CREATE INDEX IF NOT EXISTS idx_usage_events_person_ts  ON usage_events (person, ts);
CREATE INDEX IF NOT EXISTS idx_usage_events_project_ts ON usage_events (project, ts);
CREATE INDEX IF NOT EXISTS idx_usage_events_model_ts   ON usage_events (model, ts);
";

/// Applies the usage-store DDL. Safe to run on every start (idempotent).
pub async fn init_schema(pool: &Pool) -> anyhow::Result<()> {
    let client = pool.get().await?;
    client.batch_execute(USAGE_EVENTS_DDL).await?;
    Ok(())
}

/// One `usage_events` row, fully derived from a finalized [`RealUsage`].
#[derive(Debug, Clone, PartialEq)]
pub struct UsageEvent {
    pub person: String,
    pub team: Option<String>,
    pub project: String,
    pub provider: String,
    pub model: String,
    pub routed_from: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub reasoning_tokens: i64,
    pub cost_usd: f64,
    pub saved_tokens: i64,
    pub saved_usd: f64,
    pub uncompressed_input_tokens: i64,
    pub reference_model: Option<String>,
    pub reference_cost_usd: f64,
    pub is_local: bool,
    /// Where `cost_usd` came from (#1179): `provider` (the response's own
    /// charge), `shadow` (local shadow rate), `live` (current provider price
    /// list), `list` (embedded list price) or `heuristic` (estimate).
    pub cost_source: &'static str,
}

/// Maps a pricing match onto the stored `cost_source` value for table-priced
/// rows. `provider`/`shadow` are decided before pricing is consulted.
fn cost_source_of(kind: crate::core::gain::model_pricing::PricingMatchKind) -> &'static str {
    use crate::core::gain::model_pricing::PricingMatchKind as K;
    match kind {
        K::Exact => "list",
        K::Live => "live",
        K::Alias | K::Heuristic | K::Fallback => "heuristic",
    }
}

/// Identity fallbacks when a request carried no gateway key/tags: the row must
/// still be attributable (`NOT NULL`), and "anonymous/default" is honest about
/// what the gateway knew. Strict deployments make keys mandatory via
/// `proxy_require_token` + gateway-keys, so these appear only in solo mode.
const ANONYMOUS_PERSON: &str = "anonymous";
const DEFAULT_PROJECT: &str = "default";

impl UsageEvent {
    /// Derives the row from a measured turn, pricing both the actual cost and
    /// the compression saving with the shared pricing table, and stamping the
    /// counterfactual baseline (enterprise#15/#18):
    ///
    /// - `cost_usd`, in precedence order (#1179): the provider's own reported
    ///   charge (`usage.cost`, OpenRouter) — except `is_local`, which books
    ///   the transparent `local_shadow_rate` (never $0; Doc 04 §6) — then the
    ///   live/list price table. `cost_source` records which one applied.
    /// - `reference_cost_usd`: the request's **uncompressed** input tokens
    ///   priced at the contract-frozen `reference_model`'s input rate (Doc 08
    ///   §2) — the counterfactual the avoided-cost ledger settles against.
    /// - `saved_usd`: the SEE (compression) component only — saved request
    ///   tokens at the served model's input rate. Full mechanism attribution
    ///   (routing/caching) is the signed ledger's job (wave 4, enterprise#19).
    #[must_use]
    pub fn from_usage(
        usage: &RealUsage,
        pricing: &ModelPricing,
        baseline: &BaselineConfig,
    ) -> Self {
        let wire = usage.wire.as_deref();
        let quote = pricing.quote(Some(&usage.model));
        let is_local = wire.is_some_and(|w| w.is_local);
        #[allow(clippy::cast_precision_loss)]
        let (cost_usd, cost_source) = if is_local {
            let billable = usage.input_tokens
                + usage.output_tokens
                + usage.cache_read_tokens
                + usage.cache_write_tokens;
            (
                baseline.effective_local_shadow_rate() / 1_000_000.0 * billable as f64,
                "shadow",
            )
        } else if let Some(measured) = usage.provider_cost_usd {
            (measured, "provider")
        } else {
            let estimated = quote.cost.estimate_usd(
                usage.input_tokens,
                usage.output_tokens,
                usage.cache_write_tokens,
                usage.cache_read_tokens,
            );
            (estimated, cost_source_of(quote.match_kind))
        };
        let saved_tokens = wire.map_or(0, |w| w.saved_tokens);
        // Input-side saving: input-rate USD per token × saved request tokens.
        #[allow(clippy::cast_precision_loss)]
        let saved_usd = quote.cost.input_per_m / 1_000_000.0 * saved_tokens as f64;

        let uncompressed_input_tokens = wire.map_or(0, |w| w.uncompressed_input_tokens);
        let reference_model = baseline
            .reference_model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_string);
        #[allow(clippy::cast_precision_loss)]
        let reference_cost_usd = reference_model.as_deref().map_or(0.0, |reference| {
            pricing.quote(Some(reference)).cost.input_per_m / 1_000_000.0
                * uncompressed_input_tokens as f64
        });

        Self {
            person: wire
                .and_then(|w| w.person.clone())
                .unwrap_or_else(|| ANONYMOUS_PERSON.to_string()),
            team: wire.and_then(|w| w.team.clone()),
            project: wire
                .and_then(|w| w.project.clone())
                .unwrap_or_else(|| DEFAULT_PROJECT.to_string()),
            provider: wire.map_or_else(String::new, |w| w.provider.clone()),
            model: usage.model.clone(),
            routed_from: wire.and_then(|w| w.routed_from.clone()),
            input_tokens: to_i64(usage.input_tokens),
            output_tokens: to_i64(usage.output_tokens),
            cache_read_tokens: to_i64(usage.cache_read_tokens),
            cache_write_tokens: to_i64(usage.cache_write_tokens),
            reasoning_tokens: to_i64(usage.reasoning_tokens),
            cost_usd,
            saved_tokens: to_i64(saved_tokens),
            saved_usd,
            uncompressed_input_tokens: to_i64(uncompressed_input_tokens),
            reference_model,
            reference_cost_usd,
            is_local,
            cost_source,
        }
    }
}

fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// Inserts one event. Errors bubble to the writer loop, which logs and moves on.
pub async fn insert_event(
    client: &deadpool_postgres::Client,
    e: &UsageEvent,
) -> anyhow::Result<()> {
    client
        .execute(
            "INSERT INTO usage_events \
             (person, team, project, provider, model, routed_from, \
              input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, \
              reasoning_tokens, cost_usd, saved_tokens, saved_usd, \
              uncompressed_input_tokens, reference_model, reference_cost_usd, is_local, \
              cost_source) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19)",
            &[
                &e.person,
                &e.team,
                &e.project,
                &e.provider,
                &e.model,
                &e.routed_from,
                &e.input_tokens,
                &e.output_tokens,
                &e.cache_read_tokens,
                &e.cache_write_tokens,
                &e.reasoning_tokens,
                &e.cost_usd,
                &e.saved_tokens,
                &e.saved_usd,
                &e.uncompressed_input_tokens,
                &e.reference_model,
                &e.reference_cost_usd,
                &e.is_local,
                &e.cost_source,
            ],
        )
        .await?;
    Ok(())
}

/// Current-window spend sums for the budget gate (enterprise#25):
/// per-person spend for the running UTC day and per-project spend for the
/// running UTC month, straight from `usage_events`.
pub async fn budget_window_sums(
    pool: &Pool,
) -> anyhow::Result<(
    std::collections::HashMap<String, f64>,
    std::collections::HashMap<String, f64>,
)> {
    let client = pool.get().await?;
    let mut person_day = std::collections::HashMap::new();
    for row in client
        .query(
            "SELECT person, SUM(cost_usd) FROM usage_events \
             WHERE ts >= date_trunc('day', now() AT TIME ZONE 'utc') AT TIME ZONE 'utc' \
             GROUP BY person",
            &[],
        )
        .await?
    {
        person_day.insert(row.get::<_, String>(0), row.get::<_, f64>(1));
    }
    let mut project_month = std::collections::HashMap::new();
    for row in client
        .query(
            "SELECT project, SUM(cost_usd) FROM usage_events \
             WHERE ts >= date_trunc('month', now() AT TIME ZONE 'utc') AT TIME ZONE 'utc' \
             GROUP BY project",
            &[],
        )
        .await?
    {
        project_month.insert(row.get::<_, String>(0), row.get::<_, f64>(1));
    }
    Ok((person_day, project_month))
}

/// Deletes `usage_events` rows older than `days` (enterprise#36). Returns the
/// number of purged rows. `days == 0` is rejected by the caller (retention
/// disabled), never here — this function always deletes what it is told.
pub async fn purge_events_older_than(pool: &Pool, days: u32) -> anyhow::Result<u64> {
    let client = pool.get().await?;
    let purged = client
        .execute(
            "DELETE FROM usage_events WHERE ts < now() - make_interval(days => $1)",
            &[&i32::try_from(days).unwrap_or(i32::MAX)],
        )
        .await?;
    Ok(purged)
}

/// All events attributed to one of `person_keys` (raw + pseudonym, GDPR
/// Art. 15 export), as self-describing JSON rows.
pub async fn person_events(
    pool: &Pool,
    person_keys: &[String],
) -> anyhow::Result<Vec<serde_json::Value>> {
    let client = pool.get().await?;
    let rows = client
        .query(
            "SELECT to_jsonb(usage_events) FROM usage_events \
             WHERE person = ANY($1) ORDER BY ts",
            &[&person_keys],
        )
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| r.get::<_, serde_json::Value>(0))
        .collect())
}

/// Deletes all events of `person_keys` (GDPR Art. 17). Returns rows removed.
pub async fn delete_person_events(pool: &Pool, person_keys: &[String]) -> anyhow::Result<u64> {
    let client = pool.get().await?;
    let deleted = client
        .execute(
            "DELETE FROM usage_events WHERE person = ANY($1)",
            &[&person_keys],
        )
        .await?;
    Ok(deleted)
}

/// Daily evidence aggregates for the export window (enterprise#36): bounded
/// output regardless of event volume, yet fine-grained enough for an EU-AI-Act
/// usage-evidence audit (per day × person × project × model).
pub async fn evidence_rows(
    pool: &Pool,
    from: chrono::DateTime<chrono::Utc>,
    to: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<Vec<serde_json::Value>> {
    let client = pool.get().await?;
    let rows = client
        .query(
            "SELECT jsonb_build_object(
               'date', to_char(date_trunc('day', ts AT TIME ZONE 'utc'), 'YYYY-MM-DD'),
               'person', person,
               'project', project,
               'model', model,
               'provider', provider,
               'requests', count(*),
               'input_tokens', sum(input_tokens)::BIGINT,
               'output_tokens', sum(output_tokens)::BIGINT,
               'cache_read_tokens', sum(cache_read_tokens)::BIGINT,
               'cost_usd', round(sum(cost_usd)::numeric, 6),
               'saved_usd', round(sum(saved_usd)::numeric, 6),
               'reference_cost_usd', round(sum(reference_cost_usd)::numeric, 6),
               'local_requests', count(*) FILTER (WHERE is_local),
               'measured_requests', count(*) FILTER (WHERE cost_source = 'provider'),
               'estimated_requests', count(*) FILTER (WHERE cost_source = 'heuristic')
             )
             FROM usage_events WHERE ts >= $1 AND ts <= $2
             GROUP BY
               date_trunc('day', ts AT TIME ZONE 'utc'), person, project, model, provider
             ORDER BY
               date_trunc('day', ts AT TIME ZONE 'utc'), person, project, model, provider",
            &[&from, &to],
        )
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| r.get::<_, serde_json::Value>(0))
        .collect())
}

/// Wires the usage stream into Postgres: installs the process-wide sink
/// (`proxy::usage_sink`) and spawns the writer task. Call once at gateway
/// startup, after `init_schema`.
///
/// Returns `false` when a sink was already installed (double start).
pub fn spawn_writer(pool: Pool) -> bool {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<RealUsage>(WRITER_QUEUE);
    if !crate::proxy::usage_sink::install(tx) {
        return false;
    }
    tokio::spawn(async move {
        // One pricing table + baseline for the writer's lifetime: rows are
        // priced at insert time (the ledger re-values against frozen
        // references); the baseline is contract-frozen anyway (#41).
        let pricing = ModelPricing::load();
        let baseline = crate::core::config::Config::load().proxy.baseline.clone();
        while let Some(usage) = rx.recv().await {
            let event = UsageEvent::from_usage(&usage, &pricing, &baseline);
            match pool.get().await {
                Ok(client) => {
                    if let Err(e) = insert_event(&client, &event).await {
                        tracing::warn!("usage_events insert failed (fail-open): {e:#}");
                    }
                }
                Err(e) => {
                    tracing::warn!("usage_events pool unavailable (fail-open): {e:#}");
                }
            }
        }
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::usage::WireContext;

    fn usage_with_wire(wire: Option<Box<WireContext>>) -> RealUsage {
        RealUsage {
            model: "claude-sonnet-4-5".into(),
            input_tokens: 1000,
            output_tokens: 500,
            cache_read_tokens: 200,
            cache_write_tokens: 100,
            reasoning_tokens: 50,
            provider_cost_usd: None,
            cohort: None,
            wire,
        }
    }

    #[test]
    fn event_carries_identity_and_baseline_fields() {
        let usage = usage_with_wire(Some(Box::new(WireContext {
            provider: "Anthropic".into(),
            person: Some("yves".into()),
            team: Some("platform".into()),
            project: Some("ai-gateway".into()),
            saved_tokens: 4000,
            uncompressed_input_tokens: 5000,
            is_local: false,
            routed_from: Some("claude-opus-4-5".into()),
            counterfactual: None,
            lineage: None,
        })));
        let event = UsageEvent::from_usage(
            &usage,
            &ModelPricing::load(),
            &BaselineConfig {
                reference_model: Some("claude-opus-4.5".into()),
                local_shadow_rate_per_mtok: None,
            },
        );

        assert_eq!(event.person, "yves");
        assert_eq!(event.team.as_deref(), Some("platform"));
        assert_eq!(event.project, "ai-gateway");
        assert_eq!(event.provider, "Anthropic");
        assert_eq!(event.model, "claude-sonnet-4-5");
        assert_eq!(event.routed_from.as_deref(), Some("claude-opus-4-5"));
        assert_eq!(event.input_tokens, 1000);
        assert_eq!(event.saved_tokens, 4000);
        assert_eq!(event.uncompressed_input_tokens, 5000);
        assert!(!event.is_local);
        assert!(event.cost_usd > 0.0, "known model must be priced");
        assert_eq!(
            event.cost_source, "list",
            "exact table match books list price"
        );
        assert!(
            event.saved_usd > 0.0,
            "saved tokens on a priced model must yield saved USD"
        );
        // Counterfactual (enterprise#15): 5000 uncompressed input tokens at
        // claude-opus-4.5's $5/MTok input rate = $0.025.
        assert_eq!(event.reference_model.as_deref(), Some("claude-opus-4.5"));
        assert!((event.reference_cost_usd - 0.025).abs() < 1e-9);
    }

    #[test]
    fn provider_reported_cost_beats_the_price_table() {
        // #1179: OpenRouter's `usage.cost` is the bill — the table estimate for
        // these tokens (claude-sonnet at list price) would be ~50× higher and
        // must NOT be booked when a measured figure exists.
        let mut usage = usage_with_wire(Some(Box::new(WireContext {
            provider: "openrouter".into(),
            person: Some("nicolas".into()),
            team: None,
            project: Some("bot".into()),
            saved_tokens: 0,
            uncompressed_input_tokens: 1000,
            is_local: false,
            routed_from: None,
            counterfactual: None,
            lineage: None,
        })));
        usage.provider_cost_usd = Some(0.0123);
        let event =
            UsageEvent::from_usage(&usage, &ModelPricing::load(), &BaselineConfig::default());
        assert!((event.cost_usd - 0.0123).abs() < 1e-12);
        assert_eq!(event.cost_source, "provider");

        // A measured zero (":free" model) is a real price, not a missing one.
        usage.provider_cost_usd = Some(0.0);
        let event =
            UsageEvent::from_usage(&usage, &ModelPricing::load(), &BaselineConfig::default());
        assert_eq!(event.cost_usd, 0.0);
        assert_eq!(event.cost_source, "provider");
    }

    #[test]
    fn unknown_model_is_marked_heuristic_and_local_shadow_beats_measured() {
        // An unpriced model falls into the blended fallback → cost_source
        // must say so instead of presenting the estimate as exact.
        let mut usage = usage_with_wire(None);
        usage.model = "vendor/brand-new-model-20990101".into();
        let event =
            UsageEvent::from_usage(&usage, &ModelPricing::load(), &BaselineConfig::default());
        assert_eq!(event.cost_source, "heuristic");

        // Local turns book the shadow rate even if a cost slipped through —
        // the shadow rate is the contract for local inference (Doc 04 §6).
        let mut usage = usage_with_wire(Some(Box::new(WireContext {
            provider: "ollama".into(),
            person: None,
            team: None,
            project: None,
            saved_tokens: 0,
            uncompressed_input_tokens: 0,
            is_local: true,
            routed_from: None,
            counterfactual: None,
            lineage: None,
        })));
        usage.provider_cost_usd = Some(9.99);
        let event =
            UsageEvent::from_usage(&usage, &ModelPricing::load(), &BaselineConfig::default());
        assert_eq!(event.cost_source, "shadow");
        assert!(
            event.cost_usd < 1.0,
            "shadow rate, not the stray measured figure"
        );
    }

    #[test]
    fn event_without_wire_context_uses_honest_fallbacks() {
        let event = UsageEvent::from_usage(
            &usage_with_wire(None),
            &ModelPricing::load(),
            &BaselineConfig::default(),
        );
        assert_eq!(event.person, ANONYMOUS_PERSON);
        assert_eq!(event.project, DEFAULT_PROJECT);
        assert_eq!(event.team, None);
        assert_eq!(event.saved_tokens, 0);
        assert_eq!(event.saved_usd, 0.0);
        assert_eq!(event.uncompressed_input_tokens, 0);
        assert!(!event.is_local);
        // No reference model configured → no counterfactual claimed.
        assert_eq!(event.reference_model, None);
        assert_eq!(event.reference_cost_usd, 0.0);
    }

    #[test]
    fn local_usage_books_shadow_rate_never_zero() {
        // enterprise#15/#18: local inference is billed via the transparent
        // shadow rate — savings against local models stay honest, not infinite.
        let usage = usage_with_wire(Some(Box::new(WireContext {
            provider: "ollama".into(),
            person: Some("yves".into()),
            team: None,
            project: None,
            saved_tokens: 0,
            uncompressed_input_tokens: 2000,
            is_local: true,
            routed_from: None,
            counterfactual: None,
            lineage: None,
        })));
        let event =
            UsageEvent::from_usage(&usage, &ModelPricing::load(), &BaselineConfig::default());
        assert!(event.is_local);
        // billable = 1000 in + 500 out + 200 cache-read + 100 cache-write =
        // 1800 tokens × $0.25/MTok default shadow rate.
        assert!((event.cost_usd - 0.25 / 1_000_000.0 * 1800.0).abs() < 1e-12);
        assert!(event.cost_usd > 0.0, "local cost must never be zero");

        // A configured rate wins; a zero/negative config falls back to default.
        let cfg = BaselineConfig {
            reference_model: None,
            local_shadow_rate_per_mtok: Some(1.0),
        };
        let event = UsageEvent::from_usage(&usage, &ModelPricing::load(), &cfg);
        assert!((event.cost_usd - 1.0 / 1_000_000.0 * 1800.0).abs() < 1e-12);
        let zero = BaselineConfig {
            reference_model: None,
            local_shadow_rate_per_mtok: Some(0.0),
        };
        assert!(zero.effective_local_shadow_rate() > 0.0);
    }

    #[test]
    fn sslmode_selects_tls_and_pool_builds_for_both() {
        // require → TLS connector; disable/unset → plain (#54/#58).
        let tls: tokio_postgres::Config = "postgres://u:p@db.example.com:5432/app?sslmode=require"
            .parse()
            .unwrap();
        assert!(wants_tls(&tls));
        let plain: tokio_postgres::Config = "postgres://u:p@localhost:5432/app".parse().unwrap();
        assert!(!wants_tls(&plain));
        let disabled: tokio_postgres::Config = "postgres://u:p@localhost:5432/app?sslmode=disable"
            .parse()
            .unwrap();
        assert!(!wants_tls(&disabled));

        // Pool construction (no connection attempt) must succeed on both paths —
        // this exercises the rustls config + root store wiring.
        assert!(
            pool_from_database_url("postgres://u:p@db.example.com:5432/app?sslmode=require")
                .is_ok()
        );
        assert!(pool_from_database_url("postgres://u:p@localhost:5432/app").is_ok());
    }

    #[test]
    fn pool_size_env_is_clamped_and_falls_back() {
        // Env mutation is serialized process-wide through test_env_lock().
        let _guard = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var(POOL_MAX_SIZE_ENV);
        assert_eq!(pool_max_size(), 8, "unset -> default");
        crate::test_env::set_var(POOL_MAX_SIZE_ENV, "24");
        assert_eq!(pool_max_size(), 24, "explicit value wins");
        crate::test_env::set_var(POOL_MAX_SIZE_ENV, "0");
        assert_eq!(pool_max_size(), 2, "clamped low");
        crate::test_env::set_var(POOL_MAX_SIZE_ENV, "9999");
        assert_eq!(pool_max_size(), 64, "clamped high");
        crate::test_env::set_var(POOL_MAX_SIZE_ENV, "not-a-number");
        assert_eq!(pool_max_size(), 8, "garbage -> default, never panic");
        crate::test_env::remove_var(POOL_MAX_SIZE_ENV);
    }

    #[test]
    fn schema_ddl_is_idempotent_by_construction() {
        // The gateway runs this DDL on every start against a live database, so
        // every CREATE must carry IF NOT EXISTS, and post-release columns ride
        // along as ALTER TABLE … ADD COLUMN IF NOT EXISTS.
        for stmt in ["CREATE TABLE", "CREATE INDEX"] {
            for (i, _) in USAGE_EVENTS_DDL.match_indices(stmt) {
                let tail = &USAGE_EVENTS_DDL[i..(i + stmt.len() + 14).min(USAGE_EVENTS_DDL.len())];
                assert!(
                    tail.contains("IF NOT EXISTS"),
                    "non-idempotent DDL statement: {tail}"
                );
            }
        }
        for (i, _) in USAGE_EVENTS_DDL.match_indices("ADD COLUMN") {
            let tail = &USAGE_EVENTS_DDL[i..(i + 25).min(USAGE_EVENTS_DDL.len())];
            assert!(
                tail.contains("IF NOT EXISTS"),
                "non-idempotent ALTER statement: {tail}"
            );
        }
        // And the baseline fields (enterprise#18) are part of the schema.
        for col in [
            "uncompressed_input_tokens",
            "reference_model",
            "reference_cost_usd",
            "is_local",
            "cost_source",
        ] {
            assert!(
                USAGE_EVENTS_DDL.contains(col),
                "baseline column {col} missing from schema"
            );
        }
    }
}
