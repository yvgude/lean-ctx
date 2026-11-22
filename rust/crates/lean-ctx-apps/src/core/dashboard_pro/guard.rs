// SPDX-License-Identifier: Apache-2.0
//! Guard events from the signed audit trail (`<data>/audit/trail.jsonl`) and
//! delivery summaries from decision receipts (`<data>/gateway/receipts`).
//! Both are read incrementally and keep counts, kinds, tool names, models
//! and token numbers only.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde_json::{Map, Value};

use super::wire::{EgressRow, GuardEvent};
use super::{GUARD_KINDS, ProError, ProResult};

const RECENT_KEEP: usize = 300;
const MAX_TRAIL_BYTES: u64 = 4 << 30;
/// At most this many receipts are summarized per refresh.
const MAX_RECEIPTS: usize = 3_000;
const MAX_RECEIPT_BYTES: u64 = 64 << 20;
const FIXTURE_MODELS: [&str; 6] = [
    "claude-x",
    "gpt-x",
    "gpt-4o-mini",
    "m",
    "model",
    "test-model",
];

fn audit_pattern() -> &'static regex::bytes::Regex {
    static PATTERN: OnceLock<regex::bytes::Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        let kinds = GUARD_KINDS.join("|");
        regex::bytes::Regex::new(&format!(
            r#""timestamp":"(\d{{4}}-\d{{2}}-\d{{2}})T(\d{{2}}:\d{{2}}:\d{{2}})[^"]*".*?"tool":"([^"]{{0,80}})".*?"action":"({kinds}):(\d{{1,9}})"#
        ))
        .expect("static regex")
    })
}

/// The guard event of one audit line, if it is one.
pub(crate) fn parse_event(line: &[u8]) -> Option<GuardEvent> {
    if !line.windows(10).any(|w| w == b"\"action\":\"") {
        return None;
    }
    let caps = audit_pattern().captures(line)?;
    let text = |i: usize| String::from_utf8_lossy(&caps[i]).into_owned();
    Some(GuardEvent {
        at: format!("{}T{}Z", text(1), text(2)),
        kind: text(4),
        count: text(5).parse().unwrap_or(0),
        tool: text(3),
    })
}

/// Complete lines of `path` from `offset`, handed to `visit`; returns the
/// offset after the last complete line. Offsets run across the sealed
/// segments and the active trail as if they were one file: segments are
/// never written again and a seal only renames, so an offset keeps
/// pointing at the same line.
pub(crate) fn scan_lines(path: &Path, offset: u64, mut visit: impl FnMut(&[u8])) -> ProResult<u64> {
    let files = trail_files(path);
    let total: u64 = files.iter().map(|(_, len)| len).sum();
    let start = if total < offset { 0 } else { offset };
    if total - start > MAX_TRAIL_BYTES {
        return Err(ProError::new(413, "The audit trail is too large to read"));
    }
    let unreadable = |_| ProError::internal("Audit trail unreadable");
    let mut position = start;
    let mut base = 0u64;
    for (file, len) in files {
        let end = base + len;
        if end <= position {
            base = end;
            continue;
        }
        // Sealed away since it was listed: the next scan goes on from here.
        let Ok(mut file) = std::fs::File::open(&file) else {
            break;
        };
        file.seek(SeekFrom::Start(position - base))
            .map_err(unreadable)?;
        let mut reader = BufReader::new(file.take(end - position));
        let mut line = Vec::new();
        loop {
            line.clear();
            let read = reader.read_until(b'\n', &mut line).map_err(unreadable)?;
            if read == 0 {
                break;
            }
            if line.last() != Some(&b'\n') {
                return Ok(position);
            }
            position += read as u64;
            visit(&line);
        }
        base = end;
    }
    Ok(position)
}

/// The trail's files with their lengths, in chain order: the sealed
/// segments, then the active trail.
fn trail_files(path: &Path) -> Vec<(PathBuf, u64)> {
    crate::core::audit_trail::trail_files_since(path, None)
        .into_iter()
        .filter_map(|file| {
            let meta = std::fs::symlink_metadata(&file).ok()?;
            meta.file_type().is_file().then_some((file, meta.len()))
        })
        .collect()
}

