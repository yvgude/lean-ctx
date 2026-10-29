// SPDX-License-Identifier: Apache-2.0
//! Per project and UTC day, the totals the local Cockpit computes from this
//! device's own stores (`cloud-cockpit-metrics-v1` §1). Numbers only: no
//! path, file name, command, fact or content leaves this module; a project
//! is its Hive `vault_id` and, when allowed, its folder's last segment.
//! A root the caller cannot name a project for is not reported.
//!
//! Projects are the roots lean-ctx sessions ran in. Each source is joined to
//! a project the way the Cockpit joins it:
//!
//! - savings and ledger kinds: the signed savings ledger, by the folder hash
//!   each entry records (`dashboard_pro::ledger`, the Report and Ledger views);
//! - tool calls, cache hits, agents: the sessions' own per-day counters;
//! - guards and receipts: the signed audit trail, by the session each entry
//!   names (`security_events::record`, the receipt anchors);
//! - spend: the hosts' own session logs under the project
//!   (`dashboard_pro::host_usage`, the Spend view), priced at list price;
//! - memory: the project's knowledge facts and the errors it linked to a fix.
//!
//! Entries a source does not tie to a project (the proxy's prompt-cache
//! discounts, CLI-only events) are not reported per project.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;

use super::payload::{self, Agent, Day, Guards, Ledger, Memory, Savings, Spend, Vault};
use crate::core::security_events::SecurityKind;

/// The UTC days an upload covers, both ends included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Window {
    pub(crate) from: NaiveDate,
    pub(crate) to: NaiveDate,
}

impl Window {
    /// `days` days ending with `today`.
    pub(crate) fn ending(today: NaiveDate, days: u32) -> Self {
        Self {
            from: today - chrono::Duration::days(i64::from(days.max(1)) - 1),
            to: today,
        }
    }

    pub(crate) fn len_days(self) -> i64 {
        (self.to - self.from).num_days() + 1
    }

    fn contains(self, day: &str) -> bool {
        NaiveDate::parse_from_str(day, "%Y-%m-%d").is_ok_and(|d| d >= self.from && d <= self.to)
    }

    fn first_day_text(self) -> String {
        self.from.format("%Y-%m-%d").to_string()
    }

    fn start_time(self) -> std::time::SystemTime {
        let start = self
            .from
            .and_hms_opt(0, 0, 0)
            .map_or(0, |t| t.and_utc().timestamp());
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(u64::try_from(start).unwrap_or(0))
    }
}

/// Where the stores live.
pub(crate) struct Sources<'a> {
    pub(crate) data: &'a Path,
    pub(crate) home: Option<&'a Path>,
}

/// One project of this device: every root of it and their ledger hashes.
struct ProjectRef {
    id: String,
    label: Option<String>,
    roots: Vec<String>,
}

/// A session's counters as the per-day view needs them.
struct SessionCounts {
    root: String,
    client: Option<String>,
    daily: BTreeMap<String, crate::core::session::SessionDayCounts>,
}

/// The project id of a root, or `None` when the root is not reported.
pub(crate) type ProjectIds<'a> = dyn Fn(&Path) -> Option<String> + 'a;

