//! `CloudZero` Common Bill Format (CBF) serializer + `AnyCost` Stream uploader.
//!
//! Spec pinned: CBF as documented at
//! <https://docs.cloudzero.com/docs/anycost-common-bill-format-cbf> —
//! required columns `time/usage_start` + `cost/cost`; savings are emitted as
//! `lineitem/type=Discount` rows with negative cost (CBF's documented
//! mechanism, included in `CloudZero` "Real Cost").
//!
//! Idempotency: the Stream API request carries `"operation": "replace_drop"`,
//! which replaces all previously dropped data for the same month — re-running
//! an export overwrites instead of duplicating (CloudZero-side guarantee).

use super::{DailyCostRow, csv_field};

pub const HEADER: &[&str] = &[
    "lineitem/type",
    "time/usage_start",
    "resource/service",
    "resource/id",
    "resource/account",
    "usage/amount",
    "usage/units",
    "cost/cost",
    "resource/tag:project",
    "resource/tag:agent_role",
    "resource/tag:model",
    "resource/tag:tool",
];

const SERVICE: &str = "LeanCTX";

fn record(row: &DailyCostRow, line_type: &str, amount: u64, cost: f64) -> Vec<String> {
    vec![
        line_type.to_string(),
        format!("{}T00:00:00Z", row.date),
        SERVICE.to_string(),
        format!("leanctx/{}/{}/{}", row.project, row.agent_role, row.model),
        row.project.clone(),
        amount.to_string(),
        "tokens".to_string(),
        format!("{cost:.6}"),
        row.project.clone(),
        row.agent_role.clone(),
        row.model.clone(),
        row.tool.clone(),
    ]
}

fn records(rows: &[DailyCostRow]) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for row in rows {
        out.push(record(row, "Usage", row.tokens_actual, row.cost_usd));
        if row.tokens_saved > 0 {
            out.push(record(row, "Discount", row.tokens_saved, -row.savings_usd));
        }
    }
    out
}

/// CBF CSV (for `AnyCost` bucket drops or manual import).
#[must_use]
pub fn to_csv(rows: &[DailyCostRow]) -> String {
    let mut out = HEADER.join(",");
    out.push('\n');
    for rec in records(rows) {
        let line = rec
            .iter()
            .map(|f| csv_field(f))
            .collect::<Vec<_>>()
            .join(",");
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// `AnyCost` Stream request body for one month (`YYYY-MM`): all rows must
/// belong to that month; `replace_drop` makes the upload idempotent.
#[must_use]
pub fn to_stream_body(rows: &[DailyCostRow], month: &str) -> serde_json::Value {
    let data: Vec<serde_json::Value> = records(rows)
        .into_iter()
        .map(|rec| {
            let mut obj = serde_json::Map::new();
            for (key, val) in HEADER.iter().zip(rec) {
                obj.insert((*key).to_string(), serde_json::Value::String(val));
            }
            serde_json::Value::Object(obj)
        })
        .collect();
    serde_json::json!({
        "month": month,
        "operation": "replace_drop",
        "data": data,
    })
}

/// Upload one month to the `AnyCost` Stream API.
///
/// Credentials: `CLOUDZERO_API_KEY` (Authorization header) and
/// `CLOUDZERO_CONNECTION_ID` (the `AnyCost` Stream connection).
pub fn upload(rows: &[DailyCostRow], month: &str) -> Result<String, String> {
    let api_key =
        std::env::var("CLOUDZERO_API_KEY").map_err(|_| "CLOUDZERO_API_KEY not set".to_string())?;
    let connection = std::env::var("CLOUDZERO_CONNECTION_ID")
        .map_err(|_| "CLOUDZERO_CONNECTION_ID not set".to_string())?;
    let url = format!(
        "https://api.cloudzero.com/v2/connections/billing/anycost/{connection}/billing_drops"
    );

    let body = serde_json::to_vec(&to_stream_body(rows, month)).map_err(|e| e.to_string())?;
    let agent = ureq::Agent::new_with_config(
        ureq::config::Config::builder()
            .timeout_global(Some(std::time::Duration::from_secs(30)))
            .http_status_as_error(false)
            .build(),
    );
    let resp = agent
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Authorization", api_key.trim())
        .send(body.as_slice())
        .map_err(|e| format!("cloudzero unreachable: {e}"))?;

    let status = resp.status().as_u16();
    let text = resp.into_body().read_to_string().unwrap_or_default();
    if (200..300).contains(&status) {
        Ok(format!("CloudZero accepted month {month} ({status})"))
    } else {
        Err(format!("CloudZero rejected ({status}): {text}"))
    }
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
    fn usage_and_discount_rows() {
        let csv = to_csv(&[row()]);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[1].starts_with("Usage,2026-06-01T00:00:00Z"));
        assert!(lines[2].starts_with("Discount,"));
        assert!(
            lines[2].contains("-0.004"),
            "discount negative: {}",
            lines[2]
        );
    }

    #[test]
    fn stream_body_is_idempotent_replace_drop() {
        let body = to_stream_body(&[row()], "2026-06");
        assert_eq!(body["month"], "2026-06");
        assert_eq!(body["operation"], "replace_drop");
        let data = body["data"].as_array().unwrap();
        assert_eq!(data.len(), 2);
        assert_eq!(data[0]["lineitem/type"], "Usage");
        assert_eq!(data[0]["cost/cost"], "0.001000");
        assert_eq!(data[1]["resource/tag:project"], "proj_a");
    }

    #[test]
    fn required_cbf_columns_present() {
        let body = to_stream_body(&[row()], "2026-06");
        let first = &body["data"][0];
        assert!(first.get("time/usage_start").is_some());
        assert!(first.get("cost/cost").is_some());
    }
}
