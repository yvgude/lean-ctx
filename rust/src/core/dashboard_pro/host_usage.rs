// SPDX-License-Identifier: Apache-2.0
//! Host-usage pre-aggregation for `spend.trend`: what Claude Code
//! (`~/.claude/projects/*/*.jsonl`) and Codex
//! (`~/.codex/sessions/**/rollout-*.jsonl`) recorded about their own model
//! use. Only token counts, model ids, times and the session folder are
//! parsed; message content is skipped by the deserializer and never kept.
//! Bounded: files touched in the last 30 days, at most [`MAX_FILES`] files and
//! the last [`MAX_FILE_BYTES`] of each; only sessions in the bound project.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;

use super::wire::{HostCounts, HostUsageFile};

pub(crate) const WINDOW_DAYS: i64 = 30;
pub(crate) const MAX_FILES: usize = 2_000;
pub(crate) const MAX_FILE_BYTES: u64 = 64 << 20;
const MAX_DEPTH: usize = 6;

#[derive(Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: Option<i64>,
    #[serde(default)]
    output_tokens: Option<i64>,
    #[serde(default)]
    cache_read_input_tokens: Option<i64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<i64>,
    #[serde(default)]
    cached_input_tokens: Option<i64>,
    #[serde(default)]
    cache_write_input_tokens: Option<i64>,
}

#[derive(Deserialize)]
struct ClaudeMessage {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct ClaudeLine {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(rename = "requestId", default)]
    request_id: Option<String>,
    #[serde(default)]
    message: Option<ClaudeMessage>,
}

#[derive(Deserialize)]
struct CodexInfo {
    #[serde(default)]
    last_token_usage: Option<Usage>,
}

#[derive(Deserialize)]
struct CodexPayload {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    info: Option<CodexInfo>,
}

#[derive(Deserialize)]
struct CodexLine {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    payload: Option<CodexPayload>,
}

/// One session file's counts; the folder stays in memory for matching only.
#[derive(Default, Clone)]
struct FileUsage {
    host: &'static str,
    offset: u64,
    cwd: String,
    model: String,
    last_id: String,
    days: BTreeMap<String, BTreeMap<String, HostCounts>>,
    /// The same counts per UTC minute (`YYYY-MM-DDTHH:MM`) and model, for
    /// usage within a time window ([`usage_between`]); sparse.
    minutes: BTreeMap<String, BTreeMap<String, HostCounts>>,
}

fn cache() -> &'static Mutex<HashMap<PathBuf, FileUsage>> {
    static C: OnceLock<Mutex<HashMap<PathBuf, FileUsage>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn modified_after(path: &Path, cutoff: std::time::SystemTime) -> Option<u64> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    (meta.file_type().is_file() && meta.modified().ok()? >= cutoff).then_some(meta.len())
}

fn sources(home: &Path, cutoff: std::time::SystemTime) -> Vec<(&'static str, PathBuf, u64)> {
    let mut found = Vec::new();
    if let Ok(projects) = std::fs::read_dir(home.join(".claude").join("projects")) {
        for dir in projects.filter_map(Result::ok) {
            if !dir.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let Ok(files) = std::fs::read_dir(dir.path()) else {
                continue;
            };
            for file in files.filter_map(Result::ok) {
                let path = file.path();
                if path.extension().is_some_and(|e| e == "jsonl")
                    && let Some(len) = modified_after(&path, cutoff)
                {
                    found.push(("claude-code", path, len));
                }
            }
        }
    }
    let mut stack = vec![(home.join(".codex").join("sessions"), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() && depth < MAX_DEPTH {
                stack.push((path, depth + 1));
            } else if kind.is_file()
                && entry.file_name().to_string_lossy().starts_with("rollout-")
                && path.extension().is_some_and(|e| e == "jsonl")
                && let Some(len) = modified_after(&path, cutoff)
            {
                found.push(("codex", path, len));
            }
        }
    }
    found.sort_by_key(|a| std::cmp::Reverse(a.2));
    found.truncate(MAX_FILES);
    found
}

