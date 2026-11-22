// SPDX-License-Identifier: Apache-2.0
//! OAuth 2.1 for remote MCP clients (ux16): the whole flow over the real
//! router, and each refusal the MCP authorization specification requires.

use super::super::{HttpServerConfig, build_app_router_with_auth};
use super::*;
use axum::body::Body;
use axum::http::{HeaderMap, Request};
use tower::ServiceExt;

const OWNER: &str = "owner-token-for-the-oauth-tests";
const ISSUER: &str = "http://127.0.0.1:8080";
/// Registered without a port; the native client picks one per run (RFC 8252).
const REGISTERED: &str = "http://127.0.0.1/callback";
const REDIRECT: &str = "http://127.0.0.1:53682/callback";
/// RFC 7636 appendix B.
const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn app_in(dir: &std::path::Path, oauth: bool) -> Router {
    app_with_token(dir, oauth, OWNER)
}

fn app_with_token(dir: &std::path::Path, oauth: bool, token: &str) -> Router {
    let cfg = HttpServerConfig {
        project_root: dir.to_path_buf(),
        auth_token: Some(token.to_string()),
        oauth,
        oauth_state_dir: Some(dir.join("oauth-store")),
        ..HttpServerConfig::default()
    };
    build_app_router_with_auth(&cfg, true, None)
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Reply {
    fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or_else(|_| panic!("not JSON: {}", self.body))
    }
    fn location(&self) -> reqwest::Url {
        let raw = self.headers[header::LOCATION].to_str().expect("location");
        reqwest::Url::parse(raw).expect("absolute location")
    }
    fn header(&self, name: header::HeaderName) -> String {
        self.headers
            .get(name)
            .map(|v| v.to_str().unwrap_or_default().to_string())
            .unwrap_or_default()
    }
}

