// SPDX-License-Identifier: Apache-2.0
//! The cockpit's request `{op, method, query, body}`, checked per op before
//! anything is read: allowed keys, bounds, patterns and values.

use serde_json::{Map, Value};

use super::{GUARD_KINDS, ProError, ProResult, plain_text};

/// Upper bound of a cockpit request body.
pub(crate) const MAX_REQUEST_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Op {
    SecurityReport,
    MemoryAnalyze,
    MemoryCompare,
    AdaptiveState,
    ReportBuild,
    LedgerBrowse,
    LedgerExport,
    SpendTrend,
    WorkAnalysis,
    AlertsUpdate,
    AlertsTest,
    /// The cockpit's alerts loop sends this op; the engine supplies the events.
    AlertsEvaluate,
    Profiles,
    MemorySnapshot,
    MemoryRestore,
    MemoryForget,
    MemoryConsolidate,
    AdaptiveApply,
    AdaptiveDismiss,
}

const OPS: [(&str, Op); 19] = [
    ("security.report", Op::SecurityReport),
    ("memory.analyze", Op::MemoryAnalyze),
    ("memory.compare", Op::MemoryCompare),
    ("adaptive.state", Op::AdaptiveState),
    ("report.build", Op::ReportBuild),
    ("ledger.browse", Op::LedgerBrowse),
    ("ledger.export", Op::LedgerExport),
    ("spend.trend", Op::SpendTrend),
    ("work.analysis", Op::WorkAnalysis),
    ("alerts.update", Op::AlertsUpdate),
    ("alerts.test", Op::AlertsTest),
    ("alerts.evaluate", Op::AlertsEvaluate),
    ("profiles", Op::Profiles),
    ("memory.snapshot", Op::MemorySnapshot),
    ("memory.restore", Op::MemoryRestore),
    ("memory.forget", Op::MemoryForget),
    ("memory.consolidate", Op::MemoryConsolidate),
    ("adaptive.apply", Op::AdaptiveApply),
    ("adaptive.dismiss", Op::AdaptiveDismiss),
];

impl Op {
    pub(crate) fn parse(name: &str) -> Option<Self> {
        OPS.iter().find(|(n, _)| *n == name).map(|(_, op)| *op)
    }

    /// Ops that read only device-wide stores (audit trail, receipts, the
    /// savings ledger, host sessions) and so answer "All projects" too. The
    /// others read or write one project's knowledge and need a project.
    pub(crate) fn device_wide(self) -> bool {
        matches!(
            self,
            Self::SecurityReport
                | Self::ReportBuild
                | Self::LedgerBrowse
                | Self::LedgerExport
                | Self::SpendTrend
                | Self::AlertsUpdate
                | Self::AlertsTest
                | Self::AlertsEvaluate
                | Self::Profiles
        )
    }

    pub(crate) fn name(self) -> &'static str {
        OPS.iter()
            .find(|(_, op)| *op == self)
            .map_or("", |(name, _)| name)
    }

    pub(crate) fn is_write(self) -> bool {
        matches!(
            self,
            Self::AlertsUpdate
                | Self::AlertsTest
                | Self::AlertsEvaluate
                | Self::Profiles
                | Self::MemorySnapshot
                | Self::MemoryRestore
                | Self::MemoryForget
                | Self::MemoryConsolidate
                | Self::AdaptiveApply
                | Self::AdaptiveDismiss
        )
    }

    /// The effects a runtime answer to this op may plan.
    pub(crate) fn allowed_effects(self) -> &'static [&'static str] {
        match self {
            Self::AlertsUpdate => &["alerts.settings"],
            Self::AlertsTest | Self::AlertsEvaluate => &["alerts.notify"],
            Self::Profiles => &["profiles.put", "profiles.delete", "config.set"],
            Self::MemorySnapshot => &["memory.snapshot"],
            Self::MemoryRestore => &["memory.restore"],
            Self::MemoryForget => &["memory.forget"],
            Self::MemoryConsolidate => &["memory.consolidate"],
            Self::AdaptiveApply => &["context.overlay", "adaptive.record"],
            Self::AdaptiveDismiss => &["adaptive.dismiss"],
            Self::LedgerExport => &["ledger.export"],
            _ => &[],
        }
    }
}

/// A checked request: only known keys, values normalized.
#[derive(Debug, Clone)]
pub(crate) struct BridgeRequest {
    pub(crate) op: Op,
    /// Query values as strings.
    pub(crate) query: Map<String, Value>,
    pub(crate) body: Map<String, Value>,
}