/// Add one request's `counts` to its day and (with a valid stamp) minute.
fn add(usage: &mut FileUsage, stamp: &str, model: &str, now: DateTime<Utc>, counts: &HostCounts) {
    let add_to = |row: &mut HostCounts| {
        row.requests += counts.requests;
        row.input += counts.input;
        row.cache_read += counts.cache_read;
        row.cache_write += counts.cache_write;
        row.output += counts.output;
    };
    let valid_day = stamp.get(..10).filter(|d| super::request::is_day(d));
    let day = valid_day.map_or_else(|| now.date_naive().to_string(), str::to_string);
    let model: String = if model.is_empty() { "unknown" } else { model }
        .chars()
        .take(80)
        .collect();
    add_to(
        usage
            .days
            .entry(day)
            .or_default()
            .entry(model.clone())
            .or_default(),
    );
    if let Some(minute) = valid_day
        .and(stamp.get(..16))
        .filter(|m| m.as_bytes().get(10) == Some(&b'T'))
    {
        add_to(
            usage
                .minutes
                .entry(minute.to_owned())
                .or_default()
                .entry(model)
                .or_default(),
        );
    }
}

fn claude_line(usage: &mut FileUsage, line: &[u8], now: DateTime<Utc>) {
    let marker = |m: &[u8]| line.windows(m.len()).any(|w| w == m);
    if !marker(b"\"usage\"") || !marker(b"\"assistant\"") {
        return;
    }
    let Ok(record) = serde_json::from_slice::<ClaudeLine>(line) else {
        return;
    };
    if record.kind.as_deref() != Some("assistant") {
        return;
    }
    let Some(message) = record.message else {
        return;
    };
    let Some(counts) = message.usage else { return };
    let model = message.model.unwrap_or_default();
    let id = message.id.or(record.request_id).unwrap_or_default();
    if model == "<synthetic>" || (!id.is_empty() && id == usage.last_id) {
        return;
    }
    usage.last_id = id;
    if let Some(cwd) = record.cwd {
        usage.cwd = cwd;
    }
    let counts = HostCounts {
        requests: 1,
        input: counts.input_tokens.unwrap_or(0),
        cache_read: counts.cache_read_input_tokens.unwrap_or(0),
        cache_write: counts.cache_creation_input_tokens.unwrap_or(0),
        output: counts.output_tokens.unwrap_or(0),
    };
    add(
        usage,
        record.timestamp.as_deref().unwrap_or(""),
        &model,
        now,
        &counts,
    );
}

fn codex_line(usage: &mut FileUsage, line: &[u8], now: DateTime<Utc>) {
    let marker = |m: &[u8]| line.windows(m.len()).any(|w| w == m);
    if !marker(b"\"token_count\"") && !marker(b"\"turn_context\"") && !marker(b"\"session_meta\"") {
        return;
    }
    let Ok(record) = serde_json::from_slice::<CodexLine>(line) else {
        return;
    };
    let Some(payload) = record.payload else {
        return;
    };
    match record.kind.as_deref() {
        Some("session_meta") => {
            if let Some(cwd) = payload.cwd {
                usage.cwd = cwd;
            }
        }
        Some("turn_context") => {
            if let Some(model) = payload.model {
                usage.model = model.chars().take(80).collect();
            }
            if let Some(cwd) = payload.cwd {
                usage.cwd = cwd;
            }
        }
        Some("event_msg") if payload.kind.as_deref() == Some("token_count") => {
            let Some(counts) = payload.info.and_then(|i| i.last_token_usage) else {
                return;
            };
            let model = usage.model.clone();
            let cached = counts.cached_input_tokens.unwrap_or(0);
            let counts = HostCounts {
                requests: 1,
                input: (counts.input_tokens.unwrap_or(0) - cached).max(0),
                cache_read: cached,
                cache_write: counts.cache_write_input_tokens.unwrap_or(0),
                output: counts.output_tokens.unwrap_or(0),
            };
            add(
                usage,
                record.timestamp.as_deref().unwrap_or(""),
                &model,
                now,
                &counts,
            );
        }
        _ => {}
    }
}

