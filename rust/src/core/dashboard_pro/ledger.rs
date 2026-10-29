// SPDX-License-Identifier: Apache-2.0
//! The signed savings ledger (`<data>/savings/ledger.jsonl`) as compact
//! columns plus the report's day × folder buckets and hash-chain spans, read
//! once and then only from the last offset. Each entry keeps its byte offset
//! so a page or an export reads just its own lines. Numbers, names and
//! hashes only; never paths or content.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use chrono::{Datelike, NaiveDate};
use serde_json::{Map, Value};

use super::wire::{
    Chain, DayTotals, LedgerDays, LedgerEntry, LedgerFacets, LedgerFilters, LedgerTotals,
    ModelCount, ProxyDay, ProxyDays, ProxyKept, ProxyModelDay, RepoCount, RepoSelection,
    ReportBucket, ToolCount, ToolTotals,
};
use super::{ProError, ProResult, ProjectScope};

pub(crate) const KINDS: [&str; 5] = ["reduction", "cache_discount", "correction", "edit", "other"];
const BLENDED_MODEL: &str = "fallback-blended";
const UNRECORDED_BLENDED: &str = "not recorded (blended rate)";
/// The largest ledger the bridge indexes.
pub(crate) const MAX_LEDGER_BYTES: u64 = 4 << 30;
/// The longest single ledger line read back.
const MAX_LINE_BYTES: u64 = 1 << 20;

pub(crate) const LEDGER_PROXY: &str = "Via proxy";
pub(crate) const LEDGER_NONE: &str = "No project";
pub(crate) const LEDGER_OTHER: &str = "Other folder";

pub(crate) fn ledger_path(scope: &ProjectScope) -> PathBuf {
    scope.data_dir.join("savings").join("ledger.jsonl")
}

