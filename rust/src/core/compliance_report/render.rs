//! Human-readable renderings of a [`ComplianceReportV1`] (GL #677).
//!
//! [`to_text`] is the plain-text report (CLI stdout + the source the PDF
//! renderer lays out); [`to_csv`] is the flat control matrix an assessor can
//! open in a spreadsheet. The signed JSON artifact itself is produced by
//! [`super::write_artifact`].

use std::fmt::Write as _;

use crate::core::compliance::{Coverage, FrameworkReport, RowStatus};

use super::model::ComplianceReportV1;

fn coverage_label(c: Coverage) -> &'static str {
    match c {
        Coverage::Full => "full",
        Coverage::Partial => "partial",
        Coverage::None => "none",
    }
}

fn status_label(s: RowStatus) -> &'static str {
    match s {
        RowStatus::Enforced => "enforced",
        RowStatus::EngineGuarantee => "engine-guarantee",
        RowStatus::NotEnforced => "NOT-ENFORCED",
        RowStatus::NotVerified => "not-verified",
        RowStatus::Gap => "gap",
    }
}

/// Plain-text report — deterministic, ASCII-only (safe for the PDF path).
#[must_use]
pub fn to_text(r: &ComplianceReportV1) -> String {
    let mut o = String::new();
    let _ = writeln!(o, "lean-ctx Compliance Report (v{})", r.schema_version);
    let _ = writeln!(o, "{}", "=".repeat(60));
    let _ = writeln!(o, "Generated:    {}", r.created_at);
    let _ = writeln!(o, "Engine:       lean-ctx {}", r.lean_ctx_version);
    let _ = writeln!(o, "Project:      {}", r.project);
    let _ = writeln!(o, "Signed by:    {}", r.agent_id);
    let _ = writeln!(o, "Period:       {} .. {}", r.period.from, r.period.to);
    let _ = writeln!(o);

    let _ = writeln!(o, "## Enforcement (this period)");
    let _ = writeln!(
        o,
        "  Actions blocked (ToolDenied):   {}",
        r.enforcement.blocked
    );
    let _ = writeln!(
        o,
        "  Outputs redacted (SecretDetect): {}",
        r.enforcement.redacted
    );
    let _ = writeln!(
        o,
        "  Allowed tool calls:             {}",
        r.enforcement.tool_calls
    );
    let _ = writeln!(
        o,
        "  Other security events:          {}",
        r.enforcement.other_security
    );
    if !r.enforcement.by_tool_blocked.is_empty() {
        let _ = writeln!(o, "  Top blocked tools:");
        for (tool, n) in &r.enforcement.by_tool_blocked {
            let _ = writeln!(o, "    {tool}: {n}");
        }
    }
    let _ = writeln!(o);

    let _ = writeln!(o, "## Audit trail");
    let _ = writeln!(o, "  Entries in period:  {}", r.audit.entries_in_period);
    let _ = writeln!(
        o,
        "  Chain integrity:    {}",
        if r.audit.chain_valid {
            "VALID (SHA-256 intact)"
        } else {
            "BROKEN"
        }
    );
    let _ = writeln!(o, "  Anchor prev_hash:   {}", r.audit.anchor_prev_hash);
    let _ = writeln!(o, "  Head entry_hash:    {}", r.audit.head_hash);
    let _ = writeln!(o);

    let _ = writeln!(o, "## Retention");
    let _ = writeln!(
        o,
        "  Pack:               {}",
        r.retention.policy_pack.as_deref().unwrap_or("(none)")
    );
    let _ = writeln!(
        o,
        "  Pack retention:     {}",
        r.retention
            .policy_audit_retention_days
            .map_or_else(|| "(unspecified)".to_string(), |d| format!("{d} days"))
    );
    let _ = writeln!(
        o,
        "  Plan:               {} ({})",
        r.retention.plan, r.retention.plan_source
    );
    let _ = writeln!(
        o,
        "  Plan retention:     {} days",
        r.retention.plan_audit_retention_days
    );
    if let Some(covers) = r.retention.plan_covers_policy {
        let _ = writeln!(
            o,
            "  Plan covers intent: {}",
            if covers { "yes" } else { "NO" }
        );
    }
    let _ = writeln!(o);

    let _ = writeln!(o, "## OWASP Top 10 for Agentic Applications");
    let _ = writeln!(
        o,
        "  Coverage: {} full, {} partial, {} minimal",
        r.owasp.full, r.owasp.partial, r.owasp.minimal
    );
    for row in &r.owasp.rows {
        let _ = writeln!(o, "  [{}] {} ({})", row.id, row.title, row.coverage);
    }
    let _ = writeln!(o);

    for fw in &r.frameworks {
        write_framework(&mut o, fw);
    }

    let _ = writeln!(o, "## Verification");
    let _ = writeln!(
        o,
        "  Signature:  {}",
        if r.signature.is_some() {
            "present (Ed25519)"
        } else {
            "UNSIGNED"
        }
    );
    if let Some(pk) = &r.signer_public_key {
        let _ = writeln!(o, "  Signer key: {pk}");
    }
    let _ = writeln!(
        o,
        "  Verify offline:  lean-ctx compliance verify <this-report.json>"
    );
    o
}

