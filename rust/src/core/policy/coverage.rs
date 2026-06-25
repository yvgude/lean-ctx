//! CGB coverage — automated *partial* assessment of a resolved policy pack
//! against the Context Governance Benchmark v1.0-draft (GL #426).
//!
//! Honesty contract: a static pack analysis can only ever produce **partial
//! evidence** for a handful of controls. This module therefore (a) checks
//! redaction patterns against real synthetic fixtures instead of trusting
//! pattern names, (b) reports `Inconclusive` — never `Pass` — when a pack
//! simply doesn't state something, and (c) refuses to compute a maturity
//! grade: grades require the full manual assessment
//! (`assessment/TEMPLATE.md` in the spec repo).

use regex::Regex;
use serde::Serialize;

use super::ResolvedPolicy;

/// Spec version the checks below were written against.
pub const BENCHMARK_ID: &str = "cgb-v1.0-draft";
/// Total controls in the spec — denominator for the honesty line.
pub const CONTROLS_TOTAL: usize = 32;

/// Outcome of one automated check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    /// The pack provides positive evidence for this aspect.
    Pass,
    /// The pack contradicts the control's expectation.
    Fail,
    /// The pack is silent — the aspect must be verified elsewhere.
    Inconclusive,
}

/// One automated check over one control aspect.
#[derive(Debug, Clone, Serialize)]
pub struct CoverageCheck {
    /// Control ID, e.g. `CGB-1.1`.
    pub control: &'static str,
    /// Short control title (spec wording, abbreviated).
    pub title: &'static str,
    pub status: CheckStatus,
    /// What was observed, in one line.
    pub detail: String,
}

/// Synthetic credential fixtures for CGB-1.1 — one per credential class the
/// control names. A class counts as covered when any resolved redaction
/// pattern matches its fixture. Shared with the framework compliance
/// reports (GL #424) so CGB and framework claims test identical fixtures.
pub(crate) const CREDENTIAL_FIXTURES: &[(&str, &str)] = &[
    ("private key block", "-----BEGIN RSA PRIVATE KEY-----"),
    ("cloud access key", "AKIAIOSFODNN7EXAMPLE"),
    (
        "credential assignment",
        "api_key = \"sk-supersecretvalue1234\"",
    ),
    (
        "bearer token",
        "Authorization: Bearer abcdefghij0123456789xyz",
    ),
];

/// Synthetic non-credential sensitivity fixtures for CGB-1.3 (regulated
/// identifiers). Matching ≥ 1 demonstrates classification beyond secrets.
/// Shared with the framework compliance reports (GL #424).
pub(crate) const DOMAIN_FIXTURES: &[(&str, &str)] = &[
    ("IBAN", "DE89 3704 0044 0532 0130 00"),
    ("payment card", "4111 1111 1111 1111"),
    ("US SSN", "SSN: 123-45-6789"),
    ("date of birth", "DOB: 03/14/1975"),
];

/// Tool-name fragments that indicate network egress beyond the model
/// provider (CGB-5.5 aspect: egress deniable by policy).
const EGRESS_TOOL_HINTS: &[&str] = &["url", "web", "fetch", "http"];

