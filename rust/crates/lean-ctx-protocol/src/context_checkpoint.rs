// SPDX-License-Identifier: Apache-2.0

//! Canonical typed live-state checkpoint domain (`ContextCheckpointV1`).
//!
//! A `ContextCheckpointV1` is the portable semantic state needed to *continue*
//! work: the active task, its progress, the decisions taken, the files in
//! scope, the pinned policies and packages, and the lineage refs that bind the
//! checkpoint to its project, workspace, plan, and receipts.  It is explicitly
//! not a Session Bundle (bounded session transfer), a Handoff Bundle
//! (point-to-point handoff), a `.ctxpkg` (installable asset), or a Context
//! Snapshot (signed point-in-time evidence).
//!
//! The domain carries **no** machine-local or process-local state: absolute
//! paths, credentials, API keys, provider cache handles, PIDs, locks, temp
//! directories, and live-zone payloads are rejected at construction time, not
//! merely discouraged.  Every bounded string and every collection has an
//! explicit cap, every identity is validated, and every public constructor is
//! fallible so that invalid state can never reach a digest or a signature
//! through an infallible API.
//!
//! Projections, merge, migration, signing, and trust evaluation are downstream
//! slices.  They are represented here only by stable typed seams
//! ([`ContextCheckpointProjectionV1`], [`ContextCheckpointMergePolicyV1`],
//! [`ContextCheckpointMigrationV1`], [`ContextCheckpointSignerV1`]) plus the
//! sealed [`ContextCheckpointSigningPayloadV1`] and
//! [`ContextCheckpointDigestInputsV1`] carriers those slices consume.

use serde::{Deserialize, Deserializer, Serialize, de::Error as DeError};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    ContextSessionStateV1, DecisionId, PackageId, PlanId, PolicyId, ProfileId, ProjectId,
    ProtocolReference, ReceiptId, SemanticVersion, SessionIdentityV1, Sha256Digest, SourceId,
    TaskId, TenantId, UtcTimestamp, ValidationError, WorkspaceId, deserialize_schema_version,
    validate_milliunit, validate_schema_version,
};

/// Schema identity of the canonical live-state checkpoint contract.
pub const CONTEXT_CHECKPOINT_SCHEMA_ID: &str = "leanctx.context-checkpoint/v1";

/// Schema identity of the additive artifact-lineage checkpoint contract.
///
/// This is deliberately a distinct wire contract.  `ContextCheckpointV1`
/// remains byte-for-byte compatible with deployed deny-unknown readers; a
/// checkpoint carrying authoritative artifact references is a V2 value.
pub const CONTEXT_CHECKPOINT_V2_SCHEMA_ID: &str = "leanctx.context-checkpoint-live/v2";

/// Domain prefix for the whole-checkpoint content digest.
pub const CONTEXT_CHECKPOINT_DIGEST_DOMAIN: &[u8] = b"leanctx/context-checkpoint/v1\0";

/// Domain prefix for the checkpoint identity digest.
pub const CONTEXT_CHECKPOINT_IDENTITY_DIGEST_DOMAIN: &[u8] =
    b"leanctx/context-checkpoint-identity/v1\0";

/// Domain prefix for the checkpoint lineage digest.
pub const CONTEXT_CHECKPOINT_LINEAGE_DIGEST_DOMAIN: &[u8] =
    b"leanctx/context-checkpoint-lineage/v1\0";

/// Domain prefix for the checkpoint live-state digest.
pub const CONTEXT_CHECKPOINT_LIVE_STATE_DIGEST_DOMAIN: &[u8] =
    b"leanctx/context-checkpoint-live-state/v1\0";

/// Domain prefix covered by a downstream checkpoint signature.
pub const CONTEXT_CHECKPOINT_SIGNATURE_DOMAIN: &[u8] = b"leanctx/context-checkpoint-signature/v1\0";

/// Domain prefix for the V2 checkpoint content digest.
pub const CONTEXT_CHECKPOINT_V2_DIGEST_DOMAIN: &[u8] = b"leanctx/context-checkpoint/v2\0";

/// Domain prefix for the V2 checkpoint lineage digest.
pub const CONTEXT_CHECKPOINT_V2_LINEAGE_DIGEST_DOMAIN: &[u8] =
    b"leanctx/context-checkpoint-lineage/v2\0";

/// Domain prefix covered by a V2 checkpoint signature.
pub const CONTEXT_CHECKPOINT_V2_SIGNATURE_DOMAIN: &[u8] =
    b"leanctx/context-checkpoint-signature/v2\0";

/// Schema identity of the P6 `.ctxpkg` checkpoint carrier envelope.
pub const CONTEXT_CHECKPOINT_CARRIER_SCHEMA_ID: &str = "leanctx.context-checkpoint/v2";

/// Schema identity of the P6 carrier `logical_state` object.
pub const CONTEXT_CHECKPOINT_CARRIER_LOGICAL_STATE_SCHEMA_ID: &str = "leanctx.workspace.state/v1";

/// Exact key set of the P6 carrier `logical_state` object, in canonical order.
///
/// Downstream projection slices must emit exactly these keys; the constant
/// exists so the projection cannot silently drift from the carrier verifier.
pub const CONTEXT_CHECKPOINT_CARRIER_LOGICAL_STATE_KEYS: [&str; 7] = [
    "entries",
    "package_lock_digest",
    "package_pins",
    "policy",
    "schema_version",
    "sources",
    "workspace_id",
];

/// Digest domain string used by the P6 carrier for `state_digest`.
pub const CONTEXT_CHECKPOINT_CARRIER_STATE_DIGEST_DOMAIN: &str = "leanctx.workspace.state.v1";

