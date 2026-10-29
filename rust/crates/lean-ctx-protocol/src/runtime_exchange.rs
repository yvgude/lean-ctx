// SPDX-License-Identifier: Apache-2.0
//! Strict payloads for the optional local Intelligence peer, not a second Engine.
//! Transport authenticates exact bytes before decoding and owns replay state.
//! Only explicitly policy-approved projections belong here; never log payloads.

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    EngineInvocationV1, EngineObservationV1, EnginePolicyDecisionV1, Sha256Digest, ValidationError,
    receipt_document::digest_bytes, validate_bounded_opaque_identifier,
};

/// Independent successor to the legacy, one-way runtime notification.
pub const RUNTIME_EXCHANGE_VERSION: &str = "leanctx.runtime-exchange/v1";
/// Encoded JSON limit; transport must check the length prefix before allocation.
pub const MAX_RUNTIME_EXCHANGE_BYTES: usize = 1_048_576;
/// A peer cannot extend an invocation beyond this host-admitted time budget.
pub const MAX_RUNTIME_DEADLINE_MS: u64 = 60_000;

/// The size ceilings of one exchange: input and output content, and the
/// encoded request and response that carry them.
///
/// There are exactly the profiles named here; none can be built from numbers.
/// Each side selects one from what it admitted locally — the host from the
/// authenticated package and the exact operation, the peer from its protected
/// bootstrap and a live entitlement — and keeps it for the session. Nothing
/// in a message selects or widens a profile. Every function without a profile
/// argument uses [`Self::ORDINARY`], exactly as before profiles existed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeExchangeProfile {
    input: usize,
    output: usize,
    request: usize,
    response: usize,
}

/// Allowance for the typed metadata around the bulk of a verifier exchange.
const VERIFIER_METADATA_BYTES: usize = 16 * 1024;

const fn base64_bytes(bytes: usize) -> usize {
    bytes.div_ceil(3) * 4
}

/// The parts of one shared-artifact reader exchange, as decoded bytes. The
/// first three are the bounds of the artifact wire contract (manifest, body
/// envelope, room around an authorization); a part larger than its bound is
/// refused by both sides, never shortened.
pub const MAX_ARTIFACT_READER_MANIFEST_BYTES: usize = 64 * 1024;
pub const MAX_ARTIFACT_READER_BODY_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_ARTIFACT_READER_AUTHORIZATION_BYTES: usize = 16 * 1024;
/// Allowance for the typed fields around those parts.
pub const ARTIFACT_READER_METADATA_BYTES: usize = 16 * 1024;
/// Most source files one artifact may be derived from, and the longest path.
pub const MAX_ARTIFACT_READER_SOURCE_FILES: usize = 4096;
pub const MAX_ARTIFACT_READER_SOURCE_PATH_BYTES: usize = 1024;
/// Width of one serialized byte in the widest admitted JSON encoding.
const JSON_ESCAPE_WIDTH: usize = 6;
/// A `sha256:<64 hex>` digest.
const DIGEST_TEXT_BYTES: usize = 71;
/// The reader's captured checkout state as typed JSON: per file its escaped
/// path, its digest and the field names and punctuation around them.
pub const MAX_ARTIFACT_READER_LOCAL_BYTES: usize = MAX_ARTIFACT_READER_SOURCE_FILES
    * (JSON_ESCAPE_WIDTH * MAX_ARTIFACT_READER_SOURCE_PATH_BYTES
        + JSON_ESCAPE_WIDTH * DIGEST_TEXT_BYTES
        + 128)
    + ARTIFACT_READER_METADATA_BYTES;
/// Largest input of one reader step: the three byte parts in base64 at the
/// widest serialization, the local state and the fields around them. These
/// are the adapter's own widths (`ee/intelligence/src/artifact_reader.rs`),
/// which a build-time equality there holds to this profile.
pub const MAX_ARTIFACT_READER_INPUT_BYTES: usize = JSON_ESCAPE_WIDTH
    * (base64_bytes(MAX_ARTIFACT_READER_BODY_BYTES)
        + base64_bytes(MAX_ARTIFACT_READER_MANIFEST_BYTES)
        + base64_bytes(MAX_ARTIFACT_READER_AUTHORIZATION_BYTES))
    + MAX_ARTIFACT_READER_LOCAL_BYTES
    + ARTIFACT_READER_METADATA_BYTES;
