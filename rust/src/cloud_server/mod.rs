mod account_admin;
mod account_cloud;
mod auth;
mod billing_edge;
mod buddy;
mod cep;
mod commands;
mod config;
mod contribute;
mod db;
mod devices;
mod digest;
mod feedback;
mod gain;
mod global_stats;
mod gotchas;
mod helpers;
mod index_sync;
mod knowledge;
mod models;
mod oauth;
mod site_theme;
mod sso;
mod stats;
mod team_join;
mod wrapped;

use axum::Router;
use axum::routing::{delete, get, patch, post, put};
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

    // Email digests (GL #386): monthly Pro / weekly Team summaries with
    // one-click opt-out. No-op while SMTP is unconfigured.
    digest::spawn_digest_job(state.clone());

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
            axum::http::Method::PUT,
            axum::http::Method::PATCH,
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
        // Self-serve OIDC SSO (GL #482): start → IdP → callback → handoff.
        .route("/api/auth/sso/start", post(sso::sso_start))
        .route("/api/auth/sso/callback", get(sso::sso_callback))
        .route("/api/auth/sso/handoff", post(sso::sso_handoff))
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
        // Hosted Personal Index (GL #392): encrypted bundles, Pro-gated via
        // require_cloud_sync. The PUT carries up to 64 MB of ciphertext, so it
        // overrides the global 1 MB body limit route-locally.
        .route("/api/sync/index", get(index_sync::list_bundles))
        .route(
            "/api/sync/index/{project_hash}",
            put(index_sync::put_bundle)
                .get(index_sync::get_bundle)
                .delete(index_sync::delete_bundle)
                .layer(axum::extract::DefaultBodyLimit::max(
                    index_sync::MAX_BUNDLE_BYTES,
                )),
        )
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
        // Public supporters wall — proxied from the private billing plane; empty
        // (never an error) when billing is unset, so the website always renders.
        .route("/api/supporters", get(billing_edge::get_supporters))
        .route(
            "/api/supporters/checkout",
            post(billing_edge::post_supporter_checkout),
        )
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
        // Personal Cloud dashboard: the `cloud_sync` entitlement gate + a
        // privacy-preserving footprint of what the account has synced. Drives
        // the dashboard-vs-upsell split on /account/cloud for every plan.
        .route("/api/account/cloud", get(account_cloud::get_account_cloud))
        // Account self-service (GL #535): full data export (GDPR Art. 20) and
        // irreversible deletion (Art. 17) — billing dies first, then the
        // users row cascades through every synced table.
        .route("/api/account/export", get(account_admin::export_account))
        .route("/api/account", delete(account_admin::delete_account))
        // Device overview (GL #387): list machines that synced (from the
        // X-Device-Label header on pushes) + forget a stale row. Display
        // metadata only — no auth or quota semantics attached to a device.
        .route("/api/account/devices", get(devices::list_devices))
        .route(
            "/api/account/devices/{label}",
            delete(devices::forget_device),
        )
        // Hosted Team server dashboard: proxy status + token management to the
        // private plane on behalf of the logged-in owner. 503 when billing is
        // unset; 404 (from the plane) until a Team subscription provisions one.
        // Email digests (GL #386): one-click unsubscribe (from the email link,
        // no login) + the authenticated dashboard toggle.
        .route("/api/digest/opt-out", get(digest::opt_out))
        .route(
            "/api/account/digest",
            get(digest::get_digest_pref).put(digest::put_digest_pref),
        )
        .route("/api/account/team", get(billing_edge::get_account_team))
        .route(
            "/api/account/team/savings",
            get(billing_edge::get_account_team_savings),
        )
        .route(
            "/api/account/team/savings/member/{signer}",
            get(billing_edge::get_account_team_savings_member),
        )
        .route(
            "/api/account/team/settings",
            axum::routing::put(billing_edge::put_account_team_settings),
        )
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
        // Invite links (GL #385): owner-side mint/list/revoke plus the public,
        // login-less redeem behind leanctx.com/join/?code=… (rate-limited).
        .route(
            "/api/account/team/invites",
            get(billing_edge::get_account_team_invites)
                .post(billing_edge::post_account_team_invite),
        )
        .route(
            "/api/account/team/invites/{invite_id}",
            delete(billing_edge::delete_account_team_invite),
        )
        .route("/api/team/join", post(team_join::post_team_join))
        // Org SSO settings (GL #482): owner-side IdP config on the dashboard.
        .route(
            "/api/account/org/sso",
            get(billing_edge::get_account_org_sso)
                .put(billing_edge::put_account_org_sso)
                .delete(billing_edge::delete_account_org_sso),
        )
        .route(
            "/api/account/org/sso/verify",
            post(billing_edge::post_account_org_sso_verify),
        )
        .route(
            "/api/account/org/sso/required",
            put(billing_edge::put_account_org_sso_required),
        )
        // Org audit log (GL #484): owner-side governance history + CSV export.
        .route(
            "/api/account/org/audit",
            get(billing_edge::get_account_org_audit),
        )
        .route(
            "/api/account/org/audit/export.csv",
            get(billing_edge::get_account_org_audit_export),
        )
        // ctxpkg registry publisher self-service (GL #406): namespace claim +
        // publish-token lifecycle. Publishing itself goes through ctxpkg.com.
        .route(
            "/api/account/registry",
            get(billing_edge::get_account_registry),
        )
        .route(
            "/api/account/registry/namespace",
            put(billing_edge::put_account_registry_namespace),
        )
        .route(
            "/api/account/registry/tokens",
            post(billing_edge::post_account_registry_token),
        )
        .route(
            "/api/account/registry/tokens/{token_id}",
            delete(billing_edge::delete_account_registry_token),
        )
        // Verified Publisher (GL #516): DNS-TXT domain verification.
        .route(
            "/api/account/registry/domains",
            post(billing_edge::post_account_registry_domain),
        )
        .route(
            "/api/account/registry/domains/{domain_id}/verify",
            post(billing_edge::post_account_registry_domain_verify),
        )
        .route(
            "/api/account/registry/domains/{domain_id}",
            delete(billing_edge::delete_account_registry_domain),
        )
        // Paid Packs v0 (GL #529): publisher pricing + buyer checkout.
        .route(
            "/api/account/registry/price",
            put(billing_edge::put_account_registry_price),
        )
        .route(
            "/api/account/registry/buy",
            post(billing_edge::post_account_registry_buy),
        )
        // Team seats (prorated Stripe quantity), hosted-index storage footprint,
        // and managed connectors — thin proxies to the private plane so the hosted
        // team dashboard's seat stepper, storage card, and connector manager work.
        .route(
            "/api/account/team/seats",
            post(billing_edge::post_account_team_seats),
        )
        .route(
            "/api/account/team/storage",
            get(billing_edge::get_account_team_storage),
        )
        .route(
            "/api/account/team/connectors",
            get(billing_edge::get_account_team_connectors)
                .post(billing_edge::post_account_team_connector),
        )
        .route(
            "/api/account/team/connectors/{connector_id}",
            patch(billing_edge::patch_account_team_connector)
                .delete(billing_edge::delete_account_team_connector),
        )
        .with_state(state)
        .layer(cors)
        .layer(axum::extract::DefaultBodyLimit::max(1024 * 1024));

    let listener = tokio::net::TcpListener::bind(cfg.bind_addr()).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
