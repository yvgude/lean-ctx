//! FOCUS CSV serializer (`FinOps` Open Cost & Usage Specification).
//!
//! Spec pinned: FOCUS v1.2 (published June 2024,
//! <https://focus.finops.org/focus-specification/v1-2/>) — chosen over 1.3/1.4
//! because 1.2 introduced the `SaaS` columns (token-denominated pricing) and is
//! the version Vantage validates against for custom providers. All 21 v1.2
//! Mandatory columns are emitted, **plus** the v1.0 required column set
//! (`Provider`, `InvoiceIssuer`, `ResourceID`, `SubAccountId`, `Tags`, …,
//! mostly nullable) so the official `focus-validator` (pip, validates
//! against 1.0) passes the same file — additive columns are explicitly
//! allowed by the spec. lean-ctx dimensions ride in `x_`-prefixed custom
//! columns as the extensibility rules require.
//!
//! Two rows per [`DailyCostRow`]:
//! - `ChargeCategory=Usage`: the actual token spend through lean-ctx.
//! - `ChargeCategory=Credit`: verified savings as a negative cost (FOCUS's
//!   category for granted reductions) — never mixed into Usage so budgets
//!   stay clean.

use super::{DailyCostRow, csv_field};
use chrono::{Datelike, Duration, NaiveDate};

/// The 21 FOCUS 1.2 Mandatory columns (spec order: alphabetical), the v1.0
/// required/nullable compatibility set, then the `x_` custom dimensions.
pub const HEADER: &[&str] = &[
    "BilledCost",
    "BillingAccountId",
    "BillingAccountName",
    "BillingCurrency",
    "BillingPeriodEnd",
    "BillingPeriodStart",
    "ChargeCategory",
    "ChargeClass",
    "ChargeDescription",
    "ChargePeriodEnd",
    "ChargePeriodStart",
    "ContractedCost",
    "EffectiveCost",
    "InvoiceIssuerName",
    "ListCost",
    "PricingQuantity",
    "PricingUnit",
    "ProviderName",
    "PublisherName",
    "ServiceCategory",
    "ServiceName",
    // FOCUS 1.0 compatibility (required there, superseded/renamed in 1.2).
    "CommitmentDiscountCategory",
    "CommitmentDiscountId",
    "CommitmentDiscountName",
    "CommitmentDiscountStatus",
    "CommitmentDiscountType",
    "ConsumedQuantity",
    "ConsumedUnit",
    "ContractedUnitPrice",
    "InvoiceIssuer",
    "ListUnitPrice",
    "PricingCategory",
    "ChargeType",
    "Provider",
    "Publisher",
    "RegionId",
    "RegionName",
    "ResourceID",
    "ResourceName",
    "ResourceType",
    "SkuId",
    "SkuPriceId",
    "SubAccountId",
    "SubAccountName",
    "Tags",
    // lean-ctx custom dimensions.
    "x_project",
    "x_agent_role",
    "x_model",
    "x_tool",
    "x_tokens_saved",
];

const PROVIDER: &str = "LeanCTX";
const SERVICE_CATEGORY: &str = "AI and Machine Learning";
const SERVICE_NAME: &str = "LeanCTX Context Engine";

