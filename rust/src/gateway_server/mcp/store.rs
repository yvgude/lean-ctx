//! `mcp_events` + `mcp_tool_inventory` Postgres store (GL#102/#103).
//!
//! Deliberately **separate tables** from `usage_events` (a documented
//! deviation from Doc 15 §7's "new dimension" sketch): LLM spend and tool
//! context cost are different currencies, and folding MCP rows into
//! `usage_events` would silently inflate every existing spend report,
//! projection and evidence export. Attribution still joins on `person`/
//! `team`/`project` — the identity plane is shared.
//!
//! Lifecycle parity with the LLM channel is non-negotiable:
//! - Retention: `[gateway_server].usage_retention_days` purges both tables.
//! - GDPR: `gateway ln export|delete` covers both tables (Art. 15/17).
//! - Fail-open: inserts are queued by `metering::spawn_writer`; errors are
//!   logged and counted, never propagated to the tool-traffic path.
//!
//! Schema management follows the repo rule: idempotent `CREATE … IF NOT
//! EXISTS` DDL run on every start, no migration files.

use deadpool_postgres::Pool;

/// Idempotent DDL, run on every `gateway serve` start (same contract as
/// `USAGE_EVENTS_DDL`).
const MCP_DDL: &str = r"
CREATE TABLE IF NOT EXISTS mcp_events (
  id               BIGSERIAL PRIMARY KEY,
  ts               TIMESTAMPTZ      NOT NULL DEFAULT now(),
  person           TEXT             NOT NULL,
  team             TEXT,
  project          TEXT             NOT NULL,
  server_id        TEXT             NOT NULL,
  method           TEXT             NOT NULL,
  tool             TEXT,
  status           TEXT             NOT NULL,
  duration_ms      BIGINT           NOT NULL DEFAULT 0,
  result_bytes     BIGINT           NOT NULL DEFAULT 0,
  result_tokens    BIGINT           NOT NULL DEFAULT 0,
  context_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
  reference_model  TEXT
);
CREATE INDEX IF NOT EXISTS idx_mcp_events_person_ts ON mcp_events (person, ts);
CREATE INDEX IF NOT EXISTS idx_mcp_events_server_ts ON mcp_events (server_id, ts);
CREATE INDEX IF NOT EXISTS idx_mcp_events_tool_ts   ON mcp_events (server_id, tool, ts);
CREATE TABLE IF NOT EXISTS mcp_tool_inventory (
  server_id       TEXT        NOT NULL,
  tool            TEXT        NOT NULL,
  schema_sha256   TEXT        NOT NULL,
  previous_sha256 TEXT,
  first_seen      TIMESTAMPTZ NOT NULL DEFAULT now(),
  last_seen       TIMESTAMPTZ NOT NULL DEFAULT now(),
  change_count    BIGINT      NOT NULL DEFAULT 0,
  PRIMARY KEY (server_id, tool)
);
";

/// Applies the MCP-store DDL. Safe to run on every start (idempotent).
pub async fn init_schema(pool: &Pool) -> anyhow::Result<()> {
    let client = pool.get().await?;
    client.batch_execute(MCP_DDL).await?;
    Ok(())
}

/// One measured MCP exchange, ready for insertion.
#[derive(Debug, Clone, PartialEq)]
pub struct McpEvent {
    pub person: String,
    pub team: Option<String>,
    pub project: String,
    pub server_id: String,
    /// JSON-RPC method label (`tools/call`, `tools/list`, `passthrough`, …).
    pub method: String,
    /// Tool name for `tools/call`; `None` otherwise.
    pub tool: Option<String>,
    /// `ok` | `error` (JSON-RPC error frame) | `upstream_error` (transport).
    pub status: String,
    pub duration_ms: i64,
    pub result_bytes: i64,
    pub result_tokens: i64,
    /// `result_tokens` priced at the reference model's input rate — what this
    /// tool context costs every time it is sent on to an LLM. `0.0` when no
    /// `[proxy.baseline].reference_model` is configured (never invented).
    pub context_cost_usd: f64,
    pub reference_model: Option<String>,
}

