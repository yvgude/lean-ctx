//! Framework compliance mappings + coverage reports (GL #424, H3 Epic A).
//!
//! The mapping matrices live as machine-readable TOML under
//! `compliance/mappings/` (embedded at compile time, version-pinned to a
//! framework edition with a semi-annual review cycle). This module loads
//! them and produces the audit-conversation artifact:
//! `lean-ctx policy coverage --framework eu-ai-act`.
//!
//! Honesty contract (same spirit as `policy::coverage`):
//! * `full` coverage is only claimed where enforcement exists AND a CI test
//!   proves it — every full control carries its test name; a unit test
//!   below fails the build when a full claim has no test.
//! * Pack-dependent claims are verified LIVE against the resolved pack and
//!   downgrade to `NotEnforced` when the pack doesn't hold up.
//! * Organisational duties are reported as explicit gaps, never hidden.

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

use super::policy::ResolvedPolicy;
use super::policy::coverage::{CREDENTIAL_FIXTURES, DOMAIN_FIXTURES};

/// `(framework id, mapping TOML)` — pinned editions, see each file header.
const MAPPING_SOURCES: &[(&str, &str)] = &[
    (
        "eu-ai-act",
        include_str!("../../data/compliance/mappings/eu-ai-act.toml"),
    ),
    (
        "iso42001",
        include_str!("../../data/compliance/mappings/iso42001.toml"),
    ),
    (
        "soc2",
        include_str!("../../data/compliance/mappings/soc2.toml"),
    ),
];

// ── Mapping wire format ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FrameworkMapping {
    pub framework: String,
    pub title: String,
    /// Exact framework edition the mapping was written against.
    pub version_pin: String,
    /// Date the pin was taken (maintenance: re-review every `review_cycle_months`).
    pub pinned_on: String,
    pub review_cycle_months: u32,
    pub disclaimer: String,
    /// Built-in pack implementing the enforceable slice of this framework.
    pub reference_pack: String,
    pub controls: Vec<ControlMapping>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ControlMapping {
    /// Stable mapping id, e.g. `AIA-26.6`.
    pub id: String,
    /// Framework clause, e.g. `Art. 26(6)`.
    pub clause: String,
    /// Requirement, paraphrased in one sentence.
    pub requirement: String,
    pub mechanism: Mechanism,
    /// How `LeanCTX` addresses it (mechanism detail).
    pub leanctx: String,
    /// The evidence artifact an assessor receives.
    pub evidence: String,
    pub coverage: Coverage,
    /// Documented residual gap (`coverage = partial|none`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap: Option<String>,
    /// CI test proving enforcement (required for `coverage = full`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mechanism {
    /// Enforced by a policy-pack rule — verifiable against a resolved pack.
    PackRule,
    /// Engine guarantee recorded in the audit/event plane.
    AuditEvent,
    /// Engine produces an exportable evidence artifact.
    EvidenceExport,
    /// Service-level objective / monitoring guarantee.
    Slo,
    /// Not technically addressable (organisational duty).
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    Full,
    Partial,
    None,
}

// ── Registry ─────────────────────────────────────────────────────────────────

fn registry() -> &'static Vec<FrameworkMapping> {
    static REGISTRY: OnceLock<Vec<FrameworkMapping>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        MAPPING_SOURCES
            .iter()
            .map(|(id, toml_text)| {
                // Mappings are compile-time assets; a parse failure is a bug
                // caught by the tests below before it can ship.
                let m: FrameworkMapping = toml::from_str(toml_text)
                    .unwrap_or_else(|e| panic!("compliance mapping '{id}' is invalid: {e}"));
                assert_eq!(&m.framework, id, "mapping id/file mismatch for '{id}'");
                m
            })
            .collect()
    })
}

/// All mappings, registry order.
#[must_use]
pub fn frameworks() -> &'static [FrameworkMapping] {
    registry()
}

/// Framework ids, registry order.
#[must_use]
pub fn names() -> Vec<&'static str> {
    registry().iter().map(|m| m.framework.as_str()).collect()
}

/// Look up one mapping.
#[must_use]
pub fn get(framework: &str) -> Option<&'static FrameworkMapping> {
    registry().iter().find(|m| m.framework == framework)
}

