//! Offline-verifiable, tenant-scoped evidence chains.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use super::identity::{IdentityContext, TenantId};
use super::tenant_isolation::{CrossTenantLeak, TenantBoundary, assert_same_tenant};

/// One signed observation in an execution evidence chain.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EvidenceEntry {
    /// Unix timestamp in seconds supplied by the producer.
    pub timestamp: u64,
    /// Identity of the actor; its tenant is the entry's isolation boundary.
    pub actor: IdentityContext,
    /// Stable action name, such as `tool.execute` or `provider.response`.
    pub action: String,
    /// Digest of the observed payload or artifact.
    pub digest: String,
    /// MCP task that produced this evidence, if one is active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Root task of the producing MCP session, if this is a child task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Self-describing Ed25519 signature:
    /// `ed25519:<public-key-hex>:<signature-hex>`.
    pub signature: String,
    /// BLAKE3 hash of the preceding complete entry, or `None` for the first.
    pub previous_hash: Option<String>,
}

impl EvidenceEntry {
    /// Creates an entry from already encoded evidence fields.
    #[must_use]
    pub fn new(
        timestamp: u64,
        actor: IdentityContext,
        action: impl Into<String>,
        digest: impl Into<String>,
        signature: impl Into<String>,
        previous_hash: Option<String>,
    ) -> Self {
        let lineage = crate::core::task_spine::TaskSpine::current();
        Self {
            timestamp,
            actor,
            action: action.into(),
            digest: digest.into(),
            task_id: lineage
                .as_ref()
                .map(|task| task.task_id.as_str().to_owned()),
            parent_id: lineage
                .and_then(|task| task.parent_task_id.map(|parent| parent.as_str().to_owned())),
            signature: signature.into(),
            previous_hash,
        }
    }

    /// Creates and signs an entry with the supplied Ed25519 key.
    #[must_use]
    pub fn signed(
        timestamp: u64,
        actor: IdentityContext,
        action: impl Into<String>,
        digest: impl Into<String>,
        previous_hash: Option<String>,
        signing_key: &SigningKey,
    ) -> Self {
        let mut entry = Self::new(
            timestamp,
            actor,
            action,
            digest,
            String::new(),
            previous_hash,
        );
        entry.sign_with(signing_key);
        entry
    }

    /// Signs this entry's content in place.
    pub fn sign_with(&mut self, signing_key: &SigningKey) {
        let signature = signing_key.sign(&self.signing_bytes());
        self.signature = format!(
            "ed25519:{}:{}",
            hex_encode(&signing_key.verifying_key().to_bytes()),
            hex_encode(&signature.to_bytes())
        );
    }

    /// Returns canonical bytes covered by the signature.
    #[must_use]
    pub fn signing_bytes(&self) -> Vec<u8> {
        let payload = SignedEvidencePayload {
            timestamp: self.timestamp,
            actor: &self.actor,
            action: &self.action,
            digest: &self.digest,
            task_id: &self.task_id,
            parent_id: &self.parent_id,
            previous_hash: &self.previous_hash,
        };
        serde_json::to_vec(&payload).expect("evidence payload is serializable")
    }

    /// Returns this complete entry's BLAKE3 chain hash.
    #[must_use]
    pub fn entry_hash(&self) -> String {
        let bytes = serde_json::to_vec(self).expect("evidence entry is serializable");
        format!("blake3:{}", blake3::hash(&bytes).to_hex())
    }

    /// Verifies the embedded Ed25519 signature without platform state.
    #[must_use]
    pub fn has_valid_signature(&self) -> bool {
        let Some((public_key, signature)) = parse_signature(&self.signature) else {
            return false;
        };
        public_key.verify(&self.signing_bytes(), &signature).is_ok()
    }
}

impl TenantBoundary for EvidenceEntry {
    fn tenant_id(&self) -> &TenantId {
        self.actor.tenant_id()
    }
}

#[derive(Serialize)]
struct SignedEvidencePayload<'a> {
    timestamp: u64,
    actor: &'a IdentityContext,
    action: &'a str,
    digest: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_id: &'a Option<String>,
    previous_hash: &'a Option<String>,
}

/// Ordered evidence entries whose links and signatures can be checked by any
/// implementation that supports the public JSON and Ed25519 formats.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct EvidenceChain {
    /// Entries in append order.
    #[serde(default)]
    pub entries: Vec<EvidenceEntry>,
}

impl EvidenceChain {
    /// Creates an empty chain.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Creates a chain from entries in their already-recorded order.
    #[must_use]
    pub fn from_entries(entries: Vec<EvidenceEntry>) -> Self {
        Self { entries }
    }