fn read_new(path: &Path, len: u64, usage: &mut FileUsage, now: DateTime<Utc>) {
    let Ok(mut file) = std::fs::File::open(path) else {
        return;
    };
    // A file larger than the cap is read from its last MAX_FILE_BYTES only.
    let mut start = usage.offset.max(len.saturating_sub(MAX_FILE_BYTES));
    if file.seek(SeekFrom::Start(start)).is_err() {
        return;
    }
    let mut reader = BufReader::new(file.take(len - start));
    let mut line = Vec::new();
    if start > usage.offset {
        // Skip the partial line the cut landed in.
        let Ok(read) = reader.read_until(b'\n', &mut line) else {
            return;
        };
        start += read as u64;
    }
    usage.offset = start;
    loop {
        line.clear();
        let Ok(read) = reader.read_until(b'\n', &mut line) else {
            break;
        };
        if read == 0 || line.last() != Some(&b'\n') {
            break;
        }
        usage.offset += read as u64;
        if usage.host == "codex" {
            codex_line(usage, &line, now);
        } else {
            claude_line(usage, &line, now);
        }
    }
}

fn under(folder: &str, root: &str) -> bool {
    let folder = folder.trim_end_matches('/');
    let root = root.trim_end_matches('/');
    !root.is_empty() && (folder == root || folder.starts_with(&format!("{root}/")))
}

/// Input `host_usage`: per session file of the bound project, daily tokens
/// per model within the last 30 days. `project` is the project root (the
/// folder the runtime matches by its `repo_hash`).
pub(crate) fn collect(home: &Path, roots: &[String], now: DateTime<Utc>) -> Vec<HostUsageFile> {
    scan(home, now, WINDOW_DAYS, |cwd| {
        roots.iter().any(|r| under(cwd, r)).then_some(0)
    })
    .into_iter()
    .map(|(_, mut file)| {
        // Every matching session is reported under the project's own root.
        file.project.clone_from(&roots[0]);
        file
    })
    .collect()
}

/// Every session file of the last `window_days` whose folder lies under one
/// of `roots`, each assigned to its deepest matching root (an index into
/// `roots`), so nested project roots never count one session twice. The
/// returned `project` is empty: callers name projects by their own ids.
pub(crate) fn collect_assigned(
    home: &Path,
    roots: &[String],
    now: DateTime<Utc>,
    window_days: i64,
) -> Vec<(usize, HostUsageFile)> {
    scan(home, now, window_days.max(1), |cwd| {
        roots
            .iter()
            .enumerate()
            .filter(|(_, root)| under(cwd, root))
            .max_by_key(|(_, root)| root.trim_end_matches('/').len())
            .map(|(index, _)| index)
    })
}

