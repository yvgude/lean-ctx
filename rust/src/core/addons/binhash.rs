//! Binary-hash pinning for stdio addons (P3 — supply-chain hardening).
//!
//! A stdio addon spawns a local executable. Pinning that binary's SHA-256 in
//! the manifest (`[mcp] sha256 = "…"`) closes the gap between *what was audited*
//! and *what actually runs*: if the file on `PATH` is swapped after install, the
//! hash no longer matches and the gateway refuses to spawn it.
//!
//! SHA-256 (not the engine's internal BLAKE3) is deliberate — an author pins the
//! value an ordinary `sha256sum my-mcp` / `shasum -a 256 my-mcp` prints, so the
//! pin is reproducible without lean-ctx.

use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Stream-hash a file and return its lowercase hex SHA-256. Streaming (8 KiB
/// chunks) keeps memory flat regardless of binary size.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("read {} failed: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(crate::core::agent_identity::hex_encode(&hasher.finalize()))
}

/// Resolve a stdio `command` to a concrete file path. An absolute/relative path
/// (anything containing a separator) is used as-is; a bare name is looked up on
/// `PATH`, honouring `PATHEXT`-free Unix semantics (first executable match).
#[must_use]
pub fn resolve_on_path(command: &str) -> Option<PathBuf> {
    let cmd = command.trim();
    if cmd.is_empty() {
        return None;
    }
    if cmd.contains('/') || cmd.contains('\\') {
        let p = PathBuf::from(cmd);
        return p.is_file().then_some(p);
    }
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|dir| dir.join(cmd))
        .find(|candidate| candidate.is_file())
}

/// Verify that `command` resolves to a binary whose SHA-256 equals
/// `expected_sha256`. An empty `expected_sha256` means "no pin" → `Ok`. The
/// comparison is case-insensitive over hex; any mismatch, unresolved binary, or
/// read error is a hard failure (fail-closed — a pin you cannot check is a pin
/// that failed).
pub fn verify_binary(command: &str, expected_sha256: &str) -> Result<(), String> {
    let expected = expected_sha256.trim();
    if expected.is_empty() {
        return Ok(());
    }
    let path = resolve_on_path(command).ok_or_else(|| {
        format!("binary `{command}` is pinned (sha256) but could not be found on PATH")
    })?;
    let actual = sha256_file(&path)?;
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(format!(
            "binary `{command}` failed its sha256 pin — expected {}…, got {}… ({} may have been \
             replaced)",
            short(expected),
            short(&actual),
            path.display()
        ))
    }
}

fn short(hex: &str) -> String {
    hex.chars().take(12).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique scratch dir per test (PID + label) so parallel tests never share
    /// — and clean up — the same directory.
    fn tmp(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("leanctx-binhash-{}-{label}", std::process::id()))
    }

    #[test]
    fn hashes_file_matching_known_sha256() {
        let dir = tmp("known");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("payload.bin");
        std::fs::write(&f, b"abc").unwrap();
        // Known SHA-256 of "abc".
        let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(sha256_file(&f).unwrap(), expected);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_pin_is_ok_without_resolving() {
        // No pin → never even touches the filesystem.
        assert!(verify_binary("definitely-not-a-real-binary-xyz", "").is_ok());
    }

    #[test]
    fn verify_matches_and_detects_mismatch() {
        let dir = tmp("verify");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("my-mcp");
        std::fs::write(&f, b"hello world").unwrap();
        let good = sha256_file(&f).unwrap();
        let path_str = f.to_string_lossy().to_string();

        assert!(verify_binary(&path_str, &good).is_ok());
        assert!(verify_binary(&path_str, &good.to_uppercase()).is_ok());
        assert!(
            verify_binary(&path_str, &"0".repeat(64)).is_err(),
            "wrong hash must fail"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn pinned_but_missing_binary_fails_closed() {
        let err = verify_binary(
            "/nonexistent/path/to/mcp",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        );
        assert!(err.is_err(), "a pin that cannot be checked must fail");
    }

    #[test]
    fn resolve_absolute_path_roundtrips() {
        let dir = tmp("resolve");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("tool");
        std::fs::write(&f, b"x").unwrap();
        let resolved = resolve_on_path(&f.to_string_lossy()).expect("absolute path resolves");
        assert_eq!(resolved, f);
        std::fs::remove_dir_all(&dir).ok();
    }
}
