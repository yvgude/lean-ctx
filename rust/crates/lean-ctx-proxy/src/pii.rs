// SPDX-License-Identifier: Apache-2.0
//! Person pseudonymization (enterprise#39, GDPR/DSGVO).
//!
//! `usage_events.person` is normally an e-mail address — personal data under
//! GDPR. With `[gateway_server].pseudonymize_persons = true` the gateway
//! replaces it at the identity choke-point (`attach_gateway_tags`) with a
//! stable keyed hash: `p:<16 hex>`. One choke-point means budget ledgers,
//! usage rows, dashboards, metrics and logs all see only the pseudonym.
//!
//! Properties:
//!
//! - **Stable per install**: keyed BLAKE3 with a per-install salt
//!   (`<data_dir>/gateway_pii_salt`, created on first use, 0600). The same
//!   person always maps to the same pseudonym, so per-person budgets and
//!   rollups keep working.
//! - **Not reversible** without the salt file; the salt never leaves the host.
//! - **Re-identifiable on purpose** by the operator (GDPR Art. 15/17 requires
//!   acting on "the data of person X"): `pseudonymize("x@acme.com")` recomputes
//!   the key, which is exactly what the `gateway gdpr` CLI does.
//!
//! Normalization: e-mail-ish inputs are trimmed + lowercased before hashing so
//! `A@Acme.com` and `a@acme.com` land on one pseudonym.

use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

/// Pseudonym prefix — makes pseudonymized rows self-describing in exports,
/// dashboards and GDPR tooling.
pub const PSEUDONYM_PREFIX: &str = "p:";

/// True when the deployment opted into pseudonymization.
#[must_use]
pub fn enabled() -> bool {
    lean_ctx::core::config::Config::load()
        .gateway_server
        .pseudonymize_persons
        .unwrap_or(false)
}

/// Applies the configured person policy: pseudonym when enabled, identity
/// otherwise. The single entry point for the auth guard.
#[must_use]
pub fn effective_person(person: &str) -> String {
    if enabled() {
        pseudonymize(person)
    } else {
        person.to_string()
    }
}

/// The stable pseudonym for a person: `p:` + first 16 hex chars of
/// `BLAKE3_keyed(salt, normalized_person)`. Already-pseudonymized inputs pass
/// through unchanged (idempotent — safe on re-tagged requests).
#[must_use]
pub fn pseudonymize(person: &str) -> String {
    let normalized = person.trim().to_lowercase();
    if normalized.starts_with(PSEUDONYM_PREFIX) {
        return normalized;
    }
    let key = salt();
    let hash = blake3::keyed_hash(&key, normalized.as_bytes());
    let hex = hash.to_hex();
    format!("{PSEUDONYM_PREFIX}{}", &hex.as_str()[..16])
}

/// All storage keys a GDPR request for `person` must match: the raw value
/// (pre-pseudonymization rows, or pseudonymization off) and the pseudonym.
#[must_use]
pub fn person_match_keys(person: &str) -> Vec<String> {
    let raw = person.trim().to_string();
    let pseudo = pseudonymize(person);
    if raw == pseudo {
        vec![raw]
    } else {
        vec![raw, pseudo]
    }
}

fn salt_path() -> PathBuf {
    lean_ctx::core::paths::data_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("gateway_pii_salt")
}

/// Loads (or creates once) the per-install salt. Process-cached: the salt is
/// immutable for the lifetime of an install.
fn salt() -> [u8; 32] {
    static SALT: OnceLock<[u8; 32]> = OnceLock::new();
    *SALT.get_or_init(|| {
        let path = salt_path();
        if let Ok(raw) = std::fs::read_to_string(&path)
            && let Some(bytes) = decode_hex_32(raw.trim())
        {
            return bytes;
        }
        let mut bytes = [0u8; 32];
        if getrandom::fill(&mut bytes).is_err() {
            // Extremely unlikely; fall back to a hash of the path + boot time
            // rather than aborting the request path.
            let fallback = blake3::hash(
                format!("{}:{:?}", path.display(), std::time::SystemTime::now()).as_bytes(),
            );
            bytes.copy_from_slice(fallback.as_bytes());
        }
        persist_salt(&path, &bytes);
        bytes
    })
}

fn persist_salt(path: &std::path::Path, bytes: &[u8; 32]) {
    let hex: String = bytes.iter().fold(String::new(), |mut acc, b| {
        use std::fmt::Write as _;
        let _ = write!(acc, "{b:02x}");
        acc
    });
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    match opts.open(path) {
        Ok(mut f) => {
            let _ = f.write_all(hex.as_bytes());
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {} // raced by a sibling process
        Err(e) => {
            tracing::warn!(
                "gateway PII salt not persisted ({}): {e} — pseudonyms will rotate on restart",
                path.display()
            );
        }
    }
}

fn decode_hex_32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16)?;
        let lo = (chunk[1] as char).to_digit(16)?;
        out[i] = u8::try_from(hi * 16 + lo).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pseudonym_is_stable_normalized_and_prefixed() {
        let _iso = lean_ctx::core::data_dir::isolated_data_dir();
        let a = pseudonymize("Yves@Acme.com ");
        let b = pseudonymize("yves@acme.com");
        assert_eq!(a, b, "normalization must collapse case/whitespace");
        assert!(a.starts_with(PSEUDONYM_PREFIX));
        assert_eq!(a.len(), PSEUDONYM_PREFIX.len() + 16);
        // Idempotent: pseudonymizing a pseudonym is a no-op.
        assert_eq!(pseudonymize(&a), a);
        // Different persons → different pseudonyms.
        assert_ne!(pseudonymize("mara@acme.com"), a);
    }

    #[test]
    fn salt_persists_across_calls() {
        let _iso = lean_ctx::core::data_dir::isolated_data_dir();
        let first = pseudonymize("someone@acme.com");
        // Salt is cached in-process; the file must exist for restarts.
        assert_eq!(pseudonymize("someone@acme.com"), first);
    }

    #[test]
    fn match_keys_cover_raw_and_pseudonym() {
        let _iso = lean_ctx::core::data_dir::isolated_data_dir();
        let keys = person_match_keys("gdpr@acme.com");
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0], "gdpr@acme.com");
        assert!(keys[1].starts_with(PSEUDONYM_PREFIX));
    }

    #[test]
    fn hex_roundtrip() {
        let bytes = [7u8; 32];
        let hex = lean_ctx::core::agent_identity::hex_encode(&bytes);
        assert_eq!(decode_hex_32(&hex), Some(bytes));
        assert_eq!(decode_hex_32("zz"), None);
    }
}
