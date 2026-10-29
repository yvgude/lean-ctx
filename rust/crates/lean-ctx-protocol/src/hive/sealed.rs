// SPDX-License-Identifier: Apache-2.0

//! End-to-end sealed vaults.
//!
//! Each sealed vault has a 32-byte vault key per epoch. Revision documents
//! and draft patches travel as [`SealedBlobV1`] (XChaCha20-Poly1305, AAD from
//! [`sealed_aad`]); the service stores ciphertext only. Vault keys reach an
//! instance wrapped to its X25519 key ([`WrappedKeyV1`]).
//!
//! - `GET /v1/vaults/{vault_id}/sealed/keys?instance={instance_id}[&epoch=n]` → [`SealedKeyV1`]
//! - `POST /v1/vaults/{vault_id}/sealed/grants` [`SealedGrantRequestV1`] → 204
//!   (an instance that holds the key wraps it for a newly approved instance)
//! - 1.5: `GET /v1/vaults/{vault_id}/sealed/pending` → [`SealedPendingV1`]
//!   (approved recipients without a key at `epoch_required`, signed by Hive)
//!
//! Nothing readable leaves the engine for a sealed vault: a draft carries
//! no summary, body or sources in the clear; they travel inside the sealed
//! patch ([`SealedDraftContentV1`]).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::knowledge::{MAX_PATCH_OPS, PatchOpV1};
use super::{require_schema, validate_b64_bounded, validate_b64_len, validate_id, validate_text};
use crate::{UtcTimestamp, ValidationError};

pub const SEALED_KEY_SCHEMA: &str = "leanctx.hive.sealed-key/v1";
pub const SEALED_GRANT_REQUEST_SCHEMA: &str = "leanctx.hive.sealed-grant-request/v1";
pub const SEALED_PENDING_SCHEMA: &str = "leanctx.hive.sealed-pending/v1";
/// Domain of the Hive signature over a [`SealedPendingV1`] (canonical JSON
/// without `signature`, like the key set).
pub const SEALED_PENDING_SIGNATURE_DOMAIN: &str = "leanctx.hive.sealed-pending.v1\n";

/// `content_schema` of a sealed draft patch: the plaintext is a
/// [`SealedDraftContentV1`].
pub const SEALED_PATCH_CONTENT_SCHEMA: &str = "leanctx.hive.sealed-draft/v1";

/// Content cipher of [`SealedBlobV1`].
pub const SEALED_CONTENT_ALG: &str = "xchacha20poly1305";
/// Key-wrapping scheme of [`WrappedKeyV1`]: X25519 ECDH with an ephemeral
/// key, HKDF-SHA256 (salt = ephemeral_pub || recipient_pub, info =
/// `leanctx.hive.sealed-wrap.v1`), then XChaCha20-Poly1305.
pub const SEALED_WRAP_ALG: &str = "x25519-hkdf-sha256-xchacha20poly1305";
/// HKDF info of the key wrap.
pub const SEALED_WRAP_INFO: &str = "leanctx.hive.sealed-wrap.v1";
/// Largest encoded ciphertext of one sealed blob.
pub const MAX_SEALED_CIPHERTEXT_B64: usize = 4 * 1024 * 1024;

/// AAD binding a sealed blob to its place: `vault_id|revision|epoch|schema`.
/// `revision` is the revision a document belongs to, or the base revision of
/// a draft patch; `schema` is the plaintext's schema or format id.
pub fn sealed_aad(vault_id: &str, revision: u64, epoch: u32, schema: &str) -> Vec<u8> {
    format!("{vault_id}|{revision}|{epoch}|{schema}").into_bytes()
}

/// AAD of a key wrap (1.5): `vault_id|epoch|instance_id` of the
/// recipient, so a wrapped key opens for that vault, epoch and instance only.
/// The wrap key is HKDF-SHA256 over the X25519 shared secret with salt
/// `ephemeral_pub || recipient_pub` and info [`SEALED_WRAP_INFO`]; only
/// engines wrap and unwrap, Hive stores the result.
pub fn sealed_wrap_aad(vault_id: &str, epoch: u32, instance_id: &str) -> Vec<u8> {
    format!("{vault_id}|{epoch}|{instance_id}").into_bytes()
}

