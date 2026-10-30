// SPDX-License-Identifier: Apache-2.0
//! Signed compliance evidence (GL #677) on the one engine.
//!
//! The engine owns everything an auditor must be able to check without a
//! LeanCTX license: the facts it measured, the enforcement counts folded from
//! the append-only audit chain ([`aggregate`]), the OWASP alignment table, the
//! retention posture, the signed artifact ([`model::ComplianceReportV1`]) and
//! its offline verification, plus the renderings ([`render`], [`pdf`]).
//!
//! Framework coverage (EU AI Act, ISO 42001, SOC 2, …) is an Enterprise
//! capability of the licensed runtime: the engine sends the projection in
//! [`facts`] and accepts the answer only after [`frameworks::FrameworksAnswer::check`].
//! The mappings and the per-control assessment never live in this tree.

pub mod aggregate;
pub mod facts;
pub mod frameworks;
pub mod model;
pub mod pdf;
pub mod render;

use std::path::{Path, PathBuf};

pub use model::{ComplianceReportV1, ReportVerifyResult};

use crate::core::owasp_alignment;
use crate::core::policy::{self, ResolvedPolicy};
use facts::ComplianceFactsV1;
use frameworks::FrameworksAnswer;
use model::{
    AuditSection, EnforcementSection, KIND, OwaspRow, OwaspSection, Period, RetentionSection,
    SCHEMA_VERSION,
};

/// What to attest. Empty `frameworks` ⇒ every framework the runtime supports.
pub struct ReportSpec {
    /// RFC 3339 inclusive lower bound.
    pub from: String,
    /// RFC 3339 inclusive upper bound.
    pub to: String,
    pub frameworks: Vec<String>,
    /// Pack name/path override; defaults to the project pack, else `baseline`.
    pub pack: Option<String>,
}

/// Build the unsigned report; `assess` answers the framework part (the
/// licensed runtime in production). Fails loudly on any inconsistency — a
/// compliance artifact with a silently missing part is worse than none.
pub fn build(
    spec: &ReportSpec,
    assess: impl FnOnce(&ComplianceFactsV1) -> Result<FrameworksAnswer, String>,
) -> Result<ComplianceReportV1, String> {
    let from = chrono::DateTime::parse_from_rfc3339(&spec.from)
        .map_err(|e| format!("--from is not RFC 3339: {e}"))?;
    let to = chrono::DateTime::parse_from_rfc3339(&spec.to)
        .map_err(|e| format!("--to is not RFC 3339: {e}"))?;
    if from > to {
        return Err("--from must not be after --to".to_string());
    }

    let resolved = resolve_pack(spec.pack.as_deref())?;
    let frameworks = assess_frameworks(&spec.frameworks, Some(&resolved), assess)?;
    let agg = aggregate::aggregate(from, to)?;
    let chain_valid = crate::core::audit_trail::verify_chain_full().valid;

    let project = std::env::current_dir()
        .ok()
        .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "unknown".to_string());

    Ok(ComplianceReportV1 {
        schema_version: SCHEMA_VERSION,
        kind: KIND.to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        lean_ctx_version: env!("CARGO_PKG_VERSION").to_string(),
        agent_id: crate::core::agent_identity::current_agent_id().to_string(),
        project,
        period: Period {
            from: spec.from.clone(),
            to: spec.to.clone(),
        },
        owasp: build_owasp(),
        frameworks: frameworks.frameworks,
        enforcement: EnforcementSection {
            blocked: agg.blocked,
            redacted: agg.redacted,
            tool_calls: agg.tool_calls,
            other_security: agg.other_security,
            by_event: agg.by_event,
            by_tool_blocked: agg.by_tool_blocked,
        },
        audit: AuditSection {
            entries_in_period: agg.entries,
            chain_valid,
            anchor_prev_hash: agg.anchor_prev_hash,
            head_hash: agg.head_hash,
        },
        retention: build_retention(&resolved),
        signer_public_key: None,
        signature: None,
    })
}

/// Ask `assess` for the framework reports of `requested` against `policy` and
/// accept only an answer that matches the request (see [`FrameworksAnswer::check`]).
pub fn assess_frameworks(
    requested: &[String],
    policy: Option<&ResolvedPolicy>,
    assess: impl FnOnce(&ComplianceFactsV1) -> Result<FrameworksAnswer, String>,
) -> Result<FrameworksAnswer, String> {
    if let Some(bad) = requested.iter().find(|id| !frameworks::valid_id(id)) {
        return Err(format!("invalid framework id '{bad}'"));
    }
    let projection = facts::facts(requested, policy);
    let answer = assess(&projection)?;
    let label = policy.map(facts::pack_label);
    answer.check(requested, label.as_deref())?;
    Ok(answer)
}

/// Resolve the pack to assess: explicit name or `.toml` path, else the project
/// pack (`.lean-ctx/policy.toml`), else `baseline`.
pub fn resolve_pack(pack: Option<&str>) -> Result<ResolvedPolicy, String> {
    let pack_name = pack.map_or_else(
        || {
            if Path::new(".lean-ctx/policy.toml").exists() {
                ".lean-ctx/policy.toml".to_string()
            } else {
                "baseline".to_string()
            }
        },
        str::to_owned,
    );
    let pack = if Path::new(&pack_name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("toml"))
    {
        policy::parse_file(Path::new(&pack_name)).map_err(|e| format!("pack {pack_name}: {e}"))?
    } else {
        policy::builtin::get(&pack_name).ok_or_else(|| {
            format!(
                "pack {pack_name}: {}",
                policy::builtin::unknown_pack_hint(&pack_name)
            )
        })?
    };
    policy::resolve(&pack).map_err(|e| format!("pack {pack_name}: {e}"))
}

