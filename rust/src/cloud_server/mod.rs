mod auth;
mod config;
mod contribute;
mod db;
mod global_stats;
mod invite;
mod knowledge;
mod leaderboard;
mod models;
pub(crate) mod profile;
mod stats;

use axum::routing::{get, post};
use axum::Router;
use axum::http::{HeaderName, Method};
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
            "https://leanctx.com".parse().unwrap(),
            "https://www.leanctx.com".parse().unwrap(),
        ]))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PATCH,
            Method::OPTIONS,
        ])
        .allow_headers([
            HeaderName::from_static("content-type"),
            HeaderName::from_static("authorization"),
        ])
        .allow_credentials(true);

    let app = Router::new()
        .route("/health", get(auth::health))
        .route("/api/auth/register", post(auth::register))
        .route("/api/auth/request-link", post(auth::request_magic_link))
        .route("/api/auth/exchange", get(auth::exchange_magic_link))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/stats", get(stats::get_stats).post(stats::post_stats))
        .route("/api/contribute", post(contribute::post_contribute))
        .route(
            "/api/sync/knowledge",
            get(knowledge::get_knowledge).post(knowledge::post_knowledge),
        )
        .route("/api/cloud/models", get(models::get_models))
        .route("/api/pro/models", get(models::get_models))
        .route("/api/leaderboard", get(leaderboard::get_leaderboard))
        .route("/api/leaderboard/teams", get(leaderboard::get_team_leaderboard))
        .route("/api/global-stats", get(global_stats::get_global_stats))
        .route("/api/profile", get(profile::get_profile).post(profile::patch_profile))
        .route("/api/profile/leave-team", post(profile::leave_team))
        .route("/api/profile/rename-team", post(profile::rename_team))
        .route("/api/invite/generate", post(invite::generate_invite))
        .route("/api/invite/:code", get(invite::get_invite_info))
        .route("/api/invite/:code/accept", post(invite::accept_invite))
        .with_state(state)
        .layer(cors);

    let listener = tokio::net::TcpListener::bind(cfg.bind_addr()).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

