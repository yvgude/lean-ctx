use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use chrono::{DateTime, Duration, Utc};
use deadpool_postgres::Pool;
use jsonwebtoken::{EncodingKey, Header, Validation};
use lettre::message::{Mailbox, Message};
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::config::Config;

pub async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

#[derive(Clone)]
pub struct AppState {
    pub pool: Pool,
    pub cfg: Config,
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

    pub async fn send_magic_link(&self, to_email: &str, link: &str) -> anyhow::Result<()> {
        let to: Mailbox = to_email.parse()?;
        let email = Message::builder()
            .from(self.from.clone())
            .to(to)
            .subject("LeanCTX Cloud Login")
            .body(format!(
                "Click to sign in:\n\n{link}\n\nIf you didn't request this, ignore this email."
            ))?;
        self.transport.send(email).await?;
        Ok(())
    }
}

#[derive(Deserialize)]
pub struct RegisterBody {
    pub email: String,
}

#[derive(Serialize)]
pub struct RegisterResponse {
    pub api_key: String,
    pub user_id: String,
}

pub async fn register(
    State(state): State<AppState>,
    Json(body): Json<RegisterBody>,
) -> Result<Json<RegisterResponse>, (StatusCode, String)> {
    let email = body.email.trim().to_lowercase();
    if !email.contains('@') || !email.contains('.') {
        return Err((StatusCode::BAD_REQUEST, "Invalid email".into()));
    }

    let user_id = upsert_user(&state.pool, &email)
        .await
        .map_err(internal_error)?;

    let api_key = generate_api_key();
    let api_key_sha = sha256_hex(&api_key);
    rotate_api_key(&state.pool, user_id, &api_key_sha)
        .await
        .map_err(internal_error)?;

    Ok(Json(RegisterResponse {
        api_key,
        user_id: user_id.to_string(),
    }))
}

#[derive(Deserialize)]
pub struct RequestLinkBody {
    pub email: String,
}

#[derive(Serialize)]
pub struct RequestLinkResponse {
    pub ok: bool,
}

pub async fn request_magic_link(
    State(state): State<AppState>,
    Json(body): Json<RequestLinkBody>,
) -> Result<Json<RequestLinkResponse>, (StatusCode, String)> {
    let email = body.email.trim().to_lowercase();
    if !email.contains('@') || !email.contains('.') {
        return Err((StatusCode::BAD_REQUEST, "Invalid email".into()));
    }
    let mailer = state
        .mailer
        .clone()
        .ok_or((StatusCode::FAILED_DEPENDENCY, "SMTP not configured".into()))?;

    let user_id = upsert_user(&state.pool, &email)
        .await
        .map_err(internal_error)?;

    let token = generate_token();
    let token_sha = sha256_hex(&token);
    let expires_at = Utc::now() + Duration::minutes(15);
    store_magic_link(&state.pool, &token_sha, user_id, expires_at)
        .await
        .map_err(internal_error)?;

    let link = format!(
        "{}/cloud/auth?token={}",
        state.cfg.public_base_url.trim_end_matches('/'),
        token
    );
    mailer
        .send_magic_link(&email, &link)
        .await
        .map_err(internal_error)?;

    Ok(Json(RequestLinkResponse { ok: true }))
}

#[derive(Deserialize)]
pub struct ExchangeQuery {
    pub token: String,
}

pub async fn exchange_magic_link(
    State(state): State<AppState>,
    Query(q): Query<ExchangeQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let token = q.token.trim();
    if token.len() < 20 {
        return Err((StatusCode::BAD_REQUEST, "Invalid token".into()));
    }
    let token_sha = sha256_hex(token);
    let user_id = consume_magic_link(&state.pool, &token_sha)
        .await
        .map_err(|e| match e {
            ConsumeError::NotFound => (StatusCode::UNAUTHORIZED, "Invalid or expired".into()),
            ConsumeError::Db(s) => (StatusCode::INTERNAL_SERVER_ERROR, s),
        })?;

    let jwt = mint_jwt(&state, user_id)?;
    let cookie = format!(
        "leanctx_session={jwt}; Path=/; HttpOnly; Secure; SameSite=None; Max-Age={}",
        60 * 60 * 24 * 7
    );

    let body = serde_json::json!({ "token": jwt });
    let mut res = axum::response::Response::new(
        axum::body::Body::from(serde_json::to_string(&body).unwrap()),
    );
    *res.status_mut() = StatusCode::OK;
    res.headers_mut()
        .insert(axum::http::header::SET_COOKIE, cookie.parse().unwrap());
    res.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        "application/json".parse().unwrap(),
    );
    Ok(res)
}

pub async fn logout() -> impl IntoResponse {
    let cookie = "leanctx_session=; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=0";
    let mut res = axum::response::Response::new(axum::body::Body::from("ok"));
    *res.status_mut() = StatusCode::OK;
    res.headers_mut()
        .insert(axum::http::header::SET_COOKIE, cookie.parse().unwrap());
    res
}

#[derive(Serialize, Deserialize)]
struct Claims {
    sub: String,
    exp: usize,
    iat: usize,
}

