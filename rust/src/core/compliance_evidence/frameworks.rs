// SPDX-License-Identifier: Apache-2.0
//! Wire types of a framework coverage report as the licensed runtime returns it
//! (`enterprise.runtime.compliance_frameworks`). The engine owns the format and
//! checks every answer; the framework mappings and the per-control assessment
//! are proprietary and never part of this tree.

use serde::{Deserialize, Serialize};

/// Most frameworks one answer may carry, and most rows per framework.
pub const MAX_FRAMEWORKS: usize = 16;
pub const MAX_ROWS: usize = 256;
/// Longest text field of a row or header.
const MAX_TEXT: usize = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    Full,
    Partial,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mechanism {
    /// Enforced by a policy-pack rule, verified against the assessed pack.
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

/// Verification status of one control row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RowStatus {
    Enforced,
    EngineGuarantee,
    NotEnforced,
    NotVerified,
    Gap,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportRow {
    pub id: String,
    pub clause: String,
    pub requirement: String,
    pub coverage: Coverage,
    pub mechanism: Mechanism,
    pub status: RowStatus,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct FrameworkReport {
    pub framework: String,
    pub title: String,
    pub version_pin: String,
    pub pinned_on: String,
    pub disclaimer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack: Option<String>,
    pub rows: Vec<ReportRow>,
    pub summary: ReportSummary,
}

/// The runtime's answer: one report per requested framework, plus the ids it
/// supports (so the engine can name them when a request is unknown).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrameworksAnswer {
    pub schema_version: u32,
    pub supported: Vec<String>,
    pub frameworks: Vec<FrameworkReport>,
}

impl FrameworksAnswer {
    /// Accept an answer only when it is bounded, answers exactly the request
    /// (all supported frameworks when `requested` is empty) and every summary
    /// is the count of its rows.
    pub fn check(&self, requested: &[String], pack: Option<&str>) -> Result<(), String> {
        let bad = |why: &str| Err(format!("compliance runtime answer refused: {why}"));
        if self.schema_version != 1 {
            return bad("unknown schema version");
        }
        if self.supported.is_empty()
            || self.supported.len() > MAX_FRAMEWORKS
            || self.supported.iter().any(|id| !valid_id(id))
        {
            return bad("invalid list of supported frameworks");
        }
        if let Some(unknown) = requested.iter().find(|id| !self.supported.contains(id)) {
            return Err(format!(
                "unknown framework '{unknown}' (supported: {})",
                self.supported.join(", ")
            ));
        }
        let expected: Vec<&str> = if requested.is_empty() {
            self.supported.iter().map(String::as_str).collect()
        } else {
            requested.iter().map(String::as_str).collect()
        };
        let answered: Vec<&str> = self
            .frameworks
            .iter()
            .map(|f| f.framework.as_str())
            .collect();
        if answered != expected {
            return bad("frameworks differ from the request");
        }
        for report in &self.frameworks {
            report.check(pack)?;
        }
        Ok(())
    }
}

impl FrameworkReport {
    fn check(&self, pack: Option<&str>) -> Result<(), String> {
        let bad = |why: &str| {
            Err(format!(
                "compliance runtime answer refused ({}): {why}",
                self.framework
            ))
        };
        let texts = [
            &self.title,
            &self.version_pin,
            &self.pinned_on,
            &self.disclaimer,
        ];
        if texts.iter().any(|t| t.is_empty() || t.len() > MAX_TEXT) {
            return bad("empty or oversized header");
        }
        if self.pack.as_deref() != pack {
            return bad("assessed a different pack");
        }
        if self.rows.is_empty() || self.rows.len() > MAX_ROWS {
            return bad("row count out of bounds");
        }
        let mut ids = std::collections::BTreeSet::new();
        for row in &self.rows {
            let row_texts = [&row.id, &row.clause, &row.requirement, &row.detail];
            if row_texts.iter().any(|t| t.is_empty() || t.len() > MAX_TEXT)
                || row.test.as_ref().is_some_and(|t| t.len() > MAX_TEXT)
                || !ids.insert(row.id.as_str())
            {
                return bad("invalid or duplicate row");
            }
            // A claim without a pack can only be unverified, and only a pack
            // rule is verified against a pack.
            let pack_rule = row.mechanism == Mechanism::PackRule;
            let consistent = match row.status {
                RowStatus::Enforced | RowStatus::NotEnforced => pack_rule && pack.is_some(),
                RowStatus::NotVerified => pack_rule,
                RowStatus::EngineGuarantee => matches!(
                    row.mechanism,
                    Mechanism::AuditEvent | Mechanism::EvidenceExport | Mechanism::Slo
                ),
                RowStatus::Gap => row.mechanism == Mechanism::None,
            };
            if !consistent {
                return bad("row status contradicts its mechanism");
            }
        }
        let count = |s: RowStatus| self.rows.iter().filter(|r| r.status == s).count();
        let summary = ReportSummary {
            controls_total: self.rows.len(),
            full_claimed: self
                .rows
                .iter()
                .filter(|r| r.coverage == Coverage::Full)
                .count(),
            enforced: count(RowStatus::Enforced),
            engine_guarantee: count(RowStatus::EngineGuarantee),
            not_enforced: count(RowStatus::NotEnforced),
            not_verified: count(RowStatus::NotVerified),
            gaps: count(RowStatus::Gap),
        };
        if summary != self.summary {
            return bad("summary does not count the rows");
        }
        Ok(())
    }
}

/// Framework ids: lowercase ASCII, digits and `-`, at most 32 bytes.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A well-formed one-framework answer in neutral terms (the real mappings
    /// are not part of this tree).
    pub(crate) fn answer(pack: Option<&str>) -> FrameworksAnswer {
        let row = |id: &str, mechanism, status, coverage| ReportRow {
            id: id.into(),
            clause: format!("clause {id}"),
            requirement: "requirement".into(),
            coverage,
            mechanism,
            status,
            detail: "detail".into(),
            test: None,
        };
        let pack_status = if pack.is_some() {
            RowStatus::Enforced
        } else {
            RowStatus::NotVerified
        };
        let rows = vec![
            row("c-1", Mechanism::PackRule, pack_status, Coverage::Full),
            row(
                "c-2",
                Mechanism::AuditEvent,
                RowStatus::EngineGuarantee,
                Coverage::Full,
            ),
            row("c-3", Mechanism::None, RowStatus::Gap, Coverage::None),
        ];
        let enforced = usize::from(pack.is_some());
        FrameworksAnswer {
            schema_version: 1,
            supported: vec!["framework-a".into(), "framework-b".into()],
            frameworks: vec![FrameworkReport {
                framework: "framework-a".into(),
                title: "Framework A".into(),
                version_pin: "edition 1".into(),
                pinned_on: "2026-01-01".into(),
                disclaimer: "not legal advice".into(),
                pack: pack.map(str::to_owned),
                rows,
                summary: ReportSummary {
                    controls_total: 3,
                    full_claimed: 2,
                    enforced,
                    engine_guarantee: 1,
                    not_enforced: 0,
                    not_verified: 1 - enforced,
                    gaps: 1,
                },
            }],
        }
    }

    /// The engine accepts only answers that match the request and count their
    /// own rows; a runtime cannot inflate a summary or claim an unassessed pack.
    #[test]
    fn check_binds_answer_to_request_and_rows() {
        let requested = vec!["framework-a".to_string()];
        assert!(answer(Some("p v1")).check(&requested, Some("p v1")).is_ok());
        assert!(answer(None).check(&requested, None).is_ok());

        let mut inflated = answer(Some("p v1"));
        inflated.frameworks[0].summary.enforced = 3;
        let mut other_pack = answer(Some("p v1"));
        other_pack.frameworks[0].pack = Some("q v1".into());
        let mut claim_without_pack = answer(None);
        claim_without_pack.frameworks[0].rows[0].status = RowStatus::Enforced;
        claim_without_pack.frameworks[0].summary.enforced = 1;
        claim_without_pack.frameworks[0].summary.not_verified = 0;
        let mut gap_as_guarantee = answer(None);
        gap_as_guarantee.frameworks[0].rows[2].status = RowStatus::EngineGuarantee;
        for (bad, pack) in [
            (inflated, Some("p v1")),
            (other_pack, Some("p v1")),
            (claim_without_pack, None),
            (gap_as_guarantee, None),
        ] {
            assert!(bad.check(&requested, pack).is_err());
        }
        // All supported frameworks were requested but only one was answered.
        assert!(answer(None).check(&[], None).is_err());
        assert!(
            answer(None)
                .check(&["framework-b".to_string()], None)
                .is_err()
        );
    }
}
