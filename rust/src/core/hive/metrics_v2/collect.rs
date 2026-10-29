// SPDX-License-Identifier: Apache-2.0
//! The hourly counters of this engine, per Hive vault, from the stores the
//! local Cockpit reads (K15, K24: the same sources, so the numbers match).
//!
//! Joined to a vault the way the Cockpit's project switch joins a project
//! (`savings_ledger::ProjectScope`: the vault's checkouts, their worktrees
//! and the sessions that ran in them). Per field:
//!
//! | Field | Source (time) |
//! |---|---|
//! | `tokens.*`, `ledger.*` | signed savings ledger `savings/ledger.jsonl` (`ts`), by folder hash |
//! | `calls.tool_calls`, `tools[]`, `compression` | metering log `metering.jsonl` (`timestamp`), by project key; compression counts compressible tools only (`stats::classify_command`) |
//! | `guards.*` | audit trail (`timestamp`), security events by session |
//! | `policy_blocks[]` | audit trail denials (`tool_denied`, `path_jail_violation`, …) as `gateway.*` rule ids; the shell allowlist's refusals and warn-mode would-blocks ([`super::signals`]) |
//! | `refetch[]`, `unregulated[]` | [`super::signals`]: truncated outputs and their full re-fetches per filter; programs no rule covers |
//! | `sessions.started`, `hosts[].sessions` | session store (`started_at`) |
//! | `spend[]` | host session logs by minute (`host_usage::usage_between`), list price |
//! | `read_modes`, `calls.cache_*`, `hosts[].tool_calls` | the per-project stats stores and sessions, cumulative: the change between two samples ([`Snapshot`]) belongs to the hour of the later sample |
//!
//! Work no vault holds is kept apart (key `None`); only a Personal
//! workspace receives it, as the `vault: null` slice.
//!
//! Nothing here returns a string from a source: every name becomes a
//! closed-pattern wire name (`ToolName::or_other`, `RuleId`, `WireName`)
//! or is dropped.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::core::hive::metrics::collect::{Window, json_files_since, scan_window};
use crate::core::hive::vault::VaultMap;
use crate::core::savings_ledger::ProjectScope;
use crate::core::security_events::SecurityKind;
use lean_ctx_protocol::hive::metrics_v2::{
    BlockRowV2, CallsV2, CompressionV2, DetectorName, FilterName, GuardsV2, HiveHourV2, HostRowV2,
    HourStamp, LedgerV2, MAX_COUNT, MAX_FIRST_CONTEXT_MS, MAX_HOSTS, MAX_POLICY_BLOCKS,
    MAX_REFETCH, MAX_SECRET_HITS, MAX_SPEND, MAX_TASK_USAGE, MAX_TOOLS, MAX_UNREGULATED, ReadMode,
    RefetchRowV2, RuleId, SavingsV2, SecretRowV2, SessionsV2, SliceV2, SpendRowV2, TaskUsageV2,
    TokensV2, ToolName, ToolRowV2, UnregulatedKind, UnregulatedName, UnregulatedRowV2, VaultId,
    WireName,
};

/// Where the stores live.
pub(crate) struct Sources<'a> {
    pub(crate) data: &'a Path,
    pub(crate) home: Option<&'a Path>,
    /// `<data_dir>/hive` (secret fingerprints), when connected.
    pub(crate) hive: Option<&'a Path>,
}

/// A vault of the workspace as this machine knows it.
pub(crate) struct Vault {
    pub(crate) id: VaultId,
    roots: Vec<String>,
    hashes: HashSet<String>,
    sessions: HashSet<String>,
}

/// A session written in the window.
struct Session {
    id: String,
    vault: Option<usize>,
    host: WireName,
    started: DateTime<Utc>,
    tool_calls: u64,
}

/// The vaults and the sessions of the window.
pub(crate) struct Scope {
    pub(crate) vaults: Vec<Vault>,
    sessions: Vec<Session>,
}

/// `None`: work outside every vault.
pub(crate) type Key = Option<usize>;

impl Scope {
    /// The vaults Hive resolved for this machine and the sessions written
    /// since `since`.
    pub(crate) fn load(data: &Path, map: &VaultMap, since: DateTime<Utc>) -> Self {
        let raw = raw_sessions(data, since);
        let pairs: Vec<(&str, &str)> = raw
            .iter()
            .map(|s| (s.id.as_str(), s.root.as_str()))
            .collect();
        let mut vaults: Vec<Vault> = Vec::new();
        for record in map.vaults.values() {
            let Some(id) = VaultId::new(&record.vault_id) else {
                continue;
            };
            let index = vaults.iter().position(|v| v.id == id).unwrap_or_else(|| {
                vaults.push(Vault {
                    id,
                    roots: Vec::new(),
                    hashes: HashSet::new(),
                    sessions: HashSet::new(),
                });
                vaults.len() - 1
            });
            let vault = &mut vaults[index];
            for root in &record.roots {
                let path = Path::new(root);
                if !path.is_absolute() || crate::core::pathutil::is_broad_or_unsafe_root(path) {
                    continue;
                }
                let scope = ProjectScope::from_sessions(path, pairs.iter().copied());
                vault.hashes.extend(scope.repo_hashes());
                vault.sessions.extend(scope.sessions().iter().cloned());
                vault.roots.push(root.clone());
            }
        }
        let sessions = raw
            .into_iter()
            .map(|s| Session {
                vault: vaults.iter().position(|v| v.sessions.contains(&s.id)),
                // K2 (H2): a known client id or `other`, never what an MCP
                // client announced about itself.
                host: s
                    .client
                    .as_deref()
                    .and_then(WireName::new)
                    .unwrap_or_else(WireName::other),
                id: s.id,
                started: s.started,
                tool_calls: s.tool_calls,
            })
            .collect();
        Self { vaults, sessions }
    }

