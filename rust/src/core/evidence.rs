//! Evidence schema — an attributable claim with a confidence score and source.
//!
//! Research agents must attribute statements to sources and weigh how strongly
//! a source supports a statement. [`Claim`] is the shared, serializable unit for
//! that: it is produced by the research-compression modes of
//! [`crate::core::web`] (facts / quotes carry confidence + source) and can be
//! attached to provider results via `ProviderItem::claims`, so evidence flows
//! through the same consolidation pipeline as everything else.
//!
//! The distillation is deterministic and extractive (no LLM in the loop), so the
//! claim `text` is itself the verbatim supporting span — there is no separate
//! paraphrase-vs-quote distinction to model.

use serde::{Deserialize, Serialize};

/// A single attributable claim distilled from a source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Claim {
    /// The verbatim claim / fact statement.
    pub text: String,
    /// Relative confidence in `[0.0, 1.0]` (heuristic, source-relative).
    pub confidence: f32,
    /// Where the claim was extracted from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
}

impl Claim {
    /// Build a claim, clamping `confidence` into `[0.0, 1.0]`.
    pub fn new(text: impl Into<String>, confidence: f32) -> Self {
        Self {
            text: text.into(),
            confidence: confidence.clamp(0.0, 1.0),
            source_url: None,
        }
    }

    /// Attach the source URL the claim was extracted from.
    pub fn with_source(mut self, url: impl Into<String>) -> Self {
        self.source_url = Some(url.into());
        self
    }

    /// Compact one-line rendering: `(0.82) text` (+ ` — source` when present).
    #[must_use]
    pub fn render(&self) -> String {
        let mut s = format!("({:.2}) {}", self.confidence, self.text);
        if let Some(src) = &self.source_url {
            s.push_str(" — ");
            s.push_str(src);
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_clamps_confidence() {
        assert_eq!(Claim::new("x", 1.7).confidence, 1.0);
        assert_eq!(Claim::new("x", -0.3).confidence, 0.0);
    }

    #[test]
    fn render_with_and_without_source() {
        assert_eq!(Claim::new("Fact", 0.5).render(), "(0.50) Fact");
        assert_eq!(
            Claim::new("Fact", 0.5)
                .with_source("https://x.com")
                .render(),
            "(0.50) Fact — https://x.com"
        );
    }

    #[test]
    fn with_source_sets_field() {
        let c = Claim::new("t", 0.9).with_source("https://s");
        assert_eq!(c.source_url.as_deref(), Some("https://s"));
    }

    #[test]
    fn serde_skips_empty_source() {
        let json = serde_json::to_string(&Claim::new("t", 0.5)).unwrap();
        assert!(!json.contains("source_url"));
        assert!(json.contains("\"text\":\"t\""));
    }
}
