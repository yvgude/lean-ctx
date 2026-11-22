// SPDX-License-Identifier: Apache-2.0
//! LeanCTX Cloud usage (`GET /v1/usage`): how far each quota of the
//! workspace is used, so this machine warns at 80 % instead of meeting a
//! refusal.
//!
//! Read only from a server that announced the `usage` capability
//! ([`FEATURE_USAGE`], cached by [`crate::session_ref`]); an older server
//! gets no request and nothing here prints anything.
//!
//! - `lean-ctx cloud status` shows every quota as "used of limit" (GiB for
//!   bytes, $ for model credit, counts otherwise), `warn` and `full` marked
//!   with the service's next step ([`status_lines`]).
//! - The background sync reads it at most every [`REFRESH_EVERY`] and logs
//!   one line when a quota rises to `warn` or `full` ([`rises`]); the states
//!   it last logged are kept in `<data>/usage.json`, so neither the next
//!   tick nor a restart repeats the line.

use std::collections::BTreeMap;
use std::time::Duration;

use lean_ctx_protocol::cloud::usage::{CloudQuotaStateV1, CloudQuotaV1, CloudUsageV1};
use lean_ctx_time::UtcDateTime;
use serde::{Deserialize, Serialize};

pub use lean_ctx_protocol::cloud::usage::FEATURE_USAGE;

use crate::CloudError;
use crate::auth::Session;
use crate::config::CloudConfig;
use crate::error::{NextStep, amount_text, quota_label};
use crate::gateway::BALANCE_RESOURCE;
use crate::state::{CloudStore, write_private};

/// The last answer and the states already logged.
const USAGE_FILE: &str = "usage.json";

/// How often the background sync asks (a status asks every time).
pub const REFRESH_EVERY: Duration = Duration::from_mins(10);

/// The quotas in the order the Plan page shows them; others follow by name.
const ORDER: [&str; 7] = [
    "storage_bytes",
    "transfer_bytes",
    "bus_messages",
    BALANCE_RESOURCE,
    "private_vaults",
    "registered_instances",
    "connected_instances",
];

/// Width of the label column of the status lines.
const LABEL_WIDTH: usize = 22;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct Cache {
    /// The origin that answered; another enrolled server reads none of it.
    server: String,
    /// When [`Self::usage`] was read.
    fetched_at: Option<String>,
    /// When the background sync last asked, answered or not.
    tried_at: Option<String>,
    usage: Option<CloudUsageV1>,
    /// Per resource, the state the background sync last saw.
    logged: BTreeMap<String, CloudQuotaStateV1>,
}