/// Run all automated checks against a resolved policy.
pub fn assess(policy: &ResolvedPolicy) -> Vec<CoverageCheck> {
    let patterns: Vec<(String, Regex)> = policy
        .redaction
        .iter()
        .filter_map(|(name, raw)| Regex::new(raw).ok().map(|re| (name.clone(), re)))
        .collect();

    let matches = |fixture: &str| patterns.iter().any(|(_, re)| re.is_match(fixture));

    let mut checks = Vec::new();

    // CGB-1.1 — credential material never reaches the model.
    let missing: Vec<&str> = CREDENTIAL_FIXTURES
        .iter()
        .filter(|(_, fixture)| !matches(fixture))
        .map(|(class, _)| *class)
        .collect();
    checks.push(if missing.is_empty() {
        CoverageCheck {
            control: "CGB-1.1",
            title: "credential redaction",
            status: CheckStatus::Pass,
            detail: format!(
                "{}/{} credential fixture classes matched by redaction patterns",
                CREDENTIAL_FIXTURES.len(),
                CREDENTIAL_FIXTURES.len()
            ),
        }
    } else {
        CoverageCheck {
            control: "CGB-1.1",
            title: "credential redaction",
            status: CheckStatus::Fail,
            detail: format!("unredacted credential classes: {}", missing.join(", ")),
        }
    });

    // CGB-1.2 — declarative, reviewable rules (named patterns in TOML).
    checks.push(if policy.redaction.is_empty() {
        CoverageCheck {
            control: "CGB-1.2",
            title: "declarative redaction rules",
            status: CheckStatus::Fail,
            detail: "pack declares no named redaction patterns".to_string(),
        }
    } else {
        CoverageCheck {
            control: "CGB-1.2",
            title: "declarative redaction rules",
            status: CheckStatus::Pass,
            detail: format!(
                "{} named, versioned patterns (chain: {})",
                policy.redaction.len(),
                if policy.chain.is_empty() {
                    "root pack".to_string()
                } else {
                    policy.chain.join(" → ")
                }
            ),
        }
    });

    // CGB-1.3 — classification beyond secrets (regulated identifiers).
    let domain_hits: Vec<&str> = DOMAIN_FIXTURES
        .iter()
        .filter(|(_, fixture)| matches(fixture))
        .map(|(class, _)| *class)
        .collect();
    checks.push(if domain_hits.is_empty() {
        CoverageCheck {
            control: "CGB-1.3",
            title: "beyond-secret classification",
            status: CheckStatus::Inconclusive,
            detail:
                "no regulated-identifier patterns declared — acceptable outside regulated workloads"
                    .to_string(),
        }
    } else {
        CoverageCheck {
            control: "CGB-1.3",
            title: "beyond-secret classification",
            status: CheckStatus::Pass,
            detail: format!("regulated classes redacted: {}", domain_hits.join(", ")),
        }
    });

    // CGB-3.2 — hard budget configured (pack aspect: context budget cap).
    checks.push(match policy.max_context_tokens {
        Some(cap) => CoverageCheck {
            control: "CGB-3.2",
            title: "context budget cap",
            status: CheckStatus::Pass,
            detail: format!("max_context_tokens = {cap}"),
        },
        None => CoverageCheck {
            control: "CGB-3.2",
            title: "context budget cap",
            status: CheckStatus::Inconclusive,
            detail: "no cap in pack — verify budget enforcement elsewhere".to_string(),
        },
    });

    // CGB-4.3 — retention policy-driven (pack aspect: declared expectation).
    checks.push(match policy.audit_retention_days {
        Some(days) => CoverageCheck {
            control: "CGB-4.3",
            title: "audit retention declared",
            status: CheckStatus::Pass,
            detail: format!("audit_retention_days = {days}"),
        },
        None => CoverageCheck {
            control: "CGB-4.3",
            title: "audit retention declared",
            status: CheckStatus::Inconclusive,
            detail: "no retention expectation in pack".to_string(),
        },
    });

    // CGB-5.4 — capabilities scoped (pack aspect: tool allow/deny posture).
    let denies = policy.deny_tools.len();
    checks.push(match (&policy.allow_tools, denies) {
        (Some(allow), _) => CoverageCheck {
            control: "CGB-5.4",
            title: "tool surface scoped",
            status: CheckStatus::Pass,
            detail: format!(
                "allowlist posture: {} tools permitted, rest denied",
                allow.len()
            ),
        },
        (None, d) if d > 0 => CoverageCheck {
            control: "CGB-5.4",
            title: "tool surface scoped",
            status: CheckStatus::Pass,
            detail: format!("denylist posture: {d} denied tool(s)"),
        },
        _ => CoverageCheck {
            control: "CGB-5.4",
            title: "tool surface scoped",
            status: CheckStatus::Inconclusive,
            detail: "pack neither allows nor denies tools — engine defaults apply".to_string(),
        },
    });

    // CGB-5.5 — egress governed (pack aspect: egress tools restricted).
    let egress_denied: Vec<&str> = policy
        .deny_tools
        .iter()
        .filter(|t| {
            let t = t.to_lowercase();
            EGRESS_TOOL_HINTS.iter().any(|h| t.contains(h))
        })
        .map(String::as_str)
        .collect();
    let egress_allowed = policy.allow_tools.as_ref().map(|allow| {
        allow
            .iter()
            .filter(|t| {
                let t = t.to_lowercase();
                EGRESS_TOOL_HINTS.iter().any(|h| t.contains(h))
            })
            .count()
    });
    checks.push(if !egress_denied.is_empty() {
        CoverageCheck {
            control: "CGB-5.5",
            title: "egress restricted",
            status: CheckStatus::Pass,
            detail: format!("egress tools denied: {}", egress_denied.join(", ")),
        }
    } else if egress_allowed == Some(0) {
        CoverageCheck {
            control: "CGB-5.5",
            title: "egress restricted",
            status: CheckStatus::Pass,
            detail: "allowlist contains no egress-capable tools".to_string(),
        }
    } else {
        CoverageCheck {
            control: "CGB-5.5",
            title: "egress restricted",
            status: CheckStatus::Inconclusive,
            detail: "pack does not restrict egress tools — verify via roles/network policy"
                .to_string(),
        }
    });

    checks
}