async fn send(app: &Router, req: Request<Body>) -> Reply {
    let resp = app.clone().oneshot(req).await.expect("response");
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .expect("body");
    Reply {
        status,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

fn get(uri: &str, bearer: Option<&str>) -> Request<Body> {
    let mut req = Request::builder()
        .method("GET")
        .uri(uri)
        .header("Host", "127.0.0.1:8080");
    if let Some(token) = bearer {
        req = req.header("Authorization", format!("Bearer {token}"));
    }
    req.body(Body::empty()).expect("request")
}

fn post_form(uri: &str, pairs: &[(&str, &str)], bearer: Option<&str>) -> Request<Body> {
    let body = pairs
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let mut req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("Host", "127.0.0.1:8080")
        .header("Content-Type", "application/x-www-form-urlencoded");
    if let Some(token) = bearer {
        req = req.header("Authorization", format!("Bearer {token}"));
    }
    req.body(Body::from(body)).expect("request")
}

fn post_json(uri: &str, body: &serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("Host", "127.0.0.1:8080")
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

async fn register_client(app: &Router) -> String {
    let reply = send(
        app,
        post_json(
            "/oauth/register",
            &serde_json::json!({
                "redirect_uris": [REGISTERED],
                "client_name": "Remote <client>",
            }),
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    reply.json()["client_id"]
        .as_str()
        .expect("client_id")
        .to_string()
}

fn authorize_uri(client_id: &str, method: &str, redirect: &str) -> String {
    format!(
        "/oauth/authorize?response_type=code&client_id={}&redirect_uri={}&code_challenge={CHALLENGE}\
&code_challenge_method={method}&state=st-42&resource={}",
        urlencoding::encode(client_id),
        urlencoding::encode(redirect),
        urlencoding::encode(&format!("{ISSUER}/mcp")),
    )
}

fn request_id(page: &str) -> String {
    let marker = "name=\"request_id\" value=\"";
    let start = page.find(marker).expect("request_id field") + marker.len();
    let end = page[start..].find('"').expect("end of value") + start;
    page[start..end].to_string()
}

fn query(url: &reqwest::Url, key: &str) -> Option<String> {
    url.query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

/// Register, show the consent page, approve it: the client id and code.
async fn authorized_code(app: &Router) -> (String, String) {
    let client_id = register_client(app).await;
    let page = send(app, get(&authorize_uri(&client_id, "S256", REDIRECT), None)).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(page.body.contains("Remote &lt;client&gt;"), "name escaped");
    assert!(
        page.header(header::CONTENT_SECURITY_POLICY)
            .contains("frame-ancestors 'none'")
    );
    assert_eq!(page.header(header::X_FRAME_OPTIONS), "DENY");
    let approved = send(
        app,
        post_form(
            "/oauth/authorize",
            &[
                ("request_id", &request_id(&page.body)),
                ("owner_token", OWNER),
                ("decision", "allow"),
            ],
            None,
        ),
    )
    .await;
    assert_eq!(approved.status, StatusCode::SEE_OTHER, "{}", approved.body);
    let back = approved.location();
    assert!(back.as_str().starts_with(REDIRECT), "{back}");
    assert_eq!(query(&back, "state").as_deref(), Some("st-42"));
    assert_eq!(query(&back, "iss").as_deref(), Some(ISSUER));
    (client_id, query(&back, "code").expect("code"))
}

async fn exchange(app: &Router, client_id: &str, code: &str, verifier: &str) -> Reply {
    send(
        app,
        post_form(
            "/oauth/token",
            &[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("client_id", client_id),
                ("redirect_uri", REDIRECT),
                ("code_verifier", verifier),
            ],
            None,
        ),
    )
    .await
}

async fn refresh_with(app: &Router, client_id: &str, refresh: &str) -> Reply {
    send(
        app,
        post_form(
            "/oauth/token",
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh),
                ("client_id", client_id),
            ],
            None,
        ),
    )
    .await
}

fn tokens(reply: &Reply) -> (String, String) {
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.header(header::CACHE_CONTROL), "no-store");
    let body = reply.json();
    assert_eq!(body["token_type"], "Bearer");
    (
        body["access_token"].as_str().expect("access").to_string(),
        body["refresh_token"].as_str().expect("refresh").to_string(),
    )
}

#[tokio::test]
async fn discovery_documents_are_public_and_name_the_issuer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = app_in(dir.path(), true);

    let unauth = send(&app, get("/v1/health", None)).await;
    assert_eq!(unauth.status, StatusCode::UNAUTHORIZED);
    let challenge = unauth.header(header::WWW_AUTHENTICATE);
    assert!(
        challenge.contains(&format!(
            "resource_metadata=\"{ISSUER}/.well-known/oauth-protected-resource\""
        )),
        "{challenge}"
    );
    assert!(!challenge.contains("error="), "{challenge}");

    let resource = send(&app, get("/.well-known/oauth-protected-resource/mcp", None)).await;
    assert_eq!(resource.status, StatusCode::OK);
    assert_eq!(resource.json()["resource"], format!("{ISSUER}/mcp"));
    assert_eq!(resource.json()["authorization_servers"][0], ISSUER);

    let server = send(&app, get("/.well-known/oauth-authorization-server", None)).await;
    assert_eq!(server.status, StatusCode::OK);
    let doc = server.json();
    assert_eq!(doc["issuer"], ISSUER);
    assert_eq!(
        doc["code_challenge_methods_supported"],
        serde_json::json!(["S256"])
    );
    assert_eq!(doc["token_endpoint"], format!("{ISSUER}/oauth/token"));
    assert_eq!(doc["authorization_response_iss_parameter_supported"], true);
}

#[tokio::test]
async fn without_oauth_nothing_is_served_and_no_challenge_points_anywhere() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = app_in(dir.path(), false);
    for uri in [
        "/.well-known/oauth-authorization-server",
        "/.well-known/oauth-protected-resource",
        "/oauth/authorize",
    ] {
        let reply = send(&app, get(uri, None)).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{uri}");
        assert!(
            reply.headers.get(header::WWW_AUTHENTICATE).is_none(),
            "{uri}"
        );
    }
}

#[tokio::test]
async fn full_flow_grants_mcp_access_but_never_owner_routes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = app_in(dir.path(), true);
    let (client_id, code) = authorized_code(&app).await;
    let (access, _refresh) = tokens(&exchange(&app, &client_id, &code, VERIFIER).await);
    assert!(access.starts_with("lcx_at_"));

    assert_eq!(
        send(&app, get("/v1/health", Some(&access))).await.status,
        StatusCode::OK
    );
    assert_eq!(
        send(&app, get("/v1/health", Some(OWNER))).await.status,
        StatusCode::OK
    );

    let bad = send(&app, get("/v1/health", Some("lcx_at_forged"))).await;
    assert_eq!(bad.status, StatusCode::UNAUTHORIZED);
    assert!(
        bad.header(header::WWW_AUTHENTICATE)
            .contains("error=\"invalid_token\"")
    );

    // Allowlist: outside the MCP endpoint and the read-only discovery
    // routes, an OAuth token is refused, whatever the route does.
    let delete_dlq = Request::builder()
        .method("DELETE")
        .uri("/ocla/v1/dlq/some-id")
        .header("Host", "127.0.0.1:8080")
        .header("Authorization", format!("Bearer {access}"))
        .body(Body::empty())
        .expect("request");
    for req in [
        post_form("/v1/shutdown", &[], Some(&access)),
        post_form("/v1/kernel/reset", &[], Some(&access)),
        post_form("/v1/kernel/config", &[], Some(&access)),
        get("/v1/kernel/config", Some(&access)),
        get("/v1/oauth/clients", Some(&access)),
        post_form("/v1/agents/register", &[], Some(&access)),
        post_form("/v1/tools/call", &[], Some(&access)),
        post_form("/v1/index/ensure", &[], Some(&access)),
        post_form("/ocla/v1/budget", &[], Some(&access)),
        post_form("/ocla/v1/dlq/some-id/retry", &[], Some(&access)),
        delete_dlq,
        get("/v1/audit/events", Some(&access)),
        get("/v1/events", Some(&access)),
        get("/some/future/admin/route", Some(&access)),
    ] {
        let uri = format!("{} {}", req.method(), req.uri());
        let reply = send(&app, req).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{uri}: {}", reply.body);
        assert_eq!(reply.json()["error_code"], "insufficient_scope", "{uri}");
    }
    // The MCP endpoint itself is open to the token (whatever the MCP
    // transport then answers to an empty body, it is not an auth refusal).
    for uri in ["/mcp", "/"] {
        let reply = send(&app, post_json(uri, &serde_json::json!({}))).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "no token on {uri}");
        let mut req = post_json(uri, &serde_json::json!({}));
        req.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {access}")).expect("header"),
        );
        let reply = send(&app, req).await;
        assert!(
            reply.status != StatusCode::UNAUTHORIZED && reply.status != StatusCode::FORBIDDEN,
            "{uri}: {} {}",
            reply.status,
            reply.body
        );
    }

    let listed = send(&app, get("/v1/oauth/clients", Some(OWNER))).await;
    assert_eq!(listed.status, StatusCode::OK);
    assert_eq!(listed.json()["clients"][0]["client_id"], client_id.as_str());
    assert_eq!(listed.json()["clients"][0]["active_grants"], 1);

    let revoked = send(
        &app,
        post_form(
            &format!("/v1/oauth/clients/{client_id}/revoke"),
            &[],
            Some(OWNER),
        ),
    )
    .await;
    assert_eq!(revoked.status, StatusCode::OK, "{}", revoked.body);
    assert_eq!(
        send(&app, get("/v1/health", Some(&access))).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn a_replayed_code_fails_and_revokes_what_it_issued() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = app_in(dir.path(), true);
    let (client_id, code) = authorized_code(&app).await;
    let (access, _) = tokens(&exchange(&app, &client_id, &code, VERIFIER).await);

    let replay = exchange(&app, &client_id, &code, VERIFIER).await;
    assert_eq!(replay.status, StatusCode::BAD_REQUEST);
    assert_eq!(replay.json()["error"], "invalid_grant");
    assert_eq!(
        send(&app, get("/v1/health", Some(&access))).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn refresh_tokens_rotate_and_reuse_revokes_the_family() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = app_in(dir.path(), true);
    let (client_id, code) = authorized_code(&app).await;
    let (_, refresh1) = tokens(&exchange(&app, &client_id, &code, VERIFIER).await);

    let (access2, refresh2) = tokens(&refresh_with(&app, &client_id, &refresh1).await);
    assert_ne!(refresh1, refresh2);
    assert_eq!(
        send(&app, get("/v1/health", Some(&access2))).await.status,
        StatusCode::OK
    );

    let reuse = refresh_with(&app, &client_id, &refresh1).await;
    assert_eq!(reuse.status, StatusCode::BAD_REQUEST);
    assert_eq!(reuse.json()["error"], "invalid_grant");
    assert_eq!(
        refresh_with(&app, &client_id, &refresh2).await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        send(&app, get("/v1/health", Some(&access2))).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn grants_survive_a_restart_from_the_hashed_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (client_id, access, refresh) = {
        let app = app_in(dir.path(), true);
        let (client_id, code) = authorized_code(&app).await;
        let (access, refresh) = tokens(&exchange(&app, &client_id, &code, VERIFIER).await);
        (client_id, access, refresh)
    };
    let store = std::fs::read_dir(dir.path().join("oauth-store"))
        .expect("store dir")
        .map(|e| std::fs::read_to_string(e.expect("entry").path()).expect("store"))
        .collect::<String>();
    assert!(
        !store.contains(&access) && !store.contains(&refresh),
        "only hashes"
    );

    let restarted = app_in(dir.path(), true);
    assert_eq!(
        send(&restarted, get("/v1/health", Some(&access)))
            .await
            .status,
        StatusCode::OK
    );
    tokens(&refresh_with(&restarted, &client_id, &refresh).await);
}

#[tokio::test]
async fn pkce_redirects_and_owner_check_are_enforced() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = app_in(dir.path(), true);

    // A wrong verifier burns the code.
    let (client_id, code) = authorized_code(&app).await;
    let wrong = exchange(&app, &client_id, &code, &"x".repeat(43)).await;
    assert_eq!(wrong.json()["error"], "invalid_grant");
    assert_eq!(
        exchange(&app, &client_id, &code, VERIFIER).await.status,
        StatusCode::BAD_REQUEST
    );

    // `plain` PKCE goes back to the client as invalid_request.
    let plain = send(
        &app,
        get(&authorize_uri(&client_id, "plain", REDIRECT), None),
    )
    .await;
    assert_eq!(plain.status, StatusCode::SEE_OTHER);
    assert_eq!(
        query(&plain.location(), "error").as_deref(),
        Some("invalid_request")
    );

    // An unregistered redirect is never followed.
    let foreign = send(
        &app,
        get(
            &authorize_uri(&client_id, "S256", "https://evil.example/cb"),
            None,
        ),
    )
    .await;
    assert_eq!(foreign.status, StatusCode::BAD_REQUEST);
    assert!(foreign.headers.get(header::LOCATION).is_none());

    for uri in [
        "javascript:alert(1)",
        "http://evil.example/cb",
        "https://a.example/#x",
    ] {
        let reply = send(
            &app,
            post_json(
                "/oauth/register",
                &serde_json::json!({ "redirect_uris": [uri] }),
            ),
        )
        .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(reply.json()["error"], "invalid_redirect_uri", "{uri}");
    }

    // A wrong instance token shows the page again; deny returns to the client.
    let page = send(
        &app,
        get(&authorize_uri(&client_id, "S256", REDIRECT), None),
    )
    .await;
    let id = request_id(&page.body);
    let wrong_owner = send(
        &app,
        post_form(
            "/oauth/authorize",
            &[
                ("request_id", &id),
                ("owner_token", "guess"),
                ("decision", "allow"),
            ],
            None,
        ),
    )
    .await;
    assert_eq!(wrong_owner.status, StatusCode::UNAUTHORIZED);
    assert_eq!(request_id(&wrong_owner.body), id);
    let denied = send(
        &app,
        post_form(
            "/oauth/authorize",
            &[("request_id", &id), ("decision", "deny")],
            None,
        ),
    )
    .await;
    assert_eq!(denied.status, StatusCode::SEE_OTHER);
    assert_eq!(
        query(&denied.location(), "error").as_deref(),
        Some("access_denied")
    );
    // The request was single use.
    let again = send(
        &app,
        post_form(
            "/oauth/authorize",
            &[
                ("request_id", &id),
                ("owner_token", OWNER),
                ("decision", "allow"),
            ],
            None,
        ),
    )
    .await;
    assert_eq!(again.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn errors_for_a_client_nobody_approved_are_never_redirected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = app_in(dir.path(), true);
    // Anyone may register a redirect; until the owner approves the client
    // once, /oauth/authorize must not send the browser there.
    let client_id = register_client(&app).await;
    let plain = send(
        &app,
        get(&authorize_uri(&client_id, "plain", REDIRECT), None),
    )
    .await;
    assert_eq!(plain.status, StatusCode::BAD_REQUEST, "{}", plain.body);
    assert!(plain.headers.get(header::LOCATION).is_none());
    assert!(plain.body.contains("invalid_request"), "{}", plain.body);
    let no_type = send(
        &app,
        get(
            &authorize_uri(&client_id, "S256", REDIRECT)
                .replace("response_type=code", "response_type=token"),
            None,
        ),
    )
    .await;
    assert_eq!(no_type.status, StatusCode::BAD_REQUEST);
    assert!(no_type.headers.get(header::LOCATION).is_none());

    // A deny on an unapproved client stays on our page too.
    let page = send(
        &app,
        get(&authorize_uri(&client_id, "S256", REDIRECT), None),
    )
    .await;
    let denied = send(
        &app,
        post_form(
            "/oauth/authorize",
            &[
                ("request_id", &request_id(&page.body)),
                ("decision", "deny"),
            ],
            None,
        ),
    )
    .await;
    assert_eq!(denied.status, StatusCode::BAD_REQUEST);
    assert!(denied.headers.get(header::LOCATION).is_none());
}

#[tokio::test]
async fn ten_wrong_instance_tokens_lock_the_consent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = app_in(dir.path(), true);
    // Spread over clients, so no single client reaches its own lockout:
    // the instance-wide limit holds across all of them.
    let per_client = CLIENT_FAILURE_LIMIT as usize - 1;
    let mut id = String::new();
    for attempt in 0..OWNER_FAILURE_LIMIT {
        if attempt % per_client == 0 {
            let client_id = register_client(&app).await;
            let page = send(
                &app,
                get(&authorize_uri(&client_id, "S256", REDIRECT), None),
            )
            .await;
            id = request_id(&page.body);
        }
        let wrong = send(
            &app,
            post_form(
                "/oauth/authorize",
                &[
                    ("request_id", &id),
                    ("owner_token", "guess"),
                    ("decision", "allow"),
                ],
                None,
            ),
        )
        .await;
        assert_eq!(wrong.status, StatusCode::UNAUTHORIZED, "attempt {attempt}");
        assert_eq!(request_id(&wrong.body), id);
    }
    // Even the right token is refused now, and no code leaves.
    let locked = send(
        &app,
        post_form(
            "/oauth/authorize",
            &[
                ("request_id", &id),
                ("owner_token", OWNER),
                ("decision", "allow"),
            ],
            None,
        ),
    )
    .await;
    assert_eq!(
        locked.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        locked.body
    );
    assert!(locked.headers.get(header::LOCATION).is_none());
}

#[tokio::test]
async fn a_changed_explicit_token_drops_every_grant() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (client_id, access, refresh) = {
        let app = app_in(dir.path(), true);
        let (client_id, code) = authorized_code(&app).await;
        let (access, refresh) = tokens(&exchange(&app, &client_id, &code, VERIFIER).await);
        (client_id, access, refresh)
    };
    let rotated_token = concat!("rotated", "-instance-", "token");
    let rotated = app_with_token(dir.path(), true, rotated_token);
    assert_eq!(
        send(&rotated, get("/v1/health", Some(&access)))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        refresh_with(&rotated, &client_id, &refresh).await.status,
        StatusCode::BAD_REQUEST
    );
    let listed = send(&rotated, get("/v1/oauth/clients", Some(rotated_token))).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(listed.json()["clients"], serde_json::json!([]));
    // Going back to the old token does not bring the grants back.
    let back = app_in(dir.path(), true);
    assert_eq!(
        send(&back, get("/v1/health", Some(&access))).await.status,
        StatusCode::UNAUTHORIZED
    );
    let store = std::fs::read_dir(dir.path().join("oauth-store"))
        .expect("store dir")
        .map(|e| std::fs::read_to_string(e.expect("entry").path()).expect("store"))
        .collect::<String>();
    assert!(
        !store.contains(OWNER) && !store.contains(rotated_token),
        "fingerprint only"
    );
}