/// Largest answer of one reader step: a decision or the list of source paths
/// to capture. An answer never carries artifact content.
pub const MAX_ARTIFACT_READER_OUTPUT_BYTES: usize = MAX_ARTIFACT_READER_SOURCE_FILES
    * (JSON_ESCAPE_WIDTH * MAX_ARTIFACT_READER_SOURCE_PATH_BYTES + 3)
    + ARTIFACT_READER_METADATA_BYTES;
/// Largest content one shared artifact carries, before it is put into a body.
pub const MAX_ARTIFACT_AUTHOR_CONTENT_BYTES: usize = 1024 * 1024;
/// Largest input of the authoring step: the content in base64 at the widest
/// serialization, the source state it was derived from and the fields
/// around them (`ee/intelligence/src/artifact_author.rs`).
pub const MAX_ARTIFACT_AUTHOR_INPUT_BYTES: usize = JSON_ESCAPE_WIDTH
    * base64_bytes(MAX_ARTIFACT_AUTHOR_CONTENT_BYTES)
    + MAX_ARTIFACT_READER_LOCAL_BYTES
    + ARTIFACT_READER_METADATA_BYTES;
/// Largest answer of the authoring step: the body envelope and the manifest
/// in base64 with the fields around them.
pub const MAX_ARTIFACT_AUTHOR_OUTPUT_BYTES: usize = base64_bytes(MAX_ARTIFACT_READER_BODY_BYTES)
    + base64_bytes(MAX_ARTIFACT_READER_MANIFEST_BYTES)
    + ARTIFACT_READER_METADATA_BYTES;
// The numbers the adapters' author stated; a change of either side alone stops the build.
const _: () = {
    assert!(MAX_ARTIFACT_READER_LOCAL_BYTES == 27_451_392);
    assert!(MAX_ARTIFACT_READER_INPUT_BYTES == 61_677_616);
    assert!(MAX_ARTIFACT_READER_OUTPUT_BYTES == 25_194_496);
    assert!(MAX_ARTIFACT_AUTHOR_INPUT_BYTES == 35_856_400);
    assert!(MAX_ARTIFACT_AUTHOR_OUTPUT_BYTES == 5_696_176);
};

impl RuntimeExchangeProfile {
    /// Every operation unless another profile was admitted for it.
    pub const ORDINARY: Self = Self {
        input: MAX_RUNTIME_EXCHANGE_BYTES,
        output: MAX_RUNTIME_EXCHANGE_BYTES,
        request: MAX_RUNTIME_EXCHANGE_BYTES,
        response: MAX_RUNTIME_EXCHANGE_BYTES,
    };

    /// The organization policy verifier: a signed artifact whose pack may be
    /// JSON-escaped up to sixfold, a trust store and a marker, each base64 in
    /// one input; and an answer that carries the pack once more, escaped
    /// through two JSON layers (at most sevenfold).
    pub const ORG_POLICY_VERIFIER: Self = {
        let pack = crate::org_policy_verifier::MAX_ORG_POLICY_VERIFIED_PACK_BYTES;
        let snapshot = crate::org_policy_verifier::MAX_ORG_REQUIRED_MARKER_BYTES;
        let artifact = 6 * pack + VERIFIER_METADATA_BYTES;
        let input = base64_bytes(artifact) + 2 * base64_bytes(snapshot) + VERIFIER_METADATA_BYTES;
        Self {
            input,
            output: artifact,
            request: input + 2 * VERIFIER_METADATA_BYTES,
            response: 7 * pack + 3 * VERIFIER_METADATA_BYTES,
        }
    };