fn write_framework(o: &mut String, fw: &FrameworkReport) {
    let _ = writeln!(o, "## Framework: {} ({})", fw.title, fw.framework);
    let _ = writeln!(o, "  Edition pinned: {} ({})", fw.version_pin, fw.pinned_on);
    if let Some(pack) = &fw.pack {
        let _ = writeln!(o, "  Assessed pack:  {pack}");
    }
    let s = &fw.summary;
    let _ = writeln!(
        o,
        "  Controls: {} total | {} enforced | {} engine-guarantee | {} not-enforced | {} not-verified | {} gaps",
        s.controls_total, s.enforced, s.engine_guarantee, s.not_enforced, s.not_verified, s.gaps
    );
    for row in &fw.rows {
        let _ = writeln!(
            o,
            "  [{}] {} — {} / {}",
            row.id,
            row.clause,
            coverage_label(row.coverage),
            status_label(row.status)
        );
        let _ = writeln!(o, "      {}", row.detail);
    }
    let _ = writeln!(o);
}

/// RFC 4180 field quoting: wrap in quotes and double embedded quotes when the
/// value carries a comma, quote, CR or LF.
fn csv_field(v: &str) -> String {
    if v.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", v.replace('"', "\"\""))
    } else {
        v.to_string()
    }
}

fn csv_row(cells: &[&str]) -> String {
    cells
        .iter()
        .map(|c| csv_field(c))
        .collect::<Vec<_>>()
        .join(",")
}

/// Flat control matrix as CSV — one row per OWASP risk and framework control,
/// plus the enforcement and retention summary lines.
#[must_use]
pub fn to_csv(r: &ComplianceReportV1) -> String {
    let mut o = String::new();
    let _ = writeln!(
        o,
        "{}",
        csv_row(&["section", "id", "name", "coverage", "status", "detail"])
    );

    let _ = writeln!(
        o,
        "{}",
        csv_row(&[
            "meta",
            "period",
            &format!("{} .. {}", r.period.from, r.period.to),
            "",
            "",
            &format!("project={}, engine={}", r.project, r.lean_ctx_version),
        ])
    );

    for row in &r.owasp.rows {
        let _ = writeln!(
            o,
            "{}",
            csv_row(&["owasp", &row.id, &row.title, &row.coverage, "", ""])
        );
    }

    for fw in &r.frameworks {
        for row in &fw.rows {
            let _ = writeln!(
                o,
                "{}",
                csv_row(&[
                    &fw.framework,
                    &row.id,
                    &row.clause,
                    coverage_label(row.coverage),
                    status_label(row.status),
                    &row.detail,
                ])
            );
        }
    }

    let _ = writeln!(
        o,
        "{}",
        csv_row(&[
            "enforcement",
            "blocked",
            "actions blocked",
            "",
            "",
            &r.enforcement.blocked.to_string(),
        ])
    );
    let _ = writeln!(
        o,
        "{}",
        csv_row(&[
            "enforcement",
            "redacted",
            "outputs redacted",
            "",
            "",
            &r.enforcement.redacted.to_string(),
        ])
    );
    let _ = writeln!(
        o,
        "{}",
        csv_row(&[
            "audit",
            "chain",
            "audit chain integrity",
            "",
            if r.audit.chain_valid {
                "valid"
            } else {
                "broken"
            },
            &format!(
                "entries={}, head={}",
                r.audit.entries_in_period, r.audit.head_hash
            ),
        ])
    );
    let _ = writeln!(
        o,
        "{}",
        csv_row(&[
            "retention",
            "plan",
            &r.retention.plan,
            "",
            "",
            &format!(
                "pack={}d, plan={}d",
                r.retention
                    .policy_audit_retention_days
                    .map_or_else(|| "-".to_string(), |d| d.to_string()),
                r.retention.plan_audit_retention_days
            ),
        ])
    );

    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::compliance_report::{ReportSpec, build};

    fn report() -> ComplianceReportV1 {
        build(&ReportSpec {
            from: "2026-01-01T00:00:00+00:00".to_string(),
            to: "2026-12-31T00:00:00+00:00".to_string(),
            frameworks: vec!["soc2".to_string()],
            pack: Some("soc2-context".to_string()),
        })
        .unwrap()
    }

    #[test]
    fn text_has_all_sections() {
        let t = to_text(&report());
        for marker in [
            "## Enforcement",
            "## Audit trail",
            "## Retention",
            "## OWASP",
            "## Framework:",
            "## Verification",
        ] {
            assert!(t.contains(marker), "missing section: {marker}");
        }
    }

    #[test]
    fn csv_has_header_and_quotes_fields_with_commas() {
        let c = to_csv(&report());
        assert!(c.starts_with("section,id,name,coverage,status,detail\n"));
        // Detail strings contain commas → must be quoted.
        assert!(c.contains('"'), "fields with commas must be quoted");
    }

    #[test]
    fn csv_field_escapes_embedded_quotes() {
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field("plain"), "plain");
    }
}