// ── Report ───────────────────────────────────────────────────────────────────

/// Verification status of one control row in a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RowStatus {
    /// Pack-rule control verified live against the resolved pack.
    Enforced,
    /// Engine guarantee — proven by the named CI test, not pack-dependent.
    EngineGuarantee,
    /// Mapping claims a pack rule but the assessed pack does not hold it.
    NotEnforced,
    /// Pack-rule control, but no pack was supplied to verify against.
    NotVerified,
    /// Documented residual gap (organisational / out of boundary).
    Gap,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportRow {
    pub id: String,
    pub clause: String,
    pub requirement: String,
    pub coverage: Coverage,
    pub mechanism: Mechanism,
    pub status: RowStatus,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportSummary {
    pub controls_total: usize,
    pub full_claimed: usize,
    pub enforced: usize,
    pub engine_guarantee: usize,
    pub not_enforced: usize,
    pub not_verified: usize,
    pub gaps: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameworkReport {
    pub framework: String,
    pub title: String,
    pub version_pin: String,
    pub pinned_on: String,
    pub disclaimer: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pack: Option<String>,
    pub rows: Vec<ReportRow>,
    pub summary: ReportSummary,
}

/// Build the coverage report for a framework, verifying pack-rule controls
/// against `policy` when one is supplied.
#[must_use]
pub fn report(mapping: &FrameworkMapping, policy: Option<&ResolvedPolicy>) -> FrameworkReport {
    let rows: Vec<ReportRow> = mapping
        .controls
        .iter()
        .map(|c| {
            let (status, detail) = row_status(c, policy);
            ReportRow {
                id: c.id.clone(),
                clause: c.clause.clone(),
                requirement: c.requirement.clone(),
                coverage: c.coverage,
                mechanism: c.mechanism,
                status,
                detail,
                test: c.test.clone(),
            }
        })
        .collect();

    let count = |s: RowStatus| rows.iter().filter(|r| r.status == s).count();
    let summary = ReportSummary {
        controls_total: rows.len(),
        full_claimed: rows.iter().filter(|r| r.coverage == Coverage::Full).count(),
        enforced: count(RowStatus::Enforced),
        engine_guarantee: count(RowStatus::EngineGuarantee),
        not_enforced: count(RowStatus::NotEnforced),
        not_verified: count(RowStatus::NotVerified),
        gaps: count(RowStatus::Gap),
    };

    FrameworkReport {
        framework: mapping.framework.clone(),
        title: mapping.title.clone(),
        version_pin: mapping.version_pin.clone(),
        pinned_on: mapping.pinned_on.clone(),
        disclaimer: mapping.disclaimer.clone(),
        pack: policy.map(|p| format!("{} v{}", p.name, p.version)),
        rows,
        summary,
    }
}

fn row_status(control: &ControlMapping, policy: Option<&ResolvedPolicy>) -> (RowStatus, String) {
    match control.mechanism {
        Mechanism::None => (
            RowStatus::Gap,
            control
                .gap
                .clone()
                .unwrap_or_else(|| "documented gap".to_string()),
        ),
        Mechanism::AuditEvent | Mechanism::EvidenceExport | Mechanism::Slo => (
            RowStatus::EngineGuarantee,
            match &control.test {
                Some(t) => format!("{} — CI: {t}", control.leanctx),
                None => control.leanctx.clone(),
            },
        ),
        Mechanism::PackRule => match policy {
            None => (
                RowStatus::NotVerified,
                "pack rule — pass a pack to verify enforcement".to_string(),
            ),
            Some(p) => verify_pack_rule(control, p),
        },
    }
}

/// Live verification of pack-rule controls against a resolved pack.
/// Wired per control id (explicit over generic — same approach as the CGB
/// checks); the `full_claims_have_live_checks` test keeps TOML and code in
/// sync.
fn verify_pack_rule(control: &ControlMapping, p: &ResolvedPolicy) -> (RowStatus, String) {
    let patterns: Vec<regex::Regex> = p
        .redaction
        .values()
        .filter_map(|raw| regex::Regex::new(raw).ok())
        .collect();
    let matches = |fixture: &str| patterns.iter().any(|re| re.is_match(fixture));
    let missing_fixtures = |set: &'static [(&'static str, &'static str)]| -> Vec<&'static str> {
        set.iter()
            .filter(|(_, fx)| !matches(fx))
            .map(|(class, _)| *class)
            .collect()
    };
    let tool_surface_scoped = p.allow_tools.is_some() || !p.deny_tools.is_empty();
    let egress_denied = p.deny_tools.iter().any(|t| {
        ["url", "web", "fetch", "http"]
            .iter()
            .any(|hint| t.contains(hint))
    });

    match control.id.as_str() {
        "AIA-26.6" => match p.audit_retention_days {
            Some(days) if days >= 180 => (
                RowStatus::Enforced,
                format!("audit_retention_days = {days} (≥ 180 d / six months)"),
            ),
            Some(days) => (
                RowStatus::NotEnforced,
                format!("audit_retention_days = {days} < 180 — below Art. 26(6) minimum"),
            ),
            None => (
                RowStatus::NotEnforced,
                "pack declares no audit retention".to_string(),
            ),
        },
        "AIA-10.5" | "ISO-A.7.4" => {
            let missing = missing_fixtures(DOMAIN_FIXTURES);
            if missing.is_empty() {
                (
                    RowStatus::Enforced,
                    format!(
                        "{}/{} regulated-identifier fixture classes redacted",
                        DOMAIN_FIXTURES.len(),
                        DOMAIN_FIXTURES.len()
                    ),
                )
            } else if control.coverage == Coverage::Partial && missing.len() < DOMAIN_FIXTURES.len()
            {
                (
                    RowStatus::Enforced,
                    format!(
                        "partial by design — unredacted classes: {}",
                        missing.join(", ")
                    ),
                )
            } else {
                (
                    RowStatus::NotEnforced,
                    format!("unredacted identifier classes: {}", missing.join(", ")),
                )
            }
        }
        "AIA-15.5-secrets" | "SOC2-C1.1" => {
            let missing = missing_fixtures(CREDENTIAL_FIXTURES);
            if missing.is_empty() {
                (
                    RowStatus::Enforced,
                    format!(
                        "{}/{} credential fixture classes redacted",
                        CREDENTIAL_FIXTURES.len(),
                        CREDENTIAL_FIXTURES.len()
                    ),
                )
            } else {
                (
                    RowStatus::NotEnforced,
                    format!("unredacted credential classes: {}", missing.join(", ")),
                )
            }
        }
        "AIA-15.5-access" | "ISO-A.9.4" | "SOC2-CC6.1" => {
            if tool_surface_scoped {
                (
                    RowStatus::Enforced,
                    format!(
                        "tool surface scoped (allow: {}, deny: {}) on top of the default-deny capability gate",
                        p.allow_tools.as_ref().map_or(0, Vec::len),
                        p.deny_tools.len()
                    ),
                )
            } else {
                (
                    RowStatus::NotEnforced,
                    "pack neither allows nor denies tools — only the engine capability gate applies"
                        .to_string(),
                )
            }
        }
        "AIA-14.4e" => match p.max_context_tokens {
            Some(cap) => (
                RowStatus::Enforced,
                format!("max_context_tokens = {cap} bounds every assembly"),
            ),
            None => (
                RowStatus::NotEnforced,
                "no hard context cap declared".to_string(),
            ),
        },
        "SOC2-CC6.6" => {
            if egress_denied {
                (
                    RowStatus::Enforced,
                    format!("egress tools denied: {}", p.deny_tools.join(", ")),
                )
            } else {
                (
                    RowStatus::NotEnforced,
                    "no egress tool denied by pack".to_string(),
                )
            }
        }
        "ISO-A.9.2" => {
            let mut declared = Vec::new();
            if p.max_context_tokens.is_some() {
                declared.push("budget cap");
            }
            if p.audit_retention_days.is_some() {
                declared.push("retention");
            }
            if tool_surface_scoped {
                declared.push("tool scope");
            }
            if !p.redaction.is_empty() {
                declared.push("redaction");
            }
            if declared.len() >= 3 {
                (
                    RowStatus::Enforced,
                    format!("enforced process declared: {}", declared.join(", ")),
                )
            } else {
                (
                    RowStatus::NotEnforced,
                    format!(
                        "pack declares too little process ({}) for an A.9.2 claim",
                        if declared.is_empty() {
                            "nothing".to_string()
                        } else {
                            declared.join(", ")
                        }
                    ),
                )
            }
        }
        other => (
            RowStatus::NotVerified,
            format!(
                "no live check wired for pack-rule control {other} — fix the mapping or add a check"
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::policy::{builtin, resolve};

    #[test]
    fn all_mappings_parse_and_are_pinned() {
        for m in frameworks() {
            assert!(
                !m.version_pin.is_empty(),
                "{} missing version pin",
                m.framework
            );
            assert!(!m.pinned_on.is_empty(), "{} missing pin date", m.framework);
            assert!(m.review_cycle_months > 0);
            assert!(
                !m.disclaimer.is_empty(),
                "{} missing disclaimer",
                m.framework
            );
            assert!(!m.controls.is_empty());
        }
        assert_eq!(names(), vec!["eu-ai-act", "iso42001", "soc2"]);
    }

    #[test]
    fn full_claims_carry_tests_and_partial_or_none_carry_gaps() {
        for m in frameworks() {
            for c in &m.controls {
                match c.coverage {
                    Coverage::Full => {
                        assert!(
                            c.test.is_some(),
                            "{}/{} claims full coverage without a CI test (AC 2)",
                            m.framework,
                            c.id
                        );
                        assert_ne!(c.mechanism, Mechanism::None);
                    }
                    Coverage::None => {
                        assert!(
                            c.gap.is_some(),
                            "{}/{} claims no coverage without documenting the gap",
                            m.framework,
                            c.id
                        );
                        assert_eq!(c.mechanism, Mechanism::None);
                    }
                    Coverage::Partial => {
                        assert!(
                            c.gap.is_some(),
                            "{}/{} partial coverage must document the residual gap",
                            m.framework,
                            c.id
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn reference_packs_exist_and_enforce_every_pack_rule() {
        for m in frameworks() {
            let pack = builtin::get(&m.reference_pack).unwrap_or_else(|| {
                panic!(
                    "{}: reference pack '{}' missing",
                    m.framework, m.reference_pack
                )
            });
            let resolved = resolve(&pack).expect("reference pack resolves");
            let rep = report(m, Some(&resolved));
            for row in &rep.rows {
                assert_ne!(
                    row.status,
                    RowStatus::NotEnforced,
                    "{}/{}: reference pack '{}' fails its own claim: {}",
                    m.framework,
                    row.id,
                    m.reference_pack,
                    row.detail
                );
                assert_ne!(
                    row.status,
                    RowStatus::NotVerified,
                    "{}/{}: full pack-rule claim without a wired live check",
                    m.framework,
                    row.id
                );
            }
        }
    }

    #[test]
    fn weak_pack_downgrades_pack_rule_claims() {
        // The open-source pack has no regulated-identifier redaction and no
        // retention floor — full claims must downgrade, not silently pass.
        let m = get("eu-ai-act").unwrap();
        let pack = builtin::get("open-source").unwrap();
        let resolved = resolve(&pack).expect("resolves");
        let rep = report(m, Some(&resolved));
        let not_enforced = rep
            .rows
            .iter()
            .filter(|r| r.status == RowStatus::NotEnforced)
            .count();
        assert!(
            not_enforced >= 1,
            "a weak pack must produce NotEnforced rows, got none"
        );
    }

    #[test]
    fn report_without_pack_marks_pack_rules_not_verified() {
        let m = get("soc2").unwrap();
        let rep = report(m, None);
        assert!(
            rep.rows
                .iter()
                .filter(|r| r.mechanism == Mechanism::PackRule)
                .all(|r| r.status == RowStatus::NotVerified)
        );
        // Engine guarantees stay engine guarantees without a pack.
        assert!(
            rep.rows
                .iter()
                .any(|r| r.status == RowStatus::EngineGuarantee)
        );
    }
}
