//! MCP metering pipeline (GL#102): sink, writer, cost attribution.
//!
//! Mirrors the LLM channel's `proxy::usage_sink` + `store::spawn_writer`
//! discipline exactly — bounded channel, spawned writer task, fail-open by
//! construction. The proxy handler calls [`record`]; nothing on the tool
//! traffic path ever waits for Postgres.
//!
//! Cost attribution (Doc 15 §7): a tool result is *context* — it gets sent on
//! to a model as part of the next prompt. Its honest price is therefore the
//! result's tokens at the org's contract-frozen `reference_model` **input**
//! rate. No reference model configured → cost stays 0.0 (the gateway never
//! invents a number); tokens/bytes still tell the amplification story.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use super::frames::ToolDef;
use super::store::McpEvent;

/// Buffered events between the proxy path and the Postgres writer. Same
/// sizing rationale as `store::WRITER_QUEUE` (bursts drop, counted).
pub const WRITER_QUEUE: usize = 4096;

/// What the writer consumes: the measured exchange plus an optional
/// `tools/list` inventory snapshot to upsert.
#[derive(Debug)]
pub struct MeteredExchange {
    pub event: McpEvent,
    /// `Some` when the exchange was a `tools/list` response — the writer
    /// updates `mcp_tool_inventory` alongside the event row.
    pub inventory: Option<Vec<ToolDef>>,
}

static SINK: OnceLock<tokio::sync::mpsc::Sender<MeteredExchange>> = OnceLock::new();
static DROPPED: AtomicU64 = AtomicU64::new(0);

/// True once the writer is installed (metering on). The proxy skips the
/// analysis work entirely when nothing would consume it.
#[must_use]
pub fn installed() -> bool {
    SINK.get().is_some()
}

/// Forwards one measured exchange to the writer, if metering is on. Never
/// blocks: on a full or closed channel the event is dropped and counted.
pub fn record(exchange: MeteredExchange) {
    let Some(tx) = SINK.get() else { return };
    if tx.try_send(exchange).is_err() {
        let n = DROPPED.fetch_add(1, Ordering::Relaxed) + 1;
        if n.is_power_of_two() {
            tracing::warn!("mcp metering backlogged: {n} event(s) dropped so far");
        }
    }
}

/// Events dropped because the sink was full/closed (Prometheus, `/metrics`).
#[must_use]
pub fn dropped_count() -> u64 {
    DROPPED.load(Ordering::Relaxed)
}

/// Events currently queued (graceful-drain window on shutdown, same contract
/// as `usage_sink::pending_count`).
#[must_use]
pub fn pending_count() -> usize {
    SINK.get().map_or(0, |tx| tx.max_capacity() - tx.capacity())
}

/// Installs the sink and spawns the writer task. Call once at gateway
/// startup, after `mcp::store::init_schema`. Returns `false` when a sink was
/// already installed (double start).
///
/// Pricing happens here, off the request path: one `ModelPricing` table +
/// baseline for the writer's lifetime (same convention as the LLM writer in
/// `store::spawn_writer`), stamped onto every event before insert.
pub fn spawn_writer(pool: deadpool_postgres::Pool) -> bool {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<MeteredExchange>(WRITER_QUEUE);
    if SINK.set(tx).is_err() {
        return false;
    }
    tokio::spawn(async move {
        let pricing = crate::core::gain::model_pricing::ModelPricing::load();
        let reference = reference_model(&crate::core::config::Config::load().proxy.baseline);
        while let Some(MeteredExchange {
            mut event,
            inventory,
        }) = rx.recv().await
        {
            event.reference_model = reference.clone();
            event.context_cost_usd = context_cost_usd(
                u64::try_from(event.result_tokens).unwrap_or(0),
                reference.as_deref(),
                &pricing,
            );
            match pool.get().await {
                Ok(client) => {
                    if let Err(e) = super::store::insert_event(&client, &event).await {
                        tracing::warn!("mcp_events insert failed (fail-open): {e:#}");
                    }
                    if let Some(tools) = inventory
                        && let Err(e) =
                            super::store::upsert_inventory(&client, &event.server_id, &tools).await
                    {
                        tracing::warn!("mcp_tool_inventory upsert failed (fail-open): {e:#}");
                    }
                }
                Err(e) => {
                    tracing::warn!("mcp store pool unavailable (fail-open): {e:#}");
                }
            }
        }
    });
    true
}

/// Prices `result_tokens` at the reference model's input rate (USD). The
/// pricing table is the shared `ModelPricing` the LLM channel bills with —
/// one price source, no drift between the two channels.
#[must_use]
pub fn context_cost_usd(
    result_tokens: u64,
    reference_model: Option<&str>,
    pricing: &crate::core::gain::model_pricing::ModelPricing,
) -> f64 {
    let Some(model) = reference_model else {
        return 0.0;
    };
    #[allow(clippy::cast_precision_loss)]
    {
        pricing.quote(Some(model)).cost.input_per_m / 1_000_000.0 * result_tokens as f64
    }
}

/// Resolves the trimmed, non-empty reference model from the baseline config.
#[must_use]
pub fn reference_model(baseline: &crate::core::config::BaselineConfig) -> Option<String> {
    baseline
        .reference_model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::gain::model_pricing::ModelPricing;

    #[test]
    fn record_without_sink_is_a_noop() {
        // Metering off (no DATABASE_URL) → the tool path must not care.
        record(MeteredExchange {
            event: McpEvent {
                person: "p".into(),
                team: None,
                project: "default".into(),
                server_id: "s".into(),
                method: "tools/call".into(),
                tool: Some("t".into()),
                status: "ok".into(),
                duration_ms: 1,
                result_bytes: 1,
                result_tokens: 1,
                context_cost_usd: 0.0,
                reference_model: None,
            },
            inventory: None,
        });
    }

    #[test]
    fn context_cost_uses_reference_input_rate_and_never_invents() {
        let pricing = ModelPricing::load();
        // claude-opus-4.5 lists $5/MTok input → 200k tokens = $1.00.
        let cost = context_cost_usd(200_000, Some("claude-opus-4.5"), &pricing);
        assert!((cost - 1.0).abs() < 1e-9, "expected $1.00, got {cost}");
        // No reference model → honest zero.
        assert_eq!(context_cost_usd(200_000, None, &pricing), 0.0);
        // Zero tokens → zero cost.
        assert_eq!(context_cost_usd(0, Some("claude-opus-4.5"), &pricing), 0.0);
    }

    #[test]
    fn reference_model_trims_and_rejects_empty() {
        use crate::core::config::BaselineConfig;
        let some = BaselineConfig {
            reference_model: Some("  claude-opus-4.5  ".into()),
            local_shadow_rate_per_mtok: None,
        };
        assert_eq!(reference_model(&some).as_deref(), Some("claude-opus-4.5"));
        let blank = BaselineConfig {
            reference_model: Some("   ".into()),
            local_shadow_rate_per_mtok: None,
        };
        assert_eq!(reference_model(&blank), None);
        assert_eq!(reference_model(&BaselineConfig::default()), None);
    }
}
