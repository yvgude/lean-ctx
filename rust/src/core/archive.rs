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
    if let Ok(v) = std::env::var("LEAN_CTX_ARCHIVE_THRESHOLD") {
        if let Ok(n) = v.parse::<usize>() {
            return n;
        }
    }
    super::config::Config::load().archive.threshold_chars
}

fn max_age_hours() -> u64 {
    if let Ok(v) = std::env::var("LEAN_CTX_ARCHIVE_TTL") {
        if let Ok(n) = v.parse::<u64>() {
            return n;
        }
    }
    super::config::Config::load().archive.max_age_hours
}

pub fn should_archive(content: &str) -> bool {
    is_enabled() && content.len() >= threshold_chars()
}

const MAX_ARCHIVE_SIZE: usize = 10 * 1024 * 1024; // 10 MB

pub fn store(tool: &str, command: &str, content: &str, session_id: Option<&str>) -> Option<String> {
    if !is_enabled() || content.is_empty() {
        return None;
    }

    let content = if content.len() > MAX_ARCHIVE_SIZE {
        &content[..MAX_ARCHIVE_SIZE]
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
            let _ = std::fs::rename(&meta_tmp, meta_path(&id));
        }
    }

    Some(id)
}

pub fn retrieve(id: &str) -> Option<String> {
    let path = content_path(id);
    std::fs::read_to_string(path).ok()
}

pub fn retrieve_with_range(id: &str, start: usize, end: usize) -> Option<String> {
    let content = retrieve(id)?;
    let lines: Vec<&str> = content.lines().collect();
    let start = start.saturating_sub(1).min(lines.len());
    let end = end.min(lines.len());
    if start >= end {
        return Some(String::new());
    }
    Some(
        lines[start..end]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{:>6}|{line}", start + i + 1))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

pub fn retrieve_with_search(id: &str, pattern: &str) -> Option<String> {
    let content = retrieve(id)?;
    let pattern_lower = pattern.to_lowercase();
    let matches: Vec<String> = content
        .lines()
        .enumerate()
        .filter(|(_, line)| line.to_lowercase().contains(&pattern_lower))
        .map(|(i, line)| format!("{:>6}|{line}", i + 1))
        .collect();

    if matches.is_empty() {
        Some(format!("No matches for \"{pattern}\" in archive {id}"))
    } else {
        Some(format!(
            "{} match(es) for \"{}\":\n{}",
            matches.len(),
            pattern,
            matches.join("\n")
        ))
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
                    if let Ok(data) = std::fs::read_to_string(&path) {
                        if let Ok(entry) = serde_json::from_str::<ArchiveEntry>(&data) {
                            if let Some(sid) = session_id {
                                if entry.session_id.as_deref() != Some(sid) {
                                    continue;
                                }
                            }
                            entries.push(entry);
                        }
                    }
                }
            }
        }
    }
    entries.sort_by_key(|e| std::cmp::Reverse(e.created_at));
    entries
}

pub fn cleanup() -> u32 {
    let max_hours = max_age_hours();
    let cutoff = Utc::now() - chrono::Duration::hours(max_hours as i64);
    let base = archive_base_dir();
    if !base.exists() {
        return 0;
    }
    let mut removed = 0u32;
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
                    if let Ok(data) = std::fs::read_to_string(&path) {
                        if let Ok(entry) = serde_json::from_str::<ArchiveEntry>(&data) {
                            if entry.created_at < cutoff {
                                let c = content_path(&entry.id);
                                let _ = std::fs::remove_file(&c);
                                let _ = std::fs::remove_file(&path);
                                removed += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    removed
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
                    total += file.metadata().map(|m| m.len()).unwrap_or(0);
                }
            }
        }
    }
    total
}

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
}