#[test]
fn the_generated_loopback_token_keeps_grants_across_restarts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = OAuthState::store_path(dir.path(), ISSUER);
    let now = unix_now();
    let first = OAuthState::open(
        ISSUER.into(),
        "generated-one".into(),
        Some(path.clone()),
        false,
    );
    let (client_id, ..) = first
        .register(&[REGISTERED.to_string()], Some("cli"), now)
        .expect("register");
    drop(first);
    // A new start generates a new token and is not fingerprinted.
    let second = OAuthState::open(
        ISSUER.into(),
        "generated-two".into(),
        Some(path.clone()),
        false,
    );
    assert_eq!(second.list_clients(now).len(), 1);
    assert_eq!(second.list_clients(now)[0]["client_id"], client_id.as_str());
    let fp_a = second.owner_fingerprint("explicit-a");
    assert_ne!(fp_a, second.owner_fingerprint("explicit-b"));
    drop(second);
    // The first explicit token adopts the store; a different one clears it.
    let adopted = OAuthState::open(ISSUER.into(), "explicit-a".into(), Some(path.clone()), true);
    assert!(
        adopted.list_clients(now).is_empty(),
        "no fingerprint yet: grants of unknown origin go"
    );
    adopted
        .register(&[REGISTERED.to_string()], Some("cli"), now)
        .expect("register");
    drop(adopted);
    let same = OAuthState::open(ISSUER.into(), "explicit-a".into(), Some(path.clone()), true);
    assert_eq!(same.list_clients(now).len(), 1);
    drop(same);
    let other = OAuthState::open(ISSUER.into(), "explicit-b".into(), Some(path), true);
    assert!(other.list_clients(now).is_empty());
}