    /// The shared-artifact reader: a manifest, a body envelope and an
    /// authorization in base64 with the reader's checkout state, and an
    /// answer without content. Input and answer travel as one JSON string
    /// inside the request and the response; the frames add the room for the
    /// quotes of the typed part being escaped once more, and for the
    /// invocation around them.
    pub const ARTIFACT_READER: Self = Self {
        input: MAX_ARTIFACT_READER_INPUT_BYTES,
        output: MAX_ARTIFACT_READER_OUTPUT_BYTES,
        request: MAX_ARTIFACT_READER_INPUT_BYTES
            + MAX_ARTIFACT_READER_LOCAL_BYTES
            + 2 * ARTIFACT_READER_METADATA_BYTES,
        response: 2 * MAX_ARTIFACT_READER_OUTPUT_BYTES + 2 * ARTIFACT_READER_METADATA_BYTES,
    };

    /// The shared-artifact author: content and source state in, manifest and
    /// body out. The frames add the room for the typed part being escaped
    /// once more inside the request and for the answer inside the response.
    pub const ARTIFACT_AUTHOR: Self = Self {
        input: MAX_ARTIFACT_AUTHOR_INPUT_BYTES,
        output: MAX_ARTIFACT_AUTHOR_OUTPUT_BYTES,
        request: MAX_ARTIFACT_AUTHOR_INPUT_BYTES
            + MAX_ARTIFACT_READER_LOCAL_BYTES
            + 2 * ARTIFACT_READER_METADATA_BYTES,
        response: 2 * MAX_ARTIFACT_AUTHOR_OUTPUT_BYTES + 2 * ARTIFACT_READER_METADATA_BYTES,
    };

    /// Largest input content, before encoding.
    #[must_use]
    pub const fn max_input_bytes(self) -> usize {
        self.input
    }

    /// Largest output content, before encoding.
    #[must_use]
    pub const fn max_output_bytes(self) -> usize {
        self.output
    }

    /// Largest encoded request: the plaintext of a request frame.
    #[must_use]
    pub const fn max_request_bytes(self) -> usize {
        self.request
    }

    /// Largest encoded response: the plaintext of a response frame.
    #[must_use]
    pub const fn max_response_bytes(self) -> usize {
        self.response
    }
}

/// One admitted invocation plus its exact bounded input projection.
/// No Debug implementation: the projection may contain private user content.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeRequestV1 {
    pub protocol_version: String,
    pub session_id: String,
    pub request_id: String,
    /// Strictly increasing within the authenticated session; zero is reserved.
    pub sequence: u64,
    pub deadline_unix_ms: u64,
    pub invocation: EngineInvocationV1,
    pub input: String,
    /// The Hive runtime lease (`leanctx.hive.runtime-lease/v1`, standard
    /// base64), sent only to a package that declares lease support. Absent,
    /// the request is byte-identical to one without leases.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_b64: Option<String>,
}

/// Largest encoded runtime lease a request may carry.
pub const MAX_RUNTIME_LEASE_B64_BYTES: usize = 16 * 1024;

fn validate_lease_b64(lease: &str) -> Result<(), ValidationError> {
    let padding = lease.bytes().rev().take_while(|b| *b == b'=').count();
    let body = &lease[..lease.len() - padding];
    if lease.is_empty()
        || lease.len() > MAX_RUNTIME_LEASE_B64_BYTES
        || !lease.len().is_multiple_of(4)
        || padding > 2
        || !body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/'))
    {
        return Err(ValidationError::new(
            "runtime lease must be padded base64 of at most 16 KiB",
        ));
    }
    Ok(())
}

impl RuntimeRequestV1 {
    /// Validate structure and input binding; this does not grant authorization.
    pub fn validate(&self) -> Result<(), ValidationError> {
        self.validate_in(RuntimeExchangeProfile::ORDINARY)
    }

    /// [`Self::validate`] under a locally admitted profile.
    pub fn validate_in(&self, profile: RuntimeExchangeProfile) -> Result<(), ValidationError> {
        validate_version(&self.protocol_version)?;
        validate_bounded_opaque_identifier(&self.session_id, "runtime session_id")?;
        validate_bounded_opaque_identifier(&self.request_id, "runtime request_id")?;
        self.invocation.validate()?;
        if self.sequence == 0 || self.deadline_unix_ms == 0 {
            return Err(ValidationError::new(
                "runtime sequence/deadline must be nonzero",
            ));
        }
        if self.invocation.policy_admission.decision != EnginePolicyDecisionV1::Admitted {
            return Err(ValidationError::new(
                "runtime input requires host policy admission",
            ));
        }
        if let Some(lease) = &self.lease_b64 {
            validate_lease_b64(lease)?;
        }
        validate_content(
            self.input.as_bytes(),
            &self.invocation.input_digest,
            profile.input,
        )
    }

