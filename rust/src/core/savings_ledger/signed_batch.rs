//! Signed savings batch (G5) — the exportable, tamper-evident ROI/compliance artifact.
//!
//! The local ledger is already an append-only SHA-256 hash chain (see `store.rs`): editing,
//! reordering, inserting or deleting any past event breaks `verify()`. A [`SignedSavingsBatchV1`]
//! turns that internal guarantee into a *portable* one: it commits the chain head
//! (`last_entry_hash`) plus the aggregate totals, and signs the whole thing with the machine's
//! Ed25519 key (`agent_identity`). A third party can then verify, **offline and without the raw
//! ledger**, that (1) the artifact was produced by the holder of a specific public key (origin),
//! and (2) it has not been altered by a single byte since signing (integrity). The embedded
//! `last_entry_hash` binds those totals to the exact append-only chain that produced them. No raw
//! events, file paths or code ever leave the machine — only aggregates.

use std::path::{Path, PathBuf};

use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};

use super::LedgerSummary;
use super::store::GENESIS;

const SCHEMA_VERSION: u32 = 1;
const KIND: &str = "lean-ctx.savings-batch";
/// Cap on per-model / per-tool rows embedded in the artifact (keeps it compact + bounded).
const MAX_BREAKDOWN_ROWS: usize = 8;

/// Compact, privacy-preserving aggregates copied from [`LedgerSummary`]. No raw events,
/// timestamps, paths or repo attribution — only the numbers an auditor needs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchTotals {
    pub total_events: usize,
    pub saved_tokens: u64,
    pub net_saved_tokens: u64,
    pub saved_usd: f64,
    pub bounce_tokens: u64,
    pub bounce_events: usize,
    pub tokenizers: Vec<String>,
    /// `(model_id, saved_tokens, saved_usd)`, top rows by tokens.
    pub by_model: Vec<(String, u64, f64)>,
    /// `(tool, saved_tokens)`, top rows by tokens.
    pub by_tool: Vec<(String, u64)>,
}

impl BatchTotals {
    fn from_summary(s: &LedgerSummary) -> Self {
        let mut by_model = s.by_model.clone();
        by_model.truncate(MAX_BREAKDOWN_ROWS);
        let mut by_tool = s.by_tool.clone();
        by_tool.truncate(MAX_BREAKDOWN_ROWS);
        Self {
            total_events: s.total_events,
            saved_tokens: s.saved_tokens,
            net_saved_tokens: s.net_saved_tokens(),
            saved_usd: round_usd(s.saved_usd),
            bounce_tokens: s.bounce_tokens,
            bounce_events: s.bounce_events,
            tokenizers: s.tokenizers.clone(),
            by_model: by_model
                .into_iter()
                .map(|(m, t, u)| (m, t, round_usd(u)))
                .collect(),
            by_tool,
        }
    }
}

/// USD rounded to 6 decimals so the signed value matches `SavingsEvent`'s canonical content
/// precision and survives a JSON round-trip identically on both sign and verify.
fn round_usd(v: f64) -> f64 {
    (v * 1_000_000.0).round() / 1_000_000.0
}

/// A signed, exportable attestation of realized savings over the local ledger.
///
/// `signature` / `signer_public_key` are excluded from the signed payload (set to `None` while
/// computing the canonical bytes), exactly like `handoff_transfer_bundle::sign_bundle`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignedSavingsBatchV1 {
    pub schema_version: u32,
    /// Discriminator so a verifier can refuse unrelated signed JSON.
    pub kind: String,
    pub created_at: String,
    pub lean_ctx_version: String,
    pub agent_id: String,
    /// Coverage window. `"all"` = whole ledger (the only window built today).
    pub period: String,
    /// First event's `entry_hash` (or `genesis` for an empty ledger).
    pub first_entry_hash: String,
    /// Chain head — the SHA-256 tip that commits the entire event history.
    pub last_entry_hash: String,
    /// Whether the SHA-256 chain verified intact at signing time.
    pub chain_valid: bool,
    pub totals: BatchTotals,
    /// Ed25519 public key (hex). `None` until signed.
    pub signer_public_key: Option<String>,
    /// Ed25519 signature over the canonical bytes (hex). `None` until signed.
    pub signature: Option<String>,
}

/// Outcome of verifying a [`SignedSavingsBatchV1`].
#[derive(Debug, Clone, PartialEq)]
pub struct BatchVerifyResult {
    /// Ed25519 signature is present and valid over the canonical payload.
    pub signature_valid: bool,
    /// The signer's public key (hex), if the artifact carried one.
    pub signer_public_key: Option<String>,
    /// Human-readable reason when `signature_valid` is false.
    pub error: Option<String>,
}