#[test]
fn a_public_bind_without_an_issuer_is_refused() {
    let public = HttpServerConfig {
        host: "0.0.0.0".to_string(),
        auth_token: Some(OWNER.to_string()),
        oauth: true,
        ..HttpServerConfig::default()
    };
    let error = public
        .oauth_issuer_url()
        .expect_err("needs an issuer")
        .to_string();
    assert!(error.contains("--oauth-issuer"), "{error}");
    assert!(public.validate().is_err());
    let with_issuer = HttpServerConfig {
        oauth_issuer: Some("https://ctx.example.com/".to_string()),
        ..public
    };
    assert_eq!(
        with_issuer.oauth_issuer_url().expect("issuer").as_deref(),
        Some("https://ctx.example.com")
    );
    let loopback = HttpServerConfig {
        auth_token: Some(OWNER.to_string()),
        oauth: true,
        ..HttpServerConfig::default()
    };
    assert!(loopback.oauth_issuer_url().expect("loopback").is_some());
}

#[test]
fn oauth_allowlist_covers_only_mcp_and_read_only_discovery() {
    assert!(oauth_may_call(&Method::POST, "/mcp"));
    assert!(oauth_may_call(&Method::GET, "/"));
    assert!(oauth_may_call(&Method::GET, "/v1/capabilities"));
    assert!(!oauth_may_call(&Method::POST, "/v1/capabilities"));
    for path in [
        "/v1/shutdown",
        "/v1/tools/call",
        "/v1/agents/register",
        "/ocla/v1/budget",
        "/ocla/v1/dlq/x/retry",
        "/v1/oauth/clients",
        "/mcp/",
        "/MCP",
        "/v1/health/",
    ] {
        assert!(!oauth_may_call(&Method::POST, path), "{path}");
    }
}

