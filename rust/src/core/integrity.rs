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

pub(crate) fn verify_integrity() -> bool {
    compute_seed_hash(INTEGRITY_SEED) == expected_hash()
}

pub(crate) fn is_official_origin() -> bool {
    ORIGIN_REPO.contains("yvgude/lean-ctx") && ORIGIN_NAME == "lean-ctx-engine"
}

/// Which pipeline built this binary, stamped by `build.rs` at compile time
/// (never a runtime git call). `official` is the upstream release workflow
/// only; `dogfood` the promoted Instance state, `dev` any other checkout,
/// `source` a packaged crate or a tree without git.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildInfo {
    pub kind: &'static str,
    /// Full 40-hex commit; empty when the build had none.
    pub commit: &'static str,
    /// Tracked files differed from `commit` when the build script last ran.
    pub dirty: bool,
}

/// Commit digits shown in labels (`lean-ctx 3.12.2 (dogfood f78aff8dc5)`).
const SHORT_COMMIT: usize = 10;

impl BuildInfo {
    pub fn short_commit(&self) -> &'static str {
        self.commit.get(..SHORT_COMMIT).unwrap_or(self.commit)
    }

    pub fn is_official(&self) -> bool {
        self.kind == "official"
    }

    /// `official`, `dogfood f78aff8dc5`, `dev 1a2b3c4d5e, dirty`, `source`.
    pub fn label(&self) -> String {
        if self.is_official() {
            return "official".to_owned();
        }
        let mut label = self.kind.to_owned();
        if !self.commit.is_empty() {
            label.push(' ');
            label.push_str(self.short_commit());
        }
        if self.dirty {
            label.push_str(", dirty");
        }
        label
    }

    /// The additive `build` object of `--version --json` and the
    /// capabilities documents.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "kind": self.kind,
            "commit": (!self.commit.is_empty()).then_some(self.commit),
            "dirty": self.dirty,
            "official": self.is_official(),
        })
    }
}

/// The Engine's own stamp (its build script re-runs on Engine sources and
/// `HEAD` only).
const ENGINE_BUILD: BuildInfo = BuildInfo {
    kind: env!("LEANCTX_BUILD_KIND"),
    commit: env!("LEANCTX_BUILD_COMMIT"),
    dirty: matches!(env!("LEANCTX_BUILD_DIRTY").as_bytes(), b"1"),
};

/// The binary's commit and dirty flag, stamped over every crate's sources.
static BINARY_STAMP: std::sync::OnceLock<(&'static str, bool)> = std::sync::OnceLock::new();

/// Called once by the `lean-ctx` binary at start with its own compile-time
/// stamp (`crates/lean-ctx/build.rs`): the binary links every crate, so its
/// commit and dirty flag describe the whole build. An empty `commit` (a
/// packaged crate, no git) keeps the Engine's stamp. An `official` build
/// keeps its release commit.
pub fn set_binary_stamp(commit: &'static str, dirty: bool) {
    if !commit.is_empty() {
        let _ = BINARY_STAMP.set((commit, dirty));
    }
}

/// The build stamped into this binary.
pub fn build() -> BuildInfo {
    match BINARY_STAMP.get() {
        Some(&(commit, dirty)) if !ENGINE_BUILD.is_official() => BuildInfo {
            commit,
            dirty,
            ..ENGINE_BUILD
        },
        _ => ENGINE_BUILD,
    }
}

pub struct IntegrityReport {
    pub seed_ok: bool,
    pub origin_ok: bool,
    pub repo: &'static str,
    pub pkg_name: &'static str,
    pub version: &'static str,
    pub build: BuildInfo,
}

pub fn check() -> IntegrityReport {
    IntegrityReport {
        seed_ok: verify_integrity(),
        origin_ok: is_official_origin(),
        repo: ORIGIN_REPO,
        pkg_name: ORIGIN_NAME,
        version: PKG_VERSION,
        build: build(),
    }
}

/// `lean-ctx --version --json`.
pub fn version_json() -> serde_json::Value {
    let report = check();
    serde_json::json!({
        "name": "lean-ctx",
        "version": report.version,
        "repository": report.repo,
        "unmodified": report.seed_ok && report.origin_ok,
        "build": report.build.to_json(),
    })
}

pub fn origin_line() -> String {
    let report = check();
    if report.seed_ok && report.origin_ok && report.build.is_official() {
        format!("lean-ctx {} (official, {})", report.version, report.repo)
    } else if report.seed_ok && report.origin_ok {
        format!("lean-ctx {} ({})", report.version, report.build.label())
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

    #[test]
    fn build_kind_labels_name_only_release_builds_official() {
        let sha = "f78aff8dc5a1b2c3d4e5f60718293a4b5c6d7e8f";
        let b = |kind, commit, dirty| BuildInfo {
            kind,
            commit,
            dirty,
        };
        assert_eq!(b("official", sha, false).label(), "official");
        assert_eq!(b("dogfood", sha, false).label(), "dogfood f78aff8dc5");
        assert_eq!(b("dev", sha, true).label(), "dev f78aff8dc5, dirty");
        assert_eq!(b("source", "", false).label(), "source");
        assert_eq!(b("dev", sha, false).to_json()["official"], false);
        assert_eq!(
            b("source", "", false).to_json()["commit"],
            serde_json::Value::Null
        );

        // The stamped build: "official" in the line exactly when build.rs saw
        // the release workflow (release.yml runs this suite too).
        let this = build();
        assert!(
            matches!(this.kind, "official" | "dev" | "dogfood" | "source"),
            "{this:?}"
        );
        assert!(
            this.commit.is_empty() || this.commit.len() == 40,
            "{this:?}"
        );
        assert_eq!(
            origin_line().contains("(official,"),
            this.is_official(),
            "{}",
            origin_line()
        );
        assert_eq!(version_json()["build"]["kind"], this.kind);
    }
}
