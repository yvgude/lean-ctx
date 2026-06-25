//! Signed CISO compliance report (GL #677) — "the deliverable a CISO shows
//! auditors".
//!
//! Composes the engine's existing evidence surfaces into one signed,
//! exportable artifact over a date range:
//! - **OWASP** Top-10-for-Agents alignment ([`crate::core::owasp_alignment`]);
//! - **Framework** coverage — EU AI Act / ISO 42001 / SOC 2
//!   ([`crate::core::compliance`]), verified live against a resolved pack;
//! - **Enforcement** — what was *blocked* / *redacted* over the period, folded
//!   from the append-only audit chain ([`aggregate`]);
//! - **Retention** — the pack's `audit_retention_days` intent vs. the effective
//!   plan entitlement.
//!
//! The result is **Ed25519-signed** ([`model::ComplianceReportV1`]) and
//! exportable as JSON (the signed artifact), CSV or PDF ([`render`], [`pdf`]).
//! Verification is offline — no audit trail, no `LeanCTX` install required.

pub mod aggregate;
pub mod model;
pub mod pdf;
pub mod render;

use std::path::{Path, PathBuf};

pub use model::{ComplianceReportV1, ReportVerifyResult};

use crate::core::compliance;
use crate::core::owasp_alignment;
use crate::core::policy::{self, ResolvedPolicy};
use model::{
    AuditSection, EnforcementSection, KIND, OwaspRow, OwaspSection, Period, RetentionSection,
    SCHEMA_VERSION,
};

/// What to attest. Empty `frameworks` ⇒ every built-in framework.
pub struct ReportSpec {
    /// RFC 3339 inclusive lower bound.
    pub from: String,
    /// RFC 3339 inclusive upper bound.
    pub to: String,
    /// Framework ids (`eu-ai-act`, `iso42001`, `soc2`); empty ⇒ all.
    pub frameworks: Vec<String>,
    /// Pack name/path override; defaults to the project pack, else `baseline`.
    pub pack: Option<String>,
}

/// Build the unsigned report. Sign it with [`ComplianceReportV1::sign`] before
/// exporting. Fails loudly on any inconsistency — a compliance artifact with a
/// silently missing part is worse than none.
pub fn build(spec: &ReportSpec) -> Result<ComplianceReportV1, String> {
    let from = chrono::DateTime::parse_from_rfc3339(&spec.from)
        .map_err(|e| format!("--from is not RFC 3339: {e}"))?;
    let to = chrono::DateTime::parse_from_rfc3339(&spec.to)
        .map_err(|e| format!("--to is not RFC 3339: {e}"))?;
    if from > to {
        return Err("--from must not be after --to".to_string());
    }

    let resolved = resolve_pack(spec)?;
    let agg = aggregate::aggregate(from, to)?;
    let chain_valid = crate::core::audit_trail::verify_chain().valid;

    let frameworks = build_frameworks(&spec.frameworks, &resolved)?;
    let owasp = build_owasp();
    let retention = build_retention(&resolved);

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
        owasp,
        frameworks,
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
        retention,
        signer_public_key: None,
        signature: None,
    })
}

/// Resolve the pack to assess: explicit `--pack` (name or `.toml` path), else
/// the project pack (`.lean-ctx/policy.toml`), else `baseline`.
fn resolve_pack(spec: &ReportSpec) -> Result<ResolvedPolicy, String> {
    let pack_name = spec.pack.clone().unwrap_or_else(|| {
        if Path::new(".lean-ctx/policy.toml").exists() {
            ".lean-ctx/policy.toml".to_string()
        } else {
            "baseline".to_string()
        }
    });
    let pack = if Path::new(&pack_name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("toml"))
    {
        policy::parse_file(Path::new(&pack_name)).map_err(|e| format!("pack {pack_name}: {e}"))?
    } else {
        policy::builtin::get(&pack_name)
            .ok_or_else(|| format!("unknown builtin pack '{pack_name}'"))?
    };
    policy::resolve(&pack).map_err(|e| format!("pack {pack_name}: {e}"))
}

