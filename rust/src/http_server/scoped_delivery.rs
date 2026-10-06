// SPDX-License-Identifier: Apache-2.0

//! Authenticated scoped delivery. Legacy routes never authorize this path.

use axum::response::IntoResponse;
use axum::{Json, extract::State, http::StatusCode, response::Response};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::relay_replay::{RelayReplayStore, Reservation, StoreLimits};
use super::{AppState, json_error};
use crate::core::a2a::task::{
    TaskAuthorityExpectationV1,
    delivery_authority::{DeliveryOperation, SignedDeliveryRequest},
};
use crate::core::ocla::registry::OclaRegistry;
use lean_ctx_ocla::delivery_scope::DeliveryScopeV1;

#[cfg(test)]
mod delegation_tests;

fn digest(bytes: &[u8]) -> String {
    crate::core::agent_identity::hex_encode(&Sha256::digest(bytes))
}

pub(super) async fn execute(
    State(state): State<AppState>,
    Json(request): Json<SignedDeliveryRequest>,
) -> Response {
    let (Some(recipient), Some(tenant), Some(project)) = (
        state.a2a_recipient_id.as_deref(),
        state.a2a_tenant_id.as_deref(),
        state.a2a_project_id.as_deref(),
    ) else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "scope_unconfigured",
            "delivery authority is not configured",
        );
    };
    let Ok(scope) = DeliveryScopeV1::new(tenant.to_owned(), project.to_owned()) else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "scope_unconfigured",
            "invalid delivery scope",
        );
    };
    let action = request.request.action();
    let now = chrono::Utc::now();
    let body_digest = request.bound_request_digest();
    let delegated_policy = if let Some(certificate) = &request.delegation {
        let certificate = certificate.clone();
        let root = state.project_root.clone();
        let policy = state.a2a_task_authority.clone();
        let recipient = recipient.to_owned();
        let tenant = tenant.to_owned();
        let project = project.to_owned();
        let (path, record_privacy) = match &request.request {
            DeliveryOperation::Check { path, .. } => (path.clone(), None),
            DeliveryOperation::Record { entry } => (
                entry.path.clone(),
                entry.access.as_ref().map(|access| access.privacy),
            ),
        };
        let derived = tokio::task::spawn_blocking(move || {
            let canonical_root = std::path::Path::new(&root)
                .canonicalize()
                .map_err(|error| error.to_string())?;
            let canonical_path = std::path::Path::new(&path)
                .canonicalize()
                .map_err(|error| error.to_string())?;
            if certificate.project_root != canonical_root
                || !canonical_root.is_dir()
                || !canonical_path.is_file()
                || !canonical_path.starts_with(&canonical_root)
                || record_privacy.is_some_and(|privacy| privacy != certificate.privacy)
            {
                return Err("delegated delivery path or privacy mismatch".to_owned());
            }
            let store = crate::core::work_graph_store::WorkGraphStore::load(&root)?;
            store.child_delivery_authority(
                &certificate,
                &policy,
                &TaskAuthorityExpectationV1 {
                    sender: &certificate.child_agent,
                    recipient: &recipient,
                    tenant_id: &tenant,
                    project_id: &project,
                    now: chrono::Utc::now(),
                },
            )
        })
        .await;
        match derived {
            Ok(Ok(policy)) => Some(policy),
            _ => {
                return json_error(
                    StatusCode::FORBIDDEN,
                    "delivery_unauthorized",
                    "delivery delegation is not authorized for a live execution",
                );
            }
        }
    } else {
        None
    };
    let authority = &request.authority;
    let expected = TaskAuthorityExpectationV1 {
        sender: &authority.sender,
        recipient,
        tenant_id: tenant,
        project_id: project,
        now,
    };
    // `sender` is bound independently to the configured peer's key by verify.
    if authority
        .verify_authority(
            delegated_policy
                .as_ref()
                .unwrap_or(&state.a2a_task_authority),
            &expected,
            action,
            &body_digest,
        )
        .is_err()
    {
        return json_error(
            StatusCode::FORBIDDEN,
            "delivery_unauthorized",
            "delivery authorization failed",
        );
    }
    match &request.request {
        DeliveryOperation::Check {
            path,
            conversation_id,
            ..
        } => {
            if path.trim().is_empty()
                || path.len() > 4096
                || path.contains('\0')
                || conversation_id
                    .as_ref()
                    .is_some_and(|id| id.is_empty() || id.len() > 256)
            {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_delivery",
                    "invalid delivery lookup",
                );
            }
        }
        DeliveryOperation::Record { entry } => {
            if entry.agent_id != authority.sender
                || entry
                    .access
                    .as_ref()
                    .is_none_or(|access| access.scope != scope)
            {
                return json_error(
                    StatusCode::FORBIDDEN,
                    "delivery_unauthorized",
                    "delivery owner or scope mismatch",
                );
            }
        }
    }
    // Same nonce cannot authorize another operation/body. The domain separates
    // this namespace from relay proofs sharing the same durable ledger.
    let storage_id = digest(&crate::core::canonical::canonical_serialize(&(
        "leanctx.delivery.nonce.v1",
        recipient,
        tenant,
        project,
        &authority.sender,
        &authority.nonce,
    )));
    let fingerprint = digest(&authority.signing_bytes());
    let root = state.project_root.clone();
    let authority = request.authority;
    let sender = authority.sender.clone();
    let replay = tokio::task::spawn_blocking(move || {
        let store = RelayReplayStore::new(
            root,
            StoreLimits::default(),
            super::relay_replay::MIN_RETENTION_SECONDS,
        )?;
        match store.reserve(
            &storage_id,
            &authority.sender,
            &authority.tenant_id,
            &authority.project_id,
            &fingerprint,
            authority.expires_at.timestamp(),
            now.timestamp(),
        )? {
            Reservation::Reserved(lease) => {
                // Consume before accessing content. A crash can lose this attempt,
                // never permit a duplicate; retries require a freshly signed nonce.
                store.complete(&storage_id, &lease)?;
                Ok::<bool, super::relay_replay::StoreError>(true)
            }
            _ => Ok(false),
        }
    })
    .await;
    match replay {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => {
            return json_error(
                StatusCode::CONFLICT,
                "delivery_replayed",
                "use a fresh signed request",
            );
        }
        Ok(Err(_)) | Err(_) => {
            return json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "delivery_replay_unavailable",
                "delivery replay guard unavailable",
            );
        }
    }
    let registry = OclaRegistry::global();
    match request.request {
        DeliveryOperation::Check {
            path,
            blake3,
            conversation_id,
        } => {
            let record = registry.delivery_registry.check_scoped_delivery(
                &blake3,
                &path,
                &scope,
                &sender,
                conversation_id.as_deref(),
            );
            Json(json!({"hit":record.is_some(), "record":record})).into_response()
        }
        DeliveryOperation::Record { entry } => {
            Json(json!(registry.delivery_registry.record_delivery(entry))).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::a2a::task::delivery_authority::{
        DELIVERY_CHECK, DELIVERY_RECORD, DeliveryAuthorityV1,
    };
    use crate::core::a2a::task::{
        CapabilityGrantRefV1, TaskAuthorityConfigV1, TaskCapabilityGrantV1, TaskPeerTrustV1,
        TaskScopeV1,
    };
    use crate::core::ocla::types::DeliveryEntry;
    use crate::http_server::{HttpServerConfig, build_app_router};
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    #[tokio::test]
    async fn scoped_http_rejects_tampering_and_durable_replay() {
        #[cfg(unix)]
        if std::env::var_os("LEAN_CTX_TEST_SCOPED_HOME").is_none() {
            let home = tempfile::Builder::new()
                .prefix("lc-ipc-")
                .tempdir_in("/tmp")
                .expect("isolated home");
            let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
                .args(["--exact", "http_server::scoped_delivery::tests::scoped_http_rejects_tampering_and_durable_replay", "--nocapture"])
                .env("HOME", home.path())
                .env("XDG_DATA_HOME", home.path().join("data"))
                .env("LEAN_CTX_DATA_DIR", home.path().join("keys-data"))
                .env("LEAN_CTX_TEST_SCOPED_HOME", home.path())
                .status()
                .expect("isolated IPC test");
            assert!(status.success());
            return;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let now = chrono::Utc::now();
        let key = ed25519_dalek::SigningKey::from_bytes(&[73; 32]);
        let mut cfg = HttpServerConfig {
            project_root: dir.path().to_owned(),
            auth_token: Some("test-bearer".into()),
            a2a_signing_key: Some("test-channel".into()),
            a2a_recipient_id: Some("daemon".into()),
            a2a_tenant_id: Some("account".into()),
            a2a_project_id: Some("project".into()),
            a2a_task_authority: TaskAuthorityConfigV1 {
                schema_version: 1,
                peers: vec![TaskPeerTrustV1 {
                    schema_version: 1,
                    key_id: "key".into(),
                    agent_id: "reader".into(),
                    public_key: crate::core::agent_identity::hex_encode(
                        key.verifying_key().as_bytes(),
                    ),
                    allowed_actions: vec![DELIVERY_CHECK.into()],
                    allowed_scopes: vec![TaskScopeV1 {
                        schema_version: 1,
                        tenant_id: "account".into(),
                        project_id: "project".into(),
                    }],
                    not_before: now - chrono::Duration::minutes(1),
                    expires_at: now + chrono::Duration::hours(1),
                    revoked: false,
                }],
                grants: vec![TaskCapabilityGrantV1 {
                    schema_version: 1,
                    grant_id: "grant".into(),
                    key_id: "key".into(),
                    action: DELIVERY_CHECK.into(),
                    tenant_id: "account".into(),
                    project_id: "project".into(),
                    not_before: now - chrono::Duration::minutes(1),
                    expires_at: now + chrono::Duration::hours(1),
                    revoked: false,
                }],
            },
            ..HttpServerConfig::default()
        };
        cfg.a2a_task_authority.peers[0]
            .allowed_actions
            .push(DELIVERY_RECORD.into());
        let mut write_grant = cfg.a2a_task_authority.grants[0].clone();
        write_grant.grant_id = "write-grant".into();
        write_grant.action = DELIVERY_RECORD.into();
        cfg.a2a_task_authority.grants.push(write_grant);
        #[cfg(unix)]
        let second_key = crate::core::agent_identity::get_or_create_keypair("second-reader")
            .expect("isolated child signing key");
        #[cfg(not(unix))]
        let second_key = ed25519_dalek::SigningKey::from_bytes(&[74; 32]);
        let mut second_peer = cfg.a2a_task_authority.peers[0].clone();
        second_peer.key_id = "second-key".into();
        second_peer.agent_id = "second-reader".into();
        second_peer.public_key =
            crate::core::agent_identity::hex_encode(second_key.verifying_key().as_bytes());
        second_peer.allowed_actions = vec![DELIVERY_CHECK.into()];
        cfg.a2a_task_authority.peers.push(second_peer);
        let mut second_grant = cfg.a2a_task_authority.grants[0].clone();
        second_grant.key_id = "second-key".into();
        second_grant.grant_id = "second-grant".into();
        cfg.a2a_task_authority.grants.push(second_grant);
        assert!(cfg.validate().is_ok());
        let operation = DeliveryOperation::Check {
            path: "/scoped-test/missing".into(),
            blake3: [73; 12],
            conversation_id: None,
        };
        let mut authority = DeliveryAuthorityV1 {
            schema_version: 1,
            sender: "reader".into(),
            recipient: "daemon".into(),
            tenant_id: "account".into(),
            project_id: "project".into(),
            action: DELIVERY_CHECK.into(),
            request_digest: format!(
                "sha256:{}",
                digest(&crate::core::canonical::canonical_serialize(&operation))
            ),
            issued_at: now,
            expires_at: now + chrono::Duration::minutes(1),
            nonce: "request-1".into(),
            grant_ref: CapabilityGrantRefV1 {
                schema_version: 1,
                grant_id: "grant".into(),
            },
            key_id: "key".into(),
            signature: String::new(),
        };
        authority.sign(&key);
        let body = json!({"authority":authority,"request":operation});
        let send = |body: serde_json::Value| {
            let app = build_app_router(&cfg, None);
            async move {
                app.oneshot(
                    Request::post("/ocla/v1/delivery/scoped")
                        .header("content-type", "application/json")
                        .header("authorization", "Bearer test-bearer")
                        .body(Body::from(serde_json::to_vec(&body).expect("JSON")))
                        .expect("request"),
                )
                .await
                .expect("response")
            }
        };
        let mut tampered = body.clone();
        tampered["request"]["path"] = json!("/other");
        assert_eq!(send(tampered).await.status(), StatusCode::FORBIDDEN);
        assert_eq!(send(body.clone()).await.status(), StatusCode::OK);
        // Each call rebuilds the router: durable state, not a process-local set.
        assert_eq!(send(body).await.status(), StatusCode::CONFLICT);

        // A valid peer signature cannot widen its configured tenant/project.
        for (tenant, project, nonce) in [
            ("other-account", "project", "wrong-account"),
            ("account", "other-project", "wrong-project"),
        ] {
            let mut proof = authority.clone();
            proof.tenant_id = tenant.into();
            proof.project_id = project.into();
            proof.nonce = nonce.into();
            proof.sign(&key);
            assert_eq!(
                send(json!({"authority":proof,"request":operation}))
                    .await
                    .status(),
                StatusCode::FORBIDDEN
            );
        }

        // Independent routers racing the same valid proof share durable replay
        // exclusion: exactly one may reach the registry.
        let mut concurrent = authority.clone();
        concurrent.nonce = "concurrent-request".into();
        concurrent.sign(&key);
        let concurrent_body = json!({"authority":concurrent,"request":operation});
        let (first, second) = tokio::join!(send(concurrent_body.clone()), send(concurrent_body));
        let mut statuses = [first.status().as_u16(), second.status().as_u16()];
        statuses.sort_unstable();
        assert_eq!(statuses, [200, 409]);

        let make_signed = |operation: DeliveryOperation, nonce: &str, write: bool| {
            let mut proof = authority.clone();
            proof.nonce = nonce.into();
            proof.action = if write {
                DELIVERY_RECORD
            } else {
                DELIVERY_CHECK
            }
            .into();
            proof.grant_ref.grant_id = if write { "write-grant" } else { "grant" }.into();
            proof.request_digest = format!(
                "sha256:{}",
                digest(&crate::core::canonical::canonical_serialize(&operation),)
            );
            proof.sign(&key);
            json!({"authority":proof,"request":operation})
        };
        let entry = DeliveryEntry {
            access: Some(lean_ctx_ocla::delivery_scope::DeliveryAccessV1 {
                scope: DeliveryScopeV1::new("account".into(), "project".into()).expect("scope"),
                privacy: lean_ctx_ocla::delivery_scope::DeliveryPrivacyV1::Private,
            }),
            blake3: [74; 12],
            path: dir
                .path()
                .join("private-context")
                .to_string_lossy()
                .into_owned(),
            line_count: 1,
            token_count: 100,
            agent_id: "reader".into(),
            conversation_id: "original".into(),
            mtime: 1,
            relay_content: Some("verified context".into()),
            relay_mode: Some("map".into()),
        };
        let mut forged = entry.clone();
        forged.agent_id = "another-agent".into();
        assert_eq!(
            send(make_signed(
                DeliveryOperation::Record { entry: forged },
                "forged-owner",
                true
            ))
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            send(make_signed(
                DeliveryOperation::Record {
                    entry: entry.clone()
                },
                "write-1",
                true
            ))
            .await
            .status(),
            StatusCode::OK
        );
        let lookup_body = make_signed(
            DeliveryOperation::Check {
                path: entry.path.clone(),
                blake3: entry.blake3,
                conversation_id: Some("next".into()),
            },
            "read-1",
            false,
        );
        let lookup: SignedDeliveryRequest =
            serde_json::from_value(lookup_body.clone()).expect("lookup");
        let response = send(lookup_body).await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 16_384)
            .await
            .expect("body");
        let result: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
        assert_eq!(result["hit"], true);
        assert_eq!(result["record"]["relay_content"], "verified context");
        assert_eq!(result["record"]["access"]["privacy"], "private");
        let other_request = |nonce: &str| {
            let mut request: SignedDeliveryRequest = serde_json::from_value(make_signed(
                DeliveryOperation::Check {
                    path: entry.path.clone(),
                    blake3: entry.blake3,
                    conversation_id: Some("second-conversation".into()),
                },
                nonce,
                false,
            ))
            .expect("request");
            request.authority.sender = "second-reader".into();
            request.authority.key_id = "second-key".into();
            request.authority.grant_ref.grant_id = "second-grant".into();
            request.authority.sign(&second_key);
            serde_json::to_value(request).expect("JSON")
        };
        let denied = send(other_request("private-read")).await;
        assert_eq!(denied.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(denied.into_body(), 16_384)
            .await
            .expect("body");
        let denied: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
        assert_eq!(denied, json!({"hit":false,"record":null}));
        let mut shared = entry.clone();
        shared.access.as_mut().expect("access").privacy =
            lean_ctx_ocla::delivery_scope::DeliveryPrivacyV1::Project;
        assert_eq!(
            send(make_signed(
                DeliveryOperation::Record { entry: shared },
                "share-write",
                true
            ))
            .await
            .status(),
            StatusCode::OK
        );
        let shared = send(other_request("shared-read")).await;
        assert_eq!(shared.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(shared.into_body(), 16_384)
            .await
            .expect("body");
        let shared: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
        assert_eq!(shared["hit"], true);
        assert_eq!(shared["record"]["relay_content"], "verified context");
        assert_eq!(shared["record"]["access"]["privacy"], "project");
        #[cfg(unix)]
        {
            let crate::ipc::DaemonAddr::Unix(socket) = crate::daemon::daemon_addr();
            let home = std::path::PathBuf::from(
                std::env::var_os("LEAN_CTX_TEST_SCOPED_HOME").expect("isolated process"),
            );
            assert!(socket.starts_with(&home), "never bind the user's socket");
            assert!(!crate::daemon::daemon_pid_path().exists());
            let file = dir.path().join("signed-read.rs");
            std::fs::write(&file, "pub struct SignedRelay { pub value: u64 }\n")
                .expect("source fixture");
            let file = file.canonicalize().expect("canonical source");
            let bytes = std::fs::read(&file).expect("source bytes");
            let mut prefix = [0; 12];
            prefix.copy_from_slice(&blake3::hash(&bytes).as_bytes()[..12]);
            let mut context_entry = entry.clone();
            context_entry.path = file.to_string_lossy().into_owned();
            context_entry.blake3 = prefix;
            context_entry.mtime = std::fs::metadata(&file)
                .expect("metadata")
                .modified()
                .expect("mtime")
                .duration_since(std::time::UNIX_EPOCH)
                .expect("epoch")
                .as_secs();
            context_entry.access.as_mut().expect("access").privacy =
                lean_ctx_ocla::delivery_scope::DeliveryPrivacyV1::Project;
            context_entry.relay_mode = Some("map:v2".into());
            context_entry.relay_content = Some("pub struct SignedRelay { pub value: u64 }".into());
            assert_eq!(
                send(make_signed(
                    DeliveryOperation::Record {
                        entry: context_entry
                    },
                    "signed-ctx-read-write",
                    true
                ))
                .await
                .status(),
                StatusCode::OK
            );
            let profile = crate::core::a2a::task::delivery_authority::DeliverySigningProfileV1 {
                schema_version: 1,
                agent_id: "second-reader".into(),
                delegation: None,
                recipient: "daemon".into(),
                tenant_id: "account".into(),
                project_id: "project".into(),
                project_root: dir.path().canonicalize().expect("root"),
                key_id: "second-key".into(),
                read_grant_id: "second-grant".into(),
                write_grant_id: "no-write-grant".into(),
                privacy: lean_ctx_ocla::delivery_scope::DeliveryPrivacyV1::Project,
            };
            std::fs::create_dir_all(socket.parent().expect("socket parent"))
                .expect("isolated socket directory");
            let listener = tokio::net::UnixListener::bind(&socket).expect("isolated IPC socket");
            let app = crate::http_server::build_app_router_with_auth(&cfg, false, None);
            let server = tokio::spawn(async move {
                axum::serve(listener, app).await.expect("IPC server");
            });
            let request: SignedDeliveryRequest =
                serde_json::from_value(other_request("ipc-read")).expect("IPC request");
            let response = crate::daemon_client::scoped_delivery_request(&request).await;
            assert!(socket.exists(), "delivery must not remove the live socket");
            assert!(!crate::daemon::daemon_pid_path().exists());
            let status = tokio::task::spawn_blocking(move || {
                std::process::Command::new(std::env::current_exe().expect("test executable"))
                    .args([
                        "--exact",
                        "http_server::scoped_delivery::tests::signed_ctx_read_child",
                        "--nocapture",
                    ])
                    .env("LEAN_CTX_TEST_SIGNED_READ_PATH", &file)
                    .env("LEAN_CTX_AGENT_ID", "second-reader")
                    .env(
                        "LEAN_CTX_DELIVERY_PROFILE",
                        serde_json::to_string(&profile).expect("profile"),
                    )
                    .status()
                    .expect("context reader")
            })
            .await
            .expect("reader task");
            server.abort();
            let _ = server.await;
            assert!(status.success(), "signed ctx_read child failed");
            let record = crate::daemon_client::decode_scoped_delivery_hit(
                &request,
                response.expect("real IPC exchange"),
            )
            .expect("authorized response")
            .expect("shared hit");
            assert_eq!(record.relay_content.as_deref(), Some("verified context"));
        }
        let decoded = crate::daemon_client::decode_scoped_delivery_hit(&lookup, result.clone())
            .expect("valid response")
            .expect("hit");
        assert_eq!(decoded.relay_content.as_deref(), Some("verified context"));
        for (pointer, replacement) in [
            ("/record/access", serde_json::Value::Null),
            ("/record/access/scope/account_id", json!("other-account")),
            ("/record/access/scope/project_id", json!("other-project")),
            ("/record/agent_id", json!("other-agent")),
            ("/record/path", json!("/other/path")),
            ("/record/blake3", json!(vec![0; 12])),
            ("/record/conversation_id", json!("next")),
            ("/hit", json!(false)),
        ] {
            let mut invalid = result.clone();
            *invalid.pointer_mut(pointer).expect("response field") = replacement;
            assert!(
                crate::daemon_client::decode_scoped_delivery_hit(&lookup, invalid).is_err(),
                "{pointer}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn signed_ctx_read_child() {
        use crate::server::tool_trait::{McpTool, ToolContext};
        let Some(path) = std::env::var_os("LEAN_CTX_TEST_SIGNED_READ_PATH") else {
            return;
        };
        let path = std::path::PathBuf::from(path);
        let ctx = ToolContext {
            project_root: path.parent().expect("root").to_string_lossy().into_owned(),
            resolved_paths: std::collections::HashMap::from([(
                "path".into(),
                path.to_string_lossy().into_owned(),
            )]),
            cache: Some(std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::core::cache::SessionCache::new(),
            ))),
            session: Some(std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::core::session::SessionState::new(),
            ))),
            ..ToolContext::default()
        };
        let args = json!({"path":path,"mode":"map"})
            .as_object()
            .expect("args")
            .clone();
        let output = crate::tools::registered::ctx_read::CtxReadTool
            .handle(&args, &ctx)
            .expect("signed context read");
        assert!(output.text.contains("relayed from"), "{}", output.text);
        assert!(
            output
                .text
                .contains("pub struct SignedRelay { pub value: u64 }"),
            "{}",
            output.text
        );
    }
}