/// Ciphertext of a document or patch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SealedBlobV1 {
    /// Always [`SEALED_CONTENT_ALG`].
    pub alg: String,
    pub epoch: u32,
    /// Plaintext schema/format id, also part of the AAD.
    pub content_schema: String,
    /// 24 bytes, base64.
    pub nonce: String,
    /// Ciphertext with the 16-byte tag, base64.
    pub ciphertext: String,
}

impl SealedBlobV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.alg != SEALED_CONTENT_ALG {
            return Err(ValidationError::new(format!(
                "sealed alg must be {SEALED_CONTENT_ALG}"
            )));
        }
        super::validate_text(&self.content_schema, 128, "content_schema", false)?;
        if self.content_schema.contains('|') {
            return Err(ValidationError::new("content_schema must not contain '|'"));
        }
        validate_b64_len(&self.nonce, 24, "nonce")?;
        validate_b64_bounded(&self.ciphertext, MAX_SEALED_CIPHERTEXT_B64, "ciphertext")
    }
}

/// A vault key wrapped to one instance's X25519 key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WrappedKeyV1 {
    /// Always [`SEALED_WRAP_ALG`].
    pub alg: String,
    /// Ephemeral X25519 public key, 32 bytes base64.
    pub ephemeral_pub: String,
    /// 24 bytes, base64.
    pub nonce: String,
    /// 32-byte key plus 16-byte tag, base64.
    pub ciphertext: String,
}

impl WrappedKeyV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.alg != SEALED_WRAP_ALG {
            return Err(ValidationError::new(format!(
                "wrap alg must be {SEALED_WRAP_ALG}"
            )));
        }
        validate_b64_len(&self.ephemeral_pub, 32, "ephemeral_pub")?;
        validate_b64_len(&self.nonce, 24, "nonce")?;
        validate_b64_len(&self.ciphertext, 48, "ciphertext")
    }
}

/// The plaintext of a sealed draft patch (1.5): what a draft in the clear
/// carries in its own fields, plus the ops.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SealedDraftContentV1 {
    pub schema: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    pub ops: Vec<PatchOpV1>,
}

impl SealedDraftContentV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, SEALED_PATCH_CONTENT_SCHEMA)?;
        validate_text(&self.summary, 200, "summary", false)?;
        if let Some(body) = &self.body {
            validate_text(body, 20_000, "body", true)?;
        }
        if self.sources.len() > 16 {
            return Err(ValidationError::new("at most 16 sources"));
        }
        if self.ops.is_empty() || self.ops.len() > MAX_PATCH_OPS {
            return Err(ValidationError::new(format!(
                "a patch carries 1-{MAX_PATCH_OPS} ops"
            )));
        }
        self.ops.iter().try_for_each(PatchOpV1::validate)
    }
}

/// Response of `GET /v1/vaults/{vault_id}/sealed/keys`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedKeyV1 {
    pub schema: String,
    pub vault_id: String,
    pub instance_id: String,
    pub epoch: u32,
    pub wrapped_key: WrappedKeyV1,
    /// 1.5: the epoch new content must be sealed under; above `epoch` while
    /// a rotation is pending. Absent from an older service: `epoch`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epoch_required: Option<u32>,
    /// R135: the place secret wrapped under this epoch's key, when the
    /// grant carried it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place_secret_wrapped: Option<String>,
}

impl SealedKeyV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, SEALED_KEY_SCHEMA)?;
        super::validate_vault_id(&self.vault_id)?;
        validate_id(&self.instance_id, "instance_id")?;
        if self.epoch == 0
            || self
                .epoch_required
                .is_some_and(|required| required < self.epoch)
        {
            return Err(ValidationError::new(
                "epoch must be positive and epoch_required at least epoch",
            ));
        }
        validate_place_secret_wrapped(self.place_secret_wrapped.as_deref())?;
        self.wrapped_key.validate()
    }

    /// The epoch new sealed content must use.
    pub fn required_epoch(&self) -> u32 {
        self.epoch_required.unwrap_or(self.epoch)
    }
}

