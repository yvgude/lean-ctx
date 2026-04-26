mod auth;
mod buddy;
mod cep;
mod commands;
mod config;
mod contribute;
mod db;
mod feedback;
mod gotchas;
mod knowledge;
mod models;
mod stats;

use axum::routing::{get, post};
use axum::Router;
use tower_http::cors::{AllowOrigin, CorsLayer};

pub async fn run() -> anyhow::Result<()> {
    let cfg = config::Config::from_env()?;
    let pool = db::pool_from_database_url(&cfg.database_url)?;
    db::init_schema(&pool).await?;

    let mailer = if cfg.smtp_enabled() {
        let m = auth::Mailer::new(&cfg)?;
        eprintln!("[cloud] SMTP mailer configured (host: {})", cfg.smtp_host.as_deref().unwrap_or("?"));
        m.test_connection().await;
        Some(m)
    } else {
        eprintln!("[cloud] WARNING: SMTP not configured — email verification and password reset will NOT work");
        eprintln!("[cloud] Set LEANCTX_CLOUD_SMTP_HOST, _PORT, _USERNAME, _PASSWORD, _FROM to enable");
        None
    };

    let state = auth::AppState::new(pool, cfg.clone(), mailer);

    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list([
            "https://leanctx.com".parse().unwrap(),
            "https://www.leanctx.com".parse().unwrap(),
            "http://localhost:4321".parse().unwrap(),
        ]))
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::OPTIONS,
        ])
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::AUTHORIZATION,
            axum::http::header::ACCEPT,
        ])
        .allow_credentials(true);

    let app = Router::new()
        .route("/health", get(auth::health))
        .route("/api/auth/register", post(auth::register))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/forgot-password", post(auth::forgot_password))
        .route("/api/auth/reset-password", post(auth::reset_password))
        .route("/api/auth/verify-email", get(auth::verify_email))
        .route(
            "/api/auth/resend-verification",
            post(auth::resend_verification),
        )
        .route("/api/auth/me", get(auth::me))
        .route("/api/stats", get(stats::get_stats).post(stats::post_stats))
        .route("/api/contribute", post(contribute::post_contribute))
        .route(
            "/api/sync/knowledge",
            get(knowledge::get_knowledge).post(knowledge::post_knowledge),
        )
        .route(
            "/api/sync/commands",
            get(commands::get_commands).post(commands::post_commands),
        )
        .route("/api/sync/cep", get(cep::get_cep).post(cep::post_cep))
        .route(
            "/api/sync/gotchas",
            get(gotchas::get_gotchas).post(gotchas::post_gotchas),
        )
        .route(
            "/api/sync/buddy",
            get(buddy::get_buddy).post(buddy::post_buddy),
        )
        .route(
            "/api/sync/feedback",
            get(feedback::get_feedback).post(feedback::post_feedback),
        )
        .route("/api/cloud/models", get(models::get_models))
        .route("/api/pro/models", get(models::get_models))
        .with_state(state)
        .layer(cors);

    let listener = tokio::net::TcpListener::bind(cfg.bind_addr()).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