#[test]
fn issuer_and_redirect_rules() {
    assert_eq!(
        canonical_issuer("https://ctx.example.com/").expect("public issuer"),
        "https://ctx.example.com"
    );
    assert_eq!(
        canonical_issuer("http://127.0.0.1:9000").expect("loopback issuer"),
        "http://127.0.0.1:9000"
    );
    for bad in [
        "http://ctx.example.com",
        "https://ctx.example.com/mcp",
        "https://user@ctx.example.com",
        "ctx.example.com",
    ] {
        assert!(canonical_issuer(bad).is_err(), "{bad}");
    }
    assert!(validate_redirect_uri("com.example.app:/cb").is_ok());
    assert!(validate_redirect_uri("https://app.example/cb").is_ok());
    assert!(validate_redirect_uri("data:text/html,x").is_err());
    assert!(redirect_matches(REGISTERED, REDIRECT));
    assert!(!redirect_matches(REGISTERED, "http://127.0.0.1:1/other"));
    assert!(!redirect_matches(
        "https://app.example/cb",
        "https://app.example:444/cb"
    ));
    assert!(pkce_s256_matches(VERIFIER, CHALLENGE));
    assert!(!pkce_s256_matches("short", CHALLENGE));
}

// --- ux16 security follow-ups ----------------------------------------------

fn store_text(dir: &std::path::Path) -> String {
    std::fs::read_dir(dir.join("oauth-store"))
        .expect("store dir")
        .map(|e| std::fs::read_to_string(e.expect("entry").path()).expect("store"))
        .collect::<String>()
}

