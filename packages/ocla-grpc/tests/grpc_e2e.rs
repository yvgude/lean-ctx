//! Real TCP conformance coverage for the OCLA gRPC transport.

use std::time::Duration;
use std::{process::Command, str};

use lean_ctx_client::{
    AgentEnvelopeV1, CanonicalTokenEnvelopeV1, TokenEnvelopeSurface, TokenFlowDirection,
};
use lean_ctx_ocla_grpc::proto::ocla_verifier_client::OclaVerifierClient;
use lean_ctx_ocla_grpc::proto::{
    AgentEnvelope, CanonicalTokenEnvelope, Direction, Rejection, RequestContext, Surface,
    TokenBalance,
};
use lean_ctx_ocla_grpc::{MAX_GRPC_MESSAGE_BYTES, parse_loopback_addr, serve};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tonic::Code;
use tonic_health::pb::health_client::HealthClient;
use tonic_health::pb::{HealthCheckRequest, health_check_response};

fn token_fixture() -> CanonicalTokenEnvelopeV1 {
    serde_json::from_slice(include_bytes!(
        "../../../clients/rust/lean-ctx-client/tests/fixtures/canonical-token-envelope-v1.json"
    ))
    .unwrap()
}

fn agent_fixture(name: &str) -> AgentEnvelopeV1 {
    let bytes: &[u8] = match name {
        "valid" => include_bytes!(
            "../../../clients/rust/lean-ctx-client/tests/fixtures/agent-envelope-v1.json"
        ),
        "self-relay" => include_bytes!(
            "../../../clients/rust/lean-ctx-client/tests/fixtures/self-relay-agent-envelope-v1.json"
        ),
        _ => panic!("unknown fixture"),
    };
    serde_json::from_slice(bytes).unwrap()
}

fn context(value: lean_ctx_client::OclaRequestContext) -> RequestContext {
    RequestContext {
        request_id: value.request_id,
        session_id: value.session_id,
        agent_id: value.agent_id,
        content_ref: value.content_ref,
        tenant_id: value.tenant_id,
    }
}

fn token_message(value: CanonicalTokenEnvelopeV1) -> CanonicalTokenEnvelope {
    let surface = match value.surface {
        TokenEnvelopeSurface::Mcp => Surface::Mcp,
        TokenEnvelopeSurface::Proxy => Surface::Proxy,
        TokenEnvelopeSurface::Shell => Surface::Shell,
        TokenEnvelopeSurface::Agent => Surface::Agent,
    };
    let direction = match value.direction {
        TokenFlowDirection::Input => Direction::Input,
        TokenFlowDirection::Output => Direction::Output,
    };
    CanonicalTokenEnvelope {
        schema_version: value.schema_version.into(),
        context: Some(context(value.context)),
        surface: surface.into(),
        direction: direction.into(),
        provider: value.provider,
        model: value.model,
        token_balance: Some(TokenBalance {
            original_tokens: value.token_balance.original_tokens,
            materialized_tokens: value.token_balance.materialized_tokens,
            delivered_tokens: value.token_balance.delivered_tokens,
            provider_billed_tokens: value.token_balance.provider_billed_tokens,
        }),
        route_ref: value.route_ref,
        policy_ref: value.policy_ref,
        idempotency_key: value.idempotency_key,
    }
}

fn agent_message(value: AgentEnvelopeV1) -> AgentEnvelope {
    AgentEnvelope {
        schema_version: value.schema_version.into(),
        relay_id: value.relay_id,
        context: Some(context(value.context)),
        from_agent_id: value.from_agent_id,
        to_agent_id: value.to_agent_id,
        capsule_ref: value.capsule_ref,
        budget_tokens: value.budget_tokens,
    }
}

async fn assert_rejection(
    client: &mut OclaVerifierClient<tonic::transport::Channel>,
    message: CanonicalTokenEnvelope,
    expected: Rejection,
) {
    let result = client
        .verify_token_envelope(message)
        .await
        .unwrap()
        .into_inner();
    assert!(!result.accepted);
    assert_eq!(result.rejection, expected as i32);
    assert_eq!(result.api_version, "ocla/v1");
}

#[tokio::test]
async fn real_loopback_grpc_verifies_health_and_contract_boundaries() {
    assert!(parse_loopback_addr("127.0.0.1:50051").is_ok());
    assert!(parse_loopback_addr("[::1]:50051").is_ok());
    assert!(parse_loopback_addr("0.0.0.0:50051").is_err());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        serve(listener, async move {
            let _ = shutdown_rx.await;
        })
        .await
        .unwrap();
    });
    let endpoint = format!("http://{address}");
    let mut client = tokio::time::timeout(
        Duration::from_secs(5),
        OclaVerifierClient::connect(endpoint.clone()),
    )
    .await
    .unwrap()
    .unwrap();

    let channel = tonic::transport::Channel::from_shared(endpoint)
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut health = HealthClient::new(channel);
    let status = health
        .check(HealthCheckRequest {
            service: "leanctx.ocla.v1.OclaVerifier".into(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        status.status,
        health_check_response::ServingStatus::Serving as i32
    );

    let valid_token = client
        .verify_token_envelope(token_message(token_fixture()))
        .await
        .unwrap()
        .into_inner();
    assert!(valid_token.accepted);
    assert_eq!(valid_token.rejection, Rejection::None as i32);

    let mut invalid_accounting = token_message(token_fixture());
    invalid_accounting
        .token_balance
        .as_mut()
        .unwrap()
        .materialized_tokens = 101;
    assert_rejection(&mut client, invalid_accounting, Rejection::InvalidInvariant).await;

    let mut invalid_lineage = token_message(token_fixture());
    invalid_lineage.context = None;
    assert_rejection(&mut client, invalid_lineage, Rejection::InvalidInvariant).await;

    let mut unsupported = token_message(token_fixture());
    unsupported.schema_version = u32::from(u16::MAX) + 1;
    assert_rejection(&mut client, unsupported, Rejection::UnsupportedVersion).await;

    let valid_agent = client
        .verify_agent_envelope(agent_message(agent_fixture("valid")))
        .await
        .unwrap()
        .into_inner();
    assert!(valid_agent.accepted);

    let self_relay = client
        .verify_agent_envelope(agent_message(agent_fixture("self-relay")))
        .await
        .unwrap()
        .into_inner();
    assert!(!self_relay.accepted);
    assert_eq!(self_relay.rejection, Rejection::SelfRelay as i32);

    let mut oversized = token_message(token_fixture());
    oversized.provider = "x".repeat(MAX_GRPC_MESSAGE_BYTES);
    let status = client.verify_token_envelope(oversized).await.unwrap_err();
    assert_eq!(status.code(), Code::OutOfRange);

    shutdown_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
}

#[test]
fn executable_rejects_remote_bind_without_echoing_input() {
    let output = Command::new(env!("CARGO_BIN_EXE_lean-ctx-ocla-grpc"))
        .arg("--listen=0.0.0.0:50051")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(
        str::from_utf8(&output.stderr).unwrap(),
        "OCLA gRPC server rejected\n"
    );
}
