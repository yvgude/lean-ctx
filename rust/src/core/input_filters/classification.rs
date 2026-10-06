//! Data-classification label detection (GL #675).
//!
//! Gates content that is *marked* confidential/secret. To keep false positives
//! low we only treat a label as a marking when it appears as a **banner line**
//! (a line that is essentially just the label) or in an explicit
//! `Classification:`/`Sensitivity:` field — not every prose mention of the word
//! "confidential". The caller decides whether a detected label blocks or warns.
//!
//! The regexes depend on the configured label set, so a [`Matcher`] is compiled
//! **once** when the policy loads and reused on the hot path.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::Regex;

/// Labels treated as "marked sensitive" when a policy does not list its own.
pub const DEFAULT_LABELS: &[&str] = &[
    "TOP SECRET",
    "SECRET",
    "CONFIDENTIAL",
    "RESTRICTED",
    "NDA",
    "INTERNAL ONLY",
    "PROPRIETARY",
];

/// Precompiled classification detector for a fixed label set.
pub struct Matcher {
    banner: Regex,
    value: Regex,
    /// Any label anywhere, unbounded: both detection paths need one, so text
    /// without a hit cannot be marked (no false negatives, e.g. `__SECRET__`).
    any_label: Regex,
}

fn field_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?im)^\s*(?:classification|sensitivity|data[ _-]?class)\s*[:=]\s*(.+)$")
            .expect("valid field classification regex")
    })
}

impl Matcher {
    /// Build a matcher. `blocked_labels` overrides [`DEFAULT_LABELS`] when
    /// non-empty.
    #[must_use]
    pub fn new(blocked_labels: &[String]) -> Self {
        let owned: Vec<String> = if blocked_labels.is_empty() {
            DEFAULT_LABELS.iter().map(|s| (*s).to_string()).collect()
        } else {
            blocked_labels.to_vec()
        };
        let alt = owned
            .iter()
            .map(|l| regex::escape(l.trim()))
            .collect::<Vec<_>>()
            .join("|");
        // Banner: a line that is *just* the label, optionally wrapped in
        // decoration markers (`***`, `//`, `#`, `--`, `=`).
        let banner = Regex::new(&format!(r"(?im)^[\s*#/\-_=]*({alt})[\s*#/\-_=!.]*$"))
            .expect("valid banner regex");
        let value = Regex::new(&format!(r"(?i)\b({alt})\b")).expect("valid value regex");
        let any_label = Regex::new(&format!(r"(?i){alt}")).expect("valid label prefilter");
        Self {
            banner,
            value,
            any_label,
        }
    }

    /// Distinct classification labels found (uppercased), or empty if the
    /// content is not marked.
    #[must_use]
    pub fn detect(&self, text: &str) -> Vec<String> {
        let mut found: BTreeSet<String> = BTreeSet::new();
        if !self.any_label.is_match(text) {
            return Vec::new();
        }
        for caps in self.banner.captures_iter(text) {
            if let Some(m) = caps.get(1) {
                found.insert(m.as_str().trim().to_ascii_uppercase());
            }
        }
        for caps in field_re().captures_iter(text) {
            if let Some(val) = caps.get(1)
                && let Some(m) = self.value.captures(val.as_str()).and_then(|c| c.get(1))
            {
                found.insert(m.as_str().trim().to_ascii_uppercase());
            }
        }
        found.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_matcher() -> Matcher {
        Matcher::new(&[])
    }

    #[test]
    fn detects_banner_line() {
        let labels = default_matcher().detect("CONFIDENTIAL\n\nsome body text");
        assert_eq!(labels, vec!["CONFIDENTIAL".to_string()]);
    }

    #[test]
    fn detects_decorated_banner() {
        let labels = default_matcher().detect("// ***** SECRET *****\nfn main() {}");
        assert_eq!(labels, vec!["SECRET".to_string()]);
    }

    #[test]
    fn detects_classification_field() {
        let labels = default_matcher().detect("Title: X\nClassification: Confidential\nBody");
        assert_eq!(labels, vec!["CONFIDENTIAL".to_string()]);
    }

    #[test]
    fn ignores_prose_mention() {
        // The word in a sentence is NOT a marking — guardrail against FPs.
        let labels = default_matcher().detect("This keeps your data confidential and safe.");
        assert!(labels.is_empty(), "prose mention must not gate: {labels:?}");
    }

    #[test]
    fn respects_custom_labels() {
        let m = Matcher::new(&["TS//SCI".to_string()]);
        assert_eq!(m.detect("TS//SCI\nbody"), vec!["TS//SCI".to_string()]);
        // Default labels are not used when a custom list is provided.
        assert!(m.detect("CONFIDENTIAL\nbody").is_empty());
    }

    #[test]
    fn prefilter_never_hides_an_underscore_decorated_banner() {
        assert_eq!(
            default_matcher().detect("__CONFIDENTIAL__\nbody"),
            vec!["CONFIDENTIAL"]
        );
        assert!(default_matcher().detect("fn main() {}\n").is_empty());
    }

    #[test]
    fn public_field_value_does_not_gate() {
        assert!(
            default_matcher()
                .detect("Classification: public")
                .is_empty()
        );
    }
}