impl BridgeRequest {
    pub(crate) fn q(&self, key: &str) -> &str {
        self.query.get(key).and_then(Value::as_str).unwrap_or("")
    }
    pub(crate) fn b_str(&self, key: &str) -> &str {
        self.body.get(key).and_then(Value::as_str).unwrap_or("")
    }
    pub(crate) fn b_true(&self, key: &str) -> bool {
        self.body.get(key) == Some(&Value::Bool(true))
    }
}

/// Parses and checks a cockpit request.
pub(crate) fn parse(raw: &str) -> ProResult<BridgeRequest> {
    if raw.len() > MAX_REQUEST_BYTES {
        return Err(ProError::new(413, "Request too large"));
    }
    let value: Value =
        serde_json::from_str(raw).map_err(|_| ProError::bad("Invalid JSON request"))?;
    let Value::Object(object) = value else {
        return Err(ProError::bad("JSON object required"));
    };
    from_object(object)
}

/// Checks a request object `{op, method, query, body}`.
pub(crate) fn from_object(mut object: Map<String, Value>) -> ProResult<BridgeRequest> {
    let op = object
        .remove("op")
        .and_then(|v| v.as_str().and_then(Op::parse))
        .ok_or_else(|| ProError::bad("Unknown analysis operation"))?;
    let method = object.remove("method");
    let expected = if op.is_write() { "POST" } else { "GET" };
    if method.as_ref().and_then(Value::as_str) != Some(expected) {
        return Err(ProError::bad("Unsupported method for this operation"));
    }
    let query = take_object(&mut object, "query")?;
    let body = take_object(&mut object, "body")?;
    if !object.is_empty() {
        return Err(ProError::bad("Unknown request field"));
    }
    validate(op, query, &body)
}

fn take_object(object: &mut Map<String, Value>, key: &str) -> ProResult<Map<String, Value>> {
    match object.remove(key) {
        None | Some(Value::Null) => Ok(Map::new()),
        Some(Value::Object(map)) => Ok(map),
        Some(_) => Err(ProError::bad(format!("{key} must be an object"))),
    }
}

/// Checks an op's query and body (also used for the watch's own requests).
pub(crate) fn validate(
    op: Op,
    query: Map<String, Value>,
    body: &Map<String, Value>,
) -> ProResult<BridgeRequest> {
    let query = query_strings(query)?;
    if op.is_write() && !query.is_empty() {
        return Err(ProError::bad("This operation takes no query"));
    }
    if !op.is_write() && !body.is_empty() {
        return Err(ProError::bad("This operation takes no body"));
    }
    let mut request = BridgeRequest {
        op,
        query: Map::new(),
        body: Map::new(),
    };
    match op {
        Op::SecurityReport => {
            only(&query, &["days"])?;
            let days = text(&query, "days", "30");
            if !["7", "30", "90", "365", "all"].contains(&days.as_str()) {
                return Err(ProError::bad("Unsupported period"));
            }
            request.query.insert("days".into(), days.into());
        }
        Op::MemoryCompare => {
            only(&query, &["id"])?;
            let id = text(&query, "id", "");
            if !super::snapshots::is_snapshot_id(&id) {
                return Err(ProError::bad("Unknown snapshot"));
            }
            request.query.insert("id".into(), id.into());
        }
        Op::ReportBuild => {
            only(&query, &["period", "month"])?;
            let period = text(&query, "period", "this_month");
            let month = text(&query, "month", "");
            if !["this_month", "last_month", "last_30", "month"].contains(&period.as_str()) {
                return Err(ProError::bad("Unsupported report period"));
            }
            if period == "month" {
                if !is_month(&month) {
                    return Err(ProError::bad("Use the month as YYYY-MM"));
                }
            } else if !month.is_empty() {
                return Err(ProError::bad("A month only goes with the month period"));
            }
            request.query.insert("period".into(), period.into());
            request.query.insert("month".into(), month.into());
        }
        Op::LedgerBrowse | Op::LedgerExport => {
            ledger_query(op, &query, &mut request.query)?;
        }
        Op::MemoryAnalyze | Op::AdaptiveState | Op::SpendTrend | Op::WorkAnalysis => {
            only(&query, &[])?;
        }
        Op::AlertsUpdate => alerts_body(body, &mut request.body)?,
        Op::AlertsTest | Op::AlertsEvaluate => only(body, &[])?,
        Op::Profiles => {
            only(body, &["action", "name", "confirm_weaker"])?;
            let action = body.get("action").and_then(Value::as_str).unwrap_or("");
            if !["save", "apply", "delete"].contains(&action) {
                return Err(ProError::bad("Unsupported profile action"));
            }
            let name = body
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("");
            if !is_profile_name(name) {
                return Err(ProError::bad(
                    "Name the profile with letters, numbers, spaces, dots or dashes",
                ));
            }
            request.body.insert("action".into(), action.into());
            request.body.insert("name".into(), name.into());
            request.body.insert(
                "confirm_weaker".into(),
                flag(body, "confirm_weaker")?.into(),
            );
        }
        Op::MemorySnapshot => {
            only(body, &["reason"])?;
            let reason = reason(body, "Manual snapshot")?;
            request.body.insert("reason".into(), reason.into());
        }
        Op::MemoryRestore => {
            only(body, &["id", "confirm"])?;
            let id = body.get("id").and_then(Value::as_str).unwrap_or("");
            if !super::snapshots::is_snapshot_id(id) {
                return Err(ProError::bad("Unknown snapshot"));
            }
            if body.get("confirm") != Some(&Value::Bool(true)) {
                return Err(ProError::conflict("Confirm restoring this snapshot"));
            }
            request.body.insert("id".into(), id.into());
            request.body.insert("confirm".into(), true.into());
        }
        Op::MemoryForget => {
            only(body, &["refs", "reason"])?;
            let refs = body
                .get("refs")
                .and_then(Value::as_array)
                .filter(|refs| (1..=100).contains(&refs.len()))
                .ok_or_else(|| ProError::bad("Choose up to 100 insights"))?;
            let mut checked = Vec::with_capacity(refs.len());
            for item in refs {
                let item = item
                    .as_str()
                    .filter(|r| is_fact_ref(r))
                    .ok_or_else(|| ProError::bad("Invalid insight key"))?;
                if !checked.contains(&item) {
                    checked.push(item);
                }
            }
            let reason = reason(body, "Before cleanup")?;
            request.body.insert("refs".into(), checked.into());
            request.body.insert("reason".into(), reason.into());
        }
        Op::MemoryConsolidate => {
            only(body, &["run"])?;
            request.body.insert("run".into(), flag(body, "run")?.into());
        }
        Op::AdaptiveApply => {
            only(body, &["path", "action"])?;
            let action = body.get("action").and_then(Value::as_str).unwrap_or("");
            if !["exclude", "compress"].contains(&action) {
                return Err(ProError::bad("Unsupported recommendation"));
            }
            request.body.insert("path".into(), path(body)?.into());
            request.body.insert("action".into(), action.into());
        }
        Op::AdaptiveDismiss => {
            only(body, &["path"])?;
            request.body.insert("path".into(), path(body)?.into());
        }
    }
    Ok(request)
}