fn clip(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

fn engine_version(value: Option<&str>) -> Option<(u64, u64, u64)> {
    let mut parts = value?.splitn(3, '.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch: String = parts
        .next()?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    Some((major, minor, patch.parse().ok()?))
}

fn is_price_key(model: &str) -> bool {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    PATTERN
        .get_or_init(|| {
            regex::Regex::new(
                r"^(?:claude-(?:opus|sonnet|haiku)-4\.5|claude-fable-5|claude-3[-.].*|gpt-5\.4.*)$",
            )
            .expect("static regex")
        })
        .is_match(model)
}

/// The model an entry names, as the Ledger, the Report and the model filter
/// show it. Engines up to 3.11.4 stamped tool entries with a pricing key.
pub(crate) fn entry_model(entry: &LedgerEntry) -> String {
    let model = clip(
        entry
            .model_id
            .as_deref()
            .filter(|m| !m.is_empty())
            .unwrap_or("unknown"),
        80,
    );
    if model == BLENDED_MODEL {
        return UNRECORDED_BLENDED.to_string();
    }
    let written_by = engine_version(entry.version.as_deref());
    if entry.tool.as_deref() != Some("proxy_cache")
        && is_price_key(&model)
        && written_by.is_none_or(|v| v <= (3, 11, 4))
    {
        return format!("not recorded (priced as {model})");
    }
    model
}

pub(crate) fn ledger_kind(entry: &LedgerEntry) -> u8 {
    if entry.bounce_adjustment.unwrap_or(0) > 0 {
        return 2;
    }
    match entry.tool.as_deref() {
        Some("proxy_cache") => 1,
        Some("edit") => 3,
        _ if entry.saved_tokens.unwrap_or(0) > 0 => 0,
        _ => 4,
    }
}

pub(crate) fn entry_lines(entry: &LedgerEntry) -> (i64, i64) {
    (
        entry
            .lines_added
            .filter(|n| *n != 0)
            .or(entry.loc_added)
            .unwrap_or(0),
        entry
            .lines_removed
            .filter(|n| *n != 0)
            .or(entry.loc_removed)
            .unwrap_or(0),
    )
}

pub(crate) fn callable_label(tool: &str) -> String {
    match tool {
        "proxy_cache" => "prompt cache discount".into(),
        "bounce" => "correction".into(),
        "edit" => "file edit".into(),
        other => other.replace('_', " "),
    }
}

fn day_number(text: &str) -> i32 {
    text.get(..10)
        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        .map_or(0, |d| d.num_days_from_ce())
}

fn iso_day(ordinal: i32) -> String {
    if ordinal == 0 {
        return String::new();
    }
    NaiveDate::from_num_days_from_ce_opt(ordinal).map_or_else(String::new, |d| d.to_string())
}

fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

#[derive(Debug, Clone, Copy)]
struct Row {
    pos: u64,
    day: i32,
    tool: u32,
    model: u32,
    repo: u32,
    mech: u32,
    kind: u8,
    baseline: i64,
    saved: i64,
    corrected: i64,
    usd: f64,
    added: i64,
    removed: i64,
}

const TOOL: usize = 0;
const MODEL: usize = 1;
const REPO: usize = 2;
const MECH: usize = 3;

#[derive(Default)]
struct Names {
    names: Vec<String>,
    ids: HashMap<String, u32>,
}

impl Names {
    fn intern(&mut self, name: String) -> u32 {
        if let Some(id) = self.ids.get(&name) {
            return *id;
        }
        let id = u32::try_from(self.names.len()).unwrap_or(u32::MAX);
        self.ids.insert(name.clone(), id);
        self.names.push(name);
        id
    }
    fn id(&self, name: &str) -> Option<u32> {
        self.ids.get(name).copied()
    }
}

/// The incremental index of one ledger file.
#[derive(Default)]
pub(crate) struct LedgerIndex {
    path: PathBuf,
    offset: u64,
    rows: Vec<Row>,
    names: [Names; 4],
    days: LedgerDays,
    lines: i64,
    last_hash: Option<String>,
    broken: Option<i64>,
    spans: BTreeMap<String, (i64, i64, String, String)>,
    available: bool,
}

impl LedgerIndex {
    fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            ..Self::default()
        }
    }

    fn reset(&mut self) {
        *self = Self::new(&self.path);
    }

    fn refresh(&mut self) -> ProResult<()> {
        let Ok(meta) = std::fs::symlink_metadata(&self.path) else {
            self.reset();
            return Ok(());
        };
        if !meta.file_type().is_file() {
            self.reset();
            return Ok(());
        }
        if meta.len() > MAX_LEDGER_BYTES {
            return Err(ProError::new(
                413,
                "The savings ledger is too large to index",
            ));
        }
        if meta.len() < self.offset {
            self.reset();
        }
        self.available = true;
        if meta.len() == self.offset {
            return Ok(());
        }
        let mut file = std::fs::File::open(&self.path)
            .map_err(|_| ProError::internal("The savings ledger is not readable"))?;
        file.seek(SeekFrom::Start(self.offset))
            .map_err(|_| ProError::internal("The savings ledger is not readable"))?;
        let mut reader = BufReader::new(file.take(meta.len() - self.offset));
        let mut line = Vec::new();
        loop {
            line.clear();
            let read = reader
                .read_until(b'\n', &mut line)
                .map_err(|_| ProError::internal("The savings ledger is not readable"))?;
            if read == 0 || line.last() != Some(&b'\n') {
                break;
            }
            let start = self.offset;
            self.offset += read as u64;
            self.lines += 1;
            if let Ok(entry) = serde_json::from_slice::<LedgerEntry>(&line) {
                self.add(start, &entry);
            }
        }
        Ok(())
    }

    fn add(&mut self, pos: u64, entry: &LedgerEntry) {
        let (added, removed) = entry_lines(entry);
        let ts = entry.ts.clone().unwrap_or_default();
        let tool = entry.tool.clone().unwrap_or_default();
        let row = Row {
            pos,
            day: day_number(&ts),
            tool: self.names[TOOL].intern(clip(&tool, 80)),
            model: self.names[MODEL].intern(entry_model(entry)),
            repo: self.names[REPO].intern(clip(entry.repo_hash.as_deref().unwrap_or(""), 64)),
            mech: self.names[MECH].intern(clip(entry.mechanism.as_deref().unwrap_or(""), 80)),
            kind: ledger_kind(entry),
            baseline: entry.baseline_tokens.unwrap_or(0),
            saved: entry.saved_tokens.unwrap_or(0),
            corrected: entry.bounce_adjustment.unwrap_or(0),
            usd: entry.saved_usd.unwrap_or(0.0),
            added,
            removed,
        };
        self.rows.push(row);
        self.add_report(&ts, &tool, entry);
    }

    fn add_report(&mut self, ts: &str, tool: &str, entry: &LedgerEntry) {
        let current = entry.entry_hash.clone().unwrap_or_default();
        let previous = entry.prev_hash.clone().unwrap_or_default();
        if self.broken.is_none()
            && self
                .last_hash
                .as_ref()
                .is_some_and(|last| *last != previous)
        {
            self.broken = Some(self.lines);
        }
        self.last_hash = Some(current.clone());
        let day = ts.get(..10).unwrap_or("");
        if !is_day_shape(day) {
            return;
        }
        let lines = self.lines;
        self.spans
            .entry(day.to_string())
            .and_modify(|span| {
                span.1 = lines;
                span.3.clone_from(&current);
            })
            .or_insert_with(|| (lines, lines, current.clone(), current.clone()));
        let bucket = self
            .days
            .entry(day.to_string())
            .or_default()
            .entry(entry.repo_hash.clone().unwrap_or_default())
            .or_default();
        report_add(bucket, tool, entry);
    }

    /// `attributed`: the project folders known here. A proxy row recorded
    /// with one of them counts for that project like any other row; an
    /// older proxy row carries the proxy's own folder and belongs to no one
    /// project, so only the device scope shows it ("Via proxy").
    fn select(&self, filters: &LedgerFilters, attributed: &HashSet<String>) -> Vec<usize> {
        let names = &self.names;
        let scope: Option<HashSet<&str>> = filters
            .scope
            .as_ref()
            .map(|s| s.iter().map(String::as_str).collect());
        let repo_ok: Vec<bool> = names[REPO]
            .names
            .iter()
            .map(|n| scope.as_ref().is_none_or(|s| s.contains(n.as_str())))
            .collect();
        let project_ok: Vec<bool> = names[REPO]
            .names
            .iter()
            .map(|n| match &filters.repos {
                None => true,
                Some(RepoSelection::Only { hashes }) => hashes.contains(n),
                Some(RepoSelection::Except { hashes }) => !n.is_empty() && !hashes.contains(n),
            })
            .collect();
        let tool_ok: Vec<bool> = names[TOOL]
            .names
            .iter()
            .map(|n| filters.tool.is_empty() || *n == filters.tool)
            .collect();
        let model_id = (!filters.model.is_empty())
            .then(|| names[MODEL].id(&filters.model).unwrap_or(u32::MAX));
        let needle = filters.text.to_lowercase();
        let text_tool: Vec<bool> = names[TOOL]
            .names
            .iter()
            .map(|n| {
                needle.is_empty()
                    || n.to_lowercase().contains(&needle)
                    || callable_label(n).contains(&needle)
            })
            .collect();
        let text_mech: Vec<bool> = names[MECH]
            .names
            .iter()
            .map(|n| needle.is_empty() || n.to_lowercase().contains(&needle))
            .collect();
        let proxy_id = names[TOOL].id("proxy_cache");
        let start = day_number(&filters.from);
        let end = day_number(&filters.to);
        let mut chosen = Vec::new();
        for (index, row) in self.rows.iter().enumerate().rev() {
            if (start != 0 && row.day < start) || (end != 0 && row.day > end) {
                continue;
            }
            let proxied = Some(row.tool) == proxy_id
                && !attributed.contains(&names[REPO].names[row.repo as usize]);
            if proxied && scope.is_some() {
                continue;
            }
            if !tool_ok[row.tool as usize] || (!proxied && !repo_ok[row.repo as usize]) {
                continue;
            }
            if model_id.is_some_and(|m| row.model != m) {
                continue;
            }
            if proxied {
                if !filters.proxy {
                    continue;
                }
            } else if !project_ok[row.repo as usize] {
                continue;
            }
            if !(needle.is_empty() || text_tool[row.tool as usize] || text_mech[row.mech as usize])
            {
                continue;
            }
            chosen.push(index);
        }
        chosen
    }

    fn facets(&self, scope: Option<&[String]>, attributed: &HashSet<String>) -> LedgerFacets {
        let names = &self.names;
        let mut models = vec![0i64; names[MODEL].names.len()];
        let mut tools = vec![0i64; names[TOOL].names.len()];
        let mut repos = vec![0i64; names[REPO].names.len()];
        let proxy_id = names[TOOL].id("proxy_cache");
        let (mut first, mut last, mut total, mut proxied) = (0, 0, 0i64, 0i64);
        for row in &self.rows {
            let repo = &names[REPO].names[row.repo as usize];
            let unattributed = Some(row.tool) == proxy_id && !attributed.contains(repo);
            if scope.is_some_and(|s| unattributed || !s.contains(repo)) {
                continue;
            }
            total += 1;
            if row.day != 0 {
                if first == 0 {
                    first = row.day;
                }
                last = row.day;
            }
            models[row.model as usize] += 1;
            tools[row.tool as usize] += 1;
            if Some(row.tool) == proxy_id && !attributed.contains(repo) {
                proxied += 1;
            } else {
                repos[row.repo as usize] += 1;
            }
        }
        fn ranked<T>(counts: &[i64], names: &[String], make: impl Fn(String, i64) -> T) -> Vec<T> {
            let mut items: Vec<(usize, i64)> = counts
                .iter()
                .enumerate()
                .filter(|(_, n)| **n > 0)
                .map(|(i, n)| (i, *n))
                .collect();
            items.sort_by_key(|a| std::cmp::Reverse(a.1));
            items
                .into_iter()
                .map(|(i, n)| make(names[i].clone(), n))
                .collect()
        }
        LedgerFacets {
            entries: total,
            first: iso_day(first),
            last: iso_day(last),
            proxied,
            models: ranked(&models, &names[MODEL].names, |model, entries| ModelCount {
                model,
                entries,
            }),
            tools: ranked(&tools, &names[TOOL].names, |tool, entries| ToolCount {
                tool,
                entries,
            }),
            repos: ranked(&repos, &names[REPO].names, |repo, entries| RepoCount {
                repo,
                entries,
            }),
        }
    }

    fn summarize(&self, chosen: &[usize]) -> LedgerTotals {
        let mut tools: BTreeMap<String, ToolTotals> = BTreeMap::new();
        let mut days: BTreeMap<i32, DayTotals> = BTreeMap::new();
        for index in chosen {
            let row = &self.rows[*index];
            let name = &self.names[TOOL].names[row.tool as usize];
            let bucket = tools.entry(name.clone()).or_default();
            bucket.calls += 1;
            bucket.saved += row.saved;
            bucket.baseline += row.baseline;
            bucket.usd += row.usd;
            bucket.corrected += row.corrected;
            bucket.lines_added += row.added;
            bucket.lines_removed += row.removed;
            let day = days.entry(row.day).or_default();
            day.calls += 1;
            match row.kind {
                1 => day.cache_usd += row.usd,
                0 | 2 => {
                    day.saved += row.saved - row.corrected;
                    day.corrected += row.corrected;
                    day.usd += if row.kind == 0 {
                        row.usd
                    } else {
                        -row.usd.abs()
                    };
                }
                _ => {}
            }
        }
        for bucket in tools.values_mut() {
            bucket.usd = round4(bucket.usd);
        }
        LedgerTotals {
            tools,
            days: days
                .into_iter()
                .filter(|(day, _)| *day != 0)
                .map(|(day, mut row)| {
                    row.day = iso_day(day);
                    row.usd = round4(row.usd);
                    row.cache_usd = round4(row.cache_usd);
                    row
                })
                .collect(),
        }
    }

    fn read_line_at(&self, file: &mut std::fs::File, pos: u64) -> Option<Vec<u8>> {
        file.seek(SeekFrom::Start(pos)).ok()?;
        let mut line = Vec::new();
        BufReader::new(file.take(MAX_LINE_BYTES))
            .read_until(b'\n', &mut line)
            .ok()?;
        Some(line)
    }

    /// `scope`: one project's folders (its proxy rows only), or every row.
    fn proxy_days(&self, scope: Option<&[String]>) -> ProxyDays {
        let mut proxy: BTreeMap<i32, BTreeMap<String, ProxyModelDay>> = BTreeMap::new();
        let mut first = 0;
        let in_scope = |row: &&Row| {
            scope.is_none_or(|s| s.contains(&self.names[REPO].names[row.repo as usize]))
        };
        for row in self.rows.iter().filter(|r| r.kind == 1).filter(in_scope) {
            if first == 0 {
                first = row.day;
            }
            let model = self.names[MODEL].names[row.model as usize].clone();
            let entry = proxy
                .entry(row.day)
                .or_default()
                .entry(if model.is_empty() {
                    "unknown".into()
                } else {
                    model
                })
                .or_default();
            entry.prompts += 1;
            entry.cached_tokens += row.baseline;
            entry.discount_usd += row.usd;
        }
        let mut kept = ProxyKept::default();
        for row in &self.rows {
            if first == 0 || row.day < first || !matches!(row.kind, 0 | 2) {
                continue;
            }
            if row.kind == 0 {
                kept.saved += row.saved;
                kept.usd += row.usd;
                kept.reductions += 1;
            } else {
                kept.corrected += row.corrected;
                kept.usd -= row.usd.abs();
                kept.corrections += 1;
            }
        }
        kept.usd = round4(kept.usd);
        ProxyDays {
            first: iso_day(first),
            days: proxy
                .into_iter()
                .filter(|(day, _)| *day != 0)
                .map(|(day, models)| ProxyDay {
                    day: iso_day(day),
                    models: models
                        .into_iter()
                        .map(|(m, mut r)| {
                            r.discount_usd = round4(r.discount_usd);
                            (m, r)
                        })
                        .collect(),
                })
                .collect(),
            kept,
        }
    }
}

