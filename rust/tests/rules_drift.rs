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

use lean_ctx::core::rules_canonical::{RULES_VERSION, RulesFile};

/// Committed files shipping the dedicated, versioned lean-ctx rules block.
/// Add new real rule artifacts here — not docs examples or templates.
const RULE_ARTIFACTS: &[&str] = &["LEAN-CTX.md", "rust/LEAN-CTX.md"];

fn repo_root() -> PathBuf {
    let rust_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    rust_dir.parent().unwrap_or(&rust_dir).to_path_buf()
}

#[test]
fn committed_rule_artifacts_are_current() {
    let root = repo_root();
    let mut checked = 0usize;

    for rel in RULE_ARTIFACTS {
        let path = root.join(rel);
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            // Minimal checkouts may exclude an artifact; skip what isn't present.
            Err(_) => continue,
        };

        let parsed = RulesFile::parse(&content);
        assert!(
            parsed.has_content(),
            "{rel} is listed as a rule artifact but carries no '<!-- lean-ctx-rules -->' \
             block.\nRemove it from RULE_ARTIFACTS, or regenerate it via `lean-ctx setup --fix`."
        );
        assert!(
            parsed.is_current(),
            "{rel} embeds an outdated lean-ctx rules block (version {} < {RULES_VERSION}).\n\
             Regenerate committed artifacts after bumping RULES_VERSION:\n  lean-ctx setup --fix",
            parsed.version()
        );
        checked += 1;
    }

    assert!(
        checked > 0,
        "no rule artifacts found to check — RULE_ARTIFACTS is stale or the checkout is incomplete"
    );
}
