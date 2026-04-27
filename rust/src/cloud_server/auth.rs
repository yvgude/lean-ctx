use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use chrono::{DateTime, Duration, Utc};
use deadpool_postgres::Pool;
use lettre::message::{Mailbox, Message};
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::config::Config;
use super::helpers::internal_error;

pub async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

#[derive(Clone)]
pub struct AppState {
    pub pool: Pool,
    pub cfg: Config,
    #[allow(dead_code)]
    pub jwt_secret: std::sync::Arc<Vec<u8>>,
    pub mailer: Option<Mailer>,
}

impl AppState {
    pub fn new(pool: Pool, cfg: Config, mailer: Option<Mailer>) -> Self {
        let jwt_secret = std::sync::Arc::new(cfg.jwt_secret.as_bytes().to_vec());
        Self {
            pool,
            cfg,
            jwt_secret,
            mailer,
        }
    }
}

// ─── Mailer ───────────────────────────────────────────────────

#[derive(Clone)]
pub struct Mailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

impl Mailer {
    pub fn new(cfg: &Config) -> anyhow::Result<Self> {
        let host = cfg.smtp_host.as_deref().unwrap_or("");
        let port = cfg.smtp_port.unwrap_or(587);
        let username = cfg.smtp_username.as_deref().unwrap_or("");
        let password = cfg.smtp_password.as_deref().unwrap_or("");
        let from = cfg.smtp_from.as_deref().unwrap_or("");

        let from: Mailbox = from.parse()?;
        let creds = lettre::transport::smtp::authentication::Credentials::new(
            username.to_string(),
            password.to_string(),
        );

        let mut builder = AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)?;
        builder = builder.port(port).credentials(creds);
        let transport = builder.build();

        Ok(Self { transport, from })
    }

    pub async fn send_verification(&self, to_email: &str, link: &str) -> anyhow::Result<()> {
        let to: Mailbox = to_email.parse()?;
        let email = Message::builder()
            .from(self.from.clone())
            .to(to)
            .subject("Verify your LeanCTX account")
            .body(format!(
                "Welcome to LeanCTX!\n\nPlease verify your email address:\n\n{link}\n\nThis link expires in 24 hours."
            ))?;
        self.transport.send(email).await?;
        Ok(())
    }

    pub async fn send_password_reset(&self, to_email: &str, link: &str) -> anyhow::Result<()> {
        let to: Mailbox = to_email.parse()?;
        let email = Message::builder()
            .from(self.from.clone())
            .to(to)
            .subject("Reset your LeanCTX password")
            .body(format!(
                "You requested a password reset for your LeanCTX account.\n\nClick to reset your password:\n\n{link}\n\nThis link expires in 1 hour.\nIf you didn't request this, ignore this email."
            ))?;
        self.transport.send(email).await?;
        Ok(())
    }
}

// ─── POST /api/auth/register ──────────────────────────────────

