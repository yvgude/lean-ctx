use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

const HEATMAP_FLUSH_EVERY: usize = 25;
const HEATMAP_MAX_ENTRIES: usize = 10_000;

static HEATMAP_BUFFER: Mutex<Option<HeatMap>> = Mutex::new(None);
static HEATMAP_CALLS: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeatEntry {
    pub path: String,
    pub access_count: u32,
    pub last_access: String,
    pub total_tokens_saved: u64,
    pub total_original_tokens: u64,
    pub avg_compression_ratio: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HeatMap {
    pub entries: HashMap<String, HeatEntry>,
    #[serde(skip)]
    dirty: bool,
}

impl HeatMap {
    pub fn load() -> Self {
        let mut guard = HEATMAP_BUFFER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(ref hm) = *guard {
            return hm.clone();
        }
        let hm = load_from_disk();
        *guard = Some(hm.clone());
        hm
    }

    pub fn record_access(&mut self, file_path: &str, original_tokens: usize, saved_tokens: usize) {
        let now = chrono::Utc::now().to_rfc3339();
        let entry = self
            .entries
            .entry(file_path.to_string())
            .or_insert_with(|| HeatEntry {
                path: file_path.to_string(),
                access_count: 0,
                last_access: now.clone(),
                total_tokens_saved: 0,
                total_original_tokens: 0,
                avg_compression_ratio: 0.0,
            });
        entry.access_count += 1;
        entry.last_access = now;
        entry.total_tokens_saved += saved_tokens as u64;
        entry.total_original_tokens += original_tokens as u64;
        if entry.total_original_tokens > 0 {
            entry.avg_compression_ratio = 1.0
                - (entry.total_original_tokens - entry.total_tokens_saved) as f32
                    / entry.total_original_tokens as f32;
        }
        self.dirty = true;
    }

    pub fn save(&self) -> std::io::Result<()> {
        if !self.dirty && !self.entries.is_empty() {
            return Ok(());
        }
        save_to_disk(self)?;
        let mut guard = HEATMAP_BUFFER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = Some(self.clone());
        Ok(())
    }

    pub fn top_files(&self, limit: usize) -> Vec<&HeatEntry> {
        let mut sorted: Vec<&HeatEntry> = self.entries.values().collect();
        sorted.sort_by_key(|x| std::cmp::Reverse(x.access_count));
        sorted.truncate(limit);
        sorted
    }

    pub fn directory_summary(&self) -> Vec<(String, u32, u64)> {
        let mut dirs: HashMap<String, (u32, u64)> = HashMap::new();
        for entry in self.entries.values() {
            let dir = std::path::Path::new(&entry.path)
                .parent()
                .map_or_else(|| ".".to_string(), |p| p.to_string_lossy().to_string());
            let stat = dirs.entry(dir).or_insert((0, 0));
            stat.0 += entry.access_count;
            stat.1 += entry.total_tokens_saved;
        }
        let mut result: Vec<(String, u32, u64)> = dirs
            .into_iter()
            .map(|(dir, (count, saved))| (dir, count, saved))
            .collect();
        result.sort_by_key(|x| std::cmp::Reverse(x.1));
        result
    }

    pub fn cold_files(&self, all_files: &[String], limit: usize) -> Vec<String> {
        let hot: std::collections::HashSet<&str> = self
            .entries
            .keys()
            .map(std::string::String::as_str)
            .collect();
        let mut cold: Vec<String> = all_files
            .iter()
            .filter(|f| !hot.contains(f.as_str()))
            .cloned()
            .collect();
        cold.truncate(limit);
        cold
    }

    fn storage_path() -> PathBuf {
        crate::core::data_dir::lean_ctx_data_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("heatmap.json")
    }
}

fn load_from_disk() -> HeatMap {
    let path = HeatMap::storage_path();
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => HeatMap::default(),
    }
}

fn save_to_disk(hm: &HeatMap) -> std::io::Result<()> {
    let path = HeatMap::storage_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(hm)?;
    std::fs::write(&path, json)
}

pub fn record_file_access(file_path: &str, original_tokens: usize, saved_tokens: usize) {
    let mut guard = HEATMAP_BUFFER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let hm = guard.get_or_insert_with(load_from_disk);
    hm.record_access(file_path, original_tokens, saved_tokens);

    // Enforce bounded retention.
    if hm.entries.len() > HEATMAP_MAX_ENTRIES {
        let mut items: Vec<(String, u32)> = hm
            .entries
            .values()
            .map(|e| (e.path.clone(), e.access_count))
            .collect();
        items.sort_by_key(|x| x.1);
        let drop_n = hm.entries.len().saturating_sub(HEATMAP_MAX_ENTRIES);
        for (path, _) in items.into_iter().take(drop_n) {
            hm.entries.remove(&path);
        }
    }

    let n = HEATMAP_CALLS.fetch_add(1, Ordering::Relaxed) + 1;
    if n.is_multiple_of(HEATMAP_FLUSH_EVERY) && save_to_disk(hm).is_ok() {
        hm.dirty = false;
    }
}

