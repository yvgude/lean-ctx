//! Canonical serialization and content-addressed identity for snapshots.
//!
//! The `snapshot_id` is the BLAKE3 hex of the *canonical body*: the snapshot
//! serialized with `snapshot_id` blanked and `signature` cleared. Because the
//! type contains only structs / `Vec`s / `Option`s (no maps), `serde_json`'s
//! field-declaration order makes the encoding deterministic — the same layer
//! state always produces the same id (modulo `created_at`), so downstream
//! prompt caches and signatures stay byte-stable (#498).

use super::types::ContextSnapshotV1;

/// Serialize the canonical body used for hashing and signing: the snapshot with
/// `snapshot_id` blanked and `signature` removed, compact JSON.
pub fn canonical_body(snapshot: &ContextSnapshotV1) -> Result<String, String> {
    let mut body = snapshot.clone();
    body.snapshot_id = String::new();
    body.signature = None;
    serde_json::to_string(&body).map_err(|e| format!("canonical serialize: {e}"))
}

/// Compute the content-addressed id (BLAKE3 hex of the canonical body) without
/// mutating the snapshot.
pub fn compute_id(snapshot: &ContextSnapshotV1) -> Result<String, String> {
    Ok(crate::core::hasher::hash_str(&canonical_body(snapshot)?))
}

/// Set `snapshot_id` to the freshly computed content hash and return it.
pub fn finalize_id(snapshot: &mut ContextSnapshotV1) -> Result<String, String> {
    let id = compute_id(snapshot)?;
    snapshot.snapshot_id.clone_from(&id);
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::super::types::{ContextSnapshotV1, SnapshotSignatureV1};
    use super::*;

    fn sample() -> ContextSnapshotV1 {
        ContextSnapshotV1::new("2026-01-01T00:00:00Z".into(), "9.9.9".into())
    }

    #[test]
    fn id_is_deterministic() {
        let a = compute_id(&sample()).expect("id a");
        let b = compute_id(&sample()).expect("id b");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64, "BLAKE3 hex is 64 chars");
    }

    #[test]
    fn id_ignores_existing_id_and_signature() {
        let mut with_noise = sample();
        with_noise.snapshot_id = "deadbeef".into();
        with_noise.signature = Some(SnapshotSignatureV1 {
            algorithm: "ed25519".into(),
            public_key: "aa".into(),
            value: "bb".into(),
        });
        // The id must be computed over the body only, so the noise is irrelevant.
        assert_eq!(
            compute_id(&with_noise).expect("noisy"),
            compute_id(&sample()).expect("clean")
        );
    }

    #[test]
    fn id_changes_with_content() {
        let mut other = sample();
        other.git.dirty = true;
        assert_ne!(
            compute_id(&sample()).expect("a"),
            compute_id(&other).expect("b")
        );
    }

    #[test]
    fn finalize_sets_the_id_field() {
        let mut snap = sample();
        let id = finalize_id(&mut snap).expect("finalize");
        assert_eq!(snap.snapshot_id, id);
        assert!(!id.is_empty());
    }
}