impl SignedSavingsBatchV1 {
    /// Builds an unsigned batch over the whole local ledger (no IO beyond reading it).
    #[must_use]
    pub fn build_all(agent_id: &str) -> Self {
        let events = super::all_events();
        let summary = super::summary();
        let chain_valid = super::verify().valid;
        Self::from_parts(
            agent_id,
            "all",
            &events_head_tail(&events),
            chain_valid,
            &summary,
        )
    }

    /// Pure constructor (testable without touching the real data dir).
    fn from_parts(
        agent_id: &str,
        period: &str,
        (count, first_hash, last_hash): &(usize, String, String),
        chain_valid: bool,
        summary: &LedgerSummary,
    ) -> Self {
        let _ = count; // total_events lives in totals; head/tail count is informational only
        Self {
            schema_version: SCHEMA_VERSION,
            kind: KIND.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            lean_ctx_version: env!("CARGO_PKG_VERSION").to_string(),
            agent_id: agent_id.to_string(),
            period: period.to_string(),
            first_entry_hash: first_hash.clone(),
            last_entry_hash: last_hash.clone(),
            chain_valid,
            totals: BatchTotals::from_summary(summary),
            signer_public_key: None,
            signature: None,
        }
    }

    /// Deterministic bytes that get signed/verified: the whole struct with the two signature
    /// fields cleared. Identical on sign and verify regardless of JSON float formatting.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, String> {
        let mut clone = self.clone();
        clone.signature = None;
        clone.signer_public_key = None;
        serde_json::to_vec(&clone).map_err(|e| format!("serialize for signing: {e}"))
    }

    /// Signs with the persistent machine identity (`agent_identity` keystore).
    pub fn sign(&mut self, agent_id: &str) -> Result<(), String> {
        let key = crate::core::agent_identity::get_or_create_keypair(agent_id)?;
        self.sign_with_key(&key)
    }

    /// Signs with an explicit key (used by `sign` and by hermetic tests). Sets both signature
    /// fields; the public key is embedded so the artifact is self-verifying.
    pub fn sign_with_key(&mut self, key: &SigningKey) -> Result<(), String> {
        self.signature = None;
        self.signer_public_key = None;
        let canonical = self.canonical_bytes()?;
        let sig = key.sign(&canonical);
        self.signer_public_key = Some(crate::core::agent_identity::hex_encode(
            &key.verifying_key().to_bytes(),
        ));
        self.signature = Some(crate::core::agent_identity::hex_encode(&sig.to_bytes()));
        Ok(())
    }

    /// Verifies the embedded signature against the embedded public key — offline, no ledger
    /// needed. A failure means the artifact was altered or was never validly signed.
    #[must_use]
    pub fn verify(&self) -> BatchVerifyResult {
        let fail = |msg: &str| BatchVerifyResult {
            signature_valid: false,
            signer_public_key: self.signer_public_key.clone(),
            error: Some(msg.to_string()),
        };

        let (Some(sig_hex), Some(pk_hex)) = (&self.signature, &self.signer_public_key) else {
            return fail("artifact is not signed");
        };
        let (Ok(sig_bytes), Ok(pk_bytes)) = (
            crate::core::agent_identity::hex_decode(sig_hex),
            crate::core::agent_identity::hex_decode(pk_hex),
        ) else {
            return fail("malformed signature or public key hex");
        };
        let canonical = match self.canonical_bytes() {
            Ok(c) => c,
            Err(e) => return fail(&e),
        };
        if crate::core::agent_identity::verify_signature(&pk_bytes, &canonical, &sig_bytes) {
            BatchVerifyResult {
                signature_valid: true,
                signer_public_key: Some(pk_hex.clone()),
                error: None,
            }
        } else {
            fail("signature does not match payload (tampered or wrong key)")
        }
    }
}

/// `(count, first_entry_hash, last_entry_hash)` for a loaded event list.
fn events_head_tail(events: &[super::SavingsEvent]) -> (usize, String, String) {
    let first = events
        .first()
        .map_or_else(|| GENESIS.to_string(), |e| e.entry_hash.clone());
    let last = events
        .last()
        .map_or_else(|| GENESIS.to_string(), |e| e.entry_hash.clone());
    (events.len(), first, last)
}

/// Default artifact location: `<data_dir>/savings/signed-batch-v1_<utc-stamp>.json`.
pub fn default_artifact_path() -> Result<PathBuf, String> {
    let dir = crate::core::data_dir::lean_ctx_data_dir()?.join("savings");
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir savings: {e}"))?;
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    Ok(dir.join(format!("signed-batch-v1_{stamp}.json")))
}

