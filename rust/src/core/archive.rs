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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protected_until: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
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

fn lock_path() -> PathBuf {
    archive_base_dir().join(".lock")
}

fn with_archive_lock<T>(f: impl FnOnce() -> T) -> Option<T> {
    use fs2::FileExt;

    std::fs::create_dir_all(archive_base_dir()).ok()?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path())
        .ok()?;
    #[cfg(unix)]
    set_private_file_perms(&lock_path());
    lock.lock_exclusive().ok()?;
    let result = f();
    let _ = FileExt::unlock(&lock);
    Some(result)
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

pub fn should_archive(content: &str) -> bool {
    is_enabled() && content.len() >= threshold_chars()
}

const MAX_ARCHIVE_SIZE: usize = 10 * 1024 * 1024; // 10 MB
const BACKGROUND_RETENTION_HOURS: i64 = 1;

pub fn store(tool: &str, command: &str, content: &str, session_id: Option<&str>) -> Option<String> {
    store_with_result(tool, command, content, session_id).map(|result| result.id)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveStoreResult {
    pub id: String,
    pub captured_chars: usize,
    pub archived_chars: usize,
    pub truncated: bool,
}

pub fn store_with_result(
    tool: &str,
    command: &str,
    content: &str,
    session_id: Option<&str>,
) -> Option<ArchiveStoreResult> {
    if !is_enabled() || content.is_empty() {
        return None;
    }

    let (content, result) = prepared_content(content);
    let id = result.id.clone();
    let created = with_archive_lock(|| {
        let c_path = content_path(&id);
        if c_path.exists() && meta_path(&id).exists() {
            return Some(false);
        }
        std::fs::create_dir_all(entry_dir(&id)).ok()?;
        if !c_path.exists() {
            super::atomic_fs::try_atomic_write(&c_path, content.as_bytes(), None).ok()?;
            #[cfg(unix)]
            set_private_file_perms(&c_path);
        }
        let entry = new_entry(&id, tool, command, content, session_id, Utc::now());
        if write_metadata(&entry).is_none() {
            let _ = std::fs::remove_file(c_path);
            return None;
        }
        Some(true)
    })
    .flatten()?;
    if created {
        super::archive_fts::index_entry(&id, tool, command, content);
    }
    Some(result)
}

/// Persist a terminal background-shell result and guarantee that lean-ctx's
/// managed cleanup paths retain it for at least one hour.
pub fn store_background(
    tool: &str,
    job_id: &str,
    content: &str,
    session_id: Option<&str>,
) -> Option<ArchiveStoreResult> {
    store_background_with_limits(
        tool,
        job_id,
        content,
        session_id,
        Utc::now(),
        max_disk_bytes(),
    )
}

fn store_background_with_limits(
    tool: &str,
    job_id: &str,
    content: &str,
    session_id: Option<&str>,
    now: DateTime<Utc>,
    budget_bytes: u64,
) -> Option<ArchiveStoreResult> {
    if !is_enabled() || content.is_empty() {
        return None;
    }

    let (content, result) = prepared_content(content);
    let id = result.id.clone();
    let protected_until = now + chrono::Duration::hours(BACKGROUND_RETENTION_HOURS);
    let (stored, evicted) = with_archive_lock(|| {
        let mut entry = read_entry(&id)
            .unwrap_or_else(|| new_entry(&id, tool, job_id, content, session_id, now));
        if !entry.aliases.iter().any(|alias| alias == job_id) {
            entry.aliases.push(job_id.to_string());
        }
        entry.protected_until = Some(
            entry
                .protected_until
                .map_or(protected_until, |current| current.max(protected_until)),
        );

        let metadata = serde_json::to_string_pretty(&entry).ok()?;
        let existing_bytes = scanned_entries()
            .into_iter()
            .find(|candidate| candidate.id == id)
            .map_or(0, |candidate| candidate.bytes);
        let required_bytes = content.len() as u64 + metadata.len() as u64;
        let evicted = admit_locked(&id, existing_bytes, required_bytes, budget_bytes, now)?;

        let c_path = content_path(&id);
        let content_existed = c_path.exists();
        if std::fs::create_dir_all(entry_dir(&id)).is_err() {
            return Some((false, evicted));
        }
        if !content_existed {
            if super::atomic_fs::try_atomic_write(&c_path, content.as_bytes(), None).is_err() {
                return Some((false, evicted));
            }
            #[cfg(unix)]
            set_private_file_perms(&c_path);
        }
        if super::atomic_fs::try_atomic_write(&meta_path(&id), metadata.as_bytes(), None).is_err() {
            if !content_existed {
                let _ = std::fs::remove_file(c_path);
            }
            return Some((false, evicted));
        }
        #[cfg(unix)]
        set_private_file_perms(&meta_path(&id));
        Some((true, evicted))
    })
    .flatten()?;

    for evicted_id in evicted {
        super::archive_fts::remove_entry(&evicted_id);
    }
    // A content-addressed archive may have been indexed by an earlier generic
    // store. Protected background output must not remain on the FTS eviction path.
    super::archive_fts::remove_entry(&id);
    stored.then_some(result)
}

fn prepared_content(content: &str) -> (&str, ArchiveStoreResult) {
    let captured_chars = content.chars().count();
    let truncated = content.len() > MAX_ARCHIVE_SIZE;
    let content = if truncated {
        &content[..content.floor_char_boundary(MAX_ARCHIVE_SIZE)]
    } else {
        content
    };
    let id = compute_id(content);
    let result = ArchiveStoreResult {
        id,
        captured_chars,
        archived_chars: content.chars().count(),
        truncated,
    };
    (content, result)
}

fn new_entry(
    id: &str,
    tool: &str,
    command: &str,
    content: &str,
    session_id: Option<&str>,
    created_at: DateTime<Utc>,
) -> ArchiveEntry {
    ArchiveEntry {
        id: id.to_string(),
        tool: tool.to_string(),
        command: command.to_string(),
        size_chars: content.len(),
        size_tokens: super::tokens::count_tokens(content),
        created_at,
        session_id: session_id.map(std::string::ToString::to_string),
        protected_until: None,
        aliases: Vec::new(),
    }
}

fn read_entry(id: &str) -> Option<ArchiveEntry> {
    serde_json::from_str(&std::fs::read_to_string(meta_path(id)).ok()?).ok()
}

fn write_metadata(entry: &ArchiveEntry) -> Option<()> {
    let path = meta_path(&entry.id);
    let json = serde_json::to_string_pretty(entry).ok()?;
    super::atomic_fs::try_atomic_write(&path, json.as_bytes(), None).ok()?;
    #[cfg(unix)]
    set_private_file_perms(&path);
    Some(())
}

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
/// a1966..." or "reference ref_18bb..."). Shared by archive and ref store paths.
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

pub fn retrieve_with_range(id: &str, start: usize, end: usize) -> Option<String> {
    let content = retrieve(id)?;
    Some(format_range(&content, start, end))
}

pub fn retrieve_with_search(id: &str, pattern: &str) -> Option<String> {
    let content = retrieve(id)?;
    Some(format_search(&content, pattern, &format!("archive {id}")))
}

/// Retrieve the first `n` lines of an archived entry, with a line-number gutter.
pub fn retrieve_head(id: &str, n: usize) -> Option<String> {
    retrieve_with_range(id, 1, n)
}

/// Retrieve the last `n` lines of an archived entry, with a line-number gutter.
pub fn retrieve_tail(id: &str, n: usize) -> Option<String> {
    let content = retrieve(id)?;
    let total = content.lines().count();
    let start = if total > n { total - n + 1 } else { 1 };
    retrieve_with_range(id, start, total)
}

/// Describe the JSON structure of an archived entry.
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

pub fn resolve_alias(alias: &str) -> Option<String> {
    list_entries(None)
        .into_iter()
        .find(|entry| entry.aliases.iter().any(|candidate| candidate == alias))
        .map(|entry| entry.id)
}

pub(crate) fn is_protected(id: &str) -> bool {
    read_entry(id)
        .and_then(|entry| entry.protected_until)
        .is_some_and(|until| until > Utc::now())
}

/// Remove only the on-disk content + metadata files for an archive id, leaving
/// the FTS index untouched. Used by the FTS cap-enforcer so the `.txt`/`.meta.json`
/// blobs of rows it evicts can't outlive their index entry as orphans (#417).
pub fn remove_files(id: &str) {
    let _ = with_archive_lock(|| {
        if !is_protected(id) {
            remove_files_locked(id);
        }
    });
}

fn remove_files_locked(id: &str) {
    let _ = std::fs::remove_file(content_path(id));
    let _ = std::fs::remove_file(meta_path(id));
}

struct ScannedArchive {
    id: String,
    created_at: DateTime<Utc>,
    protected_until: Option<DateTime<Utc>>,
    bytes: u64,
}

fn scanned_entries() -> Vec<ScannedArchive> {
    list_entries(None)
        .into_iter()
        .map(|entry| {
            let content_bytes = std::fs::metadata(content_path(&entry.id)).map_or(0, |m| m.len());
            let metadata_bytes = std::fs::metadata(meta_path(&entry.id)).map_or(0, |m| m.len());
            ScannedArchive {
                id: entry.id,
                created_at: entry.created_at,
                protected_until: entry.protected_until,
                bytes: content_bytes + metadata_bytes,
            }
        })
        .collect()
}

fn protected_at(entry: &ScannedArchive, now: DateTime<Utc>) -> bool {
    entry.protected_until.is_some_and(|until| until > now)
}

fn admit_locked(
    new_id: &str,
    existing_bytes: u64,
    required_bytes: u64,
    budget_bytes: u64,
    now: DateTime<Utc>,
) -> Option<Vec<String>> {
    if budget_bytes == 0 {
        return Some(Vec::new());
    }

    let mut entries = scanned_entries();
    let live_bytes: u64 = entries.iter().map(|entry| entry.bytes).sum();
    let mut projected = live_bytes
        .saturating_sub(existing_bytes)
        .saturating_add(required_bytes);
    if projected <= budget_bytes {
        return Some(Vec::new());
    }

    entries.sort_by_key(|entry| entry.created_at);
    let candidates: Vec<_> = entries
        .into_iter()
        .filter(|entry| entry.id != new_id && !protected_at(entry, now))
        .collect();
    let reclaimable: u64 = candidates.iter().map(|entry| entry.bytes).sum();
    if projected.saturating_sub(reclaimable) > budget_bytes {
        return None;
    }

    let mut evicted = Vec::new();
    for entry in candidates {
        remove_files_locked(&entry.id);
        projected = projected.saturating_sub(entry.bytes);
        evicted.push(entry.id);
        if projected <= budget_bytes {
            break;
        }
    }
    Some(evicted)
}

/// Prune archived entries that exceed the age TTL (`max_age_hours`) or that push
/// the on-disk store past its size budget (`max_disk_mb`). The content file,
/// metadata, and FTS index are removed together so the two stores stay in sync.
/// Returns the number of entries removed.
///
/// Wired into MCP-start + periodic maintenance (`super::storage_maintenance`)
/// and `lean-ctx cache prune`; without an enforcer the archive grew unbounded on
/// disk and starved the host of RAM via the page cache (#417).
pub fn cleanup() -> u32 {
    let now = Utc::now();
    let cutoff = now - chrono::Duration::hours(max_age_hours() as i64);
    cleanup_with(cutoff, max_disk_bytes(), now)
}

/// Core of [`cleanup`], parameterized for testing: drop entries older than
/// `cutoff`, then evict the oldest survivors until the total on-disk footprint is
/// at or below `budget_bytes` (`0` = no size cap).
fn cleanup_with(cutoff: DateTime<Utc>, budget_bytes: u64, now: DateTime<Utc>) -> u32 {
    let base = archive_base_dir();
    if !base.exists() {
        return 0;
    }

    let removed = with_archive_lock(|| {
        let mut entries = scanned_entries();
        entries.sort_by_key(|entry| entry.created_at);
        let mut live_bytes: u64 = entries.iter().map(|entry| entry.bytes).sum();
        let mut removed = std::collections::HashSet::new();

        for entry in &entries {
            if entry.created_at < cutoff && !protected_at(entry, now) {
                remove_files_locked(&entry.id);
                live_bytes = live_bytes.saturating_sub(entry.bytes);
                removed.insert(entry.id.clone());
            }
        }

        if budget_bytes > 0 {
            for entry in &entries {
                if live_bytes <= budget_bytes {
                    break;
                }
                if !removed.contains(&entry.id) && !protected_at(entry, now) {
                    remove_files_locked(&entry.id);
                    live_bytes = live_bytes.saturating_sub(entry.bytes);
                    removed.insert(entry.id.clone());
                }
            }
        }
        removed
    })
    .unwrap_or_default();
    for id in &removed {
        super::archive_fts::remove_entry(id);
    }
    removed.len() as u32
}

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

/// Filesystem path of an archived entry's verbatim content. Exposed so recovery
/// hints can offer the MCP-free "read this file directly" route alongside
/// `ctx_expand(id=...)` — the same content is reachable both ways.
pub fn content_path_str(id: &str) -> String {
    content_path(id).to_string_lossy().into_owned()
}

pub fn format_hint(id: &str, size_chars: usize, size_tokens: usize) -> String {
    // Unified, non-MCP-first recovery grammar (see [`crate::core::recovery`]): the
    // archived blob is a real file readable with any tool, and `ctx_expand(id)`
    // reaches the same bytes for surgical slices.
    let clause = crate::core::recovery::handle_clause(id, Some(&content_path_str(id)));
    format!("[Archived: {size_chars} chars ({size_tokens} tok). {clause}]")
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
            protected_until: None,
            aliases: Vec::new(),
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
        let removed = cleanup_with(now - chrono::Duration::hours(48), u64::MAX, now);
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
        let removed = cleanup_with(now - chrono::Duration::days(365), 25_000, now);
        assert_eq!(removed, 1, "only the oldest over-budget entry is evicted");
        assert!(!content_path("c1_oldest").exists());
        assert!(content_path("c2_middle").exists());
        assert!(content_path("c3_newest").exists());

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }

    #[test]
    fn legacy_metadata_defaults_background_retention_fields() {
        let entry: ArchiveEntry = serde_json::from_value(serde_json::json!({
            "id": "legacy",
            "tool": "ctx_shell",
            "command": "printf legacy",
            "size_chars": 6,
            "size_tokens": 2,
            "created_at": "2026-01-01T00:00:00Z",
            "session_id": null
        }))
        .unwrap();

        assert!(entry.protected_until.is_none());
        assert!(entry.aliases.is_empty());
    }

    #[test]
    fn protected_archive_survives_cleanup_and_direct_fts_removal_path() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", tmp.path());

        let now = Utc::now();
        write_test_entry("protected", now - chrono::Duration::hours(100), 100);
        let mut entry = read_entry("protected").unwrap();
        entry.protected_until = Some(now + chrono::Duration::hours(1));
        write_metadata(&entry).unwrap();

        assert_eq!(cleanup_with(now - chrono::Duration::hours(48), 1, now), 0);
        remove_files("protected");
        assert!(content_path("protected").exists());

        let later = now + chrono::Duration::hours(2);
        assert_eq!(cleanup_with(now, 1, later), 1);
        assert!(!content_path("protected").exists());

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }

    #[test]
    fn background_store_refuses_when_protected_entries_fill_budget() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", tmp.path());
        crate::test_env::set_var("LEAN_CTX_ARCHIVE", "1");

        let now = Utc::now();
        let first = store_background_with_limits(
            "ctx_shell",
            "shell_first",
            "first protected output",
            None,
            now,
            u64::MAX,
        )
        .unwrap();
        let budget = scanned_entries()
            .into_iter()
            .find(|entry| entry.id == first.id)
            .unwrap()
            .bytes;

        assert!(
            store_background_with_limits(
                "ctx_shell",
                "shell_second",
                "different protected output",
                None,
                now,
                budget,
            )
            .is_none()
        );
        assert_eq!(resolve_alias("shell_first"), Some(first.id));
        assert!(resolve_alias("shell_second").is_none());

        crate::test_env::remove_var("LEAN_CTX_ARCHIVE");
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }

    #[test]
    fn concurrent_background_writers_merge_content_aliases() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", tmp.path());
        crate::test_env::set_var("LEAN_CTX_ARCHIVE", "1");

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut writers = Vec::new();
        for job_id in ["shell_alpha", "shell_beta"] {
            let barrier = barrier.clone();
            writers.push(std::thread::spawn(move || {
                barrier.wait();
                store_background_with_limits(
                    "ctx_shell",
                    job_id,
                    "shared terminal output",
                    None,
                    Utc::now(),
                    0,
                )
                .unwrap()
            }));
        }
        barrier.wait();
        let first = writers.remove(0).join().unwrap();
        let second = writers.remove(0).join().unwrap();

        assert_eq!(first.id, second.id);
        assert_eq!(resolve_alias("shell_alpha"), Some(first.id.clone()));
        assert_eq!(resolve_alias("shell_beta"), Some(first.id));

        crate::test_env::remove_var("LEAN_CTX_ARCHIVE");
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }
}
