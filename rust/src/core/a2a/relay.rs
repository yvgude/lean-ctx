use base64::Engine;
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use hmac::{Hmac, KeyInit, Mac};
use lean_ctx_protocol::DataClassification;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashSet;
use std::net::IpAddr;

use super::super::a2a_transport::TransportContentType;

pub const RELAY_RECORD_VERSION: u8 = 1;
pub const RELAY_PEER_TABLE_VERSION: u8 = 1;
pub const RELAY_METADATA_KEY: &str = "leanctx.relay.v1";
pub const RELAY_PEER_HEADER: &str = "x-leanctx-peer-id";
pub const RELAY_MAX_HOPS: usize = 16;
pub const RELAY_MAX_PEERS: usize = 32;
pub const RELAY_MAX_RECORD_BYTES: usize = 32 * 1024;
pub const RELAY_MAX_PAYLOAD_BYTES: usize = 2 * 1024 * 1024;
pub const RELAY_MAX_DEADLINE_SECS: i64 = 24 * 60 * 60;
pub const RELAY_MAX_RETRIES: u8 = 5;
pub const RELAY_MAX_RETRY_DELAY_MS: u64 = 30_000;
pub const RELAY_MAX_ID_BYTES: usize = 128;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RelayHop {
    pub agent_id: String,
    pub received_at: DateTime<Utc>,
    pub forwarded_at: Option<DateTime<Utc>>,
    pub processing_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RelayChain {
    pub hops: Vec<RelayHop>,
    pub max_hops: usize,
}

impl RelayChain {
    pub fn new(max_hops: usize) -> Self {
        Self {
            hops: Vec::new(),
            max_hops,
        }
    }

    pub fn add_hop(&mut self, hop: RelayHop) -> Result<(), RelayError> {
        if self.max_hops == 0 || self.max_hops > RELAY_MAX_HOPS || self.hops.len() >= self.max_hops
        {
            return Err(RelayError::MaxHopsExceeded(self.max_hops));
        }
        if hop.agent_id.is_empty() || hop.agent_id.len() > RELAY_MAX_ID_BYTES {
            return Err(RelayError::Malformed("relay hop identity".into()));
        }
        if self.contains_agent(&hop.agent_id) {
            return Err(RelayError::CycleDetected(hop.agent_id));
        }
        if hop
            .forwarded_at
            .is_some_and(|forwarded| forwarded < hop.received_at)
        {
            return Err(RelayError::Malformed("relay hop timestamps".into()));
        }
        self.hops.push(hop);
        Ok(())
    }

    pub fn add_peer_hop(
        &mut self,
        agent_id: impl Into<String>,
        now: DateTime<Utc>,
    ) -> Result<(), RelayError> {
        self.add_hop(RelayHop {
            agent_id: agent_id.into(),
            received_at: now,
            forwarded_at: Some(now),
            processing_ms: Some(0),
        })
    }

    pub fn depth(&self) -> usize {
        self.hops.len()
    }

    pub fn total_latency_ms(&self) -> u64 {
        self.hops
            .iter()
            .filter_map(|hop| hop.processing_ms)
            .fold(0, u64::saturating_add)
    }

    pub fn contains_agent(&self, agent_id: &str) -> bool {
        self.hops.iter().any(|hop| hop.agent_id == agent_id)
    }

    pub fn origin(&self) -> Option<&str> {
        self.hops.first().map(|hop| hop.agent_id.as_str())
    }

    pub fn validate(&self) -> Result<(), RelayError> {
        if self.max_hops == 0
            || self.max_hops > RELAY_MAX_HOPS
            || self.hops.is_empty()
            || self.hops.len() > self.max_hops
        {
            return Err(RelayError::MaxHopsExceeded(self.max_hops));
        }
        let mut identities = HashSet::new();
        for hop in &self.hops {
            if hop.agent_id.is_empty() || hop.agent_id.len() > RELAY_MAX_ID_BYTES {
                return Err(RelayError::Malformed("relay hop identity".into()));
            }
            if hop
                .forwarded_at
                .is_some_and(|forwarded| forwarded < hop.received_at)
                || !identities.insert(&hop.agent_id)
            {
                return Err(RelayError::CycleDetected(hop.agent_id.clone()));
            }
        }
        Ok(())
    }
}

impl Default for RelayChain {
    fn default() -> Self {
        Self::new(5)
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct RelayOriginView<'a> {
    schema_version: u8,
    payload_sha256: &'a str,
    delivery_id: &'a str,
    origin: &'a str,
    final_recipient: &'a str,
    tenant_id: &'a str,
    project_id: &'a str,
    content_type: &'a TransportContentType,
    classification: &'a DataClassification,
    expires_at: DateTime<Utc>,
    max_hops: usize,
    origin_hop: &'a RelayHop,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RelayRecordV1 {
    pub schema_version: u8,
    pub payload_sha256: String,
    pub delivery_id: String,
    pub origin: String,
    pub final_recipient: String,
    pub current_peer: String,
    pub next_peer: String,
    pub tenant_id: String,
    pub project_id: String,
    pub content_type: TransportContentType,
    pub classification: DataClassification,
    pub expires_at: DateTime<Utc>,
    pub relay_chain: RelayChain,
    pub origin_signature: String,
    pub hop_signature: String,
}

impl RelayRecordV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new_signed(
        delivery_id: impl Into<String>,
        origin: impl Into<String>,
        final_recipient: impl Into<String>,
        tenant_id: impl Into<String>,
        project_id: impl Into<String>,
        content_type: TransportContentType,
        classification: DataClassification,
        expires_at: DateTime<Utc>,
        max_hops: usize,
        now: DateTime<Utc>,
        origin_key: &SigningKey,
        channel_key: &str,
        payload: &[u8],
    ) -> Result<Self, RelayError> {
        let origin = origin.into();
        let mut relay_chain = RelayChain::new(max_hops);
        relay_chain.add_peer_hop(origin.clone(), now)?;
        let mut record = Self {
            schema_version: RELAY_RECORD_VERSION,
            payload_sha256: Self::payload_digest(payload),
            delivery_id: delivery_id.into(),
            origin: origin.clone(),
            final_recipient: final_recipient.into(),
            current_peer: origin,
            next_peer: String::new(),
            tenant_id: tenant_id.into(),
            project_id: project_id.into(),
            content_type,
            classification,
            expires_at,
            relay_chain,
            origin_signature: String::new(),
            hop_signature: String::new(),
        };
        record.next_peer = record.final_recipient.clone();
        record.validate_shape()?;
        record.validate_deadline(now)?;
        record.origin_signature = record.sign_origin(origin_key)?;
        record.hop_signature = record.sign_hop(channel_key)?;
        Ok(record)
    }

    pub fn validate_shape(&self) -> Result<(), RelayError> {
        if self.payload_sha256.len() != 64
            || !self
                .payload_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(RelayError::Malformed("payload digest".into()));
        }
        if self.schema_version != RELAY_RECORD_VERSION {
            return Err(RelayError::Malformed("relay record version".into()));
        }
        for (name, value) in [
            ("delivery_id", self.delivery_id.as_str()),
            ("origin", self.origin.as_str()),
            ("final_recipient", self.final_recipient.as_str()),
            ("current_peer", self.current_peer.as_str()),
            ("next_peer", self.next_peer.as_str()),
            ("tenant_id", self.tenant_id.as_str()),
            ("project_id", self.project_id.as_str()),
        ] {
            if value.is_empty() || value.len() > RELAY_MAX_ID_BYTES {
                return Err(RelayError::Malformed(name.into()));
            }
        }
        if self.origin_signature.len() > RELAY_MAX_ID_BYTES * 2
            || self.hop_signature.len() > RELAY_MAX_ID_BYTES * 2
        {
            return Err(RelayError::Malformed("relay signature".into()));
        }
        self.relay_chain.validate()?;
        if self
            .relay_chain
            .hops
            .last()
            .map(|hop| hop.agent_id.as_str())
            != Some(self.current_peer.as_str())
            || self.relay_chain.origin() != Some(self.origin.as_str())
        {
            return Err(RelayError::Malformed("relay chain identity".into()));
        }
        Ok(())
    }

    fn validate_deadline(&self, now: DateTime<Utc>) -> Result<(), RelayError> {
        if self.expires_at <= now {
            return Err(RelayError::DeadlineExceeded);
        }
        if self.expires_at.signed_duration_since(now).num_seconds() > RELAY_MAX_DEADLINE_SECS {
            return Err(RelayError::Malformed("relay deadline exceeds cap".into()));
        }
        Ok(())
    }

    pub fn validate_at(
        &self,
        now: DateTime<Utc>,
        expected_current: &str,
        expected_next: Option<&str>,
        max_hops: usize,
    ) -> Result<(), RelayError> {
        self.validate_shape()?;
        self.validate_deadline(now)?;
        if self.current_peer != expected_current
            || expected_next
                .map(|peer| self.next_peer != peer)
                .unwrap_or(false)
        {
            return Err(RelayError::RouteMismatch);
        }
        if self.relay_chain.max_hops > max_hops || self.relay_chain.depth() > max_hops {
            return Err(RelayError::MaxHopsExceeded(max_hops));
        }
        if self.origin_signature.is_empty() || self.hop_signature.is_empty() {
            return Err(RelayError::InvalidSignature);
        }
        Ok(())
    }

    fn origin_view(&self) -> Result<RelayOriginView<'_>, RelayError> {
        Ok(RelayOriginView {
            schema_version: self.schema_version,
            payload_sha256: &self.payload_sha256,
            delivery_id: &self.delivery_id,
            origin: &self.origin,
            final_recipient: &self.final_recipient,
            tenant_id: &self.tenant_id,
            project_id: &self.project_id,
            content_type: &self.content_type,
            classification: &self.classification,
            expires_at: self.expires_at,
            max_hops: self.relay_chain.max_hops,
            origin_hop: self
                .relay_chain
                .hops
                .first()
                .ok_or_else(|| RelayError::Malformed("relay origin hop".into()))?,
        })
    }

    fn origin_bytes(&self) -> Result<Vec<u8>, RelayError> {
        serde_json::to_vec(&("leanctx.relay.origin.v1", self.origin_view()?))
            .map_err(|e| RelayError::Malformed(e.to_string()))
    }

    pub(crate) fn origin_fingerprint(&self) -> Result<String, RelayError> {
        Ok(Self::payload_digest(&self.origin_bytes()?))
    }

    fn payload_digest(payload: &[u8]) -> String {
        use sha2::Digest;
        hex::encode(Sha256::digest(payload))
    }

    pub fn verify_payload(&self, payload: &[u8]) -> Result<(), RelayError> {
        if self.payload_sha256 != Self::payload_digest(payload) {
            return Err(RelayError::InvalidSignature);
        }
        Ok(())
    }

    fn hop_bytes(&self) -> Result<Vec<u8>, RelayError> {
        let mut unsigned = self.clone();
        unsigned.hop_signature.clear();
        serde_json::to_vec(&("leanctx.relay.hop.v1", unsigned))
            .map_err(|e| RelayError::Malformed(e.to_string()))
    }

    fn sign(key: &str, bytes: &[u8]) -> Result<String, RelayError> {
        if key.is_empty() {
            return Err(RelayError::InvalidCredential);
        }
        let mut mac = HmacSha256::new_from_slice(key.as_bytes())
            .map_err(|_| RelayError::InvalidCredential)?;
        mac.update(bytes);
        Ok(hex::encode(mac.finalize().into_bytes()))
    }

    fn sign_origin(&self, key: &SigningKey) -> Result<String, RelayError> {
        Ok(hex::encode(key.sign(&self.origin_bytes()?).to_bytes()))
    }

    fn sign_hop(&self, key: &str) -> Result<String, RelayError> {
        Self::sign(key, &self.hop_bytes()?)
    }

    pub fn verify_origin(&self, key: &str) -> Result<(), RelayError> {
        let key = decode_origin_key(key)?;
        if self.origin_signature.len() != 128 {
            return Err(RelayError::InvalidSignature);
        }
        let bytes =
            hex::decode(&self.origin_signature).map_err(|_| RelayError::InvalidSignature)?;
        let signature = Signature::from_slice(&bytes).map_err(|_| RelayError::InvalidSignature)?;
        key.verify_strict(&self.origin_bytes()?, &signature)
            .map_err(|_| RelayError::InvalidSignature)
    }

    pub fn verify_hop(&self, key: &str) -> Result<(), RelayError> {
        verify_signature(&self.hop_signature, &self.sign_hop(key)?)
    }

    /// Select the first transport hop without changing the signed final recipient.
    pub fn route_from_origin(
        &mut self,
        next_peer: impl Into<String>,
        now: DateTime<Utc>,
        origin_public_key: &str,
        channel_key: &str,
    ) -> Result<(), RelayError> {
        self.validate_at(now, &self.origin, None, self.relay_chain.max_hops)?;
        self.verify_origin(origin_public_key)?;
        if self.relay_chain.depth() != 1 {
            return Err(RelayError::RouteMismatch);
        }
        let mut routed = self.clone();
        routed.next_peer = next_peer.into();
        if routed.relay_chain.contains_agent(&routed.next_peer) {
            return Err(RelayError::CycleDetected(routed.next_peer));
        }
        routed.validate_shape()?;
        routed.hop_signature = routed.sign_hop(channel_key)?;
        *self = routed;
        Ok(())
    }

    pub fn forward_from(
        &mut self,
        local_peer: impl Into<String>,
        next_peer: impl Into<String>,
        now: DateTime<Utc>,
        hop_key: &str,
    ) -> Result<(), RelayError> {
        self.validate_at(
            now,
            &self.current_peer.clone(),
            None,
            self.relay_chain.max_hops,
        )?;
        let local_peer = local_peer.into();
        if local_peer != self.next_peer {
            return Err(RelayError::RouteMismatch);
        }
        let mut forwarded = self.clone();
        forwarded
            .relay_chain
            .add_peer_hop(local_peer.clone(), now)?;
        forwarded.current_peer = local_peer;
        forwarded.next_peer = next_peer.into();
        if forwarded.relay_chain.contains_agent(&forwarded.next_peer) {
            return Err(RelayError::CycleDetected(forwarded.next_peer));
        }
        forwarded.validate_shape()?;
        forwarded.hop_signature = forwarded.sign_hop(hop_key)?;
        *self = forwarded;
        Ok(())
    }

    pub fn encode_metadata(&self) -> Result<String, RelayError> {
        self.validate_shape()?;
        let bytes = serde_json::to_vec(self).map_err(|e| RelayError::Malformed(e.to_string()))?;
        if bytes.len() > RELAY_MAX_RECORD_BYTES {
            return Err(RelayError::Oversize);
        }
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
    }

    pub fn decode_metadata(value: &str) -> Result<Self, RelayError> {
        if value.is_empty() || value.len() > RELAY_MAX_RECORD_BYTES * 2 {
            return Err(RelayError::Oversize);
        }
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(value)
            .map_err(|_| RelayError::Malformed("relay metadata encoding".into()))?;
        if bytes.len() > RELAY_MAX_RECORD_BYTES {
            return Err(RelayError::Oversize);
        }
        let record: Self = serde_json::from_slice(&bytes)
            .map_err(|e| RelayError::Malformed(format!("relay metadata: {e}")))?;
        record.validate_shape()?;
        if record.encode_metadata()? != value {
            return Err(RelayError::Malformed("non-canonical relay metadata".into()));
        }
        Ok(record)
    }
}

