// SPDX-License-Identifier: Apache-2.0
//! Audit-trail anchors of stored receipts: the `gateway_receipt:<digest>`
//! entries of the signed audit trail and their chain and signature state.

use std::path::Path;

use super::is_digest_hex;

/// The `gateway_receipt:<digest>` entries of the signed audit trail, read
/// once, with the result of verifying the trail's hash chain.
pub struct AuditAnchors {
    /// The trail file exists and is readable.
    pub trail_present: bool,
    /// The trail could not be read at this moment (an append held its lock
    /// past the reader's bound, or the file is unreadable): nothing about
    /// anchors or the chain was CHECKED. Not an absent trail and not a broken
    /// chain — readers must say "not checked", never "not found" or "invalid".
    pub trail_unavailable: bool,
    /// Every hash link of the trail verifies.
    pub chain_valid: bool,
    /// Entries the trail holds, as far as the chain check counted them.
    pub entries_total: usize,
    /// The hash chain verifies from the first entry through this one
    /// (1-based); equal to `entries_total` when the whole chain verifies.
    pub verified_through: usize,
    /// Receipt digest (hex) → its anchor.
    entries: std::collections::HashMap<String, Anchor>,
}

/// What checking an audit entry's Ed25519 signature showed. The signature is
/// over the entry hash; that the hash covers the entry's fields is what the
/// chain check establishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureState {
    /// Verified with the public key of this machine's audit identity.
    Verified,
    /// A signature is there, but it cannot be checked here: the reason.
    Unverifiable(&'static str),
    /// The entry carries no signature.
    Absent,
    /// The signature does not verify with the trail's public key.
    Invalid,
}

/// One `gateway_receipt:` entry of the audit trail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor {
    pub entry_hash: String,
    /// 1-based position of the entry in the trail.
    pub position: usize,
    /// The hash chain verifies up to and including this entry.
    pub chain_verified: bool,
    pub signature: SignatureState,
    /// SHA-256 (hex) of the trace sidecar's stored bytes, when the entry
    /// anchors a pair.
    pub trace: Option<String>,
}

impl AuditAnchors {
    pub fn load() -> Self {
        match crate::core::audit_trail::default_trail_path() {
            Some(path) => Self::load_at(&path),
            None => Self {
                trail_present: false,
                trail_unavailable: false,
                chain_valid: false,
                entries_total: 0,
                verified_through: 0,
                entries: std::collections::HashMap::new(),
            },
        }
    }

    pub(crate) fn load_at(path: &Path) -> Self {
        use crate::core::agent_identity;
        let chain = crate::core::audit_trail::verify_chain_at(path);
        let verified_through = if chain.valid {
            chain.total_entries
        } else {
            chain.first_invalid_at.unwrap_or(0)
        };
        // The trail is signed by this installation's audit identity.
        let public_key = agent_identity::load_public_key("lean-ctx").ok();
        let mut entries = std::collections::HashMap::new();
        for (index, entry) in crate::core::audit_trail::load_all_at(path)
            .into_iter()
            .enumerate()
        {
            let Some(rest) = entry
                .action
                .as_deref()
                .and_then(|action| action.strip_prefix("gateway_receipt:"))
            else {
                continue;
            };
            let mut parts = rest.split('|');
            let Some(hex) = parts.next().filter(|hex| is_digest_hex(hex)) else {
                continue;
            };
            let trace = parts
                .find_map(|part| part.strip_prefix("trace="))
                .filter(|hex| is_digest_hex(hex))
                .map(str::to_owned);
            let signature = match (entry.signature.as_deref(), public_key.as_ref()) {
                (None, _) => SignatureState::Absent,
                (Some(_), None) => SignatureState::Unverifiable(
                    "the public key of the audit identity is not available on this machine",
                ),
                (Some(signature), Some(key)) => match agent_identity::hex_decode(signature) {
                    Ok(bytes)
                        if agent_identity::verify_signature(
                            key.as_bytes(),
                            entry.entry_hash.as_bytes(),
                            &bytes,
                        ) =>
                    {
                        SignatureState::Verified
                    }
                    _ => SignatureState::Invalid,
                },
            };
            let anchor = Anchor {
                position: index + 1,
                chain_verified: index < verified_through,
                signature,
                trace,
                entry_hash: entry.entry_hash,
            };
            entries.insert(hex.to_owned(), anchor);
        }
        Self {
            trail_present: chain.source_present,
            // `audit_trail` reports an unreadable or contended trail as "not
            // present and not valid"; an absent trail is "not present, valid".
            trail_unavailable: !chain.source_present && !chain.valid,
            chain_valid: chain.valid,
            entries_total: chain.total_entries,
            verified_through,
            entries,
        }
    }

    /// The anchor of the receipt stored under `hex`. `None`: no entry of the
    /// trail names it.
    pub fn of(&self, hex: &str) -> Option<&Anchor> {
        self.entries.get(hex)
    }
}