    /// Deterministically check time admission using the receiver's clock.
    /// The transport also enforces a monotonic I/O deadline for the entire call.
    pub fn validate_at(&self, now_unix_ms: u64) -> Result<(), ValidationError> {
        self.validate_at_in(now_unix_ms, RuntimeExchangeProfile::ORDINARY)
    }

    /// [`Self::validate_at`] under a locally admitted profile.
    pub fn validate_at_in(
        &self,
        now_unix_ms: u64,
        profile: RuntimeExchangeProfile,
    ) -> Result<(), ValidationError> {
        self.validate_in(profile)?;
        let remaining = self.deadline_unix_ms.saturating_sub(now_unix_ms);
        if remaining == 0 || remaining > MAX_RUNTIME_DEADLINE_MS {
            return Err(ValidationError::new(
                "runtime deadline expired or exceeds budget",
            ));
        }
        Ok(())
    }

    /// Encode the exact stable representation used by response correlation.
    pub fn to_bytes(&self) -> Result<Vec<u8>, ValidationError> {
        self.to_bytes_in(RuntimeExchangeProfile::ORDINARY)
    }

    /// [`Self::to_bytes`] under a locally admitted profile: the same bytes,
    /// refused once they would pass that profile's request ceiling.
    pub fn to_bytes_in(&self, profile: RuntimeExchangeProfile) -> Result<Vec<u8>, ValidationError> {
        self.validate_in(profile)?;
        encode(self, profile.request)
    }

    /// Decode bounded JSON, rejecting unknown fields, duplicates and bad bindings.
    /// Equivalent JSON whitespace/key order is accepted after authentication of
    /// the exact received bytes; decoding is not transport authentication.
    pub fn from_bytes(bytes: &[u8], now_unix_ms: u64) -> Result<Self, ValidationError> {
        Self::from_bytes_in(bytes, now_unix_ms, RuntimeExchangeProfile::ORDINARY)
    }

    /// [`Self::from_bytes`] under a locally admitted profile.
    pub fn from_bytes_in(
        bytes: &[u8],
        now_unix_ms: u64,
        profile: RuntimeExchangeProfile,
    ) -> Result<Self, ValidationError> {
        let request: Self = decode(bytes, profile.request)?;
        request.validate_at_in(now_unix_ms, profile)?;
        Ok(request)
    }

    /// Bind all request fields, not merely a caller-provided invocation ID.
    /// This is a semantic correlation digest of stable re-encoding, not a MAC
    /// of the received representation. Transport authenticates bytes separately.
    pub fn digest(&self) -> Result<Sha256Digest, ValidationError> {
        self.digest_in(RuntimeExchangeProfile::ORDINARY)
    }

    /// [`Self::digest`] under a locally admitted profile. The digest is of
    /// the same bytes whatever the profile; the profile only bounds them.
    pub fn digest_in(
        &self,
        profile: RuntimeExchangeProfile,
    ) -> Result<Sha256Digest, ValidationError> {
        digest_bytes(&self.to_bytes_in(profile)?)
    }
}

/// Peer result bound to the whole request; receipts remain host-owned.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeResponseV1 {
    pub protocol_version: String,
    pub request_digest: Sha256Digest,
    pub observation: EngineObservationV1,
    pub output: Option<String>,
}

impl RuntimeResponseV1 {
    /// Reuse canonical Engine lineage/status validation and check exact content.
    pub fn validate_for(&self, request: &RuntimeRequestV1) -> Result<(), ValidationError> {
        self.validate_for_in(request, RuntimeExchangeProfile::ORDINARY)
    }

