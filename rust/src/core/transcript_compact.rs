//! Transcript/conversation compaction for agent session JSONL files.
//!
//! Compresses `tool_result` blocks in JSONL conversation transcripts (e.g.
//! `~/.claude/projects/*.jsonl`, `~/.cursor/agent-transcripts/*.jsonl`)
//! by replacing large tool outputs with compact summaries.
//!
//! Analogous to `ContextZip`'s approach: 85.8% of transcript bytes are tool I/O.

use std::path::Path;

const MAX_TOOL_OUTPUT_CHARS: usize = 500;
const MIN_COMPRESS_CHARS: usize = 200;

#[derive(Debug, Default)]
pub struct CompactionStats {
    pub lines_processed: usize,
    pub lines_compacted: usize,
    pub original_bytes: usize,
    pub compacted_bytes: usize,
}

impl CompactionStats {
    #[must_use]
    pub fn savings_pct(&self) -> f64 {
        if self.original_bytes == 0 {
            return 0.0;
        }
        (1.0 - self.compacted_bytes as f64 / self.original_bytes as f64) * 100.0
    }
}

impl std::fmt::Display for CompactionStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} lines ({} compacted), {:.0}% savings ({} → {} bytes)",
            self.lines_processed,
            self.lines_compacted,
            self.savings_pct(),
            self.original_bytes,
            self.compacted_bytes,
        )
    }
}

/// Compact a single JSONL transcript file in-place.
/// Returns stats about what was compacted.
pub fn compact_file(path: &Path) -> Result<CompactionStats, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
    let mut stats = CompactionStats {
        original_bytes: content.len(),
        ..Default::default()
    };

    let mut output_lines = Vec::new();

    for line in content.lines() {
        stats.lines_processed += 1;

        if line.len() < MIN_COMPRESS_CHARS || !line.contains("tool_result") {
            output_lines.push(line.to_string());
            continue;
        }

        match compact_jsonl_line(line) {
            Some(compacted) => {
                stats.lines_compacted += 1;
                output_lines.push(compacted);
            }
            None => {
                output_lines.push(line.to_string());
            }
        }
    }

    let result = output_lines.join("\n");
    stats.compacted_bytes = result.len();

    if stats.lines_compacted > 0 {
        std::fs::write(path, &result).map_err(|e| format!("write: {e}"))?;
    }

    Ok(stats)
}

/// Compact all JSONL files in a directory.
pub fn compact_directory(dir: &Path) -> Result<CompactionStats, String> {
    if !dir.is_dir() {
        return Err(format!("not a directory: {}", dir.display()));
    }

    let mut total = CompactionStats::default();

    let entries = std::fs::read_dir(dir).map_err(|e| format!("readdir: {e}"))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "jsonl") {
            match compact_file(&path) {
                Ok(s) => {
                    total.lines_processed += s.lines_processed;
                    total.lines_compacted += s.lines_compacted;
                    total.original_bytes += s.original_bytes;
                    total.compacted_bytes += s.compacted_bytes;
                }
                Err(e) => {
                    tracing::warn!("skip {}: {e}", path.display());
                }
            }
        }
    }

    Ok(total)
}

fn compact_jsonl_line(line: &str) -> Option<String> {
    let mut doc: serde_json::Value = serde_json::from_str(line).ok()?;

    let mut modified = false;

    if let Some(content) = doc.get_mut("content") {
        if let Some(arr) = content.as_array_mut() {
            for item in arr.iter_mut() {
                if compact_content_block(item) {
                    modified = true;
                }
            }
        } else if let Some(s) = content.as_str()
            && s.len() > MAX_TOOL_OUTPUT_CHARS
            && has_tool_markers(s)
        {
            let summary = summarize_content(s);
            *content = serde_json::Value::String(summary);
            modified = true;
        }
    }

    if let Some(result) = doc.get_mut("result")
        && compact_content_block(result)
    {
        modified = true;
    }

    if modified {
        Some(serde_json::to_string(&doc).ok()?)
    } else {
        None
    }
}

fn compact_content_block(block: &mut serde_json::Value) -> bool {
    if let Some(text) = block.get_mut("text")
        && let Some(s) = text.as_str()
        && s.len() > MAX_TOOL_OUTPUT_CHARS
        && has_tool_markers(s)
    {
        let summary = summarize_content(s);
        *text = serde_json::Value::String(summary);
        return true;
    }

    if let Some(content) = block.get_mut("content") {
        if let Some(s) = content.as_str()
            && s.len() > MAX_TOOL_OUTPUT_CHARS
        {
            let summary = summarize_content(s);
            *content = serde_json::Value::String(summary);
            return true;
        }
        if let Some(arr) = content.as_array_mut() {
            let mut any_modified = false;
            for item in arr.iter_mut() {
                if compact_content_block(item) {
                    any_modified = true;
                }
            }
            return any_modified;
        }
    }

    false
}

fn has_tool_markers(s: &str) -> bool {
    s.contains("tool_result") || s.contains("ctx_") || s.contains("```") || s.len() > 2000
}

pub(crate) fn summarize_content(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let total_lines = lines.len();
    let char_count = text.len();

    let trunc = |s: &str| -> String {
        if s.len() > 120 {
            format!("{}...", &s[..s.floor_char_boundary(120)])
        } else {
            s.to_string()
        }
    };

    let first_meaningful = lines
        .iter()
        .take(3)
        .filter(|l| !l.trim().is_empty())
        .map(|l| trunc(l))
        .collect::<Vec<_>>()
        .join("\n");

    let last_line = lines
        .iter()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|l| trunc(l))
        .unwrap_or_default();

    format!("[compacted: {total_lines}L, {char_count}ch]\n{first_meaningful}\n...\n{last_line}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_preserves_first_and_last() {
        let text = "line 1\nline 2\nline 3\nline 4\nline 5\nline 6";
        let result = summarize_content(text);
        assert!(result.contains("line 1"));
        assert!(result.contains("line 6"));
        assert!(result.contains("[compacted:"));
    }

    #[test]
    fn compact_skips_short_lines() {
        let short = r#"{"type":"text","content":"hello"}"#;
        assert!(compact_jsonl_line(short).is_none());
    }

    #[test]
    fn compact_file_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        let line = serde_json::json!({
            "type": "tool_result",
            "content": "x".repeat(3000)
        });
        std::fs::write(&path, serde_json::to_string(&line).unwrap()).unwrap();

        let stats = compact_file(&path).unwrap();
        assert_eq!(stats.lines_processed, 1);
        assert!(stats.compacted_bytes < stats.original_bytes);
    }

    #[test]
    fn savings_pct_empty() {
        let stats = CompactionStats::default();
        assert_eq!(stats.savings_pct(), 0.0);
    }

    #[test]
    fn savings_pct_calculation() {
        let stats = CompactionStats {
            original_bytes: 1000,
            compacted_bytes: 200,
            ..Default::default()
        };
        assert!((stats.savings_pct() - 80.0).abs() < 0.1);
    }
}
