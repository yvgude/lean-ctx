// SPDX-License-Identifier: Apache-2.0
//! Readers of this device's own stores that engine metrics v2 build on: the
//! session files written since a time, and the lines of a time-ordered
//! append-only JSONL store (savings ledger, audit trail) inside a window of
//! UTC days, read without scanning the older part.

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use chrono::NaiveDate;

/// UTC days, both ends included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Window {
    pub(crate) from: NaiveDate,
    pub(crate) to: NaiveDate,
}

impl Window {
    fn contains(self, day: &str) -> bool {
        NaiveDate::parse_from_str(day, "%Y-%m-%d").is_ok_and(|d| d >= self.from && d <= self.to)
    }

    fn first_day_text(self) -> String {
        self.from.format("%Y-%m-%d").to_string()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn line(time: &str, n: u32) -> String {
        format!("{{\"timestamp\":\"{time}\",\"n\":{n}}}\n")
    }

    /// Only complete lines dated inside the window are visited, also when
    /// the window starts after a long older part.
    #[test]
    fn scan_window_visits_the_complete_lines_of_its_days() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trail.jsonl");
        let mut text: String = (0..3_000)
            .map(|n| line("2026-10-01T08:00:00Z", n))
            .collect();
        text += &line("2026-10-19T09:00:00Z", 1);
        text += &line("2026-10-20T09:00:00Z", 2);
        text += &line("2026-10-21T09:00:00Z", 3);
        text += "{\"timestamp\":\"2026-10-20T10:00:00Z\"";
        std::fs::write(&path, text).unwrap();
        let window = Window {
            from: NaiveDate::from_ymd_opt(2026, 10, 19).unwrap(),
            to: NaiveDate::from_ymd_opt(2026, 10, 20).unwrap(),
        };
        let mut days = Vec::new();
        scan_window(&path, window, b"\"timestamp\":\"", |day, _| {
            days.push(day.to_string());
        });
        assert_eq!(days, ["2026-10-19", "2026-10-20"]);
    }
}