fn load(store: &CloudStore) -> Cache {
    std::fs::read(store.data_dir().join(USAGE_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save(store: &CloudStore, cache: &Cache) {
    if let Ok(bytes) = serde_json::to_vec_pretty(cache) {
        let _ = write_private(store.data_dir(), USAGE_FILE, &bytes);
    }
}

/// The cache of `server`; another server's starts empty.
fn load_for(store: &CloudStore, server: &str) -> Cache {
    let cache = load(store);
    if cache.server == server {
        cache
    } else {
        Cache {
            server: server.to_string(),
            ..Cache::default()
        }
    }
}

fn stamp(at: UtcDateTime) -> String {
    at.to_rfc3339_opts(lean_ctx_time::SecondsFormat::Secs, true)
}

/// Whether the enrolled server announced `usage` when last asked.
#[must_use]
pub fn announced(store: &CloudStore) -> bool {
    crate::session_ref::announced(store, FEATURE_USAGE)
}

/// Read `GET /v1/usage`; `None` without the capability (no request).
pub fn fetch(session: &Session) -> Result<Option<CloudUsageV1>, CloudError> {
    if !announced(&session.store) {
        return Ok(None);
    }
    let answer = session.with_client(|client| client.get("/usage"))?;
    decode(answer).map(Some)
}

fn decode(answer: serde_json::Value) -> Result<CloudUsageV1, CloudError> {
    let usage: CloudUsageV1 =
        serde_json::from_value(answer).map_err(|e| CloudError::Protocol(format!("usage: {e}")))?;
    usage
        .validate()
        .map_err(|e| CloudError::Protocol(format!("usage: {e}")))?;
    Ok(usage)
}

// ---------------------------------------------------------------------------
// Background sync: one line per rise.

/// The background sync's look at the usage: at most every
/// [`REFRESH_EVERY`], only with the capability; logs each quota that rose to
/// `warn` or `full` since the last look, once.
pub(crate) fn background(session: &Session, now: UtcDateTime) {
    if !announced(&session.store) {
        return;
    }
    let store = &session.store;
    let mut cache = load_for(store, &session.instance.url);
    if !due(cache.tried_at.as_deref(), now) {
        return;
    }
    cache.tried_at = Some(stamp(now));
    match fetch(session) {
        Ok(Some(usage)) => {
            for line in note(&mut cache, usage, now) {
                tracing::warn!("{line}");
            }
        }
        Ok(None) => {}
        Err(error) => tracing::debug!("cloud: usage not read: {error}"),
    }
    save(store, &cache);
}

/// Whether [`REFRESH_EVERY`] passed since `tried_at` (or it never ran).
fn due(tried_at: Option<&str>, now: UtcDateTime) -> bool {
    tried_at
        .and_then(|t| UtcDateTime::parse_from_rfc3339(t).ok())
        .is_none_or(|at| {
            (now - at)
                .to_std()
                .map_or(true, |since| since >= REFRESH_EVERY)
        })
}

/// Take a fresh answer into `cache`: the lines of the quotas that rose, and
/// the states seen now as the new baseline. The baseline holds the quotas
/// [`ordered`] shows, no others: a model credit hidden at $0 that later gets
/// a balance and is used up is said then, not taken as already seen.
fn note(cache: &mut Cache, usage: CloudUsageV1, now: UtcDateTime) -> Vec<String> {
    let lines = rises(&cache.logged, &usage);
    cache.logged = ordered(&usage)
        .map(|(r, q)| (r.to_owned(), q.state))
        .collect();
    cache.usage = Some(usage);
    cache.fetched_at = Some(stamp(now));
    lines
}

/// One line per quota now at `warn` or `full` whose state was lower when
/// last seen (`seen`): a state change, never a repeat. A quota that falls
/// back and rises again is said again.
#[must_use]
pub fn rises(seen: &BTreeMap<String, CloudQuotaStateV1>, usage: &CloudUsageV1) -> Vec<String> {
    ordered(usage)
        .filter(|(r, q)| q.state.level() > seen.get(*r).map_or(0, |s| s.level()))
        .map(|(r, q)| rise_line(r, q))
        .collect()
}

/// The log line of a quota that reached `warn` or `full`, worded like the
/// refusal it comes before.
#[must_use]
pub fn rise_line(resource: &str, q: &CloudQuotaV1) -> String {
    let what = quota_label(resource);
    let amounts = format!(
        "{} of {}",
        amount_text(resource, q.used),
        amount_text(resource, q.limit)
    );
    let mut line = if q.state == CloudQuotaStateV1::Full {
        format!(
            "LeanCTX Cloud: all of your {what} is used ({amounts}); nothing is charged for more"
        )
    } else {
        format!(
            "LeanCTX Cloud: {} % of your {what} is used ({amounts})",
            percent(q)
        )
    };
    if let Some(at) = reset_text(q) {
        line.push_str(&format!("; it frees at {at}"));
    }
    if let Some(hint) = hint(q) {
        line.push_str("; ");
        line.push_str(hint);
    }
    line
}

// ---------------------------------------------------------------------------
// `lean-ctx cloud status`.

/// The `Usage:` lines of `lean-ctx cloud status`: read now when the server
/// announced `usage` (the last answer, dated, when that fails); nothing at
/// all without the capability.
pub fn status(store: &CloudStore, config: &CloudConfig, now: UtcDateTime) -> Vec<String> {
    if !announced(store) || !matches!(store.instance(), Ok(Some(_))) {
        return Vec::new();
    }
    match read(store, config, now) {
        Ok(Some(r)) if r.live => status_lines(&r.usage, None),
        Ok(Some(r)) => status_lines(&r.usage, r.as_of.as_deref()),
        Ok(None) => Vec::new(),
        Err(error) => vec![format!(
            "Usage:        not read now ({})",
            why_not_read(&error)
        )],
    }
}

/// Why usage could not be read, in the words of `cloud status` (the
/// [`CloudError`] Display text names the service by its old name).
fn why_not_read(error: &CloudError) -> &'static str {
    match error {
        CloudError::Offline(_) => "LeanCTX Cloud did not answer",
        CloudError::NotConnected => "this machine is not connected; run `lean-ctx cloud connect`",
        CloudError::Unauthorized | CloudError::LeaseLapsed | CloudError::Revoked(_) => {
            "LeanCTX Cloud no longer accepts this machine; run `lean-ctx cloud connect` again"
        }
        CloudError::Forbidden | CloudError::PlanRequired(_) => {
            "your role or plan does not show usage"
        }
        _ if error.is_transient() => "LeanCTX Cloud is busy; try again shortly",
        _ => "LeanCTX Cloud refused the request",
    }
}

/// A usage answer and when it was read.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageRead {
    pub usage: CloudUsageV1,
    /// When the answer was read (RFC 3339).
    pub as_of: Option<String>,
    /// Read just now (`false`: the last answer; the server did not answer).
    pub live: bool,
}

/// The usage for a reader (`lean-ctx cloud status`, `cloud usage --json`):
/// read now, else the last answer of this server. `None` when the server
/// did not announce `usage` (no request is sent).
pub fn read(
    store: &CloudStore,
    config: &CloudConfig,
    now: UtcDateTime,
) -> Result<Option<UsageRead>, CloudError> {
    if !announced(store) {
        return Ok(None);
    }
    let instance = store.instance()?.ok_or(CloudError::NotConnected)?;
    let live = crate::scheduler::connected_session(store, config).and_then(|s| fetch(&s));
    let mut cache = load_for(store, &instance.url);
    match live {
        Ok(Some(usage)) => {
            // The background sync's baseline (`logged`) stays its own.
            cache.usage = Some(usage.clone());
            cache.fetched_at = Some(stamp(now));
            save(store, &cache);
            Ok(Some(UsageRead {
                usage,
                as_of: cache.fetched_at,
                live: true,
            }))
        }
        Ok(None) => Ok(None),
        Err(error) => match cache.usage {
            Some(usage) => Ok(Some(UsageRead {
                usage,
                as_of: cache.fetched_at,
                live: false,
            })),
            None => Err(error),
        },
    }
}

/// The status lines of `usage`; `as_of` dates an answer that was not read
/// just now.
#[must_use]
pub fn status_lines(usage: &CloudUsageV1, as_of: Option<&str>) -> Vec<String> {
    let plan = match usage.plan.as_str() {
        "personal" => "Personal",
        "team" => "Team",
        "enterprise" => "Enterprise",
        _ => "no plan",
    };
    let mut head = format!("Usage:        LeanCTX Cloud, {plan}");
    if usage.read_only || usage.plan == "locked" {
        head.push_str(" (read and export only)");
    }
    if let Some(at) = as_of {
        head.push_str(&format!(" (as of {at})"));
    }
    let mut lines = vec![head];
    lines.extend(ordered(usage).map(|(r, q)| quota_line(r, q)));
    lines
}

/// One quota: `  storage   34 GiB of 40 GiB · warn (85 %); <next step>`.
#[must_use]
pub fn quota_line(resource: &str, q: &CloudQuotaV1) -> String {
    let mut line = format!(
        "  {:<LABEL_WIDTH$}{} of {}",
        quota_label(resource),
        amount_text(resource, q.used),
        amount_text(resource, q.limit)
    );
    match q.state {
        CloudQuotaStateV1::Full => {
            line.push_str(" · FULL");
            if let Some(at) = reset_text(q) {
                line.push_str(&format!(" until {at}"));
            }
        }
        CloudQuotaStateV1::Warn => line.push_str(&format!(" · warn ({} %)", percent(q))),
        CloudQuotaStateV1::Ok | CloudQuotaStateV1::Unknown => return line,
    }
    if let Some(hint) = hint(q) {
        line.push_str("; ");
        line.push_str(hint);
    }
    line
}

/// The quotas in Plan page order, unknown resources after them by name.
/// A model credit with limit $0 (no balance, e.g. a Personal trial that uses
/// its own model access) is no quota to show or warn about: it would read as
/// "$0.00 of $0.00 · FULL". That holds even when the answer reports use
/// against it (`used > 0`): without a balance there is nothing to run out of.
fn ordered(usage: &CloudUsageV1) -> impl Iterator<Item = (&str, &CloudQuotaV1)> {
    let known = ORDER.iter().filter_map(|r| usage.quotas.get_key_value(*r));
    let rest = usage
        .quotas
        .iter()
        .filter(|(r, _)| !ORDER.contains(&r.as_str()));
    known
        .chain(rest)
        .map(|(r, q)| (r.as_str(), q))
        .filter(|(r, q)| !(*r == BALANCE_RESOURCE && q.limit == 0))
}

/// Used of limit in whole percent, rounded down (a zero limit is full).
fn percent(q: &CloudQuotaV1) -> u128 {
    if q.limit == 0 {
        return 100;
    }
    u128::from(q.used) * 100 / u128::from(q.limit)
}

/// The service's next step as the refusal words it (`wait` adds none).
fn hint(q: &CloudQuotaV1) -> Option<&'static str> {
    NextStep::of_wire(q.next_step.as_deref()).and_then(NextStep::hint)
}