    fn by_hash(&self, hash: Option<&str>) -> Key {
        let hash = hash?;
        self.vaults.iter().position(|v| v.hashes.contains(hash))
    }

    fn by_session(&self, session: &str) -> Key {
        self.sessions
            .iter()
            .find(|s| s.id == session)
            .and_then(|s| s.vault)
            .or_else(|| {
                self.vaults
                    .iter()
                    .position(|v| v.sessions.contains(session))
            })
    }

    fn by_id(&self, id: &str) -> Option<usize> {
        self.vaults.iter().position(|v| v.id.as_str() == id)
    }

    /// The wire key of a vault index (`""` = outside every vault).
    pub(crate) fn wire_key(&self, key: Key) -> String {
        key.map(|i| self.vaults[i].id.as_str().to_string())
            .unwrap_or_default()
    }

    pub(crate) fn key_of(&self, wire: &str) -> Option<Key> {
        if wire.is_empty() {
            Some(None)
        } else {
            self.by_id(wire).map(Some)
        }
    }
}

struct RawSession {
    id: String,
    root: String,
    client: Option<String>,
    started: DateTime<Utc>,
    tool_calls: u64,
}

fn raw_sessions(data: &Path, since: DateTime<Utc>) -> Vec<RawSession> {
    let since = std::time::UNIX_EPOCH
        + std::time::Duration::from_secs(u64::try_from(since.timestamp()).unwrap_or(0));
    json_files_since(&data.join("sessions"), since)
        .into_iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            let session = crate::core::session::SessionState::from_storage_json(&text).ok()?;
            let root = session.project_root.clone().filter(|r| !r.is_empty())?;
            Some(RawSession {
                id: session.id.clone(),
                root,
                client: session.stats.client.clone(),
                started: session.started_at,
                tool_calls: u64::from(session.stats.total_tool_calls),
            })
        })
        .collect()
}

// ── accumulator ──────────────────────────────────────────────────────────────

/// One slice being summed. Keys are wire names already; no free text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Acc {
    pub(crate) tokens: TokensV2,
    pub(crate) savings: SavingsV2,
    pub(crate) calls: CallsV2,
    pub(crate) read_modes: BTreeMap<ReadMode, u64>,
    pub(crate) tools: BTreeMap<ToolName, (u64, u64)>,
    pub(crate) compression: CompressionV2,
    pub(crate) ledger: LedgerV2,
    pub(crate) guards: GuardsV2,
    /// rule → (blocked, would_block)
    pub(crate) blocks: BTreeMap<RuleId, (u64, u64)>,
    pub(crate) secrets: BTreeMap<DetectorName, u64>,
    /// K10: filter → (truncated, refetched)
    pub(crate) refetch: BTreeMap<FilterName, (u64, u64)>,
    /// K13: (kind, unregulated name) → calls
    pub(crate) unregulated: BTreeMap<(UnregulatedKind, UnregulatedName), u64>,
    /// X4: Hive facts used, first-context samples (ms), task usage by number.
    pub(crate) hive_hits: u64,
    pub(crate) first_context_ms: Vec<u64>,
    pub(crate) tasks: BTreeMap<u64, [u64; 3]>,
    pub(crate) started: u64,
    pub(crate) hosts: BTreeMap<WireName, (u64, u64)>,
    pub(crate) spend: BTreeMap<WireName, [u64; 5]>,
}

fn add(slot: &mut u64, n: u64) {
    *slot = slot.saturating_add(n).min(MAX_COUNT);
}

/// The provider a model name belongs to (host logs do not name it).
fn provider_of(model: &str) -> &'static str {
    let m = model.to_ascii_lowercase();
    if m.contains("claude") {
        "anthropic"
    } else if m.starts_with("gpt")
        || m.starts_with("o1")
        || m.starts_with("o3")
        || m.starts_with("o4")
        || m.contains("codex")
    {
        "openai"
    } else if m.contains("gemini") {
        "google"
    } else {
        "unknown"
    }
}

