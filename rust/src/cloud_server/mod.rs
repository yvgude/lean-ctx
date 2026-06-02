mod auth;
mod buddy;
mod cep;
mod commands;
mod config;
mod contribute;
mod db;
mod feedback;
mod gain;
mod global_stats;
mod gotchas;
mod helpers;
mod knowledge;
mod models;
mod oauth;
mod stats;
mod wrapped;

use axum::routing::{get, post};
use axum::Router;
use tower_http::cors::{AllowOrigin, CorsLayer};

pub async fn run() -> anyhow::Result<()> {
    let cfg = config::Config::from_env()?;
    let pool = db::pool_from_database_url(&cfg.database_url)?;
    db::init_schema(&pool).await?;

    let mailer = if cfg.smtp_enabled() {
        Some(auth::Mailer::new(&cfg)?)
    } else {
        None
    };

    let state = auth::AppState::new(pool, cfg.clone(), mailer);

    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list([
            "https://leanctx.com"
                .parse()
                .expect("BUG: invalid hardcoded URL"),
            "https://www.leanctx.com"
                .parse()
                .expect("BUG: invalid hardcoded URL"),
            "http://localhost:4321"
                .parse()
                .expect("BUG: invalid hardcoded URL"),
        ]))
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::DELETE,
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
        .route("/oauth/register", post(oauth::register_client))
        .route("/oauth/token", post(oauth::token))
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
        .route("/api/sync/gain", get(gain::get_gain).post(gain::post_gain))
        .route(
            "/api/wrapped",
            post(wrapped::publish).layer(axum::extract::DefaultBodyLimit::max(8 * 1024)),
        )
        .route(
            "/api/wrapped/{id}",
            get(wrapped::get_card).delete(wrapped::delete_card),
        )
        .route("/api/wrapped/{id}/card.svg", get(wrapped::get_card_svg))
        .route("/api/wrapped/{id}/card.png", get(wrapped::get_card_png))
        .route("/api/wrapped/{id}/claim", post(wrapped::claim_card))
        .route("/w/{id}", get(wrapped::get_permalink_page))
        .route("/api/leaderboard", get(wrapped::leaderboard))
        .route("/leaderboard", get(wrapped::get_leaderboard_page))
        .route("/api/global-stats", get(global_stats::get_global_stats))
        .route("/api/cloud/models", get(models::get_models))
        .with_state(state)
        .layer(cors)
        .layer(axum::extract::DefaultBodyLimit::max(1024 * 1024));

    let listener = tokio::net::TcpListener::bind(cfg.bind_addr()).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
