// SPDX-License-Identifier: Apache-2.0
//! Dual-signed, durable channel-key transitions anchored in independent host trust.

use serde::{Deserialize, Serialize};

use super::{InstallError, Result, is_digest, manifest, sha256, trust_key};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Proof {
    document: String,
    previous_signature: String,
    next_signature: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema: String,
    channel: String,
    previous_root_key_hex: String,
    next_root_key_hex: String,
    minimum_sequence: u64,
    expires_unix_ms: u64,
}

pub(super) struct TrustedRoot {
    pub(super) key: [u8; 32],
    pub(super) minimum_sequence: u64,
    pub(super) roots: std::collections::BTreeSet<String>,
    production: bool,
    keys: Vec<([u8; 32], u64)>,
}

impl Proof {
    fn verify(&self, previous: &[u8; 32], production: bool) -> Result<Document> {
        if self.document.len() > 1024 {
            return Err(InstallError::Size);
        }
        let doc: Document =
            serde_json::from_str(&self.document).map_err(|_| InstallError::Manifest)?;
        let next = trust_key(&doc.next_root_key_hex)?;
        if doc.schema
            != if production {
                "leanctx.runtime-root-transition/v2"
            } else {
                "leanctx.runtime-root-transition/v1"
            }
            || doc.channel != if production { "production" } else { "staging" }
            || trust_key(&doc.previous_root_key_hex)? != *previous
            || next == *previous
            || doc.minimum_sequence == 0
            || doc.expires_unix_ms == 0
        {
            return Err(InstallError::Manifest);
        }
        for (key, encoded) in [
            (previous, &self.previous_signature),
            (&next, &self.next_signature),
        ] {
            if !is_digest(encoded, 128) {
                return Err(InstallError::Signature);
            }
            let signature = hex::decode(encoded).map_err(|_| InstallError::Signature)?;
            manifest::verify_signature(
                self.document.as_bytes(),
                &signature,
                key,
                if production {
                    b"leanctx-runtime-root-transition-v2\0"
                } else {
                    b"leanctx-runtime-root-transition-v1\0"
                },
            )?;
        }
        Ok(doc)
    }

    pub(super) fn admit(&self, current: &TrustedRoot, sequence: u64) -> Result<()> {
        let doc = self.verify(&current.key, current.production)?;
        let remaining = doc
            .expires_unix_ms
            .checked_sub(super::catalog::now_ms()?)
            .ok_or(InstallError::Manifest)?;
        if remaining == 0
            || remaining > 7 * 24 * 60 * 60 * 1000
            || doc.minimum_sequence <= sequence.max(current.minimum_sequence)
        {
            return Err(InstallError::State);
        }
        Ok(())
    }
}

/// Accepted transitions remain durable after their admission deadline. Every
/// use still rechecks signatures/order against the independently supplied anchor.
pub(super) fn resolve_for(
    anchor: &[u8; 32],
    proofs: &[Proof],
    production: bool,
) -> Result<TrustedRoot> {
    if proofs.len() > 8 {
        return Err(InstallError::Size);
    }
    let mut trusted = TrustedRoot {
        key: *anchor,
        minimum_sequence: 0,
        roots: [sha256(anchor)].into(),
        production,
        keys: vec![(*anchor, 0)],
    };
    for proof in proofs {
        let doc = proof.verify(&trusted.key, production)?;
        let next = trust_key(&doc.next_root_key_hex)?;
        if doc.minimum_sequence <= trusted.minimum_sequence || !trusted.roots.insert(sha256(&next))
        {
            return Err(InstallError::State);
        }
        trusted.key = next;
        trusted.minimum_sequence = doc.minimum_sequence;
        trusted.keys.push((next, doc.minimum_sequence));
    }
    Ok(trusted)
}

impl TrustedRoot {
    /// A retained package can use its original catalog after rotation/expiry.
    /// Old roots cannot authorize catalog sequences reserved for newer roots.
    pub(super) fn admits_catalog_key(&self, key: &[u8; 32], sequence: u64) -> bool {
        self.keys
            .iter()
            .enumerate()
            .any(|(index, (candidate, minimum))| {
                candidate == key
                    && sequence >= *minimum
                    && self
                        .keys
                        .get(index + 1)
                        .is_none_or(|(_, next)| sequence < *next)
            })
    }
}