/// Rows beyond `max`: the largest `max - 1` stay, the rest merge into `other`.
fn bounded<K: Ord + Clone, V: Copy>(
    rows: &BTreeMap<K, V>,
    max: usize,
    weight: impl Fn(&V) -> u64,
    merge: impl Fn(&mut V, &V),
    other: K,
    zero: V,
) -> Vec<(K, V)> {
    let mut list: Vec<(K, V)> = rows.iter().map(|(k, v)| (k.clone(), *v)).collect();
    if list.len() <= max {
        return list;
    }
    list.sort_by(|a, b| weight(&b.1).cmp(&weight(&a.1)).then_with(|| a.0.cmp(&b.0)));
    let rest = list.split_off(max - 1);
    let mut merged = list
        .iter()
        .position(|(k, _)| *k == other)
        .map(|i| list.remove(i).1)
        .unwrap_or(zero);
    for (_, v) in &rest {
        merge(&mut merged, v);
    }
    list.push((other, merged));
    list.sort_by(|a, b| a.0.cmp(&b.0));
    list
}

impl Acc {
    pub(crate) fn merge(&mut self, other: &Acc) {
        add(&mut self.tokens.input, other.tokens.input);
        add(&mut self.tokens.output, other.tokens.output);
        add(&mut self.tokens.saved, other.tokens.saved);
        add(&mut self.calls.tool_calls, other.calls.tool_calls);
        add(&mut self.calls.cache_reads, other.calls.cache_reads);
        add(&mut self.calls.cache_hits, other.calls.cache_hits);
        for (k, v) in &other.read_modes {
            add(self.read_modes.entry(*k).or_default(), *v);
        }
        for (k, (c, s)) in &other.tools {
            let row = self.tools.entry(k.clone()).or_default();
            add(&mut row.0, *c);
            add(&mut row.1, *s);
        }
        add(&mut self.compression.original, other.compression.original);
        add(
            &mut self.compression.compressed,
            other.compression.compressed,
        );
        let (ledger, theirs) = (&mut self.ledger, &other.ledger);
        add(&mut ledger.reduction, theirs.reduction);
        add(&mut ledger.cache_discount, theirs.cache_discount);
        add(&mut ledger.correction, theirs.correction);
        add(&mut ledger.edit, theirs.edit);
        add(&mut ledger.other, theirs.other);
        let (guards, theirs) = (&mut self.guards, &other.guards);
        add(&mut guards.secrets_redacted, theirs.secrets_redacted);
        add(&mut guards.pii_redacted, theirs.pii_redacted);
        add(&mut guards.injection_flags, theirs.injection_flags);
        add(&mut guards.content_withheld, theirs.content_withheld);
        add(&mut guards.paths_blocked, theirs.paths_blocked);
        add(&mut guards.shell_blocked, theirs.shell_blocked);
        add(&mut guards.partial_inspections, theirs.partial_inspections);
        for (k, (b, w)) in &other.blocks {
            let row = self.blocks.entry(k.clone()).or_default();
            add(&mut row.0, *b);
            add(&mut row.1, *w);
        }
        for (k, v) in &other.secrets {
            add(self.secrets.entry(k.clone()).or_default(), *v);
        }
        for (k, (t, r)) in &other.refetch {
            let row = self.refetch.entry(k.clone()).or_default();
            add(&mut row.0, *t);
            add(&mut row.1, *r);
        }
        for (k, v) in &other.unregulated {
            add(self.unregulated.entry(k.clone()).or_default(), *v);
        }
        let (savings, theirs) = (&mut self.savings, &other.savings);
        add(&mut savings.measured, theirs.measured);
        add(&mut savings.estimated, theirs.estimated);
        add(&mut savings.estimated_low, theirs.estimated_low);
        add(&mut savings.estimated_high, theirs.estimated_high);
        add(&mut savings.evidenced, theirs.evidenced);
        add(&mut savings.overhead, theirs.overhead);
        add(&mut self.started, other.started);
        for (k, (s, c)) in &other.hosts {
            let row = self.hosts.entry(k.clone()).or_default();
            add(&mut row.0, *s);
            add(&mut row.1, *c);
        }
        for (k, v) in &other.spend {
            let row = self.spend.entry(k.clone()).or_default();
            for (slot, n) in row.iter_mut().zip(v) {
                add(slot, *n);
            }
        }
        add(&mut self.hive_hits, other.hive_hits);
        self.first_context_ms
            .extend_from_slice(&other.first_context_ms);
        for (k, v) in &other.tasks {
            let row = self.tasks.entry(*k).or_default();
            for (slot, n) in row.iter_mut().zip(v) {
                add(slot, *n);
            }
        }
    }

    /// The X4 block, when Hive helped in the hour.
    fn hive(&self) -> Option<HiveHourV2> {
        let task_usage: Vec<TaskUsageV2> = self
            .tasks
            .iter()
            .filter(|(number, v)| **number > 0 && v[0] + v[1] > 0)
            .take(MAX_TASK_USAGE)
            .map(|(number, v)| TaskUsageV2 {
                number: *number,
                input_tokens: v[0],
                output_tokens: v[1],
                list_usd_micros: v[2],
            })
            .collect();
        let mut samples = self.first_context_ms.clone();
        samples.sort_unstable();
        let p50 = (!samples.is_empty())
            .then(|| samples[(samples.len() - 1) / 2].min(MAX_FIRST_CONTEXT_MS));
        let active = self.hive_hits > 0 || !samples.is_empty() || !task_usage.is_empty();
        active.then_some(HiveHourV2 {
            knowledge_hits_referenced: self.hive_hits,
            first_context_sessions: samples.len() as u64,
            first_context_ms_p50: p50,
            task_usage,
        })
    }