    /// Appends an entry without mutating its signed fields.
    pub fn push(&mut self, entry: EvidenceEntry) {
        self.entries.push(entry);
    }

    /// Appends an entry after checking its tenant against the current chain.
    pub fn append_scoped(&mut self, entry: EvidenceEntry) -> Result<(), CrossTenantLeak> {
        if let Some(previous) = self.entries.first() {
            assert_same_tenant(previous, &entry)?;
        }
        self.entries.push(entry);
        Ok(())
    }

    /// Creates, signs, and appends an entry linked to the current chain head.
    pub fn append_signed(
        &mut self,
        timestamp: u64,
        actor: IdentityContext,
        action: impl Into<String>,
        digest: impl Into<String>,
        signing_key: &SigningKey,
    ) -> Result<(), CrossTenantLeak> {
        let previous_hash = self.entries.last().map(EvidenceEntry::entry_hash);
        let entry =
            EvidenceEntry::signed(timestamp, actor, action, digest, previous_hash, signing_key);
        self.append_scoped(entry)
    }

    /// Returns the tenant carried by the first entry, if the chain is non-empty.
    #[must_use]
    pub fn tenant_id(&self) -> Option<&TenantId> {
        self.entries.first().map(TenantBoundary::tenant_id)
    }

    /// Returns the current chain head hash.
    #[must_use]
    pub fn head_hash(&self) -> Option<String> {
        self.entries.last().map(EvidenceEntry::entry_hash)
    }

    /// Verifies this chain.
    #[must_use]
    pub fn verify(&self) -> ChainVerificationResult {
        verify_chain(self)
    }
}

/// Result of independent chain, link, tenant, and signature verification.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChainVerificationResult {
    /// True only when every entry passes every verification check.
    pub valid: bool,
    /// First entry index that failed, if any.
    pub broken_at: Option<usize>,
    /// Human-readable, stable diagnostics for each failed check.
    pub issues: Vec<String>,
}

impl ChainVerificationResult {
    /// Returns whether verification succeeded.
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        self.valid
    }
}

/// Verifies an evidence chain without contacting the platform that produced it.
#[must_use]
pub fn verify_chain(chain: &EvidenceChain) -> ChainVerificationResult {
    let mut result = ChainVerificationResult {
        valid: true,
        broken_at: None,
        issues: Vec::new(),
    };
    let mut previous_timestamp = None;

    for (index, entry) in chain.entries.iter().enumerate() {
        let mut report_issue = |issue: String| {
            result.valid = false;
            result.broken_at.get_or_insert(index);
            result.issues.push(issue);
        };

        if entry.actor.tenant.is_empty() {
            report_issue(format!("entry {index} has an empty tenant id"));
        }
        if entry.action.trim().is_empty() {
            report_issue(format!("entry {index} has an empty action"));
        }
        if entry.digest.trim().is_empty() {
            report_issue(format!("entry {index} has an empty digest"));
        }
        if let Some(timestamp) = previous_timestamp {
            if entry.timestamp < timestamp {
                report_issue(format!(
                    "entry {index} timestamp is earlier than its predecessor"
                ));
            }
        }
        previous_timestamp = Some(entry.timestamp);

        let expected_previous = if index == 0 {
            None
        } else {
            Some(chain.entries[index - 1].entry_hash())
        };
        if entry.previous_hash != expected_previous {
            report_issue(format!("entry {index} has an invalid previous_hash"));
        }

        if index > 0 {
            if let Err(leak) = assert_same_tenant(&chain.entries[0], entry) {
                report_issue(format!("entry {index} crosses tenant boundary: {leak}"));
            }
        }

        if !entry.has_valid_signature() {
            report_issue(format!("entry {index} has an invalid Ed25519 signature"));
        }
    }

    result
}

fn parse_signature(value: &str) -> Option<(VerifyingKey, Signature)> {
    let mut parts = value.split(':');
    if parts.next()? != "ed25519" || parts.clone().count() != 2 {
        return None;
    }
    let public_key = decode_fixed::<32>(parts.next()?)?;
    let signature = decode_fixed::<64>(parts.next()?)?;
    let public_key = VerifyingKey::from_bytes(&public_key).ok()?;
    Some((public_key, Signature::from_bytes(&signature)))
}

fn decode_fixed<const N: usize>(value: &str) -> Option<[u8; N]> {
    if value.len() != N * 2 {
        return None;
    }
    let bytes = value.as_bytes();
    let mut decoded = [0_u8; N];
    for (index, byte) in decoded.iter_mut().enumerate() {
        let high = hex_value(bytes[index * 2])?;
        let low = hex_value(bytes[index * 2 + 1])?;
        *byte = (high << 4) | low;
    }
    Some(decoded)
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}