/// Counts per status, for the summary line and JSON.
#[derive(Debug, Serialize)]
pub struct CoverageSummary {
    pub pass: usize,
    pub fail: usize,
    pub inconclusive: usize,
    /// Distinct controls the automated checks touch.
    pub controls_covered: usize,
    pub controls_total: usize,
}

/// Summarize a check run.
#[must_use]
pub fn summarize(checks: &[CoverageCheck]) -> CoverageSummary {
    let mut covered: Vec<&str> = checks.iter().map(|c| c.control).collect();
    covered.dedup();
    CoverageSummary {
        pass: checks
            .iter()
            .filter(|c| c.status == CheckStatus::Pass)
            .count(),
        fail: checks
            .iter()
            .filter(|c| c.status == CheckStatus::Fail)
            .count(),
        inconclusive: checks
            .iter()
            .filter(|c| c.status == CheckStatus::Inconclusive)
            .count(),
        controls_covered: covered.len(),
        controls_total: CONTROLS_TOTAL,
    }
}

#[cfg(test)]
mod tests {
    use super::super::builtin;
    use super::*;

    fn resolved(name: &str) -> ResolvedPolicy {
        let pack = builtin::get(name).expect("built-in exists");
        super::super::resolve(&pack).expect("resolves")
    }

    fn status_of(checks: &[CoverageCheck], control: &str) -> CheckStatus {
        checks
            .iter()
            .find(|c| c.control == control)
            .expect("control checked")
            .status
    }

    #[test]
    fn baseline_passes_credential_redaction() {
        let checks = assess(&resolved("baseline"));
        assert_eq!(status_of(&checks, "CGB-1.1"), CheckStatus::Pass);
        assert_eq!(status_of(&checks, "CGB-1.2"), CheckStatus::Pass);
        // baseline has no regulated-identifier classes and no tool posture.
        assert_eq!(status_of(&checks, "CGB-1.3"), CheckStatus::Inconclusive);
        assert_eq!(status_of(&checks, "CGB-5.4"), CheckStatus::Inconclusive);
    }

    #[test]
    fn finance_eu_demonstrates_domain_classes_and_egress_denial() {
        let checks = assess(&resolved("finance-eu"));
        assert_eq!(status_of(&checks, "CGB-1.1"), CheckStatus::Pass);
        assert_eq!(status_of(&checks, "CGB-1.3"), CheckStatus::Pass);
        assert_eq!(status_of(&checks, "CGB-3.2"), CheckStatus::Pass);
        assert_eq!(status_of(&checks, "CGB-4.3"), CheckStatus::Pass);
        assert_eq!(status_of(&checks, "CGB-5.5"), CheckStatus::Pass);
    }

    #[test]
    fn healthcare_demonstrates_phi_classes() {
        let checks = assess(&resolved("healthcare"));
        assert_eq!(status_of(&checks, "CGB-1.3"), CheckStatus::Pass);
    }

    #[test]
    fn empty_policy_fails_credential_checks() {
        let empty = ResolvedPolicy {
            name: "empty".into(),
            version: "0.0.1".into(),
            description: String::new(),
            chain: vec![],
            default_read_mode: None,
            allow_tools: None,
            deny_tools: vec![],
            max_context_tokens: None,
            audit_retention_days: None,
            redaction: std::collections::BTreeMap::new(),
            filters: crate::core::policy::FilterRules::default(),
            egress: crate::core::policy::EgressRules::default(),
        };
        let checks = assess(&empty);
        assert_eq!(status_of(&checks, "CGB-1.1"), CheckStatus::Fail);
        assert_eq!(status_of(&checks, "CGB-1.2"), CheckStatus::Fail);
    }

    #[test]
    fn summary_counts_are_consistent() {
        let checks = assess(&resolved("finance-eu"));
        let s = summarize(&checks);
        assert_eq!(s.pass + s.fail + s.inconclusive, checks.len());
        assert_eq!(s.controls_total, CONTROLS_TOTAL);
        assert!(s.controls_covered <= checks.len());
    }
}