    /// The wire slice, within every row limit (rest merged into `other`).
    pub(crate) fn to_slice(&self, vault: Option<VaultId>) -> SliceV2 {
        let tools = bounded(
            &self.tools,
            MAX_TOOLS,
            |v| v.0,
            |a, b| {
                add(&mut a.0, b.0);
                add(&mut a.1, b.1);
            },
            ToolName::other(),
            (0, 0),
        );
        let blocks = bounded(
            &self.blocks,
            MAX_POLICY_BLOCKS,
            |v| v.0 + v.1,
            |a, b| {
                add(&mut a.0, b.0);
                add(&mut a.1, b.1);
            },
            RuleId::other(),
            (0, 0),
        );
        let refetch = bounded(
            &self.refetch,
            MAX_REFETCH,
            |v| v.0 + v.1,
            |a, b| {
                add(&mut a.0, b.0);
                add(&mut a.1, b.1);
            },
            FilterName::or_other(lean_ctx_protocol::hive::metrics_v2::OTHER),
            (0, 0),
        );
        let unregulated = bounded(
            &self.unregulated,
            MAX_UNREGULATED,
            |v| *v,
            |a, b| add(a, *b),
            // lean-ctx: overflow folds into one row; the 32-row cap is far
            // above the kinds × known names an hour carries.
            (UnregulatedKind::Program, UnregulatedName::other()),
            0,
        );
        let secrets = bounded(
            &self.secrets,
            MAX_SECRET_HITS,
            |v| *v,
            |a, b| add(a, *b),
            DetectorName::or_other(lean_ctx_protocol::hive::metrics_v2::OTHER),
            0,
        );
        let hosts = bounded(
            &self.hosts,
            MAX_HOSTS,
            |v| v.1,
            |a, b| {
                add(&mut a.0, b.0);
                add(&mut a.1, b.1);
            },
            WireName::other(),
            (0, 0),
        );
        let spend = bounded(
            &self.spend,
            MAX_SPEND,
            |v| v[4],
            |a, b| {
                for (slot, n) in a.iter_mut().zip(b) {
                    add(slot, *n);
                }
            },
            WireName::other(),
            [0; 5],
        );
        let mut tokens = self.tokens;
        tokens.saved = tokens.saved.saturating_sub(self.ledger.correction);
        SliceV2 {
            vault,
            tokens,
            savings: self.savings,
            calls: self.calls,
            read_modes: self.read_modes.clone(),
            tools: tools
                .into_iter()
                .map(|(tool, (calls, saved))| ToolRowV2 { tool, calls, saved })
                .collect(),
            compression: self.compression,
            ledger: self.ledger,
            guards: self.guards,
            policy_blocks: blocks
                .into_iter()
                .map(|(rule, (blocked, would_block))| BlockRowV2 {
                    rule,
                    blocked,
                    would_block,
                })
                .collect(),
            refetch: refetch
                .into_iter()
                .map(|(filter, (truncated, refetched))| RefetchRowV2 {
                    filter,
                    truncated,
                    refetched,
                })
                .collect(),
            // K13: programs, host families and MCP servers no enforcing
            // rule covers ([`super::signals`]).
            unregulated: unregulated
                .into_iter()
                .map(|((kind, name), calls)| UnregulatedRowV2 { kind, name, calls })
                .collect(),
            secret_hits: secrets
                .into_iter()
                .map(|(detector, count)| SecretRowV2 { detector, count })
                .collect(),
            sessions: SessionsV2 {
                started: self.started,
            },
            hosts: hosts
                .into_iter()
                .map(|(host, (sessions, tool_calls))| HostRowV2 {
                    host,
                    sessions,
                    tool_calls,
                })
                .collect(),
            spend: spend
                .into_iter()
                .map(|(model, v)| SpendRowV2 {
                    provider: WireName::new(provider_of(model.as_str()))
                        .unwrap_or_else(WireName::other),
                    model,
                    requests: v[0],
                    input_tokens: v[1],
                    output_tokens: v[2],
                    cached_tokens: v[3],
                    usd_micros: v[4],
                })
                .collect(),
            hive: self.hive(),
            gauges: None,
        }
    }
}

// ── hour-exact sources ───────────────────────────────────────────────────────

pub(crate) type Hours = BTreeMap<(HourStamp, Key), Acc>;

fn hour_of(ts: &str) -> Option<(HourStamp, DateTime<Utc>)> {
    let t = DateTime::parse_from_rfc3339(ts).ok()?.with_timezone(&Utc);
    Some((HourStamp::of(t), t))
}

fn window_of(from: DateTime<Utc>, to: DateTime<Utc>) -> Window {
    Window {
        from: from.date_naive(),
        to: to.date_naive(),
    }
}

