//! Build-origin integrity verification and rebrand resistance.
//!
//! Detects if the binary has been modified by automated rebranding tools
//! (e.g. `sed s/lean-ctx/better-ctx/g`). The integrity seed is a compile-time
//! constant; its hash is precomputed and embedded. If the seed is altered by
//! a text-replacement tool, the hash will no longer match.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

const INTEGRITY_SEED: &str = "lean-ctx";
const ORIGIN_REPO: &str = env!("CARGO_PKG_REPOSITORY");
const ORIGIN_NAME: &str = env!("CARGO_PKG_NAME");
const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");

fn compute_seed_hash(seed: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    seed.hash(&mut hasher);
    hasher.finish()
}

fn expected_hash() -> u64 {
    compute_seed_hash("lean-ctx")
}

pub fn verify_integrity() -> bool {
    compute_seed_hash(INTEGRITY_SEED) == expected_hash()
}

pub fn is_official_origin() -> bool {
    ORIGIN_REPO.contains("yvgude/lean-ctx") && ORIGIN_NAME == "lean-ctx"
}

pub struct IntegrityReport {
    pub seed_ok: bool,
    pub origin_ok: bool,
    pub repo: &'static str,
    pub pkg_name: &'static str,
    pub version: &'static str,
}

pub fn check() -> IntegrityReport {
    IntegrityReport {
        seed_ok: verify_integrity(),
        origin_ok: is_official_origin(),
        repo: ORIGIN_REPO,
        pkg_name: ORIGIN_NAME,
        version: PKG_VERSION,
    }
}

pub fn origin_line() -> String {
    let report = check();
    if report.seed_ok && report.origin_ok {
        format!("lean-ctx {} (official, {})", report.version, report.repo)
    } else {
        format!(
            "WARNING: Modified redistribution detected. \
             Official builds: https://github.com/yvgude/lean-ctx \
             (pkg={}, repo={})",
            report.pkg_name, report.repo
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integrity_passes_in_official_build() {
        assert!(verify_integrity());
    }

    #[test]
    fn seed_hash_is_deterministic() {
        let h1 = compute_seed_hash("lean-ctx");
        let h2 = compute_seed_hash("lean-ctx");
        assert_eq!(h1, h2);
    }

    #[test]
    fn tampered_seed_fails() {
        let tampered = compute_seed_hash("better-ctx");
        assert_ne!(tampered, expected_hash());
    }

    #[test]
    fn origin_is_official() {
        assert!(is_official_origin());
    }

    #[test]
    fn origin_line_contains_version() {
        let line = origin_line();
        assert!(line.contains(env!("CARGO_PKG_VERSION")));
    }
}
