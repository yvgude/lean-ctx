// SPDX-License-Identifier: Apache-2.0

//! Org policy and filter packs.
//!
//! `GET /v1/policy/bundle?since=<policy_version>` (`If-None-Match`) →
//! [`PolicyBundleV1`] or 304.
//!
//! `org_policy` is an `OrgPolicyArtifactV2`
//! `{statement, public_key, signature, pack_toml, unpin?}`:
//! - `statement` is `OrgPolicyStatementV2` (`leanctx.org-policy/v2`), signed
//!   as `"leanctx.org-policy.v2\n" || serde_json::to_vec(statement)` in field
//!   declaration order (schema, organization_id, key_id, policy_version,
//!   issued_at, not_before, not_after, grace_seconds, pack_digest,
//!   required_filter_packs), by a key the lease pins in
//!   `policy_signer_keys`;
//! - `required_filter_packs` (C 1.8) lists the filter packs the policy puts
//!   in force as [`RequiredFilterPackV1`], ascending by `pack_id`; it is left
//!   out when empty, so a statement without it keeps its 1.7 bytes. Every
//!   listed pack must be in the same bundle with that version and statement
//!   digest ([`unmet_required_packs`]): Hive signs filter packs with its
//!   own key, and only this list binds them to the organization's signature.
//!   Signers sort the list before signing. Hive includes the field only when
//!   the org requires packs; engines below 1.8 refuse such a statement as a
//!   whole (unknown field), so their last good bundle stays. A received
//!   statement with an explicit `"required_filter_packs": []` verifies
//!   against the bytes without the field: same meaning, other wire bytes,
//!   harmless. Packs the list does not name may still arrive; a filter pack
//!   can only tighten (`redact`, `block`), never loosen, so an extra pack
//!   is harmless;
//! - `pack_toml` is the pack itself (UTF-8 TOML, at most 1 MiB) and
//!   `statement.pack_digest` = `"sha256:" + hex(sha256(pack_toml))`;
//! - encodings follow `team_context`: `public_key`/`signature` base64url
//!   without padding, `key_id` = `"key:ed25519-"` + the first 32 hex chars of
//!   sha256(public key), timestamps RFC 3339 `Z`.
//!
//! Top-level `unpin` is the signed unpin envelope
//! (`leanctx.org-policy-unpin.v2\n`) when no policy artifact is current.
//! Both stay raw JSON here and are verified by the engine's org-policy
//! verifier; a failed verification never replaces the last good bundle.
//! Fixture `policy_bundle.signed.response` and `vectors.org_policy` pin the
//! exact bytes.
//!
//! Filter packs carry their statement as base64 bytes, signed by a Hive
//! key under [`FILTER_PACK_SIGNATURE_DOMAIN`].

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    FILTER_PACK_SIGNATURE_DOMAIN, require_schema, validate_b64_bounded, validate_b64_len,
    validate_hex64, validate_id,
};
use crate::ValidationError;

pub const POLICY_BUNDLE_SCHEMA: &str = "leanctx.hive.policy-bundle/v1";
/// Largest policy-bundle response (a full 1 MiB pack plus envelopes).
pub const MAX_POLICY_BUNDLE_BYTES: usize = 2 * 1024 * 1024;
/// Largest encoded filter-pack statement.
pub const MAX_FILTER_PACK_STATEMENT_B64: usize = 2 * 1024 * 1024;
pub const MAX_FILTER_PACKS: usize = 64;

/// A statement and its signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedStatementV1 {
    /// Statement bytes, base64.
    pub statement_b64: String,
    /// Key id within the Hive key set.
    pub kid: String,
    /// Ed25519 signature (base64) over `domain || statement bytes`.
    pub signature: String,
}

impl SignedStatementV1 {
    pub fn validate(&self, max_encoded: usize) -> Result<(), ValidationError> {
        validate_b64_bounded(&self.statement_b64, max_encoded, "statement_b64")?;
        validate_id(&self.kid, "kid")?;
        validate_b64_len(&self.signature, 64, "signature")
    }
}