/// Counters with a time of their own, for the hours in `[from, to)`;
/// spend only for the hours starting at `spend_from` or later (the host
/// logs are read by minute, which costs one pass per hour and vault).
pub(crate) fn exact(
    sources: &Sources<'_>,
    scope: &Scope,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    spend_from: DateTime<Utc>,
) -> Hours {
    let mut hours = Hours::new();
    if from >= to {
        return hours;
    }
    let window = window_of(from, to);
    let inside = |t: DateTime<Utc>| t >= from && t < to;

    // Savings ledger: tokens and savings by kind (the Report and Ledger views).
    scan_window(
        &sources.data.join("savings").join("ledger.jsonl"),
        window,
        b"\"ts\":\"",
        |_, line| {
            let Ok(entry) =
                serde_json::from_slice::<crate::core::dashboard_pro::wire::LedgerEntry>(line)
            else {
                return;
            };
            let Some((hour, t)) = entry.ts.as_deref().and_then(hour_of) else {
                return;
            };
            if !inside(t) {
                return;
            }
            let key = scope.by_hash(entry.repo_hash.as_deref());
            let acc = hours.entry((hour, key)).or_default();
            let positive = |n: Option<i64>| u64::try_from(n.unwrap_or(0)).unwrap_or(0);
            let (baseline, actual, saved, bounce) = (
                positive(entry.baseline_tokens),
                positive(entry.actual_tokens),
                positive(entry.saved_tokens),
                positive(entry.bounce_adjustment),
            );
            match crate::core::dashboard_pro::ledger::ledger_kind(&entry) {
                0 => {
                    add(&mut acc.tokens.input, baseline);
                    add(&mut acc.tokens.output, actual);
                    add(&mut acc.tokens.saved, saved);
                    add(&mut acc.ledger.reduction, saved);
                    // K7: by trust label. Estimates carry a range from the
                    // entry's confidence (none recorded: ±50 %).
                    if entry.evidence_class.as_deref() == Some("measured") {
                        add(&mut acc.savings.measured, saved);
                    } else {
                        let confidence = entry
                            .confidence
                            .filter(|c| c.is_finite())
                            .map_or(0.5, |c| c.clamp(0.0, 1.0));
                        let spread = (saved as f64 * (1.0 - confidence)).round() as u64;
                        add(&mut acc.savings.estimated, saved);
                        add(&mut acc.savings.estimated_low, saved.saturating_sub(spread));
                        add(
                            &mut acc.savings.estimated_high,
                            saved.saturating_add(spread),
                        );
                    }
                    if entry.measurement_method.as_deref() == Some("provider_reconciled") {
                        add(&mut acc.savings.evidenced, saved);
                    }
                }
                1 => add(&mut acc.ledger.cache_discount, saved.max(baseline)),
                // Netted against `tokens.saved` in `to_slice`, as the
                // Cockpit's totals net corrections.
                2 => add(&mut acc.ledger.correction, bounce),
                3 => add(&mut acc.ledger.edit, saved),
                _ => add(&mut acc.ledger.other, saved),
            }
        },
    );

    // Metering log: every routed tool call, by project key.
    scan_window(
        &sources.data.join("metering.jsonl"),
        window,
        b"\"timestamp\":\"",
        |_, line| {
            let Ok(entry) = serde_json::from_slice::<crate::core::metering::MeterEntry>(line)
            else {
                return;
            };
            if !inside(entry.timestamp)
                || entry.tool_name == crate::core::metering::NATIVE_PASSTHROUGH_TOOL
            {
                return;
            }
            let key = scope.by_hash(entry.project.as_deref());
            let acc = hours
                .entry((HourStamp::of(entry.timestamp), key))
                .or_default();
            add(&mut acc.calls.tool_calls, 1);
            let tool = ToolName::or_other(&entry.tool_name);
            let row = acc.tools.entry(tool).or_default();
            add(&mut row.0, 1);
            add(&mut row.1, entry.savings_tokens);
            let command = crate::core::stats::normalize_command(&entry.tool_name);
            if crate::core::stats::classify_command(&command)
                == crate::core::stats::TrafficClass::Compressible
            {
                add(&mut acc.compression.original, entry.input_tokens);
                add(&mut acc.compression.compressed, entry.output_tokens);
            }
        },
    );

    // Audit trail: protection counters by session, denials by rule.
    #[derive(Deserialize)]
    struct AuditLine {
        #[serde(default)]
        timestamp: Option<String>,
        #[serde(default)]
        action: Option<String>,
        #[serde(default)]
        event_type: Option<String>,
    }
    let trail = sources.data.join("audit").join("trail.jsonl");
    for file in crate::core::audit_trail::trail_files_since(&trail, Some(from)) {
        scan_window(&file, window, b"\"timestamp\":\"", |_, line| {
            let Ok(entry) = serde_json::from_slice::<AuditLine>(line) else {
                return;
            };
            let Some((hour, t)) = entry.timestamp.as_deref().and_then(hour_of) else {
                return;
            };
            if !inside(t) {
                return;
            }
            if let Some((kind, n, session)) = entry
                .action
                .as_deref()
                .and_then(crate::core::security_events::parse_action)
            {
                let key = session.and_then(|s| scope.by_session(s));
                let g = &mut hours.entry((hour, key)).or_default().guards;
                let slot = match kind {
                    SecurityKind::SecretRedacted => &mut g.secrets_redacted,
                    SecurityKind::PiiRedacted => &mut g.pii_redacted,
                    SecurityKind::InjectionFlagged => &mut g.injection_flags,
                    SecurityKind::ContentWithheld => &mut g.content_withheld,
                    SecurityKind::PathBlocked => &mut g.paths_blocked,
                    SecurityKind::ShellBlocked => &mut g.shell_blocked,
                    SecurityKind::CoverageIncomplete => &mut g.partial_inspections,
                };
                add(slot, n);
                return;
            }
            if let Some(rule) = entry.event_type.as_deref().and_then(block_rule) {
                // Denials name no session: they belong to no one vault.
                let acc = hours.entry((hour, None)).or_default();
                add(&mut acc.blocks.entry(rule).or_default().0, 1);
            }
        });
    }

    // Redactions per detector the redaction path recorded (K21).
    if let Some(hive) = sources.hive {
        for (t, hit) in super::secrets::hits(hive) {
            if !inside(t) {
                continue;
            }
            let key = scope.by_hash(hit.project.as_deref());
            let acc = hours.entry((HourStamp::of(t), key)).or_default();
            add(acc.secrets.entry(hit.detector).or_default(), 1);
        }
        // K10, K12, K13 as recorded where they happen.
        for (t, signal) in super::signals::signals(hive) {
            if !inside(t) {
                continue;
            }
            let key = scope.by_hash(signal.project.as_deref());
            let acc = hours.entry((HourStamp::of(t), key)).or_default();
            use super::signals::Kind;
            match signal.kind {
                Kind::Truncated | Kind::Refetched => {
                    if let Some(filter) = signal.filter() {
                        let row = acc.refetch.entry(filter).or_default();
                        let slot = if signal.kind == Kind::Truncated {
                            &mut row.0
                        } else {
                            &mut row.1
                        };
                        add(slot, 1);
                    }
                }
                Kind::Blocked | Kind::WouldBlock => {
                    if let Some(rule) = signal.rule() {
                        let row = acc.blocks.entry(rule).or_default();
                        let slot = if signal.kind == Kind::Blocked {
                            &mut row.0
                        } else {
                            &mut row.1
                        };
                        add(slot, 1);
                    }
                }
                Kind::UnregulatedProgram | Kind::UnregulatedHost | Kind::UnregulatedMcpServer => {
                    if let Some(key) = signal.unregulated() {
                        add(acc.unregulated.entry(key).or_default(), 1);
                    }
                }
            }
        }
    }

    // Sessions started, per host; LeanCTX's own context (tool schemas,
    // instructions, rules block) rides each one at least once (K8).
    let overhead = crate::core::context_overhead::ContextOverhead::cached().total_tokens() as u64;
    for session in &scope.sessions {
        if !inside(session.started) {
            continue;
        }
        let acc = hours
            .entry((HourStamp::of(session.started), session.vault))
            .or_default();
        add(&mut acc.started, 1);
        add(&mut acc.savings.overhead, overhead);
        add(&mut acc.hosts.entry(session.host.clone()).or_default().0, 1);
    }

    // Spend: the hosts' own logs, by minute, at list price.
    if let Some(home) = sources.home {
        let pricing = crate::core::gain::model_pricing::ModelPricing::load();
        let now = Utc::now().max(to);
        let mut hour = HourStamp::of(spend_from.max(from)).start();
        while hour < to {
            for (index, vault) in scope.vaults.iter().enumerate() {
                if vault.roots.is_empty() {
                    continue;
                }
                let others: Vec<String> = scope
                    .vaults
                    .iter()
                    .enumerate()
                    .filter(|(other, _)| *other != index)
                    .flat_map(|(_, v)| v.roots.iter().cloned())
                    .collect();
                if let Some(hive_data) = sources.hive {
                    let usage = crate::core::hive::metrics::hive_use::host_window(
                        home,
                        &vault.roots,
                        &others,
                        now,
                    );
                    let tasks = crate::core::hive::metrics::hive_use::window_tasks(
                        hive_data,
                        vault.id.as_str(),
                        hour,
                        hour + Duration::hours(1),
                        now,
                        &usage,
                    );
                    if !tasks.is_empty() {
                        let acc = hours.entry((HourStamp::of(hour), Some(index))).or_default();
                        for task in tasks {
                            let row = acc.tasks.entry(task.number).or_default();
                            add(&mut row[0], task.input_tokens);
                            add(&mut row[1], task.output_tokens);
                            add(&mut row[2], task.list_usd_micros);
                        }
                    }
                }
                let models = crate::core::dashboard_pro::host_usage::usage_between(
                    home,
                    &vault.roots,
                    &others,
                    now,
                    hour,
                    hour + Duration::minutes(59),
                );
                for (model, counts) in models {
                    // K2 (H3): a model of the price table or `other`; a
                    // fine-tune or org-named model id never leaves.
                    let name = WireName::new(&model)
                        .or_else(|| {
                            let quote = pricing.quote(Some(model.as_str()));
                            matches!(
                                quote.match_kind,
                                crate::core::gain::model_pricing::PricingMatchKind::Exact
                                    | crate::core::gain::model_pricing::PricingMatchKind::Alias
                            )
                            .then(|| WireName::new(&quote.model_key))
                            .flatten()
                        })
                        .unwrap_or_else(WireName::other);
                    let positive = |n: i64| u64::try_from(n).unwrap_or(0);
                    let (input, write, read, output) = (
                        positive(counts.input),
                        positive(counts.cache_write),
                        positive(counts.cache_read),
                        positive(counts.output),
                    );
                    if positive(counts.requests) == 0 && input + write + read + output == 0 {
                        continue;
                    }
                    let usd = pricing
                        .quote(Some(model.as_str()))
                        .cost
                        .estimate_usd(input, output, write, read);
                    let micros = if usd.is_finite() && usd > 0.0 {
                        (usd * 1e6).round().min(MAX_COUNT as f64) as u64
                    } else {
                        0
                    };
                    let acc = hours.entry((HourStamp::of(hour), Some(index))).or_default();
                    let row = acc.spend.entry(name).or_default();
                    for (slot, n) in row.iter_mut().zip([
                        positive(counts.requests),
                        input + write,
                        output,
                        read,
                        micros,
                    ]) {
                        add(slot, n);
                    }
                }
            }
            hour += Duration::hours(1);
        }
    }
    hours
}