#[derive(Deserialize)]
pub struct RegisterBody {
    pub email: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct RegisterResponse {
    pub api_key: String,
    pub user_id: String,
    pub email_verified: bool,
    pub verification_sent: bool,
}

pub async fn register(
    State(state): State<AppState>,
    Json(body): Json<RegisterBody>,
) -> Result<Json<RegisterResponse>, (StatusCode, String)> {
    let email = body.email.trim().to_lowercase();
    if !email.contains('@') || !email.contains('.') {
        return Err((StatusCode::BAD_REQUEST, "Invalid email".into()));
    }
    if body.password.len() < 8 {
        return Err((
            StatusCode::BAD_REQUEST,
            "Password must be at least 8 characters".into(),
        ));
    }

    let existing = lookup_user_credentials(&state.pool, &email)
        .await
        .map_err(internal_error)?;
    if let Some((user_id, stored_hash)) = existing {
        if stored_hash.is_some() {
            return Err((
                StatusCode::CONFLICT,
                "An account with this email already exists. Please sign in.".into(),
            ));
        }
        let password_hash = hash_password(&body.password);
        update_password(&state.pool, user_id, &password_hash)
            .await
            .map_err(internal_error)?;

        let api_key = generate_api_key();
        let api_key_sha = sha256_hex(&api_key);
        rotate_api_key(&state.pool, user_id, &api_key_sha)
            .await
            .map_err(internal_error)?;

        let mut verification_sent = false;
        if let Some(ref mailer) = state.mailer {
            let token = generate_token();
            let token_sha = sha256_hex(&token);
            let expires_at = Utc::now() + Duration::hours(24);
            store_email_verification(&state.pool, &token_sha, user_id, expires_at)
                .await
                .map_err(internal_error)?;
            let link = format!(
                "{}/api/auth/verify-email?token={}",
                state.cfg.api_base_url.trim_end_matches('/'),
                token
            );
            if mailer.send_verification(&email, &link).await.is_ok() {
                verification_sent = true;
            }
        }

        return Ok(Json(RegisterResponse {
            api_key,
            user_id: user_id.to_string(),
            email_verified: false,
            verification_sent,
        }));
    }

    let password_hash = hash_password(&body.password);
    let (user_id, _is_new) = upsert_user(&state.pool, &email, Some(&password_hash))
        .await
        .map_err(internal_error)?;

    let api_key = generate_api_key();
    let api_key_sha = sha256_hex(&api_key);
    rotate_api_key(&state.pool, user_id, &api_key_sha)
        .await
        .map_err(internal_error)?;

    let mut verification_sent = false;
    if let Some(ref mailer) = state.mailer {
        let token = generate_token();
        let token_sha = sha256_hex(&token);
        let expires_at = Utc::now() + Duration::hours(24);
        store_email_verification(&state.pool, &token_sha, user_id, expires_at)
            .await
            .map_err(internal_error)?;
        let link = format!(
            "{}/api/auth/verify-email?token={}",
            state.cfg.api_base_url.trim_end_matches('/'),
            token
        );
        if mailer.send_verification(&email, &link).await.is_ok() {
            verification_sent = true;
        }
    }

    Ok(Json(RegisterResponse {
        api_key,
        user_id: user_id.to_string(),
        email_verified: false,
        verification_sent,
    }))
}

// ─── POST /api/auth/login ─────────────────────────────────────

#[derive(Deserialize)]
pub struct LoginBody {
    pub email: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub api_key: String,
    pub user_id: String,
    pub email_verified: bool,
}

pub async fn login(
    State(state): State<AppState>,
    Json(body): Json<LoginBody>,
) -> Result<Json<LoginResponse>, (StatusCode, String)> {
    let email = body.email.trim().to_lowercase();
    if email.is_empty() || body.password.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Email and password required".into(),
        ));
    }

    let (user_id, stored_hash) = lookup_user_credentials(&state.pool, &email)
        .await
        .map_err(internal_error)?
        .ok_or((StatusCode::UNAUTHORIZED, "Invalid email or password".into()))?;

    let stored_hash =
        stored_hash.ok_or((StatusCode::UNAUTHORIZED, "Invalid email or password".into()))?;

    if !verify_password(&body.password, &stored_hash) {
        return Err((StatusCode::UNAUTHORIZED, "Invalid email or password".into()));
    }

    let email_verified = is_email_verified(&state.pool, user_id)
        .await
        .map_err(internal_error)?;

    if !email_verified {
        // Resend verification email on login attempt if not verified
        if let Some(ref mailer) = state.mailer {
            let token = generate_token();
            let token_sha = sha256_hex(&token);
            let expires_at = Utc::now() + Duration::hours(24);
            store_email_verification(&state.pool, &token_sha, user_id, expires_at)
                .await
                .map_err(internal_error)?;
            let link = format!(
                "{}/api/auth/verify-email?token={}",
                state.cfg.api_base_url.trim_end_matches('/'),
                token
            );
            let _ = mailer.send_verification(&email, &link).await;
        }
        return Err((
            StatusCode::FORBIDDEN,
            "Please verify your email before signing in. A new verification email has been sent."
                .into(),
        ));
    }

    let api_key = generate_api_key();
    let api_key_sha = sha256_hex(&api_key);
    rotate_api_key(&state.pool, user_id, &api_key_sha)
        .await
        .map_err(internal_error)?;

    Ok(Json(LoginResponse {
        api_key,
        user_id: user_id.to_string(),
        email_verified,
    }))
}

// ─── POST /api/auth/forgot-password ───────────────────────────

#[derive(Deserialize)]
pub struct ForgotPasswordBody {
    pub email: String,
}

