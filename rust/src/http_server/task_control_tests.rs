// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::core::a2a::{task::*, task_response::SignedTaskStatusV1};
use crate::core::a2a_transport::{AgentIdentityV1, TransportContentType, TransportEnvelopeV1};
use axum::body::{Body, to_bytes};
use chrono::{Duration as ChronoDuration, Utc};
use ed25519_dalek::SigningKey;
use tower::ServiceExt;

struct Fixture {
    _root: tempfile::TempDir,
    config: HttpServerConfig,
    owner: SigningKey,
    request: TaskControlDescriptorV1,
    path: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("root");
        let receiver = format!("task-receiver-{}", uuid::Uuid::new_v4());
        let owner = SigningKey::from_bytes(&[17; 32]);
        // Explicit fixture provisioning, never called by request processing.
        let receiver_key =
            crate::core::agent_identity::get_or_create_keypair(&receiver).expect("key");
        let now = Utc::now();
        let peer = |agent: &str, id: &str, key: &SigningKey| TaskPeerTrustV1 {
            schema_version: 1,
            key_id: id.into(),
            agent_id: agent.into(),
            public_key: crate::core::agent_identity::hex_encode(key.verifying_key().as_bytes()),
            allowed_actions: vec![
                TASK_ACTION_SEND.into(),
                TASK_ACTION_GET.into(),
                TASK_ACTION_CANCEL.into(),
            ],
            allowed_scopes: vec![TaskScopeV1 {
                schema_version: 1,
                tenant_id: "tenant".into(),
                project_id: "project".into(),
            }],
            not_before: now - ChronoDuration::hours(1),
            expires_at: now + ChronoDuration::hours(1),
            revoked: false,
        };
        let policy = TaskAuthorityConfigV1 {
            schema_version: 1,
            peers: vec![
                peer("owner", "owner-key", &owner),
                peer(&receiver, "receiver-key", &receiver_key),
            ],
            grants: [TASK_ACTION_SEND, TASK_ACTION_GET, TASK_ACTION_CANCEL]
                .into_iter()
                .map(|action| TaskCapabilityGrantV1 {
                    schema_version: 1,
                    grant_id: action.into(),
                    key_id: "owner-key".into(),
                    action: action.into(),
                    tenant_id: "tenant".into(),
                    project_id: "project".into(),
                    not_before: now - ChronoDuration::hours(1),
                    expires_at: now + ChronoDuration::hours(1),
                    revoked: false,
                })
                .collect(),
        };
        let mut descriptor = TaskDescriptorV1::new(
            "owner",
            &receiver,
            "tenant",
            "project",
            TASK_ACTION_SEND,
            now,
            now + ChronoDuration::minutes(10),
            "task-fixture",
            "not disclosed in status",
            vec![],
            TASK_ACTION_SEND,
            "owner-key",
        );
        descriptor.sign(&owner);
        let path = TaskStore::scoped_path(root.path().to_str().expect("path")).expect("scope");
        let task = TaskStore::materialize_remote_task(&path, &descriptor).expect("persist task");
        let request = TaskControlDescriptorV1::new(
            "owner",
            &receiver,
            "tenant",
            "project",
            TASK_ACTION_GET,
            &task.task_id,
            &descriptor.content_digest(),
            now,
            now + ChronoDuration::minutes(5),
            "get-1",
            None,
            TASK_ACTION_GET,
            "owner-key",
        );
        let config = HttpServerConfig {
            project_root: root.path().into(),
            auth_token: Some("control-fixture-bearer".into()),
            a2a_signing_key: Some("control-fixture-channel".into()),
            a2a_recipient_id: Some(receiver),
            a2a_tenant_id: Some("tenant".into()),
            a2a_project_id: Some("project".into()),
            a2a_task_authority: policy,
            ..HttpServerConfig::default()
        };
        config.validate().expect("valid config");
        Self {
            _root: root,
            config,
            owner,
            request,
            path,
        }
    }

    fn control(&self, action: &str, nonce: &str) -> TaskControlDescriptorV1 {
        let mut request = self.request.clone();
        request.action = action.into();
        request.grant_ref.grant_id = action.into();
        request.nonce = nonce.into();
        request.sign(&self.owner);
        request
    }

    fn envelope(&self, request: &TaskControlDescriptorV1) -> String {
        let mut envelope = TransportEnvelopeV1::new(
            AgentIdentityV1 {
                agent_id: "owner".into(),
                agent_type: "fixture".into(),
                daemon_fingerprint: "fixture".into(),
                capabilities: vec![],
            },
            self.config.a2a_recipient_id.as_deref(),
            TransportContentType::A2ATask,
            serde_json::to_string(request).expect("request"),
        );
        envelope
            .metadata
            .insert("tenant_id".into(), "tenant".into());
        envelope
            .metadata
            .insert("project_id".into(), "project".into());
        envelope
            .sign(b"control-fixture-channel")
            .expect("channel signature");
        serde_json::to_string(&envelope).expect("envelope")
    }
}

async fn deliver(app: Router, body: &str, authenticated: bool) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri("/a2a/deliver")
        .header("Host", "localhost")
        .header("Content-Type", "application/json");
    if authenticated {
        request = request.header(
            "Authorization",
            format!("Bearer {}", "control-fixture-bearer"),
        );
    }
    let reply = app
        .oneshot(
            request
                .body(Body::from(body.to_owned()))
                .expect("HTTP request"),
        )
        .await
        .expect("HTTP response");
    let status = reply.status();
    let bytes = to_bytes(reply.into_body(), 65536)
        .await
        .expect("bounded body");
    (
        status,
        serde_json::from_slice(&bytes).expect("JSON response"),
    )
}

