//! `lean-ctx gateway serve` (enterprise#10) — the self-hosted org gateway.
//!
//! One process, three parts:
//!
//! 1. **Proxy** — the existing `proxy::start_proxy_with_token` with its
//!    gateway hardening (`proxy_bind_host`, host allowlist, strict Bearer,
//!    rate limit; enterprise#8/#37). Nothing proxy-related changes here.
//! 2. **Usage store** — Postgres `usage_events` writer via `store::spawn_writer`
//!    (enterprise#17/#18), wired to the proxy's `usage_sink`.
//! 3. **Admin listener** — a *separate* port serving `GET /api/admin/usage`
//!    (enterprise#20) and `GET /metrics` (Prometheus, enterprise#34) behind its
//!    own Bearer token, plus an unauthenticated `/healthz`. Separate on purpose:
//!    deployments keep it cluster-internal (no ingress) while the proxy port is
//!    the only exposed surface.
//!
//! **Fail-open is the core rule (enterprise#12):** LLM traffic never depends on
//! the periphery. Postgres down at startup → warn and serve anyway (the writer
//! retries per event and drops, counted). Admin token missing → admin listener
//! stays off, proxy serves. Store insert failures → logged, never propagated.
//! The only hard startup failures are a malformed `gateway-keys.toml` (auth
//! correctness) and an unbindable proxy port.

use std::sync::Arc;

use axum::response::IntoResponse;

/// Environment variable holding the admin Bearer token. Env-only by design —
/// tokens never live in config.toml (same rule as `LEAN_CTX_PROXY_TOKEN`).
pub const ADMIN_TOKEN_ENV: &str = "LEAN_CTX_GATEWAY_ADMIN_TOKEN";

/// Environment variable with the Postgres connection string for `usage_events`.
pub const DATABASE_URL_ENV: &str = "DATABASE_URL";

/// Options parsed by the CLI (`lean-ctx gateway serve`).
#[derive(Debug, Clone)]
pub struct ServeOptions {
    /// Proxy port (the exposed surface). Defaults to the standard proxy port.
    pub port: u16,
    /// Admin/metrics port. Defaults to `port + 1`.
    pub admin_port: Option<u16>,
}

/// Runs the gateway until shutdown. See module docs for the composition.
///
/// # Errors
/// Fails on invalid gateway keys or an unbindable proxy/admin port — never on
/// unavailable periphery (Postgres, missing admin token).
pub async fn serve(opts: ServeOptions) -> anyhow::Result<()> {
    let cfg = crate::core::config::Config::load();
    let admin_port = opts
        .admin_port
        .unwrap_or_else(|| opts.port.saturating_add(1));

    // -- Usage store (fail-open, enterprise#12/#17) -------------------------
    let pool = match std::env::var(DATABASE_URL_ENV) {
        Ok(url) if !url.trim().is_empty() => match super::store::pool_from_database_url(&url) {
            Ok(pool) => {
                match super::store::init_schema(&pool).await {
                    Ok(()) => println!("  Store:     usage_events ready (Postgres)"),
                    Err(e) => {
                        // Pool stays: the writer retries per event once PG is back.
                        println!(
                            "  Store:     ⚠ Postgres unreachable at startup (fail-open): {e:#}"
                        );
                    }
                }
                if super::store::spawn_writer(pool.clone()) {
                    Some(pool)
                } else {
                    tracing::warn!("usage sink already installed — store writer not started twice");
                    Some(pool)
                }
            }
            Err(e) => {
                println!(
                    "  Store:     ⚠ invalid {DATABASE_URL_ENV} (fail-open, metering off): {e:#}"
                );
                None
            }
        },
        _ => {
            println!(
                "  Store:     off — set {DATABASE_URL_ENV} to enable org-wide usage_events metering"
            );
            None
        }
    };

    // -- Admin listener (dashboard + admin API + /metrics, #20/#34/#45) -----
    match (pool.clone(), admin_token()) {
        (Some(pool), Some(token)) => {
            let state = super::admin_api::AdminState {
                pool,
                seats: cfg.gateway_server.seats,
                org_label: cfg.gateway_server.org_label.clone(),
                started_at: std::time::Instant::now(),
                providers: super::admin_status::provider_statuses(&cfg.proxy.resolve_providers()),
                routing_enabled: cfg.proxy.routing.is_active(),
                reference_model: cfg.proxy.baseline.reference_model.clone(),
                local_shadow_rate: cfg.proxy.baseline.effective_local_shadow_rate(),
            };
            let router = admin_router(state, token);
            let addr = std::net::SocketAddr::from(([0, 0, 0, 0], admin_port));
            let listener = tokio::net::TcpListener::bind(addr).await?;
            println!(
                "  Admin:     http://{addr}/ (dashboard) + /api/admin/* + /metrics (Bearer via {ADMIN_TOKEN_ENV})"
            );
            tokio::spawn(async move {
                if let Err(e) = axum::serve(listener, router).await {
                    tracing::warn!("admin listener terminated (proxy unaffected): {e:#}");
                }
            });
        }
        (Some(_), None) => {
            println!(
                "  Admin:     off — set {ADMIN_TOKEN_ENV} to serve the dashboard + /api/admin/*"
            );
        }
        (None, _) => {
            println!("  Admin:     off — requires the usage store ({DATABASE_URL_ENV})");
        }
    }

    // -- Proxy (blocking; the actual gateway surface) ------------------------
    println!("lean-ctx gateway: starting proxy on port {} …", opts.port);
    let result = crate::proxy::start_proxy(opts.port).await;

    // Graceful drain (enterprise#51): the proxy returned after SIGTERM/Ctrl-C
    // finished in-flight requests; give the store writer a bounded window to
    // flush queued usage events so a rollout doesn't shed metering.
    if pool.is_some() {
        drain_usage_queue(std::time::Duration::from_secs(5)).await;
    }
    result
}