#[test]
fn hmac_matches_rfc_4231() {
    // Test cases 1 and 2.
    assert_eq!(
        hex::encode(hmac_sha256(&[0x0b; 20], &[b"Hi There"])),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    );
    assert_eq!(
        hex::encode(hmac_sha256(
            b"Jefe",
            &[b"what do ya ", b"want for nothing?"]
        )),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
    // Test case 6: a key longer than the block is hashed first.
    assert_eq!(
        hex::encode(hmac_sha256(
            &[0xaa; 131],
            &[b"Test Using Larger Than Block-Size Key - Hash Key First"]
        )),
        "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
    );
}

/// Two processes starting at once share one secret: creation is exclusive,
/// so the second one reads the first one's file instead of replacing it.
#[test]
fn concurrent_starts_share_one_install_secret() {
    let root = tempfile::tempdir().expect("tempdir");
    // The private store directory exists, as after any earlier start.
    let store = root.path().join("oauth-store");
    private_io::write(&store, "warm-up", b"x").expect("private dir");
    let dir = store.as_path();
    let threads = 8;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(threads));
    let secrets: Vec<[u8; 32]> = (0..threads)
        .map(|_| {
            let dir = dir.to_path_buf();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                load_or_create_secret(&dir).0
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|handle| handle.join().expect("thread"))
        .collect();
    let on_disk = std::fs::read_to_string(dir.join(SECRET_FILE)).expect("secret");
    for secret in &secrets {
        assert_eq!(hex::encode(secret), on_disk.trim());
    }
    // Exclusive creation leaves an existing file as it is.
    let error = private_io::create_new(dir, SECRET_FILE, b"other").expect_err("exists");
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(
        std::fs::read_to_string(dir.join(SECRET_FILE)).expect("secret"),
        on_disk
    );
    assert_eq!(load_or_create_secret(dir), (secrets[0], false));
}

/// A secret others could read may have leaked: it is replaced (ending every
/// grant), not narrowed to 0600 and trusted.
#[cfg(unix)]
#[test]
fn a_secret_readable_by_others_is_replaced() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().expect("tempdir");
    let leaked = [7u8; 32];
    let path = dir.path().join(SECRET_FILE);
    std::fs::write(&path, hex::encode(leaked)).expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    let (secret, fresh) = load_or_create_secret(dir.path());
    assert!(fresh, "a replaced secret ends every grant");
    assert_ne!(secret, leaked);
    assert_eq!(
        std::fs::read_to_string(&path).expect("secret").trim(),
        hex::encode(secret)
    );
    let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "{mode:o}");
    // The replacement is owner-only, so it is kept from now on.
    assert_eq!(load_or_create_secret(dir.path()), (secret, false));
}

#[tokio::test]
async fn the_store_holds_no_plain_token_hash_and_the_secret_is_owner_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = app_in(dir.path(), true);
    let (client_id, code) = authorized_code(&app).await;
    let (access, refresh) = tokens(&exchange(&app, &client_id, &code, VERIFIER).await);
    let store = store_text(dir.path());
    for token in [&access, &refresh] {
        let plain = hex::encode(Sha256::digest(token.as_bytes()));
        assert!(
            !store.contains(&plain),
            "plain SHA-256 of a token is stored"
        );
    }
    let secret_path = dir.path().join("oauth-store").join(SECRET_FILE);
    let secret = std::fs::read_to_string(&secret_path).expect("install secret");
    assert_eq!(hex::decode(secret.trim()).expect("hex").len(), 32);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        for name in [
            SECRET_FILE.to_string(),
            OAuthState::store_path(std::path::Path::new(""), ISSUER)
                .to_string_lossy()
                .into_owned(),
        ] {
            let mode = std::fs::metadata(dir.path().join("oauth-store").join(&name))
                .expect("owner-only file")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "{name}");
        }
    }
    // The grant survives a restart under the same secret.
    let again = app_in(dir.path(), true);
    assert_eq!(
        send(&again, get("/v1/health", Some(&access))).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_lost_install_secret_ends_every_grant() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (client_id, access, refresh) = {
        let app = app_in(dir.path(), true);
        let (client_id, code) = authorized_code(&app).await;
        let (access, refresh) = tokens(&exchange(&app, &client_id, &code, VERIFIER).await);
        (client_id, access, refresh)
    };
    std::fs::remove_file(dir.path().join("oauth-store").join(SECRET_FILE)).expect("secret");
    let app = app_in(dir.path(), true);
    assert_eq!(
        send(&app, get("/v1/health", Some(&access))).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        refresh_with(&app, &client_id, &refresh).await.status,
        StatusCode::BAD_REQUEST
    );
}

#[test]
fn installs_with_different_secrets_hash_differently() {
    let a = tempfile::tempdir().expect("tempdir");
    let b = tempfile::tempdir().expect("tempdir");
    let open = |dir: &std::path::Path| {
        OAuthState::open(
            ISSUER.into(),
            OWNER.into(),
            Some(OAuthState::store_path(dir, ISSUER)),
            true,
        )
    };
    let (first, second) = (open(a.path()), open(b.path()));
    assert_ne!(
        first.hash_token("lcx_at_same"),
        second.hash_token("lcx_at_same")
    );
    assert_ne!(
        first.owner_fingerprint(OWNER),
        second.owner_fingerprint(OWNER)
    );
    // The same install keeps its key across starts.
    let reopened = open(a.path());
    assert_eq!(
        first.hash_token("lcx_at_same"),
        reopened.hash_token("lcx_at_same")
    );
}

#[test]
fn a_version_1_store_keeps_its_clients_but_not_its_tokens() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = OAuthState::store_path(dir.path(), ISSUER);
    let legacy = serde_json::json!({
        "version": 1,
        "owner_fingerprint": legacy_owner_fingerprint(OWNER),
        "clients": [{
            "client_id": "lcx_client_old",
            "client_name": "old",
            "redirect_uris": [REGISTERED],
            "created_at": 1,
            "approved": true,
        }],
        "tokens": [{
            "hash": hex::encode(Sha256::digest(b"lcx_at_old")),
            "kind": "access",
            "family": "fam_old",
            "client_id": "lcx_client_old",
            "expires_at": u64::MAX,
        }],
    });
    private_io::write(
        dir.path(),
        &path.file_name().expect("name").to_string_lossy(),
        legacy.to_string().as_bytes(),
    )
    .expect("legacy store");
    let now = unix_now();
    let state = OAuthState::open(ISSUER.into(), OWNER.into(), Some(path.clone()), true);
    assert_eq!(state.list_clients(now).len(), 1);
    assert_eq!(state.verify_access("lcx_at_old", now), None);
    let stored: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("store")).expect("json");
    assert_eq!(stored["version"], STORE_VERSION);
    assert_eq!(stored["tokens"], serde_json::json!([]));
}