fn verify_signature(actual: &str, expected: &str) -> Result<(), RelayError> {
    let actual = hex::decode(actual).map_err(|_| RelayError::InvalidSignature)?;
    let expected = hex::decode(expected).map_err(|_| RelayError::InvalidSignature)?;
    if actual.len() != expected.len() || !constant_time_eq(&actual, &expected) {
        return Err(RelayError::InvalidSignature);
    }
    Ok(())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (&left, &right) in left.iter().zip(right) {
        difference |= left ^ right;
    }
    difference == 0
}

fn decode_origin_key(raw: &str) -> Result<VerifyingKey, RelayError> {
    if raw.len() != 64 {
        return Err(RelayError::InvalidCredential);
    }
    let bytes: [u8; 32] = hex::decode(raw)
        .map_err(|_| RelayError::InvalidCredential)?
        .try_into()
        .map_err(|_| RelayError::InvalidCredential)?;
    let key = VerifyingKey::from_bytes(&bytes).map_err(|_| RelayError::InvalidCredential)?;
    if key.is_weak() {
        return Err(RelayError::InvalidCredential);
    }
    Ok(key)
}

#[cfg(test)]
pub(crate) fn test_origin_key(label: &str) -> SigningKey {
    use sha2::Digest;
    // Deterministic fixture keys only; production must supply an actual seed.
    let seed: [u8; 32] = Sha256::digest(format!("test-only-relay-origin:{label}")).into();
    SigningKey::from_bytes(&seed)
}