/// Pretty-prints the artifact to `out` (creating parent dirs). Returns the written path.
pub fn write_artifact(batch: &SignedSavingsBatchV1, out: &Path) -> Result<PathBuf, String> {
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }
    let json = serde_json::to_string_pretty(batch).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(out, json).map_err(|e| format!("write {}: {e}", out.display()))?;
    Ok(out.to_path_buf())
}

/// Loads and parses a signed batch artifact, rejecting unrelated JSON by `kind`.
pub fn load_artifact(path: &Path) -> Result<SignedSavingsBatchV1, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let batch: SignedSavingsBatchV1 =
        serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
    if batch.kind != KIND {
        return Err(format!("not a {KIND} artifact (kind = {:?})", batch.kind));
    }
    Ok(batch)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(saved: u64, usd: f64) -> LedgerSummary {
        LedgerSummary {
            total_events: 2,
            saved_tokens: saved,
            saved_usd: usd,
            bounce_tokens: 0,
            bounce_events: 0,
            tokenizers: vec!["o200k_base".into()],
            by_model: vec![("claude-opus".into(), saved, usd)],
            by_day: vec![],
            by_tool: vec![("ctx_read".into(), saved)],
        }
    }

    fn batch() -> SignedSavingsBatchV1 {
        SignedSavingsBatchV1::from_parts(
            "local",
            "all",
            &(2, "firsthash".into(), "lasthash".into()),
            true,
            &summary(800, 0.0024),
        )
    }

    fn key() -> SigningKey {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).unwrap();
        SigningKey::from_bytes(&seed)
    }

    #[test]
    fn canonical_bytes_exclude_signature_fields() {
        let mut b = batch();
        let before = b.canonical_bytes().unwrap();
        b.signature = Some("deadbeef".into());
        b.signer_public_key = Some("cafe".into());
        let after = b.canonical_bytes().unwrap();
        assert_eq!(
            before, after,
            "signature fields must not affect signed bytes"
        );
    }

    #[test]
    fn sign_then_verify_roundtrips() {
        let mut b = batch();
        b.sign_with_key(&key()).unwrap();
        let res = b.verify();
        assert!(
            res.signature_valid,
            "freshly signed batch must verify: {res:?}"
        );
        assert!(b.signature.is_some() && b.signer_public_key.is_some());
    }

    #[test]
    fn verify_detects_tampered_totals() {
        let mut b = batch();
        b.sign_with_key(&key()).unwrap();
        b.totals.saved_tokens = 999_999_999; // forge a bigger number after signing
        assert!(
            !b.verify().signature_valid,
            "edited totals must fail verification"
        );
    }

    #[test]
    fn verify_detects_tampered_chain_head() {
        let mut b = batch();
        b.sign_with_key(&key()).unwrap();
        b.last_entry_hash = "0000000000000000".into();
        assert!(
            !b.verify().signature_valid,
            "rewriting the chain head must fail"
        );
    }

    #[test]
    fn verify_rejects_unsigned_and_wrong_key() {
        let unsigned = batch();
        assert!(
            !unsigned.verify().signature_valid,
            "unsigned batch is not valid"
        );

        // Valid signature, but swap in a different public key → must fail.
        let mut b = batch();
        b.sign_with_key(&key()).unwrap();
        b.signer_public_key = Some(crate::core::agent_identity::hex_encode(
            &key().verifying_key().to_bytes(),
        ));
        assert!(
            !b.verify().signature_valid,
            "mismatched public key must fail"
        );
    }

    #[test]
    fn artifact_write_load_roundtrip_preserves_signature() {
        let mut b = batch();
        b.sign_with_key(&key()).unwrap();

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "lc-signed-batch-{}-{nanos}.json",
            std::process::id()
        ));

        write_artifact(&b, &path).unwrap();
        let loaded = load_artifact(&path).unwrap();
        assert_eq!(loaded, b, "round-trip must be byte-faithful");
        assert!(
            loaded.verify().signature_valid,
            "loaded artifact still verifies"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_artifact_rejects_foreign_json() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path =
            std::env::temp_dir().join(format!("lc-foreign-{}-{nanos}.json", std::process::id()));
        std::fs::write(&path, r#"{"hello":"world"}"#).unwrap();
        assert!(
            load_artifact(&path).is_err(),
            "non-batch JSON must be rejected"
        );
        let _ = std::fs::remove_file(&path);
    }
}
