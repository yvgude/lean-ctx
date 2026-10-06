//! Standalone OCLA wire-API sidecar deployment.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Configuration for the standalone OCLA wire-API process.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SidecarConfig {
    /// HTTP bind address for the sidecar.
    pub bind_addr: String,
    /// Optional bearer token required on every wire-API request.
    pub auth_token: Option<String>,
    /// Tenant scope authorized by this sidecar credential for DLQ administration.
    pub tenant_id: Option<String>,
    /// Project scope authorized by this sidecar credential for DLQ administration.
    pub project_id: Option<String>,
    /// PEM certificate chain for HTTPS.
    pub tls_cert_path: Option<PathBuf>,
    /// PEM private key for HTTPS.
    pub tls_key_path: Option<PathBuf>,
    /// Whether startup should bind a listener.
    pub enabled: bool,
}

impl Default for SidecarConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:3334".to_string(),
            auth_token: None,
            tenant_id: None,
            project_id: None,
            tls_cert_path: None,
            tls_key_path: None,
            enabled: false,
        }
    }
}

#[cfg(feature = "http-server")]
mod http {
    use std::sync::Arc;

    use anyhow::{Result, anyhow};
    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode, header},
        middleware::{self, Next},
        response::{IntoResponse, Response},
    };
    use subtle::ConstantTimeEq;
    use tokio::net::TcpListener;

    use super::SidecarConfig;

    pub(super) async fn start(config: &SidecarConfig) -> Result<()> {
        if !config.enabled {
            return Ok(());
        }

        if config.auth_token.as_ref().is_none_or(String::is_empty) {
            return Err(anyhow!(
                "OCLA sidecar requires auth_token when enabled — \\
                 set [ocla.sidecar] auth_token in config.toml"
            ));
        }

        if config.tls_cert_path.is_some() != config.tls_key_path.is_some() {
            return Err(anyhow!(
                "tls_cert_path and tls_key_path must be configured together"
            ));
        }
        if config.tls_cert_path.is_some() {
            return Err(anyhow!(
                "TLS sidecar transport requires a TLS listener dependency; \
                 configure the plain HTTP sidecar or add that runtime dependency"
            ));
        }

        let dlq_scope = match (config.tenant_id.as_deref(), config.project_id.as_deref()) {
            (Some(tenant_id), Some(project_id)) => Some(
                crate::core::a2a::dlq::DlqScope::new(tenant_id, project_id)
                    .map_err(|error| anyhow!(error.to_string()))?,
            ),
            (None, None) => None,
            _ => {
                return Err(anyhow!(
                    "sidecar tenant_id and project_id must be configured together"
                ));
            }
        };

        let listener = TcpListener::bind(&config.bind_addr).await?;
        axum::serve(listener, router(config.auth_token.as_deref(), dlq_scope)).await?;
        Ok(())
    }

    fn router(
        auth_token: Option<&str>,
        dlq_scope: Option<crate::core::a2a::dlq::DlqScope>,
    ) -> Router {
        let router = dlq_scope.map_or_else(
            crate::core::ocla::wire_api::ocla_router,
            crate::core::ocla::wire_api::ocla_router_with_dlq,
        );
        match auth_token.filter(|token| !token.is_empty()) {
            Some(token) => {
                let expected = Arc::new(token.as_bytes().to_vec());
                router.layer(middleware::from_fn(move |req, next| {
                    let expected = Arc::clone(&expected);
                    async move { auth_middleware(req, next, expected).await }
                }))
            }
            None => router,
        }
    }

    async fn auth_middleware(
        request: Request<Body>,
        next: Next,
        expected: Arc<Vec<u8>>,
    ) -> Response {
        let Some(value) = request.headers().get(header::AUTHORIZATION) else {
            return unauthorized();
        };
        let Ok(value) = value.to_str() else {
            return unauthorized();
        };
        let Some(token) = value
            .strip_prefix("Bearer ")
            .or_else(|| value.strip_prefix("bearer "))
        else {
            return unauthorized();
        };
        if !bool::from(token.as_bytes().ct_eq(expected.as_slice())) {
            return unauthorized();
        }
        next.run(request).await
    }

    fn unauthorized() -> Response {
        (StatusCode::UNAUTHORIZED, "unauthorized\n").into_response()
    }

    #[cfg(test)]
    mod tests {
        use axum::body::{Body, to_bytes};
        use axum::http::{Request, StatusCode, header};
        use tower::ServiceExt;

        use super::{SidecarConfig, router, start};

        #[tokio::test]
        async fn sidecar_requires_auth_when_enabled() {
            for auth_token in [None, Some(String::new())] {
                let config = SidecarConfig {
                    enabled: true,
                    auth_token,
                    ..Default::default()
                };

                let error = start(&config).await.expect_err("missing auth token");
                assert!(
                    error
                        .to_string()
                        .contains("OCLA sidecar requires auth_token when enabled")
                );
            }
        }

        #[tokio::test]
        async fn auth_rejects_missing_and_wrong_bearer_tokens() {
            for authorization in [None, Some("Bearer wrong"), Some("Basic secret")] {
                let mut request = Request::builder().method("GET").uri("/ocla/v1/health");
                if let Some(value) = authorization {
                    request = request.header(header::AUTHORIZATION, value);
                }
                let response = router(Some("secret"), None)
                    .oneshot(request.body(Body::empty()).expect("request"))
                    .await
                    .expect("response");
                assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            }
        }

        #[tokio::test]
        async fn auth_accepts_matching_bearer_token() {
            let response = router(Some("secret"), None)
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri("/ocla/v1/health")
                        .header(header::AUTHORIZATION, "Bearer secret")
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::OK);
            let body = to_bytes(response.into_body(), 4096).await.expect("body");
            assert!(body.windows(2).any(|window| window == b"ok"));
        }
    }
}

#[cfg(feature = "http-server")]
pub async fn start_sidecar(config: &SidecarConfig) -> anyhow::Result<()> {
    http::start(config).await
}

#[cfg(not(feature = "http-server"))]
pub async fn start_sidecar(_config: &SidecarConfig) -> anyhow::Result<()> {
    Err(anyhow::anyhow!(
        "OCLA sidecar requires the `http-server` feature"
    ))
}