/// The rule id of an audit denial: gateway rule ids, never free text.
fn block_rule(event_type: &str) -> Option<RuleId> {
    let id = match event_type {
        "tool_denied" => "gateway.tool_denied",
        "path_jail_violation" => "gateway.path_jail",
        "budget_exceeded" => "gateway.budget_exceeded",
        "cross_project_access" => "gateway.cross_project",
        "rate_limited" => "gateway.rate_limited",
        _ => return None,
    };
    RuleId::new(id)
}

// ── cumulative sources ───────────────────────────────────────────────────────

/// CEP counters of one key (vault id, `""` = outside every vault).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Cep {
    pub(crate) hits: u64,
    pub(crate) reads: u64,
    pub(crate) modes: BTreeMap<ReadMode, u64>,
}

/// A session's tool calls at the sample.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SessionMark {
    pub(crate) vault: String,
    pub(crate) host: WireName,
    pub(crate) calls: u64,
}

/// The cumulative counters at one sample. Local state only, never sent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Snapshot {
    pub(crate) cep: BTreeMap<String, Cep>,
    pub(crate) sessions: BTreeMap<String, SessionMark>,
    /// X4 per vault: the day, facts used so far that day, its first-context
    /// samples so far.
    #[serde(default)]
    pub(crate) hive: BTreeMap<String, (String, u64, Vec<u64>)>,
}

