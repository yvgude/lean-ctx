//! Rules drift gate (#903): committed lean-ctx rule artifacts stay current.
//!
//! `rules_canonical` is the single source of truth and `rules_consistency.rs`
//! proves the *renderer* is correct. This gate proves the *committed files* that
//! embed the dedicated rules block (`<!-- lean-ctx-rules -->` + `<!-- version: N -->`)
//! were regenerated after the SSOT changed: when `RULES_VERSION` is bumped, any
//! artifact still carrying an older version fails here — the precise
//! "SSOT changed but the file was not regenerated" drift scenario.
//!
//! Only real, versioned artifacts are listed. Docs examples (`docs/guides/*.md`)
//! and templates (placeholder `{RULES_MARKER}`) deliberately carry no live
//! version marker and are out of scope.

use std::path::PathBuf;

use lean_ctx::core::reference_docs::content_matches;
use lean_ctx::core::rule_artifacts::{ARTIFACT_PATHS, artifacts};
use lean_ctx::core::rules_canonical::{RULES_VERSION, RulesFile};

fn repo_root() -> PathBuf {
    let rust_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    rust_dir.parent().unwrap_or(&rust_dir).to_path_buf()
}

#[test]
fn committed_rule_artifacts_are_current() {
    let root = repo_root();
    let mut checked = 0usize;

    for rel in ARTIFACT_PATHS {
        let path = root.join(rel);
        // Minimal checkouts may exclude an artifact; skip what isn't present.
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };

        let parsed = RulesFile::parse(&content);
        assert!(
            parsed.has_content(),
            "{rel} is listed as a rule artifact but carries no '<!-- lean-ctx-rules -->' \
             block.\nRemove it from ARTIFACT_PATHS, or regenerate it via \
             `cargo run --example gen_rules --features dev-tools`."
        );
        assert!(
            parsed.is_current(),
            "{rel} embeds an outdated lean-ctx rules block (version {} < {RULES_VERSION}).\n\
             Regenerate committed artifacts after bumping RULES_VERSION:\n  \
             cargo run --example gen_rules --features dev-tools",
            parsed.version()
        );
        checked += 1;
    }

    assert!(
        checked > 0,
        "no rule artifacts found to check — ARTIFACT_PATHS is stale or the checkout is incomplete"
    );
}

/// Stronger than the version gate: the committed bytes must equal what the
/// generator produces from the SSOT, so an edited canonical section (even one
/// that keeps the version) cannot silently drift from the checked-in files.
#[test]
fn committed_rule_artifacts_match_generator_output() {
    let root = repo_root();
    let mut checked = 0usize;

    for (rel, expected) in artifacts() {
        let path = root.join(rel);
        let Ok(on_disk) = std::fs::read_to_string(&path) else {
            continue;
        };
        assert!(
            content_matches(&on_disk, &expected),
            "{rel} drifted from the canonical rules render.\n\
             Regenerate: cargo run --example gen_rules --features dev-tools"
        );
        checked += 1;
    }

    assert!(
        checked > 0,
        "no rule artifacts found to check — ARTIFACT_PATHS is stale or the checkout is incomplete"
    );
}