fn is_day_shape(day: &str) -> bool {
    let b = day.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

fn add_group(map: &mut BTreeMap<String, (i64, i64, f64)>, name: String, saved: i64, usd: f64) {
    let row = map.entry(name).or_insert((0, 0, 0.0));
    row.0 += 1;
    row.1 += saved;
    row.2 += usd;
}

fn report_add(bucket: &mut ReportBucket, tool: &str, entry: &LedgerEntry) {
    bucket.entries += 1;
    let tool = clip(tool, 60);
    let corrected = entry.bounce_adjustment.unwrap_or(0);
    let saved = entry.saved_tokens.unwrap_or(0);
    let usd = entry.saved_usd.unwrap_or(0.0);
    if corrected > 0 {
        bucket.corrections += 1;
        bucket.corrected += corrected;
        bucket.corrected_usd += usd.abs();
    } else if tool == "proxy_cache" {
        bucket.cache_entries += 1;
    } else if tool == "edit" {
        bucket.edits += 1;
    } else if saved > 0 {
        let baseline = entry.baseline_tokens.unwrap_or(0);
        let mut model = entry_model(entry);
        bucket.reductions += 1;
        bucket.saved += saved;
        bucket.baseline += baseline;
        bucket.actual += entry.actual_tokens.unwrap_or(0);
        bucket.usd += usd;
        bucket.would_usd +=
            baseline as f64 * entry.unit_price_per_m_usd.unwrap_or(0.0) / 1_000_000.0;
        if entry.model_id.as_deref() == Some(BLENDED_MODEL) {
            bucket.blended_usd += usd;
            bucket.blended_saved += saved;
            model = "Model unknown".into();
        } else if model.starts_with("not recorded (priced as ") {
            bucket.stand_in_usd += usd;
            model = "Model not recorded".into();
        }
        let tool_name = if tool.is_empty() {
            "unknown".to_string()
        } else {
            tool
        };
        let mechanism = clip(
            entry
                .mechanism
                .as_deref()
                .filter(|m| !m.is_empty())
                .unwrap_or("other"),
            40,
        );
        add_group(&mut bucket.tools, tool_name, saved, usd);
        add_group(&mut bucket.models, model, saved, usd);
        add_group(&mut bucket.mechanisms, mechanism, saved, usd);
    }
}

fn indexes() -> &'static Mutex<HashMap<PathBuf, LedgerIndex>> {
    static INDEXES: OnceLock<Mutex<HashMap<PathBuf, LedgerIndex>>> = OnceLock::new();
    INDEXES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Runs `f` on the refreshed index of `path`.
fn with_index<T>(path: &Path, f: impl FnOnce(&LedgerIndex) -> ProResult<T>) -> ProResult<T> {
    let mut map = indexes()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let index = map
        .entry(path.to_path_buf())
        .or_insert_with(|| LedgerIndex::new(path));
    index.refresh()?;
    f(index)
}

/// The Ledger page's checked filters for this project (`ledger_filters()`).
pub(crate) fn filters(
    scope: &ProjectScope,
    query: &Map<String, Value>,
) -> ProResult<LedgerFilters> {
    let get = |key: &str| {
        query
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let project = get("project");
    let labels = scope.labels();
    let labelled: Vec<String> = labels.iter().map(|label| label.hash.clone()).collect();
    let named: Vec<String> = labels
        .iter()
        .filter(|label| label.name == project)
        .map(|label| label.hash.clone())
        .collect();
    let (repos, proxy) = match project.as_str() {
        "" => (None, true),
        LEDGER_PROXY => (Some(RepoSelection::Only { hashes: Vec::new() }), true),
        LEDGER_NONE => (
            Some(RepoSelection::Only {
                hashes: vec![String::new()],
            }),
            false,
        ),
        LEDGER_OTHER => (Some(RepoSelection::Except { hashes: labelled }), false),
        _ if !named.is_empty() => (Some(RepoSelection::Only { hashes: named }), false),
        _ => return Err(ProError::bad("Unknown project filter")),
    };
    Ok(LedgerFilters {
        // The device scope reads every folder; a project reads its own.
        scope: (!scope.is_device()).then(|| scope.repo_hashes.clone()),
        repos,
        proxy,
        tool: get("tool"),
        model: get("model"),
        text: get("q"),
        from: get("from"),
        to: get("to"),
    })
}

/// Inputs `ledger_page`, `ledger_facets`, `ledger_totals`.
pub(crate) fn browse(
    scope: &ProjectScope,
    filters: &LedgerFilters,
    offset: usize,
    limit: usize,
) -> ProResult<(Value, Value, Value)> {
    let path = ledger_path(scope);
    let attributed = attributed(scope);
    with_index(&path, |index| {
        let facets = index.facets(filters.scope.as_deref(), &attributed);
        let chosen = index.select(filters, &attributed);
        let totals = index.summarize(&chosen);
        let page_rows = chosen.iter().skip(offset).take(limit.clamp(1, 500));
        let mut entries = Vec::new();
        if index.available
            && let Ok(mut file) = std::fs::File::open(&path)
        {
            for i in page_rows {
                if let Some(line) = index.read_line_at(&mut file, index.rows[*i].pos)
                    && let Ok(entry) = serde_json::from_slice::<LedgerEntry>(&line)
                {
                    entries.push(entry);
                }
            }
        }
        let day_of = |i: Option<&usize>| i.map_or(String::new(), |i| iso_day(index.rows[*i].day));
        let page = super::wire::LedgerPage {
            available: index.available,
            total: chosen.len() as i64,
            offset: offset as i64,
            limit: limit.clamp(1, 500) as i64,
            entries,
            from: day_of(chosen.last()),
            to: day_of(chosen.first()),
        };
        Ok((
            serde_json::to_value(page).unwrap_or(Value::Null),
            serde_json::to_value(facets).unwrap_or(Value::Null),
            serde_json::to_value(totals).unwrap_or(Value::Null),
        ))
    })
}

/// Raw entries of a selection in ledger (chain) order, oldest first, each
/// handed to `visit` until it returns `false`.
pub(crate) fn export_entries(
    scope: &ProjectScope,
    filters: &LedgerFilters,
    mut visit: impl FnMut(&Map<String, Value>, &LedgerEntry) -> ProResult<()>,
) -> ProResult<()> {
    let path = ledger_path(scope);
    let attributed = attributed(scope);
    with_index(&path, |index| {
        let chosen = index.select(filters, &attributed);
        if chosen.is_empty() {
            return Ok(());
        }
        let mut file = std::fs::File::open(&path)
            .map_err(|_| ProError::internal("The savings ledger is not readable"))?;
        for i in chosen.iter().rev() {
            let Some(line) = index.read_line_at(&mut file, index.rows[*i].pos) else {
                continue;
            };
            let (Ok(raw), Ok(entry)) = (
                serde_json::from_slice::<Map<String, Value>>(&line),
                serde_json::from_slice::<LedgerEntry>(&line),
            ) else {
                continue;
            };
            visit(&raw, &entry)?;
        }
        Ok(())
    })
}

/// Inputs `ledger_days` (this project's folders) and `chain`.
pub(crate) fn report(scope: &ProjectScope) -> ProResult<(LedgerDays, Chain)> {
    with_index(&ledger_path(scope), |index| {
        let days = index
            .days
            .iter()
            .filter_map(|(day, repos)| {
                let mine: BTreeMap<String, ReportBucket> = repos
                    .iter()
                    .filter(|(repo, _)| scope.is_device() || scope.repo_hashes.contains(repo))
                    .map(|(r, b)| (r.clone(), b.clone()))
                    .collect();
                (!mine.is_empty()).then(|| (day.clone(), mine))
            })
            .collect();
        Ok((
            days,
            Chain {
                lines: index.lines,
                broken: index.broken,
                spans: index.spans.clone(),
            },
        ))
    })
}

/// Input `proxy_days`: the project's proxy rows (recorded with the folder
/// the request named), or every row for the device scope.
pub(crate) fn proxy_days(scope: &ProjectScope) -> ProResult<ProxyDays> {
    let mine = (!scope.is_device()).then(|| scope.repo_hashes.clone());
    with_index(&ledger_path(scope), |index| {
        Ok(index.proxy_days(mine.as_deref()))
    })
}

/// The folders whose proxy rows count for a project: the project's own, or
/// for the device scope every named project's.
fn attributed(scope: &ProjectScope) -> HashSet<String> {
    if scope.is_device() {
        scope.labels().into_iter().map(|label| label.hash).collect()
    } else {
        scope.repo_hashes.iter().cloned().collect()
    }
}

/// USD per token kept out, as recorded with this project's reductions.
pub(crate) fn recorded_price(scope: &ProjectScope) -> ProResult<Option<f64>> {
    with_index(&ledger_path(scope), |index| {
        let (mut saved, mut usd) = (0i64, 0f64);
        for row in &index.rows {
            if row.kind == 0
                && (scope.is_device()
                    || scope
                        .repo_hashes
                        .contains(&index.names[REPO].names[row.repo as usize]))
            {
                saved += row.saved;
                usd += row.usd;
            }
        }
        Ok((saved > 0 && usd > 0.0).then(|| usd / saved as f64))
    })
}