/// A client that keeps failing at the token endpoint gets 429 for further
/// junk, but its valid credentials still work: the lockout cannot be used to
/// keep a legitimate client out. Other clients are not affected.
#[tokio::test]
async fn repeated_failed_token_requests_lock_out_that_client() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = app_in(dir.path(), true);
    let (client_a, code_a) = authorized_code(&app).await;
    let (_, refresh_a) = tokens(&exchange(&app, &client_a, &code_a, VERIFIER).await);
    for attempt in 0..CLIENT_FAILURE_LIMIT {
        let wrong = refresh_with(&app, &client_a, "lcx_rt_guess").await;
        assert_eq!(wrong.status, StatusCode::BAD_REQUEST, "attempt {attempt}");
    }
    let locked = refresh_with(&app, &client_a, "lcx_rt_guess").await;
    assert_eq!(
        locked.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        locked.body
    );
    assert_eq!(locked.json()["error"], "temporarily_unavailable");
    let wait: u64 = locked
        .header(header::RETRY_AFTER)
        .parse()
        .expect("Retry-After seconds");
    assert!((1..=CLIENT_BACKOFF_BASE_SECS).contains(&wait), "{wait}");
    // The valid refresh token still rotates while the client is locked.
    tokens(&refresh_with(&app, &client_a, &refresh_a).await);
    let (client_b, code_b) = authorized_code(&app).await;
    tokens(&exchange(&app, &client_b, &code_b, VERIFIER).await);
}