pub fn format_heatmap_status(heatmap: &HeatMap, limit: usize) -> String {
    let top = heatmap.top_files(limit);
    if top.is_empty() {
        return "No file access data recorded yet.".to_string();
    }
    let mut lines = vec![format!(
        "File Access Heat Map ({} tracked files):",
        heatmap.entries.len()
    )];
    lines.push(String::new());
    for (i, entry) in top.iter().enumerate() {
        let short = short_path(&entry.path);
        let heat = heat_indicator(entry.access_count);
        lines.push(format!(
            "  {heat} #{} {} — {} accesses, {:.0}% compression, {} tok saved",
            i + 1,
            short,
            entry.access_count,
            entry.avg_compression_ratio * 100.0,
            entry.total_tokens_saved
        ));
    }
    lines.join("\n")
}

pub fn format_directory_summary(heatmap: &HeatMap) -> String {
    let dirs = heatmap.directory_summary();
    if dirs.is_empty() {
        return "No directory data.".to_string();
    }
    let mut lines = vec!["Directory Heat Map:".to_string(), String::new()];
    for (dir, count, saved) in dirs.iter().take(15) {
        let heat = heat_indicator(*count);
        lines.push(format!(
            "  {heat} {dir}/ — {count} accesses, {saved} tok saved"
        ));
    }
    lines.join("\n")
}

fn heat_indicator(count: u32) -> &'static str {
    match count {
        0 => "  ",
        1..=3 => "▁▁",
        4..=8 => "▃▃",
        9..=15 => "▅▅",
        16..=30 => "▇▇",
        _ => "██",
    }
}

fn short_path(path: &str) -> &str {
    let parts: Vec<&str> = path.rsplitn(3, '/').collect();
    if parts.len() >= 2 {
        let start = path.len() - parts[0].len() - parts[1].len() - 1;
        &path[start..]
    } else {
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_query() {
        let mut hm = HeatMap::default();
        hm.record_access("src/main.rs", 100, 80);
        hm.record_access("src/main.rs", 100, 90);
        hm.record_access("src/lib.rs", 200, 50);

        assert_eq!(hm.entries.len(), 2);
        assert_eq!(hm.entries["src/main.rs"].access_count, 2);
        assert_eq!(hm.entries["src/lib.rs"].total_tokens_saved, 50);
    }

    #[test]
    fn top_files_sorted() {
        let mut hm = HeatMap::default();
        hm.record_access("a.rs", 100, 50);
        hm.record_access("b.rs", 100, 50);
        hm.record_access("b.rs", 100, 50);
        hm.record_access("c.rs", 100, 50);
        hm.record_access("c.rs", 100, 50);
        hm.record_access("c.rs", 100, 50);

        let top = hm.top_files(2);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].path, "c.rs");
        assert_eq!(top[1].path, "b.rs");
    }

    #[test]
    fn directory_summary_works() {
        let mut hm = HeatMap::default();
        hm.record_access("src/a.rs", 100, 50);
        hm.record_access("src/b.rs", 100, 50);
        hm.record_access("tests/t.rs", 200, 100);

        let dirs = hm.directory_summary();
        assert!(dirs.len() >= 2);
    }

    #[test]
    fn cold_files_detection() {
        let mut hm = HeatMap::default();
        hm.record_access("src/a.rs", 100, 50);

        let all = vec![
            "src/a.rs".to_string(),
            "src/b.rs".to_string(),
            "src/c.rs".to_string(),
        ];
        let cold = hm.cold_files(&all, 10);
        assert_eq!(cold.len(), 2);
        assert!(cold.contains(&"src/b.rs".to_string()));
    }

    #[test]
    fn heat_indicators() {
        assert_eq!(heat_indicator(0), "  ");
        assert_eq!(heat_indicator(1), "▁▁");
        assert_eq!(heat_indicator(10), "▅▅");
        assert_eq!(heat_indicator(50), "██");
    }

    #[test]
    fn compression_ratio() {
        let mut hm = HeatMap::default();
        hm.record_access("a.rs", 1000, 800);
        let entry = &hm.entries["a.rs"];
        assert!((entry.avg_compression_ratio - 0.8).abs() < 0.01);
    }
}
