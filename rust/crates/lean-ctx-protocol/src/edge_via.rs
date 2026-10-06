// SPDX-License-Identifier: Apache-2.0

//! LeanCTX Edge↔Via protocol V1 core contracts.
//!
//! These metadata-first types intentionally contain no provider request,
//! header, cookie, credential, or arbitrary JSON-patch field. Edge validates
//! and executes typed plans; Via never receives authority to rewrite an opaque
//! provider request.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};

use chrono::DateTime;
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{Error as DeError, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::{
    AgentId, CapabilityId, EventId, PlanId, ProjectId, ProtocolReference, SemanticVersion,
    SessionId, Sha256Digest, UtcTimestamp, ValidationError, WorkspaceId,
};

pub const VIA_PROTOCOL_V1: u16 = 1;
pub const MAX_VIA_CAUSALITY: usize = 32;
pub const MAX_VIA_CAPABILITIES: usize = 64;
pub const MAX_VIA_CONTENT_ITEMS: usize = 4_096;
pub const MAX_VIA_PLAN_SOURCES: usize = 64;
pub const MAX_VIA_FRAME_BYTES: usize = 65_536;
pub const MAX_VIA_MANIFEST_BYTES: usize = 262_144;
pub const MAX_VIA_PLAN_BYTES: usize = 65_536;
pub const MAX_VIA_WIRE_BYTES: usize = 262_656;
pub const MAX_VIA_OUTPUT_TOKENS: u64 = 1_000_000;
pub const MAX_VIA_PLAN_TTL_SECONDS: u64 = 300;
pub const MAX_VIA_ACK_EVENTS: u64 = 1_024;
pub const MAX_VIA_ACK_GRAPH_NODES: u64 = 4_096;
pub const MAX_VIA_ACK_GRAPH_EDGES: u64 = 8_192;
pub const MAX_VIA_STREAM_WINDOW: u16 = 32;
pub const MAX_VIA_BACKPRESSURE_RETRY_MS: u32 = 30_000;
pub const MAX_VIA_CONTENT_CHUNKS_PER_REQUEST: usize = 4;
pub const MAX_VIA_CONTENT_CHUNK_BYTES: usize = 16 * 1_024;
pub const MAX_VIA_CONTENT_REQUEST_BYTES: u64 = 64 * 1_024;
pub const MAX_VIA_CONTENT_REQUEST_TOKENS: u64 = 16_384;
pub const MAX_VIA_CONTENT_REQUEST_TTL_SECONDS: u64 = 30;
pub const MAX_VIA_INFERENCE_SEGMENTS: usize = 128;
pub const MAX_VIA_INFERENCE_LINEAGE: usize = 8;
pub const MAX_VIA_INFERENCE_MANIFEST_BYTES: usize = 49_152;
pub const MAX_VIA_INFERENCE_CANDIDATES: usize = 64;
pub const MAX_VIA_INFERENCE_CANDIDATE_SET_BYTES: usize = 65_536;
pub const MAX_VIA_PLAN_REQUEST_BYTES: usize = 65_536;

fn deserialize_via_protocol_version<'de, D>(deserializer: D) -> Result<u16, D::Error>
where
    D: Deserializer<'de>,
{
    let version = u16::deserialize(deserializer)?;
    if version == VIA_PROTOCOL_V1 {
        Ok(version)
    } else {
        Err(DeError::custom(format!(
            "unsupported Via protocol_version {version}; expected {VIA_PROTOCOL_V1}"
        )))
    }
}

fn validate_protocol_version(version: u16) -> Result<(), ValidationError> {
    if version == VIA_PROTOCOL_V1 {
        Ok(())
    } else {
        Err(ValidationError::new(format!(
            "unsupported Via protocol_version {version}; expected {VIA_PROTOCOL_V1}"
        )))
    }
}

fn sha256(bytes: &[u8]) -> Result<Sha256Digest, ValidationError> {
    let mut value = String::with_capacity(71);
    value.push_str("sha256:");
    for byte in Sha256::digest(bytes) {
        write!(&mut value, "{byte:02x}").expect("writing to String cannot fail");
    }
    Sha256Digest::new(value)
}

fn canonical_json_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, ValidationError> {
    let value = serde_json::to_value(value)
        .map_err(|error| ValidationError::new(format!("serialize canonical JSON: {error}")))?;
    serde_json::to_vec(&canonical_json_value(value))
        .map_err(|error| ValidationError::new(format!("encode canonical JSON: {error}")))
}

fn canonical_json_value(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(fields) => {
            let sorted = fields
                .into_iter()
                .map(|(key, value)| (key, canonical_json_value(value)))
                .collect::<BTreeMap<_, _>>();
            serde_json::Value::Object(sorted.into_iter().collect())
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(canonical_json_value).collect())
        }
        scalar => scalar,
    }
}

fn validate_content_ttl(
    now: &UtcTimestamp,
    expires_at: &UtcTimestamp,
) -> Result<(), ValidationError> {
    let now = DateTime::parse_from_rfc3339(now.as_str())
        .map_err(|_| ValidationError::new("content request time is invalid"))?;
    let expires_at = DateTime::parse_from_rfc3339(expires_at.as_str())
        .map_err(|_| ValidationError::new("content request expiry is invalid"))?;
    let seconds = expires_at.signed_duration_since(now).num_seconds();
    if !(1..=MAX_VIA_CONTENT_REQUEST_TTL_SECONDS as i64).contains(&seconds) {
        return Err(ValidationError::new(
            "content request TTL must be active and at most 30 seconds",
        ));
    }
    Ok(())
}

fn validate_plan_ttl(now: &UtcTimestamp, expires_at: &UtcTimestamp) -> Result<(), ValidationError> {
    let now = DateTime::parse_from_rfc3339(now.as_str())
        .map_err(|_| ValidationError::new("plan time is invalid"))?;
    let expires_at = DateTime::parse_from_rfc3339(expires_at.as_str())
        .map_err(|_| ValidationError::new("plan expiry is invalid"))?;
    let seconds = expires_at.signed_duration_since(now).num_seconds();
    if !(1..=MAX_VIA_PLAN_TTL_SECONDS as i64).contains(&seconds) {
        return Err(ValidationError::new(
            "plan TTL must be active and at most 300 seconds",
        ));
    }
    Ok(())
}