#[cfg(test)]
pub(crate) fn test_origin_public_key(label: &str) -> String {
    hex::encode(test_origin_key(label).verifying_key().to_bytes())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RelayPeerConfigV1 {
    pub schema_version: u8,
    pub peer_id: String,
    pub endpoint_url: String,
    pub bearer_token: String,
    pub channel_key: String,
    /// Public Ed25519 key only. Intermediate peers never receive origin seeds.
    pub origin_public_key: String,
    pub recipient_id: String,
    pub allowed_tenant_ids: Vec<String>,
    pub allowed_project_ids: Vec<String>,
    pub allowed_content_types: Vec<TransportContentType>,
    pub allowed_classifications: Vec<DataClassification>,
    pub max_hops: usize,
    pub max_payload_bytes: usize,
    pub retry_count: u8,
    pub retry_delay_ms: u64,
}

impl RelayPeerConfigV1 {
    pub fn validate(&self, allow_loopback_http: bool) -> Result<(), RelayError> {
        decode_origin_key(&self.origin_public_key)?;
        if self.schema_version != RELAY_PEER_TABLE_VERSION
            || self.peer_id.is_empty()
            || self.peer_id == "legacy"
            || self.peer_id.len() > RELAY_MAX_ID_BYTES
            || self.recipient_id.is_empty()
            || self.recipient_id.len() > RELAY_MAX_ID_BYTES
            || self.bearer_token.is_empty()
            || self.channel_key.is_empty()
            || self.bearer_token == self.channel_key
        {
            return Err(RelayError::InvalidPeer(self.peer_id.clone()));
        }
        if self.allowed_tenant_ids.is_empty()
            || self.allowed_project_ids.is_empty()
            || self.allowed_content_types.is_empty()
            || self.allowed_classifications.is_empty()
        {
            return Err(RelayError::ScopeViolation);
        }
        ensure_unique(&self.allowed_tenant_ids)?;
        ensure_unique(&self.allowed_project_ids)?;
        if self.max_hops == 0 || self.max_hops > RELAY_MAX_HOPS {
            return Err(RelayError::MaxHopsExceeded(self.max_hops));
        }
        if self.max_payload_bytes == 0 || self.max_payload_bytes > RELAY_MAX_PAYLOAD_BYTES {
            return Err(RelayError::Oversize);
        }
        if self.retry_count > RELAY_MAX_RETRIES || self.retry_delay_ms > RELAY_MAX_RETRY_DELAY_MS {
            return Err(RelayError::RetryCap);
        }
        if self
            .allowed_content_types
            .iter()
            .any(|kind| matches!(kind, TransportContentType::A2AMessage))
        {
            return Err(RelayError::ScopeViolation);
        }
        let url = reqwest::Url::parse(&self.endpoint_url)
            .map_err(|_| RelayError::InvalidPeer(self.peer_id.clone()))?;
        let loopback = url.host_str().map(is_loopback_host).unwrap_or(false);
        if url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || !(url.scheme() == "https"
                || (allow_loopback_http && loopback && url.scheme() == "http"))
        {
            return Err(RelayError::InvalidPeer(self.peer_id.clone()));
        }
        Ok(())
    }

    pub fn allows(&self, record: &RelayRecordV1, payload_bytes: usize) -> bool {
        payload_bytes <= self.max_payload_bytes
            && self
                .allowed_tenant_ids
                .iter()
                .any(|id| id == &record.tenant_id)
            && self
                .allowed_project_ids
                .iter()
                .any(|id| id == &record.project_id)
            && self
                .allowed_content_types
                .iter()
                .any(|kind| kind == &record.content_type)
            && self
                .allowed_classifications
                .iter()
                .any(|kind| kind == &record.classification)
            && record.relay_chain.depth() <= self.max_hops
    }
}

fn ensure_unique(values: &[String]) -> Result<(), RelayError> {
    let mut seen = HashSet::new();
    if values
        .iter()
        .any(|value| value.is_empty() || !seen.insert(value))
    {
        return Err(RelayError::ScopeViolation);
    }
    Ok(())
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RelayPeerTableV1 {
    pub schema_version: u8,
    pub peers: Vec<RelayPeerConfigV1>,
}

impl Default for RelayPeerTableV1 {
    fn default() -> Self {
        Self {
            schema_version: RELAY_PEER_TABLE_VERSION,
            peers: Vec::new(),
        }
    }
}

impl RelayPeerTableV1 {
    pub fn validate(&self, allow_loopback_http: bool) -> Result<(), RelayError> {
        if self.schema_version != RELAY_PEER_TABLE_VERSION || self.peers.len() > RELAY_MAX_PEERS {
            return Err(RelayError::InvalidPeer("peer table".into()));
        }
        let mut ids = HashSet::new();
        for peer in &self.peers {
            peer.validate(allow_loopback_http)?;
            if !ids.insert(&peer.peer_id) {
                return Err(RelayError::InvalidPeer(peer.peer_id.clone()));
            }
        }
        Ok(())
    }

    pub fn validate_for_tests(&self) -> Result<(), RelayError> {
        self.validate(true)
    }

    pub fn from_json(value: &str, allow_loopback_http: bool) -> Result<Self, RelayError> {
        let table: Self = serde_json::from_str(value)
            .map_err(|e| RelayError::Malformed(format!("peer table: {e}")))?;
        table.validate(allow_loopback_http)?;
        Ok(table)
    }

    pub fn peer(&self, peer_id: &str) -> Option<&RelayPeerConfigV1> {
        self.peers.iter().find(|peer| peer.peer_id == peer_id)
    }

    pub fn route_for_recipient(
        &self,
        recipient_id: &str,
    ) -> Result<&RelayPeerConfigV1, RelayError> {
        let mut routes = self
            .peers
            .iter()
            .filter(|peer| peer.recipient_id == recipient_id);
        let first = routes
            .next()
            .ok_or_else(|| RelayError::UnknownRoute(recipient_id.into()))?;
        if routes.next().is_some() {
            return Err(RelayError::AmbiguousRoute(recipient_id.into()));
        }
        Ok(first)
    }

    pub fn reject_shared_secret(&self) -> Result<(), RelayError> {
        for (index, left) in self.peers.iter().enumerate() {
            for right in self.peers.iter().skip(index + 1) {
                if left.channel_key == right.channel_key || left.bearer_token == right.bearer_token
                {
                    return Err(RelayError::InvalidCredential);
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, thiserror::Error)]
pub enum RelayError {
    #[error("relay chain exceeded maximum of {0} hops")]
    MaxHopsExceeded(usize),
    #[error("relay cycle detected at agent {0}")]
    CycleDetected(String),
    #[error("malformed relay data: {0}")]
    Malformed(String),
    #[error("relay data exceeds its cap")]
    Oversize,
    #[error("relay deadline expired")]
    DeadlineExceeded,
    #[error("relay route mismatch")]
    RouteMismatch,
    #[error("relay scope or classification is not allowed")]
    ScopeViolation,
    #[error("unknown relay route: {0}")]
    UnknownRoute(String),
    #[error("ambiguous relay route: {0}")]
    AmbiguousRoute(String),
    #[error("invalid relay peer: {0}")]
    InvalidPeer(String),
    #[error("invalid relay signature")]
    InvalidSignature,
    #[error("invalid relay credential")]
    InvalidCredential,
    #[error("relay retry cap exceeded")]
    RetryCap,
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone, Utc};
    use lean_ctx_protocol::DataClassification;

    use super::{
        RelayChain, RelayError, RelayHop, RelayRecordV1, test_origin_key, test_origin_public_key,
    };
    use crate::core::a2a_transport::TransportContentType;

    fn hop(agent_id: &str, processing_ms: Option<u64>) -> RelayHop {
        RelayHop {
            agent_id: agent_id.to_owned(),
            received_at: Utc.timestamp_millis_opt(1_700_000_000_000).unwrap(),
            forwarded_at: None,
            processing_ms,
        }
    }

    #[test]
    fn unforwarded_hop_is_valid_but_time_reversal_is_rejected() {
        let mut pending = hop("pending", None);
        pending.forwarded_at = None;
        let mut chain = RelayChain::default();
        chain.add_hop(pending.clone()).unwrap();
        chain.validate().unwrap();

        pending.agent_id = "backwards".into();
        pending.forwarded_at = Some(pending.received_at - Duration::seconds(1));
        assert!(chain.add_hop(pending.clone()).is_err());
        chain.hops.push(pending);
        assert!(chain.validate().is_err());
    }

    #[test]
    fn relay_peer_cannot_select_the_legacy_retry_namespace() {
        let mut peer = super::RelayPeerConfigV1 {
            schema_version: 1,
            peer_id: "peer".into(),
            endpoint_url: "https://peer.example".into(),
            bearer_token: "bearer".into(),
            channel_key: "channel-key".into(),
            origin_public_key: test_origin_public_key("peer"),
            recipient_id: "recipient".into(),
            allowed_tenant_ids: vec!["tenant".into()],
            allowed_project_ids: vec!["project".into()],
            allowed_content_types: vec![TransportContentType::EvidenceBundle],
            allowed_classifications: vec![DataClassification::Internal],
            max_hops: 4,
            max_payload_bytes: 1024,
            retry_count: 0,
            retry_delay_ms: 0,
        };
        peer.validate(false).unwrap();
        peer.peer_id = "legacy".into();
        assert_eq!(
            peer.validate(false),
            Err(RelayError::InvalidPeer("legacy".into()))
        );
    }

    #[test]
    fn adds_hop_and_reports_chain_metadata() {
        let mut chain = RelayChain::new(3);
        chain.add_hop(hop("origin-agent", Some(4))).unwrap();
        assert_eq!(chain.depth(), 1);
        assert!(chain.contains_agent("origin-agent"));
        assert_eq!(chain.origin(), Some("origin-agent"));
    }

    #[test]
    fn rejects_cycle() {
        let mut chain = RelayChain::new(3);
        chain.add_hop(hop("relay-agent", None)).unwrap();
        let error = chain.add_hop(hop("relay-agent", Some(1))).unwrap_err();
        assert_eq!(error, RelayError::CycleDetected("relay-agent".to_owned()));
    }

    #[test]
    fn rejects_hop_beyond_maximum() {
        let mut chain = RelayChain::new(1);
        chain.add_hop(hop("origin-agent", None)).unwrap();
        assert_eq!(
            chain.add_hop(hop("next-agent", None)),
            Err(RelayError::MaxHopsExceeded(1))
        );
    }

    #[test]
    fn sums_available_processing_latency() {
        let mut chain = RelayChain::new(3);
        chain.add_hop(hop("origin-agent", Some(12))).unwrap();
        chain.add_hop(hop("relay-agent", None)).unwrap();
        chain.add_hop(hop("recipient-agent", Some(8))).unwrap();
        assert_eq!(chain.total_latency_ms(), 20);
    }

    #[test]
    fn default_chain_allows_five_hops() {
        let mut chain = RelayChain::default();
        for index in 0..5 {
            chain
                .add_hop(hop(&format!("agent-{index}"), Some(u64::MAX)))
                .unwrap();
        }
        assert_eq!(chain.depth(), 5);
        assert_eq!(chain.total_latency_ms(), u64::MAX);
        assert_eq!(
            chain.add_hop(hop("sixth-agent", None)),
            Err(RelayError::MaxHopsExceeded(5))
        );
    }

    #[test]
    fn signed_record_has_canonical_reserved_wire_value_and_forwarding_changes_route_only() {
        let now = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let mut record = RelayRecordV1::new_signed(
            "delivery-1",
            "origin",
            "recipient",
            "tenant",
            "project",
            TransportContentType::A2ATask,
            DataClassification::Internal,
            now + Duration::hours(1),
            4,
            now,
            &test_origin_key("origin"),
            "origin-key",
            b"task-payload",
        )
        .unwrap();
        record
            .route_from_origin(
                "relay-a",
                now,
                &test_origin_public_key("origin"),
                "inbound-key",
            )
            .unwrap();
        assert!(
            record
                .verify_origin(&test_origin_public_key("origin"))
                .is_ok()
        );
        assert!(record.verify_hop("inbound-key").is_ok());
        let unchanged = record.encode_metadata().unwrap();
        assert_eq!(
            record.forward_from("unaddressed-peer", "recipient", now, "relay-key"),
            Err(RelayError::RouteMismatch)
        );
        assert_eq!(record.encode_metadata().unwrap(), unchanged);
        for invalid_next in ["", "origin", "relay-a"] {
            assert!(
                record
                    .forward_from("relay-a", invalid_next, now, "relay-key")
                    .is_err()
            );
            assert_eq!(record.encode_metadata().unwrap(), unchanged);
        }
        let encoded = record.encode_metadata().unwrap();
        let mut forwarded = RelayRecordV1::decode_metadata(&encoded).unwrap();
        forwarded
            .forward_from(
                "relay-a",
                "recipient",
                now + Duration::seconds(1),
                "relay-key",
            )
            .unwrap();
        assert_eq!(forwarded.delivery_id, "delivery-1");
        assert!(
            forwarded
                .verify_origin(&test_origin_public_key("origin"))
                .is_ok()
        );
        assert!(forwarded.verify_payload(b"task-payload").is_ok());
        assert!(forwarded.verify_payload(b"substituted-payload").is_err());
        let mut substituted = forwarded.clone();
        substituted.payload_sha256 = RelayRecordV1::payload_digest(b"substituted-payload");
        assert!(
            substituted
                .verify_origin(&test_origin_public_key("origin"))
                .is_err()
        );
        // An authenticated forwarding peer can replace its hop MAC, but cannot
        // impersonate the origin using its own independent signing key.
        substituted.origin_signature = substituted
            .sign_origin(&test_origin_key("relay-a"))
            .unwrap();
        substituted.hop_signature = substituted.sign_hop("relay-key").unwrap();
        assert!(substituted.verify_hop("relay-key").is_ok());
        assert!(substituted.verify_payload(b"substituted-payload").is_ok());
        assert_eq!(
            substituted.verify_origin(&test_origin_public_key("origin")),
            Err(RelayError::InvalidSignature)
        );
        assert!(forwarded.verify_hop("relay-key").is_ok());
        assert!(RelayRecordV1::decode_metadata(&format!("{encoded} ")).is_err());
    }

    #[test]
    fn relay_origin_public_keys_reject_wrong_length_non_hex_and_weak_points() {
        for invalid in [
            String::new(),
            "a".repeat(62),
            "a".repeat(66),
            "z".repeat(64),
            "0".repeat(64),
        ] {
            assert!(super::decode_origin_key(&invalid).is_err());
        }
        assert!(super::decode_origin_key(&test_origin_public_key("origin")).is_ok());
    }
}
