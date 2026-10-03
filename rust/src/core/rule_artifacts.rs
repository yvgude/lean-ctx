//! The committed `LEAN-CTX.md` rule artifacts as `(relative_path, content)`
//! pairs — the single source shared by the regenerator (`gen_rules` example)
//! and the drift gate (`tests/rules_drift.rs`) so the project copy and the
//! `rust/` copy can never disagree.
//!
//! Content is forced to the default profile (non-shadow, compression `Off`) so
//! the committed bytes are independent of the developer's local lean-ctx config
//! and stay deterministic (#498). The live writer
//! (`hooks::ensure_project_agents_integration`) renders with the *user's* config
//! instead — that is each user's own copy, not this repo's checked-in artifact.

use crate::core::config::CompressionLevel;
use crate::core::rules_canonical::{self, Wrapper};

/// Project-relative paths of every committed dedicated-rules artifact. Add new
/// real rule artifacts here — not docs examples or templates.
pub const ARTIFACT_PATHS: &[&str] = &["LEAN-CTX.md", "rust/LEAN-CTX.md"];

/// Canonical body of a project `LEAN-CTX.md`: the owner banner, the long-form
/// rules block (non-shadow, compression `Off`), and a trailing newline.
/// Inverse of what the drift gate reads back. Longform because `LEAN-CTX.md`
/// is opened on demand (AGENTS.md pointer), never auto-loaded — it can afford
/// the verbose teaching sections the injected profiles fold away (#578).
#[must_use]
pub fn canonical_body() -> String {
    format!(
        "{}\n{}\n",
        rules_canonical::PROJECT_LEAN_CTX_OWNED_MARKER,
        rules_canonical::render(
            false,
            Wrapper::Longform,
            CompressionLevel::Off,
            &super::tool_profiles::ToolProfile::Power,
        )
    )
}

/// The repo-root skill (installed by `skills/lean-ctx/scripts/install.sh` and
/// skill marketplaces) is a verbatim copy of the template `lean-ctx setup`
/// installs, so the two can never drift apart again.
pub const SKILL_COPY_PATH: &str = "skills/lean-ctx/SKILL.md";

/// `(relative_path, content)` for every artifact the generator writes: the
/// versioned rule artifacts (one canonical body) plus the skill copy.
#[must_use]
pub fn artifacts() -> Vec<(&'static str, String)> {
    let body = canonical_body();
    let mut arts: Vec<(&'static str, String)> =
        ARTIFACT_PATHS.iter().map(|p| (*p, body.clone())).collect();
    arts.push((
        SKILL_COPY_PATH,
        include_str!("../templates/SKILL.md").to_string(),
    ));
    arts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_body_is_owned_versioned_and_current() {
        let body = canonical_body();
        assert!(body.starts_with(rules_canonical::PROJECT_LEAN_CTX_OWNED_MARKER));
        assert!(body.contains(&format!(
            "<!-- version: {} -->",
            rules_canonical::RULES_VERSION
        )));
        // The body must carry the v3 guidance it exists to ship.
        assert!(body.contains("AGENT LOOP"));
        assert!(body.contains("NAVIGATION PARADOX"));
        assert!(body.ends_with('\n'));
    }

    #[test]
    fn artifacts_cover_every_declared_path() {
        let arts = artifacts();
        for declared in ARTIFACT_PATHS.iter().chain([&SKILL_COPY_PATH]) {
            assert!(
                arts.iter().any(|(p, b)| p == declared && !b.is_empty()),
                "{declared} missing from the generator output"
            );
        }
    }
}
