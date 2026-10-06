// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::core::archive::authority::ArchiveAuthority;
use crate::core::policy::runtime::{self, TestPolicyOverride};
use crate::server::reference_store;
use std::time::Duration;

#[tokio::test]
async fn tee_http_auth_cannot_authorize_originless_recovery() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let original = "community tee HTTP payload\n".repeat(40);
    let hash = ccr::litellm_hash(&original);
    {
        let _community = TestPolicyOverride::set(None);
        assert!(ccr::persist(&original).is_some());
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    for loopback_open in [false, true] {
        let (_sender, upstreams) = tokio::sync::watch::channel(Arc::new(
            crate::core::config::Config::default()
                .proxy
                .resolve_all_disk(),
        ));
        #[cfg(feature = "enterprise")]
        let gateway_keys = Arc::new(gateway_identity::GatewayKeys::default());
        let app = axum::Router::new()
            .route("/v1/retrieve/{hash}", axum::routing::get(v1_retrieve_ccr))
            .layer(axum::middleware::from_fn(move |request, next| {
                proxy_auth_guard(
                    request,
                    next,
                    "tee-fixture-token".into(),
                    true,
                    loopback_open,
                    #[cfg(feature = "enterprise")]
                    gateway_keys.clone(),
                    upstreams.clone(),
                )
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .unwrap();
        });
        for protected in [false, true] {
            let _policy = TestPolicyOverride::set(protected.then(|| {
                crate::core::policy::load("name='tee-http'\nversion='1.0.0'\ndescription='test'\n")
                    .unwrap()
            }));
            for authenticated in [false, true] {
                let mut request = client.get(format!("http://{address}/v1/retrieve/{hash}"));
                if authenticated {
                    request = request.bearer_auth("tee-fixture-token");
                }
                let response = request.send().await.unwrap();
                let authorized = authenticated || loopback_open;
                let expected = if !authorized {
                    401
                } else if protected {
                    404
                } else {
                    200
                };
                assert_eq!(response.status().as_u16(), expected);
                assert_eq!(
                    response
                        .text()
                        .await
                        .unwrap()
                        .contains("community tee HTTP payload"),
                    authorized && !protected
                );
            }
        }
        stop.send(()).unwrap();
        server.await.unwrap();
    }
}

#[tokio::test]
async fn reference_http_auth_does_not_supply_protected_source_identity() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let community = TestPolicyOverride::set(None);
    let public_id = reference_store::store("community HTTP payload").unwrap();
    drop(community);
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source.txt");
    std::fs::write(&path, "allowed source").unwrap();
    let protected_id = {
        let _policy = TestPolicyOverride::set(Some(
            crate::core::policy::load(
                "name='http-reference'\nversion='1.0.0'\ndescription='test'\n",
            )
            .unwrap(),
        ));
        runtime::REQUEST_PROJECT.sync_scope(
            std::cell::RefCell::new(Some(root.path().to_path_buf())),
            || {
                runtime::with_source_view(|| {
                    let mut remaining = crate::core::limits::max_read_bytes();
                    let read = crate::tools::ctx_read::read_file_for_tool_rooted_with_path(
                        path.to_str().unwrap(),
                        root.path().to_str().unwrap(),
                        "ctx_execute",
                        &mut remaining,
                    )
                    .unwrap();
                    let origin = ArchiveAuthority::file(
                        root.path(),
                        &read.canonical_path,
                        "ctx_execute",
                        &read.content,
                    )
                    .unwrap();
                    reference_store::store_with_authority("protected HTTP payload", Some(&origin))
                        .unwrap()
                })
                .unwrap()
            },
        )
    };
    let _community = TestPolicyOverride::set(None);
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    for loopback_open in [false, true] {
        let (_upstream_sender, upstreams) = tokio::sync::watch::channel(Arc::new(
            crate::core::config::Config::default()
                .proxy
                .resolve_all_disk(),
        ));
        #[cfg(feature = "enterprise")]
        let gateway_keys = Arc::new(gateway_identity::GatewayKeys::default());
        // Real production handler and auth middleware, served over a disposable
        // TCP listener. No provider/model route exists in this test router.
        let app = axum::Router::new()
            .route(
                "/v1/references/{id}",
                axum::routing::get(v1_resolve_reference),
            )
            .layer(axum::middleware::from_fn(move |request, next| {
                proxy_auth_guard(
                    request,
                    next,
                    "reference-fixture-token".into(),
                    true,
                    loopback_open,
                    #[cfg(feature = "enterprise")]
                    gateway_keys.clone(),
                    upstreams.clone(),
                )
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .unwrap();
        });
        for authenticated in [false, true] {
            for (id, protected) in [(&public_id, false), (&protected_id, true)] {
                let mut request = client.get(format!("http://{address}/v1/references/{id}"));
                if authenticated {
                    request = request.bearer_auth("reference-fixture-token");
                }
                let response = request.send().await.unwrap();
                let admitted = authenticated || loopback_open;
                let expected = if !admitted {
                    401
                } else if protected {
                    404
                } else {
                    200
                };
                assert_eq!(response.status().as_u16(), expected);
                let body = response.text().await.unwrap();
                assert!(!body.contains("protected HTTP payload"));
                assert_eq!(
                    body.contains("community HTTP payload"),
                    admitted && !protected
                );
            }
        }
        stop.send(()).unwrap();
        server.await.unwrap();
    }
}
