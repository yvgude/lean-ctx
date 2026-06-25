use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::data_dir::lean_ctx_data_dir;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveEntry {
    pub id: String,
    pub tool: String,
    pub command: String,
    pub size_chars: usize,
    pub size_tokens: usize,
    pub created_at: DateTime<Utc>,
    pub session_id: Option<String>,
}

fn archive_base_dir() -> PathBuf {
    lean_ctx_data_dir()
        .unwrap_or_else(|_| PathBuf::from(".lean-ctx"))
        .join("archives")
}

fn entry_dir(id: &str) -> PathBuf {
    let prefix = if id.len() >= 2 { &id[..2] } else { id };
    archive_base_dir().join(prefix)
}

fn content_path(id: &str) -> PathBuf {
    entry_dir(id).join(format!("{id}.txt"))
}

fn meta_path(id: &str) -> PathBuf {
    entry_dir(id).join(format!("{id}.meta.json"))
}

#[cfg(unix)]
fn set_private_file_perms(path: &PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

fn compute_id(content: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    let hash = hasher.finish();
    format!("{hash:016x}")
}

#[must_use]
pub fn is_enabled() -> bool {
    if let Ok(v) = std::env::var("LEAN_CTX_ARCHIVE") {
        return !matches!(v.as_str(), "0" | "false" | "off");
    }
    super::config::Config::load().archive.enabled
}

fn threshold_chars() -> usize {
    if let Ok(v) = std::env::var("LEAN_CTX_ARCHIVE_THRESHOLD")
        && let Ok(n) = v.parse::<usize>()
    {
        return n;
    }
    super::config::Config::load().archive.threshold_chars
}

fn max_age_hours() -> u64 {
    if let Ok(v) = std::env::var("LEAN_CTX_ARCHIVE_TTL")
        && let Ok(n) = v.parse::<u64>()
    {
        return n;
    }
    super::config::Config::load().archive_max_age_hours_effective()
}

/// Effective on-disk byte budget for archived `.txt`/`.meta.json` content,
/// derived from `[archive] max_disk_mb` (or the simplified global `max_disk_mb`)
/// so it matches what `doctor` reports. `0` disables the size cap; the TTL still
/// applies. This is what bounds the archive store on disk (#417).
fn max_disk_bytes() -> u64 {
    super::config::Config::load()
        .archive_max_disk_mb_effective()
        .saturating_mul(1024 * 1024)
}

#[must_use]
pub fn should_archive(content: &str) -> bool {
    is_enabled() && content.len() >= threshold_chars()
}

const MAX_ARCHIVE_SIZE: usize = 10 * 1024 * 1024; // 10 MB

pub fn store(tool: &str, command: &str, content: &str, session_id: Option<&str>) -> Option<String> {
    if !is_enabled() || content.is_empty() {
        return None;
    }

    let content = if content.len() > MAX_ARCHIVE_SIZE {
        &content[..content.floor_char_boundary(MAX_ARCHIVE_SIZE)]
    } else {
        content
    };

    let id = compute_id(content);
    let c_path = content_path(&id);

    // Fast path: content already archived (idempotent, no race)
    if c_path.exists() {
        return Some(id);
    }

    let dir = entry_dir(&id);
    if std::fs::create_dir_all(&dir).is_err() {
        return None;
    }

    // Atomic write: PID-unique tmp file prevents race between parallel writers.
    // rename() is atomic on POSIX; on Windows it replaces atomically too.
    // If two processes race past the exists() check, both write their own tmp
    // file and both rename to the same target — last writer wins, content is
    // identical (same hash), so the result is correct either way.
    let pid = std::process::id();
    let tmp_path = c_path.with_extension(format!("tmp.{pid}"));
    if std::fs::write(&tmp_path, content).is_err() {
        return None;
    }
    if std::fs::rename(&tmp_path, &c_path).is_err() {
        let _ = std::fs::remove_file(&tmp_path);
        // Another process may have won the race — check if content is there now
        if c_path.exists() {
            return Some(id);
        }
        return None;
    }
    #[cfg(unix)]
    set_private_file_perms(&c_path);

    let tokens = super::tokens::count_tokens(content);
    let entry = ArchiveEntry {
        id: id.clone(),
        tool: tool.to_string(),
        command: command.to_string(),
        size_chars: content.len(),
        size_tokens: tokens,
        created_at: Utc::now(),
        session_id: session_id.map(std::string::ToString::to_string),
    };

    if let Ok(json) = serde_json::to_string_pretty(&entry) {
        let meta_tmp = meta_path(&id).with_extension(format!("tmp.{pid}"));
        if std::fs::write(&meta_tmp, &json).is_ok() {
            let meta_final = meta_path(&id);
            let _ = std::fs::rename(&meta_tmp, &meta_final);
            #[cfg(unix)]
            set_private_file_perms(&meta_final);
        }
    }

    super::archive_fts::index_entry(&id, tool, command, content);

    Some(id)
}

#[must_use]
pub fn retrieve(id: &str) -> Option<String> {
    let path = content_path(id);
    std::fs::read_to_string(path).ok()
}

/// Format a range of lines from content with `{:>6}|` line-number gutter.
/// Shared by `retrieve_with_range` (archive) and `expand_reference` (ref store).
pub(crate) fn format_range(content: &str, start: usize, end: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let start = start.saturating_sub(1).min(lines.len());
    let end = end.min(lines.len());
    if start >= end {
        return String::new();
    }
    lines[start..end]
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{:>6}|{line}", start + i + 1))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Search content for lines matching `pattern` (case-insensitive) and return
/// gutter-prefixed matches. `label` appears in the result message (e.g. "archive
/// a1966..." or "reference `ref_18bb`..."). Shared by archive and ref store paths.
pub(crate) fn format_search(content: &str, pattern: &str, label: &str) -> String {
    let pattern_lower = pattern.to_lowercase();
    let matches: Vec<String> = content
        .lines()
        .enumerate()
        .filter(|(_, line)| line.to_lowercase().contains(&pattern_lower))
        .map(|(i, line)| format!("{:>6}|{line}", i + 1))
        .collect();
    if matches.is_empty() {
        format!("No matches for \"{pattern}\" in {label}")
    } else {
        format!(
            "{} match(es) for \"{}\":\n{}",
            matches.len(),
            pattern,
            matches.join("\n")
        )
    }
}

/// Describe JSON structure: navigate `path` (dot/slash separated) into parsed
/// content, then format with `describe_json`. `label` appears in error messages.
/// Shared by archive and ref store paths.
pub(crate) fn format_json_keys(content: &str, path: Option<&str>, label: &str) -> Option<String> {
    let root: serde_json::Value = serde_json::from_str(content.trim()).ok()?;
    let mut cur = &root;
    let mut walked = String::from("$");
    if let Some(p) = path {
        for seg in p.split(['.', '/']).filter(|s| !s.is_empty()) {
            let next = if let Ok(idx) = seg.parse::<usize>() {
                cur.get(idx)
            } else {
                cur.get(seg)
            };
            match next {
                Some(v) => {
                    cur = v;
                    walked.push('.');
                    walked.push_str(seg);
                }
                None => {
                    return Some(format!("Path '{p}' not found at '{walked}' in {label}"));
                }
            }
        }
    }
    Some(format!("{walked} => {}", describe_json(cur)))
}

#[must_use]
pub fn retrieve_with_range(id: &str, start: usize, end: usize) -> Option<String> {
    let content = retrieve(id)?;
    Some(format_range(&content, start, end))
}

#[must_use]
pub fn retrieve_with_search(id: &str, pattern: &str) -> Option<String> {
    let content = retrieve(id)?;
    Some(format_search(&content, pattern, &format!("archive {id}")))
}

/// Retrieve the first `n` lines of an archived entry, with a line-number gutter.
#[must_use]
pub fn retrieve_head(id: &str, n: usize) -> Option<String> {
    retrieve_with_range(id, 1, n)
}

/// Retrieve the last `n` lines of an archived entry, with a line-number gutter.
#[must_use]
pub fn retrieve_tail(id: &str, n: usize) -> Option<String> {
    let content = retrieve(id)?;
    let total = content.lines().count();
    let start = if total > n { total - n + 1 } else { 1 };
    retrieve_with_range(id, start, total)
}

/// Describe the JSON structure of an archived entry.
#[must_use]
pub fn retrieve_json_keys(id: &str, path: Option<&str>) -> Option<String> {
    let content = retrieve(id)?;
    format_json_keys(&content, path, &format!("archive {id}"))
}

pub(crate) fn json_type_hint(v: &serde_json::Value) -> String {
    use serde_json::Value;
    match v {
        Value::Object(m) => format!("object({})", m.len()),
        Value::Array(a) => format!("array({})", a.len()),
        Value::String(s) => {
            let preview: String = s.chars().take(40).collect();
            if s.chars().count() > 40 {
                format!("string \"{preview}…\"")
            } else {
                format!("string \"{preview}\"")
            }
        }
        Value::Number(n) => format!("number {n}"),
        Value::Bool(b) => format!("bool {b}"),
        Value::Null => "null".to_string(),
    }
}

pub(crate) fn describe_json(v: &serde_json::Value) -> String {
    use serde_json::Value;
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let rendered: Vec<String> = keys
                .iter()
                .map(|k| format!("  {k}: {}", json_type_hint(&map[*k])))
                .collect();
            format!("object ({} keys)\n{}", map.len(), rendered.join("\n"))
        }
        Value::Array(arr) => {
            let elem = arr.first().map_or("empty", |e| match e {
                Value::Object(_) => "object",
                Value::Array(_) => "array",
                Value::String(_) => "string",
                Value::Number(_) => "number",
                Value::Bool(_) => "bool",
                Value::Null => "null",
            });
            let mut out = format!("array ({} items of {elem})", arr.len());
            if let Some(Value::Object(map)) = arr.first() {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                out.push_str(&format!(
                    "\n  [0] keys: {}",
                    keys.iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            out
        }
        Value::String(s) => format!("string ({} chars)", s.len()),
        Value::Number(n) => format!("number ({n})"),
        Value::Bool(b) => format!("bool ({b})"),
        Value::Null => "null".to_string(),
    }
}

#[must_use]
pub fn list_entries(session_id: Option<&str>) -> Vec<ArchiveEntry> {
    let base = archive_base_dir();
    if !base.exists() {
        return Vec::new();
    }
    let mut entries = Vec::new();
    if let Ok(dirs) = std::fs::read_dir(&base) {
        for dir_entry in dirs.flatten() {
            if !dir_entry.path().is_dir() {
                continue;
            }
            if let Ok(files) = std::fs::read_dir(dir_entry.path()) {
                for file in files.flatten() {
                    let path = file.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("json") {
                        continue;
                    }
                    if let Ok(data) = std::fs::read_to_string(&path)
                        && let Ok(entry) = serde_json::from_str::<ArchiveEntry>(&data)
                    {
                        if let Some(sid) = session_id
                            && entry.session_id.as_deref() != Some(sid)
                        {
                            continue;
                        }
                        entries.push(entry);
                    }
                }
            }
        }
    }
    entries.sort_by_key(|e| std::cmp::Reverse(e.created_at));
    entries
}

/// Remove only the on-disk content + metadata files for an archive id, leaving
/// the FTS index untouched. Used by the FTS cap-enforcer so the `.txt`/`.meta.json`
/// blobs of rows it evicts can't outlive their index entry as orphans (#417).
pub fn remove_files(id: &str) {
    let _ = std::fs::remove_file(content_path(id));
    let _ = std::fs::remove_file(meta_path(id));
}

/// Prune archived entries that exceed the age TTL (`max_age_hours`) or that push
/// the on-disk store past its size budget (`max_disk_mb`). The content file,
/// metadata, and FTS index are removed together so the two stores stay in sync.
/// Returns the number of entries removed.
///
/// Wired into MCP-start + periodic maintenance ([`super::storage_maintenance`])
/// and `lean-ctx cache prune`; without an enforcer the archive grew unbounded on
/// disk and starved the host of RAM via the page cache (#417).
#[must_use]
pub fn cleanup() -> u32 {
    let cutoff = Utc::now() - chrono::Duration::hours(max_age_hours() as i64);
    cleanup_with(cutoff, max_disk_bytes())
}

/// Core of [`cleanup`], parameterized for testing: drop entries older than
/// `cutoff`, then evict the oldest survivors until the total on-disk footprint is
/// at or below `budget_bytes` (`0` = no size cap).
fn cleanup_with(cutoff: DateTime<Utc>, budget_bytes: u64) -> u32 {
    let base = archive_base_dir();
    if !base.exists() {
        return 0;
    }

    struct Scanned {
        id: String,
        created_at: DateTime<Utc>,
        bytes: u64,
    }

    let mut entries: Vec<Scanned> = Vec::new();
    if let Ok(dirs) = std::fs::read_dir(&base) {
        for dir_entry in dirs.flatten() {
            if !dir_entry.path().is_dir() {
                continue;
            }
            if let Ok(files) = std::fs::read_dir(dir_entry.path()) {
                for file in files.flatten() {
                    let path = file.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("json") {
                        continue;
                    }
                    let Ok(data) = std::fs::read_to_string(&path) else {
                        continue;
                    };
                    let Ok(entry) = serde_json::from_str::<ArchiveEntry>(&data) else {
                        continue;
                    };
                    let content_bytes =
                        std::fs::metadata(content_path(&entry.id)).map_or(0, |m| m.len());
                    let meta_bytes = file.metadata().map_or(0, |m| m.len());
                    entries.push(Scanned {
                        id: entry.id,
                        created_at: entry.created_at,
                        bytes: content_bytes + meta_bytes,
                    });
                }
            }
        }
    }

    // Oldest first: TTL victims drop first, then the oldest survivors are evicted
    // until the store is back under budget. Sorted order lets us stop early.
    entries.sort_by_key(|e| e.created_at);
    let mut live_bytes: u64 = entries.iter().map(|e| e.bytes).sum();

    let mut removed = 0u32;
    for e in &entries {
        let expired = e.created_at < cutoff;
        let over_budget = budget_bytes > 0 && live_bytes > budget_bytes;
        if !expired && !over_budget {
            break;
        }
        remove_files(&e.id);
        super::archive_fts::remove_entry(&e.id);
        live_bytes = live_bytes.saturating_sub(e.bytes);
        removed += 1;
    }
    removed
}

#[must_use]
pub fn disk_usage_bytes() -> u64 {
    let base = archive_base_dir();
    if !base.exists() {
        return 0;
    }
    let mut total = 0u64;
    if let Ok(dirs) = std::fs::read_dir(&base) {
        for dir_entry in dirs.flatten() {
            if let Ok(files) = std::fs::read_dir(dir_entry.path()) {
                for file in files.flatten() {
                    total += file.metadata().map_or(0, |m| m.len());
                }
            }
        }
    }
    total
}

#[must_use]
pub fn format_hint(id: &str, size_chars: usize, size_tokens: usize) -> String {
    format!("[Archived: {size_chars} chars ({size_tokens} tok). Retrieve: ctx_expand(id=\"{id}\")]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_id_deterministic() {
        let id1 = compute_id("test content");
        let id2 = compute_id("test content");
        assert_eq!(id1, id2);
        let id3 = compute_id("different content");
        assert_ne!(id1, id3);
    }

    #[test]
    fn nonexistent_id_returns_none() {
        assert!(retrieve("nonexistent_archive_id_xyz").is_none());
    }

    #[test]
    fn format_hint_readable() {
        let hint = format_hint("abc123", 5000, 1200);
        assert!(hint.contains("5000 chars"));
        assert!(hint.contains("1200 tok"));
        assert!(hint.contains("ctx_expand"));
        assert!(hint.contains("abc123"));
    }

    fn write_test_entry(id: &str, created_at: DateTime<Utc>, content_bytes: usize) {
        std::fs::create_dir_all(entry_dir(id)).unwrap();
        std::fs::write(content_path(id), "x".repeat(content_bytes)).unwrap();
        let entry = ArchiveEntry {
            id: id.to_string(),
            tool: "ctx_shell".to_string(),
            command: "test".to_string(),
            size_chars: content_bytes,
            size_tokens: content_bytes / 4,
            created_at,
            session_id: None,
        };
        std::fs::write(meta_path(id), serde_json::to_string(&entry).unwrap()).unwrap();
    }

    #[test]
    fn cleanup_removes_expired_keeps_fresh() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", tmp.path());

        let now = Utc::now();
        write_test_entry("aa_old", now - chrono::Duration::hours(100), 100);
        write_test_entry("bb_new", now - chrono::Duration::hours(1), 100);

        // Cutoff = 48h ago; budget effectively unlimited so only the TTL applies.
        let removed = cleanup_with(now - chrono::Duration::hours(48), u64::MAX);
        assert_eq!(removed, 1);
        assert!(!content_path("aa_old").exists());
        assert!(!meta_path("aa_old").exists());
        assert!(content_path("bb_new").exists());

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }

    #[test]
    fn cleanup_enforces_disk_budget_oldest_first() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", tmp.path());

        let now = Utc::now();
        write_test_entry("c1_oldest", now - chrono::Duration::minutes(30), 10_000);
        write_test_entry("c2_middle", now - chrono::Duration::minutes(20), 10_000);
        write_test_entry("c3_newest", now - chrono::Duration::minutes(10), 10_000);

        // Nothing expired (cutoff far in the past). Budget 25 KB holds the two
        // newest (~20 KB content + meta); the single oldest entry is evicted.
        let removed = cleanup_with(now - chrono::Duration::days(365), 25_000);
        assert_eq!(removed, 1, "only the oldest over-budget entry is evicted");
        assert!(!content_path("c1_oldest").exists());
        assert!(content_path("c2_middle").exists());
        assert!(content_path("c3_newest").exists());

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }
}
