//! Encrypted index bundles for the hosted Personal Index (GL #392).
//!
//! Packs the locally built retrieval artifacts (`bm25_index.bin.zst`,
//! `embeddings.json` from the project's vector namespace) into one container,
//! encrypts it client-side, and unpacks pulled bundles back into the
//! namespace — so a fresh device gets working `ctx_semantic_search` without a
//! local re-index.
//!
//! Contract: `docs/contracts/hosted-personal-index-v1.md`.
//!
//! ## Container format (`LCIB1`)
//!
//! ```text
//! "LCIB1\n" | u32 LE manifest_len | manifest JSON | zstd(files payload)
//! ```
//!
//! ## Encryption
//!
//! XChaCha20-Poly1305 with a 24-byte random nonce prepended to the
//! ciphertext. The key is HKDF-SHA256-derived from the account API key —
//! the backend stores that key only as a SHA-256 hash, so the server can
//! never decrypt a bundle (true E2E for the operator threat model). Every
//! logged-in device derives the same key with zero extra setup.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

const MAGIC: &[u8; 6] = b"LCIB1\n";
const ZSTD_LEVEL: i32 = 3;
const NONCE_LEN: usize = 24;
/// Hard ceiling for a decoded payload (defense against decompression bombs
/// on pull; the server enforces its own per-bundle upload cap).
const MAX_DECODED_BYTES: usize = 512 * 1024 * 1024;

/// The two retrieval artifacts a v1 bundle carries.
const BUNDLE_FILES: [&str; 2] = ["bm25_index.bin.zst", "embeddings.json"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleManifest {
    /// Format version of the container ("1").
    pub version: u32,
    /// Project namespace hash this bundle belongs to.
    pub project_hash: String,
    /// RFC 3339 creation timestamp.
    pub created_at: String,
    /// Engine version that produced the bundle.
    pub engine_version: String,
    pub files: Vec<BundleFileEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleFileEntry {
    pub name: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    #[error(
        "no index artifacts found in {0} — run a search once (or `lean-ctx index`) to build the local index first"
    )]
    NothingToBundle(String),
    #[error("not a lean-ctx index bundle (bad magic)")]
    BadMagic,
    #[error("corrupt bundle: {0}")]
    Corrupt(String),
    #[error(
        "decryption failed — bundle was encrypted with a different account key (re-push from a logged-in device)"
    )]
    Decrypt,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    let digest = h.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}

// ---------------------------------------------------------------------------
// Pack / unpack (plaintext container)
// ---------------------------------------------------------------------------

/// Whether this project has any bundleable index artifacts on disk — the
/// cheap pre-check the background auto-push (GL #392) uses to skip silently
/// instead of erroring through [`pack`].
#[must_use]
pub fn local_index_present(project_root: &Path) -> bool {
    let dir = crate::core::index_namespace::vectors_dir(project_root);
    BUNDLE_FILES.iter().any(|name| dir.join(name).is_file())
}

