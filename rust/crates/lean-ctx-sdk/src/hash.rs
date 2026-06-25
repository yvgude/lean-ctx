//! Content hashing — BLAKE3 hex digests, the engine's canonical hash.

/// Hex-encoded BLAKE3 hash of `data` (64 hex chars).
#[must_use]
pub fn blake3_hex(data: &[u8]) -> String {
    lean_ctx::core::hasher::hash_hex(data)
}

/// Convenience: hash a string slice.
#[must_use]
pub fn blake3_str(s: &str) -> String {
    lean_ctx::core::hasher::hash_str(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_deterministic_64_hex() {
        let a = blake3_str("hello");
        assert_eq!(a.len(), 64);
        assert_eq!(a, blake3_str("hello"));
        assert_ne!(a, blake3_str("world"));
    }
}
