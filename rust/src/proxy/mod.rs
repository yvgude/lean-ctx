pub mod anthropic;
pub mod compress;
pub mod forward;
pub mod google;
pub mod openai;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get},
    Router,
};

#[derive(Clone)]
pub struct ProxyState {
    pub client: reqwest::Client,
    pub port: u16,
    pub stats: Arc<ProxyStats>,
}

pub struct ProxyStats {
    pub requests_total: AtomicU64,
    pub requests_compressed: AtomicU64,
    pub tokens_saved: AtomicU64,
    pub bytes_original: AtomicU64,
    pub bytes_compressed: AtomicU64,
}

impl Default for ProxyStats {
    fn default() -> Self {
        Self {
            requests_total: AtomicU64::new(0),
            requests_compressed: AtomicU64::new(0),
            tokens_saved: AtomicU64::new(0),
            bytes_original: AtomicU64::new(0),
            bytes_compressed: AtomicU64::new(0),
        }
    }
}

impl ProxyStats {
    pub fn record_request(&self) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_compression(&self, original: usize, compressed: usize) {
        self.requests_compressed.fetch_add(1, Ordering::Relaxed);
        self.bytes_original
            .fetch_add(original as u64, Ordering::Relaxed);
        self.bytes_compressed
            .fetch_add(compressed as u64, Ordering::Relaxed);
        let saved_tokens = (original.saturating_sub(compressed) / 4) as u64;
        self.tokens_saved.fetch_add(saved_tokens, Ordering::Relaxed);
    }

    pub fn compression_ratio(&self) -> f64 {
        let original = self.bytes_original.load(Ordering::Relaxed);
        if original == 0 {
            return 0.0;
        }
        let compressed = self.bytes_compressed.load(Ordering::Relaxed);
        (1.0 - compressed as f64 / original as f64) * 100.0
    }
}

pub async fn start_proxy(port: u16) -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_mins(2))
        .build()?;

    let state = ProxyState {
        client,
        port,
        stats: Arc::new(ProxyStats::default()),
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/status", get(status_handler))
        .route("/v1/messages", any(anthropic::handler))
        .route("/v1/chat/completions", any(openai::handler))
        .fallback(fallback_router)
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    println!("lean-ctx proxy listening on http://{addr}");
    println!("  Anthropic: POST /v1/messages");
    println!("  OpenAI:    POST /v1/chat/completions");
    println!("  Gemini:    POST /v1beta/models/...");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn status_handler(State(state): State<ProxyState>) -> impl IntoResponse {
    use std::sync::atomic::Ordering::Relaxed;
    let s = &state.stats;
    let body = serde_json::json!({
        "status": "running",
        "port": state.port,
        "requests_total": s.requests_total.load(Relaxed),
        "requests_compressed": s.requests_compressed.load(Relaxed),
        "tokens_saved": s.tokens_saved.load(Relaxed),
        "bytes_original": s.bytes_original.load(Relaxed),
        "bytes_compressed": s.bytes_compressed.load(Relaxed),
        "compression_ratio_pct": format!("{:.1}", s.compression_ratio()),
    });
    (StatusCode::OK, axum::Json(body))
}

async fn fallback_router(State(state): State<ProxyState>, req: Request<Body>) -> Response {
    let path = req.uri().path().to_string();

    if path.starts_with("/v1beta/models/") || path.starts_with("/v1/models/") {
        match google::handler(State(state), req).await {
            Ok(resp) => resp,
            Err(status) => Response::builder()
                .status(status)
                .body(Body::from("proxy error"))
                .expect("BUG: building error response with valid status should never fail"),
        }
    } else {
        let method = req.method().to_string();
        eprintln!("lean-ctx proxy: unmatched {method} {path}");
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from(format!(
                "lean-ctx proxy: no handler for {method} {path}"
            )))
            .expect("BUG: building 404 response should never fail")
    }
}
