pub mod auth;
pub mod db;
pub mod routes;

use axum::{routing::{get, post}, Router};
use tower_http::cors::{Any, CorsLayer};

pub fn build_router(pool: db::DbPool) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/api/auth/register", post(routes::auth::register))
        .route("/api/auth/me", get(routes::auth::me))
        .route("/api/stats", post(routes::stats::upload_stats))
        .route("/api/stats", get(routes::stats::get_stats))
        .route("/api/stats/summary", get(routes::stats::get_summary))
        .route("/api/checkout", post(routes::stripe::create_checkout))
        .route("/api/webhooks/stripe", post(routes::stripe::webhook))
        .route("/api/contribute", post(routes::contribute::contribute))
        .route("/api/contribute/stats", get(routes::contribute::collective_stats))
        .route("/api/pro/models", get(routes::pro::get_models))
        .route("/api/admin/overview", get(routes::admin::overview))
        .route("/api/admin/users", get(routes::admin::users))
        .route("/api/admin/collective", get(routes::admin::collective))
        .route("/api/admin/make-admin", post(routes::admin::make_admin))
        .route("/api/sync/knowledge", post(routes::sync::push_knowledge))
        .route("/api/sync/knowledge", get(routes::sync::pull_knowledge))
        .route("/health", get(health))
        .layer(cors)
        .with_state(pool)
}

async fn health() -> &'static str {
    "ok"
}