/// Pack the project's index artifacts into a plaintext `LCIB1` container.
/// Returns the container bytes and its manifest.
pub fn pack(project_root: &Path) -> Result<(Vec<u8>, BundleManifest), BundleError> {
    let dir = crate::core::index_namespace::vectors_dir(project_root);
    let mut files = Vec::new();
    let mut payload = Vec::new();

    for name in BUNDLE_FILES {
        let path = dir.join(name);
        let Ok(data) = std::fs::read(&path) else {
            continue;
        };
        files.push(BundleFileEntry {
            name: name.to_string(),
            size: data.len() as u64,
            sha256: sha256_hex(&data),
        });
        payload.extend_from_slice(&data);
    }

    if files.is_empty() {
        return Err(BundleError::NothingToBundle(dir.display().to_string()));
    }

    let manifest = BundleManifest {
        version: 1,
        // The vector-namespace hash: derived from the project *identity*
        // (git remote / manifest name), so the same repo cloned on another
        // device maps to the same hosted bucket.
        project_hash: crate::core::index_namespace::namespace_hash(project_root),
        created_at: chrono::Utc::now().to_rfc3339(),
        engine_version: env!("CARGO_PKG_VERSION").to_string(),
        files,
    };

    let manifest_json =
        serde_json::to_vec(&manifest).map_err(|e| BundleError::Corrupt(e.to_string()))?;
    let compressed = zstd::encode_all(payload.as_slice(), ZSTD_LEVEL)
        .map_err(|e| BundleError::Corrupt(format!("zstd: {e}")))?;

    let mut out = Vec::with_capacity(MAGIC.len() + 4 + manifest_json.len() + compressed.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(
        &u32::try_from(manifest_json.len())
            .map_err(|_| BundleError::Corrupt("manifest too large".into()))?
            .to_le_bytes(),
    );
    out.extend_from_slice(&manifest_json);
    out.extend_from_slice(&compressed);
    Ok((out, manifest))
}

/// Parse a plaintext container without writing anything (manifest preview).
pub fn read_manifest(container: &[u8]) -> Result<BundleManifest, BundleError> {
    let (manifest, _payload) = split_container(container)?;
    Ok(manifest)
}

fn split_container(container: &[u8]) -> Result<(BundleManifest, Vec<u8>), BundleError> {
    if container.len() < MAGIC.len() + 4 || &container[..MAGIC.len()] != MAGIC {
        return Err(BundleError::BadMagic);
    }
    let len_start = MAGIC.len();
    let manifest_len = u32::from_le_bytes(
        container[len_start..len_start + 4]
            .try_into()
            .map_err(|_| BundleError::Corrupt("truncated header".into()))?,
    ) as usize;
    let manifest_end = len_start + 4 + manifest_len;
    if container.len() < manifest_end {
        return Err(BundleError::Corrupt("truncated manifest".into()));
    }
    let manifest: BundleManifest = serde_json::from_slice(&container[len_start + 4..manifest_end])
        .map_err(|e| BundleError::Corrupt(format!("manifest: {e}")))?;

    let declared: u64 = manifest.files.iter().map(|f| f.size).sum();
    if declared > MAX_DECODED_BYTES as u64 {
        return Err(BundleError::Corrupt(format!(
            "declared payload {declared} bytes exceeds the {MAX_DECODED_BYTES} byte ceiling"
        )));
    }

    let payload = zstd::decode_all(&container[manifest_end..])
        .map_err(|e| BundleError::Corrupt(format!("zstd: {e}")))?;
    if payload.len() as u64 != declared {
        return Err(BundleError::Corrupt(format!(
            "payload size mismatch: got {}, manifest declares {declared}",
            payload.len()
        )));
    }
    Ok((manifest, payload))
}

/// Unpack a plaintext container into the project's vector namespace. Every
/// file's SHA-256 is verified before anything is written; writes are atomic
/// (tmp + rename) so a torn pull can never corrupt a working local index.
pub fn unpack(project_root: &Path, container: &[u8]) -> Result<BundleManifest, BundleError> {
    let (manifest, payload) = split_container(container)?;

    let dir = crate::core::index_namespace::vectors_dir(project_root);
    std::fs::create_dir_all(&dir)?;

    // Verify all hashes first — only then start writing.
    let mut offset = 0usize;
    let mut verified: Vec<(&BundleFileEntry, &[u8])> = Vec::with_capacity(manifest.files.len());
    for entry in &manifest.files {
        let size = usize::try_from(entry.size)
            .map_err(|_| BundleError::Corrupt("file size overflow".into()))?;
        let end = offset
            .checked_add(size)
            .filter(|&e| e <= payload.len())
            .ok_or_else(|| BundleError::Corrupt("file extends past payload".into()))?;
        let data = &payload[offset..end];
        if sha256_hex(data) != entry.sha256 {
            return Err(BundleError::Corrupt(format!(
                "sha256 mismatch for {}",
                entry.name
            )));
        }
        // File names are fixed by the format — never trust path components.
        if !BUNDLE_FILES.contains(&entry.name.as_str()) {
            return Err(BundleError::Corrupt(format!(
                "unexpected file in bundle: {}",
                entry.name
            )));
        }
        verified.push((entry, data));
        offset = end;
    }

    for (entry, data) in verified {
        let target = dir.join(&entry.name);
        let tmp = dir.join(format!(".{}.pull.tmp", entry.name));
        std::fs::write(&tmp, data)?;
        std::fs::rename(&tmp, &target)?;
    }
    Ok(manifest)
}

// ---------------------------------------------------------------------------
// Encryption (XChaCha20-Poly1305, HKDF-SHA256 account key)
// ---------------------------------------------------------------------------

/// Derive the per-account bundle key from the API key. The server only ever
/// stores `sha256(api_key)`, so this key is unknowable server-side.
#[must_use]
pub fn derive_key(api_key: &str) -> [u8; 32] {
    let hk = hkdf::Hkdf::<Sha256>::new(Some(b"leanctx"), api_key.as_bytes());
    let mut okm = [0u8; 32];
    hk.expand(b"index-bundle-v1", &mut okm)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    okm
}

/// Encrypt a plaintext container. Output: `nonce (24B) || ciphertext`.
pub fn encrypt(container: &[u8], key: &[u8; 32]) -> Result<Vec<u8>, BundleError> {
    use chacha20poly1305::aead::{Aead, KeyInit};
    use chacha20poly1305::{XChaCha20Poly1305, XNonce};

    let mut nonce_bytes = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce_bytes)
        .map_err(|e| BundleError::Corrupt(format!("nonce generation: {e}")))?;
    let cipher = XChaCha20Poly1305::new(key.into());
    let ciphertext = cipher
        .encrypt(XNonce::from_slice(&nonce_bytes), container)
        .map_err(|_| BundleError::Corrupt("encryption failed".into()))?;

    let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt `nonce || ciphertext` back into the plaintext container.
pub fn decrypt(blob: &[u8], key: &[u8; 32]) -> Result<Vec<u8>, BundleError> {
    use chacha20poly1305::aead::{Aead, KeyInit};
    use chacha20poly1305::{XChaCha20Poly1305, XNonce};

    if blob.len() <= NONCE_LEN {
        return Err(BundleError::Corrupt("blob shorter than nonce".into()));
    }
    let (nonce, ciphertext) = blob.split_at(NONCE_LEN);
    let cipher = XChaCha20Poly1305::new(key.into());
    cipher
        .decrypt(XNonce::from_slice(nonce), ciphertext)
        .map_err(|_| BundleError::Decrypt)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a fake project with index artifacts and return (dir, root).
    fn project_with_index() -> (tempfile::TempDir, std::path::PathBuf) {
        let data_dir = tempfile::tempdir().unwrap();
        let root = data_dir.path().join("proj");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", data_dir.path()) };

        let vectors = crate::core::index_namespace::vectors_dir(&root);
        std::fs::create_dir_all(&vectors).unwrap();
        std::fs::write(vectors.join("bm25_index.bin.zst"), b"fake-bm25-bytes").unwrap();
        std::fs::write(vectors.join("embeddings.json"), br#"{"entries":[]}"#).unwrap();
        (data_dir, root)
    }

    #[test]
    fn pack_unpack_roundtrip_preserves_artifacts() {
        let _env = crate::core::data_dir::test_env_lock();
        let (data_dir, root) = project_with_index();

        let (container, manifest) = pack(&root).unwrap();
        assert_eq!(manifest.version, 1);
        assert_eq!(manifest.files.len(), 2);

        // Wipe the local artifacts, then restore from the bundle.
        let vectors = crate::core::index_namespace::vectors_dir(&root);
        std::fs::remove_file(vectors.join("bm25_index.bin.zst")).unwrap();
        std::fs::remove_file(vectors.join("embeddings.json")).unwrap();

        let restored = unpack(&root, &container).unwrap();
        assert_eq!(restored.project_hash, manifest.project_hash);
        assert_eq!(
            std::fs::read(vectors.join("bm25_index.bin.zst")).unwrap(),
            b"fake-bm25-bytes"
        );
        assert_eq!(
            std::fs::read(vectors.join("embeddings.json")).unwrap(),
            br#"{"entries":[]}"#
        );
        drop(data_dir);
    }

    #[test]
    fn pack_without_artifacts_is_a_clear_error() {
        let _env = crate::core::data_dir::test_env_lock();
        let data_dir = tempfile::tempdir().unwrap();
        let root = data_dir.path().join("empty");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", data_dir.path()) };

        match pack(&root) {
            Err(BundleError::NothingToBundle(_)) => {}
            other => panic!("expected NothingToBundle, got {other:?}"),
        }
    }

    #[test]
    fn tampered_payload_is_rejected() {
        let _env = crate::core::data_dir::test_env_lock();
        let (_data_dir, root) = project_with_index();
        let (container, _) = pack(&root).unwrap();

        // Re-compress a tampered payload behind the original manifest: the
        // per-file sha256 must catch it.
        let (manifest, mut payload) = split_container(&container).unwrap();
        payload[0] ^= 0xFF;
        let manifest_json = serde_json::to_vec(&manifest).unwrap();
        let mut forged = Vec::new();
        forged.extend_from_slice(MAGIC);
        forged.extend_from_slice(&(u32::try_from(manifest_json.len()).unwrap()).to_le_bytes());
        forged.extend_from_slice(&manifest_json);
        forged.extend_from_slice(&zstd::encode_all(payload.as_slice(), 3).unwrap());

        match unpack(&root, &forged) {
            Err(BundleError::Corrupt(msg)) => assert!(msg.contains("sha256"), "{msg}"),
            other => panic!("expected Corrupt(sha256), got {other:?}"),
        }
    }

    #[test]
    fn bad_magic_is_rejected() {
        match read_manifest(b"not-a-bundle") {
            Err(BundleError::BadMagic) => {}
            other => panic!("expected BadMagic, got {other:?}"),
        }
    }

    #[test]
    fn encrypt_decrypt_roundtrip_and_wrong_key_fails() {
        let key = derive_key("test-api-key-1");
        let plaintext = b"LCIB1\n-fake-container-bytes".to_vec();

        let blob = encrypt(&plaintext, &key).unwrap();
        assert_ne!(&blob[NONCE_LEN..], plaintext.as_slice());
        assert_eq!(decrypt(&blob, &key).unwrap(), plaintext);

        let wrong = derive_key("test-api-key-2");
        match decrypt(&blob, &wrong) {
            Err(BundleError::Decrypt) => {}
            other => panic!("expected Decrypt error, got {other:?}"),
        }
    }

    #[test]
    fn key_derivation_is_stable_and_key_separated() {
        // Same input ⇒ same key (multi-device); different input ⇒ different key.
        assert_eq!(derive_key("k"), derive_key("k"));
        assert_ne!(derive_key("k"), derive_key("k2"));
        // And never the raw key material itself.
        assert_ne!(derive_key("k").as_slice(), b"k".as_slice());
    }

    #[test]
    fn nonces_are_unique_per_encryption() {
        let key = derive_key("k");
        let a = encrypt(b"same", &key).unwrap();
        let b = encrypt(b"same", &key).unwrap();
        assert_ne!(a[..NONCE_LEN], b[..NONCE_LEN]);
    }
}