/// Every day entry with activity in `window`, ordered by date then project.
pub(crate) fn collect(
    sources: &Sources<'_>,
    window: Window,
    ids: &ProjectIds<'_>,
    labels: bool,
) -> Vec<Day> {
    let (session_roots, sessions) = sessions(sources.data, window);
    let projects = projects(session_roots.values().cloned(), ids, labels);
    if projects.is_empty() {
        return Vec::new();
    }
    let mut root_project: HashMap<String, usize> = HashMap::new();
    for (index, project) in projects.iter().enumerate() {
        for root in &project.roots {
            root_project.insert(root.clone(), index);
        }
    }
    let session_project: HashMap<&str, usize> = session_roots
        .iter()
        .filter_map(|(id, root)| Some((id.as_str(), *root_project.get(root)?)))
        .collect();

    let mut days: BTreeMap<(String, usize), Day> = BTreeMap::new();
    for (date, project, savings, ledger) in ledger_days(sources.data, window, &projects) {
        let day = day_mut(&mut days, &projects, &date, project);
        day.savings.tokens_requested = savings.tokens_requested;
        day.savings.tokens_delivered = savings.tokens_delivered;
        day.savings.tokens_saved = savings.tokens_saved;
        day.ledger = ledger;
    }

    let mut agents: BTreeMap<(String, usize), BTreeMap<String, Agent>> = BTreeMap::new();
    for session in &sessions {
        let Some(&project) = root_project.get(&session.root) else {
            continue;
        };
        let host = host_name(session.client.as_deref());
        for (date, counts) in &session.daily {
            if !window.contains(date) || (counts.tool_calls == 0 && counts.cache_hits == 0) {
                continue;
            }
            let day = day_mut(&mut days, &projects, date, project);
            day.savings.tool_calls = payload::count(day.savings.tool_calls + counts.tool_calls);
            day.savings.cache_hits = payload::count(day.savings.cache_hits + counts.cache_hits);
            let row = agents
                .entry((date.clone(), project))
                .or_default()
                .entry(host.clone())
                .or_insert_with(|| Agent {
                    host: host.clone(),
                    sessions: 0,
                    tool_calls: 0,
                });
            row.sessions += 1;
            row.tool_calls = payload::count(row.tool_calls + counts.tool_calls);
        }
    }
    for ((date, project), rows) in agents {
        day_mut(&mut days, &projects, &date, project).agents =
            payload::bound_agents(rows.into_values().collect());
    }

    for (date, project, guards, receipts) in audit_days(sources.data, window, &session_project) {
        let day = day_mut(&mut days, &projects, &date, project);
        day.guards = guards;
        day.receipts = receipts;
    }

    if let Some(home) = sources.home {
        for ((date, project), rows) in spend_days(home, window, &projects) {
            day_mut(&mut days, &projects, &date, project).spend = payload::bound_spend(rows);
        }
    }

    for (index, project) in projects.iter().enumerate() {
        let memory = memory_days(project, window);
        for (date, (added, lessons)) in &memory.flows {
            let day = day_mut(&mut days, &projects, date, index);
            day.memory.facts_added = *added;
            day.memory.lessons_linked = *lessons;
        }
        for ((date, project_index), day) in &mut days {
            if *project_index == index {
                day.memory.facts_current = memory.current_on(date);
            }
        }
    }

    // X4: how Hive helped, from this machine's own Hive-use record.
    let hive_data = sources.data.join("hive");
    let now = window
        .to
        .and_hms_opt(23, 59, 59)
        .map_or_else(Utc::now, |end| end.and_utc().min(Utc::now()));
    for (index, project) in projects.iter().enumerate() {
        let others: Vec<String> = projects
            .iter()
            .enumerate()
            .filter(|(other, _)| *other != index)
            .flat_map(|(_, other)| other.roots.iter().cloned())
            .collect();
        let usage = |from, to| match sources.home {
            Some(home) => {
                super::hive_use::host_window(home, &project.roots, &others, now)(from, to)
            }
            None => Vec::new(),
        };
        let mut date = window.from;
        while date <= window.to {
            let text = date.format("%Y-%m-%d").to_string();
            if let Some(entry) =
                super::hive_use::day_entry(&hive_data, &project.id, &text, now, &usage)
            {
                day_mut(&mut days, &projects, &text, index).hive = Some(entry);
            }
            date += chrono::Duration::days(1);
        }
    }

    days.into_values().filter(Day::has_activity).collect()
}

fn day_mut<'a>(
    days: &'a mut BTreeMap<(String, usize), Day>,
    projects: &[ProjectRef],
    date: &str,
    project: usize,
) -> &'a mut Day {
    days.entry((date.to_string(), project))
        .or_insert_with(|| empty_day(date, &projects[project]))
}

fn empty_day(date: &str, project: &ProjectRef) -> Day {
    Day {
        date: date.to_string(),
        vault: Vault {
            id: project.id.clone(),
            label: project.label.clone(),
        },
        savings: Savings::default(),
        spend: Vec::new(),
        ledger: Ledger::default(),
        guards: Guards::default(),
        agents: Vec::new(),
        memory: Memory::default(),
        receipts: 0,
        hive: None,
    }
}

/// The `host` of a session: its client's family (`claude`, `codex`, …), the
/// sanitized client id for an unrecognised client, `unknown` without one.
pub(crate) fn host_name(client: Option<&str>) -> String {
    let Some(client) = client else {
        return "unknown".into();
    };
    crate::core::telemetry_v2::ClientFamily::from_client_id(client)
        .and_then(|family| serde_json::to_value(family).ok())
        .and_then(|value| value.as_str().map(str::to_string))
        .or_else(|| payload::name(client))
        .unwrap_or_else(|| "unknown".into())
}

