//! Vantage Custom Provider serializer + uploader.
//!
//! Spec pinned: Vantage Custom Providers ingest a FOCUS-aligned CSV
//! (<https://docs.vantage.sh> → Custom Providers). Required columns:
//! `BilledCost`, `ChargeCategory`, `ChargePeriodStart`, `ServiceName`;
//! negative `BilledCost` values are accepted, and lean-ctx dimensions ride in
//! the documented `Tags` JSON column.
//!
//! Idempotency: Vantage treats every CSV upload as a separate dataset
//! (documented platform semantics) — re-uploading a period **duplicates** it.
//! The exporter therefore prints the dataset window so operators delete the
//! previous upload in Vantage (Settings → Integrations) before re-sending;
//! there is no replace operation to call.

use super::{DailyCostRow, csv_field};

pub const HEADER: &[&str] = &[
    "ChargePeriodStart",
    "ChargeCategory",
    "BilledCost",
    "ResourceId",
    "ServiceCategory",
    "ServiceName",
    "Tags",
];

const SERVICE_NAME: &str = "LeanCTX Context Engine";
const SERVICE_CATEGORY: &str = "AI and Machine Learning";

fn tags_json(row: &DailyCostRow) -> String {
    serde_json::json!({
        "project": row.project,
        "agent_role": row.agent_role,
        "model": row.model,
        "tool": row.tool,
    })
    .to_string()
}

/// Vantage custom-provider CSV: Usage rows plus Credit rows (negative cost)
/// for the verified savings.
#[must_use]
pub fn to_csv(rows: &[DailyCostRow]) -> String {
    let mut out = HEADER.join(",");
    out.push('\n');
    for row in rows {
        let mut emit = |category: &str, cost: f64| {
            let fields = [
                format!("{}T00:00:00Z", row.date),
                category.to_string(),
                format!("{cost:.6}"),
                format!("leanctx/{}/{}/{}", row.project, row.agent_role, row.model),
                SERVICE_CATEGORY.to_string(),
                SERVICE_NAME.to_string(),
                tags_json(row),
            ];
            let line = fields
                .iter()
                .map(|f| csv_field(f))
                .collect::<Vec<_>>()
                .join(",");
            out.push_str(&line);
            out.push('\n');
        };
        emit("Usage", row.cost_usd);
        if row.tokens_saved > 0 {
            emit("Credit", -row.savings_usd);
        }
    }
    out
}

/// Upload the CSV to a Vantage Custom Provider integration
/// (`POST /v2/integrations/{token}/costs.csv`, multipart).
///
/// Credentials: `VANTAGE_API_TOKEN` (Bearer) and
/// `VANTAGE_INTEGRATION_TOKEN` (the custom-provider integration).
pub fn upload(csv: &str) -> Result<String, String> {
    let api_token =
        std::env::var("VANTAGE_API_TOKEN").map_err(|_| "VANTAGE_API_TOKEN not set".to_string())?;
    let integration = std::env::var("VANTAGE_INTEGRATION_TOKEN")
        .map_err(|_| "VANTAGE_INTEGRATION_TOKEN not set".to_string())?;
    let url = format!("https://api.vantage.sh/v2/integrations/{integration}/costs.csv");

    // Minimal multipart/form-data body (one file part named `csv`).
    let boundary = format!("leanctx{:x}", u64::from(std::process::id()) ^ 0x5f3759df);
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"csv\"; filename=\"leanctx_costs.csv\"\r\n",
    );
    body.extend_from_slice(b"Content-Type: text/csv\r\n\r\n");
    body.extend_from_slice(csv.as_bytes());
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let agent = ureq::Agent::new_with_config(
        ureq::config::Config::builder()
            .timeout_global(Some(std::time::Duration::from_secs(30)))
            .http_status_as_error(false)
            .build(),
    );
    let resp = agent
        .post(&url)
        .header("Authorization", &format!("Bearer {}", api_token.trim()))
        .header(
            "Content-Type",
            &format!("multipart/form-data; boundary={boundary}"),
        )
        .send(body.as_slice())
        .map_err(|e| format!("vantage unreachable: {e}"))?;

    let status = resp.status().as_u16();
    let text = resp.into_body().read_to_string().unwrap_or_default();
    if (200..300).contains(&status) {
        Ok(format!("Vantage accepted upload ({status})"))
    } else {
        Err(format!("Vantage rejected ({status}): {text}"))
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
    fn required_vantage_columns_in_header() {
        let csv = to_csv(&[row()]);
        let header = csv.lines().next().unwrap();
        for col in [
            "BilledCost",
            "ChargeCategory",
            "ChargePeriodStart",
            "ServiceName",
        ] {
            assert!(header.contains(col), "missing {col}");
        }
    }

    #[test]
    fn tags_column_is_valid_json() {
        let csv = to_csv(&[row()]);
        let line = csv.lines().nth(1).unwrap();
        // Tags is the last column; extract via the quoted JSON blob.
        let start = line.find('{').map(|i| i - 1).unwrap();
        let raw = &line[start..];
        let unquoted = raw.trim_matches('"').replace("\"\"", "\"");
        let parsed: serde_json::Value = serde_json::from_str(&unquoted).expect("tags JSON parses");
        assert_eq!(parsed["project"], "proj_a");
        assert_eq!(parsed["agent_role"], "coder");
    }

    #[test]
    fn credit_rows_negative() {
        let csv = to_csv(&[row()]);
        let credit = csv.lines().nth(2).unwrap();
        assert!(credit.contains("Credit"));
        assert!(credit.contains("-0.004"));
    }
}
