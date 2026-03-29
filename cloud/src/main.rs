mod auth;
mod db;
mod routes;

use axum::{routing::{get, post}, Router};
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse().unwrap()))
        .init();

    let db_path = db::db_path();
    tracing::info!("Database: {db_path}");
    let pool = db::init_db(&db_path);

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/api/auth/register", post(routes::auth::register))
        .route("/api/auth/me", get(routes::auth::me))
        .route("/api/stats", post(routes::stats::upload_stats))
        .route("/api/stats", get(routes::stats::get_stats))
        .route("/api/stats/summary", get(routes::stats::get_summary))
        .route("/api/checkout", post(routes::stripe::create_checkout))
        .route("/api/webhooks/stripe", post(routes::stripe::webhook))
        .route("/api/contribute", post(routes::contribute::contribute))
        .route("/api/contribute/stats", get(routes::contribute::collective_stats))
        .route("/api/sync/knowledge", post(routes::sync::push_knowledge))
        .route("/api/sync/knowledge", get(routes::sync::pull_knowledge))
        .route("/health", get(health))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(pool);

    let port = std::env::var("PORT").unwrap_or_else(|_| "3334".to_string());
    let addr = format!("0.0.0.0:{port}");
    tracing::info!("LeanCTX Cloud API listening on {addr}");

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn health() -> &'static str {
    "ok"
}