    /// [`Self::validate_for`] under a locally admitted profile.
    pub fn validate_for_in(
        &self,
        request: &RuntimeRequestV1,
        profile: RuntimeExchangeProfile,
    ) -> Result<(), ValidationError> {
        validate_version(&self.protocol_version)?;
        if self.request_digest != request.digest_in(profile)? {
            return Err(ValidationError::new(
                "runtime response belongs to another request",
            ));
        }
        self.observation.validate_for(&request.invocation)?;
        if self.observation.receipt_link.is_some() {
            return Err(ValidationError::new(
                "runtime peer cannot issue a host receipt",
            ));
        }
        match (&self.output, &self.observation.output_digest) {
            (Some(output), Some(digest)) => {
                validate_content(output.as_bytes(), digest, profile.output)
            }
            (None, None) => Ok(()),
            _ => Err(ValidationError::new(
                "runtime output and digest must be present together",
            )),
        }
    }

    /// Encode only a result that is bound to the admitted request.
    pub fn to_bytes(&self, request: &RuntimeRequestV1) -> Result<Vec<u8>, ValidationError> {
        self.to_bytes_in(request, RuntimeExchangeProfile::ORDINARY)
    }

    /// [`Self::to_bytes`] under a locally admitted profile.
    pub fn to_bytes_in(
        &self,
        request: &RuntimeRequestV1,
        profile: RuntimeExchangeProfile,
    ) -> Result<Vec<u8>, ValidationError> {
        self.validate_for_in(request, profile)?;
        encode(self, profile.response)
    }

    /// Reject late, mismatched, malformed or oversized replies before use.
    pub fn from_bytes(
        bytes: &[u8],
        request: &RuntimeRequestV1,
        now_unix_ms: u64,
    ) -> Result<Self, ValidationError> {
        Self::from_bytes_in(
            bytes,
            request,
            now_unix_ms,
            RuntimeExchangeProfile::ORDINARY,
        )
    }

    /// [`Self::from_bytes`] under a locally admitted profile.
    pub fn from_bytes_in(
        bytes: &[u8],
        request: &RuntimeRequestV1,
        now_unix_ms: u64,
        profile: RuntimeExchangeProfile,
    ) -> Result<Self, ValidationError> {
        request.validate_at_in(now_unix_ms, profile)?;
        let response: Self = decode(bytes, profile.response)?;
        response.validate_for_in(request, profile)?;
        Ok(response)
    }
}

fn validate_version(version: &str) -> Result<(), ValidationError> {
    if version != RUNTIME_EXCHANGE_VERSION {
        return Err(ValidationError::new("unsupported runtime exchange version"));
    }
    Ok(())
}

fn validate_content(
    bytes: &[u8],
    digest: &Sha256Digest,
    limit: usize,
) -> Result<(), ValidationError> {
    if bytes.len() > limit || digest_bytes(bytes)? != *digest {
        return Err(ValidationError::new(
            "runtime content exceeds limit or differs from digest",
        ));
    }
    Ok(())
}

/// Collects encoded bytes and fails the write that would pass `limit`, so an
/// oversized message is refused while it is produced, not after it was built.
struct Bounded {
    bytes: Vec<u8>,
    limit: usize,
}

impl std::io::Write for Bounded {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if buffer.len() > self.limit - self.bytes.len() {
            return Err(std::io::ErrorKind::FileTooLarge.into());
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn encode(value: &impl Serialize, limit: usize) -> Result<Vec<u8>, ValidationError> {
    let mut bounded = Bounded {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut bounded, value).map_err(|error| {
        ValidationError::new(if error.is_io() {
            "runtime message exceeds encoded limit"
        } else {
            "runtime JSON encoding failed"
        })
    })?;
    Ok(bounded.bytes)
}

fn decode<T: DeserializeOwned>(bytes: &[u8], limit: usize) -> Result<T, ValidationError> {
    if bytes.is_empty() || bytes.len() > limit {
        return Err(ValidationError::new("runtime message length is invalid"));
    }
    serde_json::from_slice(bytes).map_err(|_| ValidationError::new("invalid runtime JSON"))
}

#[cfg(test)]
mod tests;
