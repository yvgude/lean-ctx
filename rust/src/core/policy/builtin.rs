//! Curated built-in policy packs (GL #489), embedded at compile time.
//!
//! These are real, adoptable governance baselines — not samples. Each TOML
//! lives next to this file so users can copy one as the starting point for an
//! org-specific pack (`lean-ctx policy show <name> --toml`).

use std::sync::OnceLock;

use super::PolicyPack;

/// `(name, toml)` pairs, base packs first (parents before children, so the
/// registry is resolvable in declaration order).
const BUILTIN_SOURCES: &[(&str, &str)] = &[
    ("baseline", include_str!("builtin/baseline.toml")),
    (
        "strict-redaction",
        include_str!("builtin/strict-redaction.toml"),
    ),
    ("finance-eu", include_str!("builtin/finance-eu.toml")),
    ("healthcare", include_str!("builtin/healthcare.toml")),
    ("open-source", include_str!("builtin/open-source.toml")),
    // Framework template packs (GL #424): enforceable slices of EU AI Act /
    // ISO 42001 / SOC 2 — the residual gaps live in data/compliance/mappings/.
    (
        "eu-ai-act-deployer",
        include_str!("builtin/eu-ai-act-deployer.toml"),
    ),
    (
        "iso42001-aligned",
        include_str!("builtin/iso42001-aligned.toml"),
    ),
    ("soc2-context", include_str!("builtin/soc2-context.toml")),
];

fn registry() -> &'static Vec<PolicyPack> {
    static REGISTRY: OnceLock<Vec<PolicyPack>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        BUILTIN_SOURCES
            .iter()
            .map(|(name, toml_text)| {
                // Built-ins are compile-time assets; a parse failure is a bug in
                // this crate, caught by the tests below before it can ship.
                super::parse(toml_text)
                    .unwrap_or_else(|e| panic!("built-in policy pack '{name}' is invalid: {e}"))
            })
            .collect()
    })
}

/// All built-in packs, base packs first.
#[must_use]
pub fn all() -> &'static [PolicyPack] {
    registry()
}

/// The built-in pack names, in registry order.
#[must_use]
pub fn names() -> Vec<&'static str> {
    registry().iter().map(|p| p.name.as_str()).collect()
}

/// Look up one built-in pack by name.
#[must_use]
pub fn get(name: &str) -> Option<PolicyPack> {
    registry().iter().find(|p| p.name == name).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_parses_validates_and_resolves() {
        for pack in all() {
            let resolved = crate::core::policy::resolve(pack)
                .unwrap_or_else(|e| panic!("built-in '{}' fails to resolve: {e}", pack.name));
            assert_eq!(resolved.name, pack.name);
            // Every pack inherits the baseline secret coverage.
            assert!(
                resolved.redaction.contains_key("private_key"),
                "'{}' lost the baseline private_key pattern",
                pack.name
            );
        }
    }

    #[test]
    fn builtin_names_match_their_files() {
        for (declared, _) in BUILTIN_SOURCES {
            let pack = get(declared).expect("registered");
            assert_eq!(&pack.name, declared);
        }
    }

    #[test]
    fn regulated_packs_deny_web_fetches_and_pin_budgets() {
        for name in ["finance-eu", "healthcare"] {
            let resolved =
                crate::core::policy::resolve(&get(name).expect("exists")).expect("resolves");
            assert!(
                resolved.deny_tools.contains(&"ctx_url_read".to_string()),
                "'{name}' must deny ctx_url_read"
            );
            assert_eq!(resolved.max_context_tokens, Some(12_000));
            assert!(resolved.audit_retention_days.unwrap_or(0) >= 365);
        }
    }

    #[test]
    fn open_source_stays_permissive_but_keeps_secrets_covered() {
        let resolved =
            crate::core::policy::resolve(&get("open-source").expect("exists")).expect("resolves");
        assert!(resolved.allow_tools.is_none());
        assert!(resolved.deny_tools.is_empty());
        assert!(resolved.redaction.contains_key("aws_access_key"));
        assert_eq!(resolved.audit_retention_days, Some(30));
    }
}
