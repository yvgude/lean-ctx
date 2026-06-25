//! Precision-biased gate: decide whether a candidate is durable enough to codify.
//!
//! Bias is toward *precision over recall* (per the capture philosophy): a missed
//! skill is invisible, but a wrong one erodes trust in the whole rule set. The
//! MERGE-vs-CREATE distinction lives in `rule_file` (it depends on what is
//! already on disk); this gate only decides KEEP vs SKIP.

use super::candidate::SkillCandidate;

/// Outcome of judging a single candidate.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Worth codifying (create or merge handled by the writer).
    Keep,
    /// Rejected, with a human-readable reason.
    Skip(String),
}

/// Bodies shorter than this carry too little to be a rule.
const MIN_BODY_LEN: usize = 25;

/// Generic, low-information phrasings that must never become a rule.
const NOISE: [&str; 9] = [
    "fixed bug",
    "updated code",
    "wip",
    "work in progress",
    "todo",
    "refactored",
    "cleanup",
    "minor change",
    "various changes",
];

fn is_generic(body: &str) -> bool {
    let trimmed = body.trim().to_ascii_lowercase();
    NOISE.iter().any(|n| {
        trimmed == *n || (trimmed.starts_with(n) && trimmed.chars().count() < n.len() + 12)
    })
}

/// Judge a candidate against the configured thresholds.
#[must_use]
pub fn judge(c: &SkillCandidate, min_confidence: f32, min_recurrence: u32) -> Verdict {
    if c.body.chars().count() < MIN_BODY_LEN {
        return Verdict::Skip("too short".to_string());
    }
    if is_generic(&c.body) {
        return Verdict::Skip("generic / one-off phrasing".to_string());
    }
    // Codify when it recurs enough OR is high-confidence curated knowledge.
    if c.recurrence >= min_recurrence || c.confidence >= min_confidence {
        Verdict::Keep
    } else {
        Verdict::Skip(format!(
            "one-off (recurrence {} < {min_recurrence}, confidence {:.2} < {min_confidence:.2})",
            c.recurrence, c.confidence
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(body: &str, recurrence: u32, confidence: f32) -> SkillCandidate {
        SkillCandidate {
            slug: "s".into(),
            title: "t".into(),
            body: body.into(),
            category: "decision".into(),
            recurrence,
            confidence,
            sources: vec![],
        }
    }

    #[test]
    fn rejects_too_short() {
        assert!(matches!(
            judge(&cand("short", 5, 0.9), 0.7, 2),
            Verdict::Skip(_)
        ));
    }

    #[test]
    fn rejects_generic() {
        assert!(matches!(
            judge(&cand("fixed bug", 5, 0.9), 0.7, 2),
            Verdict::Skip(_)
        ));
    }

    #[test]
    fn keeps_recurring_pattern() {
        let c = cand(
            "Always run lean-ctx stop before building the binary.",
            2,
            0.5,
        );
        assert_eq!(judge(&c, 0.7, 2), Verdict::Keep);
    }

    #[test]
    fn keeps_high_confidence_single() {
        let c = cand(
            "Always run lean-ctx stop before building the binary.",
            1,
            0.85,
        );
        assert_eq!(judge(&c, 0.7, 2), Verdict::Keep);
    }

    #[test]
    fn skips_low_signal_oneoff() {
        let c = cand(
            "Always run lean-ctx stop before building the binary.",
            1,
            0.5,
        );
        assert!(matches!(judge(&c, 0.7, 2), Verdict::Skip(_)));
    }
}
