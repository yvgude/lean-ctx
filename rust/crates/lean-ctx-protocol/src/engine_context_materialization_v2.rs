// SPDX-License-Identifier: Apache-2.0

//! Opt-in v2 materialization: the unchanged v1 exchange plus a presenter
//! capability.
//!
//! A presenter capability is a bearer secret the Engine hands to the caller
//! that presented the sources. Whoever later shows it is treated as THE
//! HOLDER OF THAT CAPABILITY for those presentations. It is not an agent
//! name, not an attested identity and not an authorization: current source
//! rights are checked again at every use, with or without it.
//!
//! The capability is host-internal. It must never be placed in model context,
//! tool arguments, tool results, logs or receipts, and never sent to a
//! provider. Records name a presenter only by [`EnginePresenterGrantV1::presenter_ref`].

use serde::{Deserialize, Serialize};

use crate::{
    EngineContextSourceMaterializationRequestV1, EngineContextSourceMaterializationResponseV1,
    ProtocolReference, ValidationError,
};

/// Schema id of the v2 materialization exchange.
pub const ENGINE_CONTEXT_MATERIALIZATION_V2_SCHEMA_ID: &str =
    "leanctx.engine-context-materialization/v2";
/// Schema id of the presenter revocation exchange.
pub const ENGINE_PRESENTER_REVOCATION_V1_SCHEMA_ID: &str = "leanctx.engine-presenter-revocation/v1";
/// First Engine release that serves `context-materialize-sources-v2`.
pub const ENGINE_PRESENTER_CAPABILITY_MIN_ENGINE_VERSION: &str = "3.11.0";
/// Longest lifetime a caller may ask for: one day.
pub const MAX_ENGINE_PRESENTER_TTL_SECONDS: u32 = 24 * 60 * 60;
/// Wire prefix of a presenter capability.
pub const ENGINE_PRESENTER_CAPABILITY_PREFIX: &str = "lcp1_";
/// Wire prefix of a content-free presenter reference.
pub const ENGINE_PRESENTER_REF_PREFIX: &str = "presenter:";

const CAPABILITY_HEX_CHARS: usize = 64;
const PRESENTER_REF_HEX_CHARS: usize = 32;

fn lower_hex(text: &str, length: usize) -> bool {
    text.len() == length
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Whether `text` has the wire form of a presenter capability. Form only:
/// says nothing about whether any Engine issued it.
#[must_use]
pub fn is_engine_presenter_capability_form(text: &str) -> bool {
    text.strip_prefix(ENGINE_PRESENTER_CAPABILITY_PREFIX)
        .is_some_and(|hex| lower_hex(hex, CAPABILITY_HEX_CHARS))
}

/// How the caller wants the sources of this request to be presented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case", tag = "mode")]
pub enum EnginePresenterRequestV1 {
    /// Mint a new capability with this lifetime.
    Issue { ttl_seconds: u32 },
    /// Present under a capability the caller already holds. The capability
    /// is never part of this JSON: it reaches the Engine through
    /// `--presenter-capability-file`.
    Renew,
}

/// The v1 materialization request, with the presenter mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineContextSourceMaterializationRequestV2 {
    pub schema_version: u32,
    pub materialization: EngineContextSourceMaterializationRequestV1,
    pub presenter: EnginePresenterRequestV1,
}

impl EngineContextSourceMaterializationRequestV2 {
    /// Validate the envelope and the unchanged v1 payload.
    pub fn validate_payload(&self) -> Result<(), ValidationError> {
        if self.schema_version != 2 {
            return Err(invalid());
        }
        match self.presenter {
            EnginePresenterRequestV1::Issue { ttl_seconds }
                if !(1..=MAX_ENGINE_PRESENTER_TTL_SECONDS).contains(&ttl_seconds) =>
            {
                Err(invalid())
            }
            _ => self.materialization.validate_payload(),
        }
    }
}

/// What the Engine tells the caller about its presenter entry.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnginePresenterGrantV1 {
    /// Content-free reference of the presenter entry: `presenter:` and 32
    /// lowercase hex characters. Derived from a stored verifier, never from
    /// the capability. Safe in records.
    pub presenter_ref: ProtocolReference,
    /// End of the capability's lifetime, Unix seconds.
    pub expires_at_unix: u64,
    /// The capability, for `issue` only. Host-internal secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<String>,
}

impl std::fmt::Debug for EnginePresenterGrantV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EnginePresenterGrantV1")
            .field("presenter_ref", &self.presenter_ref)
            .field("expires_at_unix", &self.expires_at_unix)
            .field(
                "capability",
                &self.capability.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

impl EnginePresenterGrantV1 {
    fn validate(&self) -> Result<(), ValidationError> {
        let reference_ok = self
            .presenter_ref
            .as_str()
            .strip_prefix(ENGINE_PRESENTER_REF_PREFIX)
            .is_some_and(|hex| lower_hex(hex, PRESENTER_REF_HEX_CHARS));
        let capability_ok = self
            .capability
            .as_deref()
            .is_none_or(is_engine_presenter_capability_form);
        if reference_ok && capability_ok {
            Ok(())
        } else {
            Err(invalid())
        }
    }
}

/// The unchanged v1 materialization response, with the presenter grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineContextSourceMaterializationResponseV2 {
    pub schema_version: u32,
    pub materialization: EngineContextSourceMaterializationResponseV1,
    pub presenter: EnginePresenterGrantV1,
}

impl EngineContextSourceMaterializationResponseV2 {
    /// Validate the envelope, the grant's form and the v1 payload. The
    /// capability never appears inside the materialized content.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != 2 {
            return Err(invalid());
        }
        self.presenter.validate()?;
        let leaked = self
            .presenter
            .capability
            .as_deref()
            .is_some_and(|capability| self.materialization.content.contains(capability));
        if leaked {
            return Err(invalid());
        }
        self.materialization.validate()
    }
}

/// Request of `context-presenter-revoke`. The capability to revoke reaches
/// the Engine through `--presenter-capability-file`, never through this JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnginePresenterRevocationRequestV1 {
    pub schema_version: u32,
}

/// Answer of `context-presenter-revoke`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnginePresenterRevocationResponseV1 {
    pub schema_version: u32,
    pub presenter_ref: ProtocolReference,
    pub revoked: bool,
}

fn invalid() -> ValidationError {
    ValidationError::new("invalid Engine materialization v2 envelope")
}