pub async fn forgot_password(
    State(state): State<AppState>,
    Json(body): Json<ForgotPasswordBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let email = body.email.trim().to_lowercase();
    if !email.contains('@') {
        return Err((StatusCode::BAD_REQUEST, "Invalid email".into()));
    }

    // Always return success to avoid email enumeration
    let user = lookup_user_credentials(&state.pool, &email)
        .await
        .map_err(internal_error)?;

    if let Some((user_id, _)) = user {
        if let Some(ref mailer) = state.mailer {
            let token = generate_token();
            let token_sha = sha256_hex(&token);
            let expires_at = Utc::now() + Duration::hours(1);
            store_password_reset(&state.pool, &token_sha, user_id, expires_at)
                .await
                .map_err(internal_error)?;
            let link = format!(
                "{}/login?reset_token={}",
                state.cfg.public_base_url.trim_end_matches('/'),
                token
            );
            let _ = mailer.send_password_reset(&email, &link).await;
        }
    }

    Ok(Json(
        serde_json::json!({ "message": "If an account exists, a reset email has been sent." }),
    ))
}

// ─── POST /api/auth/reset-password ────────────────────────────

#[derive(Deserialize)]
pub struct ResetPasswordBody {
    pub token: String,
    pub password: String,
}

pub async fn reset_password(
    State(state): State<AppState>,
    Json(body): Json<ResetPasswordBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if body.password.len() < 8 {
        return Err((
            StatusCode::BAD_REQUEST,
            "Password must be at least 8 characters".into(),
        ));
    }

    let token_sha = sha256_hex(body.token.trim());
    let user_id = consume_password_reset(&state.pool, &token_sha)
        .await
        .map_err(|e| match e {
            ConsumeError::NotFound => (
                StatusCode::UNAUTHORIZED,
                "Invalid or expired reset link".into(),
            ),
            ConsumeError::Db(s) => (StatusCode::INTERNAL_SERVER_ERROR, s),
        })?;

    let new_hash = hash_password(&body.password);
    update_password(&state.pool, user_id, &new_hash)
        .await
        .map_err(internal_error)?;

    // Also verify email since they proved ownership
    mark_email_verified(&state.pool, user_id)
        .await
        .map_err(internal_error)?;

    Ok(Json(
        serde_json::json!({ "message": "Password has been reset. You can now sign in." }),
    ))
}

// ─── GET /api/auth/verify-email ───────────────────────────────

#[derive(Deserialize)]
pub struct VerifyEmailQuery {
    pub token: String,
}

pub async fn verify_email(
    State(state): State<AppState>,
    Query(q): Query<VerifyEmailQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let token = q.token.trim();
    if token.len() < 20 {
        return Err((StatusCode::BAD_REQUEST, "Invalid token".into()));
    }
    let token_sha = sha256_hex(token);
    let user_id = consume_email_verification(&state.pool, &token_sha)
        .await
        .map_err(|e| match e {
            ConsumeError::NotFound => (
                StatusCode::UNAUTHORIZED,
                "Invalid or expired verification link".into(),
            ),
            ConsumeError::Db(s) => (StatusCode::INTERNAL_SERVER_ERROR, s),
        })?;

    mark_email_verified(&state.pool, user_id)
        .await
        .map_err(internal_error)?;

    let redirect_url = format!(
        "{}/login?verified=true",
        state.cfg.public_base_url.trim_end_matches('/')
    );

    Ok(axum::response::Redirect::temporary(&redirect_url))
}

// ─── POST /api/auth/resend-verification ───────────────────────

#[derive(Deserialize)]
pub struct ResendVerificationBody {
    pub email: String,
}

pub async fn resend_verification(
    State(state): State<AppState>,
    Json(body): Json<ResendVerificationBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let email = body.email.trim().to_lowercase();

    if let Some((user_id, _)) = lookup_user_credentials(&state.pool, &email)
        .await
        .map_err(internal_error)?
    {
        let verified = is_email_verified(&state.pool, user_id)
            .await
            .map_err(internal_error)?;
        if !verified {
            if let Some(ref mailer) = state.mailer {
                let token = generate_token();
                let token_sha = sha256_hex(&token);
                let expires_at = Utc::now() + Duration::hours(24);
                store_email_verification(&state.pool, &token_sha, user_id, expires_at)
                    .await
                    .map_err(internal_error)?;
                let link = format!(
                    "{}/api/auth/verify-email?token={}",
                    state.cfg.api_base_url.trim_end_matches('/'),
                    token
                );
                let _ = mailer.send_verification(&email, &link).await;
            }
        }
    }

    Ok(Json(
        serde_json::json!({ "message": "If an unverified account exists, a verification email has been sent." }),
    ))
}