/// Length of the whole trail, the end [`scan_lines`] offsets run to.
pub(crate) fn trail_len(path: &Path) -> u64 {
    trail_files(path).iter().map(|(_, len)| len).sum()
}

#[derive(Default)]
struct Timeline {
    offset: u64,
    days: BTreeMap<String, BTreeMap<String, i64>>,
    recent: Vec<GuardEvent>,
}

/// Timelines per trail and, for one project, per digest of its sessions
/// (`None`: the whole trail).
type TimelineKey = (PathBuf, Option<blake3::Hash>);

fn timelines() -> &'static Mutex<HashMap<TimelineKey, Timeline>> {
    static T: OnceLock<Mutex<HashMap<TimelineKey, Timeline>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
}

fn session_pattern() -> &'static regex::bytes::Regex {
    static PATTERN: OnceLock<regex::bytes::Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        regex::bytes::Regex::new(r#""action":"[^"]*\|session=([A-Za-z0-9_.-]{1,128})""#)
            .expect("static regex")
    })
}

/// The session an audit line was recorded in, if it names one.
fn line_session(line: &[u8]) -> Option<&str> {
    let caps = session_pattern().captures(line)?;
    std::str::from_utf8(caps.get(1)?.as_bytes()).ok()
}

/// Inputs `guard_days` (`[{day, <kind>: n}]`, whole trail) and
/// `guard_recent` (last 300 events, oldest first).
pub(crate) fn timeline(trail: &Path) -> ProResult<(Vec<Value>, Vec<GuardEvent>)> {
    timeline_for(trail, None)
}

/// [`timeline`] over the entries of `sessions` only (one project's agent
/// sessions); an entry without a session belongs to no one project.
pub(crate) fn timeline_for(
    trail: &Path,
    sessions: Option<&std::collections::HashSet<String>>,
) -> ProResult<(Vec<Value>, Vec<GuardEvent>)> {
    let digest = sessions.map(|sessions| {
        let mut ids: Vec<&str> = sessions.iter().map(String::as_str).collect();
        ids.sort_unstable();
        blake3::hash(ids.join("\n").as_bytes())
    });
    let mut map = timelines()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if digest.is_some() {
        // A project's sessions grow: its older timelines are never read again.
        map.retain(|(path, kept), _| path != trail || kept.is_none() || *kept == digest);
    }
    let entry = map.entry((trail.to_path_buf(), digest)).or_default();
    if trail_len(trail) < entry.offset {
        *entry = Timeline::default();
    }
    let mut fresh = Vec::new();
    entry.offset = scan_lines(trail, entry.offset, |line| {
        if let Some(sessions) = sessions
            && !line_session(line).is_some_and(|id| sessions.contains(id))
        {
            return;
        }
        if let Some(event) = parse_event(line) {
            fresh.push(event);
        }
    })?;
    for event in fresh {
        *entry
            .days
            .entry(event.at[..10].to_string())
            .or_default()
            .entry(event.kind.clone())
            .or_default() += event.count;
        entry.recent.push(event);
    }
    let keep = entry.recent.len().saturating_sub(RECENT_KEEP);
    entry.recent.drain(..keep);
    let days = entry
        .days
        .iter()
        .map(|(day, kinds)| {
            let mut row = Map::new();
            row.insert("day".into(), day.clone().into());
            for kind in GUARD_KINDS {
                row.insert(kind.into(), kinds.get(kind).copied().unwrap_or(0).into());
            }
            Value::Object(row)
        })
        .collect();
    Ok((days, entry.recent.clone()))
}

fn is_packed(name: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("zst"))
}

fn receipt_digest(name: &str) -> Option<&str> {
    name.strip_suffix(".json.zst")
        .or_else(|| name.strip_suffix(".json"))
        .filter(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_alphanumeric()))
}