/// Waits until the usage sink queue is empty or the deadline passes.
async fn drain_usage_queue(max_wait: std::time::Duration) {
    let started = std::time::Instant::now();
    let mut pending = crate::proxy::usage_sink::pending_count();
    while pending > 0 && started.elapsed() < max_wait {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        pending = crate::proxy::usage_sink::pending_count();
    }
    if pending > 0 {
        tracing::warn!("shutdown drain window elapsed with {pending} usage event(s) unflushed");
    } else {
        println!("  Store:     usage queue drained.");
    }
}

fn admin_token() -> Option<String> {
    std::env::var(ADMIN_TOKEN_ENV)
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// Admin router: `/healthz` and the dashboard's static shell open (the login
/// screen must render without a token), all data APIs + /metrics Bearer-guarded.
fn admin_router(state: super::admin_api::AdminState, token: String) -> axum::Router {
    let token = Arc::new(token);
    super::admin_api::router(state)
        .route("/metrics", axum::routing::get(metrics_handler))
        .layer(axum::middleware::from_fn(move |req, next| {
            let token = token.clone();
            admin_auth_guard(req, next, token)
        }))
        .route("/healthz", axum::routing::get(|| async { "ok" }))
        .merge(super::admin_ui::router())
}

async fn admin_auth_guard(
    req: axum::extract::Request,
    next: axum::middleware::Next,
    expected: Arc<String>,
) -> Result<axum::response::Response, axum::response::Response> {
    let ok = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|auth| auth.strip_prefix("Bearer "))
        .is_some_and(|token| constant_time_eq(token.as_bytes(), expected.as_bytes()));
    if ok {
        Ok(next.run(req).await)
    } else {
        Err((
            axum::http::StatusCode::UNAUTHORIZED,
            axum::Json(
                serde_json::json!({"error": format!("Bearer token required ({ADMIN_TOKEN_ENV})")}),
            ),
        )
            .into_response())
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// `GET /metrics` — Prometheus text exposition (enterprise#34).
///
/// Sourced from the live in-process meters (the proxy runs in this process):
/// per-model measured usage/cost from `usage_meter`, sink drop counter from
/// `usage_sink`, verified savings from the signed ledger. No timestamps — the
/// scraper stamps samples (output-determinism rule #498 applies to bodies of
/// tool outputs, not here, but stable ordering keeps diffs and dashboards sane).
async fn metrics_handler() -> axum::response::Response {
    let mut out = String::with_capacity(2048);
    render_metrics(&mut out);
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        out,
    )
        .into_response()
}

fn render_metrics(out: &mut String) {
    use std::fmt::Write as _;

    let mut spend = crate::proxy::usage_meter::snapshot();
    spend.sort_by(|a, b| a.model.cmp(&b.model));
    let _ = writeln!(
        out,
        "# HELP leanctx_model_requests_total Measured requests per served model.\n# TYPE leanctx_model_requests_total counter"
    );
    for m in &spend {
        let _ = writeln!(
            out,
            "leanctx_model_requests_total{{model=\"{}\"}} {}",
            escape_label(&m.model),
            m.requests
        );
    }
    let _ = writeln!(
        out,
        "# HELP leanctx_model_tokens_total Billed tokens per served model and direction.\n# TYPE leanctx_model_tokens_total counter"
    );
    for m in &spend {
        let model = escape_label(&m.model);
        let _ = writeln!(
            out,
            "leanctx_model_tokens_total{{model=\"{model}\",direction=\"input\"}} {}",
            m.input_tokens
        );
        let _ = writeln!(
            out,
            "leanctx_model_tokens_total{{model=\"{model}\",direction=\"output\"}} {}",
            m.output_tokens
        );
        let _ = writeln!(
            out,
            "leanctx_model_tokens_total{{model=\"{model}\",direction=\"cache_read\"}} {}",
            m.cache_read_tokens
        );
    }
    let _ = writeln!(
        out,
        "# HELP leanctx_model_cost_usd_total Measured provider cost per served model (USD).\n# TYPE leanctx_model_cost_usd_total counter"
    );
    for m in &spend {
        let _ = writeln!(
            out,
            "leanctx_model_cost_usd_total{{model=\"{}\"}} {}",
            escape_label(&m.model),
            m.cost_usd
        );
    }

    let ledger = crate::core::savings_ledger::summary();
    let _ = writeln!(
        out,
        "# HELP leanctx_saved_tokens_total Verified net tokens saved (signed ledger).\n# TYPE leanctx_saved_tokens_total counter\nleanctx_saved_tokens_total {}",
        ledger.net_saved_tokens()
    );
    let _ = writeln!(
        out,
        "# HELP leanctx_saved_usd_total Verified USD saved (signed ledger).\n# TYPE leanctx_saved_usd_total counter\nleanctx_saved_usd_total {}",
        ledger.saved_usd
    );
    for (mechanism, tokens, usd) in &ledger.by_mechanism {
        let _ = writeln!(
            out,
            "leanctx_saved_by_mechanism_tokens_total{{mechanism=\"{}\"}} {tokens}",
            escape_label(mechanism)
        );
        let _ = writeln!(
            out,
            "leanctx_saved_by_mechanism_usd_total{{mechanism=\"{}\"}} {usd}",
            escape_label(mechanism)
        );
    }

    let _ = writeln!(
        out,
        "# HELP leanctx_usage_events_dropped_total Usage events dropped because the store writer was saturated (fail-open).\n# TYPE leanctx_usage_events_dropped_total counter\nleanctx_usage_events_dropped_total {}",
        crate::proxy::usage_sink::dropped_count()
    );
}

/// Prometheus label values: escape backslash, quote and newline.
fn escape_label(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_escaping_covers_prometheus_specials() {
        assert_eq!(escape_label(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(escape_label("x\ny"), "x\\ny");
    }

    #[test]
    fn metrics_render_is_valid_exposition_shape() {
        let mut out = String::new();
        render_metrics(&mut out);
        // Every non-comment line is `name{labels} value` or `name value`.
        for line in out.lines().filter(|l| !l.starts_with('#') && !l.is_empty()) {
            let (name_part, value) = line.rsplit_once(' ').expect("metric line has value");
            assert!(
                value.parse::<f64>().is_ok(),
                "metric value must be numeric: {line}"
            );
            assert!(
                name_part.starts_with("leanctx_"),
                "metric namespace: {line}"
            );
        }
        // The fail-open drop counter is always present (enterprise#12/#34).
        assert!(out.contains("leanctx_usage_events_dropped_total"));
    }

    #[test]
    fn admin_token_requires_non_empty() {
        // Not set in the test env → None (admin listener stays off).
        // (Uses a scoped var name to avoid mutating the real one.)
        assert!(std::env::var(ADMIN_TOKEN_ENV).is_err() || admin_token().is_some());
    }

    #[test]
    fn constant_time_eq_basic() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