/// The part of an hour that comes from cumulative counters: per wire key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Sampled {
    pub(crate) cache_hits: u64,
    pub(crate) cache_reads: u64,
    pub(crate) read_modes: BTreeMap<ReadMode, u64>,
    pub(crate) host_calls: BTreeMap<WireName, u64>,
    #[serde(default)]
    pub(crate) hive_hits: u64,
    #[serde(default)]
    pub(crate) first_context_ms: Vec<u64>,
}

impl Sampled {
    pub(crate) fn into_acc(self) -> Acc {
        let mut acc = Acc::default();
        acc.calls.cache_hits = self.cache_hits;
        acc.calls.cache_reads = self.cache_reads;
        acc.read_modes = self.read_modes;
        for (host, calls) in self.host_calls {
            acc.hosts.entry(host).or_default().1 = calls;
        }
        acc.hive_hits = self.hive_hits;
        acc.first_context_ms = self.first_context_ms;
        acc
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.cache_hits == 0
            && self.cache_reads == 0
            && self.read_modes.values().all(|n| *n == 0)
            && self.host_calls.values().all(|n| *n == 0)
            && self.hive_hits == 0
            && self.first_context_ms.is_empty()
    }
}

fn stats_at(path: &Path) -> Option<crate::core::stats::StatsStore> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn cep_of(store: &crate::core::stats::StatsStore) -> Cep {
    let mut modes = BTreeMap::new();
    for (mode, n) in &store.cep.modes {
        add(modes.entry(ReadMode::of(mode)).or_default(), *n);
    }
    Cep {
        hits: store.cep.total_cache_hits,
        reads: store.cep.total_cache_reads,
        modes,
    }
}

fn cep_sum(into: &mut Cep, o: &Cep) {
    add(&mut into.hits, o.hits);
    add(&mut into.reads, o.reads);
    for (k, v) in &o.modes {
        add(into.modes.entry(*k).or_default(), *v);
    }
}

fn cep_minus(a: &Cep, b: &Cep) -> Cep {
    Cep {
        hits: a.hits.saturating_sub(b.hits),
        reads: a.reads.saturating_sub(b.reads),
        modes: a
            .modes
            .iter()
            .map(|(k, v)| (*k, v.saturating_sub(b.modes.get(k).copied().unwrap_or(0))))
            .collect(),
    }
}

