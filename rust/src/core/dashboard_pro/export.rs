// SPDX-License-Identifier: Apache-2.0
//! The Ledger export: a filtered selection as CSV (formula-safe cells) or a
//! JSON array, in ledger order, with numbers, names and chain hashes only.

use std::borrow::Cow;

use serde_json::{Map, Value};

use super::ledger::{KINDS, LEDGER_NONE, LEDGER_OTHER, LEDGER_PROXY, entry_lines, ledger_kind};
use super::wire::{LedgerEntry, LedgerFilters, ProjectColumn};
use super::{ProError, ProResult, ProjectScope};

pub(crate) const LEDGER_EXPORT_FIELDS: [&str; 21] = [
    "ts",
    "kind",
    "tool",
    "mechanism",
    "model_id",
    "baseline_tokens",
    "actual_tokens",
    "saved_tokens",
    "bounce_adjustment",
    "saved_usd",
    "unit_price_per_m_usd",
    "lines_added",
    "lines_removed",
    "measurement_method",
    "evidence_class",
    "attribution_group",
    "repo_hash",
    "project",
    "version",
    "prev_hash",
    "entry_hash",
];

/// The largest export the engine produces.
pub(crate) const MAX_EXPORT_BYTES: usize = 32 << 20;

pub(crate) fn mime_of(format: &str) -> &'static str {
    if format == "csv" {
        "text/csv; charset=utf-8"
    } else {
        "application/json"
    }
}

/// `leanctx-ledger-YYYY-MM-DD.<format>` for `day`.
pub(crate) fn file_name(day: chrono::NaiveDate, format: &str) -> String {
    format!("leanctx-ledger-{day}.{format}")
}

/// A spreadsheet must never run a cell as a formula.
pub(crate) fn csv_safe(value: &str) -> Cow<'_, str> {
    if value.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        Cow::Owned(format!("'{value}"))
    } else {
        Cow::Borrowed(value)
    }
}

fn csv_cell(value: &Value) -> String {
    let text: Cow<'_, str> = match value {
        Value::Null => Cow::Borrowed(""),
        Value::String(text) => csv_safe(text),
        Value::Bool(flag) => Cow::Borrowed(if *flag { "True" } else { "False" }),
        Value::Number(number) => Cow::Owned(number.to_string()),
        other => {
            let rendered = other.to_string();
            Cow::Owned(csv_safe(&rendered).into_owned())
        }
    };
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.into_owned()
    }
}

fn project_of(column: &ProjectColumn, entry: &LedgerEntry) -> String {
    match column {
        ProjectColumn::Fixed { name } => name.clone(),
        ProjectColumn::Labels { labels } => {
            if entry.tool.as_deref() == Some("proxy_cache") {
                return LEDGER_PROXY.into();
            }
            let repo = entry.repo_hash.as_deref().unwrap_or("");
            labels.iter().find(|l| l.hash == repo).map_or_else(
                || {
                    if repo.is_empty() {
                        LEDGER_NONE
                    } else {
                        LEDGER_OTHER
                    }
                    .to_string()
                },
                |l| l.name.clone(),
            )
        }
    }
}

fn record(raw: &Map<String, Value>, entry: &LedgerEntry, project: &str) -> Map<String, Value> {
    let (added, removed) = entry_lines(entry);
    LEDGER_EXPORT_FIELDS
        .iter()
        .map(|field| {
            let value = match *field {
                "kind" => Value::from(KINDS[ledger_kind(entry) as usize]),
                "lines_added" => Value::from(added),
                "lines_removed" => Value::from(removed),
                "project" => Value::from(project),
                other => raw.get(other).cloned().unwrap_or_else(|| "".into()),
            };
            ((*field).to_string(), value)
        })
        .collect()
}

/// The export bytes, or 413 once they would exceed [`MAX_EXPORT_BYTES`].
pub(crate) fn build(
    scope: &ProjectScope,
    filters: &LedgerFilters,
    format: &str,
    column: &ProjectColumn,
) -> ProResult<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    let csv = format == "csv";
    if csv {
        out.extend_from_slice(LEDGER_EXPORT_FIELDS.join(",").as_bytes());
        out.push(b'\n');
    } else {
        out.push(b'[');
    }
    let mut count = 0usize;
    super::ledger::export_entries(scope, filters, |raw, entry| {
        let values = record(raw, entry, &project_of(column, entry));
        if csv {
            let line: Vec<String> = LEDGER_EXPORT_FIELDS
                .iter()
                .map(|f| csv_cell(values.get(*f).unwrap_or(&Value::Null)))
                .collect();
            out.extend_from_slice(line.join(",").as_bytes());
            out.push(b'\n');
        } else {
            if count > 0 {
                out.push(b',');
            }
            out.push(b'\n');
            let encoded = serde_json::to_vec(&Value::Object(values))
                .map_err(|_| ProError::internal("Export failed"))?;
            out.extend_from_slice(&encoded);
        }
        count += 1;
        if out.len() > MAX_EXPORT_BYTES {
            return Err(ProError::new(
                413,
                "The selection is too large to export; narrow the filters",
            ));
        }
        Ok(())
    })?;
    if !csv {
        out.extend_from_slice(b"\n]\n");
    }
    Ok(out)
}
