// SPDX-License-Identifier: Apache-2.0

//! Vault resolution: the engine never sends a path, remote or name, only
//! an org-salted digest of its git repository's stable identity. One git
//! repository maps to exactly one vault.
//!
//! - `GET /v1/org/salt` → [`OrgSaltV1`]
//! - `POST /v1/vaults/resolve` [`VaultResolveRequestV1`] → [`VaultV1`]
//!
//! `identity_digest = hex(HMAC-SHA256(salt, stable_identity))`, where
//! `stable_identity` is [`stable_identity`] of the engine's repository identity
//! (`.lean-ctx-id` > normalized git origin without credentials > manifests).
//!
//! Since 1.1 the request may name a `vault` (written by `lean-ctx hive link
//! <ns>/<name>`) to bind the repository to a vault created on the web. The
//! service binds on first sight when the instance may contribute to that
//! vault, otherwise answers 403 `forbidden_right`. Without `vault`, resolve
//! creates a vault on first sight.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::{self, Write as _};

use super::{
    REPO_IDENTITY_DOMAIN, require_schema, validate_b64_len, validate_hex64, validate_id,
    validate_text,
};
use crate::ValidationError;

pub const ORG_SALT_SCHEMA: &str = "leanctx.hive.org-salt/v1";
pub const VAULT_RESOLVE_REQUEST_SCHEMA: &str = "leanctx.hive.vault-resolve-request/v1";
pub const VAULT_SCHEMA: &str = "leanctx.hive.vault/v1";

/// Longest vault label, in characters.
pub const MAX_VAULT_LABEL_CHARS: usize = 64;

/// `sha256_hex("leanctx.project-identity.v1\n" + repo_identity)`.
pub fn stable_identity(repo_identity: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(REPO_IDENTITY_DOMAIN.as_bytes());
    hasher.update(repo_identity.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in hasher.finalize() {
        write!(&mut hex, "{byte:02x}").expect("writing to String cannot fail");
    }
    hex
}

/// `GET /v1/org/salt`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgSaltV1 {
    pub schema: String,
    pub org_salt_id: String,
    /// 32 random bytes, base64. Org-confidential: never logged.
    pub salt: String,
}

impl fmt::Debug for OrgSaltV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OrgSaltV1")
            .field("org_salt_id", &self.org_salt_id)
            .field("salt", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl OrgSaltV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, ORG_SALT_SCHEMA)?;
        validate_id(&self.org_salt_id, "org_salt_id")?;
        validate_b64_len(&self.salt, 32, "salt")
    }
}

/// `POST /v1/vaults/resolve`. Creates the vault on first sight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultResolveRequestV1 {
    pub schema: String,
    pub org_salt_id: String,
    /// 64 lowercase hex characters.
    pub identity_digest: String,
    /// Display label, only when `[hive] vault_labels = true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// 1.1: the vault to bind to (from `lean-ctx hive link`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault: Option<VaultRefV1>,
}

impl VaultResolveRequestV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, VAULT_RESOLVE_REQUEST_SCHEMA)?;
        validate_id(&self.org_salt_id, "org_salt_id")?;
        validate_hex64(&self.identity_digest, "identity_digest")?;
        if let Some(label) = &self.label {
            validate_text(label, MAX_VAULT_LABEL_CHARS, "label", false)?;
        }
        if let Some(vault) = &self.vault {
            vault.validate()?;
        }
        Ok(())
    }
}

/// The vault a git repository maps to, as `@<ns>/<name>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultRefV1 {
    pub ns: String,
    pub name: String,
}

impl VaultRefV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_id(&self.ns, "vault.ns")?;
        validate_id(&self.name, "vault.name")
    }

    /// Parse `<ns>/<name>` as `lean-ctx hive link` takes it.
    pub fn parse(value: &str) -> Result<Self, ValidationError> {
        let (ns, name) = value
            .split_once('/')
            .ok_or_else(|| ValidationError::new("vault must be <ns>/<name>"))?;
        let vault = Self {
            ns: ns.into(),
            name: name.into(),
        };
        vault.validate()?;
        Ok(vault)
    }
}

/// Response of `POST /v1/vaults/resolve`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultV1 {
    pub schema: String,
    pub vault_id: String,
    pub vault: VaultRefV1,
    /// End-to-end sealed vault: content only as `SealedBlobV1`.
    pub sealed: bool,
    /// Current sealed key epoch, present when `sealed`. 1.9 R3: `0` means
    /// the vault has no key yet (created sealed on the web); the first engine
    /// that seals content creates epoch 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_epoch: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// 1.9 (section 7): the vault's auto-merge threshold; absent means
    /// people merge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_merge: Option<super::automerge::AutoMergeV1>,
    /// 1.9 (section 8): `full` when the owner publishes the code graph;
    /// absent means `anchors`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_graph: Option<super::codegraph::CodeGraphMode>,
}

impl VaultV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, VAULT_SCHEMA)?;
        super::validate_vault_id(&self.vault_id)?;
        validate_id(&self.vault.ns, "vault.ns")?;
        validate_id(&self.vault.name, "vault.name")?;
        if self.sealed != self.key_epoch.is_some() {
            return Err(ValidationError::new(
                "key_epoch is present exactly when the vault is sealed",
            ));
        }
        if let Some(auto_merge) = &self.auto_merge {
            auto_merge.validate()?;
        }
        if let Some(label) = &self.label {
            validate_text(label, MAX_VAULT_LABEL_CHARS, "label", false)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_identity_is_domain_separated_sha256() {
        // sha256("leanctx.project-identity.v1\ngit:github.com/yvgude/lean-ctx")
        let id = stable_identity("git:github.com/yvgude/lean-ctx");
        assert_eq!(id.len(), 64);
        assert_eq!(id, stable_identity("git:github.com/yvgude/lean-ctx"));
        assert_ne!(id, stable_identity("git:github.com/yvgude/other"));
    }
}