pub(crate) fn json_files_since(dir: &Path, since: std::time::SystemTime) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|e| {
            e.metadata()
                .is_ok_and(|m| m.file_type().is_file() && m.modified().is_ok_and(|t| t >= since))
        })
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter(|p| p.file_name().is_some_and(|n| n != "latest.json"))
        .collect()
}

/// Sessions written during `window`: every session id's project root (from
/// the session store and the value snapshots, so audit entries of a session
/// whose store was pruned still find their project) and the stored sessions'
/// per-day counters.
fn sessions(data: &Path, window: Window) -> (HashMap<String, String>, Vec<SessionCounts>) {
    let since = window.start_time();
    let mut roots = HashMap::new();
    let mut counts = Vec::new();
    for path in json_files_since(&data.join("sessions"), since) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(session) = crate::core::session::SessionState::from_storage_json(&text) else {
            continue;
        };
        let Some(root) = session.project_root.clone().filter(|r| !r.is_empty()) else {
            continue;
        };
        roots.insert(session.id.clone(), root.clone());
        let mut daily = session.stats.daily.clone();
        // Sessions written before per-day counters existed: their totals
        // belong to one day only when the session never crossed midnight.
        if daily.is_empty()
            && session.stats.total_tool_calls > 0
            && session.started_at.date_naive() == session.updated_at.date_naive()
        {
            daily.insert(
                session.updated_at.format("%Y-%m-%d").to_string(),
                crate::core::session::SessionDayCounts {
                    tool_calls: u64::from(session.stats.total_tool_calls),
                    cache_hits: u64::from(session.stats.cache_hits),
                },
            );
        }
        counts.push(SessionCounts {
            root,
            client: session.stats.client.clone(),
            daily,
        });
    }
    for path in json_files_since(&data.join("value").join("sessions"), since) {
        if let Some(snapshot) = crate::core::value::snapshot::read_path(&path)
            && let Some(root) = snapshot.project_root.filter(|r| !r.is_empty())
            && !snapshot.session_id.is_empty()
        {
            roots.entry(snapshot.session_id).or_insert(root);
        }
    }
    (roots, counts)
}

/// The device's projects: real project folders (never a home or system
/// directory), grouped by their project id so two checkouts of one
/// repository are one project.
fn projects(
    roots: impl Iterator<Item = String>,
    ids: &ProjectIds<'_>,
    _labels: bool,
) -> Vec<ProjectRef> {
    let unique: BTreeSet<String> = roots
        .filter(|root| {
            let path = Path::new(root);
            path.is_absolute()
                && path.is_dir()
                && !crate::core::pathutil::is_broad_or_unsafe_root(path)
        })
        .collect();
    let mut by_id: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for root in unique {
        if let Some(id) = ids(Path::new(&root)) {
            by_id.entry(id).or_default().push(root);
        }
    }
    by_id
        .into_iter()
        .map(|(id, roots)| ProjectRef {
            // K2 review (C1): never a folder name, whatever `labels` says.
            label: None,
            id,
            roots,
        })
        .collect()
}

// ── time-ordered JSONL ───────────────────────────────────────────────────────

/// The `YYYY-MM-DD` after the first `needle` (`"ts":"`) of a line.
fn day_after<'a>(line: &'a [u8], needle: &[u8]) -> Option<&'a str> {
    let at = line.windows(needle.len()).position(|w| w == needle)? + needle.len();
    std::str::from_utf8(line.get(at..at + 10)?).ok()
}

/// Calls `visit` for every complete line of the time-ordered append-only
/// `path` dated `window.from` or later, without reading the older part: a
/// binary search over byte offsets finds where the window starts, so the
/// audit trail (often gigabytes) costs only the window's own lines.
pub(crate) fn scan_window(
    path: &Path,
    window: Window,
    needle: &[u8],
    mut visit: impl FnMut(&str, &[u8]),
) {
    const PROBE: u64 = 64 * 1024;
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    if !meta.file_type().is_file() {
        return;
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return;
    };
    let len = meta.len();
    let from = window.first_day_text();
    let first_day_after = |file: &mut std::fs::File, offset: u64| -> Option<String> {
        file.seek(SeekFrom::Start(offset)).ok()?;
        let mut probe = Vec::new();
        file.by_ref().take(PROBE).read_to_end(&mut probe).ok()?;
        let mut lines = probe.split(|b| *b == b'\n');
        if offset > 0 {
            lines.next();
        }
        lines.find_map(|line| day_after(line, needle).map(str::to_string))
    };
    let (mut low, mut high) = (0u64, len);
    while high - low > PROBE {
        let mid = low + (high - low) / 2;
        match first_day_after(&mut file, mid) {
            Some(day) if day.as_str() >= from.as_str() => high = mid,
            _ => low = mid,
        }
    }
    if file.seek(SeekFrom::Start(low)).is_err() {
        return;
    }
    let mut reader = BufReader::new(file.take(len - low));
    let mut line = Vec::new();
    if low > 0 {
        let _ = reader.read_until(b'\n', &mut line);
    }
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) if line.last() != Some(&b'\n') => break,
            Ok(_) => {}
        }
        if let Some(day) = day_after(&line, needle)
            && window.contains(day)
        {
            let day = day.to_string();
            visit(&day, &line);
        }
    }
}