/// Inserts one event. Errors bubble to the writer loop, which logs and moves on.
pub async fn insert_event(client: &deadpool_postgres::Client, e: &McpEvent) -> anyhow::Result<()> {
    client
        .execute(
            "INSERT INTO mcp_events \
             (person, team, project, server_id, method, tool, status, \
              duration_ms, result_bytes, result_tokens, context_cost_usd, reference_model) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
            &[
                &e.person,
                &e.team,
                &e.project,
                &e.server_id,
                &e.method,
                &e.tool,
                &e.status,
                &e.duration_ms,
                &e.result_bytes,
                &e.result_tokens,
                &e.context_cost_usd,
                &e.reference_model,
            ],
        )
        .await?;
    Ok(())
}

/// Records a `tools/list` snapshot for one server: upsert per tool, bumping
/// `change_count` and remembering the previous hash whenever the definition
/// fingerprint moved (the rug-pull trail, GL#103). Tools that disappeared
/// from the listing keep their last row — an inventory is also a history.
pub async fn upsert_inventory(
    client: &deadpool_postgres::Client,
    server_id: &str,
    tools: &[super::frames::ToolDef],
) -> anyhow::Result<()> {
    let stmt = client
        .prepare_cached(
            "INSERT INTO mcp_tool_inventory (server_id, tool, schema_sha256) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (server_id, tool) DO UPDATE SET \
               last_seen       = now(), \
               previous_sha256 = CASE WHEN mcp_tool_inventory.schema_sha256 <> EXCLUDED.schema_sha256 \
                                      THEN mcp_tool_inventory.schema_sha256 \
                                      ELSE mcp_tool_inventory.previous_sha256 END, \
               change_count    = mcp_tool_inventory.change_count + \
                                 CASE WHEN mcp_tool_inventory.schema_sha256 <> EXCLUDED.schema_sha256 \
                                      THEN 1 ELSE 0 END, \
               schema_sha256   = EXCLUDED.schema_sha256",
        )
        .await?;
    for t in tools {
        client
            .execute(&stmt, &[&server_id, &t.name, &t.schema_sha256])
            .await?;
    }
    Ok(())
}

/// Deletes `mcp_events` rows older than `days` (retention parity with
/// `usage_events`, enterprise#36). The inventory is config-scale metadata,
/// not per-person telemetry — it is never purged by retention.
pub async fn purge_events_older_than(pool: &Pool, days: u32) -> anyhow::Result<u64> {
    let client = pool.get().await?;
    let purged = client
        .execute(
            "DELETE FROM mcp_events WHERE ts < now() - make_interval(days => $1)",
            &[&i32::try_from(days).unwrap_or(i32::MAX)],
        )
        .await?;
    Ok(purged)
}

/// All MCP events attributed to one of `person_keys` (raw + pseudonym) —
/// GDPR Art. 15 export, same contract as `store::person_events`.
pub async fn person_events(
    pool: &Pool,
    person_keys: &[String],
) -> anyhow::Result<Vec<serde_json::Value>> {
    let client = pool.get().await?;
    let rows = client
        .query(
            "SELECT to_jsonb(mcp_events) FROM mcp_events \
             WHERE person = ANY($1) ORDER BY ts",
            &[&person_keys],
        )
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| r.get::<_, serde_json::Value>(0))
        .collect())
}

/// Deletes all MCP events of `person_keys` (GDPR Art. 17). Returns rows removed.
pub async fn delete_person_events(pool: &Pool, person_keys: &[String]) -> anyhow::Result<u64> {
    let client = pool.get().await?;
    let deleted = client
        .execute(
            "DELETE FROM mcp_events WHERE person = ANY($1)",
            &[&person_keys],
        )
        .await?;
    Ok(deleted)
}