fn receipt_tail(path: &Path, compressed: bool) -> Option<Value> {
    let marker = b"],\"destination\":";
    let parse = |tail: &[u8]| -> Option<Value> {
        let at = tail.windows(marker.len()).rposition(|w| w == marker)?;
        let mut json = Vec::with_capacity(tail.len() - at);
        json.push(b'{');
        json.extend_from_slice(&tail[at + 2..]);
        serde_json::from_slice(&json).ok()
    };
    if compressed {
        let file = std::fs::File::open(path).ok()?;
        let mut raw = Vec::new();
        zstd::stream::read::Decoder::new(file)
            .ok()?
            .take(MAX_RECEIPT_BYTES)
            .read_to_end(&mut raw)
            .ok()?;
        return parse(&raw);
    }
    let mut file = std::fs::File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    for window in [16_384u64, 262_144] {
        file.seek(SeekFrom::Start(size.saturating_sub(window)))
            .ok()?;
        let mut tail = Vec::new();
        (&mut file).take(window).read_to_end(&mut tail).ok()?;
        if let Some(value) = parse(&tail) {
            return Some(value);
        }
    }
    None
}

fn summarize(path: &Path, mtime: i64, compressed: bool) -> Option<EgressRow> {
    let receipt = receipt_tail(path, compressed)?;
    let s = |v: &Value, key: &str, default: &str, limit: usize| -> String {
        v.get(key)
            .and_then(Value::as_str)
            .unwrap_or(default)
            .chars()
            .take(limit)
            .collect()
    };
    let n = |v: &Value, key: &str| v.get(key).and_then(Value::as_i64).unwrap_or(0);
    let destination = receipt.get("destination").cloned().unwrap_or(Value::Null);
    let tokens = receipt.get("tokens").cloned().unwrap_or(Value::Null);
    let security = receipt.get("security").cloned().unwrap_or(Value::Null);
    Some(EgressRow {
        mtime,
        provider: s(&destination, "provider", "unknown", 60),
        model: s(&destination, "model", "", 80),
        locality: s(&destination, "locality", "unknown", 20),
        outcome: s(&receipt, "outcome", "unknown", 20),
        original: n(&tokens, "original"),
        delivered: n(&tokens, "delivered"),
        redactions: n(&security, "redactions"),
        blocked: n(&security, "blocked_objects"),
        injection: n(&security, "injection_signals"),
        incomplete: n(&security, "incomplete_coverage"),
        mode: s(&receipt, "mode", "", 20),
    })
}

fn receipt_cache() -> &'static Mutex<HashMap<PathBuf, HashMap<String, EgressRow>>> {
    static C: OnceLock<Mutex<HashMap<PathBuf, HashMap<String, EgressRow>>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Input `egress_rows`: delivery summaries of receipts written since
/// `since_unix` (receipts are immutable, so each is read once), without the
/// fixture models engine tests once wrote into real stores.
pub(crate) fn egress_rows(receipts: &Path, since_unix: i64) -> Value {
    let Ok(entries) = std::fs::read_dir(receipts) else {
        return serde_json::json!({ "rows": [], "indexed": 0, "total": 0, "running": false });
    };
    let mut files: BTreeMap<String, (String, i64)> = BTreeMap::new();
    for entry in entries.filter_map(Result::ok) {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(digest) = receipt_digest(&name) else {
            continue;
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
        if mtime < since_unix {
            continue;
        }
        let digest = digest.to_string();
        if files
            .get(&digest)
            .is_none_or(|(existing, _)| !is_packed(existing))
        {
            files.insert(digest, (name, mtime));
        }
    }
    let total = files.len();
    let mut newest: Vec<(String, (String, i64))> = files.into_iter().collect();
    newest.sort_by_key(|a| std::cmp::Reverse(a.1.1));
    newest.truncate(MAX_RECEIPTS);
    let mut cache = receipt_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let known = cache.entry(receipts.to_path_buf()).or_default();
    let listed: std::collections::HashSet<&String> = newest.iter().map(|(d, _)| d).collect();
    known.retain(|digest, _| listed.contains(digest));
    let mut rows = Vec::with_capacity(newest.len());
    for (digest, (name, mtime)) in &newest {
        if !known.contains_key(digest) {
            let path = receipts.join(name);
            if let Some(row) = summarize(&path, *mtime, is_packed(name)) {
                known.insert(digest.clone(), row);
            }
        }
        if let Some(row) = known.get(digest)
            && !FIXTURE_MODELS.contains(&row.model.as_str())
        {
            rows.push(row.clone());
        }
    }
    serde_json::json!({
        "rows": rows,
        "indexed": newest.len(),
        "total": total,
        "running": false,
    })
}
