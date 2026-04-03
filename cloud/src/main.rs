use lean_ctx_cloud::{build_router, db};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse().unwrap()))
        .init();

    let db_path = db::db_path();
    tracing::info!("Database: {db_path}");
    let pool = db::init_db(&db_path);

    let app = build_router(pool);

    let port = std::env::var("PORT").unwrap_or_else(|_| "3334".to_string());
    let addr = format!("0.0.0.0:{port}");
    tracing::info!("LeanCTX Cloud API listening on {addr}");

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