/// `domain || statement`: what a filter-pack signature covers.
pub fn filter_pack_signing_input(statement: &[u8]) -> Vec<u8> {
    let mut bytes = FILTER_PACK_SIGNATURE_DOMAIN.as_bytes().to_vec();
    bytes.extend_from_slice(statement);
    bytes
}

pub const FILTER_PACK_SCHEMA: &str = "leanctx.filter-pack/v1";
pub const MAX_FILTER_PATTERNS: usize = 64;
/// Longest pattern id and label (`hive:<id>` must fit a 64-byte rule label).
pub const MAX_FILTER_LABEL_BYTES: usize = 48;
pub const MAX_FILTER_PATTERN_CHARS: usize = 1000;
/// Compiled-size limit of one pattern (`regex` size_limit and dfa_size_limit).
pub const MAX_FILTER_PATTERN_COMPILED_BYTES: usize = 1024 * 1024;

/// What a matching pattern does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterActionV1 {
    /// Replace each match with `[REDACTED:<label or id>]`.
    Redact,
    /// Refuse the content: screening drops it, egress refuses it.
    Block,
}

/// One pattern of a filter pack. Unknown fields or values refuse the pack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilterPatternV1 {
    pub id: String,
    /// Always `regex` in v1.
    pub kind: String,
    /// `regex` crate syntax.
    pub pattern: String,
    pub action: FilterActionV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// The signed statement of a filter pack (the bytes behind `statement_b64`).
/// Unknown top-level fields are tolerated; patterns are strict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterPackV1 {
    pub schema: String,
    pub pack_id: String,
    pub version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub patterns: Vec<FilterPatternV1>,
}

fn valid_slug(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

impl FilterPatternV1 {
    /// Compile the pattern within the v1 size limits.
    pub fn compile(&self) -> Result<regex::Regex, ValidationError> {
        regex::RegexBuilder::new(&self.pattern)
            .size_limit(MAX_FILTER_PATTERN_COMPILED_BYTES)
            .dfa_size_limit(MAX_FILTER_PATTERN_COMPILED_BYTES)
            .build()
            .map_err(|_| ValidationError::new(format!("pattern {} does not compile", self.id)))
    }

    /// The rule label the engine uses (`hive:<label or id>`); a redacted
    /// match reads `[REDACTED:hive:<label or id>]`.
    pub fn rule_label(&self) -> String {
        format!("hive:{}", self.label.as_deref().unwrap_or(&self.id))
    }
}

impl FilterPackV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, FILTER_PACK_SCHEMA)?;
        if !valid_slug(&self.pack_id, 128) || self.version == 0 {
            return Err(ValidationError::new("pack_id or version is malformed"));
        }
        if let Some(title) = &self.title {
            super::validate_text(title, 200, "title", false)?;
        }
        if self.patterns.is_empty() || self.patterns.len() > MAX_FILTER_PATTERNS {
            return Err(ValidationError::new("a filter pack holds 1-64 patterns"));
        }
        let mut ids = std::collections::BTreeSet::new();
        for pattern in &self.patterns {
            if !valid_slug(&pattern.id, MAX_FILTER_LABEL_BYTES) || !ids.insert(pattern.id.as_str())
            {
                return Err(ValidationError::new(format!(
                    "pattern id {:?} is malformed or repeats",
                    pattern.id
                )));
            }
            if pattern.kind != "regex" {
                return Err(ValidationError::new(format!(
                    "pattern {}: unknown kind",
                    pattern.id
                )));
            }
            if pattern.pattern.is_empty()
                || pattern.pattern.chars().count() > MAX_FILTER_PATTERN_CHARS
            {
                return Err(ValidationError::new(format!(
                    "pattern {}: 1-1000 characters",
                    pattern.id
                )));
            }
            if let Some(label) = &pattern.label
                && (label.len() > MAX_FILTER_LABEL_BYTES
                    || !valid_slug(label, MAX_FILTER_LABEL_BYTES))
            {
                return Err(ValidationError::new(format!(
                    "pattern {}: label is malformed",
                    pattern.id
                )));
            }
            pattern.compile()?;
        }
        Ok(())
    }
}

