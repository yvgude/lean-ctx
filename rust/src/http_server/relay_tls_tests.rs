// SPDX-License-Identifier: Apache-2.0

//! Real TLS termination in front of the production router; no TLS bypass.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use tokio::task::{JoinHandle, JoinSet};

use super::{HttpServerConfig, build_app_router};
use crate::core::a2a::relay::{
    RelayPeerConfigV1, RelayPeerTableV1, RelayRecordV1, test_origin_key, test_origin_public_key,
};
use crate::core::a2a::remote_transport::RemoteTransport;
use crate::core::a2a_transport::{AgentIdentityV1, TransportContentType, TransportEnvelopeV1};
use crate::core::context_kernel::evidence_bundle::EvidenceBundle;
use lean_ctx_protocol::DataClassification;

struct RunningServer(JoinHandle<()>);

impl Drop for RunningServer {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn peer(id: &str, endpoint_url: String) -> RelayPeerConfigV1 {
    RelayPeerConfigV1 {
        schema_version: 1,
        peer_id: id.into(),
        endpoint_url,
        bearer_token: "tls-test-hop-bearer".into(),
        channel_key: "tls-test-hop-channel".into(),
        origin_public_key: test_origin_public_key(id),
        recipient_id: "recipient".into(),
        allowed_tenant_ids: vec!["tenant".into()],
        allowed_project_ids: vec!["project".into()],
        allowed_content_types: vec![TransportContentType::EvidenceBundle],
        allowed_classifications: vec![DataClassification::Internal],
        max_hops: 4,
        max_payload_bytes: 16 * 1024,
        retry_count: 0,
        retry_delay_ms: 0,
    }
}

#[tokio::test]
async fn relay_https_validates_certificate_and_production_configuration() {
    // Public test identity only. Never deploy this key or add it to OS trust.
    let cert = base64::engine::general_purpose::STANDARD
        .decode(include_str!("../../tests/fixtures/p17-relay-tls/cert.der.b64").trim())
        .unwrap();
    let key = base64::engine::general_purpose::STANDARD
        .decode(include_str!("../../tests/fixtures/p17-relay-tls/key.pk8.b64").trim())
        .unwrap();
    let ca = base64::engine::general_purpose::STANDARD
        .decode(include_str!("../../tests/fixtures/p17-relay-tls/ca.der.b64").trim())
        .unwrap();
    let tls_config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![rustls::pki_types::CertificateDer::from(cert.clone())],
        rustls::pki_types::PrivatePkcs8KeyDer::from(key).into(),
    )
    .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls_config));
    let tls_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("https://{}", tls_listener.local_addr().unwrap());
    let root = tempfile::tempdir().unwrap();
    let cfg = HttpServerConfig {
        project_root: root.path().to_path_buf(),
        auth_token: Some("tls-test-hop-bearer".into()),
        a2a_recipient_id: Some("recipient".into()),
        a2a_tenant_id: Some("tenant".into()),
        a2a_project_id: Some("project".into()),
        a2a_peers: RelayPeerTableV1 {
            schema_version: 1,
            peers: vec![peer("origin", "https://origin.example".into())],
        },
        ..HttpServerConfig::default()
    };
    cfg.validate().unwrap();
    let mut invalid = cfg.clone();
    invalid.a2a_peers.peers[0].endpoint_url = "http://127.0.0.1:1234".into();
    assert!(
        invalid.validate().is_err(),
        "production config must reject even loopback HTTP peers"
    );
    let app = build_app_router(&cfg, None);
    let upstream = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let _http = RunningServer(tokio::spawn(async move {
        axum::serve(upstream, app).await.unwrap();
    }));
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
    let _tls = RunningServer(tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                incoming = tls_listener.accept() => {
                    let Ok((socket, _)) = incoming else { break };
                    let acceptor = acceptor.clone();
                    let events = events_tx.clone();
                    connections.spawn(async move {
                        let _ = tokio::time::timeout(Duration::from_secs(15), async move {
                            let connection = acceptor.accept(socket).await;
                            let _ = events.send(connection.is_ok());
                            let Ok(mut tls) = connection else { return };
                            let mut target = tokio::net::TcpStream::connect(upstream_address).await.unwrap();
                            let _ = tokio::io::copy_bidirectional(&mut tls, &mut target).await;
                        }).await;
                    });
                }
                _ = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    }));

    // The default root store must reject this test-only certificate before HTTP.
    assert!(
        reqwest::Client::new()
            .post(format!("{endpoint}/a2a/deliver"))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .is_err()
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), events_rx.recv())
            .await
            .unwrap(),
        Some(false)
    );
    let evidence_dir = root.path().join(".lean-ctx/handoffs/evidence");
    assert!(!evidence_dir.exists());

    let table = RelayPeerTableV1 {
        schema_version: 1,
        peers: vec![peer("recipient", endpoint.clone())],
    };
    table.validate(false).unwrap();
    let transport = RemoteTransport::for_peer_table(table, "origin", Duration::from_secs(5), false)
        .unwrap()
        .with_test_tls_root(reqwest::Certificate::from_der(&ca).unwrap());
    let mut bundle = EvidenceBundle::new("tls-task".into());
    bundle.finalize();
    let payload = serde_json::to_string(&bundle).unwrap();
    let now = chrono::Utc::now();
    let record = RelayRecordV1::new_signed(
        "tls-delivery",
        "origin",
        "recipient",
        "tenant",
        "project",
        TransportContentType::EvidenceBundle,
        DataClassification::Internal,
        now + chrono::Duration::minutes(1),
        4,
        now,
        &test_origin_key("origin"),
        "tls-test-hop-channel",
        payload.as_bytes(),
    )
    .unwrap();
    let mut envelope = TransportEnvelopeV1::new(
        AgentIdentityV1::from_current("origin", "test"),
        Some("recipient"),
        TransportContentType::EvidenceBundle,
        payload.clone(),
    );
    envelope
        .metadata
        .insert("tenant_id".into(), "tenant".into());
    envelope
        .metadata
        .insert("project_id".into(), "project".into());
    envelope.attach_relay_record(&record).unwrap();
    for _ in 0..2 {
        let result = transport.deliver_to("recipient", &envelope).await;
        if let Err(error) = &result {
            let mut probe_envelope = envelope.clone();
            probe_envelope.sign(b"tls-test-hop-channel").unwrap();
            let probe = reqwest::Client::builder()
                .add_root_certificate(reqwest::Certificate::from_der(&ca).unwrap())
                .build()
                .unwrap()
                .post(format!("{endpoint}/a2a/deliver"))
                .header(crate::core::a2a::relay::RELAY_PEER_HEADER, "origin")
                .bearer_auth("tls-test-hop-bearer")
                .json(&probe_envelope)
                .timeout(Duration::from_secs(5))
                .send()
                .await;
            let detail = match probe {
                Ok(response) => format!(
                    "HTTP {}: {}",
                    response.status(),
                    response.text().await.unwrap()
                ),
                Err(failure) => format!("{failure:?}"),
            };
            panic!("peer delivery failed: {error:?}; diagnostic: {detail}");
        }
        assert_eq!(result.unwrap().remote_status, 200);
    }
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), events_rx.recv())
            .await
            .unwrap(),
        Some(true)
    );
    let files = std::fs::read_dir(evidence_dir)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(std::fs::read_to_string(files[0].path()).unwrap(), payload);
}