fn validate_unique<T: Ord + Clone>(
    values: &[T],
    maximum: usize,
    field: &str,
) -> Result<(), ValidationError> {
    if values.len() > maximum {
        return Err(ValidationError::new(format!(
            "{field} exceeds the {maximum} item limit"
        )));
    }
    let unique = values.iter().cloned().collect::<BTreeSet<_>>();
    if unique.len() != values.len() {
        return Err(ValidationError::new(format!(
            "{field} must not contain duplicates"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViaClassificationV1 {
    Public,
    Normal,
    Sensitive,
    Secret,
    LocalOnly,
    EnterprisePrivate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViaScopeV1 {
    #[serde(deserialize_with = "deserialize_via_protocol_version")]
    pub protocol_version: u16,
    pub event_or_request_id: ProtocolReference,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causality_id: Option<ProtocolReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<ProtocolReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<ProtocolReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<ProtocolReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<AgentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<ProtocolReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<ProtocolReference>,
    pub classification: ViaClassificationV1,
}

impl ViaScopeV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_protocol_version(self.protocol_version)
    }

    pub fn digest(&self) -> Result<Sha256Digest, ValidationError> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)
            .map_err(|error| ValidationError::new(format!("serialize Via scope: {error}")))?;
        sha256(&bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViaCapabilityV1 {
    EventStream,
    ContentManifest,
    ContentOnDemand,
    TypedPlans,
    MultiAgentIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViaProtocolHelloV1 {
    pub minimum_version: u16,
    pub maximum_version: u16,
    pub capabilities: Vec<ViaCapabilityV1>,
}

impl ViaProtocolHelloV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.minimum_version == 0 || self.minimum_version > self.maximum_version {
            return Err(ValidationError::new(
                "ViaProtocolHelloV1 version range must be nonzero and ordered",
            ));
        }
        validate_unique(
            &self.capabilities,
            MAX_VIA_CAPABILITIES,
            "ViaProtocolHelloV1 capabilities",
        )
    }

    pub fn negotiate(&self, peer: &Self) -> Result<u16, ValidationError> {
        self.validate()?;
        peer.validate()?;
        let minimum = self.minimum_version.max(peer.minimum_version);
        let maximum = self.maximum_version.min(peer.maximum_version);
        if minimum > maximum || !(minimum..=maximum).contains(&VIA_PROTOCOL_V1) {
            return Err(ValidationError::new(
                "Via protocol version ranges have no locally supported overlap",
            ));
        }
        Ok(VIA_PROTOCOL_V1)
    }

    pub fn shared_capabilities(
        &self,
        peer: &Self,
    ) -> Result<Vec<ViaCapabilityV1>, ValidationError> {
        self.negotiate(peer)?;
        let peer_capabilities = peer.capabilities.iter().copied().collect::<BTreeSet<_>>();
        let mut shared = self
            .capabilities
            .iter()
            .copied()
            .filter(|capability| peer_capabilities.contains(capability))
            .collect::<Vec<_>>();
        shared.sort_unstable();
        Ok(shared)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopedContentIdV1 {
    pub scope_digest: Sha256Digest,
    pub content_digest: Sha256Digest,
}

impl ScopedContentIdV1 {
    /// Derive path-independent identity from exact bytes and canonical scope.
    pub fn from_scope_and_bytes(
        scope: &ViaScopeV1,
        content: &[u8],
    ) -> Result<Self, ValidationError> {
        Ok(Self {
            scope_digest: scope.digest()?,
            content_digest: sha256(content)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopedReferenceV1 {
    pub scope_digest: Sha256Digest,
    pub reference: ProtocolReference,
}

impl ScopedReferenceV1 {
    pub fn new(scope: &ViaScopeV1, reference: ProtocolReference) -> Result<Self, ValidationError> {
        Ok(Self {
            scope_digest: scope.digest()?,
            reference,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKindV1 {
    SourceFile,
    Message,
    SearchResult,
    ShellResult,
    McpResult,
    InferenceManifest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentAvailabilityV1 {
    EdgeOnly,
    TransientRemoteAllowed,
    DeniedRemote,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenCountV1 {
    pub family: CapabilityId,
    pub tokens: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceSegmentRoleV1 {
    System,
    Developer,
    User,
    Assistant,
    ToolCall,
    ToolResult,
    ToolSchema,
    RequestControl,
}

impl InferenceSegmentRoleV1 {
    const fn discriminant(self) -> u8 {
        match self {
            Self::System => 0,
            Self::Developer => 1,
            Self::User => 2,
            Self::Assistant => 3,
            Self::ToolCall => 4,
            Self::ToolResult => 5,
            Self::ToolSchema => 6,
            Self::RequestControl => 7,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceMutabilityV1 {
    Immutable,
    Conditional,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceDeliveryStateV1 {
    CacheMarked,
    Uncached,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceTokenizerFamilyV1 {
    Cl100kBaseApproxV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceCachePolicyV1 {
    AnthropicExplicitCacheControlV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InferenceSegmentV1 {
    pub segment_id: Sha256Digest,
    pub position: u16,
    pub content_or_representation_id: ScopedContentIdV1,
    pub role: InferenceSegmentRoleV1,
    pub mutability: InferenceMutabilityV1,
    pub guard_hash: Sha256Digest,
    pub token_count: u64,
    pub delivery_state: InferenceDeliveryStateV1,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_lineage: Vec<ScopedContentIdV1>,
}

impl InferenceSegmentV1 {
    #[allow(clippy::too_many_arguments)] // Mirrors the closed wire fields; no options bag or arbitrary JSON.
    pub fn from_canonical_bytes(
        scope: &ViaScopeV1,
        position: u16,
        role: InferenceSegmentRoleV1,
        mutability: InferenceMutabilityV1,
        canonical_bytes: &[u8],
        token_count: u64,
        delivery_state: InferenceDeliveryStateV1,
        source_lineage: Vec<ScopedContentIdV1>,
    ) -> Result<Self, ValidationError> {
        let content_or_representation_id =
            ScopedContentIdV1::from_scope_and_bytes(scope, canonical_bytes)?;
        let guard_hash = content_or_representation_id.content_digest.clone();
        let segment_id = inference_segment_id(scope, position, role, &guard_hash)?;
        let segment = Self {
            segment_id,
            position,
            content_or_representation_id,
            role,
            mutability,
            guard_hash,
            token_count,
            delivery_state,
            source_lineage,
        };
        segment.validate(scope, usize::from(position))?;
        Ok(segment)
    }

    fn validate(
        &self,
        scope: &ViaScopeV1,
        expected_position: usize,
    ) -> Result<(), ValidationError> {
        if usize::from(self.position) != expected_position {
            return Err(ValidationError::new(
                "InferenceSegmentV1 positions must be contiguous and ordered",
            ));
        }
        let scope_digest = scope.digest()?;
        if self.content_or_representation_id.scope_digest != scope_digest
            || self
                .source_lineage
                .iter()
                .any(|source| source.scope_digest != scope_digest)
        {
            return Err(ValidationError::new(
                "InferenceSegmentV1 content and lineage IDs must match manifest scope",
            ));
        }
        if self.guard_hash != self.content_or_representation_id.content_digest {
            return Err(ValidationError::new(
                "InferenceSegmentV1 guard_hash must match content identity",
            ));
        }
        if self.source_lineage.len() > MAX_VIA_INFERENCE_LINEAGE {
            return Err(ValidationError::new(format!(
                "InferenceSegmentV1 source_lineage exceeds the {MAX_VIA_INFERENCE_LINEAGE} item limit"
            )));
        }
        validate_unique(
            &self.source_lineage,
            MAX_VIA_INFERENCE_LINEAGE,
            "InferenceSegmentV1 source_lineage",
        )?;
        if self.segment_id
            != inference_segment_id(scope, self.position, self.role, &self.guard_hash)?
        {
            return Err(ValidationError::new(
                "InferenceSegmentV1 segment_id does not match its canonical binding",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InferenceAccountingV1 {
    pub total_tokens: u64,
    pub immutable_tokens: u64,
    pub conditional_tokens: u64,
    pub cache_marked_tokens: u64,
    pub uncached_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InferenceManifestV1 {
    pub scope: ViaScopeV1,
    pub manifest_id: ScopedContentIdV1,
    pub tokenizer_family: InferenceTokenizerFamilyV1,
    pub cache_policy: InferenceCachePolicyV1,
    pub segments: Vec<InferenceSegmentV1>,
    pub accounting: InferenceAccountingV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stable_prefix_digest: Option<Sha256Digest>,
}

#[derive(Serialize)]
struct InferenceManifestIdentityV1<'a> {
    accounting: &'a InferenceAccountingV1,
    cache_policy: InferenceCachePolicyV1,
    scope: &'a ViaScopeV1,
    segments: &'a [InferenceSegmentV1],
    stable_prefix_digest: &'a Option<Sha256Digest>,
    tokenizer_family: InferenceTokenizerFamilyV1,
}

impl InferenceManifestV1 {
    pub fn new(
        scope: ViaScopeV1,
        segments: Vec<InferenceSegmentV1>,
        stable_prefix_digest: Option<Sha256Digest>,
    ) -> Result<Self, ValidationError> {
        let accounting = inference_accounting(&segments)?;
        let placeholder = ScopedContentIdV1::from_scope_and_bytes(&scope, b"")?;
        let mut manifest = Self {
            scope,
            manifest_id: placeholder,
            tokenizer_family: InferenceTokenizerFamilyV1::Cl100kBaseApproxV1,
            cache_policy: InferenceCachePolicyV1::AnthropicExplicitCacheControlV1,
            segments,
            accounting,
            stable_prefix_digest,
        };
        manifest.manifest_id = manifest.expected_manifest_id()?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        self.scope.validate()?;
        if self.segments.is_empty() || self.segments.len() > MAX_VIA_INFERENCE_SEGMENTS {
            return Err(ValidationError::new(format!(
                "InferenceManifestV1 segments must contain 1..={MAX_VIA_INFERENCE_SEGMENTS} items"
            )));
        }
        let mut segment_ids = BTreeSet::new();
        for (position, segment) in self.segments.iter().enumerate() {
            segment.validate(&self.scope, position)?;
            if !segment_ids.insert(segment.segment_id.clone()) {
                return Err(ValidationError::new(
                    "InferenceManifestV1 segment IDs must be unique",
                ));
            }
        }
        if self.accounting != inference_accounting(&self.segments)? {
            return Err(ValidationError::new(
                "InferenceManifestV1 accounting does not match segment tokens",
            ));
        }
        if self.manifest_id != self.expected_manifest_id()? {
            return Err(ValidationError::new(
                "InferenceManifestV1 manifest_id does not match canonical fields",
            ));
        }
        let bytes = serde_json::to_vec(self).map_err(|error| {
            ValidationError::new(format!("serialize inference manifest: {error}"))
        })?;
        if bytes.len() > MAX_VIA_INFERENCE_MANIFEST_BYTES {
            return Err(ValidationError::new(format!(
                "InferenceManifestV1 exceeds the {MAX_VIA_INFERENCE_MANIFEST_BYTES} byte limit"
            )));
        }
        Ok(())
    }

    fn expected_manifest_id(&self) -> Result<ScopedContentIdV1, ValidationError> {
        let body = InferenceManifestIdentityV1 {
            accounting: &self.accounting,
            cache_policy: self.cache_policy,
            scope: &self.scope,
            segments: &self.segments,
            stable_prefix_digest: &self.stable_prefix_digest,
            tokenizer_family: self.tokenizer_family,
        };
        let encoded = canonical_json_bytes(&body)?;
        let mut domain = b"leanctx.inference.manifest.v1\0".to_vec();
        domain.extend_from_slice(&encoded);
        ScopedContentIdV1::from_scope_and_bytes(&self.scope, &domain)
    }
}

fn inference_accounting(
    segments: &[InferenceSegmentV1],
) -> Result<InferenceAccountingV1, ValidationError> {
    let mut accounting = InferenceAccountingV1 {
        total_tokens: 0,
        immutable_tokens: 0,
        conditional_tokens: 0,
        cache_marked_tokens: 0,
        uncached_tokens: 0,
    };
    for segment in segments {
        accounting.total_tokens = accounting
            .total_tokens
            .checked_add(segment.token_count)
            .ok_or_else(|| ValidationError::new("InferenceManifestV1 total token overflow"))?;
        let mutability = match segment.mutability {
            InferenceMutabilityV1::Immutable => &mut accounting.immutable_tokens,
            InferenceMutabilityV1::Conditional => &mut accounting.conditional_tokens,
        };
        *mutability = mutability
            .checked_add(segment.token_count)
            .ok_or_else(|| ValidationError::new("InferenceManifestV1 mutability token overflow"))?;
        let delivery = match segment.delivery_state {
            InferenceDeliveryStateV1::CacheMarked => &mut accounting.cache_marked_tokens,
            InferenceDeliveryStateV1::Uncached => &mut accounting.uncached_tokens,
        };
        *delivery = delivery
            .checked_add(segment.token_count)
            .ok_or_else(|| ValidationError::new("InferenceManifestV1 delivery token overflow"))?;
    }
    Ok(accounting)
}

fn inference_segment_id(
    scope: &ViaScopeV1,
    position: u16,
    role: InferenceSegmentRoleV1,
    guard_hash: &Sha256Digest,
) -> Result<Sha256Digest, ValidationError> {
    let mut bytes = b"leanctx.inference.segment.v1\0".to_vec();
    bytes.extend_from_slice(scope.digest()?.as_str().as_bytes());
    bytes.extend_from_slice(&position.to_be_bytes());
    bytes.push(role.discriminant());
    bytes.extend_from_slice(guard_hash.as_str().as_bytes());
    sha256(&bytes)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChunkDescriptorV1 {
    pub chunk_id: Sha256Digest,
    pub start_byte: u64,
    pub end_byte: u64,
    pub byte_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_count: Option<u64>,
    pub operator_id: CapabilityId,
    pub operator_version: SemanticVersion,
}

impl ChunkDescriptorV1 {
    fn validate(&self, content_bytes: u64) -> Result<(), ValidationError> {
        if self.start_byte >= self.end_byte
            || self.end_byte > content_bytes
            || self.byte_count != self.end_byte - self.start_byte
        {
            return Err(ValidationError::new(
                "ChunkDescriptorV1 byte range and byte_count are inconsistent",
            ));
        }
        if self.symbol.as_ref().is_some_and(|value| {
            value.trim().is_empty() || value.len() > 1_024 || value.chars().any(char::is_control)
        }) {
            return Err(ValidationError::new(
                "ChunkDescriptorV1 symbol must be bounded printable text",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentManifestV1 {
    pub scope: ViaScopeV1,
    pub content_id: ScopedContentIdV1,
    pub kind: ContentKindV1,
    pub byte_count: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub token_counts: Vec<TokenCountV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_version: Option<Sha256Digest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structural_manifest: Option<Sha256Digest>,
    pub chunks: Vec<ChunkDescriptorV1>,
    pub availability: ContentAvailabilityV1,
}

impl ContentManifestV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        self.scope.validate()?;
        if self.content_id.scope_digest != self.scope.digest()? {
            return Err(ValidationError::new(
                "ContentManifestV1 content_id scope does not match manifest scope",
            ));
        }
        if self.chunks.len() > MAX_VIA_CONTENT_ITEMS {
            return Err(ValidationError::new(format!(
                "ContentManifestV1 chunks exceeds the {MAX_VIA_CONTENT_ITEMS} item limit"
            )));
        }
        if self.byte_count == 0 && !self.chunks.is_empty()
            || self.byte_count > 0 && self.chunks.is_empty()
        {
            return Err(ValidationError::new(
                "ContentManifestV1 empty state is inconsistent with chunks",
            ));
        }
        if matches!(
            self.scope.classification,
            ViaClassificationV1::Secret | ViaClassificationV1::LocalOnly
        ) && self.availability == ContentAvailabilityV1::TransientRemoteAllowed
        {
            return Err(ValidationError::new(
                "secret or local-only content cannot allow remote availability",
            ));
        }
        if self.language.as_ref().is_some_and(|value| {
            value.trim().is_empty() || value.len() > 128 || value.chars().any(char::is_control)
        }) {
            return Err(ValidationError::new(
                "ContentManifestV1 language must be bounded printable text",
            ));
        }
        let families = self
            .token_counts
            .iter()
            .map(|entry| entry.family.as_str().to_owned())
            .collect::<Vec<_>>();
        validate_unique(
            &families,
            MAX_VIA_CAPABILITIES,
            "ContentManifestV1 token families",
        )?;
        let mut previous_end = 0;
        let mut chunk_ids = BTreeSet::new();
        for chunk in &self.chunks {
            chunk.validate(self.byte_count)?;
            if chunk.start_byte != previous_end || !chunk_ids.insert(chunk.chunk_id.clone()) {
                return Err(ValidationError::new(
                    "ContentManifestV1 chunks must be contiguous, ordered, and unique",
                ));
            }
            previous_end = chunk.end_byte;
        }
        if previous_end != self.byte_count {
            return Err(ValidationError::new(
                "ContentManifestV1 chunks must cover the declared byte_count",
            ));
        }
        let bytes = serde_json::to_vec(self).map_err(|error| {
            ValidationError::new(format!("serialize content manifest: {error}"))
        })?;
        if bytes.len() > MAX_VIA_MANIFEST_BYTES {
            return Err(ValidationError::new(format!(
                "ContentManifestV1 exceeds the {MAX_VIA_MANIFEST_BYTES} byte limit"
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NeedContentPurposeV1 {
    ContextPlanning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NeedContentV1 {
    #[serde(deserialize_with = "deserialize_via_protocol_version")]
    pub protocol_version: u16,
    pub request_id: ScopedReferenceV1,
    pub source_content_id: ScopedContentIdV1,
    pub chunk_ids: Vec<Sha256Digest>,
    pub purpose: NeedContentPurposeV1,
    pub max_bytes: u64,
    pub max_tokens: u64,
    pub expires_at: UtcTimestamp,
    pub policy_version: SemanticVersion,
}

impl NeedContentV1 {
    pub fn validate_at(&self, now: &UtcTimestamp) -> Result<(), ValidationError> {
        validate_protocol_version(self.protocol_version)?;
        if self.request_id.scope_digest != self.source_content_id.scope_digest {
            return Err(ValidationError::new(
                "NeedContentV1 request and source scopes must match",
            ));
        }
        if self.chunk_ids.is_empty() {
            return Err(ValidationError::new(
                "NeedContentV1 must request at least one chunk",
            ));
        }
        validate_unique(
            &self.chunk_ids,
            MAX_VIA_CONTENT_CHUNKS_PER_REQUEST,
            "NeedContentV1 chunk IDs",
        )?;
        if self.max_bytes == 0
            || self.max_bytes > MAX_VIA_CONTENT_REQUEST_BYTES
            || self.max_tokens == 0
            || self.max_tokens > MAX_VIA_CONTENT_REQUEST_TOKENS
        {
            return Err(ValidationError::new(
                "NeedContentV1 budgets and expiry must be bounded and active",
            ));
        }
        validate_content_ttl(now, &self.expires_at)?;
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentChunkV1 {
    #[serde(deserialize_with = "deserialize_via_protocol_version")]
    pub protocol_version: u16,
    pub request_id: ScopedReferenceV1,
    pub source_content_id: ScopedContentIdV1,
    pub chunk_id: Sha256Digest,
    pub start_byte: u64,
    pub end_byte: u64,
    pub byte_count: u64,
    pub source_version: Sha256Digest,
    pub content: String,
    pub payload_digest: Sha256Digest,
    pub expires_at: UtcTimestamp,
}

impl fmt::Debug for ContentChunkV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContentChunkV1")
            .field("request_id", &self.request_id)
            .field("source_content_id", &self.source_content_id)
            .field("chunk_id", &self.chunk_id)
            .field("byte_count", &self.byte_count)
            .field("content", &"[REDACTED]")
            .field("payload_digest", &self.payload_digest)
            .finish()
    }
}

impl ContentChunkV1 {
    pub fn validate_at(&self, now: &UtcTimestamp) -> Result<(), ValidationError> {
        validate_protocol_version(self.protocol_version)?;
        if self.request_id.scope_digest != self.source_content_id.scope_digest {
            return Err(ValidationError::new(
                "ContentChunkV1 request and source scopes must match",
            ));
        }
        let content = self.content.as_bytes();
        if self.start_byte >= self.end_byte
            || self.end_byte.checked_sub(self.start_byte) != Some(self.byte_count)
            || self.byte_count as usize != content.len()
            || content.len() > MAX_VIA_CONTENT_CHUNK_BYTES
        {
            return Err(ValidationError::new(
                "ContentChunkV1 range, size, or expiry is invalid",
            ));
        }
        validate_content_ttl(now, &self.expires_at)?;
        let digest = sha256(content)?;
        if digest != self.chunk_id || digest != self.payload_digest {
            return Err(ValidationError::new(
                "ContentChunkV1 payload digest does not bind exact content bytes",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentChunkAckV1 {
    #[serde(deserialize_with = "deserialize_via_protocol_version")]
    pub protocol_version: u16,
    pub request_id: ScopedReferenceV1,
    pub source_content_id: ScopedContentIdV1,
    pub chunk_id: Sha256Digest,
}

impl ContentChunkAckV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_protocol_version(self.protocol_version)?;
        if self.request_id.scope_digest != self.source_content_id.scope_digest {
            return Err(ValidationError::new(
                "ContentChunkAckV1 request and source scopes must match",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViaObservedEventKindV1 {
    SessionStarted,
    SessionEnded,
    AgentObserved,
    UserMessageObserved,
    AssistantMessageObserved,
    FileObserved,
    FileReadObserved,
    FileChanged,
    SearchExecuted,
    SearchResultObserved,
    ShellExecuted,
    ShellResultObserved,
    McpToolCalled,
    McpResultObserved,
    ProviderRequestPending,
    ContextDelivered,
    ContextReused,
    ContextInvalidated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationPayloadV1 {
    pub kind: ViaObservedEventKindV1,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_ids: Vec<ScopedContentIdV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_ids: Vec<ScopedContentIdV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_id: Option<CapabilityId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanEventStatusV1 {
    Applied,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanEventPayloadV1 {
    pub plan_id: PlanId,
    pub status: PlanEventStatusV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event_type", content = "payload", rename_all = "snake_case")]
pub enum ViaEventPayloadV1 {
    Observation(ObservationPayloadV1),
    ContentManifest(Box<ContentManifestV1>),
    InferenceManifest(Box<InferenceManifestV1>),
    Plan(PlanEventPayloadV1),
}

impl ViaEventPayloadV1 {
    fn validate(&self, scope: &ViaScopeV1) -> Result<(), ValidationError> {
        match self {
            Self::Observation(payload) => {
                validate_unique(
                    &payload.input_ids,
                    MAX_VIA_CONTENT_ITEMS,
                    "ObservationPayloadV1 input_ids",
                )?;
                validate_unique(
                    &payload.output_ids,
                    MAX_VIA_CONTENT_ITEMS,
                    "ObservationPayloadV1 output_ids",
                )?;
                let scope_digest = scope.digest()?;
                if payload
                    .input_ids
                    .iter()
                    .chain(&payload.output_ids)
                    .any(|id| id.scope_digest != scope_digest)
                {
                    return Err(ValidationError::new(
                        "ObservationPayloadV1 content IDs must match frame scope",
                    ));
                }
                Ok(())
            }
            Self::ContentManifest(manifest) => {
                if manifest.scope != *scope {
                    return Err(ValidationError::new(
                        "ContentManifestV1 scope must match frame scope",
                    ));
                }
                manifest.validate()
            }
            Self::InferenceManifest(manifest) => {
                if manifest.scope != *scope {
                    return Err(ValidationError::new(
                        "InferenceManifestV1 scope must match frame scope",
                    ));
                }
                manifest.validate()
            }
            Self::Plan(_) => Ok(()),
        }
    }

    pub fn digest(&self) -> Result<Sha256Digest, ValidationError> {
        let bytes = serde_json::to_vec(self).map_err(|error| {
            ValidationError::new(format!("serialize Via event payload: {error}"))
        })?;
        sha256(&bytes)
    }
}

#[derive(Deserialize)]
struct ViaEventFrameV1Wire {
    #[serde(deserialize_with = "deserialize_via_protocol_version")]
    protocol_version: u16,
    stream_id: ScopedReferenceV1,
    seq: u64,
    event_id: EventId,
    #[serde(default)]
    causality: Vec<EventId>,
    payload_hash: Sha256Digest,
    scope: ViaScopeV1,
    #[serde(flatten)]
    payload: ViaEventPayloadV1,
    #[serde(flatten)]
    unknown_fields: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ViaEventFrameV1 {
    #[serde(deserialize_with = "deserialize_via_protocol_version")]
    pub protocol_version: u16,
    pub stream_id: ScopedReferenceV1,
    pub seq: u64,
    pub event_id: EventId,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub causality: Vec<EventId>,
    pub payload_hash: Sha256Digest,
    pub scope: ViaScopeV1,
    #[serde(flatten)]
    pub payload: ViaEventPayloadV1,
}

impl<'de> Deserialize<'de> for ViaEventFrameV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ViaEventFrameV1Wire::deserialize(deserializer)?;
        if !wire.unknown_fields.is_empty() {
            let fields = wire
                .unknown_fields
                .keys()
                .map(|field| field.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(DeError::custom(format!(
                "ViaEventFrameV1 contains unknown top-level fields: {fields}"
            )));
        }
        Ok(Self {
            protocol_version: wire.protocol_version,
            stream_id: wire.stream_id,
            seq: wire.seq,
            event_id: wire.event_id,
            causality: wire.causality,
            payload_hash: wire.payload_hash,
            scope: wire.scope,
            payload: wire.payload,
        })
    }
}

impl ViaEventFrameV1 {
    pub fn new(
        stream_id: ScopedReferenceV1,
        seq: u64,
        event_id: EventId,
        causality: Vec<EventId>,
        scope: ViaScopeV1,
        payload: ViaEventPayloadV1,
    ) -> Result<Self, ValidationError> {
        let payload_hash = payload.digest()?;
        let frame = Self {
            protocol_version: VIA_PROTOCOL_V1,
            stream_id,
            seq,
            event_id,
            causality,
            payload_hash,
            scope,
            payload,
        };
        frame.validate()?;
        Ok(frame)
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_protocol_version(self.protocol_version)?;
        self.scope.validate()?;
        if self.stream_id.scope_digest != self.scope.digest()? {
            return Err(ValidationError::new(
                "ViaEventFrameV1 stream_id scope does not match frame scope",
            ));
        }
        validate_unique(
            &self.causality,
            MAX_VIA_CAUSALITY,
            "ViaEventFrameV1 causality",
        )?;
        if self.causality.contains(&self.event_id) {
            return Err(ValidationError::new(
                "ViaEventFrameV1 event_id cannot be its own cause",
            ));
        }
        self.payload.validate(&self.scope)?;
        if self.payload.digest()? != self.payload_hash {
            return Err(ValidationError::new(
                "ViaEventFrameV1 payload_hash does not match payload",
            ));
        }
        let bytes = serde_json::to_vec(self)
            .map_err(|error| ValidationError::new(format!("serialize Via event frame: {error}")))?;
        if bytes.len() > MAX_VIA_FRAME_BYTES {
            return Err(ValidationError::new(format!(
                "ViaEventFrameV1 exceeds the {MAX_VIA_FRAME_BYTES} byte limit"
            )));
        }
        Ok(())
    }

    /// Validate deterministic stream ordering against the immediately prior frame.
    pub fn validate_successor(&self, previous: &Self) -> Result<(), ValidationError> {
        self.validate()?;
        previous.validate()?;
        let expected_seq = previous.seq.checked_add(1).ok_or_else(|| {
            ValidationError::new("ViaEventFrameV1 predecessor seq cannot advance")
        })?;
        if self.stream_id != previous.stream_id || self.seq != expected_seq {
            return Err(ValidationError::new(
                "ViaEventFrameV1 successor must share stream_id and increment seq by one",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViaEventAckStatusV1 {
    Stored,
    Duplicate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViaEventAckV1 {
    #[serde(deserialize_with = "deserialize_via_protocol_version")]
    pub protocol_version: u16,
    pub stream_id: ScopedReferenceV1,
    pub acknowledged_seq: u64,
    pub acknowledged_event_id: EventId,
    pub next_seq: u64,
    pub status: ViaEventAckStatusV1,
    pub accepted_event_count: u64,
    pub graph_nodes: u64,
    pub graph_edges: u64,
}

impl ViaEventAckV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_protocol_version(self.protocol_version)?;
        if self.acknowledged_seq.checked_add(1) != Some(self.next_seq) {
            return Err(ValidationError::new(
                "ViaEventAckV1 next_seq must immediately follow acknowledged_seq",
            ));
        }
        if self.accepted_event_count != self.next_seq
            || self.accepted_event_count > MAX_VIA_ACK_EVENTS
            || self.graph_nodes > MAX_VIA_ACK_GRAPH_NODES
            || self.graph_edges > MAX_VIA_ACK_GRAPH_EDGES
        {
            return Err(ValidationError::new(
                "ViaEventAckV1 accepted_event_count must equal the stream frontier and counters must remain bounded",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViaBackpressureV1 {
    #[serde(deserialize_with = "deserialize_via_protocol_version")]
    pub protocol_version: u16,
    pub stream_id: ScopedReferenceV1,
    pub accepted_window: u16,
    pub retry_after_ms: u32,
}

impl ViaBackpressureV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_protocol_version(self.protocol_version)?;
        if self.accepted_window > MAX_VIA_STREAM_WINDOW
            || self.retry_after_ms == 0
            || self.retry_after_ms > MAX_VIA_BACKPRESSURE_RETRY_MS
        {
            return Err(ValidationError::new(
                "ViaBackpressureV1 window or retry interval exceeds its bound",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViaStreamResumeV1 {
    #[serde(deserialize_with = "deserialize_via_protocol_version")]
    pub protocol_version: u16,
    pub stream_id: ScopedReferenceV1,
}

impl ViaStreamResumeV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_protocol_version(self.protocol_version)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViaStreamPositionV1 {
    pub seq: u64,
    pub event_id: EventId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViaStreamCursorV1 {
    #[serde(deserialize_with = "deserialize_via_protocol_version")]
    pub protocol_version: u16,
    pub stream_id: ScopedReferenceV1,
    pub next_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_accepted: Option<ViaStreamPositionV1>,
}

impl ViaStreamCursorV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_protocol_version(self.protocol_version)?;
        match &self.last_accepted {
            None if self.next_seq == 0 => Ok(()),
            Some(position) if position.seq.checked_add(1) == Some(self.next_seq) => Ok(()),
            _ => Err(ValidationError::new(
                "ViaStreamCursorV1 next_seq must bind the last accepted position",
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViaPlanHeaderV1 {
    pub plan_id: PlanId,
    pub caused_by: EventId,
    pub policy_id: CapabilityId,
    pub policy_version: SemanticVersion,
    pub expected_source_ids: Vec<ScopedContentIdV1>,
    pub immutable_guard_digest: Sha256Digest,
    pub operator_id: CapabilityId,
    pub operator_version: SemanticVersion,
    pub expires_at: UtcTimestamp,
    pub output_budget_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViaPlanRequestV1 {
    #[serde(deserialize_with = "deserialize_via_protocol_version")]
    pub protocol_version: u16,
    pub stream_id: ScopedReferenceV1,
    pub event_id: EventId,
    pub immutable_guard_digest: Sha256Digest,
    pub now: UtcTimestamp,
    pub expires_at: UtcTimestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inference_candidates: Option<InferenceCandidateSetV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InferenceSegmentTransitionV1 {
    pub position: u16,
    pub old_segment_id: Sha256Digest,
    pub old_representation_id: ScopedContentIdV1,
    pub old_guard_hash: Sha256Digest,
    pub new_segment_id: Sha256Digest,
    pub new_representation_id: ScopedContentIdV1,
    pub new_guard_hash: Sha256Digest,
    pub old_token_count: u64,
    pub new_token_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InferenceCandidateSetV1 {
    pub candidate_set_id: ScopedContentIdV1,
    pub manifest_id: ScopedContentIdV1,
    pub operator_id: CapabilityId,
    pub operator_version: SemanticVersion,
    pub transitions: Vec<InferenceSegmentTransitionV1>,
}

#[derive(Serialize)]
struct InferenceCandidateSetIdentityV1<'a> {
    manifest_id: &'a ScopedContentIdV1,
    operator_id: &'a CapabilityId,
    operator_version: &'a SemanticVersion,
    transitions: &'a [InferenceSegmentTransitionV1],
}

#[derive(Serialize)]
struct InferencePlanIdIdentityV1<'a> {
    scope_digest: &'a Sha256Digest,
    caused_by: &'a EventId,
    manifest_id: &'a ScopedContentIdV1,
    candidate_set_id: &'a ScopedContentIdV1,
    selected_transitions: &'a [InferenceSegmentTransitionV1],
    output_budget_tokens: u64,
}

impl InferenceCandidateSetV1 {
    pub fn validate_for_scope(&self, scope_digest: &Sha256Digest) -> Result<(), ValidationError> {
        if self.manifest_id.scope_digest != *scope_digest
            || self.candidate_set_id.scope_digest != *scope_digest
        {
            return Err(ValidationError::new(
                "InferenceCandidateSetV1 scope does not match request",
            ));
        }
        if self.operator_id.as_str() != "leanctx.inference.history.compact"
            || self.operator_version.as_str() != "1.0.0"
        {
            return Err(ValidationError::new(
                "InferenceCandidateSetV1 operator is unsupported",
            ));
        }
        if self.transitions.is_empty() || self.transitions.len() > MAX_VIA_INFERENCE_CANDIDATES {
            return Err(ValidationError::new(format!(
                "InferenceCandidateSetV1 transitions must be within 1..={MAX_VIA_INFERENCE_CANDIDATES}"
            )));
        }
        let mut prior_position = None;
        let mut old_segments = BTreeSet::new();
        let mut new_segments = BTreeSet::new();
        for transition in &self.transitions {
            if prior_position.is_some_and(|position| transition.position <= position) {
                return Err(ValidationError::new(
                    "InferenceCandidateSetV1 transitions must be position-sorted",
                ));
            }
            prior_position = Some(transition.position);
            if !old_segments.insert(&transition.old_segment_id)
                || !new_segments.insert(&transition.new_segment_id)
                || transition.old_representation_id.scope_digest != *scope_digest
                || transition.new_representation_id.scope_digest != *scope_digest
                || transition.old_guard_hash != transition.old_representation_id.content_digest
                || transition.new_guard_hash != transition.new_representation_id.content_digest
                || transition.old_segment_id == transition.new_segment_id
                || transition.old_representation_id == transition.new_representation_id
                || transition.new_token_count >= transition.old_token_count
            {
                return Err(ValidationError::new(
                    "InferenceCandidateSetV1 transition identity or reduction is invalid",
                ));
            }
        }
        let encoded = serde_json::to_vec(self).map_err(|error| {
            ValidationError::new(format!("serialize inference candidate set: {error}"))
        })?;
        if encoded.len() > MAX_VIA_INFERENCE_CANDIDATE_SET_BYTES {
            return Err(ValidationError::new(format!(
                "InferenceCandidateSetV1 exceeds the {MAX_VIA_INFERENCE_CANDIDATE_SET_BYTES} byte limit"
            )));
        }
        if self.candidate_set_id != self.expected_id(scope_digest)? {
            return Err(ValidationError::new(
                "InferenceCandidateSetV1 ID does not match canonical metadata",
            ));
        }
        Ok(())
    }

    pub fn expected_id(
        &self,
        scope_digest: &Sha256Digest,
    ) -> Result<ScopedContentIdV1, ValidationError> {
        let identity = InferenceCandidateSetIdentityV1 {
            manifest_id: &self.manifest_id,
            operator_id: &self.operator_id,
            operator_version: &self.operator_version,
            transitions: &self.transitions,
        };
        let mut bytes = b"leanctx.inference.candidate-set.v1\0".to_vec();
        bytes.extend_from_slice(&serde_json::to_vec(&identity).map_err(|error| {
            ValidationError::new(format!("serialize inference candidate identity: {error}"))
        })?);
        Ok(ScopedContentIdV1 {
            scope_digest: scope_digest.clone(),
            content_digest: sha256(&bytes)?,
        })
    }
}

pub fn inference_plan_id(
    scope_digest: &Sha256Digest,
    caused_by: &EventId,
    manifest_id: &ScopedContentIdV1,
    candidate_set_id: &ScopedContentIdV1,
    selected_transitions: &[InferenceSegmentTransitionV1],
    output_budget_tokens: u64,
) -> Result<PlanId, ValidationError> {
    let identity = InferencePlanIdIdentityV1 {
        scope_digest,
        caused_by,
        manifest_id,
        candidate_set_id,
        selected_transitions,
        output_budget_tokens,
    };
    let mut bytes = b"leanctx.inference.plan-id.v1\0".to_vec();
    bytes.extend_from_slice(&serde_json::to_vec(&identity).map_err(|error| {
        ValidationError::new(format!("serialize inference plan identity: {error}"))
    })?);
    let digest = sha256(&bytes)?;
    let hex = digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or_else(|| ValidationError::new("inference plan digest prefix is invalid"))?;
    PlanId::new(format!("inference-{hex}"))
}

/// Deterministically binds every immutable inference-manifest field that an
/// optimizer is forbidden to change.
pub fn inference_immutable_guard(
    manifest: &InferenceManifestV1,
) -> Result<Sha256Digest, ValidationError> {
    manifest.validate()?;
    let mut bytes = b"leanctx.inference.immutable-guard.v1\0".to_vec();
    push_guard_text(&mut bytes, manifest.scope.digest()?.as_str())?;
    bytes.push(match manifest.tokenizer_family {
        InferenceTokenizerFamilyV1::Cl100kBaseApproxV1 => 0,
    });
    bytes.push(match manifest.cache_policy {
        InferenceCachePolicyV1::AnthropicExplicitCacheControlV1 => 0,
    });
    let immutable = manifest
        .segments
        .iter()
        .filter(|segment| segment.mutability == InferenceMutabilityV1::Immutable)
        .collect::<Vec<_>>();
    bytes.extend_from_slice(
        &u16::try_from(immutable.len())
            .map_err(|_| ValidationError::new("too many immutable inference segments"))?
            .to_be_bytes(),
    );
    for segment in immutable {
        bytes.extend_from_slice(&segment.position.to_be_bytes());
        bytes.push(match segment.role {
            InferenceSegmentRoleV1::System => 0,
            InferenceSegmentRoleV1::Developer => 1,
            InferenceSegmentRoleV1::User => 2,
            InferenceSegmentRoleV1::Assistant => 3,
            InferenceSegmentRoleV1::ToolCall => 4,
            InferenceSegmentRoleV1::ToolResult => 5,
            InferenceSegmentRoleV1::ToolSchema => 6,
            InferenceSegmentRoleV1::RequestControl => 7,
        });
        push_guard_text(&mut bytes, segment.segment_id.as_str())?;
        push_guard_text(
            &mut bytes,
            segment.content_or_representation_id.scope_digest.as_str(),
        )?;
        push_guard_text(
            &mut bytes,
            segment.content_or_representation_id.content_digest.as_str(),
        )?;
        push_guard_text(&mut bytes, segment.guard_hash.as_str())?;
        bytes.extend_from_slice(&segment.token_count.to_be_bytes());
        bytes.push(match segment.delivery_state {
            InferenceDeliveryStateV1::CacheMarked => 0,
            InferenceDeliveryStateV1::Uncached => 1,
        });
        bytes.push(
            u8::try_from(segment.source_lineage.len())
                .map_err(|_| ValidationError::new("too much inference lineage"))?,
        );
        for source in &segment.source_lineage {
            push_guard_text(&mut bytes, source.scope_digest.as_str())?;
            push_guard_text(&mut bytes, source.content_digest.as_str())?;
        }
    }
    sha256(&bytes)
}

fn push_guard_text(bytes: &mut Vec<u8>, value: &str) -> Result<(), ValidationError> {
    bytes.extend_from_slice(
        &u32::try_from(value.len())
            .map_err(|_| ValidationError::new("inference guard field is too large"))?
            .to_be_bytes(),
    );
    bytes.extend_from_slice(value.as_bytes());
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InferencePlanV1 {
    pub manifest_id: ScopedContentIdV1,
    pub transitions: Vec<InferenceSegmentTransitionV1>,
}

impl InferencePlanV1 {
    fn validate(&self, scope: &ViaScopeV1) -> Result<(), ValidationError> {
        if self.transitions.is_empty() || self.transitions.len() > MAX_VIA_INFERENCE_SEGMENTS {
            return Err(ValidationError::new(format!(
                "InferencePlanV1 transitions must be within 1..={MAX_VIA_INFERENCE_SEGMENTS}"
            )));
        }
        let scope_digest = scope.digest()?;
        if self.manifest_id.scope_digest != scope_digest {
            return Err(ValidationError::new(
                "InferencePlanV1 manifest_id must match plan scope",
            ));
        }
        let mut prior_position = None;
        let mut old_segments = BTreeSet::new();
        let mut new_segments = BTreeSet::new();
        for transition in &self.transitions {
            if prior_position.is_some_and(|position| transition.position <= position) {
                return Err(ValidationError::new(
                    "InferencePlanV1 transitions must be strictly position-sorted",
                ));
            }
            prior_position = Some(transition.position);
            if !old_segments.insert(&transition.old_segment_id)
                || !new_segments.insert(&transition.new_segment_id)
            {
                return Err(ValidationError::new(
                    "InferencePlanV1 transition segment IDs must be unique",
                ));
            }
            if transition.old_representation_id.scope_digest != scope_digest
                || transition.new_representation_id.scope_digest != scope_digest
                || transition.old_guard_hash != transition.old_representation_id.content_digest
                || transition.new_guard_hash != transition.new_representation_id.content_digest
                || transition.old_segment_id == transition.new_segment_id
                || transition.old_representation_id == transition.new_representation_id
                || transition.new_token_count >= transition.old_token_count
            {
                return Err(ValidationError::new(
                    "InferencePlanV1 transition identity or token reduction is invalid",
                ));
            }
        }
        Ok(())
    }
}

impl ViaPlanRequestV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_protocol_version(self.protocol_version)?;
        validate_plan_ttl(&self.now, &self.expires_at)?;
        if let Some(candidates) = &self.inference_candidates {
            candidates.validate_for_scope(&self.stream_id.scope_digest)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "plan_type", rename_all = "snake_case")]
pub enum ViaPlanBodyV1 {
    Read { source_id: ScopedContentIdV1 },
    Search { index_id: ScopedContentIdV1 },
    Shell { output_id: ScopedContentIdV1 },
    Mcp { output_id: ScopedContentIdV1 },
    Reuse { source_ids: Vec<ScopedContentIdV1> },
    Inference { inference_plan: InferencePlanV1 },
}

impl ViaPlanBodyV1 {
    fn source_ids(&self) -> Vec<&ScopedContentIdV1> {
        match self {
            Self::Read { source_id } => vec![source_id],
            Self::Search { index_id } => vec![index_id],
            Self::Shell { output_id } | Self::Mcp { output_id } => vec![output_id],
            Self::Reuse { source_ids } => source_ids.iter().collect(),
            Self::Inference { inference_plan } => vec![&inference_plan.manifest_id],
        }
    }
}

#[derive(Deserialize)]
struct ViaOptimizationPlanV1Wire {
    #[serde(deserialize_with = "deserialize_via_protocol_version")]
    protocol_version: u16,
    scope: ViaScopeV1,
    header: ViaPlanHeaderV1,
    #[serde(flatten)]
    body: ViaPlanBodyV1,
}

struct StrictPlanJsonValue(Value);

impl<'de> Deserialize<'de> for StrictPlanJsonValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictPlanJsonVisitor)
    }
}

struct StrictPlanJsonVisitor;

impl<'de> Visitor<'de> for StrictPlanJsonVisitor {
    type Value = StrictPlanJsonValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON plan value with unique object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(StrictPlanJsonValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(StrictPlanJsonValue(Value::Number(value.into())))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(StrictPlanJsonValue(Value::Number(value.into())))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .map(StrictPlanJsonValue)
            .ok_or_else(|| E::custom("non-finite JSON number in Via plan"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(StrictPlanJsonValue(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(StrictPlanJsonValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(StrictPlanJsonValue(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(StrictPlanJsonValue(Value::Null))
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = seq.next_element::<StrictPlanJsonValue>()? {
            values.push(value.0);
        }
        Ok(StrictPlanJsonValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut object = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if object.contains_key(&key) {
                return Err(serde::de::Error::custom(format!(
                    "duplicate JSON object key {key:?}"
                )));
            }
            let value = map.next_value::<StrictPlanJsonValue>()?;
            object.insert(key, value.0);
        }
        Ok(StrictPlanJsonValue(Value::Object(object)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ViaOptimizationPlanV1 {
    #[serde(deserialize_with = "deserialize_via_protocol_version")]
    pub protocol_version: u16,
    pub scope: ViaScopeV1,
    pub header: ViaPlanHeaderV1,
    #[serde(flatten)]
    pub body: ViaPlanBodyV1,
}

impl<'de> Deserialize<'de> for ViaOptimizationPlanV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = StrictPlanJsonValue::deserialize(deserializer)?.0;
        let object = value
            .as_object()
            .ok_or_else(|| DeError::custom("ViaOptimizationPlanV1 must be an object"))?;
        let plan_type = object
            .get("plan_type")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| DeError::custom("ViaOptimizationPlanV1 requires plan_type"))?;
        let variant_field = match plan_type {
            "read" => "source_id",
            "search" => "index_id",
            "shell" | "mcp" => "output_id",
            "reuse" => "source_ids",
            "inference" => "inference_plan",
            other => {
                return Err(DeError::custom(format!(
                    "ViaOptimizationPlanV1 has unsupported plan_type {other}"
                )));
            }
        };
        let unknown = object
            .keys()
            .filter(|field| {
                !matches!(
                    field.as_str(),
                    "protocol_version" | "scope" | "header" | "plan_type"
                ) && field.as_str() != variant_field
            })
            .map(String::as_str)
            .collect::<Vec<_>>();
        if !unknown.is_empty() {
            return Err(DeError::custom(format!(
                "ViaOptimizationPlanV1 contains unknown top-level fields: {}",
                unknown.join(", ")
            )));
        }
        let wire =
            serde_json::from_value::<ViaOptimizationPlanV1Wire>(value).map_err(DeError::custom)?;
        Ok(Self {
            protocol_version: wire.protocol_version,
            scope: wire.scope,
            header: wire.header,
            body: wire.body,
        })
    }
}

impl ViaOptimizationPlanV1 {
    pub fn validate_at(&self, now: &UtcTimestamp) -> Result<(), ValidationError> {
        validate_protocol_version(self.protocol_version)?;
        self.scope.validate()?;
        let scope_digest = self.scope.digest()?;
        if self.header.output_budget_tokens == 0
            || self.header.output_budget_tokens > MAX_VIA_OUTPUT_TOKENS
        {
            return Err(ValidationError::new(format!(
                "ViaPlanHeaderV1 output_budget_tokens must be within 1..={MAX_VIA_OUTPUT_TOKENS}"
            )));
        }
        validate_unique(
            &self.header.expected_source_ids,
            MAX_VIA_PLAN_SOURCES,
            "ViaPlanHeaderV1 expected_source_ids",
        )?;
        if self.header.expected_source_ids.is_empty() {
            return Err(ValidationError::new(
                "ViaPlanHeaderV1 expected_source_ids must not be empty",
            ));
        }
        if self
            .header
            .expected_source_ids
            .iter()
            .any(|source| source.scope_digest != scope_digest)
        {
            return Err(ValidationError::new(
                "ViaPlanHeaderV1 expected_source_ids must match plan scope",
            ));
        }
        validate_plan_ttl(now, &self.header.expires_at)?;
        let expected = self
            .header
            .expected_source_ids
            .iter()
            .collect::<BTreeSet<_>>();
        let body_sources = self
            .body
            .source_ids()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        validate_unique(
            &body_sources,
            MAX_VIA_PLAN_SOURCES,
            "Via plan body source IDs",
        )?;
        if body_sources.is_empty() || body_sources.iter().any(|source| !expected.contains(source)) {
            return Err(ValidationError::new(
                "Via plan body sources must be covered by expected_source_ids",
            ));
        }
        if let ViaPlanBodyV1::Inference { inference_plan } = &self.body {
            inference_plan.validate(&self.scope)?;
        }
        let bytes = serde_json::to_vec(self)
            .map_err(|error| ValidationError::new(format!("serialize Via plan: {error}")))?;
        if bytes.len() > MAX_VIA_PLAN_BYTES {
            return Err(ValidationError::new(format!(
                "ViaOptimizationPlanV1 exceeds the {MAX_VIA_PLAN_BYTES} byte limit"
            )));
        }
        Ok(())
    }

    /// Validate a plan against Edge-owned state immediately before execution.
    pub fn validate_for_execution(
        &self,
        now: &UtcTimestamp,
        current_event_id: &EventId,
        current_source_ids: &[ScopedContentIdV1],
        current_guard_digest: &Sha256Digest,
    ) -> Result<(), ValidationError> {
        self.validate_at(now)?;
        if &self.header.caused_by != current_event_id {
            return Err(ValidationError::new(
                "Via optimization plan causal event is stale",
            ));
        }
        validate_unique(
            current_source_ids,
            MAX_VIA_PLAN_SOURCES,
            "current source IDs",
        )?;
        let expected = self
            .header
            .expected_source_ids
            .iter()
            .collect::<BTreeSet<_>>();
        let current = current_source_ids.iter().collect::<BTreeSet<_>>();
        if current != expected {
            return Err(ValidationError::new(
                "Via optimization plan source state is stale",
            ));
        }
        if current_guard_digest != &self.header.immutable_guard_digest {
            return Err(ValidationError::new(
                "Via optimization plan immutable guard is stale",
            ));
        }
        Ok(())
    }
}

/// Closed V1 serialization boundary between Edge and Via.
///
/// Only these protocol-owned variants can enter the canonical wire encoder.
/// Provider requests, headers, credentials, and cookie stores have no variant
/// and cannot acquire one outside this crate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "message_type",
    content = "message",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ViaWireMessageV1 {
    Hello(ViaProtocolHelloV1),
    Event(Box<ViaEventFrameV1>),
    EventAck(ViaEventAckV1),
    Backpressure(ViaBackpressureV1),
    StreamResume(ViaStreamResumeV1),
    StreamCursor(ViaStreamCursorV1),
    ContentManifest(Box<ContentManifestV1>),
    NeedContent(NeedContentV1),
    ContentChunk(Box<ContentChunkV1>),
    ContentChunkAck(ContentChunkAckV1),
    PlanRequest(ViaPlanRequestV1),
    OptimizationPlan(Box<ViaOptimizationPlanV1>),
}

impl ViaWireMessageV1 {
    pub fn validate_at(&self, now: &UtcTimestamp) -> Result<(), ValidationError> {
        match self {
            Self::Hello(hello) => hello.validate(),
            Self::Event(event) => event.validate(),
            Self::EventAck(ack) => ack.validate(),
            Self::Backpressure(backpressure) => backpressure.validate(),
            Self::StreamResume(resume) => resume.validate(),
            Self::StreamCursor(cursor) => cursor.validate(),
            Self::ContentManifest(manifest) => manifest.validate(),
            Self::NeedContent(request) => request.validate_at(now),
            Self::ContentChunk(chunk) => chunk.validate_at(now),
            Self::ContentChunkAck(ack) => ack.validate(),
            Self::PlanRequest(request) => request.validate(),
            Self::OptimizationPlan(plan) => plan.validate_at(now),
        }
    }

    pub fn encode_at(&self, now: &UtcTimestamp) -> Result<Vec<u8>, ValidationError> {
        self.validate_at(now)?;
        let bytes = serde_json::to_vec(self)
            .map_err(|error| ValidationError::new(format!("serialize Via V1 message: {error}")))?;
        if bytes.len() > MAX_VIA_WIRE_BYTES {
            return Err(ValidationError::new(format!(
                "ViaWireMessageV1 exceeds the {MAX_VIA_WIRE_BYTES} byte limit"
            )));
        }
        if matches!(self, Self::PlanRequest(_)) && bytes.len() > MAX_VIA_PLAN_REQUEST_BYTES {
            return Err(ValidationError::new(format!(
                "Via plan request exceeds the {MAX_VIA_PLAN_REQUEST_BYTES} byte limit"
            )));
        }
        Ok(bytes)
    }

    pub fn decode_at(bytes: &[u8], now: &UtcTimestamp) -> Result<Self, ValidationError> {
        if bytes.len() > MAX_VIA_WIRE_BYTES {
            return Err(ValidationError::new(format!(
                "ViaWireMessageV1 exceeds the {MAX_VIA_WIRE_BYTES} byte limit"
            )));
        }
        let message = serde_json::from_slice::<Self>(bytes)
            .map_err(|error| ValidationError::new(format!("decode Via V1 message: {error}")))?;
        if matches!(message, Self::PlanRequest(_)) && bytes.len() > MAX_VIA_PLAN_REQUEST_BYTES {
            return Err(ValidationError::new(format!(
                "Via plan request exceeds the {MAX_VIA_PLAN_REQUEST_BYTES} byte limit"
            )));
        }
        message.validate_at(now)?;
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const C: &str = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    const FORBIDDEN_AUTH_FIELDS: &[&str] = &[
        "authorization",
        "cookie",
        "api_key",
        "api-key",
        "x-api-key",
        "x-goog-api-key",
        "x-oauth-token",
        "x-xai-token-auth",
        "oauth_token",
        "access_token",
        "refresh_token",
        "session_token",
        "client_secret",
        "credentials",
        "provider_headers",
        "raw_provider_request",
    ];

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn digest(value: &str) -> Sha256Digest {
        Sha256Digest::new(value).unwrap()
    }

    fn scope(classification: ViaClassificationV1) -> ViaScopeV1 {
        ViaScopeV1 {
            protocol_version: VIA_PROTOCOL_V1,
            event_or_request_id: id("request-1"),
            causality_id: Some(id("cause-1")),
            user_id: Some(id("user-1")),
            device_id: Some(id("device-1")),
            project_id: Some(id("project-1")),
            workspace_id: Some(id("workspace-1")),
            session_id: Some(id("session-1")),
            conversation_id: Some(id("conversation-1")),
            agent_id: Some(id("agent-1")),
            team_id: None,
            organization_id: None,
            classification,
        }
    }

    fn content_id(scope: &ViaScopeV1) -> ScopedContentIdV1 {
        ScopedContentIdV1 {
            scope_digest: scope.digest().unwrap(),
            content_digest: digest(B),
        }
    }

    fn stream_id(scope: &ViaScopeV1) -> ScopedReferenceV1 {
        ScopedReferenceV1::new(scope, id("stream-1")).unwrap()
    }

    fn valid_manifest(
        scope: ViaScopeV1,
        kind: ContentKindV1,
        availability: ContentAvailabilityV1,
    ) -> ContentManifestV1 {
        let content_id = content_id(&scope);
        ContentManifestV1 {
            scope,
            content_id,
            kind,
            byte_count: 4,
            token_counts: vec![],
            source_version: None,
            language: None,
            structural_manifest: None,
            chunks: vec![ChunkDescriptorV1 {
                chunk_id: digest(A),
                start_byte: 0,
                end_byte: 4,
                byte_count: 4,
                symbol: None,
                token_count: Some(1),
                operator_id: id("operator:chunk"),
                operator_version: SemanticVersion::new("1.0.0").unwrap(),
            }],
            availability,
        }
    }

    fn valid_plan(scope: ViaScopeV1, body: ViaPlanBodyV1) -> ViaOptimizationPlanV1 {
        ViaOptimizationPlanV1 {
            protocol_version: VIA_PROTOCOL_V1,
            header: ViaPlanHeaderV1 {
                plan_id: id("plan-wire-1"),
                caused_by: id("event-wire-1"),
                policy_id: id("policy:wire"),
                policy_version: SemanticVersion::new("1.0.0").unwrap(),
                expected_source_ids: vec![content_id(&scope)],
                immutable_guard_digest: digest(A),
                operator_id: id("operator:wire"),
                operator_version: SemanticVersion::new("1.0.0").unwrap(),
                expires_at: UtcTimestamp::new("2026-08-26T17:05:00Z").unwrap(),
                output_budget_tokens: 1_000,
            },
            scope,
            body,
        }
    }

    fn assert_observed_kind_is_covered(kind: ViaObservedEventKindV1) {
        match kind {
            ViaObservedEventKindV1::SessionStarted
            | ViaObservedEventKindV1::SessionEnded
            | ViaObservedEventKindV1::AgentObserved
            | ViaObservedEventKindV1::UserMessageObserved
            | ViaObservedEventKindV1::AssistantMessageObserved
            | ViaObservedEventKindV1::FileObserved
            | ViaObservedEventKindV1::FileReadObserved
            | ViaObservedEventKindV1::FileChanged
            | ViaObservedEventKindV1::SearchExecuted
            | ViaObservedEventKindV1::SearchResultObserved
            | ViaObservedEventKindV1::ShellExecuted
            | ViaObservedEventKindV1::ShellResultObserved
            | ViaObservedEventKindV1::McpToolCalled
            | ViaObservedEventKindV1::McpResultObserved
            | ViaObservedEventKindV1::ProviderRequestPending
            | ViaObservedEventKindV1::ContextDelivered
            | ViaObservedEventKindV1::ContextReused
            | ViaObservedEventKindV1::ContextInvalidated => {}
        }
    }

    fn assert_manifest_enums_are_covered(
        classification: ViaClassificationV1,
        kind: ContentKindV1,
        availability: ContentAvailabilityV1,
    ) {
        match classification {
            ViaClassificationV1::Public
            | ViaClassificationV1::Normal
            | ViaClassificationV1::Sensitive
            | ViaClassificationV1::Secret
            | ViaClassificationV1::LocalOnly
            | ViaClassificationV1::EnterprisePrivate => {}
        }
        match kind {
            ContentKindV1::SourceFile
            | ContentKindV1::Message
            | ContentKindV1::SearchResult
            | ContentKindV1::ShellResult
            | ContentKindV1::McpResult
            | ContentKindV1::InferenceManifest => {}
        }
        match availability {
            ContentAvailabilityV1::EdgeOnly
            | ContentAvailabilityV1::TransientRemoteAllowed
            | ContentAvailabilityV1::DeniedRemote => {}
        }
    }

    #[test]
    fn identity_scope_round_trips_with_reserved_agent_fields() {
        let value = scope(ViaClassificationV1::Normal);
        let json = serde_json::to_string(&value).unwrap();
        assert_eq!(serde_json::from_str::<ViaScopeV1>(&json).unwrap(), value);
        assert!(json.contains("conversation_id"));
        assert!(json.contains("agent_id"));
    }

    #[test]
    fn negotiation_selects_v1_and_rejects_no_overlap() {
        let hello = ViaProtocolHelloV1 {
            minimum_version: 1,
            maximum_version: 2,
            capabilities: vec![ViaCapabilityV1::EventStream],
        };
        assert_eq!(hello.negotiate(&hello).unwrap(), VIA_PROTOCOL_V1);
        let future = ViaProtocolHelloV1 {
            minimum_version: 2,
            maximum_version: 3,
            capabilities: vec![],
        };
        assert!(hello.negotiate(&future).is_err());
        let peer = ViaProtocolHelloV1 {
            minimum_version: 1,
            maximum_version: 1,
            capabilities: vec![ViaCapabilityV1::TypedPlans, ViaCapabilityV1::EventStream],
        };
        assert_eq!(
            hello.shared_capabilities(&peer).unwrap(),
            vec![ViaCapabilityV1::EventStream]
        );
    }

    #[test]
    fn manifest_is_metadata_only_bounded_and_classification_safe() {
        let scope = scope(ViaClassificationV1::Secret);
        let mut manifest = ContentManifestV1 {
            content_id: content_id(&scope),
            scope,
            kind: ContentKindV1::SourceFile,
            byte_count: 4,
            token_counts: vec![],
            source_version: None,
            language: Some("rust".into()),
            structural_manifest: None,
            chunks: vec![ChunkDescriptorV1 {
                chunk_id: digest(A),
                start_byte: 0,
                end_byte: 4,
                byte_count: 4,
                symbol: None,
                token_count: Some(1),
                operator_id: id("operator:chunk"),
                operator_version: SemanticVersion::new("1.0.0").unwrap(),
            }],
            availability: ContentAvailabilityV1::TransientRemoteAllowed,
        };
        assert!(manifest.validate().is_err());
        manifest.availability = ContentAvailabilityV1::EdgeOnly;
        manifest.validate().unwrap();
        manifest.content_id.scope_digest = digest(A);
        assert!(manifest.validate().is_err());
        manifest.content_id.scope_digest = manifest.scope.digest().unwrap();
        let json = serde_json::to_string(&manifest).unwrap();
        assert!(!json.contains("bytes"));
    }

    #[test]
    fn event_hash_is_deterministic_and_detects_mutation() {
        let scope = scope(ViaClassificationV1::Normal);
        let payload = ViaEventPayloadV1::Observation(ObservationPayloadV1 {
            kind: ViaObservedEventKindV1::FileObserved,
            input_ids: vec![],
            output_ids: vec![content_id(&scope)],
            capability_id: None,
        });
        let mut frame = ViaEventFrameV1::new(
            stream_id(&scope),
            1,
            id("event-1"),
            vec![],
            scope,
            payload.clone(),
        )
        .unwrap();
        assert_eq!(frame.payload_hash, payload.digest().unwrap());
        assert_eq!(
            serde_json::to_vec(&frame).unwrap(),
            serde_json::to_vec(&frame).unwrap()
        );
        frame.payload = ViaEventPayloadV1::Plan(PlanEventPayloadV1 {
            plan_id: id("plan-1"),
            status: PlanEventStatusV1::Applied,
        });
        assert!(frame.validate().is_err());
    }

    #[test]
    fn event_decoder_rejects_unsupported_version() {
        let scope = scope(ViaClassificationV1::Normal);
        let payload = ViaEventPayloadV1::Observation(ObservationPayloadV1 {
            kind: ViaObservedEventKindV1::SessionStarted,
            input_ids: vec![],
            output_ids: vec![],
            capability_id: None,
        });
        let frame =
            ViaEventFrameV1::new(stream_id(&scope), 0, id("event-1"), vec![], scope, payload)
                .unwrap();
        let mut value = serde_json::to_value(frame).unwrap();
        value["protocol_version"] = serde_json::json!(2);
        assert!(serde_json::from_value::<ViaEventFrameV1>(value).is_err());
    }

    #[test]
    fn typed_plan_rejects_stale_or_unexpected_sources() {
        let scope = scope(ViaClassificationV1::Normal);
        let expected = content_id(&scope);
        let mut plan = ViaOptimizationPlanV1 {
            protocol_version: VIA_PROTOCOL_V1,
            scope,
            header: ViaPlanHeaderV1 {
                plan_id: id("plan-1"),
                caused_by: id("event-1"),
                policy_id: id("policy:read"),
                policy_version: SemanticVersion::new("1.0.0").unwrap(),
                expected_source_ids: vec![expected.clone()],
                immutable_guard_digest: digest(A),
                operator_id: id("operator:read"),
                operator_version: SemanticVersion::new("1.0.0").unwrap(),
                expires_at: UtcTimestamp::new("2026-08-26T17:05:00Z").unwrap(),
                output_budget_tokens: 1_000,
            },
            body: ViaPlanBodyV1::Read {
                source_id: expected.clone(),
            },
        };
        let now = UtcTimestamp::new("2026-08-26T17:00:00Z").unwrap();
        plan.validate_at(&now).unwrap();
        plan.validate_for_execution(
            &now,
            &id("event-1"),
            std::slice::from_ref(&expected),
            &digest(A),
        )
        .unwrap();
        assert!(
            plan.validate_for_execution(
                &now,
                &id("event-1"),
                std::slice::from_ref(&expected),
                &digest(B),
            )
            .is_err()
        );
        assert!(
            plan.validate_for_execution(&now, &id("event-1"), &[], &digest(A))
                .is_err()
        );
        assert!(
            plan.validate_for_execution(
                &now,
                &id("event-other"),
                std::slice::from_ref(&expected),
                &digest(A),
            )
            .is_err()
        );
        let expired = UtcTimestamp::new("2026-08-26T17:05:00Z").unwrap();
        assert!(plan.validate_at(&expired).is_err());
        plan.body = ViaPlanBodyV1::Reuse {
            source_ids: vec![expected.clone(), expected],
        };
        assert!(plan.validate_at(&now).is_err());
        plan.body = ViaPlanBodyV1::Read {
            source_id: ScopedContentIdV1 {
                scope_digest: digest(B),
                content_digest: digest(A),
            },
        };
        assert!(plan.validate_at(&now).is_err());
    }

    #[test]
    fn scoped_content_identity_depends_on_scope_and_bytes_not_path() {
        let primary_scope = scope(ViaClassificationV1::Normal);
        let other = scope(ViaClassificationV1::Sensitive);
        let first = ScopedContentIdV1::from_scope_and_bytes(&primary_scope, b"same bytes").unwrap();
        let second =
            ScopedContentIdV1::from_scope_and_bytes(&primary_scope, b"same bytes").unwrap();
        let other_scope = ScopedContentIdV1::from_scope_and_bytes(&other, b"same bytes").unwrap();
        assert_eq!(first, second);
        assert_ne!(first, other_scope);
    }

    #[test]
    fn manifest_rejects_gaps_and_incomplete_coverage() {
        let chunk = |chunk_id, start_byte, end_byte| ChunkDescriptorV1 {
            chunk_id: digest(chunk_id),
            start_byte,
            end_byte,
            byte_count: end_byte - start_byte,
            symbol: None,
            token_count: None,
            operator_id: id("operator:chunk"),
            operator_version: SemanticVersion::new("1.0.0").unwrap(),
        };
        let scope = scope(ViaClassificationV1::Normal);
        let mut manifest = ContentManifestV1 {
            content_id: content_id(&scope),
            scope,
            kind: ContentKindV1::SourceFile,
            byte_count: 8,
            token_counts: vec![],
            source_version: None,
            language: None,
            structural_manifest: None,
            chunks: vec![chunk(A, 0, 4), chunk(B, 5, 8)],
            availability: ContentAvailabilityV1::EdgeOnly,
        };
        assert!(manifest.validate().is_err());
        manifest.chunks = vec![chunk(A, 0, 4)];
        assert!(manifest.validate().is_err());
        manifest.chunks = vec![chunk(A, 0, 4), chunk(B, 4, 8)];
        manifest.validate().unwrap();

        manifest.byte_count = 300;
        manifest.chunks = (0_u64..300)
            .map(|offset| ChunkDescriptorV1 {
                chunk_id: sha256(&offset.to_be_bytes()).unwrap(),
                start_byte: offset,
                end_byte: offset + 1,
                byte_count: 1,
                symbol: Some("x".repeat(1_024)),
                token_count: None,
                operator_id: id("operator:chunk"),
                operator_version: SemanticVersion::new("1.0.0").unwrap(),
            })
            .collect();
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn event_rejects_duplicate_self_causality_and_invalid_successor() {
        let scope = scope(ViaClassificationV1::Normal);
        let payload = || {
            ViaEventPayloadV1::Observation(ObservationPayloadV1 {
                kind: ViaObservedEventKindV1::SessionStarted,
                input_ids: vec![],
                output_ids: vec![],
                capability_id: None,
            })
        };
        let first = ViaEventFrameV1::new(
            stream_id(&scope),
            7,
            id("event-1"),
            vec![],
            scope.clone(),
            payload(),
        )
        .unwrap();
        let next = ViaEventFrameV1::new(
            stream_id(&scope),
            8,
            id("event-2"),
            vec![id("event-1")],
            scope.clone(),
            payload(),
        )
        .unwrap();
        next.validate_successor(&first).unwrap();

        let terminal = ViaEventFrameV1::new(
            stream_id(&scope),
            u64::MAX,
            id("event-terminal"),
            vec![],
            scope.clone(),
            payload(),
        )
        .unwrap();
        let terminal_replay = ViaEventFrameV1::new(
            stream_id(&scope),
            u64::MAX,
            id("event-replay"),
            vec![],
            scope,
            payload(),
        )
        .unwrap();
        assert!(terminal_replay.validate_successor(&terminal).is_err());

        let mut invalid = next.clone();
        invalid.stream_id.scope_digest = digest(A);
        assert!(invalid.validate().is_err());
        invalid = next.clone();
        invalid.seq = 9;
        assert!(invalid.validate_successor(&first).is_err());
        invalid = next.clone();
        invalid.causality = vec![id("event-1"), id("event-1")];
        assert!(invalid.validate().is_err());
        invalid.causality = vec![id("event-2")];
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn stream_ack_and_resume_cursor_are_exactly_bound() {
        let scope = scope(ViaClassificationV1::Normal);
        let stream_id = stream_id(&scope);
        let event_id: EventId = id("event-ack-7");
        let ack = ViaEventAckV1 {
            protocol_version: VIA_PROTOCOL_V1,
            stream_id: stream_id.clone(),
            acknowledged_seq: 7,
            acknowledged_event_id: event_id.clone(),
            next_seq: 8,
            status: ViaEventAckStatusV1::Stored,
            accepted_event_count: 8,
            graph_nodes: 2,
            graph_edges: 1,
        };
        ack.validate().unwrap();
        let mut invalid_ack = ack.clone();
        invalid_ack.next_seq = 9;
        assert!(invalid_ack.validate().is_err());
        invalid_ack = ack.clone();
        invalid_ack.accepted_event_count = 0;
        assert!(invalid_ack.validate().is_err());
        invalid_ack = ack.clone();
        invalid_ack.accepted_event_count = 9;
        assert!(invalid_ack.validate().is_err());
        invalid_ack = ack.clone();
        invalid_ack.graph_edges = MAX_VIA_ACK_GRAPH_EDGES + 1;
        assert!(invalid_ack.validate().is_err());

        let backpressure = ViaBackpressureV1 {
            protocol_version: VIA_PROTOCOL_V1,
            stream_id: stream_id.clone(),
            accepted_window: MAX_VIA_STREAM_WINDOW,
            retry_after_ms: MAX_VIA_BACKPRESSURE_RETRY_MS,
        };
        backpressure.validate().unwrap();
        let mut invalid_backpressure = backpressure.clone();
        invalid_backpressure.accepted_window = MAX_VIA_STREAM_WINDOW + 1;
        assert!(invalid_backpressure.validate().is_err());
        invalid_backpressure = backpressure;
        invalid_backpressure.retry_after_ms = 0;
        assert!(invalid_backpressure.validate().is_err());

        ViaStreamResumeV1 {
            protocol_version: VIA_PROTOCOL_V1,
            stream_id: stream_id.clone(),
        }
        .validate()
        .unwrap();
        ViaStreamCursorV1 {
            protocol_version: VIA_PROTOCOL_V1,
            stream_id: stream_id.clone(),
            next_seq: 0,
            last_accepted: None,
        }
        .validate()
        .unwrap();
        let cursor = ViaStreamCursorV1 {
            protocol_version: VIA_PROTOCOL_V1,
            stream_id,
            next_seq: 8,
            last_accepted: Some(ViaStreamPositionV1 { seq: 7, event_id }),
        };
        cursor.validate().unwrap();
        let mut invalid_cursor = cursor;
        invalid_cursor.next_seq = 7;
        assert!(invalid_cursor.validate().is_err());
    }

    #[test]
    fn content_on_demand_is_request_bound_bounded_and_debug_redacted() {
        let now = UtcTimestamp::new("2026-08-26T17:00:00Z").unwrap();
        let expires_at = UtcTimestamp::new("2026-08-26T17:00:30Z").unwrap();
        let scope = scope(ViaClassificationV1::Normal);
        let source_content_id = ScopedContentIdV1::from_scope_and_bytes(&scope, b"hello").unwrap();
        let request_id = ScopedReferenceV1::new(&scope, id("content-request-1")).unwrap();
        let chunk_id = sha256(b"hello").unwrap();
        let request = NeedContentV1 {
            protocol_version: VIA_PROTOCOL_V1,
            request_id: request_id.clone(),
            source_content_id: source_content_id.clone(),
            chunk_ids: vec![chunk_id.clone()],
            purpose: NeedContentPurposeV1::ContextPlanning,
            max_bytes: 5,
            max_tokens: 5,
            expires_at: expires_at.clone(),
            policy_version: SemanticVersion::new("1.0.0").unwrap(),
        };
        request.validate_at(&now).unwrap();
        let mut invalid_request = request.clone();
        invalid_request.chunk_ids = vec![chunk_id.clone(); 2];
        assert!(invalid_request.validate_at(&now).is_err());
        invalid_request = request.clone();
        invalid_request.max_bytes = MAX_VIA_CONTENT_REQUEST_BYTES + 1;
        assert!(invalid_request.validate_at(&now).is_err());
        invalid_request = request.clone();
        invalid_request.expires_at = UtcTimestamp::new("2026-08-26T17:00:31Z").unwrap();
        assert!(invalid_request.validate_at(&now).is_err());
        assert!(request.validate_at(&expires_at).is_err());

        let chunk = ContentChunkV1 {
            protocol_version: VIA_PROTOCOL_V1,
            request_id: request_id.clone(),
            source_content_id: source_content_id.clone(),
            chunk_id: chunk_id.clone(),
            start_byte: 0,
            end_byte: 5,
            byte_count: 5,
            source_version: sha256(b"hello").unwrap(),
            content: "hello".to_owned(),
            payload_digest: chunk_id.clone(),
            expires_at,
        };
        chunk.validate_at(&now).unwrap();
        assert!(!format!("{chunk:?}").contains("hello"));
        let mut over_ttl = chunk.clone();
        over_ttl.expires_at = UtcTimestamp::new("2026-08-26T17:00:31Z").unwrap();
        assert!(over_ttl.validate_at(&now).is_err());
        let mut altered = chunk.clone();
        altered.content = "jello".to_owned();
        assert!(altered.validate_at(&now).is_err());

        let ack = ContentChunkAckV1 {
            protocol_version: VIA_PROTOCOL_V1,
            request_id,
            source_content_id,
            chunk_id,
        };
        ack.validate().unwrap();
        for message in [
            ViaWireMessageV1::NeedContent(request),
            ViaWireMessageV1::ContentChunk(Box::new(chunk)),
            ViaWireMessageV1::ContentChunkAck(ack),
        ] {
            let encoded = message.encode_at(&now).unwrap();
            assert_eq!(
                ViaWireMessageV1::decode_at(&encoded, &now).unwrap(),
                message
            );
        }
    }

    #[test]
    fn negotiation_and_plan_bounds_fail_closed() {
        let hello = ViaProtocolHelloV1 {
            minimum_version: 1,
            maximum_version: 1,
            capabilities: vec![ViaCapabilityV1::EventStream, ViaCapabilityV1::EventStream],
        };
        assert!(hello.validate().is_err());

        let scope = scope(ViaClassificationV1::Normal);
        let expected = content_id(&scope);
        let mut plan = ViaOptimizationPlanV1 {
            protocol_version: VIA_PROTOCOL_V1,
            scope,
            header: ViaPlanHeaderV1 {
                plan_id: id("plan-1"),
                caused_by: id("event-1"),
                policy_id: id("policy:read"),
                policy_version: SemanticVersion::new("1.0.0").unwrap(),
                expected_source_ids: vec![expected.clone()],
                immutable_guard_digest: digest(A),
                operator_id: id("operator:read"),
                operator_version: SemanticVersion::new("1.0.0").unwrap(),
                expires_at: UtcTimestamp::new("2026-08-26T17:05:00Z").unwrap(),
                output_budget_tokens: 0,
            },
            body: ViaPlanBodyV1::Read {
                source_id: expected,
            },
        };
        let now = UtcTimestamp::new("2026-08-26T17:00:00Z").unwrap();
        assert!(plan.validate_at(&now).is_err());
        plan.header.output_budget_tokens = MAX_VIA_OUTPUT_TOKENS + 1;
        assert!(plan.validate_at(&now).is_err());
        plan.header.output_budget_tokens = 1;
        plan.header.expires_at = UtcTimestamp::new("2026-08-26T17:05:01Z").unwrap();
        assert!(plan.validate_at(&now).is_err());
    }

    #[test]
    fn wire_gate_covers_all_v1_variants_and_drops_or_rejects_auth_injection() {
        const CANARIES: &[&str] = &[
            "via-auth-canary-never-wire",
            "via-cookie-canary-never-wire",
            "via-oauth-canary-never-wire",
            "via-refresh-canary-never-wire",
            "via-client-secret-canary-never-wire",
        ];
        let now = UtcTimestamp::new("2026-08-26T17:00:00Z").unwrap();
        let normal_scope = scope(ViaClassificationV1::Normal);
        let source = content_id(&normal_scope);
        let mut messages = vec![ViaWireMessageV1::Hello(ViaProtocolHelloV1 {
            minimum_version: VIA_PROTOCOL_V1,
            maximum_version: VIA_PROTOCOL_V1,
            capabilities: vec![
                ViaCapabilityV1::EventStream,
                ViaCapabilityV1::ContentManifest,
                ViaCapabilityV1::ContentOnDemand,
                ViaCapabilityV1::TypedPlans,
                ViaCapabilityV1::MultiAgentIdentity,
            ],
        })];
        let stream = stream_id(&normal_scope);
        messages.extend([
            ViaWireMessageV1::EventAck(ViaEventAckV1 {
                protocol_version: VIA_PROTOCOL_V1,
                stream_id: stream.clone(),
                acknowledged_seq: 0,
                acknowledged_event_id: id("event-wire-ack"),
                next_seq: 1,
                status: ViaEventAckStatusV1::Stored,
                accepted_event_count: 1,
                graph_nodes: 0,
                graph_edges: 0,
            }),
            ViaWireMessageV1::Backpressure(ViaBackpressureV1 {
                protocol_version: VIA_PROTOCOL_V1,
                stream_id: stream.clone(),
                accepted_window: 0,
                retry_after_ms: 100,
            }),
            ViaWireMessageV1::StreamResume(ViaStreamResumeV1 {
                protocol_version: VIA_PROTOCOL_V1,
                stream_id: stream.clone(),
            }),
            ViaWireMessageV1::StreamCursor(ViaStreamCursorV1 {
                protocol_version: VIA_PROTOCOL_V1,
                stream_id: stream,
                next_seq: 0,
                last_accepted: None,
            }),
        ]);

        for kind in [
            ViaObservedEventKindV1::SessionStarted,
            ViaObservedEventKindV1::SessionEnded,
            ViaObservedEventKindV1::AgentObserved,
            ViaObservedEventKindV1::UserMessageObserved,
            ViaObservedEventKindV1::AssistantMessageObserved,
            ViaObservedEventKindV1::FileObserved,
            ViaObservedEventKindV1::FileReadObserved,
            ViaObservedEventKindV1::FileChanged,
            ViaObservedEventKindV1::SearchExecuted,
            ViaObservedEventKindV1::SearchResultObserved,
            ViaObservedEventKindV1::ShellExecuted,
            ViaObservedEventKindV1::ShellResultObserved,
            ViaObservedEventKindV1::McpToolCalled,
            ViaObservedEventKindV1::McpResultObserved,
            ViaObservedEventKindV1::ProviderRequestPending,
            ViaObservedEventKindV1::ContextDelivered,
            ViaObservedEventKindV1::ContextReused,
            ViaObservedEventKindV1::ContextInvalidated,
        ] {
            assert_observed_kind_is_covered(kind);
            let payload = ViaEventPayloadV1::Observation(ObservationPayloadV1 {
                kind,
                input_ids: vec![source.clone()],
                output_ids: vec![],
                capability_id: None,
            });
            messages.push(ViaWireMessageV1::Event(Box::new(
                ViaEventFrameV1::new(
                    stream_id(&normal_scope),
                    1,
                    id("event-wire-1"),
                    vec![],
                    normal_scope.clone(),
                    payload,
                )
                .unwrap(),
            )));
        }

        for status in [PlanEventStatusV1::Applied, PlanEventStatusV1::Rejected] {
            let payload = ViaEventPayloadV1::Plan(PlanEventPayloadV1 {
                plan_id: id("plan-wire-1"),
                status,
            });
            messages.push(ViaWireMessageV1::Event(Box::new(
                ViaEventFrameV1::new(
                    stream_id(&normal_scope),
                    1,
                    id("event-wire-1"),
                    vec![],
                    normal_scope.clone(),
                    payload,
                )
                .unwrap(),
            )));
        }

        for (classification, kind) in [
            (ViaClassificationV1::Public, ContentKindV1::SourceFile),
            (ViaClassificationV1::Normal, ContentKindV1::Message),
            (ViaClassificationV1::Sensitive, ContentKindV1::SearchResult),
            (ViaClassificationV1::Secret, ContentKindV1::ShellResult),
            (ViaClassificationV1::LocalOnly, ContentKindV1::McpResult),
            (
                ViaClassificationV1::EnterprisePrivate,
                ContentKindV1::InferenceManifest,
            ),
        ] {
            assert_manifest_enums_are_covered(
                classification,
                kind,
                ContentAvailabilityV1::EdgeOnly,
            );
            messages.push(ViaWireMessageV1::ContentManifest(Box::new(valid_manifest(
                scope(classification),
                kind,
                ContentAvailabilityV1::EdgeOnly,
            ))));
        }
        messages.push(ViaWireMessageV1::ContentManifest(Box::new(valid_manifest(
            normal_scope.clone(),
            ContentKindV1::SourceFile,
            ContentAvailabilityV1::TransientRemoteAllowed,
        ))));
        messages.push(ViaWireMessageV1::ContentManifest(Box::new(valid_manifest(
            normal_scope.clone(),
            ContentKindV1::SourceFile,
            ContentAvailabilityV1::DeniedRemote,
        ))));

        for body in [
            ViaPlanBodyV1::Read {
                source_id: source.clone(),
            },
            ViaPlanBodyV1::Search {
                index_id: source.clone(),
            },
            ViaPlanBodyV1::Shell {
                output_id: source.clone(),
            },
            ViaPlanBodyV1::Mcp {
                output_id: source.clone(),
            },
            ViaPlanBodyV1::Reuse {
                source_ids: vec![source.clone()],
            },
            ViaPlanBodyV1::Inference {
                inference_plan: InferencePlanV1 {
                    manifest_id: source.clone(),
                    transitions: vec![InferenceSegmentTransitionV1 {
                        position: 0,
                        old_segment_id: digest(A),
                        old_representation_id: source.clone(),
                        old_guard_hash: source.content_digest.clone(),
                        new_segment_id: digest(
                            "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                        ),
                        new_representation_id: ScopedContentIdV1::from_scope_and_bytes(
                            &normal_scope,
                            b"new",
                        )
                        .unwrap(),
                        new_guard_hash: ScopedContentIdV1::from_scope_and_bytes(
                            &normal_scope,
                            b"new",
                        )
                        .unwrap()
                        .content_digest,
                        old_token_count: 2,
                        new_token_count: 1,
                    }],
                },
            },
        ] {
            messages.push(ViaWireMessageV1::OptimizationPlan(Box::new(valid_plan(
                normal_scope.clone(),
                body,
            ))));
        }
        messages.push(ViaWireMessageV1::PlanRequest(ViaPlanRequestV1 {
            protocol_version: VIA_PROTOCOL_V1,
            stream_id: stream_id(&normal_scope),
            event_id: id("event-wire-1"),
            immutable_guard_digest: digest(A),
            now: now.clone(),
            expires_at: UtcTimestamp::new("2026-08-26T17:00:30Z").unwrap(),
            inference_candidates: None,
        }));

        for message in &messages {
            let encoded = message.encode_at(&now).unwrap();
            assert_eq!(
                ViaWireMessageV1::decode_at(&encoded, &now).unwrap(),
                *message
            );
            let wire = String::from_utf8(encoded).unwrap();
            for canary in CANARIES {
                assert!(!wire.contains(canary));
            }
            for field in FORBIDDEN_AUTH_FIELDS {
                assert!(!wire.contains(&format!("\"{field}\"")));
            }
        }

        for (index, field) in FORBIDDEN_AUTH_FIELDS.iter().enumerate() {
            let canary = format!("via-{index}-credential-canary-never-wire");
            let mut injected = serde_json::to_value(&messages[0]).unwrap();
            injected[*field] = serde_json::json!(&canary);
            let injected = serde_json::to_vec(&injected).unwrap();
            assert!(ViaWireMessageV1::decode_at(&injected, &now).is_err());

            let mut nested = serde_json::to_value(&messages[0]).unwrap();
            nested["message"][*field] = serde_json::json!(&canary);
            let nested = serde_json::to_vec(&nested).unwrap();
            assert!(ViaWireMessageV1::decode_at(&nested, &now).is_err());
        }
    }

    #[test]
    fn representative_wire_values_have_no_provider_authority_fields() {
        fn assert_safe_keys(value: &serde_json::Value) {
            match value {
                serde_json::Value::Object(fields) => {
                    for (key, value) in fields {
                        assert!(!FORBIDDEN_AUTH_FIELDS.contains(&key.as_str()));
                        assert_safe_keys(value);
                    }
                }
                serde_json::Value::Array(values) => {
                    values.iter().for_each(assert_safe_keys);
                }
                _ => {}
            }
        }

        let scope = scope(ViaClassificationV1::Sensitive);
        let payload = ViaEventPayloadV1::Observation(ObservationPayloadV1 {
            kind: ViaObservedEventKindV1::ProviderRequestPending,
            input_ids: vec![content_id(&scope)],
            output_ids: vec![],
            capability_id: Some(id("provider-call")),
        });
        let frame =
            ViaEventFrameV1::new(stream_id(&scope), 1, id("event-1"), vec![], scope, payload)
                .unwrap();
        assert_safe_keys(&serde_json::to_value(frame).unwrap());
    }

    #[test]
    fn event_and_plan_decoders_reject_unknown_authority_fields() {
        let scope = scope(ViaClassificationV1::Normal);
        let payload = ViaEventPayloadV1::Observation(ObservationPayloadV1 {
            kind: ViaObservedEventKindV1::SessionStarted,
            input_ids: vec![],
            output_ids: vec![],
            capability_id: None,
        });
        let frame = ViaEventFrameV1::new(
            stream_id(&scope),
            1,
            id("event-1"),
            vec![],
            scope.clone(),
            payload,
        )
        .unwrap();
        let frame_value = serde_json::to_value(&frame).unwrap();
        assert!(serde_json::from_value::<ViaEventFrameV1>(frame_value.clone()).is_ok());

        let expected = content_id(&scope);
        let plan = ViaOptimizationPlanV1 {
            protocol_version: VIA_PROTOCOL_V1,
            scope,
            header: ViaPlanHeaderV1 {
                plan_id: id("plan-1"),
                caused_by: id("event-1"),
                policy_id: id("policy:read"),
                policy_version: SemanticVersion::new("1.0.0").unwrap(),
                expected_source_ids: vec![expected.clone()],
                immutable_guard_digest: digest(A),
                operator_id: id("operator:read"),
                operator_version: SemanticVersion::new("1.0.0").unwrap(),
                expires_at: UtcTimestamp::new("2026-08-26T17:05:00Z").unwrap(),
                output_budget_tokens: 1_000,
            },
            body: ViaPlanBodyV1::Read {
                source_id: expected,
            },
        };
        let plan_value = serde_json::to_value(&plan).unwrap();
        let decoded_plan = serde_json::from_value::<ViaOptimizationPlanV1>(plan_value.clone());
        assert!(decoded_plan.is_ok(), "{decoded_plan:?}");

        for field in FORBIDDEN_AUTH_FIELDS {
            let mut frame_with_unknown = frame_value.clone();
            frame_with_unknown[*field] = serde_json::json!("sensitive");
            assert!(
                serde_json::from_value::<ViaEventFrameV1>(frame_with_unknown).is_err(),
                "ViaEventFrameV1 must reject top-level {field}"
            );

            let mut plan_with_unknown = plan_value.clone();
            plan_with_unknown[*field] = serde_json::json!("sensitive");
            assert!(
                serde_json::from_value::<ViaOptimizationPlanV1>(plan_with_unknown).is_err(),
                "ViaOptimizationPlanV1 must reject top-level {field}"
            );
        }
    }

    #[test]
    fn inference_manifest_is_deterministic_metadata_only_and_strictly_bound() {
        const PROMPT_CANARY: &[u8] = b"provider-key-and-raw-prompt-canary";
        let scope = scope(ViaClassificationV1::Sensitive);
        let segment = |position, role, tokens| {
            InferenceSegmentV1::from_canonical_bytes(
                &scope,
                position,
                role,
                InferenceMutabilityV1::Immutable,
                PROMPT_CANARY,
                tokens,
                InferenceDeliveryStateV1::Uncached,
                Vec::new(),
            )
            .unwrap()
        };
        let first = InferenceManifestV1::new(
            scope.clone(),
            vec![
                segment(0, InferenceSegmentRoleV1::System, 4),
                segment(1, InferenceSegmentRoleV1::User, 7),
            ],
            None,
        )
        .unwrap();
        let second = InferenceManifestV1::new(
            scope.clone(),
            vec![
                segment(0, InferenceSegmentRoleV1::System, 4),
                segment(1, InferenceSegmentRoleV1::User, 7),
            ],
            None,
        )
        .unwrap();
        assert_eq!(first, second);
        assert_eq!(first.accounting.total_tokens, 11);
        let encoded = serde_json::to_vec(&first).unwrap();
        assert!(
            !encoded
                .windows(PROMPT_CANARY.len())
                .any(|window| window == PROMPT_CANARY)
        );

        let mut tampered = first.clone();
        tampered.segments[1].position = 0;
        assert!(tampered.validate().is_err());
        let mut tampered = first.clone();
        tampered.segments[0].guard_hash = digest(A);
        assert!(tampered.validate().is_err());
        let mut tampered = first.clone();
        tampered.accounting.total_tokens += 1;
        assert!(tampered.validate().is_err());
        let mut unknown = serde_json::to_value(first).unwrap();
        unknown["provider_api_key"] = serde_json::json!("credential");
        assert!(serde_json::from_value::<InferenceManifestV1>(unknown).is_err());
    }

    #[test]
    fn inference_manifest_bounds_and_overflow_fail_closed() {
        let scope = scope(ViaClassificationV1::Normal);
        assert!(InferenceManifestV1::new(scope.clone(), Vec::new(), None).is_err());
        let lineage = (0..=MAX_VIA_INFERENCE_LINEAGE)
            .map(|index| {
                ScopedContentIdV1::from_scope_and_bytes(&scope, index.to_string().as_bytes())
                    .unwrap()
            })
            .collect();
        assert!(
            InferenceSegmentV1::from_canonical_bytes(
                &scope,
                0,
                InferenceSegmentRoleV1::User,
                InferenceMutabilityV1::Immutable,
                b"bounded",
                1,
                InferenceDeliveryStateV1::Uncached,
                lineage,
            )
            .is_err()
        );
        let maximum = InferenceSegmentV1::from_canonical_bytes(
            &scope,
            0,
            InferenceSegmentRoleV1::System,
            InferenceMutabilityV1::Immutable,
            b"maximum",
            u64::MAX,
            InferenceDeliveryStateV1::Uncached,
            Vec::new(),
        )
        .unwrap();
        let overflow = InferenceSegmentV1::from_canonical_bytes(
            &scope,
            1,
            InferenceSegmentRoleV1::User,
            InferenceMutabilityV1::Immutable,
            b"overflow",
            1,
            InferenceDeliveryStateV1::Uncached,
            Vec::new(),
        )
        .unwrap();
        assert!(InferenceManifestV1::new(scope, vec![maximum, overflow], None).is_err());
    }

    fn inference_plan_fixture() -> ViaOptimizationPlanV1 {
        let scope = ViaScopeV1 {
            protocol_version: VIA_PROTOCOL_V1,
            event_or_request_id: id("request-inference-plan"),
            causality_id: None,
            user_id: None,
            device_id: None,
            project_id: None,
            workspace_id: None,
            session_id: None,
            conversation_id: None,
            agent_id: None,
            team_id: None,
            organization_id: None,
            classification: ViaClassificationV1::Normal,
        };
        let scope_digest = scope.digest().unwrap();
        let scoped = |content_digest| ScopedContentIdV1 {
            scope_digest: scope_digest.clone(),
            content_digest,
        };
        let manifest_id = scoped(digest(B));
        ViaOptimizationPlanV1 {
            protocol_version: VIA_PROTOCOL_V1,
            scope,
            header: ViaPlanHeaderV1 {
                plan_id: id("plan-inference-1"),
                caused_by: id("event-inference-pending"),
                policy_id: id("via.inference-planning"),
                policy_version: SemanticVersion::new("1.0.0").unwrap(),
                expected_source_ids: vec![manifest_id.clone()],
                immutable_guard_digest: digest(A),
                operator_id: id("leanctx.inference.history.compact"),
                operator_version: SemanticVersion::new("1.0.0").unwrap(),
                expires_at: UtcTimestamp::new("2026-08-26T17:00:30Z").unwrap(),
                output_budget_tokens: 8,
            },
            body: ViaPlanBodyV1::Inference {
                inference_plan: InferencePlanV1 {
                    manifest_id: manifest_id.clone(),
                    transitions: vec![InferenceSegmentTransitionV1 {
                        position: 3,
                        old_segment_id: digest(A),
                        old_representation_id: manifest_id,
                        old_guard_hash: digest(B),
                        new_segment_id: digest(
                            "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                        ),
                        new_representation_id: scoped(digest(
                            "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
                        )),
                        new_guard_hash: digest(
                            "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
                        ),
                        old_token_count: 10,
                        new_token_count: 5,
                    }],
                },
            },
        }
    }

    #[test]
    #[allow(clippy::useless_concat)]
    fn inference_plan_wire_is_canonical_metadata_only_and_strict() {
        const FIXTURE: &str = concat!(
            r#"{"protocol_version":1,"scope":{"protocol_version":1,"event_or_request_id":"request-inference-plan","classification":"normal"},"header":{"plan_id":"plan-inference-1","caused_by":"event-inference-pending","policy_id":"via.inference-planning","policy_version":"1.0.0","expected_source_ids":[{"scope_digest":"sha256:4c6e93595932ddbe4418c020f30fe5fd321c4567ac17f5703644125647ccdcd0","content_digest":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}],"immutable_guard_digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","operator_id":"leanctx.inference.history.compact","operator_version":"1.0.0","expires_at":"2026-08-26T17:00:30Z","output_budget_tokens":8},"plan_type":"inference","inference_plan":{"manifest_id":{"scope_digest":"sha256:4c6e93595932ddbe4418c020f30fe5fd321c4567ac17f5703644125647ccdcd0","content_digest":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},"transitions":[{"position":3,"old_segment_id":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","old_representation_id":{"scope_digest":"sha256:4c6e93595932ddbe4418c020f30fe5fd321c4567ac17f5703644125647ccdcd0","content_digest":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},"old_guard_hash":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","new_segment_id":"sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","new_representation_id":{"scope_digest":"sha256:4c6e93595932ddbe4418c020f30fe5fd321c4567ac17f5703644125647ccdcd0","content_digest":"sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"},"new_guard_hash":"sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","old_token_count":10,"new_token_count":5}]}}"#,
        );
        let now = UtcTimestamp::new("2026-08-26T17:00:00Z").unwrap();
        let plan = inference_plan_fixture();
        plan.validate_at(&now).unwrap();
        assert_eq!(serde_json::to_string(&plan).unwrap(), FIXTURE);
        assert_eq!(
            serde_json::from_str::<ViaOptimizationPlanV1>(FIXTURE).unwrap(),
            plan
        );
        for canary in ["provider-key", "raw-prompt", "authorization", "cookie"] {
            assert!(!FIXTURE.contains(canary));
        }
    }

    #[test]
    fn inference_plan_bounds_and_hostile_shapes_fail_closed() {
        let now = UtcTimestamp::new("2026-08-26T17:00:00Z").unwrap();
        let valid = inference_plan_fixture();

        let mutate = |mut plan: ViaOptimizationPlanV1, change: fn(&mut InferencePlanV1)| {
            let ViaPlanBodyV1::Inference { inference_plan } = &mut plan.body else {
                unreachable!()
            };
            change(inference_plan);
            assert!(plan.validate_at(&now).is_err());
        };
        mutate(valid.clone(), |body| body.transitions.clear());
        mutate(valid.clone(), |body| {
            body.transitions[0].new_token_count = body.transitions[0].old_token_count;
        });
        mutate(valid.clone(), |body| {
            body.transitions.push(body.transitions[0].clone());
        });
        mutate(valid.clone(), |body| {
            body.transitions[0].new_guard_hash = digest(A);
        });

        let mut unknown = serde_json::to_value(valid).unwrap();
        unknown["inference_plan"]["raw_provider_request"] =
            serde_json::json!("provider-key-and-prompt-canary");
        assert!(serde_json::from_value::<ViaOptimizationPlanV1>(unknown).is_err());
        let encoded = serde_json::to_string(&inference_plan_fixture()).unwrap();
        let duplicate = encoded.replacen(
            "\"plan_type\":\"inference\"",
            "\"plan_type\":\"inference\",\"plan_type\":\"inference\"",
            1,
        );
        assert!(serde_json::from_str::<ViaOptimizationPlanV1>(&duplicate).is_err());
    }

    fn inference_candidate_set_fixture(scope: &ViaScopeV1) -> InferenceCandidateSetV1 {
        let scope_digest = scope.digest().unwrap();
        let scoped = |content_digest| ScopedContentIdV1 {
            scope_digest: scope_digest.clone(),
            content_digest,
        };
        let mut candidates = InferenceCandidateSetV1 {
            candidate_set_id: scoped(digest(A)),
            manifest_id: scoped(digest(B)),
            operator_id: id("leanctx.inference.history.compact"),
            operator_version: SemanticVersion::new("1.0.0").unwrap(),
            transitions: vec![InferenceSegmentTransitionV1 {
                position: 4,
                old_segment_id: digest(A),
                old_representation_id: scoped(digest(B)),
                old_guard_hash: digest(B),
                new_segment_id: digest(C),
                new_representation_id: scoped(digest(C)),
                new_guard_hash: digest(C),
                old_token_count: 10,
                new_token_count: 5,
            }],
        };
        candidates.candidate_set_id = candidates.expected_id(&scope_digest).unwrap();
        candidates
    }

    #[test]
    fn inference_candidate_set_wire_is_exact_metadata_only_and_strict() {
        let scope = scope(ViaClassificationV1::Normal);
        let candidates = inference_candidate_set_fixture(&scope);
        candidates
            .validate_for_scope(&scope.digest().unwrap())
            .unwrap();
        assert_eq!(
            candidates.candidate_set_id.content_digest.as_str(),
            "sha256:06a8f76a1d6118e7508364a6b5f72bebcde7a851d07a23c49e2ef0108f35fda9"
        );
        let value = serde_json::to_value(&candidates).unwrap();
        assert_eq!(
            value.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec![
                "candidate_set_id",
                "manifest_id",
                "operator_id",
                "operator_version",
                "transitions"
            ]
        );
        let request = ViaPlanRequestV1 {
            protocol_version: VIA_PROTOCOL_V1,
            stream_id: stream_id(&scope),
            event_id: id("event-candidates"),
            immutable_guard_digest: digest(A),
            now: UtcTimestamp::new("2026-08-26T17:00:00Z").unwrap(),
            expires_at: UtcTimestamp::new("2026-08-26T17:00:30Z").unwrap(),
            inference_candidates: Some(candidates.clone()),
        };
        request.validate().unwrap();
        let encoded = serde_json::to_string(&request).unwrap();
        const REQUEST_FIXTURE: &str = r#"{"protocol_version":1,"stream_id":{"scope_digest":"sha256:261c81d042ed5b69e3ff4d24e632714d700d39ca9cd6e1295565c12a15d7124d","reference":"stream-1"},"event_id":"event-candidates","immutable_guard_digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","now":"2026-08-26T17:00:00Z","expires_at":"2026-08-26T17:00:30Z","inference_candidates":{"candidate_set_id":{"scope_digest":"sha256:261c81d042ed5b69e3ff4d24e632714d700d39ca9cd6e1295565c12a15d7124d","content_digest":"sha256:06a8f76a1d6118e7508364a6b5f72bebcde7a851d07a23c49e2ef0108f35fda9"},"manifest_id":{"scope_digest":"sha256:261c81d042ed5b69e3ff4d24e632714d700d39ca9cd6e1295565c12a15d7124d","content_digest":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},"operator_id":"leanctx.inference.history.compact","operator_version":"1.0.0","transitions":[{"position":4,"old_segment_id":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","old_representation_id":{"scope_digest":"sha256:261c81d042ed5b69e3ff4d24e632714d700d39ca9cd6e1295565c12a15d7124d","content_digest":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},"old_guard_hash":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","new_segment_id":"sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","new_representation_id":{"scope_digest":"sha256:261c81d042ed5b69e3ff4d24e632714d700d39ca9cd6e1295565c12a15d7124d","content_digest":"sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"},"new_guard_hash":"sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","old_token_count":10,"new_token_count":5}]}}"#;
        assert_eq!(encoded, REQUEST_FIXTURE);
        assert_eq!(
            serde_json::from_str::<ViaPlanRequestV1>(&encoded).unwrap(),
            request
        );
        for canary in ["provider-key", "raw-prompt", "authorization", "cookie"] {
            assert!(!encoded.contains(canary));
        }
        let mut unknown = serde_json::to_value(&candidates).unwrap();
        unknown["raw_prompt"] = serde_json::json!("prompt-canary");
        assert!(serde_json::from_value::<InferenceCandidateSetV1>(unknown).is_err());
        let duplicate = serde_json::to_string(&candidates).unwrap().replacen(
            "\"operator_id\":\"leanctx.inference.history.compact\"",
            "\"operator_id\":\"leanctx.inference.history.compact\",\"operator_id\":\"leanctx.inference.history.compact\"",
            1,
        );
        assert!(serde_json::from_str::<InferenceCandidateSetV1>(&duplicate).is_err());
    }

    #[test]
    fn inference_candidate_set_limits_hash_and_scope_reject_hostile() {
        let scope = scope(ViaClassificationV1::Normal);
        let valid = inference_candidate_set_fixture(&scope);
        let scope_digest = scope.digest().unwrap();
        let mut wrong_scope = valid.clone();
        wrong_scope.manifest_id.scope_digest = digest(A);
        assert!(wrong_scope.validate_for_scope(&scope_digest).is_err());
        let mut wrong_request_scope = ViaPlanRequestV1 {
            protocol_version: VIA_PROTOCOL_V1,
            stream_id: stream_id(&scope),
            event_id: id("event-candidates"),
            immutable_guard_digest: digest(A),
            now: UtcTimestamp::new("2026-08-26T17:00:00Z").unwrap(),
            expires_at: UtcTimestamp::new("2026-08-26T17:00:30Z").unwrap(),
            inference_candidates: Some(valid.clone()),
        };
        wrong_request_scope.stream_id.scope_digest = digest(A);
        assert!(wrong_request_scope.validate().is_err());

        let mut wrong_operator = valid.clone();
        wrong_operator.operator_id = id("operator:unknown");
        assert!(wrong_operator.validate_for_scope(&scope_digest).is_err());

        let mut wrong_hash = valid.clone();
        wrong_hash.transitions[0].new_guard_hash = digest(A);
        assert!(wrong_hash.validate_for_scope(&scope_digest).is_err());

        let mut unsorted = valid.clone();
        unsorted.transitions.push(valid.transitions[0].clone());
        assert!(unsorted.validate_for_scope(&scope_digest).is_err());

        let mut maximum = valid.clone();
        maximum.transitions = (0..MAX_VIA_INFERENCE_CANDIDATES)
            .map(|position| {
                let mut transition = maximum.transitions[0].clone();
                transition.position = position as u16;
                transition.old_segment_id = digest(&format!("sha256:{position:064x}"));
                transition.new_segment_id = digest(&format!("sha256:{:064x}", position + 1));
                transition
            })
            .collect();
        maximum.candidate_set_id = maximum.expected_id(&scope_digest).unwrap();
        maximum.validate_for_scope(&scope_digest).unwrap();
        let now = UtcTimestamp::new("2026-08-26T17:00:00Z").unwrap();
        let request = ViaPlanRequestV1 {
            protocol_version: VIA_PROTOCOL_V1,
            stream_id: stream_id(&scope),
            event_id: id("event-candidates-maximum"),
            immutable_guard_digest: digest(A),
            now: now.clone(),
            expires_at: UtcTimestamp::new("2026-08-26T17:00:30Z").unwrap(),
            inference_candidates: Some(maximum),
        };
        let encoded = ViaWireMessageV1::PlanRequest(request)
            .encode_at(&now)
            .unwrap();
        assert!(encoded.len() <= MAX_VIA_PLAN_REQUEST_BYTES);
        let mut over_byte_cap = encoded;
        over_byte_cap.resize(MAX_VIA_PLAN_REQUEST_BYTES + 1, b' ');
        assert!(ViaWireMessageV1::decode_at(&over_byte_cap, &now).is_err());

        let mut oversized = valid;
        oversized.transitions = (0..MAX_VIA_INFERENCE_CANDIDATES + 1)
            .map(|position| {
                let mut transition = oversized.transitions[0].clone();
                transition.position = position as u16;
                transition.old_segment_id = digest(&format!("sha256:{position:064x}"));
                transition.new_segment_id = digest(&format!("sha256:{:064x}", position + 1));
                transition
            })
            .collect();
        assert!(oversized.validate_for_scope(&scope_digest).is_err());
    }
}