/// Digest domain string used by the P6 carrier for `envelope_digest`.
pub const CONTEXT_CHECKPOINT_CARRIER_ENVELOPE_DIGEST_DOMAIN: &str =
    "leanctx.checkpoint.envelope.v2";

/// Maximum byte length of a bounded checkpoint free-text value.
pub const MAX_CONTEXT_CHECKPOINT_TEXT_BYTES: usize = 1_024;

/// Maximum encoded checkpoint size accepted before JSON parsing/allocation.
pub const MAX_CONTEXT_CHECKPOINT_ENCODED_BYTES: usize = 1_048_576;

/// Maximum byte length of a bounded checkpoint identity slug.
pub const MAX_CONTEXT_CHECKPOINT_SLUG_BYTES: usize = 128;

/// Maximum byte length of an existing protocol identity admitted by a checkpoint.
pub const MAX_CONTEXT_CHECKPOINT_ID_BYTES: usize = 256;

/// Maximum byte length of a scheme-prefixed checkpoint reference.
pub const MAX_CONTEXT_CHECKPOINT_REFERENCE_BYTES: usize = 512;

/// Maximum decisions carried by one checkpoint.
pub const MAX_CONTEXT_CHECKPOINT_DECISIONS: usize = 64;

/// Maximum evidence refs carried by one decision.
pub const MAX_CONTEXT_CHECKPOINT_DECISION_REFS: usize = 16;

/// Maximum findings carried by one checkpoint.
pub const MAX_CONTEXT_CHECKPOINT_FINDINGS: usize = 64;

/// Maximum next steps carried by one checkpoint.
pub const MAX_CONTEXT_CHECKPOINT_NEXT_STEPS: usize = 64;

/// Maximum file entries carried by one checkpoint.
pub const MAX_CONTEXT_CHECKPOINT_FILES: usize = 256;

/// Maximum policy pins carried by one checkpoint.
pub const MAX_CONTEXT_CHECKPOINT_POLICY_PINS: usize = 64;

/// Maximum package pins carried by one checkpoint.
pub const MAX_CONTEXT_CHECKPOINT_PACKAGE_PINS: usize = 128;

/// Maximum entries in any one checkpoint lineage reference list.
pub const MAX_CONTEXT_CHECKPOINT_LINEAGE_REFS: usize = 64;

/// Maximum step count a checkpoint may report as progress.
pub const MAX_CONTEXT_CHECKPOINT_STEPS: u32 = 4_096;

/// Strong credential or process-local assignment markers.
///
/// These deliberately require value-bearing syntax.  Words such as “secret”,
/// “lock”, or “provider” are valid task vocabulary and must not make otherwise
/// portable semantic state impossible to checkpoint.
const REJECTED_ASSIGNMENTS: [&str; 19] = [
    "-----begin",
    "pid=",
    "process_id=",
    "lock_id=",
    "provider_handle=",
    "cache_handle=",
    "temp_dir=",
    "tmpdir=",
    "flush_timestamp=",
    "authorization: bearer ",
    "bearer ",
    "password=",
    "password:",
    "passwd=",
    "api_key=",
    "access_key=",
    "aws_secret=",
    "client_secret=",
    "token=",
];

/// Token prefixes that mark well-known credential shapes.
///
/// These are matched per whitespace-delimited token so that ordinary words
/// which merely contain the prefix (for example `risk-based` for `sk-`) are
/// not rejected.
const REJECTED_TOKEN_PREFIXES: [&str; 12] = [
    "base64:",
    "ghp_",
    "gho_",
    "ghr_",
    "ghs_",
    "ghu_",
    "github_pat_",
    "hex:",
    "sk-",
    "xoxa-",
    "xoxb-",
    "xoxp-",
];

macro_rules! checkpoint_string {
    ($(#[$meta:meta])* $name:ident, $validate:path) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Construct the value after applying every wire invariant.
            pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
                let value = value.into();
                $validate(&value, stringify!($name))?;
                Ok(Self(value))
            }

            /// Borrow the validated wire value.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Consume the value and return its wire representation.
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl std::str::FromStr for $name {
            type Err = ValidationError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = ValidationError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = ValidationError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(DeError::custom)
            }
        }
    };
}

macro_rules! validated_deserialize {
    ($target:ident { $($field:ident : $field_type:ty),+ $(,)? }) => {
        impl<'de> Deserialize<'de> for $target {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Wire {
                    $($field: $field_type),+
                }

                let Wire { $($field),+ } = Wire::deserialize(deserializer)?;
                let value = Self { $($field),+ };
                value.validate().map_err(DeError::custom)?;
                Ok(value)
            }
        }
    };
}

checkpoint_string!(
    /// Canonical lowercase hyphenated UUID identifying one checkpoint.
    ContextCheckpointIdV1,
    validate_canonical_uuid
);

checkpoint_string!(
    /// Bounded slug naming the checkpoint branch a device writes to.
    ContextCheckpointBranchIdV1,
    validate_checkpoint_slug
);

checkpoint_string!(
    /// Bounded slug naming the device that produced a checkpoint.
    ContextCheckpointDeviceIdV1,
    validate_checkpoint_slug
);

checkpoint_string!(
    /// Bounded single-line free text carried by the live state.
    ContextCheckpointTextV1,
    validate_checkpoint_text
);

mod validation;
use validation::*;
mod v1;
pub use v1::*;
mod v2;
pub use v2::*;
#[cfg(test)]
mod tests;
pub(super) use validation::{
    canonical_bytes_of, digest_with_domain, is_unicode_format_character,
    reject_machine_local_content, require_capacity, require_sorted_unique, require_unique,
    validate_checkpoint_identifier, validate_checkpoint_reference,
};
