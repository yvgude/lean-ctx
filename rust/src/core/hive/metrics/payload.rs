// SPDX-License-Identifier: Apache-2.0
//! Wire shape of `POST /api/account/metrics/v1` (`cloud-cockpit-metrics-v1`
//! §3) and the checks the edge applies, so a body that would be refused is
//! never sent. Field names are the contract's; every struct refuses unknown
//! fields so nothing beyond §3 can ride along.

use serde::{Deserialize, Serialize};

pub(crate) const SCHEMA: &str = "leanctx.cloud-metrics/v1";
/// Day entries one request may carry.
pub(crate) const MAX_DAYS_PER_REQUEST: usize = 62;
/// Largest request body the edge accepts.
pub(crate) const MAX_BODY_BYTES: usize = 1 << 20;
pub(crate) const MAX_SPEND: usize = 32;
pub(crate) const MAX_AGENTS: usize = 16;
const MAX_LABEL_CHARS: usize = 64;
const MAX_NAME_CHARS: usize = 64;
/// Largest integer a JSON number carries exactly (2^53).
pub(crate) const MAX_COUNT: u64 = 1 << 53;
const MAX_LIST_USD: f64 = 1e7;
/// How far back and ahead (UTC days) the edge accepts a `date`.
const MAX_AGE_DAYS: i64 = 400;
const MAX_SKEW_DAYS: i64 = 1;
/// Name of the bucket that absorbs spend or agent rows beyond the limits.
const OVERFLOW: &str = "other";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Upload {
    pub(crate) schema: String,
    pub(crate) device: Device,
    pub(crate) days: Vec<Day>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Device {
    pub(crate) id: String,
    pub(crate) label: Option<String>,
    pub(crate) engine_version: String,
    pub(crate) os: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Day {
    pub(crate) date: String,
    pub(crate) vault: Vault,
    pub(crate) savings: Savings,
    pub(crate) spend: Vec<Spend>,
    pub(crate) ledger: Ledger,
    pub(crate) guards: Guards,
    pub(crate) agents: Vec<Agent>,
    pub(crate) memory: Memory,
    pub(crate) receipts: u64,
    /// 1.9 (X4): how Hive helped in the project that day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) hive: Option<lean_ctx_protocol::hive::telemetry::MetricsHiveV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Vault {
    pub(crate) id: String,
    pub(crate) label: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Savings {
    pub(crate) tokens_requested: u64,
    pub(crate) tokens_delivered: u64,
    pub(crate) tokens_saved: u64,
    pub(crate) tool_calls: u64,
    pub(crate) cache_hits: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Spend {
    pub(crate) model: String,
    pub(crate) provider: String,
    pub(crate) requests: u64,
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: u64,
    pub(crate) cached_tokens: u64,
    pub(crate) list_usd: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Ledger {
    pub(crate) reduction: u64,
    pub(crate) edit: u64,
    pub(crate) correction: u64,
    pub(crate) cache_discount: u64,
    pub(crate) other: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Guards {
    pub(crate) secrets_redacted: u64,
    pub(crate) pii_redacted: u64,
    pub(crate) injection_flags: u64,
    pub(crate) content_withheld: u64,
    pub(crate) paths_blocked: u64,
    pub(crate) shell_blocked: u64,
    pub(crate) partial_inspections: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Agent {
    pub(crate) host: String,
    pub(crate) sessions: u64,
    pub(crate) tool_calls: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Memory {
    pub(crate) facts_current: u64,
    pub(crate) facts_added: u64,
    pub(crate) lessons_linked: u64,
}

impl Day {
    /// Whether anything happened in this project on this day. `facts_current`
    /// is a stock, not activity, so a quiet day is not reported.
    pub(crate) fn has_activity(&self) -> bool {
        let s = &self.savings;
        let l = &self.ledger;
        let g = &self.guards;
        [
            s.tokens_requested,
            s.tokens_delivered,
            s.tokens_saved,
            s.tool_calls,
            s.cache_hits,
            l.reduction,
            l.edit,
            l.correction,
            l.cache_discount,
            l.other,
            g.secrets_redacted,
            g.pii_redacted,
            g.injection_flags,
            g.content_withheld,
            g.paths_blocked,
            g.shell_blocked,
            g.partial_inspections,
            self.memory.facts_added,
            self.memory.lessons_linked,
            self.receipts,
        ]
        .into_iter()
        .any(|n| n > 0)
            || !self.spend.is_empty()
            || !self.agents.is_empty()
            || self.hive.is_some()
    }
}

/// A display label as §3 allows it: at most 64 characters, no control
/// characters, no `/` or `\`. Offending characters are dropped; nothing
/// usable left means no label.
#[cfg(test)]
pub(crate) fn label(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .trim()
        .chars()
        .filter(|c| !c.is_control() && *c != '/' && *c != '\\')
        .take(MAX_LABEL_CHARS)
        .collect();
    let cleaned = cleaned.trim().to_string();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// A `model`, `provider` or `host` as §3 allows it: 1–64 characters of
/// `[A-Za-z0-9._:/-]`; other characters become `-`. `None` when nothing
/// meaningful remains.
pub(crate) fn name(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .trim()
        .chars()
        .map(|c| if is_name_char(c) { c } else { '-' })
        .take(MAX_NAME_CHARS)
        .collect();
    let trimmed = cleaned.trim_matches('-');
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '/' | '-')
}

fn valid_name(value: &str) -> bool {
    (1..=MAX_NAME_CHARS).contains(&value.chars().count()) && value.chars().all(is_name_char)
}

fn valid_label(value: Option<&str>) -> bool {
    value.is_none_or(|v| {
        !v.is_empty()
            && v.chars().count() <= MAX_LABEL_CHARS
            && !v.chars().any(|c| c.is_control() || c == '/' || c == '\\')
    })
}

/// The operating system name §3 accepts for this build, if any.
pub(crate) fn os_name(os: &str) -> Option<&'static str> {
    match os {
        "macos" => Some("macos"),
        "linux" => Some("linux"),
        "windows" => Some("windows"),
        _ => None,
    }
}

pub(crate) fn valid_engine_version(value: &str) -> bool {
    (1..=32).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-'))
}

/// A UUID v4 in its canonical lowercase hyphenated form.
pub(crate) fn valid_device_id(value: &str) -> bool {
    uuid::Uuid::parse_str(value)
        .is_ok_and(|id| id.get_version_num() == 4 && id.hyphenated().to_string() == value)
}

fn valid_project_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Count as a JSON-safe integer.
pub(crate) fn count(n: u64) -> u64 {
    n.min(MAX_COUNT)
}

/// List price in USD as §3 bounds it; anything not finite counts as 0.
pub(crate) fn usd(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, MAX_LIST_USD)
    } else {
        0.0
    }
}

/// Keeps at most [`MAX_SPEND`] spend rows: the most expensive ones, the rest
/// merged into one `other` row so the day's totals stay whole.
pub(crate) fn bound_spend(mut rows: Vec<Spend>) -> Vec<Spend> {
    rows.sort_by(|a, b| {
        b.list_usd
            .total_cmp(&a.list_usd)
            .then_with(|| a.model.cmp(&b.model))
            .then_with(|| a.provider.cmp(&b.provider))
    });
    if rows.len() <= MAX_SPEND {
        return rows;
    }
    let rest = rows.split_off(MAX_SPEND - 1);
    let mut other = Spend {
        model: OVERFLOW.into(),
        provider: OVERFLOW.into(),
        requests: 0,
        input_tokens: 0,
        output_tokens: 0,
        cached_tokens: 0,
        list_usd: 0.0,
    };
    for row in rest {
        other.requests = count(other.requests.saturating_add(row.requests));
        other.input_tokens = count(other.input_tokens.saturating_add(row.input_tokens));
        other.output_tokens = count(other.output_tokens.saturating_add(row.output_tokens));
        other.cached_tokens = count(other.cached_tokens.saturating_add(row.cached_tokens));
        other.list_usd = usd(other.list_usd + row.list_usd);
    }
    rows.push(other);
    rows
}

/// Keeps at most [`MAX_AGENTS`] agent rows, the busiest ones, the rest merged
/// into one `other` row.
pub(crate) fn bound_agents(mut rows: Vec<Agent>) -> Vec<Agent> {
    rows.sort_by(|a, b| {
        b.tool_calls
            .cmp(&a.tool_calls)
            .then_with(|| a.host.cmp(&b.host))
    });
    if rows.len() <= MAX_AGENTS {
        return rows;
    }
    let rest = rows.split_off(MAX_AGENTS - 1);
    let mut other = Agent {
        host: OVERFLOW.into(),
        sessions: 0,
        tool_calls: 0,
    };
    for row in rest {
        other.sessions = count(other.sessions.saturating_add(row.sessions));
        other.tool_calls = count(other.tool_calls.saturating_add(row.tool_calls));
    }
    rows.push(other);
    rows
}

/// Every rule of §3 the edge enforces, checked against `today` (UTC).
pub(crate) fn validate(upload: &Upload, today: chrono::NaiveDate) -> Result<(), String> {
    if upload.schema != SCHEMA {
        return Err("schema".into());
    }
    let device = &upload.device;
    if !valid_device_id(&device.id) {
        return Err("device.id".into());
    }
    if !valid_label(device.label.as_deref()) {
        return Err("device.label".into());
    }
    if !valid_engine_version(&device.engine_version) {
        return Err("device.engine_version".into());
    }
    if os_name(&device.os).is_none() {
        return Err("device.os".into());
    }
    if upload.days.is_empty() || upload.days.len() > MAX_DAYS_PER_REQUEST {
        return Err("days: 1–62 entries".into());
    }
    let oldest = today - chrono::Duration::days(MAX_AGE_DAYS);
    let newest = today + chrono::Duration::days(MAX_SKEW_DAYS);
    let mut seen = std::collections::HashSet::new();
    for day in &upload.days {
        let date = chrono::NaiveDate::parse_from_str(&day.date, "%Y-%m-%d")
            .ok()
            .filter(|d| d.format("%Y-%m-%d").to_string() == day.date)
            .ok_or("date")?;
        if date < oldest || date > newest {
            return Err("date out of range".into());
        }
        if !seen.insert((day.date.as_str(), day.vault.id.as_str())) {
            return Err("duplicate (date, vault)".into());
        }
        if !valid_project_id(&day.vault.id) || !valid_label(day.vault.label.as_deref()) {
            return Err("vault".into());
        }
        if day.spend.len() > MAX_SPEND || day.agents.len() > MAX_AGENTS {
            return Err("spend/agents limit".into());
        }
        for row in &day.spend {
            if !valid_name(&row.model) || !valid_name(&row.provider) {
                return Err("spend name".into());
            }
            if !row.list_usd.is_finite() || !(0.0..=MAX_LIST_USD).contains(&row.list_usd) {
                return Err("list_usd".into());
            }
        }
        if day.agents.iter().any(|a| !valid_name(&a.host)) {
            return Err("agent host".into());
        }
        if counts(day).any(|n| n > MAX_COUNT) {
            return Err("count".into());
        }
    }
    Ok(())
}

fn counts(day: &Day) -> impl Iterator<Item = u64> + '_ {
    let s = day.savings;
    let l = day.ledger;
    let g = day.guards;
    let m = day.memory;
    [
        s.tokens_requested,
        s.tokens_delivered,
        s.tokens_saved,
        s.tool_calls,
        s.cache_hits,
        l.reduction,
        l.edit,
        l.correction,
        l.cache_discount,
        l.other,
        g.secrets_redacted,
        g.pii_redacted,
        g.injection_flags,
        g.content_withheld,
        g.paths_blocked,
        g.shell_blocked,
        g.partial_inspections,
        m.facts_current,
        m.facts_added,
        m.lessons_linked,
        day.receipts,
    ]
    .into_iter()
    .chain(
        day.spend
            .iter()
            .flat_map(|r| [r.requests, r.input_tokens, r.output_tokens, r.cached_tokens]),
    )
    .chain(day.agents.iter().flat_map(|a| [a.sessions, a.tool_calls]))
}

/// Splits `days` into requests of at most 62 entries and 1 MiB each. A
/// single entry larger than 1 MiB cannot be sent and is dropped.
pub(crate) fn chunk(device: &Device, days: Vec<Day>) -> Vec<Upload> {
    let upload = |days: Vec<Day>| Upload {
        schema: SCHEMA.into(),
        device: device.clone(),
        days,
    };
    let size =
        |days: &[Day]| serde_json::to_vec(&upload(days.to_vec())).map_or(usize::MAX, |b| b.len());
    let mut requests = Vec::new();
    let mut pending: Vec<Day> = Vec::new();
    for day in days {
        pending.push(day);
        if size(&pending) > MAX_BODY_BYTES {
            let last = pending.pop();
            if !pending.is_empty() {
                requests.push(upload(std::mem::take(&mut pending)));
            }
            if let Some(last) = last
                && size(std::slice::from_ref(&last)) <= MAX_BODY_BYTES
            {
                pending.push(last);
            }
        } else if pending.len() == MAX_DAYS_PER_REQUEST {
            requests.push(upload(std::mem::take(&mut pending)));
        }
    }
    if !pending.is_empty() {
        requests.push(upload(pending));
    }
    requests
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(date: &str, project: &str) -> Day {
        Day {
            date: date.into(),
            vault: Vault {
                id: project.into(),
                label: Some("lean-ctx".into()),
            },
            savings: Savings {
                tokens_requested: 10,
                tokens_delivered: 4,
                tokens_saved: 6,
                tool_calls: 3,
                cache_hits: 1,
            },
            spend: vec![Spend {
                model: "claude-opus-5-5".into(),
                provider: "anthropic".into(),
                requests: 2,
                input_tokens: 100,
                output_tokens: 50,
                cached_tokens: 900,
                list_usd: 0.25,
            }],
            ledger: Ledger {
                reduction: 2,
                ..Ledger::default()
            },
            guards: Guards::default(),
            agents: vec![Agent {
                host: "claude".into(),
                sessions: 1,
                tool_calls: 3,
            }],
            memory: Memory::default(),
            receipts: 0,
            hive: None,
        }
    }

    fn upload() -> Upload {
        Upload {
            schema: SCHEMA.into(),
            device: Device {
                id: "0b7d1c3e-9f2a-4c5b-8d6e-1a2b3c4d5e6f".into(),
                label: Some("mbp".into()),
                engine_version: "3.11.5".into(),
                os: "macos".into(),
            },
            days: vec![day("2026-10-20", &"a".repeat(32))],
        }
    }

    fn keys(value: &serde_json::Value) -> Vec<&str> {
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        keys
    }

    /// The upload is sent as the protocol's `MetricsUploadV1` (upload.rs parses
    /// it into that type first), so every field name must be the contract's:
    /// after the vault rename a day names its `vault`, not its `project`.
    #[test]
    fn the_upload_parses_as_the_protocol_type() {
        let wire: lean_ctx_protocol::hive::telemetry::MetricsUploadV1 =
            serde_json::from_value(serde_json::to_value(upload()).unwrap())
                .expect("the contract's MetricsUploadV1 reads the engine's upload");
        assert_eq!(wire.days.len(), upload().days.len());
    }

    #[test]
    fn body_has_exactly_the_contract_fields_and_the_edge_rules_hold() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 20).unwrap();
        let json = serde_json::to_value(upload()).unwrap();
        assert_eq!(keys(&json), ["days", "device", "schema"]);
        assert_eq!(
            keys(&json["device"]),
            ["engine_version", "id", "label", "os"]
        );
        let d = &json["days"][0];
        assert_eq!(
            keys(d),
            [
                "agents", "date", "guards", "ledger", "memory", "receipts", "savings", "spend",
                "vault"
            ]
        );
        assert_eq!(
            keys(&d["savings"]),
            [
                "cache_hits",
                "tokens_delivered",
                "tokens_requested",
                "tokens_saved",
                "tool_calls"
            ]
        );
        assert_eq!(
            keys(&d["spend"][0]),
            [
                "cached_tokens",
                "input_tokens",
                "list_usd",
                "model",
                "output_tokens",
                "provider",
                "requests"
            ]
        );
        assert_eq!(
            keys(&d["ledger"]),
            ["cache_discount", "correction", "edit", "other", "reduction"]
        );
        assert_eq!(
            keys(&d["guards"]),
            [
                "content_withheld",
                "injection_flags",
                "partial_inspections",
                "paths_blocked",
                "pii_redacted",
                "secrets_redacted",
                "shell_blocked"
            ]
        );
        assert_eq!(keys(&d["agents"][0]), ["host", "sessions", "tool_calls"]);
        assert_eq!(
            keys(&d["memory"]),
            ["facts_added", "facts_current", "lessons_linked"]
        );
        // Labels switched off are sent as null, not left out.
        let mut unlabelled = upload();
        unlabelled.days[0].vault.label = None;
        let json = serde_json::to_value(&unlabelled).unwrap();
        assert!(json["days"][0]["vault"]["label"].is_null());
        // Our own shape refuses extra fields, like the edge.
        let mut extra = serde_json::to_value(upload()).unwrap();
        extra["days"][0]["savings"]["path"] = "x".into();
        assert!(serde_json::from_value::<Upload>(extra).is_err());

        assert_eq!(validate(&upload(), today), Ok(()));
        type Breach = fn(&mut Upload);
        let breaches: [(&str, Breach); 14] = [
            ("schema", |u| u.schema = "leanctx.cloud-metrics/v2".into()),
            ("device id not v4", |u| {
                u.device.id = "0b7d1c3e-9f2a-1c5b-8d6e-1a2b3c4d5e6f".into();
            }),
            ("os", |u| u.device.os = "freebsd".into()),
            ("engine version", |u| {
                u.device.engine_version = "3.11.5 beta".into();
            }),
            ("no days", |u| u.days.clear()),
            ("63 days", |u| {
                u.days = (0..63)
                    .map(|i| day("2026-10-20", &format!("{i:032x}")))
                    .collect();
            }),
            ("future date", |u| u.days[0].date = "2026-10-22".into()),
            ("too old", |u| u.days[0].date = "2025-09-14".into()),
            ("duplicate", |u| u.days.push(u.days[0].clone())),
            ("vault id", |u| u.days[0].vault.id = "A".repeat(32)),
            ("label slash", |u| {
                u.days[0].vault.label = Some("a/b".into());
            }),
            ("model charset", |u| {
                u.days[0].spend[0].model = "claude opus".into();
            }),
            ("list_usd", |u| u.days[0].spend[0].list_usd = f64::NAN),
            ("count", |u| u.days[0].receipts = MAX_COUNT + 1),
        ];
        for (rule, breach) in breaches {
            let mut bad = upload();
            breach(&mut bad);
            assert!(validate(&bad, today).is_err(), "{rule} must be refused");
        }
        // One day of clock skew is allowed.
        let mut skewed = upload();
        skewed.days[0].date = "2026-10-21".into();
        assert_eq!(validate(&skewed, today), Ok(()));
    }

    #[test]
    fn names_labels_and_limits_are_made_valid_instead_of_refused() {
        assert_eq!(label(" a/b\\c\u{7} ").as_deref(), Some("abc"));
        assert_eq!(label("/").as_deref(), None);
        assert_eq!(label(&"x".repeat(80)).map(|l| l.len()), Some(64));
        assert_eq!(name("gpt 5.4 (mini)").as_deref(), Some("gpt-5.4--mini"));
        assert_eq!(name("über").as_deref(), Some("ber"));
        assert_eq!(name("  ").as_deref(), None);

        let rows: Vec<Spend> = (0..40)
            .map(|i| Spend {
                model: format!("m{i}"),
                provider: "p".into(),
                requests: 1,
                input_tokens: 10,
                output_tokens: 1,
                cached_tokens: 0,
                list_usd: f64::from(i),
            })
            .collect();
        let bounded = bound_spend(rows);
        assert_eq!(bounded.len(), MAX_SPEND);
        assert_eq!(bounded.iter().map(|r| r.requests).sum::<u64>(), 40);
        assert_eq!(bounded.last().unwrap().model, "other");

        let agents: Vec<Agent> = (0..20)
            .map(|i| Agent {
                host: format!("h{i}"),
                sessions: 1,
                tool_calls: i,
            })
            .collect();
        let bounded = bound_agents(agents);
        assert_eq!(bounded.len(), MAX_AGENTS);
        assert_eq!(bounded.iter().map(|a| a.sessions).sum::<u64>(), 20);

        // 100 project-days go out as requests of at most 62 entries.
        let device = upload().device;
        let days: Vec<Day> = (0..100)
            .map(|i| day("2026-10-20", &format!("{i:032x}")))
            .collect();
        let requests = chunk(&device, days);
        assert_eq!(
            requests.iter().map(|r| r.days.len()).collect::<Vec<_>>(),
            [62, 38]
        );
    }
}