/// One filter pack the org enabled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterPackRefV1 {
    pub pack_id: String,
    /// Monotonic per pack.
    pub version: u64,
    /// Lowercase hex SHA-256 of the statement bytes.
    pub sha256: String,
    pub envelope: SignedStatementV1,
}

/// A filter pack an org policy requires: field order is the signed byte order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequiredFilterPackV1 {
    pub pack_id: String,
    pub version: u64,
    /// Lowercase hex SHA-256 of the pack's statement bytes, as
    /// [`FilterPackRefV1::sha256`].
    pub sha256: String,
}

/// At most [`MAX_FILTER_PACKS`], ids strictly ascending (one encoding per
/// set), versions from 1, digests lowercase hex.
pub fn validate_required_filter_packs(
    packs: &[RequiredFilterPackV1],
) -> Result<(), ValidationError> {
    if packs.len() > MAX_FILTER_PACKS {
        return Err(ValidationError::new("too many required filter packs"));
    }
    for (i, pack) in packs.iter().enumerate() {
        if !valid_slug(&pack.pack_id, 128) || pack.version == 0 {
            return Err(ValidationError::new(
                "required filter pack id or version is malformed",
            ));
        }
        validate_hex64(&pack.sha256, "sha256")?;
        if i > 0 && packs[i - 1].pack_id >= pack.pack_id {
            return Err(ValidationError::new(
                "required filter packs must be in ascending pack_id order without repeats",
            ));
        }
    }
    Ok(())
}

/// Why `available` does not satisfy `required`, one line per unmet pack
/// (empty when every required pack is there with its version and digest).
pub fn unmet_required_packs(
    required: &[RequiredFilterPackV1],
    available: &[FilterPackRefV1],
) -> Vec<String> {
    required
        .iter()
        .filter_map(
            |r| match available.iter().find(|p| p.pack_id == r.pack_id) {
                None => Some(format!("required filter pack {} is missing", r.pack_id)),
                Some(p) if p.version != r.version => Some(format!(
                    "required filter pack {} is version {}, the policy requires {}",
                    r.pack_id, p.version, r.version
                )),
                Some(p) if p.sha256 != r.sha256 => Some(format!(
                    "required filter pack {} version {} is not the one the policy signed",
                    r.pack_id, r.version
                )),
                Some(_) => None,
            },
        )
        .collect()
}

/// Response of `GET /v1/policy/bundle`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicyBundleV1 {
    pub schema: String,
    /// Highest version in the org's policy sequence (0 = never published).
    pub policy_version: u64,
    pub org_policy: Option<Value>,
    pub unpin: Option<Value>,
    pub filter_packs: Vec<FilterPackRefV1>,
}

impl PolicyBundleV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, POLICY_BUNDLE_SCHEMA)?;
        for envelope in [&self.org_policy, &self.unpin].into_iter().flatten() {
            if !envelope.is_object() {
                return Err(ValidationError::new("policy envelopes must be objects"));
            }
        }
        if self.filter_packs.len() > MAX_FILTER_PACKS {
            return Err(ValidationError::new("too many filter packs"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for pack in &self.filter_packs {
            validate_id(&pack.pack_id, "pack_id")?;
            validate_hex64(&pack.sha256, "sha256")?;
            pack.envelope.validate(MAX_FILTER_PACK_STATEMENT_B64)?;
            if !seen.insert(pack.pack_id.as_str()) {
                return Err(ValidationError::new("pack_id repeats"));
            }
        }
        Ok(())
    }
}