fn mint_jwt(state: &AppState, user_id: Uuid) -> Result<String, (StatusCode, String)> {
    let now = Utc::now().timestamp() as usize;
    let exp = (Utc::now() + Duration::days(7)).timestamp() as usize;
    let claims = Claims {
        sub: user_id.to_string(),
        iat: now,
        exp,
    };
    let key = EncodingKey::from_secret(&state.jwt_secret);
    jsonwebtoken::encode(&Header::default(), &claims, &key)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("JWT encode failed: {e}")))
}

pub async fn auth_user(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(Uuid, String), (StatusCode, String)> {
    if let Some(v) = headers.get(axum::http::header::AUTHORIZATION) {
        if let Ok(s) = v.to_str() {
            if let Some(key) = s.strip_prefix("Bearer ").map(|x| x.trim()) {
                if let Ok(td) = jsonwebtoken::decode::<Claims>(
                    key,
                    &jsonwebtoken::DecodingKey::from_secret(&state.jwt_secret),
                    &Validation::default(),
                ) {
                    let user_id: Uuid = td.claims.sub.parse()
                        .map_err(|_| (StatusCode::UNAUTHORIZED, "Invalid JWT".into()))?;
                    let email = lookup_user_email(&state.pool, user_id).await
                        .map_err(internal_error)?
                        .unwrap_or_default();
                    return Ok((user_id, email));
                }
                let sha = sha256_hex(key);
                if let Some((user_id, email)) = lookup_api_key(&state.pool, &sha).await.map_err(internal_error)? {
                    return Ok((user_id, email));
                }
                return Err((StatusCode::UNAUTHORIZED, "Invalid token".into()));
            }
        }
    }

    if let Some(cookie) = headers.get(axum::http::header::COOKIE).and_then(|v| v.to_str().ok()) {
        for part in cookie.split(';') {
            let p = part.trim();
            if let Some(v) = p.strip_prefix("leanctx_session=") {
                let claims = jsonwebtoken::decode::<Claims>(
                    v,
                    &jsonwebtoken::DecodingKey::from_secret(&state.jwt_secret),
                    &Validation::default(),
                )
                .map_err(|_| (StatusCode::UNAUTHORIZED, "Invalid session".into()))?
                .claims;
                let user_id: Uuid = claims
                    .sub
                    .parse()
                    .map_err(|_| (StatusCode::UNAUTHORIZED, "Invalid session".into()))?;
                let email = lookup_user_email(&state.pool, user_id)
                    .await
                    .map_err(internal_error)?
                    .ok_or((StatusCode::UNAUTHORIZED, "Unknown user".into()))?;
                return Ok((user_id, email));
            }
        }
    }

    Err((StatusCode::UNAUTHORIZED, "Unauthorized".into()))
}

async fn upsert_user(pool: &Pool, email: &str) -> anyhow::Result<Uuid> {
    let client = pool.get().await?;
    let row = client
        .query_opt("SELECT id FROM users WHERE email=$1", &[&email])
        .await?;
    if let Some(r) = row {
        return Ok(r.get(0));
    }
    let id = Uuid::new_v4();
    client
        .execute(
            "INSERT INTO users (id,email) VALUES ($1,$2) ON CONFLICT (email) DO NOTHING",
            &[&id, &email],
        )
        .await?;
    let row = client
        .query_one("SELECT id FROM users WHERE email=$1", &[&email])
        .await?;
    Ok(row.get(0))
}

async fn rotate_api_key(pool: &Pool, user_id: Uuid, api_key_sha: &str) -> anyhow::Result<()> {
    let client = pool.get().await?;
    client
        .execute("DELETE FROM api_keys WHERE user_id=$1", &[&user_id])
        .await?;
    let id = Uuid::new_v4();
    client
        .execute(
            "INSERT INTO api_keys (id,user_id,api_key_sha256) VALUES ($1,$2,$3)",
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

async fn lookup_user_email(pool: &Pool, user_id: Uuid) -> anyhow::Result<Option<String>> {
    let client = pool.get().await?;
    Ok(client
        .query_opt("SELECT email FROM users WHERE id=$1", &[&user_id])
        .await?
        .map(|r| r.get(0)))
}

async fn store_magic_link(
    pool: &Pool,
    token_sha: &str,
    user_id: Uuid,
    expires_at: DateTime<Utc>,
) -> anyhow::Result<()> {
    let client = pool.get().await?;
    client
        .execute(
            "INSERT INTO magic_links (token_sha256,user_id,expires_at) VALUES ($1,$2,$3)",
            &[&token_sha, &user_id, &expires_at],
        )
        .await?;
    Ok(())
}

enum ConsumeError {
    NotFound,
    Db(String),
}

async fn consume_magic_link(pool: &Pool, token_sha: &str) -> Result<Uuid, ConsumeError> {
    let client = pool.get().await.map_err(|e| ConsumeError::Db(e.to_string()))?;
    let row = client
        .query_opt(
            "SELECT user_id, expires_at, consumed_at FROM magic_links WHERE token_sha256=$1",
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
            "UPDATE magic_links SET consumed_at=NOW() WHERE token_sha256=$1",
            &[&token_sha],
        )
        .await
        .map_err(|e| ConsumeError::Db(e.to_string()))?;
    Ok(user_id)
}

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

fn internal_error<E: std::fmt::Display>(e: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

