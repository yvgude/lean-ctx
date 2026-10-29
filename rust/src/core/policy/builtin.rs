//! Curated built-in policy packs (GL #489), embedded at compile time.
//!
//! The Community engine ships the deterministic floor only: `baseline`,
//! `strict-redaction` and `open-source`. Domain packs (health, finance, legal,
//! company-specific) and compliance-framework packs are commercial and are
//! delivered by LeanCTX Pro / Enterprise, never from this source tree
//! (`scripts/check-private-paths.py` enforces the list).

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
    ("open-source", include_str!("builtin/open-source.toml")),
];

/// Packs that earlier releases bundled and that now belong to a paid tier,
/// with that tier. Naming one yields a pointer instead of "unknown pack", so
/// an existing `extends = "healthcare"` fails with an actionable message.
const PAID_PACKS: &[(&str, &str)] = &[
    ("healthcare", "Pro"),
    ("finance-eu", "Pro"),
    ("eu-ai-act-deployer", "Enterprise"),
    ("iso42001-aligned", "Enterprise"),
    ("soc2-context", "Enterprise"),
];

/// The paid tier a former built-in now belongs to, if any.
pub fn paid_tier(name: &str) -> Option<&'static str> {
    PAID_PACKS
        .iter()
        .find(|(pack, _)| *pack == name)
        .map(|(_, tier)| *tier)
}

/// Why `name` is not a built-in: the paid-tier pointer, or the list of built-ins.
pub fn unknown_pack_hint(name: &str) -> String {
    match paid_tier(name) {
        Some(tier) => format!(
            "'{name}': this pack is part of LeanCTX {tier} and no longer ships with the \
             Community engine (docs/guides/policy-packs.md)"
        ),
        None => format!("no pack named '{name}' (built-ins: {})", names().join(", ")),
    }
}

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
pub fn all() -> &'static [PolicyPack] {
    registry()
}

/// The built-in pack names, in registry order.
pub fn names() -> Vec<&'static str> {
    registry().iter().map(|p| p.name.as_str()).collect()
}

/// Look up one built-in pack by name.
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
    fn community_ships_only_the_floor_and_points_paid_packs_to_their_tier() {
        assert_eq!(names(), vec!["baseline", "strict-redaction", "open-source"]);
        for (pack, tier) in PAID_PACKS {
            assert!(get(pack).is_none(), "'{pack}' must not be bundled");
            assert!(unknown_pack_hint(pack).contains(&format!("LeanCTX {tier}")));
        }
        assert!(unknown_pack_hint("no-such-pack").contains("built-ins: baseline"));
    }

    #[test]
    fn open_source_stays_permissive_but_keeps_secrets_covered() {
        let resolved =
            crate::core::policy::resolve(&get("open-source").expect("exists")).expect("resolves");
        assert!(resolved.allow_tools.is_none());
        assert!(resolved.deny_tools.is_empty(), "{:?}", resolved.deny_tools);
        assert!(resolved.redaction.contains_key("aws_access_key"));
        assert_eq!(resolved.audit_retention_days, Some(30));
    }
}
