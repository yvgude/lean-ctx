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
    stamp: Regex,
    value: Regex,
    /// Any label anywhere, unbounded: both detection paths need one, so text
    /// without a hit cannot be marked (no false negatives, e.g. `__SECRET__`).
    any_label: Regex,
    /// Configured labels that are themselves written in lowercase: only
    /// these may mark a banner or stamp written all in lowercase.
    lowercase_labels: BTreeSet<String>,
}

/// Whether a banner or stamp label, as written, can be a marking. A marking
/// is written to be seen: upper or title case. A bare lowercase word on its
/// own line is a file name in a listing, a list item or a log token — the
/// banner form needs no decoration, so treating it as a marking classified
/// ordinary tool output `restricted` and locked sessions out (26.10.).
fn written_as_marking(found: &str, lowercase_labels: &BTreeSet<String>) -> bool {
    found.chars().any(char::is_uppercase)
        || !found.chars().any(char::is_alphabetic)
        || lowercase_labels.contains(found.trim())
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
        // Stamp: the label fenced on BOTH sides by a run of at least three
        // decoration marks at the start of a line, with content following on
        // the same line (`*** TOP SECRET *** invoice ledger`). One-line
        // objects — a provider record, an explicit source body — carry their
        // marking this way and have no banner line of their own. The fence is
        // what keeps a prose mention, a `// SECRET` comment or `**bold**`
        // markdown from being read as a marking. A provider record is
        // rendered behind its reference (`#12 *** SECRET *** title`); that
        // reference may precede the stamp.
        let stamp = Regex::new(&format!(
            r"(?im)^\s*(?:#[0-9A-Za-z_.\-]+[ \t]+)?[*#/\-_=]{{3,}}[ \t]*({alt})[ \t]*[*#/\-_=]{{3,}}"
        ))
        .expect("valid stamp regex");
        let value = Regex::new(&format!(r"(?i)\b({alt})\b")).expect("valid value regex");
        let any_label = Regex::new(&format!(r"(?i){alt}")).expect("valid label prefilter");
        let lowercase_labels = owned
            .iter()
            .map(|label| label.trim())
            .filter(|label| !label.chars().any(char::is_uppercase))
            .map(str::to_owned)
            .collect();
        Self {
            banner,
            stamp,
            value,
            any_label,
            lowercase_labels,
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
        for caps in self
            .banner
            .captures_iter(text)
            .chain(self.stamp.captures_iter(text))
        {
            if let Some(m) = caps.get(1)
                && written_as_marking(m.as_str(), &self.lowercase_labels)
            {
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

    /// A one-line object has no banner line of its own: the fenced stamp at
    /// its start is its marking. Before, only a line that was *just* the
    /// label counted, so such an object was classified by its descriptor.
    #[test]
    fn detects_a_fenced_stamp_in_front_of_content_but_no_looser_form() {
        let matcher = default_matcher();
        assert_eq!(
            matcher.detect("*** TOP SECRET *** invoice ledger plans"),
            vec!["TOP SECRET".to_string()]
        );
        assert_eq!(
            matcher.detect("intro\n=== CONFIDENTIAL === quarterly numbers"),
            vec!["CONFIDENTIAL".to_string()]
        );
        // A provider record rendered behind its reference (AP09 Q05).
        assert_eq!(
            matcher.detect("gitlab issues (2):\n  #2 *** CONFIDENTIAL *** merger plan (opened)"),
            vec!["CONFIDENTIAL".to_string()]
        );
        for unmarked in [
            "// SECRET sauce for the build cache",
            "see #2 *** SECRET *** mid-line",
            "**Confidential** computing is a hardware feature.",
            "the *** SECRET *** is mentioned mid-line",
            "*** SECRET handling is described below",
        ] {
            assert!(matcher.detect(unmarked).is_empty(), "{unmarked}");
        }
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
        {
            let found = &m.detect("CONFIDENTIAL\nbody");
            assert!(found.is_empty(), "{found:?}");
        }
    }

    #[test]
    fn prefilter_never_hides_an_underscore_decorated_banner() {
        assert_eq!(
            default_matcher().detect("__CONFIDENTIAL__\nbody"),
            vec!["CONFIDENTIAL"]
        );
        {
            let found = &default_matcher().detect("fn main() {}\n");
            assert!(found.is_empty(), "{found:?}");
        }
    }

    /// The 26.10. lockout: a directory listing with an entry named like the
    /// top marking level classified a whole tool result `restricted`.
    #[test]
    fn a_lowercase_word_on_its_own_line_is_not_a_marking() {
        let matcher = default_matcher();
        for unmarked in [
            "Cargo.toml\nsrc\nsecret\ntarget\n",
            "README.md\nrestricted\n",
            "- secret\n- tokens\n",
            "*** top secret *** lowercase stamp\n",
            "   confidential   \n",
        ] {
            let found = matcher.detect(unmarked);
            assert!(found.is_empty(), "{unmarked:?}: {found:?}");
        }
        // A marking written to be seen still marks, in every form.
        for (marked, label) in [
            ("SECRET\nbody\n", "SECRET"),
            ("Secret\nbody\n", "SECRET"),
            ("## RESTRICTED ##\nbody\n", "RESTRICTED"),
            ("*** TOP SECRET *** plans\n", "TOP SECRET"),
            ("Classification: secret\nbody\n", "SECRET"),
        ] {
            assert_eq!(
                matcher.detect(marked),
                vec![label.to_string()],
                "{marked:?}"
            );
        }
        // An operator's own lowercase label keeps marking as configured.
        let custom = Matcher::new(&["eyes only".to_string()]);
        assert_eq!(custom.detect("eyes only\nbody"), vec!["EYES ONLY"]);
    }

    #[test]
    fn public_field_value_does_not_gate() {
        {
            let found = &default_matcher().detect("Classification: public");
            assert!(found.is_empty(), "{found:?}");
        }
    }
}
