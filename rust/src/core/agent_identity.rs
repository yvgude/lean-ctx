use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Canonical resolver for the current agent identity. Reads `LEAN_CTX_AGENT_ID`
/// (or legacy `LCTX_AGENT_ID`), falling back to `"local"`. Resolved once per
/// process and cached, so all subsystems (heatmap, savings ledger, audit)
/// attribute traces to the same identity.
#[must_use]
pub fn current_agent_id() -> &'static str {
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE.get_or_init(|| {
        std::env::var("LEAN_CTX_AGENT_ID")
            .or_else(|_| std::env::var("LCTX_AGENT_ID"))
            .unwrap_or_else(|_| "local".to_string())
    })
}

pub fn get_or_create_keypair(agent_id: &str) -> Result<SigningKey, String> {
    let path = key_path(agent_id)?;
    if path.exists() {
        load_key(&path)
    } else {
        generate_and_save(agent_id)
    }
}

pub fn get_public_key(agent_id: &str) -> Result<VerifyingKey, String> {
    let key = get_or_create_keypair(agent_id)?;
    Ok(key.verifying_key())
}

pub fn sign_bytes(agent_id: &str, data: &[u8]) -> Result<Vec<u8>, String> {
    let key = get_or_create_keypair(agent_id)?;
    let sig = key.sign(data);
    Ok(sig.to_bytes().to_vec())
}

/// Sign `data` and return the signature together with the verifying key of
/// the SAME keypair — one atomic key-store resolution.
///
/// Callers that embed both the signature and the public key MUST use this
/// instead of separate `sign_bytes` + `get_public_key` calls: those perform
/// two independent store reads, and when the store location or key file
/// changes in between (env-driven data-dir moves under test, key
/// regeneration by a concurrent process), the embedded public key belongs to
/// a different keypair than the signature — which then can never verify.
pub fn sign_with_public_key(
    agent_id: &str,
    data: &[u8],
) -> Result<(Vec<u8>, VerifyingKey), String> {
    let key = get_or_create_keypair(agent_id)?;
    let sig = key.sign(data);
    Ok((sig.to_bytes().to_vec(), key.verifying_key()))
}

/// Sign with an already-resolved keypair (no store access). Pair with
/// [`get_or_create_keypair`] when the public key must be embedded in the
/// payload *before* the signature is computed over it.
#[must_use]
pub fn sign_bytes_with(key: &SigningKey, data: &[u8]) -> Vec<u8> {
    key.sign(data).to_bytes().to_vec()
}

#[must_use]
pub fn verify_signature(public_key_bytes: &[u8], data: &[u8], signature_bytes: &[u8]) -> bool {
    let pk_bytes: [u8; 32] = match public_key_bytes.try_into() {
        Ok(b) => b,
        Err(_) => return false,
    };
    let Ok(verifying_key) = VerifyingKey::from_bytes(&pk_bytes) else {
        return false;
    };
    let sig_bytes: [u8; 64] = match signature_bytes.try_into() {
        Ok(b) => b,
        Err(_) => return false,
    };
    let signature = Signature::from_bytes(&sig_bytes);
    verifying_key.verify(data, &signature).is_ok()
}

#[must_use]
pub fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

pub fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err("odd-length hex string".to_string());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

fn key_path(agent_id: &str) -> Result<PathBuf, String> {
    let base = crate::core::data_dir::lean_ctx_data_dir()?;
    Ok(base.join("keys").join(format!("{agent_id}.key")))
}

fn pub_key_path(agent_id: &str) -> Result<PathBuf, String> {
    let base = crate::core::data_dir::lean_ctx_data_dir()?;
    Ok(base.join("keys").join(format!("{agent_id}.pub")))
}

fn generate_and_save(agent_id: &str) -> Result<SigningKey, String> {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| format!("CSPRNG unavailable: {e}"))?;
    let signing_key = SigningKey::from_bytes(&seed);

    let key_file = key_path(agent_id)?;
    let pub_file = pub_key_path(agent_id)?;

    if let Some(parent) = key_file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir keys: {e}"))?;
    }

    std::fs::write(&key_file, signing_key.to_bytes()).map_err(|e| format!("write key: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        let _ = std::fs::set_permissions(&key_file, perms);
    }

    let pub_bytes = signing_key.verifying_key().to_bytes();
    std::fs::write(&pub_file, pub_bytes).map_err(|e| format!("write pub: {e}"))?;

    Ok(signing_key)
}

fn load_key(path: &Path) -> Result<SigningKey, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read key: {e}"))?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "invalid key file (expected 32 bytes)".to_string())?;
    Ok(SigningKey::from_bytes(&arr))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify_roundtrip() {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).unwrap();
        let key = SigningKey::from_bytes(&seed);
        let data = b"test payload";
        let sig = key.sign(data);

        let pub_bytes = key.verifying_key().to_bytes();
        assert!(verify_signature(&pub_bytes, data, &sig.to_bytes()));
    }

    #[test]
    fn verify_rejects_tampered_data() {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).unwrap();
        let key = SigningKey::from_bytes(&seed);
        let sig = key.sign(b"original");

        let pub_bytes = key.verifying_key().to_bytes();
        assert!(!verify_signature(&pub_bytes, b"tampered", &sig.to_bytes()));
    }

    #[test]
    fn hex_roundtrip() {
        let data = vec![0xde, 0xad, 0xbe, 0xef];
        let encoded = hex_encode(&data);
        assert_eq!(encoded, "deadbeef");
        let decoded = hex_decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }
}
