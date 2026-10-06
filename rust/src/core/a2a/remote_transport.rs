use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::core::a2a::dlq::{DeadLetter, DeadLetterDelivery, DlqScope, MAX_ERROR_BYTES};
use crate::core::a2a::relay::{
    RELAY_MAX_PAYLOAD_BYTES, RELAY_MAX_RETRIES, RELAY_MAX_RETRY_DELAY_MS, RELAY_PEER_HEADER,
    RelayPeerConfigV1, RelayPeerTableV1,
};
use crate::core::a2a_transport::TransportEnvelopeV1;

const DEFAULT_MAX_PAYLOAD_BYTES: usize = 2_000_000;
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
const MAX_TASK_RESPONSE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteTransportConfig {
    pub endpoint_url: String,
    pub timeout: Duration,
    pub max_payload_bytes: usize,
    pub auth_token: Option<String>,
    pub signing_key: Option<String>,
    pub recipient_id: Option<String>,
    pub tenant_id: Option<String>,
    pub project_id: Option<String>,
    pub retry_count: u8,
    pub retry_delay: Duration,
    #[serde(default)]
    pub peer_id: Option<String>,
    #[serde(default)]
    pub local_peer_id: Option<String>,
    #[serde(default)]
    pub peers: Vec<RelayPeerConfigV1>,
    #[serde(skip)]
    pub allow_loopback_http: bool,
}

impl Default for RemoteTransportConfig {
    fn default() -> Self {
        Self {
            endpoint_url: String::new(),
            timeout: Duration::from_secs(30),
            max_payload_bytes: DEFAULT_MAX_PAYLOAD_BYTES,
            auth_token: None,
            signing_key: None,
            recipient_id: None,
            tenant_id: None,
            project_id: None,
            retry_count: 2,
            retry_delay: Duration::from_secs(1),
            peer_id: None,
            local_peer_id: None,
            peers: Vec::new(),
            allow_loopback_http: false,
        }
    }
}

impl RemoteTransportConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.timeout.is_zero() || self.max_payload_bytes == 0 {
            return Err("transport bounds must be greater than zero".into());
        }
        if self.retry_count > RELAY_MAX_RETRIES
            || self.retry_delay.as_millis() > RELAY_MAX_RETRY_DELAY_MS as u128
        {
            return Err("retry policy exceeds bounded relay caps".into());
        }
        // Presence of a local identity selects peer mode; partial or mixed
        // authority must fail closed, never fall back to singleton credentials.
        if self.local_peer_id.is_some() || !self.peers.is_empty() {
            return self.validate_peer_mode();
        }
        if self.peer_id.is_some() {
            return Err("singleton transport must not contain a peer_id".into());
        }
        let url = reqwest::Url::parse(&self.endpoint_url)
            .map_err(|error| format!("invalid endpoint_url: {error}"))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err("endpoint_url must be an absolute HTTP(S) URL".to_string());
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err("endpoint_url must not contain credentials".to_string());
        }
        if url.scheme() == "http" && !is_loopback_host(&url) {
            return Err("endpoint_url must use HTTPS except for loopback hosts".to_string());
        }
        if self.auth_token.as_ref().is_none_or(String::is_empty) {
            return Err("auth_token is required for remote A2A delivery".to_string());
        }
        if self.signing_key.as_ref().is_none_or(String::is_empty) {
            return Err("signing_key is required for remote A2A delivery".to_string());
        }
        if self.auth_token == self.signing_key {
            return Err("signing_key must be distinct from auth_token".to_string());
        }
        if self.recipient_id.as_ref().is_none_or(String::is_empty) {
            return Err("recipient_id is required for remote A2A delivery".to_string());
        }
        if self.tenant_id.as_ref().is_none_or(String::is_empty) {
            return Err("tenant_id is required for remote A2A delivery".to_string());
        }
        if self.project_id.as_ref().is_none_or(String::is_empty) {
            return Err("project_id is required for remote A2A delivery".to_string());
        }
        Ok(())
    }

    fn validate_peer_mode(&self) -> Result<(), String> {
        if self.local_peer_id.as_deref().is_none_or(|id| {
            id.is_empty() || id.len() > crate::core::a2a::relay::RELAY_MAX_ID_BYTES
        }) || self.peers.is_empty()
            || self.max_payload_bytes > RELAY_MAX_PAYLOAD_BYTES
        {
            return Err(
                "peer transport requires a bounded local identity, peer table and payload limit"
                    .into(),
            );
        }
        if !self.endpoint_url.is_empty()
            || [
                &self.auth_token,
                &self.signing_key,
                &self.recipient_id,
                &self.tenant_id,
                &self.project_id,
                &self.peer_id,
            ]
            .iter()
            .any(|value| value.is_some())
            || self.retry_count != 0
            || !self.retry_delay.is_zero()
        {
            return Err(
                "peer transport must not contain singleton authority or retry policy".into(),
            );
        }
        let table = RelayPeerTableV1 {
            schema_version: crate::core::a2a::relay::RELAY_PEER_TABLE_VERSION,
            peers: self.peers.clone(),
        };
        table
            .validate(self.allow_loopback_http)
            .map_err(|error| error.to_string())?;
        table
            .reject_shared_secret()
            .map_err(|error| error.to_string())
    }

    fn delivery_url(&self) -> Result<reqwest::Url, String> {
        self.validate()?;
        let mut url = reqwest::Url::parse(&self.endpoint_url)
            .map_err(|error| format!("invalid endpoint_url: {error}"))?;
        let path = format!("{}/a2a/deliver", url.path().trim_end_matches('/'));
        url.set_path(&path);
        url.set_query(None);
        url.set_fragment(None);
        Ok(url)
    }
}