/// Month boundaries for the billing period: first of the charge month and
/// first of the following month, ISO 8601 UTC.
fn billing_period(date: &NaiveDate) -> (String, String) {
    let start = date.with_day(1).expect("day 1 always valid");
    let end = if start.month() == 12 {
        NaiveDate::from_ymd_opt(start.year() + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(start.year(), start.month() + 1, 1)
    }
    .expect("first of next month always valid");
    (iso(&start), iso(&end))
}

fn iso(d: &NaiveDate) -> String {
    format!("{}T00:00:00Z", d.format("%Y-%m-%d"))
}

fn push_row(out: &mut String, fields: &[String]) {
    let line = fields
        .iter()
        .map(|f| csv_field(f))
        .collect::<Vec<_>>()
        .join(",");
    out.push_str(&line);
    out.push('\n');
}

/// Serialize rows to a FOCUS 1.2 CSV document. Rows with unparseable dates
/// are skipped (the aggregate layer already filters malformed timestamps).
pub fn to_csv(rows: &[DailyCostRow]) -> String {
    let mut out = String::new();
    push_row(
        &mut out,
        &HEADER
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>(),
    );

    for row in rows {
        let Ok(date) = NaiveDate::parse_from_str(&row.date, "%Y-%m-%d") else {
            continue;
        };
        let charge_start = iso(&date);
        let charge_end = iso(&(date + Duration::days(1)));
        let (bill_start, bill_end) = billing_period(&date);

        let resource_id = format!("leanctx/{}/{}/{}", row.project, row.agent_role, row.model);
        let tags = serde_json::json!({
            "project": row.project,
            "agent_role": row.agent_role,
            "model": row.model,
            "tool": row.tool,
        })
        .to_string();

        let mut emit = |category: &str, cost: f64, qty: u64, desc: String| {
            push_row(
                &mut out,
                &[
                    format!("{cost:.6}"),                       // BilledCost
                    row.project.clone(),                        // BillingAccountId
                    format!("LeanCTX project {}", row.project), // BillingAccountName
                    "USD".into(),                               // BillingCurrency
                    bill_end.clone(),                           // BillingPeriodEnd
                    bill_start.clone(),                         // BillingPeriodStart
                    category.into(),                            // ChargeCategory
                    String::new(),                              // ChargeClass (null)
                    desc,                                       // ChargeDescription
                    charge_end.clone(),                         // ChargePeriodEnd
                    charge_start.clone(),                       // ChargePeriodStart
                    format!("{cost:.6}"),                       // ContractedCost
                    format!("{cost:.6}"),                       // EffectiveCost
                    PROVIDER.into(),                            // InvoiceIssuerName
                    format!("{cost:.6}"),                       // ListCost
                    format!("{qty}.0"),                         // PricingQuantity (decimal)
                    "tokens".into(),                            // PricingUnit
                    PROVIDER.into(),                            // ProviderName
                    PROVIDER.into(),                            // PublisherName
                    SERVICE_CATEGORY.into(),                    // ServiceCategory
                    SERVICE_NAME.into(),                        // ServiceName
                    // FOCUS 1.0 compatibility block.
                    String::new(),                  // CommitmentDiscountCategory
                    String::new(),                  // CommitmentDiscountId
                    String::new(),                  // CommitmentDiscountName
                    String::new(),                  // CommitmentDiscountStatus
                    String::new(),                  // CommitmentDiscountType
                    format!("{qty}.0"),             // ConsumedQuantity (decimal)
                    "tokens".into(),                // ConsumedUnit
                    String::new(),                  // ContractedUnitPrice
                    PROVIDER.into(),                // InvoiceIssuer
                    String::new(),                  // ListUnitPrice
                    "Standard".into(),              // PricingCategory
                    category.into(),                // ChargeType (1.0 name for ChargeCategory)
                    PROVIDER.into(),                // Provider
                    PROVIDER.into(),                // Publisher
                    String::new(),                  // RegionId
                    String::new(),                  // RegionName
                    resource_id.clone(),            // ResourceID (1.0 spelling)
                    resource_id.clone(),            // ResourceName
                    "context-engine".into(),        // ResourceType
                    row.model.clone(),              // SkuId (model = the priced SKU)
                    format!("{}-input", row.model), // SkuPriceId
                    row.agent_role.clone(),         // SubAccountId
                    row.agent_role.clone(),         // SubAccountName
                    tags.clone(),                   // Tags
                    // lean-ctx custom dimensions.
                    row.project.clone(),
                    row.agent_role.clone(),
                    row.model.clone(),
                    row.tool.clone(),
                    row.tokens_saved.to_string(),
                ],
            );
        };

        emit(
            "Usage",
            row.cost_usd,
            row.tokens_actual,
            format!("LLM context tokens via {} ({})", row.tool, row.model),
        );
        if row.tokens_saved > 0 {
            emit(
                "Credit",
                -row.savings_usd,
                row.tokens_saved,
                format!(
                    "LeanCTX verified savings (hash-chained ledger) via {} ({})",
                    row.tool, row.model
                ),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> DailyCostRow {
        DailyCostRow {
            date: "2026-06-01".into(),
            project: "proj_a".into(),
            agent_role: "coder".into(),
            model: "claude".into(),
            tool: "ctx_read".into(),
            tokens_actual: 400,
            tokens_saved: 1600,
            cost_usd: 0.001,
            savings_usd: 0.004,
        }
    }

    #[test]
    fn emits_all_mandatory_columns() {
        let csv = to_csv(&[row()]);
        let header = csv.lines().next().unwrap();
        assert_eq!(header.split(',').count(), HEADER.len());
        for col in [
            "BilledCost",
            "ChargeCategory",
            "ChargePeriodStart",
            "ServiceName",
            "PricingUnit",
        ] {
            assert!(header.contains(col), "missing {col}");
        }
    }

    #[test]
    fn usage_and_credit_rows_with_negative_savings() {
        let csv = to_csv(&[row()]);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 3, "header + usage + credit");
        assert!(lines[1].contains("Usage"));
        assert!(lines[2].contains("Credit"));
        assert!(
            lines[2].starts_with("-0.004"),
            "credit is negative: {}",
            lines[2]
        );
    }

    #[test]
    fn billing_period_handles_december() {
        let d = NaiveDate::from_ymd_opt(2026, 12, 15).unwrap();
        let (start, end) = billing_period(&d);
        assert_eq!(start, "2026-12-01T00:00:00Z");
        assert_eq!(end, "2027-01-01T00:00:00Z");
    }

    #[test]
    fn charge_period_is_one_day() {
        let csv = to_csv(&[row()]);
        let usage = csv.lines().nth(1).unwrap();
        assert!(usage.contains("2026-06-01T00:00:00Z"));
        assert!(usage.contains("2026-06-02T00:00:00Z"));
    }

    #[test]
    fn no_credit_row_when_nothing_saved() {
        let mut r = row();
        r.tokens_saved = 0;
        let csv = to_csv(&[r]);
        assert_eq!(csv.lines().count(), 2, "header + usage only");
    }
}