// ── savings ledger ───────────────────────────────────────────────────────────

/// Per (day, project): the ledger's measured savings and its entries by kind,
/// joined by the folder hash each entry records — the Cockpit's own join.
fn ledger_days(
    data: &Path,
    window: Window,
    projects: &[ProjectRef],
) -> Vec<(String, usize, Savings, Ledger)> {
    use crate::core::dashboard_pro::ledger::ledger_kind;
    let mut by_hash: HashMap<String, usize> = HashMap::new();
    for (index, project) in projects.iter().enumerate() {
        for root in &project.roots {
            by_hash.insert(crate::core::dashboard_pro::repo_hash(root), index);
            // The canonical spelling the ledger's project scope hashes
            // (`savings_ledger::project_scope`): no Windows `\\?\` prefix.
            if let Ok(canonical) = crate::core::pathutil::safe_canonicalize(Path::new(root)) {
                let canonical = canonical.to_string_lossy();
                by_hash.insert(crate::core::dashboard_pro::repo_hash(&canonical), index);
            }
        }
    }
    // (saved, corrected) are netted at the end, as the Cockpit's day totals do.
    let mut totals: BTreeMap<(String, usize), (Savings, Ledger, u64, u64)> = BTreeMap::new();
    scan_window(
        &data.join("savings").join("ledger.jsonl"),
        window,
        b"\"ts\":\"",
        |day, line| {
            let Ok(entry) =
                serde_json::from_slice::<crate::core::dashboard_pro::wire::LedgerEntry>(line)
            else {
                return;
            };
            let Some(&project) = entry.repo_hash.as_deref().and_then(|h| by_hash.get(h)) else {
                return;
            };
            let positive = |n: Option<i64>| u64::try_from(n.unwrap_or(0)).unwrap_or(0);
            let row = totals.entry((day.to_string(), project)).or_default();
            match ledger_kind(&entry) {
                0 => {
                    row.0.tokens_requested += positive(entry.baseline_tokens);
                    row.0.tokens_delivered += positive(entry.actual_tokens);
                    row.2 += positive(entry.saved_tokens);
                    row.1.reduction += 1;
                }
                1 => row.1.cache_discount += 1,
                2 => {
                    row.3 += positive(entry.bounce_adjustment);
                    row.1.correction += 1;
                }
                3 => row.1.edit += 1,
                _ => row.1.other += 1,
            }
        },
    );
    totals
        .into_iter()
        .map(
            |((day, project), (mut savings, ledger, saved, corrected))| {
                savings.tokens_requested = payload::count(savings.tokens_requested);
                savings.tokens_delivered = payload::count(savings.tokens_delivered);
                savings.tokens_saved = payload::count(saved.saturating_sub(corrected));
                (day, project, savings, bounded_ledger(ledger))
            },
        )
        .collect()
}

fn bounded_ledger(l: Ledger) -> Ledger {
    Ledger {
        reduction: payload::count(l.reduction),
        edit: payload::count(l.edit),
        correction: payload::count(l.correction),
        cache_discount: payload::count(l.cache_discount),
        other: payload::count(l.other),
    }
}

// ── audit trail ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct AuditLine {
    #[serde(default)]
    action: Option<String>,
}