fn verified(
    body: &Value,
    request: &TaskControlDescriptorV1,
    config: &HttpServerConfig,
) -> SignedTaskStatusV1 {
    let reply = SignedTaskStatusV1::from_json(&body.to_string()).expect("signed status");
    reply
        .verify(request, &config.a2a_task_authority, Utc::now())
        .expect("receiver provenance");
    assert!(!body.to_string().contains("not disclosed"));
    reply
}

#[tokio::test]
async fn signed_control_http_get_cancel_replay_and_router_restart() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let fixture = Fixture::new();
    let app = build_app_router(&fixture.config, None);
    let get = fixture.control(TASK_ACTION_GET, "get-1");
    let (code, body) = deliver(app.clone(), &fixture.envelope(&get), true).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(
        verified(&body, &get, &fixture.config).status.state,
        TaskState::Created
    );
    let cancel = fixture.control(TASK_ACTION_CANCEL, "cancel-1");
    let body = fixture.envelope(&cancel);
    let (code, response) = deliver(app.clone(), &body, true).await;
    assert_eq!(code, StatusCode::OK);
    let first = verified(&response, &cancel, &fixture.config);
    assert_eq!(first.status.state, TaskState::Canceled);
    let (code, response) = deliver(app, &body, true).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(
        verified(&response, &cancel, &fixture.config).status,
        first.status
    );
    let (code, response) = deliver(build_app_router(&fixture.config, None), &body, true).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(
        verified(&response, &cancel, &fixture.config).status,
        first.status
    );
    let history = TaskStore::read_locked(&fixture.path, |store| {
        store.get_task(&cancel.task_id).expect("task").history.len()
    })
    .expect("read");
    assert_eq!(history, 2);
}

#[tokio::test]
async fn signed_control_http_denials_preserve_task_bytes() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let fixture = Fixture::new();
    let before = std::fs::read(&fixture.path).expect("before");
    for case in [
        "bearer",
        "signature",
        "grant",
        "owner",
        "digest",
        "signer-revoked",
        "signer-scope",
        "signer-key",
        "key-case-alias",
        "signer-missing",
    ] {
        let mut config = fixture.config.clone();
        let mut request = fixture.control(TASK_ACTION_CANCEL, case);
        let expected = match case {
            "bearer" => StatusCode::UNAUTHORIZED,
            "signature" => {
                request.nonce = "tampered".into();
                StatusCode::UNAUTHORIZED
            }
            "grant" => {
                config.a2a_task_authority.grants[2].revoked = true;
                StatusCode::UNAUTHORIZED
            }
            "owner" => {
                request.sender = "foreign".into();
                request.sign(&fixture.owner);
                StatusCode::UNAUTHORIZED
            }
            "digest" => {
                request.task_descriptor_digest = format!("sha256:{}", "b".repeat(64));
                request.sign(&fixture.owner);
                StatusCode::NOT_FOUND
            }
            "signer-revoked" => {
                config.a2a_task_authority.peers[1].revoked = true;
                StatusCode::SERVICE_UNAVAILABLE
            }
            "signer-scope" => {
                config.a2a_task_authority.peers[1].allowed_scopes[0].project_id = "foreign".into();
                StatusCode::SERVICE_UNAVAILABLE
            }
            "signer-key" => {
                config.a2a_task_authority.peers[1].public_key =
                    crate::core::agent_identity::hex_encode(
                        SigningKey::from_bytes(&[29; 32]).verifying_key().as_bytes(),
                    );
                StatusCode::SERVICE_UNAVAILABLE
            }
            "key-case-alias" => {
                let mut alias = config.a2a_task_authority.peers[0].clone();
                alias.key_id = "case-alias".into();
                alias.public_key.make_ascii_uppercase();
                config.a2a_task_authority.peers.push(alias);
                StatusCode::UNAUTHORIZED
            }
            "signer-missing" => {
                let path = crate::core::data_dir::lean_ctx_data_dir()
                    .expect("data")
                    .join("keys")
                    .join(format!("{}.key", request.recipient));
                std::fs::remove_file(&path).expect("remove only owned fixture key");
                StatusCode::SERVICE_UNAVAILABLE
            }
            _ => unreachable!("closed fixture cases"),
        };
        let (code, body) = deliver(
            build_app_router(&config, None),
            &fixture.envelope(&request),
            case != "bearer",
        )
        .await;
        assert_eq!(code, expected, "{case}: {body}");
        assert!(body.get("signature").is_none(), "{case}");
        assert_eq!(
            std::fs::read(&fixture.path).expect("after"),
            before,
            "{case}"
        );
        if case == "signer-missing" {
            let path = crate::core::data_dir::lean_ctx_data_dir()
                .expect("data")
                .join("keys")
                .join(format!("{}.key", request.recipient));
            assert!(!path.exists(), "request must not create a replacement key");
        }
    }
}

#[tokio::test]
async fn signed_control_http_reply_respects_receiver_key_expiry() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let mut fixture = Fixture::new();
    let deadline = Utc::now() + ChronoDuration::seconds(30);
    fixture.config.a2a_task_authority.peers[1].expires_at = deadline;
    let request = fixture.control(TASK_ACTION_GET, "short-key");
    let (code, body) = deliver(
        build_app_router(&fixture.config, None),
        &fixture.envelope(&request),
        true,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(
        verified(&body, &request, &fixture.config).expires_at,
        deadline
    );
}