/// Project the static OWASP alignment table into a report section. `minimal`
/// stays in the v1 format and is always 0 since the table has no such row.
fn build_owasp() -> OwaspSection {
    let mappings = owasp_alignment::alignment();
    let label = |c: owasp_alignment::Coverage| match c {
        owasp_alignment::Coverage::Full => "full",
        owasp_alignment::Coverage::Partial => "partial",
    };
    let count = |c: owasp_alignment::Coverage| mappings.iter().filter(|m| m.coverage == c).count();
    OwaspSection {
        full: count(owasp_alignment::Coverage::Full),
        partial: count(owasp_alignment::Coverage::Partial),
        minimal: 0,
        rows: mappings
            .iter()
            .map(|m| OwaspRow {
                id: m.owasp_id.to_string(),
                title: m.owasp_title.to_string(),
                coverage: label(m.coverage).to_string(),
            })
            .collect(),
    }
}

/// Retention posture: the pack's declared intent vs. the plan window. The
/// engine verifies no plan of its own (accounts live in LeanCTX Hive), so
/// the plan window is reported as unverified. An unverified window cannot
/// answer whether it covers the pack's intent, so `plan_covers_policy` stays
/// unset instead of claiming `false` for every pack (F11); the local trail
/// itself never deletes sealed segments.
fn build_retention(resolved: &ResolvedPolicy) -> RetentionSection {
    let plan = crate::core::product_capabilities::Plan::Community;
    RetentionSection {
        policy_pack: Some(facts::pack_label(resolved)),
        policy_audit_retention_days: resolved.audit_retention_days,
        plan: plan.as_str().to_string(),
        plan_source: UNVERIFIED_PLAN.to_string(),
        plan_audit_retention_days: 0,
        plan_covers_policy: None,
    }
}

/// `plan_source` of a plan window the engine could not verify.
pub(crate) const UNVERIFIED_PLAN: &str = "unverified";

/// Default artifact location: `<data_dir>/compliance/report-v1_<utc-stamp>.json`.
pub fn default_artifact_path() -> Result<PathBuf, String> {
    let dir = crate::core::data_dir::lean_ctx_data_dir()?.join("compliance");
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir compliance: {e}"))?;
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    Ok(dir.join(format!("report-v1_{stamp}.json")))
}

/// Pretty-prints the signed JSON artifact to `out` (creating parent dirs).
pub fn write_artifact(report: &ComplianceReportV1, out: &Path) -> Result<PathBuf, String> {
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }
    let json = serde_json::to_string_pretty(report).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(out, json).map_err(|e| format!("write {}: {e}", out.display()))?;
    Ok(out.to_path_buf())
}

/// Loads and parses a signed report artifact, rejecting unrelated JSON by `kind`.
pub fn load_artifact(path: &Path) -> Result<ComplianceReportV1, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let report: ComplianceReportV1 =
        serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
    if report.kind != KIND {
        return Err(format!("not a {KIND} artifact (kind = {:?})", report.kind));
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(from: &str, to: &str, frameworks: &[&str]) -> ReportSpec {
        ReportSpec {
            from: from.into(),
            to: to.into(),
            frameworks: frameworks.iter().map(|s| (*s).to_string()).collect(),
            pack: Some("strict-redaction".into()),
        }
    }

    /// The report binds the runtime's assessment to the pack it measured, and
    /// refuses an answer for another pack, an inverted period or a bad id.
    #[test]
    fn build_binds_assessment_to_the_measured_pack() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let label = facts::pack_label(&resolve_pack(Some("strict-redaction")).unwrap());
        let ok = |facts: &ComplianceFactsV1| {
            assert_eq!(
                facts.policy.as_ref().map(|p| p.pack.as_str()),
                Some(label.as_str())
            );
            Ok(frameworks::tests::answer(Some(&label)))
        };
        let report = build(
            &spec(
                "2026-01-01T00:00:00+00:00",
                "2026-12-31T00:00:00+00:00",
                &["framework-a"],
            ),
            ok,
        )
        .unwrap();
        assert_eq!(report.kind, KIND);
        assert_eq!(report.frameworks.len(), 1);
        assert_eq!(report.owasp.rows.len(), 10);
        assert!(
            report.signature.is_none(),
            "build yields an unsigned report"
        );

        let foreign = |_: &ComplianceFactsV1| Ok(frameworks::tests::answer(Some("other v9")));
        let inverted = |_: &ComplianceFactsV1| -> Result<FrameworksAnswer, String> {
            unreachable!("an inverted period never reaches the runtime")
        };
        let year = ("2026-01-01T00:00:00+00:00", "2026-12-31T00:00:00+00:00");
        assert!(build(&spec(year.0, year.1, &["framework-a"]), foreign).is_err());
        assert!(build(&spec(year.1, year.0, &[]), inverted).is_err());
        assert!(build(&spec(year.0, year.1, &["../etc"]), inverted).is_err());
    }

    /// F11: an unverified plan window never claims it fails to cover the
    /// pack's declared retention, and renders as unverified, not 0 days.
    #[test]
    fn unverified_plan_makes_no_retention_coverage_claim() {
        let resolved = resolve_pack(Some("strict-redaction")).unwrap();
        assert_eq!(resolved.audit_retention_days, Some(180));
        let retention = build_retention(&resolved);
        assert_eq!(retention.plan_source, UNVERIFIED_PLAN);
        assert_eq!(retention.plan_covers_policy, None);

        let mut report = model::tests::sample();
        report.retention = retention;
        let text = render::to_text(&report);
        assert!(text.contains("Plan retention:     unverified"), "{text}");
        assert!(!text.contains("Plan covers intent"), "{text}");
        assert!(render::to_csv(&report).contains("plan=unverified"));
    }
}