fn query_strings(query: Map<String, Value>) -> ProResult<Map<String, Value>> {
    let mut out = Map::new();
    for (key, value) in query {
        let value = match value {
            Value::String(text) => text,
            Value::Number(number) => number.to_string(),
            Value::Array(mut items) if items.len() == 1 => match items.remove(0) {
                Value::String(text) => text,
                Value::Number(number) => number.to_string(),
                _ => return Err(ProError::bad("Unsupported query value")),
            },
            _ => return Err(ProError::bad("Unsupported query value")),
        };
        if value.len() > 256 || value.chars().any(char::is_control) {
            return Err(ProError::bad("Unsupported query value"));
        }
        out.insert(key, Value::String(value));
    }
    Ok(out)
}

fn only(map: &Map<String, Value>, keys: &[&str]) -> ProResult<()> {
    match map.keys().find(|k| !keys.contains(&k.as_str())) {
        Some(key) => Err(ProError::bad(format!("Unknown field {key}"))),
        None => Ok(()),
    }
}

fn text(map: &Map<String, Value>, key: &str, default: &str) -> String {
    map.get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .unwrap_or(default)
        .to_string()
}

fn flag(body: &Map<String, Value>, key: &str) -> ProResult<bool> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(ProError::bad(format!("{key} must be true or false"))),
    }
}

fn reason(body: &Map<String, Value>, default: &str) -> ProResult<String> {
    match body.get("reason") {
        None | Some(Value::Null) => Ok(default.to_string()),
        Some(Value::String(text)) if plain_text(text, 120) => {
            let text = text.trim();
            Ok(if text.is_empty() { default } else { text }.to_string())
        }
        Some(_) => Err(ProError::bad("Give a reason of up to 120 characters")),
    }
}

fn path(body: &Map<String, Value>) -> ProResult<String> {
    body.get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|p| !p.is_empty() && plain_text(p, 4096))
        .map(str::to_string)
        .ok_or_else(|| ProError::bad("path is required"))
}

pub(crate) fn is_profile_name(name: &str) -> bool {
    (1..=60).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b" ._-".contains(&b))
}

/// `category/key` of a fact, neither part empty or starting with `-`.
pub(crate) fn is_fact_ref(value: &str) -> bool {
    let Some((category, key)) = value.split_once('/') else {
        return false;
    };
    plain_text(value, 300)
        && !category.is_empty()
        && !key.is_empty()
        && !category.starts_with('-')
        && !key.starts_with('-')
}