/// Per model, what the session files under `roots` recorded from the
/// minute of `from` up to the minute of `to` (both included), from files
/// touched in the last [`WINDOW_DAYS`]. A session under a deeper root of
/// `others` (another project nested inside) belongs to that one only.
pub(crate) fn usage_between(
    home: &Path,
    roots: &[String],
    others: &[String],
    now: DateTime<Utc>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> BTreeMap<String, HostCounts> {
    let minute = |t: DateTime<Utc>| t.format("%Y-%m-%dT%H:%M").to_string();
    let (first, last) = (minute(from), minute(to));
    let mut out: BTreeMap<String, HostCounts> = BTreeMap::new();
    if first > last {
        return out;
    }
    visit(home, now, WINDOW_DAYS, |usage| {
        let depth = |list: &[String]| {
            list.iter()
                .filter(|root| under(&usage.cwd, root))
                .map(|root| root.trim_end_matches('/').len())
                .max()
        };
        let Some(own) = depth(roots) else {
            return;
        };
        if depth(others).is_some_and(|other| other > own) {
            return;
        }
        for models in usage
            .minutes
            .range(first.clone()..=last.clone())
            .map(|(_, models)| models)
        {
            for (model, counts) in models {
                let row = out.entry(model.clone()).or_default();
                row.requests += counts.requests;
                row.input += counts.input;
                row.cache_read += counts.cache_read;
                row.cache_write += counts.cache_write;
                row.output += counts.output;
            }
        }
    });
    out
}

/// Bring the cached counts of every session file touched in the last
/// `window_days` up to date and hand each to `each`.
fn visit(home: &Path, now: DateTime<Utc>, window_days: i64, mut each: impl FnMut(&FileUsage)) {
    let cutoff_time = std::time::SystemTime::now()
        - std::time::Duration::from_secs(u64::try_from(window_days).unwrap_or(30) * 86_400);
    let found = sources(home, cutoff_time);
    let mut map = cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let live: std::collections::HashSet<&PathBuf> = found.iter().map(|(_, p, _)| p).collect();
    map.retain(|path, _| live.contains(path));
    for (host, path, len) in &found {
        let usage = map.entry(path.clone()).or_insert_with(|| FileUsage {
            host,
            ..FileUsage::default()
        });
        if *len < usage.offset {
            *usage = FileUsage {
                host,
                ..FileUsage::default()
            };
        }
        if *len > usage.offset {
            read_new(path, *len, usage, now);
        }
        each(usage);
    }
}

fn scan(
    home: &Path,
    now: DateTime<Utc>,
    window_days: i64,
    assign: impl Fn(&str) -> Option<usize>,
) -> Vec<(usize, HostUsageFile)> {
    let cutoff_day = (now - Duration::days(window_days - 1))
        .date_naive()
        .to_string();
    let mut out = Vec::new();
    visit(home, now, window_days, |usage| {
        let Some(assigned) = assign(&usage.cwd) else {
            return;
        };
        let days: BTreeMap<String, BTreeMap<String, HostCounts>> = usage
            .days
            .iter()
            .filter(|(day, _)| **day >= cutoff_day)
            .map(|(d, m)| (d.clone(), m.clone()))
            .collect();
        if days.is_empty() {
            return;
        }
        out.push((
            assigned,
            HostUsageFile {
                project: String::new(),
                host: usage.host.to_string(),
                days,
            },
        ));
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(stamp: &str, id: &str, input: i64, output: i64, cwd: &str) -> String {
        serde_json::json!({
            "type": "assistant", "timestamp": stamp, "cwd": cwd, "requestId": id,
            "message": {"id": id, "model": "claude-opus-5-5",
                "usage": {"input_tokens": input, "output_tokens": output}}
        })
        .to_string()
            + "\n"
    }

    /// Only the requests inside the window, of sessions under the roots,
    /// count; the day totals stay as they were.
    #[test]
    fn usage_between_counts_the_requests_inside_the_window() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".claude/projects/p");
        std::fs::create_dir_all(&dir).unwrap();
        let now = Utc::now();
        let minute = |m: i64| (now - Duration::minutes(m)).to_rfc3339();
        let log = [
            line(&minute(90), "r1", 100, 10, "/work/payments"),
            line(&minute(30), "r2", 200, 20, "/work/payments"),
            line(&minute(10), "r3", 400, 40, "/work/payments/src"),
        ]
        .concat();
        std::fs::write(dir.join("s.jsonl"), log).unwrap();
        std::fs::write(
            dir.join("other.jsonl"),
            line(&minute(30), "o1", 9_000, 900, "/work/other"),
        )
        .unwrap();
        let roots = vec!["/work/payments".to_owned()];
        let counts = usage_between(
            home.path(),
            &roots,
            &[],
            now,
            now - Duration::minutes(45),
            now,
        );
        let row = &counts["claude-opus-5-5"];
        assert_eq!((row.requests, row.input, row.output), (2, 600, 60));
        let all = collect_assigned(home.path(), &roots, now, 2);
        let total: i64 = all
            .iter()
            .flat_map(|(_, file)| file.days.values())
            .flat_map(BTreeMap::values)
            .map(|c| c.input)
            .sum();
        assert_eq!(total, 700, "the day buckets keep every request");
        assert!(
            usage_between(
                home.path(),
                &roots,
                &[],
                now,
                now,
                now - Duration::minutes(1)
            )
            .is_empty()
        );

        // A project nested inside another owns its sessions alone.
        std::fs::write(
            dir.join("nested.jsonl"),
            line(&minute(20), "n1", 50, 5, "/work/payments/billing"),
        )
        .unwrap();
        let nested = vec!["/work/payments/billing".to_owned()];
        let window = (now - Duration::minutes(45), now);
        let outer = usage_between(home.path(), &roots, &nested, now, window.0, window.1);
        assert_eq!(outer["claude-opus-5-5"].input, 600);
        let inner = usage_between(home.path(), &nested, &roots, now, window.0, window.1);
        assert_eq!(inner["claude-opus-5-5"].input, 50);
    }
}