/// Build one [`compliance::FrameworkReport`] per requested framework (or all).
fn build_frameworks(
    requested: &[String],
    resolved: &ResolvedPolicy,
) -> Result<Vec<compliance::FrameworkReport>, String> {
    let ids: Vec<String> = if requested.is_empty() {
        compliance::names().into_iter().map(String::from).collect()
    } else {
        requested.to_vec()
    };
    ids.iter()
        .map(|fw| {
            let mapping = compliance::get(fw).ok_or_else(|| {
                format!(
                    "unknown framework '{fw}' (supported: {})",
                    compliance::names().join(", ")
                )
            })?;
            Ok(compliance::report(mapping, Some(resolved)))
        })
        .collect()
}

/// Project the static OWASP alignment table into a report section.
fn build_owasp() -> OwaspSection {
    let mappings = owasp_alignment::alignment();
    let coverage_label = |c: owasp_alignment::Coverage| match c {
        owasp_alignment::Coverage::Full => "full",
        owasp_alignment::Coverage::Partial => "partial",
        owasp_alignment::Coverage::Minimal => "minimal",
    };
    let count = |c: owasp_alignment::Coverage| mappings.iter().filter(|m| m.coverage == c).count();
    OwaspSection {
        full: count(owasp_alignment::Coverage::Full),
        partial: count(owasp_alignment::Coverage::Partial),
        minimal: count(owasp_alignment::Coverage::Minimal),
        rows: mappings
            .iter()
            .map(|m| OwaspRow {
                id: m.owasp_id.to_string(),
                title: m.owasp_title.to_string(),
                coverage: coverage_label(m.coverage).to_string(),
            })
            .collect(),
    }
}

/// Retention posture: the pack's declared intent vs. the effective plan window.
fn build_retention(resolved: &ResolvedPolicy) -> RetentionSection {
    let eff = crate::cloud_client::resolve_effective_plan_cached();
    let plan_days = eff.plan.entitlements().audit_retention_days;
    let plan_covers_policy = resolved
        .audit_retention_days
        .map(|declared| plan_days >= declared);
    RetentionSection {
        policy_pack: Some(format!("{} v{}", resolved.name, resolved.version)),
        policy_audit_retention_days: resolved.audit_retention_days,
        plan: eff.plan.as_str().to_string(),
        plan_source: plan_source_label(eff.source).to_string(),
        plan_audit_retention_days: plan_days,
        plan_covers_policy,
    }
}

fn plan_source_label(s: crate::cloud_client::PlanSource) -> &'static str {
    use crate::cloud_client::PlanSource;
    match s {
        PlanSource::Live => "live",
        PlanSource::Cached => "cached",
        PlanSource::Expired => "expired",
        PlanSource::None => "unverified",
    }
}

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

    #[test]
    fn build_with_all_frameworks_against_baseline() {
        let spec = ReportSpec {
            from: "2026-01-01T00:00:00+00:00".to_string(),
            to: "2026-12-31T23:59:59+00:00".to_string(),
            frameworks: vec![],
            pack: Some("strict-redaction".to_string()),
        };
        let report = build(&spec).expect("build succeeds");
        assert_eq!(report.kind, KIND);
        assert_eq!(report.frameworks.len(), compliance::names().len());
        assert_eq!(report.owasp.rows.len(), 10);
        assert!(
            report.signature.is_none(),
            "build yields an unsigned report"
        );
    }

    #[test]
    fn build_rejects_unknown_framework() {
        let spec = ReportSpec {
            from: "2026-01-01T00:00:00+00:00".to_string(),
            to: "2026-12-31T23:59:59+00:00".to_string(),
            frameworks: vec!["nonexistent".to_string()],
            pack: Some("baseline".to_string()),
        };
        assert!(build(&spec).is_err());
    }

    #[test]
    fn build_rejects_inverted_period() {
        let spec = ReportSpec {
            from: "2026-12-31T00:00:00+00:00".to_string(),
            to: "2026-01-01T00:00:00+00:00".to_string(),
            frameworks: vec![],
            pack: Some("baseline".to_string()),
        };
        assert!(build(&spec).is_err());
    }

    #[test]
    fn retention_reflects_pack_declaration() {
        let spec = ReportSpec {
            from: "2026-01-01T00:00:00+00:00".to_string(),
            to: "2026-12-31T00:00:00+00:00".to_string(),
            frameworks: vec!["soc2".to_string()],
            pack: Some("soc2-context".to_string()),
        };
        let report = build(&spec).unwrap();
        assert_eq!(report.retention.policy_audit_retention_days, Some(365));
    }
}