fn is_month(value: &str) -> bool {
    chrono::NaiveDate::parse_from_str(&format!("{value}-01"), "%Y-%m-%d").is_ok()
        && value.len() == 7
}

pub(crate) fn is_day(value: &str) -> bool {
    value.len() == 10 && chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok()
}

fn ledger_query(op: Op, query: &Map<String, Value>, out: &mut Map<String, Value>) -> ProResult<()> {
    let mut keys = vec!["tool", "model", "q", "from", "to", "project"];
    if op == Op::LedgerBrowse {
        keys.extend(["limit", "offset"]);
    } else {
        keys.push("format");
    }
    only(query, &keys)?;
    let one = |name: &str, limit: usize| -> ProResult<String> {
        let value = text(query, name, "");
        if value.chars().count() > limit {
            return Err(ProError::bad("Unsupported ledger filter"));
        }
        Ok(value)
    };
    let tool = one("tool", 60)?;
    if !tool.is_empty()
        && !tool
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b))
    {
        return Err(ProError::bad("Unknown tool filter"));
    }
    let model = one("model", 80)?;
    let q = one("q", 60)?.trim().to_string();
    let from = one("from", 10)?;
    let to = one("to", 10)?;
    for day in [&from, &to] {
        if !day.is_empty() && !is_day(day) {
            return Err(ProError::bad("Use dates as YYYY-MM-DD"));
        }
    }
    if !from.is_empty() && !to.is_empty() && from > to {
        return Err(ProError::bad("The start date is after the end date"));
    }
    let project = one("project", 200)?;
    for (key, value) in [
        ("tool", tool),
        ("model", model),
        ("q", q),
        ("from", from),
        ("to", to),
        ("project", project),
    ] {
        out.insert(key.into(), value.into());
    }
    if op == Op::LedgerBrowse {
        let number = |name: &str, default: u64, max: u64| -> ProResult<u64> {
            let raw = text(query, name, "");
            if raw.is_empty() {
                return Ok(default);
            }
            raw.parse::<u64>()
                .ok()
                .filter(|n| *n <= max)
                .ok_or_else(|| ProError::bad("Use whole numbers for offset and limit"))
        };
        let limit = number("limit", 50, 500)?.max(1);
        let offset = number("offset", 0, 100_000_000)?;
        out.insert("limit".into(), limit.to_string().into());
        out.insert("offset".into(), offset.to_string().into());
    } else {
        let format = text(query, "format", "csv");
        if format != "csv" && format != "json" {
            return Err(ProError::bad("Choose csv or json"));
        }
        out.insert("format".into(), format.into());
    }
    Ok(())
}

fn alerts_body(body: &Map<String, Value>, out: &mut Map<String, Value>) -> ProResult<()> {
    only(body, &["enabled", "kinds", "desktop", "webhook"])?;
    for key in ["enabled", "desktop"] {
        if let Some(value) = body.get(key) {
            if !value.is_boolean() {
                return Err(ProError::bad(format!("{key} must be true or false")));
            }
            out.insert(key.into(), value.clone());
        }
    }
    if let Some(kinds) = body.get("kinds") {
        let list = kinds
            .as_array()
            .filter(|list| list.len() <= GUARD_KINDS.len())
            .ok_or_else(|| ProError::bad("Choose alert kinds from the guard list"))?;
        let mut checked: Vec<&str> = Vec::new();
        for kind in list {
            let kind = kind
                .as_str()
                .filter(|k| GUARD_KINDS.contains(k))
                .ok_or_else(|| ProError::bad("Choose alert kinds from the guard list"))?;
            if !checked.contains(&kind) {
                checked.push(kind);
            }
        }
        out.insert("kinds".into(), checked.into());
    }
    if let Some(webhook) = body.get("webhook") {
        let webhook = webhook
            .as_str()
            .filter(|w| w.len() <= 500)
            .ok_or_else(|| ProError::bad("Webhook must be a URL up to 500 characters"))?
            .trim();
        if !webhook.is_empty() && !is_safe_webhook(webhook) {
            return Err(ProError::bad(
                "Use an https webhook URL without embedded credentials",
            ));
        }
        out.insert("webhook".into(), webhook.into());
    }
    Ok(())
}

/// An `https://host/...` URL of at most 500 characters, without user info,
/// whitespace or control characters.
pub(crate) fn is_safe_webhook(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit_once(':').map_or(authority, |(h, port)| {
        if port.bytes().all(|b| b.is_ascii_digit()) && !port.is_empty() {
            h
        } else {
            authority
        }
    });
    url.len() <= 500
        && !authority.contains('@')
        && !host.is_empty()
        && !url.chars().any(|c| c.is_control() || c.is_whitespace())
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-[]:".contains(&b))
}