/// `kx_fingerprint` of an X25519 public key: the first 20 hex characters of
/// its SHA-256, in groups of four (`abcd-ef01-2345-6789-abcd`). Members
/// compare it with the recipient's own `lean-ctx hive status`.
pub fn kx_fingerprint(kx_public_key: &[u8; 32]) -> String {
    use sha2::{Digest as _, Sha256};
    let hex: String = Sha256::digest(kx_public_key)
        .iter()
        .take(10)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    hex.as_bytes()
        .chunks(4)
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect::<Vec<_>>()
        .join("-")
}

/// Someone named in a [`SealedRecipientV1`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedPersonV1 {
    pub id: String,
    pub name: String,
}

/// An approved instance that has no key at `epoch_required` yet (1.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedRecipientV1 {
    pub instance_id: String,
    pub label: String,
    pub owner: SealedPersonV1,
    /// X25519, 32 bytes base64.
    pub instance_kx_public_key: String,
    /// [`kx_fingerprint`] of `instance_kx_public_key`.
    pub kx_fingerprint: String,
    pub approved_by: SealedPersonV1,
    pub approved_at: UtcTimestamp,
}

/// Response of `GET /v1/vaults/{vault_id}/sealed/pending` (1.5), signed
/// by a Hive key of the key set ([`SEALED_PENDING_SIGNATURE_DOMAIN`]).
/// The signature only rules out tampering below Hive; against Hive itself
/// the member confirms the fingerprint (`[hive] sealed_grants = "confirm"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedPendingV1 {
    pub schema: String,
    pub vault_id: String,
    pub epoch_required: u32,
    pub recipients: Vec<SealedRecipientV1>,
    pub kid: String,
    /// Ed25519 (base64) over [`SealedPendingV1::signing_input_raw`].
    pub signature: String,
}

impl SealedPendingV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, SEALED_PENDING_SCHEMA)?;
        super::validate_vault_id(&self.vault_id)?;
        validate_id(&self.kid, "kid")?;
        validate_b64_len(&self.signature, 64, "signature")?;
        if self.epoch_required == 0 || self.recipients.len() > 256 {
            return Err(ValidationError::new(
                "epoch_required must be positive and at most 256 recipients listed",
            ));
        }
        for recipient in &self.recipients {
            validate_id(&recipient.instance_id, "instance_id")?;
            validate_b64_len(
                &recipient.instance_kx_public_key,
                32,
                "instance_kx_public_key",
            )?;
        }
        Ok(())
    }

    /// Signing input of a pending list as received.
    pub fn signing_input_raw(raw: &Value) -> Result<Vec<u8>, ValidationError> {
        super::signing_input_without_signature(SEALED_PENDING_SIGNATURE_DOMAIN, raw)
    }
}

/// `POST /v1/vaults/{vault_id}/sealed/grants`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SealedGrantRequestV1 {
    pub schema: String,
    /// Recipient instance, already approved by an org admin.
    pub instance_id: String,
    pub epoch: u32,
    pub wrapped_key: WrappedKeyV1,
    /// R135: the place secret wrapped under this grant's epoch key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place_secret_wrapped: Option<String>,
}

impl SealedGrantRequestV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, SEALED_GRANT_REQUEST_SCHEMA)?;
        validate_id(&self.instance_id, "instance_id")?;
        validate_place_secret_wrapped(self.place_secret_wrapped.as_deref())?;
        self.wrapped_key.validate()
    }
}

/// `place_secret_wrapped` (R135): base64url without padding, 32–128 chars.
fn validate_place_secret_wrapped(value: Option<&str>) -> Result<(), ValidationError> {
    match value {
        Some(v)
            if !(32..=128).contains(&v.len())
                || !v
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') =>
        {
            Err(ValidationError::new(
                "place_secret_wrapped must be 32-128 base64url characters",
            ))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_are_five_groups_of_four_hex() {
        let fingerprint = kx_fingerprint(&[7u8; 32]);
        assert_eq!(fingerprint.len(), 24);
        assert_eq!(fingerprint.split('-').count(), 5);
        assert!(fingerprint.split('-').all(|group| {
            group.len() == 4
                && group
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        }));
        assert_ne!(fingerprint, kx_fingerprint(&[8u8; 32]));
    }

    #[test]
    fn aad_is_pipe_separated() {
        assert_eq!(
            sealed_aad("prj_1", 7, 2, "ctxpkg.research-context.v1"),
            b"prj_1|7|2|ctxpkg.research-context.v1".to_vec()
        );
    }
}