/// A locked client still redeems a valid code with its PKCE verifier, while
/// a wrong verifier is refused with 429, burns its code and is not counted.
#[test]
fn a_locked_client_still_redeems_a_valid_code() {
    let state = OAuthState::open(ISSUER.into(), OWNER.into(), None, true);
    let now = unix_now();
    let (client_id, ..) = state
        .register(&[REGISTERED.to_string()], Some("cli"), now)
        .expect("register");
    for _ in 0..CLIENT_FAILURE_LIMIT {
        state
            .refresh("lcx_rt_guess", &client_id, now)
            .expect_err("unknown refresh token");
    }
    let issue = |code: &str| {
        lock(&state.inner).codes.insert(
            state.hash_token(code),
            CodeRecord {
                client_id: client_id.clone(),
                redirect_uri: REDIRECT.into(),
                redirect_uri_given: true,
                code_challenge: CHALLENGE.into(),
                expires_at: now + CODE_TTL_SECS,
                redeemed_family: None,
            },
        );
    };
    issue("lcx_code_burned");
    let wrong = state
        .exchange_code(
            "lcx_code_burned",
            &client_id,
            Some(REDIRECT),
            "wrong-verifier",
            now,
        )
        .expect_err("wrong verifier");
    assert_eq!(wrong.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(wrong.retry_after, Some(CLIENT_BACKOFF_BASE_SECS));
    assert_eq!(
        lock(&state.inner).client_failures[&client_id].count,
        CLIENT_FAILURE_LIMIT
    );
    assert!(
        state
            .exchange_code("lcx_code_burned", &client_id, Some(REDIRECT), VERIFIER, now)
            .is_err(),
        "a failed exchange burns the code"
    );
    issue("lcx_code_valid");
    state
        .exchange_code("lcx_code_valid", &client_id, Some(REDIRECT), VERIFIER, now)
        .expect("a valid code with its verifier");
    assert!(!lock(&state.inner).client_failures.contains_key(&client_id));
}

#[test]
fn the_client_backoff_grows_expires_and_resets() {
    let state = OAuthState::open(ISSUER.into(), OWNER.into(), None, true);
    let start = unix_now();
    let (client_id, ..) = state
        .register(&[REGISTERED.to_string()], Some("cli"), start)
        .expect("register");
    for _ in 0..CLIENT_FAILURE_LIMIT {
        let error = state
            .refresh("lcx_rt_guess", &client_id, start)
            .expect_err("unknown refresh token");
        assert_eq!(error.code, "invalid_grant");
    }
    let locked = state
        .refresh("lcx_rt_guess", &client_id, start + 1)
        .expect_err("locked");
    assert_eq!(locked.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(locked.retry_after, Some(CLIENT_BACKOFF_BASE_SECS - 1));
    // After the first lockout, the next failure locks twice as long.
    let later = start + CLIENT_BACKOFF_BASE_SECS;
    assert_eq!(
        state
            .refresh("lcx_rt_guess", &client_id, later)
            .expect_err("bad token")
            .code,
        "invalid_grant"
    );
    assert_eq!(
        state
            .refresh("lcx_rt_guess", &client_id, later)
            .expect_err("locked")
            .retry_after,
        Some(2 * CLIENT_BACKOFF_BASE_SECS)
    );
    // The backoff is capped, and forgotten after a quiet window.
    let mut now = later;
    let mut longest = 0;
    for _ in 0..40 {
        let error = state
            .refresh("lcx_rt_guess", &client_id, now)
            .expect_err("bad token");
        if let Some(wait) = error.retry_after {
            longest = longest.max(wait);
            if wait == CLIENT_BACKOFF_MAX_SECS {
                break;
            }
            now += wait;
        }
    }
    assert_eq!(longest, CLIENT_BACKOFF_MAX_SECS);
    let quiet = now + CLIENT_BACKOFF_MAX_SECS + CLIENT_FAILURE_WINDOW_SECS;
    assert_eq!(
        state
            .refresh("lcx_rt_guess", &client_id, quiet)
            .expect_err("bad token")
            .code,
        "invalid_grant"
    );
    assert_eq!(lock(&state.inner).client_failures[&client_id].count, 1);
}

#[test]
fn the_failure_table_stays_bounded() {
    let state = OAuthState::open(ISSUER.into(), OWNER.into(), None, true);
    let now = unix_now();
    // Unknown client ids are never tracked.
    for i in 0..(MAX_FAILURE_ENTRIES * 4) {
        let _ = state.refresh("lcx_rt_guess", &format!("lcx_client_{i}"), now);
    }
    assert!(lock(&state.inner).client_failures.is_empty());
    // Registered clients are tracked up to the bound, oldest out first.
    let mut inner = lock(&state.inner);
    for i in 0..(MAX_FAILURE_ENTRIES + 10) {
        let client_id = format!("lcx_client_{i}");
        inner.persisted.clients.push(Client {
            client_id: client_id.clone(),
            client_name: "c".into(),
            redirect_uris: vec![REGISTERED.into()],
            created_at: now,
            approved: true,
        });
        OAuthState::record_client_failure(&mut inner, &client_id, now + i as u64);
    }
    assert_eq!(inner.client_failures.len(), MAX_FAILURE_ENTRIES);
    assert!(!inner.client_failures.contains_key("lcx_client_0"));
}

#[tokio::test]
async fn wrong_instance_tokens_lock_out_that_client_at_the_consent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = app_in(dir.path(), true);
    let client_a = register_client(&app).await;
    let page = send(&app, get(&authorize_uri(&client_a, "S256", REDIRECT), None)).await;
    let id = request_id(&page.body);
    for attempt in 0..CLIENT_FAILURE_LIMIT {
        let wrong = send(
            &app,
            post_form(
                "/oauth/authorize",
                &[
                    ("request_id", &id),
                    ("owner_token", "guess"),
                    ("decision", "allow"),
                ],
                None,
            ),
        )
        .await;
        assert_eq!(wrong.status, StatusCode::UNAUTHORIZED, "attempt {attempt}");
    }
    // A further wrong token is refused outright and no code leaves.
    let locked = send(
        &app,
        post_form(
            "/oauth/authorize",
            &[
                ("request_id", &id),
                ("owner_token", "guess"),
                ("decision", "allow"),
            ],
            None,
        ),
    )
    .await;
    assert_eq!(
        locked.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        locked.body
    );
    assert!(locked.headers.get(header::LOCATION).is_none());
    // The right token still approves the locked client: wrong guesses cannot
    // lock the owner out.
    let page = send(&app, get(&authorize_uri(&client_a, "S256", REDIRECT), None)).await;
    let id = request_id(&page.body);
    let approved = send(
        &app,
        post_form(
            "/oauth/authorize",
            &[
                ("request_id", &id),
                ("owner_token", OWNER),
                ("decision", "allow"),
            ],
            None,
        ),
    )
    .await;
    assert!(
        approved
            .location()
            .query_pairs()
            .any(|(key, _)| key == "code"),
        "{}",
        approved.body
    );
    // Another client is still approved with the right token.
    authorized_code(&app).await;
}

#[test]
fn serve_warns_when_oauth_is_exposed_without_tls_or_an_explicit_token() {
    let public = HttpServerConfig {
        host: "0.0.0.0".to_string(),
        auth_token: Some(OWNER.to_string()),
        oauth: true,
        oauth_issuer: Some("https://ctx.example.com".to_string()),
        ..HttpServerConfig::default()
    };
    let warnings = public.oauth_exposure_warnings();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("TLS"), "{}", warnings[0]);

    // Behind a proxy on loopback, a public issuer needs an explicit token.
    let proxied = HttpServerConfig {
        auth_token: None,
        oauth_issuer: Some("https://ctx.example.com".to_string()),
        ..HttpServerConfig::default()
    };
    let warnings = proxied.oauth_exposure_warnings();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("--auth-token"), "{}", warnings[0]);

    let local = HttpServerConfig {
        oauth: true,
        ..HttpServerConfig::default()
    };
    assert!(local.oauth_exposure_warnings().is_empty());
    let explicit = HttpServerConfig {
        auth_token: Some(OWNER.to_string()),
        ..proxied
    };
    assert!(explicit.oauth_exposure_warnings().is_empty());
    assert!(
        HttpServerConfig::default()
            .oauth_exposure_warnings()
            .is_empty()
    );

    // `--oauth` on a public bind without a token is refused, and says why.
    let no_token = HttpServerConfig {
        auth_token: None,
        ..public
    };
    let error = no_token.validate().expect_err("no token").to_string();
    assert!(error.contains("--oauth does not replace it"), "{error}");
}
