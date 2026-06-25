//! Publisher signing-key management for ctxpkg (GL #406).
//!
//! One ed25519 keypair per machine user, stored as a 32-byte hex seed under the
//! XDG data dir at `<data_dir>/keys/ctxpkg-ed25519.key` (mode 0600 on unix).
//! Created lazily on first `pack export --sign`. The public key identifies the
//! publisher across releases — registries and clients surface it per version.

use std::path::PathBuf;

use ed25519_dalek::SigningKey;

pub const KEY_REL_PATH: &str = "keys/ctxpkg-ed25519.key";

/// Resolves through the XDG data dir (not a hardcoded `~/.lean-ctx`) so the key
/// follows the migrated layout and never re-creates the legacy dir (GH #436).
pub fn key_path() -> Result<PathBuf, String> {
    Ok(crate::core::paths::data_dir()?.join(KEY_REL_PATH))
}

/// Load the signing key, creating it on first use. Returns the key and
/// whether it was newly generated (so the CLI can tell the user once).
pub fn load_or_create() -> Result<(SigningKey, bool), String> {
    let path = key_path()?;
    if path.exists() {
        let hex_seed =
            std::fs::read_to_string(&path).map_err(|e| format!("read signing key: {e}"))?;
        let seed = parse_seed(hex_seed.trim())?;
        return Ok((SigningKey::from_bytes(&seed), false));
    }

    let dir = path.parent().expect("key path has a parent");
    std::fs::create_dir_all(dir).map_err(|e| format!("create key dir: {e}"))?;

    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| format!("entropy source failed: {e}"))?;
    let encoded = hex_encode(&seed);

    std::fs::write(&path, &encoded).map_err(|e| format!("write signing key: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("chmod signing key: {e}"))?;
    }
    Ok((SigningKey::from_bytes(&seed), true))
}

/// Hex of the public verifying key — the publisher's stable identity.
#[must_use]
pub fn public_key_hex(key: &SigningKey) -> String {
    hex_encode(key.verifying_key().as_bytes())
}

fn parse_seed(s: &str) -> Result<[u8; 32], String> {
    if s.len() != 64 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "signing key file is corrupt (expected 64 hex chars, got {} chars) — \
             delete it to regenerate (this changes your publisher identity!)",
            s.len()
        ));
    }
    let mut seed = [0u8; 32];
    for (i, byte) in seed.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
            .map_err(|e| format!("signing key hex: {e}"))?;
    }
    Ok(seed)
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_roundtrip() {
        let seed = [42u8; 32];
        let encoded = hex_encode(&seed);
        assert_eq!(parse_seed(&encoded).expect("parses"), seed);
    }

    #[test]
    fn corrupt_seed_rejected() {
        assert!(parse_seed("zz").is_err());
        assert!(parse_seed(&"a".repeat(63)).is_err());
    }

    #[test]
    fn public_key_is_stable_for_seed() {
        let k1 = SigningKey::from_bytes(&[7u8; 32]);
        let k2 = SigningKey::from_bytes(&[7u8; 32]);
        assert_eq!(public_key_hex(&k1), public_key_hex(&k2));
        assert_eq!(public_key_hex(&k1).len(), 64);
    }
}