/// Per (day, project): guard events and Context Gateway receipts, from the
/// signed audit trail. Each entry names its session; the session names its
/// project. Entries without a session (CLI runs, the proxy) are not counted.
fn audit_days(
    data: &Path,
    window: Window,
    session_project: &HashMap<&str, usize>,
) -> Vec<(String, usize, Guards, u64)> {
    let mut guards: BTreeMap<(String, usize), Guards> = BTreeMap::new();
    let mut receipts: BTreeMap<(String, usize), HashSet<String>> = BTreeMap::new();
    let mut visit = |day: &str, line: &[u8]| {
        if !line.windows(10).any(|w| w == b"\"action\":\"") {
            return;
        }
        let Ok(entry) = serde_json::from_slice::<AuditLine>(line) else {
            return;
        };
        let Some(action) = entry.action else { return };
        if let Some(rest) = action.strip_prefix("gateway_receipt:") {
            let Some((head, session)) = rest.rsplit_once("|session=") else {
                return;
            };
            let Some(&project) = session_project.get(session) else {
                return;
            };
            let digest = head.split('|').next().unwrap_or(head).to_string();
            receipts
                .entry((day.to_string(), project))
                .or_default()
                .insert(digest);
            return;
        }
        let Some((kind, n, Some(session))) = crate::core::security_events::parse_action(&action)
        else {
            return;
        };
        let Some(&project) = session_project.get(session) else {
            return;
        };
        let row = guards.entry((day.to_string(), project)).or_default();
        let slot = match kind {
            SecurityKind::SecretRedacted => &mut row.secrets_redacted,
            SecurityKind::PiiRedacted => &mut row.pii_redacted,
            SecurityKind::InjectionFlagged => &mut row.injection_flags,
            SecurityKind::ContentWithheld => &mut row.content_withheld,
            SecurityKind::PathBlocked => &mut row.paths_blocked,
            SecurityKind::ShellBlocked => &mut row.shell_blocked,
            SecurityKind::CoverageIncomplete => &mut row.partial_inspections,
        };
        *slot = payload::count(slot.saturating_add(n));
    };
    // Sealed segments oldest first, then the active trail; a file moved by a
    // seal in between reads as empty.
    let trail = data.join("audit").join("trail.jsonl");
    let since = window.from.and_hms_opt(0, 0, 0).map(|t| t.and_utc());
    for file in crate::core::audit_trail::trail_files_since(&trail, since) {
        scan_window(&file, window, b"\"timestamp\":\"", &mut visit);
    }
    let keys: BTreeSet<(String, usize)> = guards.keys().chain(receipts.keys()).cloned().collect();
    keys.into_iter()
        .map(|key| {
            let g = guards.get(&key).copied().unwrap_or_default();
            let r = receipts.get(&key).map_or(0, |set| set.len() as u64);
            (key.0, key.1, g, payload::count(r))
        })
        .collect()
}

// ── spend ────────────────────────────────────────────────────────────────────

/// The provider a host's own log describes: Claude Code calls Anthropic,
/// Codex calls OpenAI.
fn provider_of(host: &str) -> &'static str {
    match host {
        "claude-code" => "anthropic",
        "codex" => "openai",
        _ => "unknown",
    }
}

/// Per (day, project): requests, tokens and list price per model, from the
/// hosts' session logs whose folder lies in the project (the Cockpit's Spend
/// view reads the same logs).
fn spend_days(
    home: &Path,
    window: Window,
    projects: &[ProjectRef],
) -> BTreeMap<(String, usize), Vec<Spend>> {
    let mut roots = Vec::new();
    let mut root_project = Vec::new();
    for (index, project) in projects.iter().enumerate() {
        for root in &project.roots {
            roots.push(root.clone());
            root_project.push(index);
        }
    }
    let now = window
        .to
        .and_hms_opt(23, 59, 59)
        .map_or_else(Utc::now, |t| t.and_utc());
    let files = crate::core::dashboard_pro::host_usage::collect_assigned(
        home,
        &roots,
        now,
        window.len_days(),
    );
    let pricing = crate::core::gain::model_pricing::ModelPricing::load();
    let mut rows: BTreeMap<(String, usize), BTreeMap<(String, String), Spend>> = BTreeMap::new();
    for (root_index, file) in files {
        let project = root_project[root_index];
        let provider = provider_of(&file.host);
        for (day, models) in &file.days {
            if !window.contains(day) {
                continue;
            }
            for (model, counts) in models {
                let Some(model_name) = payload::name(model) else {
                    continue;
                };
                let positive = |n: i64| u64::try_from(n).unwrap_or(0);
                let (input, write, read, output) = (
                    positive(counts.input),
                    positive(counts.cache_write),
                    positive(counts.cache_read),
                    positive(counts.output),
                );
                let cost = pricing
                    .quote(Some(model.as_str()))
                    .cost
                    .estimate_usd(input, output, write, read);
                let row = rows
                    .entry((day.clone(), project))
                    .or_default()
                    .entry((model_name.clone(), provider.to_string()))
                    .or_insert_with(|| Spend {
                        model: model_name.clone(),
                        provider: provider.to_string(),
                        requests: 0,
                        input_tokens: 0,
                        output_tokens: 0,
                        cached_tokens: 0,
                        list_usd: 0.0,
                    });
                row.requests = payload::count(row.requests + positive(counts.requests));
                // Cache writes are billed as input; cache reads are the
                // cached tokens.
                row.input_tokens = payload::count(row.input_tokens + input + write);
                row.output_tokens = payload::count(row.output_tokens + output);
                row.cached_tokens = payload::count(row.cached_tokens + read);
                row.list_usd = payload::usd(row.list_usd + cost);
            }
        }
    }
    rows.into_iter()
        .map(|(key, models)| (key, models.into_values().collect()))
        .collect()
}