fn is_loopback_host(url: &reqwest::Url) -> bool {
    url.host_str().is_some_and(|host| {
        let ip_host = host
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
            .unwrap_or(host);
        host.eq_ignore_ascii_case("localhost")
            || ip_host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteTransport {
    config: RemoteTransportConfig,
    #[serde(skip, default = "fixed_peer_client")]
    client: reqwest::Client,
}

fn fixed_peer_client() -> reqwest::Client {
    // A configured peer cannot delegate delivery authority with an HTTP redirect.
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("initialize fixed-peer HTTP client")
}

impl RemoteTransport {
    #[cfg(test)]
    pub(crate) fn with_test_tls_root(mut self, certificate: reqwest::Certificate) -> Self {
        self.client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .add_root_certificate(certificate)
            .build()
            .expect("test TLS client");
        self
    }

    pub fn new(config: RemoteTransportConfig) -> Result<Self, String> {
        config.validate()?;
        Ok(Self {
            config,
            client: fixed_peer_client(),
        })
    }

    pub fn for_peer_table(
        table: RelayPeerTableV1,
        local_peer_id: &str,
        timeout: Duration,
        allow_loopback_http: bool,
    ) -> Result<Self, String> {
        table
            .validate(allow_loopback_http)
            .map_err(|error| error.to_string())?;
        Self::new(RemoteTransportConfig {
            timeout,
            max_payload_bytes: RELAY_MAX_PAYLOAD_BYTES,
            retry_count: 0,
            retry_delay: Duration::ZERO,
            local_peer_id: Some(local_peer_id.to_string()),
            peers: table.peers,
            allow_loopback_http,
            ..RemoteTransportConfig::default()
        })
    }

    pub async fn deliver(
        &self,
        envelope: &TransportEnvelopeV1,
    ) -> Result<DeliveryReceipt, TransportError> {
        self.config
            .validate()
            .map_err(TransportError::SerializationError)?;
        if self.config.local_peer_id.is_some() {
            return Err(TransportError::SerializationError(
                "peer transport requires explicit peer delivery".into(),
            ));
        }
        self.ensure_dlq_available().await?;
        let mut signed = envelope.clone();
        let expected_recipient = self
            .config
            .recipient_id
            .as_deref()
            .expect("validated transport has recipient identity");
        if signed.recipient.as_deref() != Some(expected_recipient) {
            return Err(TransportError::SerializationError(
                "envelope recipient does not match configured remote recipient".to_string(),
            ));
        }
        signed.metadata.insert(
            "tenant_id".to_string(),
            self.config
                .tenant_id
                .clone()
                .expect("validated transport has tenant scope"),
        );
        signed.metadata.insert(
            "project_id".to_string(),
            self.config
                .project_id
                .clone()
                .expect("validated transport has project scope"),
        );
        let secret = self
            .config
            .signing_key
            .as_deref()
            .ok_or_else(|| TransportError::SerializationError("missing signing key".into()))?;
        signed
            .sign(secret.as_bytes())
            .map_err(TransportError::SerializationError)?;
        let body = serialize_and_validate(&signed, self.config.max_payload_bytes)?;
        let envelope_id = envelope_id(&body);
        let delivery_url = self
            .config
            .delivery_url()
            .map_err(TransportError::SerializationError)?;
        let started_at = Instant::now();

        for attempt in 0..=self.config.retry_count {
            let mut request = self
                .client
                .post(delivery_url.clone())
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .timeout(self.config.timeout)
                .body(body.clone());
            if let Some(token) = self.config.auth_token.as_deref() {
                request = request.bearer_auth(token);
            }

            match request.send().await {
                Ok(response) if response.status().is_success() => {
                    let remote_status = response.status().as_u16();
                    let unverified_task_response = match read_task_response(response, &signed).await
                    {
                        Ok(body) => body,
                        // A 2xx may mean the remote effect committed even when its
                        // response stream is unusable. Do not enqueue a false
                        // non-delivery record; the caller must reconcile/retry by ID.
                        Err(error) => return Err(error),
                    };
                    return Ok(DeliveryReceipt {
                        envelope_id,
                        delivered_at: Utc::now(),
                        remote_status,
                        round_trip_ms: elapsed_millis(started_at),
                        unverified_task_response,
                    });
                }
                Ok(response) if response.status().is_server_error() => {
                    if attempt == self.config.retry_count {
                        return Err(self
                            .record_failure(
                                &signed,
                                &body,
                                TransportError::Exhausted(self.config.retry_count),
                                attempt.saturating_add(1),
                            )
                            .await);
                    }
                }
                Ok(response) => {
                    let status = response.status();
                    let error_body = read_error_body(response).await;
                    let error = TransportError::RemoteError(status.as_u16(), error_body);
                    return Err(self
                        .record_failure(&signed, &body, error, attempt.saturating_add(1))
                        .await);
                }
                Err(error) if error.is_timeout() && self.config.retry_count == 0 => {
                    return Err(self
                        .record_failure(
                            &signed,
                            &body,
                            TransportError::Timeout,
                            attempt.saturating_add(1),
                        )
                        .await);
                }
                Err(_) if attempt == self.config.retry_count => {
                    return Err(self
                        .record_failure(
                            &signed,
                            &body,
                            TransportError::Exhausted(self.config.retry_count),
                            attempt.saturating_add(1),
                        )
                        .await);
                }
                Err(_) => {}
            }

            tokio::time::sleep(self.config.retry_delay).await;
        }

        Err(TransportError::Exhausted(self.config.retry_count))
    }

    /// Deliver through one explicitly selected peer. The peer table is the
    /// authority for route, scope, credential, and retry bounds; singleton
    /// transport configuration is never consulted for this path.
    pub async fn deliver_to(
        &self,
        peer_id: &str,
        envelope: &TransportEnvelopeV1,
    ) -> Result<DeliveryReceipt, TransportError> {
        self.config
            .validate()
            .map_err(TransportError::SerializationError)?;
        if self.config.local_peer_id.is_none() {
            return Err(TransportError::SerializationError(
                "explicit peer delivery requires peer transport".into(),
            ));
        }
        let peer = self
            .config
            .peers
            .iter()
            .find(|peer| peer.peer_id == peer_id)
            .cloned()
            .ok_or_else(|| {
                TransportError::SerializationError(format!("unknown relay peer: {peer_id}"))
            })?;
        let mut signed = envelope.clone();
        let record = signed
            .relay_record()
            .map_err(TransportError::SerializationError)?
            .ok_or_else(|| {
                TransportError::SerializationError(
                    "relay record is required for peer delivery".into(),
                )
            })?;
        let now = Utc::now();
        record
            .validate_at(
                now,
                &record.current_peer.clone(),
                Some(peer.peer_id.as_str()),
                peer.max_hops,
            )
            .map_err(|error| TransportError::SerializationError(error.to_string()))?;
        record
            .verify_hop(&peer.channel_key)
            .map_err(|error| TransportError::SerializationError(error.to_string()))?;
        if !peer.allows(&record, signed.payload_json.len())
            || signed.content_type != record.content_type
            || signed.sender.agent_id != record.current_peer
            || self.config.local_peer_id.as_deref() != Some(record.current_peer.as_str())
            || signed.metadata.get("tenant_id").map(String::as_str)
                != Some(record.tenant_id.as_str())
            || signed.metadata.get("project_id").map(String::as_str)
                != Some(record.project_id.as_str())
            || record
                .verify_payload(signed.payload_json.as_bytes())
                .is_err()
        {
            return Err(TransportError::SerializationError(
                "peer policy denied relay scope".into(),
            ));
        }
        if signed.recipient.as_deref() != Some(record.final_recipient.as_str()) {
            return Err(TransportError::SerializationError(
                "envelope recipient does not match signed final recipient".into(),
            ));
        }
        signed
            .attach_relay_record(&record)
            .map_err(TransportError::SerializationError)?;
        signed.signature = None;
        signed
            .sign(peer.channel_key.as_bytes())
            .map_err(TransportError::SerializationError)?;
        let body = serialize_and_validate(&signed, peer.max_payload_bytes)?;
        let delivery_id = record.delivery_id.clone();
        let delivery_url = peer_delivery_url(&peer).map_err(TransportError::SerializationError)?;
        self.ensure_dlq_scope(&record.tenant_id, &record.project_id)
            .await?;
        let started_at = Instant::now();
        let source_peer = self
            .config
            .local_peer_id
            .as_deref()
            .unwrap_or(record.current_peer.as_str());

        for attempt in 0..=peer.retry_count {
            let request = self
                .client
                .post(delivery_url.clone())
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .header("idempotency-key", delivery_id.as_str())
                .header(RELAY_PEER_HEADER, source_peer)
                .timeout(self.config.timeout)
                .bearer_auth(peer.bearer_token.as_str())
                .body(body.clone());
            match request.send().await {
                Ok(response) if response.status().is_success() => {
                    let remote_status = response.status().as_u16();
                    let unverified_task_response = match read_task_response(response, &signed).await
                    {
                        Ok(body) => body,
                        // Delivery was acknowledged before response decoding.
                        // Recording it as a permanent non-delivery would mislead
                        // operators and could authorize an unsafe blind replay.
                        Err(error) => return Err(error),
                    };
                    return Ok(DeliveryReceipt {
                        envelope_id: delivery_id,
                        delivered_at: Utc::now(),
                        remote_status,
                        round_trip_ms: elapsed_millis(started_at),
                        unverified_task_response,
                    });
                }
                Ok(response) if response.status().is_server_error() => {
                    if attempt == peer.retry_count {
                        let failure = TransportError::Exhausted(peer.retry_count);
                        return Err(self
                            .record_failure_for_peer(
                                &signed,
                                &body,
                                &failure,
                                attempt.saturating_add(1),
                                &peer,
                            )
                            .await);
                    }
                }
                Ok(response) => {
                    let status = response.status();
                    let error = TransportError::RemoteError(
                        status.as_u16(),
                        read_error_body(response).await,
                    );
                    return Err(self
                        .record_failure_for_peer(
                            &signed,
                            &body,
                            &error,
                            attempt.saturating_add(1),
                            &peer,
                        )
                        .await);
                }
                Err(error) if error.is_timeout() && peer.retry_count == 0 => {
                    return Err(self
                        .record_failure_for_peer(
                            &signed,
                            &body,
                            &TransportError::Timeout,
                            attempt.saturating_add(1),
                            &peer,
                        )
                        .await);
                }
                Err(_) if attempt == peer.retry_count => {
                    let failure = TransportError::Exhausted(peer.retry_count);
                    return Err(self
                        .record_failure_for_peer(
                            &signed,
                            &body,
                            &failure,
                            attempt.saturating_add(1),
                            &peer,
                        )
                        .await);
                }
                Err(_) => {}
            }
            tokio::time::sleep(Duration::from_millis(peer.retry_delay_ms)).await;
        }
        Err(TransportError::Exhausted(peer.retry_count))
    }

    async fn record_failure(
        &self,
        envelope: &TransportEnvelopeV1,
        body: &[u8],
        failure: TransportError,
        attempts: u8,
    ) -> TransportError {
        let envelope = envelope.clone();
        let body = body.to_vec();
        let config = self.config.clone();
        let failure_for_store = failure.clone();
        let stored = tokio::task::spawn_blocking(move || {
            enqueue_permanent_failure(&envelope, &body, &failure_for_store, attempts, &config)
        })
        .await;
        match stored {
            Ok(Ok(())) => failure,
            Ok(Err(error)) => TransportError::DeadLetterFailure(format!("{failure}; {error}")),
            Err(error) => TransportError::DeadLetterFailure(format!(
                "{failure}; dead-letter worker failed: {error}"
            )),
        }
    }

    async fn ensure_dlq_available(&self) -> Result<(), TransportError> {
        let scope = DlqScope::new(
            self.config
                .tenant_id
                .clone()
                .expect("validated tenant scope"),
            self.config
                .project_id
                .clone()
                .expect("validated project scope"),
        )
        .map_err(|error| TransportError::DeadLetterFailure(error.to_string()))?;
        self.ensure_dlq_scope_inner(scope).await
    }

    async fn ensure_dlq_scope(
        &self,
        tenant_id: &str,
        project_id: &str,
    ) -> Result<(), TransportError> {
        let scope = DlqScope::new(tenant_id, project_id)
            .map_err(|error| TransportError::DeadLetterFailure(error.to_string()))?;
        self.ensure_dlq_scope_inner(scope).await
    }

    async fn ensure_dlq_scope_inner(&self, scope: DlqScope) -> Result<(), TransportError> {
        let queue = crate::core::ocla::health::dead_letter_queue().clone();
        tokio::task::spawn_blocking(move || queue.ensure_writable(&scope))
            .await
            .map_err(|error| {
                TransportError::DeadLetterFailure(format!("dead-letter worker failed: {error}"))
            })?
            .map_err(|error| TransportError::DeadLetterFailure(error.to_string()))
    }

    async fn record_failure_for_peer(
        &self,
        envelope: &TransportEnvelopeV1,
        body: &[u8],
        failure: &TransportError,
        attempts: u8,
        peer: &RelayPeerConfigV1,
    ) -> TransportError {
        let envelope = envelope.clone();
        let body = body.to_vec();
        let config = self.config.clone();
        let failure_for_store = failure.clone();
        let peer_id = peer.peer_id.clone();
        let endpoint_url = peer.endpoint_url.clone();
        let stored = tokio::task::spawn_blocking(move || {
            enqueue_permanent_failure_for_peer(
                &envelope,
                &body,
                &failure_for_store,
                attempts,
                &config,
                &peer_id,
                &endpoint_url,
            )
        })
        .await;
        match stored {
            Ok(Ok(())) => failure.clone(),
            Ok(Err(error)) => TransportError::DeadLetterFailure(format!("{failure}; {error}")),
            Err(error) => TransportError::DeadLetterFailure(format!(
                "{failure}; dead-letter worker failed: {error}"
            )),
        }
    }

    pub async fn retry_dead_letter(
        &self,
        letter: &DeadLetter,
    ) -> Result<DeliveryReceipt, TransportError> {
        if letter.peer_id != "legacy" {
            let envelope: TransportEnvelopeV1 = serde_json::from_str(&letter.original_message)
                .map_err(|error| TransportError::SerializationError(error.to_string()))?;
            let record = envelope
                .relay_record()
                .map_err(TransportError::SerializationError)?
                .ok_or_else(|| {
                    TransportError::SerializationError(
                        "peer dead letter is missing relay record".into(),
                    )
                })?;
            if record.delivery_id != letter.delivery_id {
                return Err(TransportError::SerializationError(
                    "dead letter delivery ID does not match relay record".into(),
                ));
            }
            if record.tenant_id != letter.tenant_id || record.project_id != letter.project_id {
                return Err(TransportError::SerializationError(
                    "dead letter scope does not match relay record".into(),
                ));
            }
            let peer = self
                .config
                .peers
                .iter()
                .find(|peer| peer.peer_id == letter.peer_id)
                .ok_or_else(|| {
                    TransportError::SerializationError("dead letter peer is not configured".into())
                })?;
            if &peer.endpoint_url
                != match &letter.delivery {
                    DeadLetterDelivery::RemoteHttp { endpoint_url } => endpoint_url,
                    DeadLetterDelivery::LocalAgentBus => {
                        return Err(TransportError::SerializationError(
                            "dead letter is not remote".into(),
                        ));
                    }
                }
            {
                return Err(TransportError::SerializationError(
                    "dead letter endpoint does not match peer".into(),
                ));
            }
            let mut envelope = envelope;
            envelope.sent_at = Utc::now();
            envelope.signature = None;
            return self.deliver_to(&letter.peer_id, &envelope).await;
        }
        let expected_scope = (
            self.config.tenant_id.as_deref(),
            self.config.project_id.as_deref(),
        );
        if expected_scope
            != (
                Some(letter.tenant_id.as_str()),
                Some(letter.project_id.as_str()),
            )
        {
            return Err(TransportError::SerializationError(
                "dead letter scope does not match configured remote transport".into(),
            ));
        }
        let DeadLetterDelivery::RemoteHttp { endpoint_url } = &letter.delivery else {
            return Err(TransportError::SerializationError(
                "dead letter is not a remote delivery".into(),
            ));
        };
        if endpoint_url != &self.config.endpoint_url {
            return Err(TransportError::SerializationError(
                "dead letter endpoint does not match configured remote transport".into(),
            ));
        }
        let mut envelope: TransportEnvelopeV1 = serde_json::from_str(&letter.original_message)
            .map_err(|error| TransportError::SerializationError(error.to_string()))?;
        if !envelope
            .metadata
            .contains_key(crate::core::a2a_transport::LEGACY_DELIVERY_ID_METADATA)
            && let Some(signature) = envelope.signature.as_deref()
        {
            envelope.metadata.insert(
                crate::core::a2a_transport::LEGACY_DELIVERY_ID_METADATA.to_string(),
                signature.to_ascii_lowercase(),
            );
            envelope.metadata.insert(
                crate::core::a2a_transport::LEGACY_DELIVERY_SENT_AT_METADATA.to_string(),
                envelope.sent_at.to_rfc3339(),
            );
        }
        envelope.sent_at = Utc::now();
        envelope.signature = None;
        self.deliver(&envelope).await
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeliveryReceipt {
    pub envelope_id: String,
    pub delivered_at: DateTime<Utc>,
    pub remote_status: u16,
    pub round_trip_ms: u64,
    /// Bounded, complete UTF-8 task response. HTTP success is NOT evidence of
    /// task authority: callers must verify its signature and request binding.
    /// Absent for non-task deliveries and old serialized receipts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unverified_task_response: Option<String>,
}

impl DeliveryReceipt {
    /// Return only a status authenticated against the caller's trusted peer
    /// policy and exact control request. Receipt metadata is not authority.
    /// Re-check current time and revocations even for a deserialized receipt.
    pub fn verified_task_status(
        &self,
        request: &super::task::TaskControlDescriptorV1,
        policy: &super::task::TaskAuthorityConfigV1,
        now: DateTime<Utc>,
    ) -> Result<super::task::TaskStatusV1, super::task_response::InvalidTaskResponse> {
        use super::task_response::{InvalidTaskResponse, SignedTaskStatusV1};

        if !(200..300).contains(&self.remote_status) {
            return Err(InvalidTaskResponse);
        }
        let raw = self
            .unverified_task_response
            .as_deref()
            .ok_or(InvalidTaskResponse)?;
        let response = SignedTaskStatusV1::from_json(raw)?;
        response.verify(request, policy, now)?;
        Ok(response.status)
    }
}

async fn read_task_response(
    response: reqwest::Response,
    envelope: &TransportEnvelopeV1,
) -> Result<Option<String>, TransportError> {
    if envelope.content_type != crate::core::a2a_transport::TransportContentType::A2ATask {
        return Ok(None);
    }
    let invalid =
        || TransportError::SerializationError("invalid or oversized task response".into());
    if response
        .content_length()
        .is_some_and(|length| length > MAX_TASK_RESPONSE_BYTES as u64)
    {
        return Err(invalid());
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| invalid())?;
        if chunk.len() > MAX_TASK_RESPONSE_BYTES.saturating_sub(bytes.len()) {
            return Err(invalid());
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        return Err(invalid());
    }
    String::from_utf8(bytes).map(Some).map_err(|_| invalid())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    #[error("payload too large: {0} bytes")]
    PayloadTooLarge(usize),
    #[error("transport timed out")]
    Timeout,
    #[error("remote returned HTTP {0}: {1}")]
    RemoteError(u16, String),
    #[error("serialization failed: {0}")]
    SerializationError(String),
    #[error("delivery exhausted after {0} retries")]
    Exhausted(u8),
    #[error("delivery failed and could not be persisted to DLQ: {0}")]
    DeadLetterFailure(String),
}

fn serialize_and_validate(
    envelope: &TransportEnvelopeV1,
    max_payload_bytes: usize,
) -> Result<Vec<u8>, TransportError> {
    let body = serde_json::to_vec(envelope)
        .map_err(|error| TransportError::SerializationError(error.to_string()))?;
    if body.len() > max_payload_bytes {
        return Err(TransportError::PayloadTooLarge(body.len()));
    }
    Ok(body)
}

fn peer_delivery_url(peer: &RelayPeerConfigV1) -> Result<reqwest::Url, String> {
    let mut url = reqwest::Url::parse(&peer.endpoint_url)
        .map_err(|error| format!("invalid peer endpoint_url: {error}"))?;
    let path = format!("{}/a2a/deliver", url.path().trim_end_matches('/'));
    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

fn envelope_id(body: &[u8]) -> String {
    format!("envelope:{}", blake3::hash(body).to_hex())
}

fn elapsed_millis(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
}

async fn read_error_body(response: reqwest::Response) -> String {
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            break;
        };
        let remaining = MAX_ERROR_BODY_BYTES.saturating_sub(body.len());
        body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        if body.len() == MAX_ERROR_BODY_BYTES {
            break;
        }
    }
    String::from_utf8_lossy(&body).into_owned()
}

fn enqueue_permanent_failure(
    envelope: &TransportEnvelopeV1,
    body: &[u8],
    failure: &TransportError,
    attempts: u8,
    config: &RemoteTransportConfig,
) -> crate::core::ocla::types::OclaResult<()> {
    let peer_id = config.peer_id.as_deref().unwrap_or("legacy");
    enqueue_permanent_failure_for_peer(
        envelope,
        body,
        failure,
        attempts,
        config,
        peer_id,
        &config.endpoint_url,
    )
}

fn enqueue_permanent_failure_for_peer(
    envelope: &TransportEnvelopeV1,
    body: &[u8],
    failure: &TransportError,
    attempts: u8,
    config: &RemoteTransportConfig,
    peer_id: &str,
    endpoint_url: &str,
) -> crate::core::ocla::types::OclaResult<()> {
    let failed_at = Utc::now().to_rfc3339();
    let target_agent = envelope.recipient.as_deref().unwrap_or(endpoint_url);
    let mut error = failure.to_string();
    if error.len() > MAX_ERROR_BYTES {
        let mut boundary = MAX_ERROR_BYTES;
        while !error.is_char_boundary(boundary) {
            boundary -= 1;
        }
        error.truncate(boundary);
    }
    let relay_record = envelope.relay_record().ok().flatten();
    let delivery_id = relay_record
        .as_ref()
        .map(|record| record.delivery_id.clone())
        .or_else(|| envelope.stable_delivery_id().ok())
        .unwrap_or_else(|| envelope_id(body));
    let tenant_id = relay_record
        .as_ref()
        .map(|record| record.tenant_id.clone())
        .or_else(|| config.tenant_id.clone())
        .expect("validated tenant scope");
    let project_id = relay_record
        .as_ref()
        .map(|record| record.project_id.clone())
        .or_else(|| config.project_id.clone())
        .expect("validated project scope");
    crate::core::ocla::health::dead_letter_queue().enqueue(DeadLetter {
        id: envelope_id(body),
        peer_id: peer_id.to_string(),
        delivery_id,
        tenant_id,
        project_id,
        delivery: DeadLetterDelivery::RemoteHttp {
            endpoint_url: endpoint_url.to_string(),
        },
        original_message: String::from_utf8_lossy(body).into_owned(),
        target_agent: target_agent.to_string(),
        error,
        attempts,
        first_failed_at: failed_at.clone(),
        last_failed_at: failed_at,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use axum::{Router, http::StatusCode, routing::post};

    use super::*;

    #[tokio::test]
    async fn relay_peer_mode_has_no_singleton_authority_or_fallback() {
        let peer = RelayPeerConfigV1 {
            schema_version: 1,
            peer_id: "destination".into(),
            endpoint_url: "https://destination.example".into(),
            bearer_token: "destination-bearer".into(),
            channel_key: "destination-channel".into(),
            origin_public_key: crate::core::a2a::relay::test_origin_public_key("destination"),
            recipient_id: "recipient".into(),
            allowed_tenant_ids: vec!["tenant".into()],
            allowed_project_ids: vec!["project".into()],
            allowed_content_types: vec![
                crate::core::a2a_transport::TransportContentType::EvidenceBundle,
            ],
            allowed_classifications: vec![lean_ctx_protocol::DataClassification::Internal],
            max_hops: 4,
            max_payload_bytes: 1024,
            retry_count: 0,
            retry_delay_ms: 0,
        };
        let transport = RemoteTransport::for_peer_table(
            RelayPeerTableV1 {
                schema_version: 1,
                peers: vec![peer],
            },
            "origin",
            Duration::from_secs(5),
            false,
        )
        .unwrap();
        assert!(transport.config.endpoint_url.is_empty());
        assert!(transport.config.auth_token.is_none());
        assert!(transport.config.signing_key.is_none());
        assert!(transport.config.recipient_id.is_none());
        assert!(transport.config.tenant_id.is_none());
        assert!(transport.config.project_id.is_none());
        let restored: RemoteTransport =
            serde_json::from_str(&serde_json::to_string(&transport).unwrap()).unwrap();
        restored.config.validate().unwrap();
        for current in [&transport, &restored] {
            assert!(matches!(current.deliver(&envelope("payload")).await,
                Err(TransportError::SerializationError(message))
                    if message == "peer transport requires explicit peer delivery"));
        }
        let mut mixed = transport.config.clone();
        mixed.auth_token = Some("legacy-bearer".into());
        assert!(RemoteTransport::new(mixed).is_err());
        let mut incomplete = transport.config.clone();
        incomplete.local_peer_id = None;
        assert!(RemoteTransport::new(incomplete).is_err());
        let mut empty = transport.config.clone();
        empty.peers.clear();
        assert!(RemoteTransport::new(empty).is_err());
        let mut malformed = restored;
        malformed.config.timeout = Duration::ZERO;
        assert!(matches!(
            malformed
                .deliver_to("destination", &envelope("payload"))
                .await,
            Err(TransportError::SerializationError(_))
        ));
    }
    use crate::core::a2a_transport::{AgentIdentityV1, TransportContentType};

    #[tokio::test]
    async fn relay_clients_do_not_follow_redirects_after_deserialization() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let hits = Arc::new(AtomicUsize::new(0));
        let received = hits.clone();
        let target_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_url = format!("http://{}/sink", target_listener.local_addr().unwrap());
        let target = tokio::spawn(async move {
            axum::serve(
                target_listener,
                Router::new().route(
                    "/sink",
                    post(move || {
                        let received = received.clone();
                        async move {
                            received.fetch_add(1, Ordering::SeqCst);
                            StatusCode::OK
                        }
                    }),
                ),
            )
            .await
            .unwrap();
        });
        for status in [
            StatusCode::TEMPORARY_REDIRECT,
            StatusCode::PERMANENT_REDIRECT,
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let location = target_url.clone();
            let source = tokio::spawn(async move {
                axum::serve(
                    listener,
                    Router::new().route(
                        "/a2a/deliver",
                        post(move || {
                            let location = location.clone();
                            async move { (status, [(axum::http::header::LOCATION, location)]) }
                        }),
                    ),
                )
                .await
                .unwrap();
            });
            let original = RemoteTransport::new(RemoteTransportConfig {
                endpoint_url: endpoint.clone(),
                auth_token: Some("bearer".into()),
                signing_key: Some("signing-key".into()),
                recipient_id: Some("recipient".into()),
                tenant_id: Some("tenant".into()),
                project_id: Some("project".into()),
                ..RemoteTransportConfig::default()
            })
            .unwrap();
            let restored: RemoteTransport =
                serde_json::from_str(&serde_json::to_string(&original).unwrap()).unwrap();
            for transport in [original, restored] {
                let response = transport
                    .client
                    .post(format!("{endpoint}/a2a/deliver"))
                    .timeout(Duration::from_secs(5))
                    .body("private context")
                    .send()
                    .await
                    .unwrap();
                assert_eq!(response.status(), status);
                assert_eq!(hits.load(Ordering::SeqCst), 0);
            }
            source.abort();
            let _ = source.await;
        }
        target.abort();
        let _ = target.await;
    }

    fn envelope(payload: &str) -> TransportEnvelopeV1 {
        TransportEnvelopeV1 {
            format_version: 1,
            sent_at: Utc::now(),
            sender: AgentIdentityV1 {
                agent_id: "sender".to_string(),
                agent_type: "test".to_string(),
                daemon_fingerprint: "fingerprint".to_string(),
                capabilities: Vec::new(),
            },
            recipient: Some("recipient".to_string()),
            content_type: TransportContentType::A2AMessage,
            payload_json: payload.to_string(),
            signature: None,
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn default_config_has_bounded_transport_values() {
        let config = RemoteTransportConfig {
            endpoint_url: "https://agent.example/api/".to_string(),
            auth_token: Some("secret".to_string()),
            signing_key: Some("signing-secret".to_string()),
            recipient_id: Some("recipient".to_string()),
            tenant_id: Some("tenant-a".to_string()),
            project_id: Some("project-a".to_string()),
            ..RemoteTransportConfig::default()
        };

        assert!(config.validate().is_ok());
        assert_eq!(config.timeout, Duration::from_secs(30));
        assert_eq!(config.max_payload_bytes, 2_000_000);
        assert_eq!(config.retry_count, 2);
        assert_eq!(config.retry_delay, Duration::from_secs(1));
        assert_eq!(
            config.delivery_url().expect("valid URL").as_str(),
            "https://agent.example/api/a2a/deliver"
        );
        for peer_id in ["p", "legacy", ""] {
            let mut mixed = config.clone();
            mixed.peer_id = Some(peer_id.into());
            let restored: RemoteTransportConfig =
                serde_json::from_str(&serde_json::to_string(&mixed).unwrap()).unwrap();
            assert!(restored.validate().unwrap_err().contains("peer_id"));
            assert!(RemoteTransport::new(restored).is_err());
        }
    }

    #[tokio::test]
    async fn task_response_is_complete_bounded_utf8_and_explicitly_unverified() {
        let mut task = envelope("{}");
        task.content_type = TransportContentType::A2ATask;
        for size in [1, MAX_TASK_RESPONSE_BYTES] {
            let body = "x".repeat(size);
            let response = reqwest::Response::from(axum::http::Response::new(body.clone()));
            assert_eq!(
                read_task_response(response, &task).await.unwrap(),
                Some(body)
            );
        }
        for body in [
            Vec::new(),
            vec![b'x'; MAX_TASK_RESPONSE_BYTES + 1],
            vec![0xff],
        ] {
            let response = reqwest::Response::from(axum::http::Response::new(body));
            assert!(read_task_response(response, &task).await.is_err());
        }
        let response = reqwest::Response::from(axum::http::Response::new(vec![0xff]));
        assert_eq!(
            read_task_response(response, &envelope("{}")).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn interrupted_task_response_is_not_accepted_as_complete() {
        let mut task = envelope("{}");
        task.content_type = TransportContentType::A2ATask;
        let chunks = futures::stream::iter([
            Ok::<_, std::io::Error>(vec![b'{']),
            Err(std::io::Error::other("interrupted response")),
        ]);
        let response = reqwest::Response::from(axum::http::Response::new(
            reqwest::Body::wrap_stream(chunks),
        ));
        assert!(read_task_response(response, &task).await.is_err());
    }

    #[test]
    fn config_validation_rejects_unbounded_or_unsupported_values() {
        let zero_timeout = RemoteTransportConfig {
            endpoint_url: "https://agent.example".to_string(),
            timeout: Duration::ZERO,
            auth_token: Some("secret".to_string()),
            signing_key: Some("signing-secret".to_string()),
            ..RemoteTransportConfig::default()
        };
        let unsupported_scheme = RemoteTransportConfig {
            endpoint_url: "file:///tmp/agent".to_string(),
            auth_token: Some("secret".to_string()),
            signing_key: Some("signing-secret".to_string()),
            ..RemoteTransportConfig::default()
        };

        assert!(zero_timeout.validate().is_err());
        assert!(unsupported_scheme.validate().is_err());
        let insecure_remote = RemoteTransportConfig {
            endpoint_url: "http://agent.example".to_string(),
            auth_token: Some("bearer-secret".to_string()),
            signing_key: Some("signing-secret".to_string()),
            recipient_id: Some("recipient".to_string()),
            tenant_id: Some("tenant-a".to_string()),
            project_id: Some("project-a".to_string()),
            ..RemoteTransportConfig::default()
        };
        assert_eq!(
            insecure_remote.validate(),
            Err("endpoint_url must use HTTPS except for loopback hosts".to_string())
        );
        assert!(
            RemoteTransportConfig {
                endpoint_url: "http://[::1]:8080".to_string(),
                ..insecure_remote
            }
            .validate()
            .is_ok()
        );
        assert!(
            RemoteTransportConfig {
                endpoint_url: "https://user:secret@agent.example".to_string(),
                auth_token: Some("secret".to_string()),
                signing_key: Some("signing-secret".to_string()),
                recipient_id: Some("recipient".to_string()),
                tenant_id: Some("tenant-a".to_string()),
                project_id: Some("project-a".to_string()),
                ..RemoteTransportConfig::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            RemoteTransportConfig {
                endpoint_url: "https://agent.example".to_string(),
                ..RemoteTransportConfig::default()
            }
            .validate()
            .is_err()
        );
        assert_eq!(
            RemoteTransportConfig {
                endpoint_url: "https://agent.example".to_string(),
                auth_token: Some("same-secret".to_string()),
                signing_key: Some("same-secret".to_string()),
                ..RemoteTransportConfig::default()
            }
            .validate(),
            Err("signing_key must be distinct from auth_token".to_string())
        );
    }

    #[tokio::test]
    async fn delivery_rejects_recipient_outside_configured_authority() {
        let transport = RemoteTransport::new(RemoteTransportConfig {
            endpoint_url: "http://127.0.0.1:9".to_string(),
            auth_token: Some("bearer-secret".to_string()),
            signing_key: Some("signing-secret".to_string()),
            recipient_id: Some("different-recipient".to_string()),
            tenant_id: Some("tenant-a".to_string()),
            project_id: Some("project-a".to_string()),
            retry_count: 0,
            ..RemoteTransportConfig::default()
        })
        .expect("valid transport config");

        assert_eq!(
            transport.deliver(&envelope("payload")).await,
            Err(TransportError::SerializationError(
                "envelope recipient does not match configured remote recipient".to_string()
            ))
        );
    }

    #[tokio::test]
    async fn remote_retry_refreshes_timestamp_and_signature_for_authority_receiver() {
        let signing_key = "retry-signing-secret";
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/a2a/deliver",
            post(move |body: axum::body::Bytes| async move {
                let Ok(envelope) = serde_json::from_slice::<TransportEnvelopeV1>(&body) else {
                    return StatusCode::BAD_REQUEST;
                };
                let age = Utc::now().signed_duration_since(envelope.sent_at);
                if age > chrono::Duration::seconds(300)
                    || age < chrono::Duration::seconds(-30)
                    || !envelope.verify_signature(signing_key.as_bytes())
                {
                    StatusCode::UNAUTHORIZED
                } else {
                    StatusCode::OK
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let transport = RemoteTransport::new(RemoteTransportConfig {
            endpoint_url: format!("http://{address}"),
            auth_token: Some("bearer-secret".into()),
            signing_key: Some(signing_key.into()),
            recipient_id: Some("recipient".into()),
            tenant_id: Some("tenant-a".into()),
            project_id: Some("project-a".into()),
            retry_count: 0,
            ..RemoteTransportConfig::default()
        })
        .unwrap();
        let mut stale = envelope("payload");
        stale.sent_at = Utc::now() - chrono::Duration::minutes(10);
        stale.sign(signing_key.as_bytes()).unwrap();
        let letter = DeadLetter {
            id: "stale-letter".into(),
            peer_id: "legacy".into(),
            delivery_id: stale.stable_delivery_id().unwrap(),
            tenant_id: "tenant-a".into(),
            project_id: "project-a".into(),
            delivery: DeadLetterDelivery::RemoteHttp {
                endpoint_url: format!("http://{address}"),
            },
            original_message: serde_json::to_string(&stale).unwrap(),
            target_agent: "recipient".into(),
            error: "previous failure".into(),
            attempts: 1,
            first_failed_at: Utc::now().to_rfc3339(),
            last_failed_at: Utc::now().to_rfc3339(),
        };

        assert_eq!(
            transport
                .retry_dead_letter(&letter)
                .await
                .unwrap()
                .remote_status,
            StatusCode::OK.as_u16()
        );
        server.abort();
    }

    #[tokio::test]
    async fn permanent_remote_failure_is_persisted_with_exact_scope() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/a2a/deliver",
            post(|| async { (StatusCode::BAD_REQUEST, "scope denied") }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let suffix = uuid::Uuid::new_v4().to_string();
        let tenant_id = format!("tenant-{suffix}");
        let project_id = format!("project-{suffix}");
        let scope = crate::core::a2a::dlq::DlqScope::new(&tenant_id, &project_id).unwrap();
        let transport = RemoteTransport::new(RemoteTransportConfig {
            endpoint_url: format!("http://{address}"),
            auth_token: Some("bearer-secret".into()),
            signing_key: Some("signing-secret".into()),
            recipient_id: Some("recipient".into()),
            tenant_id: Some(tenant_id),
            project_id: Some(project_id),
            retry_count: 0,
            ..RemoteTransportConfig::default()
        })
        .unwrap();

        assert!(matches!(
            transport.deliver(&envelope("payload")).await,
            Err(TransportError::RemoteError(400, _))
        ));
        let entries = crate::core::ocla::health::dead_letter_queue()
            .peek(&scope)
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].target_agent, "recipient");
        assert!(matches!(
            entries[0].delivery,
            DeadLetterDelivery::RemoteHttp { .. }
        ));
        server.abort();
    }

    #[test]
    fn payload_size_limit_reports_serialized_size() {
        let envelope = envelope("payload");
        let serialized = serde_json::to_vec(&envelope).expect("serializable envelope");
        let error = serialize_and_validate(&envelope, serialized.len() - 1)
            .expect_err("payload must exceed configured limit");

        assert_eq!(error, TransportError::PayloadTooLarge(serialized.len()));
    }

    #[test]
    fn transport_error_variants_are_serializable_and_distinct() {
        let variants = [
            TransportError::PayloadTooLarge(10),
            TransportError::Timeout,
            TransportError::RemoteError(400, "bad request".to_string()),
            TransportError::SerializationError("invalid JSON".to_string()),
            TransportError::Exhausted(2),
            TransportError::DeadLetterFailure("store unavailable".to_string()),
        ];

        for variant in variants {
            let json = serde_json::to_string(&variant).expect("serialize error");
            let decoded = serde_json::from_str(&json).expect("deserialize error");
            assert_eq!(variant, decoded);
        }
    }

    #[test]
    fn receipt_round_trips_without_losing_delivery_fields() {
        let receipt = DeliveryReceipt {
            envelope_id: "envelope:abc".to_string(),
            delivered_at: Utc::now(),
            remote_status: 202,
            round_trip_ms: 17,
            unverified_task_response: None,
        };

        let json = serde_json::to_string(&receipt).expect("serialize receipt");
        let decoded: DeliveryReceipt = serde_json::from_str(&json).expect("deserialize receipt");
        assert_eq!(decoded, receipt);
    }

    #[test]
    fn envelope_ids_are_deterministic_and_content_addressed() {
        let first = serialize_and_validate(&envelope("one"), usize::MAX).expect("serialize");
        let first_again = first.clone();
        let second = serialize_and_validate(&envelope("two"), usize::MAX).expect("serialize");

        assert_eq!(envelope_id(&first), envelope_id(&first_again));
        assert_ne!(envelope_id(&first), envelope_id(&second));
    }
}