// ─── GET /api/auth/me ─────────────────────────────────────────

#[derive(Serialize)]
pub struct MeResponse {
    pub user_id: String,
    pub email: String,
    pub plan: String,
    pub email_verified: bool,
}

pub async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<MeResponse>, (StatusCode, String)> {
    let (user_id, email) = auth_user(&state, &headers).await?;

    let verified = is_email_verified(&state.pool, user_id)
        .await
        .map_err(internal_error)?;

    Ok(Json(MeResponse {
        user_id: user_id.to_string(),
        email,
        plan: "cloud".to_string(),
        email_verified: verified,
    }))
}

// ─── Auth middleware ──────────────────────────────────────────

pub async fn auth_user(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(Uuid, String), (StatusCode, String)> {
    if let Some(v) = headers.get(axum::http::header::AUTHORIZATION) {
        if let Ok(s) = v.to_str() {
            if let Some(key) = s.strip_prefix("Bearer ").map(str::trim) {
                let sha = sha256_hex(key);
                if let Some((user_id, email)) = lookup_api_key(&state.pool, &sha)
                    .await
                    .map_err(internal_error)?
                {
                    return Ok((user_id, email));
                }
                return Err((StatusCode::UNAUTHORIZED, "Invalid API key".into()));
            }
        }
    }

    Err((StatusCode::UNAUTHORIZED, "Unauthorized".into()))
}

// ─── Database helpers ─────────────────────────────────────────

async fn upsert_user(
    pool: &Pool,
    email: &str,
    password_hash: Option<&str>,
) -> anyhow::Result<(Uuid, bool)> {
    let client = pool.get().await?;
    let row = client
        .query_opt("SELECT id FROM users WHERE email=$1", &[&email])
        .await?;
    if let Some(r) = row {
        let user_id: Uuid = r.get(0);
        if let Some(ph) = password_hash {
            client
                .execute(
                    "UPDATE users SET password_hash=$1 WHERE id=$2 AND password_hash IS NULL",
                    &[&ph, &user_id],
                )
                .await?;
        }
        return Ok((user_id, false));
    }
    let id = Uuid::new_v4();
    client
        .execute(
            "INSERT INTO users (id, email, password_hash) VALUES ($1, $2, $3) ON CONFLICT (email) DO NOTHING",
            &[&id, &email, &password_hash],
        )
        .await?;
    let row = client
        .query_one("SELECT id FROM users WHERE email=$1", &[&email])
        .await?;
    Ok((row.get(0), true))
}

async fn lookup_user_credentials(
    pool: &Pool,
    email: &str,
) -> anyhow::Result<Option<(Uuid, Option<String>)>> {
    let client = pool.get().await?;
    let row = client
        .query_opt(
            "SELECT id, password_hash FROM users WHERE email=$1",
            &[&email],
        )
        .await?;
    Ok(row.map(|r| (r.get(0), r.get(1))))
}

async fn is_email_verified(pool: &Pool, user_id: Uuid) -> anyhow::Result<bool> {
    let client = pool.get().await?;
    let row = client
        .query_one(
            "SELECT email_verified_at IS NOT NULL FROM users WHERE id=$1",
            &[&user_id],
        )
        .await?;
    Ok(row.get(0))
}

async fn mark_email_verified(pool: &Pool, user_id: Uuid) -> anyhow::Result<()> {
    let client = pool.get().await?;
    client
        .execute(
            "UPDATE users SET email_verified_at=NOW() WHERE id=$1 AND email_verified_at IS NULL",
            &[&user_id],
        )
        .await?;
    Ok(())
}

async fn update_password(pool: &Pool, user_id: Uuid, password_hash: &str) -> anyhow::Result<()> {
    let client = pool.get().await?;
    client
        .execute(
            "UPDATE users SET password_hash=$1 WHERE id=$2",
            &[&password_hash, &user_id],
        )
        .await?;
    Ok(())
}

async fn rotate_api_key(pool: &Pool, user_id: Uuid, api_key_sha: &str) -> anyhow::Result<()> {
    let client = pool.get().await?;
    client
        .execute("DELETE FROM api_keys WHERE user_id=$1", &[&user_id])
        .await?;
    let id = Uuid::new_v4();
    client
        .execute(
            "INSERT INTO api_keys (id, user_id, api_key_sha256) VALUES ($1, $2, $3)",
            &[&id, &user_id, &api_key_sha],
        )
        .await?;
    Ok(())
}

async fn lookup_api_key(pool: &Pool, api_key_sha: &str) -> anyhow::Result<Option<(Uuid, String)>> {
    let client = pool.get().await?;
    if let Some(row) = client
        .query_opt(
            "SELECT u.id, u.email FROM api_keys k JOIN users u ON u.id=k.user_id WHERE k.api_key_sha256=$1",
            &[&api_key_sha],
        )
        .await?
    {
        return Ok(Some((row.get(0), row.get(1))));
    }
    Ok(None)
}

async fn store_email_verification(
    pool: &Pool,
    token_sha: &str,
    user_id: Uuid,
    expires_at: DateTime<Utc>,
) -> anyhow::Result<()> {
    let client = pool.get().await?;
    client
        .execute(
            "INSERT INTO email_verifications (token_sha256, user_id, expires_at) VALUES ($1, $2, $3)",
            &[&token_sha, &user_id, &expires_at],
        )
        .await?;
    Ok(())
}

async fn store_password_reset(
    pool: &Pool,
    token_sha: &str,
    user_id: Uuid,
    expires_at: DateTime<Utc>,
) -> anyhow::Result<()> {
    let client = pool.get().await?;
    client
        .execute(
            "INSERT INTO email_verifications (token_sha256, user_id, expires_at) VALUES ($1, $2, $3)",
            &[&token_sha, &user_id, &expires_at],
        )
        .await?;
    Ok(())
}

enum ConsumeError {
    NotFound,
    Db(String),
}

async fn consume_email_verification(pool: &Pool, token_sha: &str) -> Result<Uuid, ConsumeError> {
    let client = pool
        .get()
        .await
        .map_err(|e| ConsumeError::Db(e.to_string()))?;
    let row = client
        .query_opt(
            "SELECT user_id, expires_at, consumed_at FROM email_verifications WHERE token_sha256=$1",
            &[&token_sha],
        )
        .await
        .map_err(|e| ConsumeError::Db(e.to_string()))?;
    let row = row.ok_or(ConsumeError::NotFound)?;
    let user_id: Uuid = row.get(0);
    let expires_at: DateTime<Utc> = row.get(1);
    let consumed_at: Option<DateTime<Utc>> = row.get(2);
    if consumed_at.is_some() || expires_at < Utc::now() {
        return Err(ConsumeError::NotFound);
    }
    client
        .execute(
            "UPDATE email_verifications SET consumed_at=NOW() WHERE token_sha256=$1",
            &[&token_sha],
        )
        .await
        .map_err(|e| ConsumeError::Db(e.to_string()))?;
    Ok(user_id)
}

async fn consume_password_reset(pool: &Pool, token_sha: &str) -> Result<Uuid, ConsumeError> {
    consume_email_verification(pool, token_sha).await
}

// ─── Password hashing (salted SHA256) ─────────────────────────

fn hash_password(password: &str) -> String {
    let salt: [u8; 16] = rand::random();
    let salt_hex = hex::encode(salt);
    let digest = sha256_hex(&format!("{salt_hex}:{password}"));
    format!("{salt_hex}:{digest}")
}

fn verify_password(password: &str, stored: &str) -> bool {
    let parts: Vec<&str> = stored.splitn(2, ':').collect();
    if parts.len() != 2 {
        return false;
    }
    let salt_hex = parts[0];
    let expected_digest = parts[1];
    let actual_digest = sha256_hex(&format!("{salt_hex}:{password}"));
    constant_time_eq(expected_digest.as_bytes(), actual_digest.as_bytes())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ─── Token/key generation ─────────────────────────────────────

fn generate_api_key() -> String {
    let bytes: [u8; 32] = rand::random();
    hex::encode(bytes)
}

fn generate_token() -> String {
    let bytes: [u8; 32] = rand::random();
    hex::encode(bytes)
}

fn sha256_hex(input: &str) -> String {
    let mut h = Sha256::new();
    h.update(input.as_bytes());
    hex::encode(h.finalize())
}
