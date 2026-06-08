mod auth;
mod billing_edge;
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
mod site_theme;
mod stats;
mod wrapped;

use axum::routing::{delete, get, post};
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
            // Hard memory cap (DoS defence-in-depth); the documented 8 KB limit is enforced
            // inside the handler so oversized bodies get the JSON `payload_too_large` envelope.
            "/api/wrapped",
            post(wrapped::publish).layer(axum::extract::DefaultBodyLimit::max(64 * 1024)),
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
        .route(
            // Edge to the private commercial plane: resolves the caller's plan +
            // additive entitlements. Free (gates nothing) when billing is unset.
            "/api/account/entitlements",
            get(billing_edge::get_account_entitlements),
        )
        // Self-serve billing: proxy Checkout / Portal to the private plane so the
        // shared internal key never reaches the browser. 503 when billing is unset.
        .route(
            "/api/account/checkout",
            post(billing_edge::post_account_checkout),
        )
        .route(
            "/api/account/portal",
            post(billing_edge::post_account_portal),
        )
        // Hosted Team server dashboard: proxy status + token management to the
        // private plane on behalf of the logged-in owner. 503 when billing is
        // unset; 404 (from the plane) until a Team subscription provisions one.
        .route("/api/account/team", get(billing_edge::get_account_team))
        .route(
            "/api/account/team/owner-token",
            post(billing_edge::post_account_team_owner_token),
        )
        .route(
            "/api/account/team/members",
            post(billing_edge::post_account_team_member),
        )
        .route(
            "/api/account/team/members/{token_id}",
            delete(billing_edge::delete_account_team_member),
        )
        .with_state(state)
        .layer(cors)
        .layer(axum::extract::DefaultBodyLimit::max(1024 * 1024));

    let listener = tokio::net::TcpListener::bind(cfg.bind_addr()).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