fn project_stats(data: &Path, key: &str) -> PathBuf {
    data.join("project-stats").join(key).join("stats.json")
}

/// The cumulative counters now.
pub(crate) fn snapshot(sources: &Sources<'_>, scope: &Scope) -> Snapshot {
    let mut cep = BTreeMap::new();
    let mut in_vaults = Cep::default();
    for vault in &scope.vaults {
        let mut sum = Cep::default();
        for hash in &vault.hashes {
            if let Some(store) = stats_at(&project_stats(sources.data, hash)) {
                cep_sum(&mut sum, &cep_of(&store));
            }
        }
        cep_sum(&mut in_vaults, &sum);
        cep.insert(vault.id.as_str().to_string(), sum);
    }
    let device = stats_at(&sources.data.join("stats.json"))
        .map(|s| cep_of(&s))
        .unwrap_or_default();
    cep.insert(String::new(), cep_minus(&device, &in_vaults));
    let sessions = scope
        .sessions
        .iter()
        .map(|s| {
            (
                s.id.clone(),
                SessionMark {
                    vault: scope.wire_key(s.vault),
                    host: s.host.clone(),
                    calls: s.tool_calls,
                },
            )
        })
        .collect();
    let mut hive = BTreeMap::new();
    if let Some(hive_data) = sources.hive {
        let today = Utc::now().format("%Y-%m-%d").to_string();
        for vault in &scope.vaults {
            let (hits, samples) = crate::core::hive::metrics::hive_use::day_counters(
                hive_data,
                vault.id.as_str(),
                &today,
            );
            hive.insert(
                vault.id.as_str().to_string(),
                (today.clone(), hits, samples),
            );
        }
    }
    Snapshot {
        cep,
        sessions,
        hive,
    }
}

/// What changed from `before` to `after`, per wire key. A key or session
/// missing from `before` started from zero.
pub(crate) fn sampled(before: &Snapshot, after: &Snapshot) -> BTreeMap<String, Sampled> {
    let mut out: BTreeMap<String, Sampled> = BTreeMap::new();
    let zero = Cep::default();
    for (key, now) in &after.cep {
        let d = cep_minus(now, before.cep.get(key).unwrap_or(&zero));
        let row = out.entry(key.clone()).or_default();
        add(&mut row.cache_hits, d.hits);
        add(&mut row.cache_reads, d.reads);
        for (k, v) in d.modes.into_iter().filter(|(_, v)| *v > 0) {
            add(row.read_modes.entry(k).or_default(), v);
        }
    }
    for (id, now) in &after.sessions {
        let before = before.sessions.get(id).map_or(0, |m| m.calls);
        let d = now.calls.saturating_sub(before);
        if d > 0 {
            let row = out.entry(now.vault.clone()).or_default();
            add(row.host_calls.entry(now.host.clone()).or_default(), d);
        }
    }
    for (vault, (day, hits, samples)) in &after.hive {
        let (hits_before, seen) = match before.hive.get(vault) {
            Some((d, h, s)) if d == day => (*h, s.len()),
            _ => (0, 0),
        };
        let row = out.entry(vault.clone()).or_default();
        add(&mut row.hive_hits, hits.saturating_sub(hits_before));
        row.first_context_ms
            .extend(samples.iter().skip(seen).copied());
    }
    out.retain(|_, s| !s.is_empty());
    out
}

/// The slices of one hour: exact counters plus the sampled part, only for
/// vaults allowed (`vaults_off` left out; outside every vault only when
/// `unattributed`), only slices with activity.
pub(crate) fn slices(
    scope: &Scope,
    hour: &HourStamp,
    exact: &Hours,
    sampled: &BTreeMap<String, Sampled>,
    vaults_off: &std::collections::BTreeSet<String>,
    unattributed: bool,
) -> Vec<SliceV2> {
    let mut by_key: HashMap<Key, Acc> = HashMap::new();
    for ((h, key), acc) in exact.range((hour.clone(), None)..) {
        if h != hour {
            break;
        }
        by_key.entry(*key).or_default().merge(acc);
    }
    for (wire, part) in sampled {
        if let Some(key) = scope.key_of(wire) {
            by_key
                .entry(key)
                .or_default()
                .merge(&part.clone().into_acc());
        }
    }
    let mut out: Vec<SliceV2> = by_key
        .into_iter()
        .filter_map(|(key, acc)| {
            let vault = match key {
                Some(index) => {
                    let id = scope.vaults[index].id.clone();
                    if vaults_off.contains(id.as_str()) {
                        return None;
                    }
                    Some(id)
                }
                None if unattributed => None,
                None => return None,
            };
            let slice = acc.to_slice(vault);
            slice.has_activity().then_some(slice)
        })
        .collect();
    out.sort_by(|a, b| a.vault.cmp(&b.vault));
    // lean-ctx: 32 vaults + one null per hour (contract); more active vaults
    // in one hour keep the first 33 by id, upgrade when Hive raises the cap.
    out.truncate(lean_ctx_protocol::hive::metrics_v2::MAX_SLICES);
    out
}