/// `reset_at` of a full or warned quota, when it is a timestamp.
fn reset_text(q: &CloudQuotaV1) -> Option<String> {
    let at = UtcDateTime::parse_from_rfc3339(q.reset_at.as_deref()?).ok()?;
    Some(stamp(at))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn order_lists_the_balance_resource_from_the_gateway() {
        assert!(ORDER.contains(&BALANCE_RESOURCE));
        assert_eq!(BALANCE_RESOURCE, "model_credit_usd_micros");
    }

    fn fixture() -> serde_json::Value {
        serde_json::from_str(include_str!(
            "../../lean-ctx-protocol/tests/fixtures/cloud/usage.response.json"
        ))
        .unwrap()
    }

    fn usage() -> CloudUsageV1 {
        decode(fixture()).unwrap()
    }

    fn now() -> UtcDateTime {
        UtcDateTime::parse_from_rfc3339("2026-11-17T12:00:00Z").unwrap()
    }

    fn enrolled(dir: &std::path::Path, features: &[&str]) -> CloudStore {
        let store = CloudStore::at(dir.join("config"), dir.join("cloud"));
        let instance: crate::state::InstanceRecord = serde_json::from_value(json!({
            "instance_id": "inst_1", "org_id": "org_acme", "org_name": "Platform",
            "label": "laptop", "url": "https://cloud.example", "org_salt_id": "salt",
            "enrolled_at": "2026-11-16T12:00:00Z"
        }))
        .unwrap();
        store
            .write_config(crate::state::INSTANCE_FILE, &instance)
            .unwrap();
        crate::session_ref::record(
            &store,
            "https://cloud.example",
            features.iter().map(|f| (*f).to_string()).collect(),
        )
        .unwrap();
        store
    }

    #[test]
    fn status_shows_every_quota_in_its_unit_with_warn_and_full_marked() {
        let lines = status_lines(&usage(), None);
        assert_eq!(
            lines,
            [
                "Usage:        LeanCTX Cloud, Personal",
                "  storage               34 GiB of 40 GiB · warn (85 %); Team gives every seat its own allowance",
                "  monthly transfer      40 GiB of 40 GiB · FULL until 2026-12-01T00:00:00Z; Team gives every seat its own allowance",
                "  monthly bus messages  1200 of 200000",
                "  model credit          $1.25 of $8.00",
                "  private vaults        3 of 20",
                "  instance slots        2 of 3",
                "  connected instances   1 of 2",
            ]
        );
        for line in &lines {
            for word in ["engine", "Queen", "workspace"] {
                assert!(!line.contains(word), "{line}");
            }
        }
    }

    #[test]
    fn a_model_credit_of_nothing_is_neither_shown_nor_warned() {
        let mut raw = fixture();
        raw["quotas"]["model_credit_usd_micros"] =
            json!({"limit": 0, "used": 0, "state": "full", "next_step": "top_up"});
        let usage = decode(raw).unwrap();
        let lines = status_lines(&usage, None);
        assert!(
            lines.iter().all(|l| !l.contains("model credit")),
            "{lines:?}"
        );
        assert_eq!(lines.len(), 7, "{lines:?}");
        let rises = rises(&BTreeMap::new(), &usage);
        assert!(
            rises.iter().all(|l| !l.contains("model credit")),
            "{rises:?}"
        );
    }

    #[test]
    fn a_model_credit_without_balance_stays_hidden_with_use_reported() {
        let mut raw = fixture();
        raw["quotas"]["model_credit_usd_micros"] =
            json!({"limit": 0, "used": 1_250_000, "state": "full", "next_step": "top_up"});
        let usage = decode(raw).unwrap();
        let lines = status_lines(&usage, None);
        assert!(
            lines.iter().all(|l| !l.contains("model credit")),
            "{lines:?}"
        );
        assert!(
            rises(&BTreeMap::new(), &usage)
                .iter()
                .all(|l| !l.contains("model credit"))
        );
    }

    #[test]
    fn a_model_credit_that_gets_a_balance_and_runs_out_is_said_then() {
        let mut cache = Cache::default();
        let mut raw = fixture();
        raw["quotas"]["model_credit_usd_micros"] =
            json!({"limit": 0, "used": 0, "state": "full", "next_step": "top_up"});
        let first = note(&mut cache, decode(raw.clone()).unwrap(), now());
        assert!(
            first.iter().all(|l| !l.contains("model credit")),
            "{first:?}"
        );
        assert!(
            !cache.logged.contains_key(BALANCE_RESOURCE),
            "a hidden quota is no baseline"
        );
        // A top-up of $8.00, all used: the first time the credit can run out.
        raw["quotas"]["model_credit_usd_micros"] =
            json!({"limit": 8_000_000, "used": 8_000_000, "state": "full", "next_step": "top_up"});
        let topped = note(&mut cache, decode(raw).unwrap(), now());
        assert_eq!(
            topped
                .iter()
                .filter(|l| l.contains("all of your model credit is used ($8.00 of $8.00)"))
                .count(),
            1,
            "{topped:?}"
        );
    }

    #[test]
    fn a_locked_or_cached_answer_says_so_and_unknown_resources_follow() {
        let mut raw = fixture();
        raw["plan"] = "locked".into();
        raw["read_only"] = true.into();
        raw["quotas"]["agent_seats"] = json!({"limit": 4, "used": 4, "state": "full",
                                              "next_step": "contact", "reset_at": "soon"});
        let lines = status_lines(&decode(raw).unwrap(), Some("2026-11-17T11:00:00Z"));
        assert_eq!(
            lines[0],
            "Usage:        LeanCTX Cloud, no plan (read and export only) (as of 2026-11-17T11:00:00Z)"
        );
        // A reset that is no timestamp is left out.
        assert_eq!(
            lines.last().unwrap(),
            "  agent_seats           4 of 4 · FULL; contact us to extend your Enterprise contract"
        );
    }

    #[test]
    fn a_rise_is_logged_once_and_again_only_after_falling_back() {
        let mut cache = Cache::default();
        let first = note(&mut cache, usage(), now());
        assert_eq!(
            first,
            [
                "LeanCTX Cloud: 85 % of your storage is used (34 GiB of 40 GiB); Team gives every seat its own allowance",
                "LeanCTX Cloud: all of your monthly transfer is used (40 GiB of 40 GiB); nothing is charged for more; it frees at 2026-12-01T00:00:00Z; Team gives every seat its own allowance",
            ]
        );
        for line in &first {
            for word in ["workspace", "engine", "Queen"] {
                assert!(!line.contains(word), "{word} in {line}");
            }
        }
        // The next tick with the same states: nothing.
        assert!(note(&mut cache, usage(), now()).is_empty());
        // Storage warn → full: one line; transfer stays full: none.
        let mut raw = fixture();
        raw["quotas"]["storage_bytes"]["used"] = (40_u64 << 30).into();
        raw["quotas"]["storage_bytes"]["state"] = "full".into();
        let full = note(&mut cache, decode(raw.clone()).unwrap(), now());
        assert_eq!(
            full,
            [
                "LeanCTX Cloud: all of your storage is used (40 GiB of 40 GiB); nothing is charged for more; Team gives every seat its own allowance"
            ]
        );
        // Falling back says nothing; rising again says it again.
        raw["quotas"]["storage_bytes"]["state"] = "ok".into();
        raw["quotas"]["transfer_bytes"]["state"] = "ok".into();
        assert!(note(&mut cache, decode(raw).unwrap(), now()).is_empty());
        assert_eq!(note(&mut cache, usage(), now()).len(), 2);
    }

    #[test]
    fn model_credit_and_counts_keep_their_units_in_a_rise() {
        let credit = CloudQuotaV1 {
            limit: 8_000_000,
            used: 6_500_000,
            reset_at: Some("2026-12-01T00:00:00Z".into()),
            state: CloudQuotaStateV1::Warn,
            next_step: Some("top_up".into()),
        };
        assert_eq!(
            rise_line("model_credit_usd_micros", &credit),
            "LeanCTX Cloud: 81 % of your model credit is used ($6.50 of $8.00); it frees at 2026-12-01T00:00:00Z; a top-up in LeanCTX Cloud adds more"
        );
        let seats = CloudQuotaV1 {
            limit: 4,
            used: 4,
            reset_at: None,
            state: CloudQuotaStateV1::Full,
            next_step: Some("add_seats".into()),
        };
        assert_eq!(
            quota_line("connected_instances", &seats),
            "  connected instances   4 of 4 · FULL; every seat you add in LeanCTX Cloud brings its own allowance"
        );
    }

    #[test]
    fn the_cache_survives_a_restart_and_another_server_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = enrolled(dir.path(), &[FEATURE_USAGE]);
        let mut cache = load_for(&store, "https://cloud.example");
        assert!(!note(&mut cache, usage(), now()).is_empty());
        save(&store, &cache);
        let mut again = load_for(&store, "https://cloud.example");
        assert!(
            note(&mut again, usage(), now()).is_empty(),
            "no repeat after a restart"
        );
        let other = load_for(&store, "https://other.example");
        assert!(other.logged.is_empty() && other.usage.is_none());
    }

    #[test]
    fn the_background_look_is_spaced() {
        assert!(due(None, now()));
        assert!(!due(Some("2026-11-17T11:55:00Z"), now()));
        assert!(due(Some("2026-11-17T11:50:00Z"), now()));
        assert!(due(Some("garbage"), now()));
    }

    #[test]
    fn an_older_server_gets_no_request_and_no_status_lines() {
        let dir = tempfile::tempdir().unwrap();
        // Enrolled with a server that announced only `session_ref`: the
        // status neither asks nor prints (a request would fail: there is no
        // lease, so no session can be built at all).
        let store = enrolled(dir.path(), &["session_ref"]);
        assert!(!announced(&store));
        assert!(status(&store, &CloudConfig::default(), now()).is_empty());
        assert!(!store.data_dir().join(USAGE_FILE).exists());
        // Announced but not reachable: one line that says why, no cache.
        let store = enrolled(dir.path(), &[FEATURE_USAGE]);
        assert!(announced(&store));
        let lines = status(&store, &CloudConfig::default(), now());
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].starts_with("Usage:        not read now ("),
            "{lines:?}"
        );
    }
}