// ── memory ───────────────────────────────────────────────────────────────────

/// A project's knowledge over time: facts added and errors linked to a fix
/// per day, and every fact's validity interval for the stock on a given day.
struct MemoryDays {
    flows: BTreeMap<String, (u64, u64)>,
    intervals: Vec<(NaiveDate, Option<NaiveDate>)>,
}

impl MemoryDays {
    /// Facts valid at the end of `date`.
    fn current_on(&self, date: &str) -> u64 {
        let Ok(day) = NaiveDate::parse_from_str(date, "%Y-%m-%d") else {
            return 0;
        };
        let n = self
            .intervals
            .iter()
            .filter(|(from, until)| *from <= day && until.is_none_or(|u| u > day))
            .count();
        payload::count(n as u64)
    }
}

fn memory_days(project: &ProjectRef, window: Window) -> MemoryDays {
    let day_of = |t: DateTime<Utc>| t.date_naive();
    let mut flows: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let mut intervals = Vec::new();
    let mut partitions = HashSet::new();
    for root in &project.roots {
        if !partitions.insert(crate::core::project_hash::hash_project_root(root)) {
            continue;
        }
        if let Some(knowledge) = crate::core::knowledge::ProjectKnowledge::load(root) {
            for fact in &knowledge.facts {
                let from = day_of(fact.valid_from.unwrap_or(fact.created_at));
                intervals.push((from, fact.valid_until.map(day_of)));
                let added = day_of(fact.created_at);
                if added >= window.from && added <= window.to {
                    flows.entry(added.to_string()).or_default().0 += 1;
                }
            }
        }
        let gotchas = crate::core::gotcha_tracker::GotchaStore::load(root);
        for gotcha in &gotchas.gotchas {
            let linked = day_of(gotcha.first_seen);
            if linked >= window.from && linked <= window.to {
                flows.entry(linked.to_string()).or_default().1 += 1;
            }
        }
    }
    MemoryDays { flows, intervals }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(time: &str, digest: &str) -> String {
        format!("{{\"timestamp\":\"{time}\",\"action\":\"gateway_receipt:{digest}|session=s1\"}}\n")
    }

    #[test]
    fn audit_days_read_sealed_segments_and_the_active_trail() {
        let data = tempfile::tempdir().unwrap();
        let audit = data.path().join("audit");
        let segments = audit.join("trail.segments");
        std::fs::create_dir_all(&segments).unwrap();
        std::fs::write(
            segments.join("000001.jsonl"),
            receipt("2026-10-01T08:00:00Z", "old"),
        )
        .unwrap();
        std::fs::write(
            segments.join("000002.jsonl"),
            receipt("2026-10-01T09:00:00Z", "old") + &receipt("2026-10-19T09:00:00Z", "aaaa"),
        )
        .unwrap();
        std::fs::write(
            audit.join("trail.jsonl"),
            receipt("2026-10-20T09:00:00Z", "aaaa") + &receipt("2026-10-20T10:00:00Z", "bbbb"),
        )
        .unwrap();
        let window = Window::ending(NaiveDate::from_ymd_opt(2026, 10, 20).unwrap(), 3);
        let sessions = HashMap::from([("s1", 0usize)]);
        let days: Vec<(String, u64)> = audit_days(data.path(), window, &sessions)
            .into_iter()
            .map(|(day, _, _, receipts)| (day, receipts))
            .collect();
        assert_eq!(
            days,
            vec![("2026-10-19".to_string(), 1), ("2026-10-20".to_string(), 2)]
        );
    }
}