/// Aggregated per-server × tool activity for the admin window (console
/// "Tools" section). Stable ordering: cost desc, then name — deterministic
/// output for identical database contents (#498).
pub const TOOL_BREAKDOWN_SQL: &str = "
SELECT server_id,
       coalesce(tool, method)        AS tool,
       count(*)                      AS calls,
       count(*) FILTER (WHERE status <> 'ok') AS errors,
       count(DISTINCT person)        AS persons,
       sum(result_tokens)::BIGINT    AS result_tokens,
       sum(context_cost_usd)         AS context_cost_usd,
       max(duration_ms)              AS max_duration_ms,
       percentile_cont(0.5) WITHIN GROUP (ORDER BY duration_ms) AS p50_duration_ms
FROM mcp_events
WHERE ts >= $1 AND ts <= $2
GROUP BY server_id, coalesce(tool, method)
ORDER BY context_cost_usd DESC, tool";

/// Per-person MCP totals for `/me` ("your tools").
pub const ME_TOOLS_SQL: &str = "
SELECT server_id,
       coalesce(tool, method)     AS tool,
       count(*)                   AS calls,
       sum(result_tokens)::BIGINT AS result_tokens,
       sum(context_cost_usd)      AS context_cost_usd
FROM mcp_events
WHERE ts >= $1 AND ts <= $2 AND person = $3
GROUP BY server_id, coalesce(tool, method)
ORDER BY context_cost_usd DESC, tool
LIMIT 50";

/// Window totals for the admin summary strip.
pub const TOTALS_SQL: &str = "
SELECT count(*)                                AS calls,
       count(*) FILTER (WHERE status <> 'ok')  AS errors,
       count(DISTINCT person)                  AS persons,
       coalesce(sum(result_tokens), 0)::BIGINT AS result_tokens,
       coalesce(sum(context_cost_usd), 0)      AS context_cost_usd
FROM mcp_events
WHERE ts >= $1 AND ts <= $2";

/// Inventory listing with live hash status. `changed` surfaces every tool
/// whose definition fingerprint moved at least once — the observe-stage
/// rug-pull signal (enforcement pins hashes in M4).
pub const INVENTORY_SQL: &str = "
SELECT server_id, tool, schema_sha256, previous_sha256,
       change_count,
       to_char(first_seen AT TIME ZONE 'utc', 'YYYY-MM-DD') AS first_seen,
       to_char(last_seen  AT TIME ZONE 'utc', 'YYYY-MM-DD') AS last_seen
FROM mcp_tool_inventory
ORDER BY server_id, tool";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ddl_is_idempotent_by_construction() {
        for stmt in ["CREATE TABLE", "CREATE INDEX"] {
            for (i, _) in MCP_DDL.match_indices(stmt) {
                let tail = &MCP_DDL[i..(i + stmt.len() + 14).min(MCP_DDL.len())];
                assert!(
                    tail.contains("IF NOT EXISTS"),
                    "non-idempotent DDL statement: {tail}"
                );
            }
        }
    }

    #[test]
    fn schema_carries_the_observe_columns() {
        // The columns the observe stage's queries and the M4 enforce stage's
        // pinning depend on — a rename here is a breaking change.
        for col in [
            "server_id",
            "method",
            "tool",
            "status",
            "result_tokens",
            "context_cost_usd",
            "schema_sha256",
            "previous_sha256",
            "change_count",
        ] {
            assert!(MCP_DDL.contains(col), "column {col} missing from DDL");
        }
    }

    #[test]
    fn aggregate_sql_is_window_bounded_and_deterministically_ordered() {
        for sql in [TOOL_BREAKDOWN_SQL, ME_TOOLS_SQL, TOTALS_SQL] {
            assert!(
                sql.contains("ts >= $1 AND ts <= $2"),
                "window bounds: {sql}"
            );
        }
        for sql in [TOOL_BREAKDOWN_SQL, ME_TOOLS_SQL, INVENTORY_SQL] {
            assert!(sql.contains("ORDER BY"), "stable ordering required: {sql}");
        }
    }
}
